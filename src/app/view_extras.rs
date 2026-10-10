// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Mini-player mode and the Albums page filter-chip state.
//!
//! Mini-player mode shrinks the window to a small fixed-purpose player and
//! hides the header bar, navigation sidebar and context drawer; leaving it
//! restores the previous window size and chrome. A separate always-on-top
//! window is not attempted: libcosmic/Wayland offers no portable
//! always-on-top request, so resizing the one window is the robust option.

use super::{AppModel, Message};
use crate::player::PlaybackState;
use crate::views::album_filters::AlbumFilter;
use crate::views::mini_player::{self, MiniMsg};
use cosmic::iced::{Color, Length, Size, window};
use cosmic::prelude::*;
use cosmic::widget;
use std::time::Duration;

/// Window size while in mini-player mode.
const MINI_SIZE: Size = Size::new(360.0, 540.0);
/// Smallest window size allowed in mini-player mode.
const MINI_MIN: Size = Size::new(300.0, 160.0);
/// Minimum window size of the normal UI (mirrors `main.rs`).
const NORMAL_MIN: Size = Size::new(900.0, 600.0);
/// Size restored when the pre-mini size is unknown.
const FALLBACK_SIZE: Size = Size::new(1200.0, 800.0);

#[derive(Debug, Clone)]
pub enum MiniPlayerMsg {
    /// Enter or leave mini-player mode.
    Toggle,
    /// The current window size was read; enter mini-player mode.
    Sized(f32, f32),
    /// Start dragging the window.
    Drag,
}

/// Window/chrome state to put back when leaving mini-player mode.
#[derive(Debug, Clone, Copy)]
struct Restore {
    size: Size,
    nav_active: bool,
    show_context: bool,
}

#[derive(Debug, Default)]
pub struct ViewExtras {
    pub mini_player: bool,
    restore: Option<Restore>,
    /// Chip selection on the Albums page.
    pub album_filter: AlbumFilter,
}

impl AppModel {
    pub(super) fn update_mini_player(
        &mut self,
        msg: MiniPlayerMsg,
    ) -> Task<cosmic::Action<Message>> {
        match msg {
            MiniPlayerMsg::Toggle => {
                if self.extras.mini_player {
                    return self.leave_mini_player();
                }
                match self.core.main_window_id() {
                    Some(id) => window::size(id).map(|size| {
                        cosmic::Action::App(Message::Mini(MiniPlayerMsg::Sized(
                            size.width,
                            size.height,
                        )))
                    }),
                    None => self.enter_mini_player(FALLBACK_SIZE),
                }
            }
            MiniPlayerMsg::Sized(w, h) => self.enter_mini_player(Size::new(w, h)),
            MiniPlayerMsg::Drag => match self.core.main_window_id() {
                Some(id) => window::drag(id),
                None => Task::none(),
            },
        }
    }

    fn enter_mini_player(&mut self, current: Size) -> Task<cosmic::Action<Message>> {
        if self.extras.mini_player {
            return Task::none();
        }
        self.extras.mini_player = true;
        self.extras.restore = Some(Restore {
            size: current,
            nav_active: self.core.nav_bar_active(),
            show_context: self.core.window.show_context,
        });
        self.core.window.show_headerbar = false;
        self.core.window.show_context = false;
        self.core.nav_bar_set_toggled(false);

        let Some(id) = self.core.main_window_id() else {
            return Task::none();
        };
        window::maximize(id, false)
            .chain(window::set_min_size(id, Some(MINI_MIN)))
            .chain(window::resize(id, MINI_SIZE))
    }

    fn leave_mini_player(&mut self) -> Task<cosmic::Action<Message>> {
        self.extras.mini_player = false;
        let restore = self.extras.restore.take();
        self.core.window.show_headerbar = true;
        let nav_active = restore.is_none_or(|r| r.nav_active);
        self.core.nav_bar_set_toggled(nav_active);
        self.core.window.show_context = restore.is_some_and(|r| r.show_context);

        let Some(id) = self.core.main_window_id() else {
            return Task::none();
        };
        let size = restore.map_or(FALLBACK_SIZE, |r| r.size);
        let size = Size::new(
            size.width.max(NORMAL_MIN.width),
            size.height.max(NORMAL_MIN.height),
        );
        window::set_min_size(id, Some(NORMAL_MIN)).chain(window::resize(id, size))
    }

    /// The whole window content while in mini-player mode.
    pub(super) fn mini_player_view(&self) -> Element<'_, Message> {
        let track = self.current_track.as_ref();
        let state = self
            .player
            .as_ref()
            .map(|p| p.state())
            .unwrap_or(PlaybackState::Stopped);
        let duration = track.map(|t| t.duration).unwrap_or(Duration::ZERO);
        let (progress, seeking) = match self.seeking_preview {
            Some(fraction) => (fraction, true),
            None if duration > Duration::ZERO => (
                (self.playback_position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0),
                false,
            ),
            None => (0.0, false),
        };
        let cover = track.and_then(|t| {
            let artist = if t.album_artist.is_empty() {
                &t.artist
            } else {
                &t.album_artist
            };
            self.cover_images
                .get(&crate::library::CoverArt::album_key(artist, &t.album))
                .cloned()
        });

        let props = mini_player::MiniProps {
            title: track.map(|t| t.title.clone()).unwrap_or_default(),
            subtitle: track
                .map(|t| match (t.artist.is_empty(), t.album.is_empty()) {
                    (false, false) => format!("{} — {}", t.artist, t.album),
                    (false, true) => t.artist.clone(),
                    (true, false) => t.album.clone(),
                    (true, true) => String::new(),
                })
                .unwrap_or_default(),
            track_id: track.map(|t| t.id.to_string()),
            is_favorite: track.is_some_and(|t| t.is_favorite),
            has_track: track.is_some(),
            is_live: track.is_some_and(crate::views::now_playing::is_live_stream),
            cover,
            playing: state == PlaybackState::Playing,
            progress,
            position: self.playback_position,
            duration,
            seeking,
            accent: self
                .accent
                .as_ref()
                .map(|a| Color::from_rgb(a.color[0], a.color[1], a.color[2])),
        };

        let content = mini_player::view(props).map(|msg| match msg {
            MiniMsg::Drag => Message::Mini(MiniPlayerMsg::Drag),
            MiniMsg::Restore => Message::Mini(MiniPlayerMsg::Toggle),
            MiniMsg::Close => Message::Quit,
            MiniMsg::TogglePlayback => Message::TogglePlayback,
            MiniMsg::Previous => Message::PreviousTrack,
            MiniMsg::Next => Message::NextTrack,
            MiniMsg::SeekPreview(v) => Message::SeekPreview(v),
            MiniMsg::SeekCommit => Message::SeekCommit,
            MiniMsg::ToggleFavorite(id) => Message::ToggleFavorite(id),
        });

        let background = widget::container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::WindowBackground);
        widget::toaster(&self.toasts, background)
    }
}
