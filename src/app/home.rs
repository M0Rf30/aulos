// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! App-side glue for the Home page: mapping Home messages to the session's
//! Home workflows (`aulos_core::session::home`) and resolving card clicks.

use super::{AppModel, ContextPage, Message, Page};
use crate::views::Route;
use crate::views::home::{HomeMessage, album_route};
use aulos_core::library::Track;
use aulos_core::library::home::artist_tracks;
use aulos_core::session::HomeEvent;
use aulos_core::task::Effect;
use cosmic::Application;
use cosmic::prelude::*;

/// Wraps a Home workflow's effect as an iced task.
fn run(effect: Effect<HomeEvent>) -> Task<cosmic::Action<Message>> {
    super::tasks::effect(effect).map(|event| cosmic::Action::App(Message::HomeEvent(event)))
}

impl AppModel {
    /// Switch the sidebar to `page` (no-op if it isn't in the nav).
    fn open_page(&mut self, page: Page) -> Task<cosmic::Action<Message>> {
        let target = self
            .nav
            .iter()
            .find(|&id| self.nav.data::<Page>(id) == Some(&page));
        match target {
            Some(id) => self.select_nav(id),
            None => Task::none(),
        }
    }

    /// Handle every `Message::Home`.
    pub(super) fn update_home(&mut self, msg: HomeMessage) -> Task<cosmic::Action<Message>> {
        match msg {
            HomeMessage::OpenAlbum { artist, name } => {
                return self.navigate(album_route(&artist, &name));
            }
            HomeMessage::OpenArtist(name) => return self.navigate(Route::Artist(name)),
            HomeMessage::PlayAlbum { artist, name } => {
                if let Some(idx) = self.session.library.find_album_exact(&artist, &name) {
                    let tracks = self.session.library.albums[idx].tracks.clone();
                    return self.play_track_list(tracks, 0);
                }
            }
            HomeMessage::AddMusicDir => return self.update(Message::AddMusicDir),
            HomeMessage::ConnectServer => {
                return self.update(Message::ToggleContextPage(ContextPage::Providers));
            }
            HomeMessage::OpenSettings => {
                return self.update(Message::ToggleContextPage(ContextPage::Settings));
            }
            HomeMessage::BrowseRadio => return self.open_page(Page::Radio),
            HomeMessage::DiscoverPodcasts => return self.open_page(Page::Podcasts),
            HomeMessage::PlayArtist(name) => {
                let tracks = artist_tracks(&self.session.library.albums, &name);
                if !tracks.is_empty() {
                    return self.play_track_list(tracks, 0);
                }
            }
            HomeMessage::Refresh => return self.load_home(false),
            HomeMessage::ShuffleRandom => return run(self.session.reroll_random_picks()),
            HomeMessage::OpenDecade(decade) => return run(self.session.open_decade(decade)),
            HomeMessage::CloseDecade => self.session.home.decade = None,
            HomeMessage::PlayDecade => {
                let tracks = self.session.decade_tracks();
                if !tracks.is_empty() {
                    return self.play_shuffled(tracks);
                }
            }
            HomeMessage::OpenUrl(url) => {
                if url.starts_with("https://") {
                    return self.update(Message::LaunchUrl(url));
                }
            }
            HomeMessage::Scrolled(area, offset) => self.extras.grid_scroll.set(area, offset),
        }
        Task::none()
    }

    /// Applies an async Home result to the session.
    pub(super) fn update_home_event(&mut self, event: HomeEvent) -> Task<cosmic::Action<Message>> {
        run(self.session.apply_home_event(event))
    }

    /// (Re)load every Home shelf.
    pub(super) fn load_home(&mut self, keep_picks: bool) -> Task<cosmic::Action<Message>> {
        run(self.session.load_home(keep_picks))
    }

    /// (Re)load the "Because you listened to …" shelves.
    pub(super) fn load_similar(&mut self) -> Task<cosmic::Action<Message>> {
        run(self.session.load_similar())
    }

    /// Called when the Home page is opened from the sidebar (or returned
    /// to).
    pub(super) fn enter_home(&mut self) -> Task<cosmic::Action<Message>> {
        run(self.session.enter_home())
    }

    /// Feed the playback tick into the play-history tracker.
    pub(super) fn track_play_history(&mut self) -> Task<cosmic::Action<Message>> {
        run(self.session.track_play_history())
    }

    /// Turn shuffle on and start `tracks` at a random position, like the
    /// genre page's shuffle.
    fn play_shuffled(&mut self, tracks: Vec<Track>) -> Task<cosmic::Action<Message>> {
        if !self.session.config.config.shuffle {
            self.session.config.config.shuffle = true;
            if let Some(player) = &mut self.session.playback.player {
                player.set_shuffle(true);
            }
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0);
        let start = nanos % tracks.len().max(1);
        self.play_track_list(tracks, start)
    }
}
