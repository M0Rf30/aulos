// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! App-side wiring for the playback / desktop extras:
//!
//! - "stop after this track" and fade in/out (engine-level, see
//!   `aulos_core::player::engine::fade`),
//! - party mode and auto-random / auto-similar continuation
//!   (`aulos_core::player::party`),
//! - suspend/idle inhibition while playing (`aulos_core::desktop::inhibit`),
//! - track-change desktop notifications (`aulos_core::desktop::notify`),
//! - background playback (closing the window while playing keeps audio and
//!   MPRIS alive).
//!
//! One wrapped [`PlaybackExtrasMessage`] variant (`Message::Playback`) keeps
//! this feature's vocabulary out of the top-level `Message` enum.
//!
//! ## Background playback limitation
//!
//! libcosmic (this revision) runs Aulos as a single-window, non-daemon iced
//! application: once its only window is destroyed the whole process exits,
//! and a window can't be re-created afterwards. So "closing" the window
//! while background playback is on **minimizes** it instead (audio, MPRIS and
//! the tick loop keep running); an MPRIS `Raise` / a second `aulos` launch
//! restores and focuses it. The window stays in the task list; it cannot be
//! hidden from it.

use super::{AppModel, Message};
use aulos_core::config::AutoPlayMode;
use aulos_core::player::extras::{CloseAction, close_action};
use cosmic::Task;
use std::time::Duration;

type AppTask = Task<cosmic::Action<Message>>;

#[derive(Debug, Clone)]
pub enum PlaybackExtrasMessage {
    /// Toggle "stop after the current track".
    ToggleStopAfter,
    /// Fade in/out duration in seconds (`0` = off).
    SetFadeDuration(f32),
    SetAutoPlayMode(AutoPlayMode),
    SetPartyMode(bool),
    /// Flip party mode (menu entry; the settings toggler sends `SetPartyMode`).
    TogglePartyMode,
    /// Add/remove a genre from the party-mode pool.
    TogglePartyGenre(String),
    SetNotifyTrackChange(bool),
    SetInhibit(bool),
    SetBackgroundPlayback(bool),
    /// The window was asked to close (header button, compositor, Alt+F4).
    CloseRequested,
    /// Periodic housekeeping while idle/paused (the playback tick only runs
    /// while playing): releases the inhibit lock after a pause or stop.
    Sync,
}

impl AppModel {
    /// Push the persisted settings into a (re)created player.
    pub(super) fn apply_playback_extras_config(&mut self) {
        if let Some(player) = &mut self.session.playback.player {
            player.set_fade_secs(self.session.config.config.fade_duration_secs);
        }
    }

    pub(super) fn update_playback_extras(&mut self, msg: PlaybackExtrasMessage) -> AppTask {
        match msg {
            PlaybackExtrasMessage::ToggleStopAfter => {
                self.session.toggle_stop_after();
                Task::none()
            }
            PlaybackExtrasMessage::SetFadeDuration(secs) => {
                self.session.set_fade_duration(secs);
                self.session.config.save();
                Task::none()
            }
            PlaybackExtrasMessage::SetAutoPlayMode(mode) => {
                self.session.set_auto_play_mode(mode);
                self.session.config.save();
                Task::none()
            }
            PlaybackExtrasMessage::SetPartyMode(on) => {
                let update = self.session.set_party_mode(on);
                self.session.config.save();
                self.playback_task(update)
            }
            PlaybackExtrasMessage::TogglePartyMode => {
                let on = !self.session.config.config.party_mode;
                self.update_playback_extras(PlaybackExtrasMessage::SetPartyMode(on))
            }
            PlaybackExtrasMessage::TogglePartyGenre(genre) => {
                self.session.toggle_party_genre(genre);
                self.session.config.save();
                Task::none()
            }
            PlaybackExtrasMessage::SetNotifyTrackChange(on) => {
                self.session.config.config.notify_track_change = on;
                self.session.config.save();
                Task::none()
            }
            PlaybackExtrasMessage::SetInhibit(on) => {
                let effect = self.session.set_inhibit_while_playing(on);
                self.session.config.save();
                self.playback_effect(effect)
            }
            PlaybackExtrasMessage::SetBackgroundPlayback(on) => {
                self.session.config.config.background_playback = on;
                self.session.config.save();
                Task::none()
            }
            PlaybackExtrasMessage::CloseRequested => self.handle_close_requested(),
            PlaybackExtrasMessage::Sync => {
                let effect = self.session.playback_extras_tick();
                self.playback_effect(effect)
            }
        }
    }

    /// Pop a desktop notification for the track that just became current,
    /// when enabled and the window isn't focused.
    pub(super) fn notify_track_changed(&mut self) -> AppTask {
        let focused = self.core.focused_window().is_some();
        let cover_art_bytes = &mut self.cover_art_bytes;
        let effect = self
            .session
            .notify_track_changed(focused, |track| cover_art_bytes.get(&track.cover_key()));
        self.playback_effect(effect)
    }

    // -- Window lifecycle ----------------------------------------------

    /// Closing the window: keep playing in the background (minimized) when
    /// that is enabled and music is actually playing, otherwise quit.
    fn handle_close_requested(&mut self) -> AppTask {
        let main_window = self.core.main_window_id();
        let action = close_action(
            self.session.config.config.background_playback,
            self.session.playback.player.as_ref(),
            main_window.is_some(),
        );
        if let (CloseAction::Minimize, Some(id)) = (action, main_window) {
            return cosmic::iced::window::minimize(id, true);
        }
        self.session.config.flush();
        cosmic::iced::exit()
    }

    /// Restore and focus the main window (MPRIS `Raise`, second-instance
    /// handoff).
    pub(super) fn raise_window(&self) -> AppTask {
        let Some(id) = self.core.main_window_id() else {
            return Task::none();
        };
        cosmic::iced::window::minimize::<cosmic::Action<Message>>(id, false)
            .chain(cosmic::iced::window::gain_focus(id))
    }
}

impl AppModel {
    /// Subscriptions owned by this module: window-close requests from the
    /// compositor (`exit_on_close` is off, see `main.rs`), plus — only while
    /// an inhibit lock is held or being taken — a slow housekeeping tick
    /// that releases it after a pause/stop (the playback tick only runs
    /// while playing).
    pub(super) fn playback_extras_subscription(&self) -> cosmic::iced::Subscription<Message> {
        let mut subs = vec![
            cosmic::iced::window::close_requests()
                .map(|_| Message::Playback(PlaybackExtrasMessage::CloseRequested)),
        ];
        if self.session.playback.extras.inhibit_busy() {
            subs.push(cosmic::iced::Subscription::run(sync_stream));
        }
        cosmic::iced::Subscription::batch(subs)
    }
}

fn sync_stream() -> impl futures_util::Stream<Item = Message> {
    use futures_util::StreamExt;
    aulos_core::task::interval_stream(Duration::from_secs(2))
        .map(|()| Message::Playback(PlaybackExtrasMessage::Sync))
}

// ---------------------------------------------------------------------------
// Settings UI
// ---------------------------------------------------------------------------

/// Longest genre list shown as party-pool toggles (the drawer scrolls, but
/// a huge wall of chips helps nobody).
const MAX_PARTY_GENRE_CHIPS: usize = 48;

fn dim_text() -> cosmic::theme::Text {
    cosmic::theme::Text::Color(cosmic::theme::active().cosmic().palette.neutral_7.into())
}

fn section_header<'a>(
    title: String,
    description: String,
) -> cosmic::Element<'a, PlaybackExtrasMessage> {
    cosmic::widget::Column::new()
        .push(cosmic::widget::text::heading(title))
        .push(cosmic::widget::text::caption(description).class(dim_text()))
        .spacing(2)
        .into()
}

impl AppModel {
    /// The "Fades and continuous playback" and "Desktop" settings
    /// sections, shown inside the Settings drawer.
    pub(super) fn playback_extras_settings(&self) -> cosmic::Element<'_, PlaybackExtrasMessage> {
        use cosmic::iced::{Alignment, Length};
        use cosmic::widget;

        let sp = cosmic::theme::active().cosmic().spacing;
        let cfg = &self.session.config.config;
        type Msg = PlaybackExtrasMessage;

        // Fade duration.
        let fade = cfg.fade_duration_secs;
        let fade_label = if fade < 0.05 {
            aulos_core::fl!("fade-disabled")
        } else {
            aulos_core::fl!("fade-seconds", secs = format!("{fade:.1}"))
        };
        let fade_item = widget::Column::new()
            .push(
                widget::Row::new()
                    .push(widget::text::body(aulos_core::fl!("fade-duration")))
                    .push(widget::space::horizontal())
                    .push(widget::text::caption(fade_label).class(dim_text()))
                    .align_y(Alignment::Center),
            )
            .push(
                widget::slider(
                    0.0..=aulos_core::player::engine::fade::MAX_FADE_SECS,
                    fade,
                    Msg::SetFadeDuration,
                )
                .step(0.1_f32)
                .width(Length::Fill),
            )
            .push(widget::text::caption(aulos_core::fl!("fade-description")).class(dim_text()))
            .spacing(sp.space_xxs)
            .width(Length::Fill);

        // Auto-play mode (what plays when the queue runs out).
        let auto_labels = vec![
            aulos_core::fl!("auto-play-off"),
            aulos_core::fl!("auto-play-random"),
            aulos_core::fl!("auto-play-similar"),
        ];
        let auto_selected = AutoPlayMode::ALL
            .iter()
            .position(|m| *m == cfg.auto_play_mode);
        let auto_item = widget::settings::item::builder(aulos_core::fl!("auto-play"))
            .description(aulos_core::fl!("auto-play-description"))
            .control(widget::dropdown(auto_labels, auto_selected, |i| {
                Msg::SetAutoPlayMode(AutoPlayMode::ALL[i])
            }));

        // Party mode + its genre pool.
        let party_item = widget::settings::item::builder(aulos_core::fl!("party-mode"))
            .description(aulos_core::fl!("party-mode-description"))
            .toggler(cfg.party_mode, Msg::SetPartyMode);

        let mut playback_section = widget::settings::section()
            .header(section_header(
                aulos_core::fl!("settings-playback-extras"),
                aulos_core::fl!("settings-playback-extras-description"),
            ))
            .add(fade_item)
            .add(auto_item)
            .add(party_item);

        if cfg.party_mode {
            let chips: Vec<cosmic::Element<'_, Msg>> = self
                .session
                .library
                .genres
                .iter()
                .filter(|g| !g.trim().is_empty())
                .take(MAX_PARTY_GENRE_CHIPS)
                .map(|genre| {
                    let on = cfg.party_genres.iter().any(|g| g == genre);
                    let class = if on {
                        cosmic::theme::Button::Suggested
                    } else {
                        cosmic::theme::Button::Standard
                    };
                    widget::button::text(genre.as_str())
                        .class(class)
                        .on_press(Msg::TogglePartyGenre(genre.clone()))
                        .into()
                })
                .collect();
            let genres_block: cosmic::Element<'_, Msg> = if chips.is_empty() {
                widget::text::caption(aulos_core::fl!("party-genres-none"))
                    .class(dim_text())
                    .into()
            } else {
                widget::Column::new()
                    .push(widget::text::body(aulos_core::fl!("party-genres")))
                    .push(
                        widget::flex_row(chips)
                            .row_spacing(sp.space_xxs)
                            .column_spacing(sp.space_xxs),
                    )
                    .spacing(sp.space_xxs)
                    .width(Length::Fill)
                    .into()
            };
            playback_section = playback_section.add(genres_block);
        }

        // Desktop integration.
        let desktop_section = widget::settings::section()
            .header(section_header(
                aulos_core::fl!("settings-desktop"),
                aulos_core::fl!("settings-desktop-description"),
            ))
            .add(
                widget::settings::item::builder(aulos_core::fl!("notify-track-change"))
                    .description(aulos_core::fl!("notify-track-change-description"))
                    .toggler(cfg.notify_track_change, Msg::SetNotifyTrackChange),
            )
            .add(
                widget::settings::item::builder(aulos_core::fl!("inhibit-suspend"))
                    .description(aulos_core::fl!("inhibit-suspend-description"))
                    .toggler(cfg.inhibit_while_playing, Msg::SetInhibit),
            )
            .add(
                widget::settings::item::builder(aulos_core::fl!("background-playback"))
                    .description(aulos_core::fl!("background-playback-description"))
                    .toggler(cfg.background_playback, Msg::SetBackgroundPlayback),
            );

        widget::Column::new()
            .push(playback_section)
            .push(desktop_section)
            .spacing(sp.space_l)
            .width(Length::Fill)
            .into()
    }
}
