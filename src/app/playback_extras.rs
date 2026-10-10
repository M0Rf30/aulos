// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! App-side wiring for the playback / desktop extras:
//!
//! - "stop after this track" and fade in/out (engine-level, see
//!   `crate::player::engine::fade`),
//! - party mode and auto-random / auto-similar continuation
//!   (`crate::player::party`),
//! - suspend/idle inhibition while playing (`crate::desktop::inhibit`),
//! - track-change desktop notifications (`crate::desktop::notify`),
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
use crate::config::AutoPlayMode;
use crate::desktop::{inhibit, notify};
use crate::library::CoverArt;
use crate::player::{ActiveBackend, PlaybackState, party};
use cosmic::Task;
use std::collections::HashSet;
use std::time::{Duration, Instant};

type AppTask = Task<cosmic::Action<Message>>;

/// How long to wait before retrying a failed inhibit request.
const INHIBIT_RETRY: Duration = Duration::from_secs(60);

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
    /// Result of an async inhibit request.
    InhibitAcquired(Option<inhibit::InhibitToken>),
    /// The notification server's id for the last notification, to replace.
    NotificationSent(Option<u32>),
    /// Result of a fire-and-forget task.
    Noop,
    /// Periodic housekeeping while idle/paused (the playback tick only runs
    /// while playing): releases the inhibit lock after a pause or stop.
    Sync,
}

enum InhibitPhase {
    Idle,
    /// A request is in flight.
    Acquiring,
    Held(inhibit::InhibitToken),
}

/// Runtime state for the extras (nothing here is persisted).
pub struct PlaybackExtrasState {
    rng: party::Rng,
    inhibit: InhibitPhase,
    /// Don't retry a failed inhibit before this instant.
    inhibit_retry_at: Option<Instant>,
    /// Server id of the last track notification, for `replaces_id`.
    notification_id: u32,
    /// Key of the last track a notification was shown for.
    last_notified: Option<String>,
    /// `(track id, upcoming)` a top-up already came up empty for, so an
    /// unfillable queue doesn't rescan the library on every tick.
    top_up_dry: Option<(i64, usize)>,
}

impl Default for PlaybackExtrasState {
    fn default() -> Self {
        Self {
            rng: party::Rng::seeded(),
            inhibit: InhibitPhase::Idle,
            inhibit_retry_at: None,
            notification_id: 0,
            last_notified: None,
            top_up_dry: None,
        }
    }
}

impl AppModel {
    /// Push the persisted settings into a (re)created player.
    pub(super) fn apply_playback_extras_config(&mut self) {
        if let Some(player) = &mut self.player {
            player.set_fade_secs(self.config.fade_duration_secs);
        }
    }

    pub(super) fn update_playback_extras(&mut self, msg: PlaybackExtrasMessage) -> AppTask {
        match msg {
            PlaybackExtrasMessage::ToggleStopAfter => {
                if let Some(player) = &mut self.player
                    && player.active_backend_type() == ActiveBackend::Local
                {
                    let on = !player.stop_after_current();
                    // Arming only makes sense while something is playing;
                    // disarming is always allowed.
                    if !on || player.state() != PlaybackState::Stopped {
                        player.set_stop_after_current(on);
                    }
                }
                Task::none()
            }
            PlaybackExtrasMessage::SetFadeDuration(secs) => {
                let secs = secs.clamp(0.0, crate::player::engine::fade::MAX_FADE_SECS);
                self.config.fade_duration_secs = secs;
                if let Some(player) = &mut self.player {
                    player.set_fade_secs(secs);
                }
                self.save_config();
                Task::none()
            }
            PlaybackExtrasMessage::SetAutoPlayMode(mode) => {
                self.config.auto_play_mode = mode;
                self.save_config();
                self.top_up_queue()
            }
            PlaybackExtrasMessage::SetPartyMode(on) => {
                self.config.party_mode = on;
                self.save_config();
                if on { self.start_party() } else { Task::none() }
            }
            PlaybackExtrasMessage::TogglePartyMode => {
                let on = !self.config.party_mode;
                self.update_playback_extras(PlaybackExtrasMessage::SetPartyMode(on))
            }
            PlaybackExtrasMessage::TogglePartyGenre(genre) => {
                if let Some(pos) = self.config.party_genres.iter().position(|g| *g == genre) {
                    self.config.party_genres.remove(pos);
                } else {
                    self.config.party_genres.push(genre);
                }
                self.save_config();
                if self.config.party_mode {
                    // The upcoming queue was drawn from the old pool.
                    if let Some(player) = &mut self.player
                        && player.active_backend_type() == ActiveBackend::Local
                    {
                        player.queue_clear_upcoming();
                    }
                    self.playback_extras.top_up_dry = None;
                    return self.top_up_queue();
                }
                Task::none()
            }
            PlaybackExtrasMessage::SetNotifyTrackChange(on) => {
                self.config.notify_track_change = on;
                self.save_config();
                Task::none()
            }
            PlaybackExtrasMessage::SetInhibit(on) => {
                self.config.inhibit_while_playing = on;
                self.save_config();
                self.playback_extras.inhibit_retry_at = None;
                self.sync_inhibit()
            }
            PlaybackExtrasMessage::SetBackgroundPlayback(on) => {
                self.config.background_playback = on;
                self.save_config();
                Task::none()
            }
            PlaybackExtrasMessage::CloseRequested => self.handle_close_requested(),
            PlaybackExtrasMessage::InhibitAcquired(token) => {
                let wanted = self.wants_inhibit();
                match token {
                    Some(token) if wanted => {
                        self.playback_extras.inhibit = InhibitPhase::Held(token);
                        Task::none()
                    }
                    Some(token) => {
                        // Playback stopped while the request was in flight.
                        self.playback_extras.inhibit = InhibitPhase::Idle;
                        release_token(token)
                    }
                    None => {
                        self.playback_extras.inhibit = InhibitPhase::Idle;
                        self.playback_extras.inhibit_retry_at =
                            Some(Instant::now() + INHIBIT_RETRY);
                        Task::none()
                    }
                }
            }
            PlaybackExtrasMessage::NotificationSent(id) => {
                if let Some(id) = id {
                    self.playback_extras.notification_id = id;
                }
                Task::none()
            }
            PlaybackExtrasMessage::Noop => Task::none(),
            PlaybackExtrasMessage::Sync => self.playback_extras_tick(),
        }
    }

    /// Called from every `PlaybackTick`: keeps the queue topped up,
    /// the inhibit lock in step with the playback state, and the engine's
    /// fade duration in step with the (possibly externally edited) config.
    pub(super) fn playback_extras_tick(&mut self) -> AppTask {
        if let Some(player) = &mut self.player {
            player.set_fade_secs(self.config.fade_duration_secs);
        }
        Task::batch([self.top_up_queue(), self.sync_inhibit()])
    }

    // -- Auto-play / party ---------------------------------------------

    /// Append more music when the queue is about to run out (auto-random /
    /// auto-similar) or party mode wants its look-ahead refilled.
    ///
    /// Runs while the *last* queued track plays, so the engine's gapless
    /// look-ahead carries straight on into what is appended here.
    fn top_up_queue(&mut self) -> AppTask {
        let Some(player) = &mut self.player else {
            return Task::none();
        };
        if player.active_backend_type() != ActiveBackend::Local
            || player.state() != PlaybackState::Playing
            || player.queue_is_empty()
            || player.stop_after_current()
        {
            return Task::none();
        }
        let Some(current) = self.current_track.as_ref() else {
            return Task::none();
        };
        // Radio and podcasts have their own (endless / per-feed) flow.
        if matches!(&*current.provider_id, "radio" | "podcast") {
            return Task::none();
        }
        let upcoming = player.upcoming_len();
        let Some(kind) = party::plan_top_up(
            upcoming,
            player.repeat_mode(),
            self.config.party_mode,
            self.config.auto_play_mode,
        ) else {
            return Task::none();
        };
        if self.playback_extras.top_up_dry == Some((current.id, upcoming)) {
            return Task::none();
        }

        let exclude: HashSet<i64> = player.queue().iter().map(|t| t.id).collect();
        let picks = party::continuation(
            kind,
            Some(current),
            &self.all_tracks,
            &self.config.party_genres,
            &exclude,
            &mut self.playback_extras.rng,
        );
        if picks.is_empty() {
            self.playback_extras.top_up_dry = Some((current.id, upcoming));
            return Task::none();
        }
        self.playback_extras.top_up_dry = None;
        let added = picks.len();
        match player.queue_append(picks) {
            Ok(_) => tracing::debug!("auto-play: queued {added} track(s) ({kind:?})"),
            Err(e) => tracing::warn!("auto-play: failed to extend the queue: {e}"),
        }
        Task::none()
    }

    /// Party mode was just switched on: make the upcoming queue party
    /// material, starting playback from the pool if nothing is playing.
    fn start_party(&mut self) -> AppTask {
        self.playback_extras.top_up_dry = None;
        let Some(player) = &mut self.player else {
            return Task::none();
        };
        if player.active_backend_type() == ActiveBackend::Mpd {
            return Task::none();
        }
        let idle = player.queue_is_empty() || player.state() == PlaybackState::Stopped;
        if idle {
            let picks = party::continuation(
                party::TopUp::Party,
                None,
                &self.all_tracks,
                &self.config.party_genres,
                &HashSet::new(),
                &mut self.playback_extras.rng,
            );
            if picks.is_empty() {
                return Task::none();
            }
            return self.play_track_list(picks, 0);
        }
        player.queue_clear_upcoming();
        self.top_up_queue()
    }

    // -- Inhibit -------------------------------------------------------

    fn wants_inhibit(&self) -> bool {
        self.player.as_ref().is_some_and(|p| {
            inhibit::should_inhibit(
                self.config.inhibit_while_playing,
                p.state(),
                p.active_backend_type() == ActiveBackend::Local,
            )
        })
    }

    /// Acquire/release the suspend+idle inhibition to match playback.
    fn sync_inhibit(&mut self) -> AppTask {
        let want = self.wants_inhibit();
        match (&self.playback_extras.inhibit, want) {
            (InhibitPhase::Idle, true) => {
                if self
                    .playback_extras
                    .inhibit_retry_at
                    .is_some_and(|at| Instant::now() < at)
                {
                    return Task::none();
                }
                self.playback_extras.inhibit = InhibitPhase::Acquiring;
                cosmic::task::future(async {
                    let token = inhibit::acquire(inhibit::REASON).await;
                    cosmic::Action::App(Message::Playback(PlaybackExtrasMessage::InhibitAcquired(
                        token,
                    )))
                })
            }
            (InhibitPhase::Held(_), false) => {
                let InhibitPhase::Held(token) =
                    std::mem::replace(&mut self.playback_extras.inhibit, InhibitPhase::Idle)
                else {
                    return Task::none();
                };
                release_token(token)
            }
            _ => Task::none(),
        }
    }

    // -- Notifications -------------------------------------------------

    /// Pop a desktop notification for the track that just became current,
    /// when enabled and the window isn't focused.
    pub(super) fn notify_track_changed(&mut self) -> AppTask {
        let Some(track) = self.current_track.clone() else {
            return Task::none();
        };
        let playing = self
            .player
            .as_ref()
            .is_some_and(|p| p.state() == PlaybackState::Playing);
        let focused = self.core.focused_window().is_some();
        let key = notify::track_key(&track);
        if !notify::should_notify(
            self.config.notify_track_change,
            focused,
            playing,
            &key,
            self.playback_extras.last_notified.as_deref(),
        ) {
            return Task::none();
        }
        self.playback_extras.last_notified = Some(key);

        let artist = if track.album_artist.is_empty() {
            &track.artist
        } else {
            &track.album_artist
        };
        let cover_key = CoverArt::album_key(artist, &track.album);
        let cover_bytes = self.cover_art_bytes.get(&cover_key);
        // Not cached: load the bytes inside the blocking job below instead of
        // keeping every album's cover in memory.
        let lazy_cover = if cover_bytes.is_none() {
            self.registry
                .active_shared()
                .zip(self.cover_hint_for(artist, &track.album))
        } else {
            None
        };
        let replaces_id = self.playback_extras.notification_id;

        cosmic::task::future(async move {
            let for_cover = track.clone();
            let image = tokio::task::spawn_blocking(move || {
                let loaded;
                let bytes: Option<&[u8]> = match (&cover_bytes, &lazy_cover) {
                    (Some(b), _) => Some(b.as_slice()),
                    (None, Some((provider, hint))) => {
                        loaded = provider.get_cover_art(hint).ok().flatten();
                        loaded.as_deref()
                    }
                    (None, None) => None,
                };
                notify::prepare_cover(&for_cover, bytes)
            })
            .await
            .ok()
            .flatten();
            let (summary, body) = notify::notification_text(&track);
            let id = match notify::send(&summary, &body, image.as_deref(), replaces_id).await {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::debug!("track notification failed: {e}");
                    None
                }
            };
            cosmic::Action::App(Message::Playback(PlaybackExtrasMessage::NotificationSent(
                id,
            )))
        })
    }

    // -- Window lifecycle ----------------------------------------------

    /// Closing the window: keep playing in the background (minimized) when
    /// that is enabled and music is actually playing, otherwise quit.
    fn handle_close_requested(&mut self) -> AppTask {
        let playing = self
            .player
            .as_ref()
            .is_some_and(|p| p.state() == PlaybackState::Playing);
        if self.config.background_playback
            && playing
            && let Some(id) = self.core.main_window_id()
        {
            return cosmic::iced::window::minimize(id, true);
        }
        self.flush_config();
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

/// Release a held inhibition in the background.
fn release_token(token: inhibit::InhibitToken) -> AppTask {
    cosmic::task::future(async move {
        token.release().await;
        cosmic::Action::App(Message::Playback(PlaybackExtrasMessage::Noop))
    })
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
        if !matches!(self.playback_extras.inhibit, InhibitPhase::Idle) {
            subs.push(cosmic::iced::Subscription::run(sync_stream));
        }
        cosmic::iced::Subscription::batch(subs)
    }
}

fn sync_stream() -> impl futures_util::Stream<Item = Message> {
    use futures_util::SinkExt;
    cosmic::iced::stream::channel(
        1,
        |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            loop {
                interval.tick().await;
                _ = emitter
                    .send(Message::Playback(PlaybackExtrasMessage::Sync))
                    .await;
            }
        },
    )
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
        let cfg = &self.config;
        type Msg = PlaybackExtrasMessage;

        // Fade duration.
        let fade = cfg.fade_duration_secs;
        let fade_label = if fade < 0.05 {
            crate::fl!("fade-disabled")
        } else {
            crate::fl!("fade-seconds", secs = format!("{fade:.1}"))
        };
        let fade_item = widget::Column::new()
            .push(
                widget::Row::new()
                    .push(widget::text::body(crate::fl!("fade-duration")))
                    .push(widget::space::horizontal())
                    .push(widget::text::caption(fade_label).class(dim_text()))
                    .align_y(Alignment::Center),
            )
            .push(
                widget::slider(
                    0.0..=crate::player::engine::fade::MAX_FADE_SECS,
                    fade,
                    Msg::SetFadeDuration,
                )
                .step(0.1_f32)
                .width(Length::Fill),
            )
            .push(widget::text::caption(crate::fl!("fade-description")).class(dim_text()))
            .spacing(sp.space_xxs)
            .width(Length::Fill);

        // Auto-play mode (what plays when the queue runs out).
        let auto_labels = vec![
            crate::fl!("auto-play-off"),
            crate::fl!("auto-play-random"),
            crate::fl!("auto-play-similar"),
        ];
        let auto_selected = AutoPlayMode::ALL
            .iter()
            .position(|m| *m == cfg.auto_play_mode);
        let auto_item = widget::settings::item::builder(crate::fl!("auto-play"))
            .description(crate::fl!("auto-play-description"))
            .control(widget::dropdown(auto_labels, auto_selected, |i| {
                Msg::SetAutoPlayMode(AutoPlayMode::ALL[i])
            }));

        // Party mode + its genre pool.
        let party_item = widget::settings::item::builder(crate::fl!("party-mode"))
            .description(crate::fl!("party-mode-description"))
            .toggler(cfg.party_mode, Msg::SetPartyMode);

        let mut playback_section = widget::settings::section()
            .header(section_header(
                crate::fl!("settings-playback-extras"),
                crate::fl!("settings-playback-extras-description"),
            ))
            .add(fade_item)
            .add(auto_item)
            .add(party_item);

        if cfg.party_mode {
            let chips: Vec<cosmic::Element<'_, Msg>> = self
                .all_genres
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
                widget::text::caption(crate::fl!("party-genres-none"))
                    .class(dim_text())
                    .into()
            } else {
                widget::Column::new()
                    .push(widget::text::body(crate::fl!("party-genres")))
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
                crate::fl!("settings-desktop"),
                crate::fl!("settings-desktop-description"),
            ))
            .add(
                widget::settings::item::builder(crate::fl!("notify-track-change"))
                    .description(crate::fl!("notify-track-change-description"))
                    .toggler(cfg.notify_track_change, Msg::SetNotifyTrackChange),
            )
            .add(
                widget::settings::item::builder(crate::fl!("inhibit-suspend"))
                    .description(crate::fl!("inhibit-suspend-description"))
                    .toggler(cfg.inhibit_while_playing, Msg::SetInhibit),
            )
            .add(
                widget::settings::item::builder(crate::fl!("background-playback"))
                    .description(crate::fl!("background-playback-description"))
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
