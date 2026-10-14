// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! App-side logic for the Home page: loading the shelves, resolving card
//! clicks, and recording plays into the library's play history.

use super::{AppModel, ContextPage, Message, Page};
use crate::views::Route;
use crate::views::home::{HomeMessage, album_route};
use aulos_core::library::history::{AlbumRef, HomeData, LOCAL_PROVIDER, SHELF_LEN, exclude_albums};
use aulos_core::library::home::{
    artist_tracks, decade_albums, home_from_albums, random_album_refs,
};
use aulos_core::library::home_state::DecadeView;
use aulos_core::library::{LibraryDb, Track};
use aulos_core::online::scrobble::Service;
use aulos_core::online::scrobble::controller::is_connected;
use aulos_core::online::similar::{self, SuggestParams};
use aulos_core::player::PlaybackState;
use cosmic::Application;
use cosmic::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

impl AppModel {
    /// Whether the active provider's library lives in the library database
    /// (and therefore has play history).
    fn home_uses_database(&self) -> bool {
        self.registry.active_id() == LOCAL_PROVIDER
    }

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
                if let Some(idx) = self.library.find_album_exact(&artist, &name) {
                    let tracks = self.library.albums[idx].tracks.clone();
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
                let tracks = artist_tracks(&self.library.albums, &name);
                if !tracks.is_empty() {
                    return self.play_track_list(tracks, 0);
                }
            }
            HomeMessage::Refresh => return self.load_home(false),
            HomeMessage::ShuffleRandom => return self.reroll_random_picks(),
            HomeMessage::OpenDecade(decade) => {
                self.home.decade = Some(DecadeView {
                    decade,
                    albums: None,
                });
                return self.load_decade(decade);
            }
            HomeMessage::CloseDecade => self.home.decade = None,
            HomeMessage::PlayDecade => {
                let tracks: Vec<Track> = self
                    .home
                    .decade
                    .as_ref()
                    .and_then(|view| view.albums.as_ref())
                    .map(|albums| {
                        albums
                            .iter()
                            .filter_map(|a| self.library.find_album_exact(&a.artist, &a.name))
                            .flat_map(|idx| self.library.albums[idx].tracks.iter().cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                if !tracks.is_empty() {
                    return self.play_shuffled(tracks);
                }
            }
            HomeMessage::Loaded {
                request,
                keep_picks,
                data,
            } => {
                if request == self.home.request {
                    let mut data = *data;
                    if keep_picks && let Some(old) = &self.home.data {
                        if !old.random.is_empty() {
                            data.random = old.random.clone();
                        }
                        if !old.rediscover.is_empty() && !data.rediscover.is_empty() {
                            data.rediscover = old.rediscover.clone();
                        }
                    }
                    self.home.data = Some(data);
                    self.home_cache.mark_loaded(
                        self.registry.active_id(),
                        self.suggestions_enabled(),
                        std::time::Instant::now(),
                    );
                    return self.load_similar();
                }
            }
            HomeMessage::RandomLoaded { request, albums } => {
                if request == self.home.request
                    && let Some(data) = &mut self.home.data
                {
                    data.random = albums;
                }
            }
            HomeMessage::DecadeLoaded { decade, albums } => {
                if let Some(view) = &mut self.home.decade
                    && view.decade == decade
                {
                    view.albums = Some(albums);
                }
            }
            HomeMessage::OpenUrl(url) => {
                if url.starts_with("https://") {
                    return self.update(Message::LaunchUrl(url));
                }
            }
            HomeMessage::SimilarShelf { request, shelf } => {
                if request == self.home.request {
                    if self.home.similar_request != request {
                        self.home.similar.clear();
                        self.home.similar_request = request;
                    }
                    self.home.similar.push(*shelf);
                }
            }
            HomeMessage::SimilarDone { request, count } => {
                // Nothing came back (offline, no taste data): show no shelves
                // rather than the previous visit's.
                if request == self.home.request
                    && count == 0
                    && self.home.similar_request != request
                {
                    self.home.similar.clear();
                    self.home.similar_request = request;
                }
            }
            HomeMessage::PlayRecorded => self.home_cache.invalidate(),
            HomeMessage::Scrolled(area, offset) => self.extras.grid_scroll.set(area, offset),
        }
        Task::none()
    }

    /// Whether "Because you listened to …" shelves should be loaded: the
    /// setting is on (default) and a scrobbling service is connected.
    fn suggestions_enabled(&self) -> bool {
        self.config.home_online_suggestions
            && Service::ALL.iter().any(|s| is_connected(&self.config, *s))
    }

    /// (Re)load the similar-artist shelves in the background, one shelf at a
    /// time as they become available. Cancels a lookup still in flight.
    pub(super) fn load_similar(&mut self) -> Task<cosmic::Action<Message>> {
        self.home.similar_cancel.store(true, Ordering::Relaxed);
        if !self.suggestions_enabled() || !self.home_uses_database() {
            self.home.similar.clear();
            return Task::none();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.home.similar_cancel = Arc::clone(&cancel);
        let params = SuggestParams {
            db_path: aulos_core::paths::library_db_path(),
            cache_path: similar::cache_path(),
            lastfm_key: self.config.scrobble_lastfm_api_key.clone(),
            now: aulos_core::unix_now(),
        };
        cosmic::task::stream(similar_stream(params, self.home.request, cancel))
    }

    /// (Re)load every Home shelf. Library-database providers query the play
    /// history on a blocking thread; remote providers get a history-less
    /// Home built from the albums already in memory.
    pub(super) fn load_home(&mut self, keep_picks: bool) -> Task<cosmic::Action<Message>> {
        self.home.request += 1;
        let request = self.home.request;

        if !self.home_uses_database() {
            self.home.data = Some(home_from_albums(&self.library.albums));
            return Task::none();
        }

        let db_path = aulos_core::paths::library_db_path();
        cosmic::task::future(async move {
            let data = tokio::task::spawn_blocking(move || {
                LibraryDb::open(&db_path)
                    .and_then(|db| db.home_data(LOCAL_PROVIDER, aulos_core::unix_now()))
            })
            .await;
            let data = match data {
                Ok(Ok(data)) => data,
                Ok(Err(e)) => {
                    tracing::warn!("Home data failed to load: {e}");
                    HomeData::default()
                }
                Err(e) => {
                    tracing::warn!("Home data task failed: {e}");
                    HomeData::default()
                }
            };
            cosmic::Action::App(Message::Home(HomeMessage::Loaded {
                request,
                keep_picks,
                data: Box::new(data),
            }))
        })
    }

    /// Called when the Home page is opened from the sidebar (or returned
    /// to): leave any decade grid and refresh the shelves, keeping the
    /// random picks stable.
    pub(super) fn enter_home(&mut self) -> Task<cosmic::Action<Message>> {
        self.home.decade = None;
        // Database-backed shelves are reused while nothing they depend on
        // changed (no play recorded, same provider, within the TTL): the
        // library-load and import paths call `load_home` directly, so those
        // still refresh.
        if self.home_uses_database()
            && self.home.data.is_some()
            && self.home_cache.is_fresh(
                self.registry.active_id(),
                self.suggestions_enabled(),
                std::time::Instant::now(),
            )
        {
            return Task::none();
        }
        self.load_home(true)
    }

    /// Re-roll only the "Random picks" shelf.
    fn reroll_random_picks(&mut self) -> Task<cosmic::Action<Message>> {
        let exclude: Vec<AlbumRef> = self
            .home
            .data
            .as_ref()
            .map(|d| d.rediscover.clone())
            .unwrap_or_default();

        if !self.home_uses_database() {
            let mut picks = random_album_refs(&self.library.albums, SHELF_LEN as usize * 2);
            exclude_albums(&mut picks, &exclude, SHELF_LEN as usize);
            if let Some(data) = &mut self.home.data {
                data.random = picks;
            }
            return Task::none();
        }

        let request = self.home.request;
        let db_path = aulos_core::paths::library_db_path();
        cosmic::task::future(async move {
            let picks = tokio::task::spawn_blocking(move || {
                LibraryDb::open(&db_path)
                    .and_then(|db| db.random_albums(LOCAL_PROVIDER, SHELF_LEN * 2))
            })
            .await;
            let mut albums = match picks {
                Ok(Ok(albums)) => albums,
                Ok(Err(e)) => {
                    tracing::warn!("Random picks failed to load: {e}");
                    Vec::new()
                }
                Err(e) => {
                    tracing::warn!("Random picks task failed: {e}");
                    Vec::new()
                }
            };
            exclude_albums(&mut albums, &exclude, SHELF_LEN as usize);
            cosmic::Action::App(Message::Home(HomeMessage::RandomLoaded { request, albums }))
        })
    }

    /// Load the albums of one decade for the decade grid.
    fn load_decade(&mut self, decade: u32) -> Task<cosmic::Action<Message>> {
        if !self.home_uses_database() {
            if let Some(view) = &mut self.home.decade {
                view.albums = Some(decade_albums(&self.library.albums, decade));
            }
            return Task::none();
        }

        let db_path = aulos_core::paths::library_db_path();
        cosmic::task::future(async move {
            let albums = tokio::task::spawn_blocking(move || {
                LibraryDb::open(&db_path).and_then(|db| db.albums_by_decade(LOCAL_PROVIDER, decade))
            })
            .await;
            let albums = match albums {
                Ok(Ok(albums)) => albums,
                Ok(Err(e)) => {
                    tracing::warn!("Decade albums failed to load: {e}");
                    Vec::new()
                }
                Err(e) => {
                    tracing::warn!("Decade albums task failed: {e}");
                    Vec::new()
                }
            };
            cosmic::Action::App(Message::Home(HomeMessage::DecadeLoaded { decade, albums }))
        })
    }

    /// Turn shuffle on and start `tracks` at a random position, like the
    /// genre page's shuffle.
    fn play_shuffled(&mut self, tracks: Vec<Track>) -> Task<cosmic::Action<Message>> {
        if !self.config.shuffle {
            self.config.shuffle = true;
            if let Some(player) = &mut self.player {
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

    /// Feed the playback tick into the play-history tracker; once the
    /// current track has been listened to for half its length (or four
    /// minutes), write one play to the library database.
    ///
    /// Only database-backed tracks are counted — remote tracks (MPD,
    /// Subsonic), radio and podcasts have no stable library id.
    pub(super) fn track_play_history(&mut self) -> Task<cosmic::Action<Message>> {
        let Some(track) = &self.current_track else {
            self.home.tracked_track = None;
            return Task::none();
        };
        if track.id <= 0 || &*track.provider_id != LOCAL_PROVIDER {
            return Task::none();
        }
        let (id, duration) = (track.id, track.duration);

        if self.home.tracked_track != Some(id) {
            self.home.tracker.reset();
            self.home.tracked_track = Some(id);
        }
        let playing = self
            .player
            .as_ref()
            .is_some_and(|p| p.state() == PlaybackState::Playing);

        if !self
            .home
            .tracker
            .observe(self.playback_position, duration, playing)
        {
            return Task::none();
        }

        let db_path = aulos_core::paths::library_db_path();
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                LibraryDb::open(&db_path).and_then(|db| db.record_play(id, aulos_core::unix_now()))
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("Failed to record play: {e}"),
                Err(e) => tracing::warn!("Record-play task failed: {e}"),
            }
            cosmic::Action::App(Message::Home(HomeMessage::PlayRecorded))
        })
    }
}

/// Compute the similar-artist shelves on the blocking pool, streaming each
/// shelf as a `HomeMessage` and finishing with `SimilarDone`.
fn similar_stream(
    params: SuggestParams,
    request: u64,
    cancel: Arc<AtomicBool>,
) -> impl cosmic::iced::futures::Stream<Item = cosmic::Action<Message>> {
    use cosmic::iced::futures::SinkExt;
    cosmic::iced::stream::channel(
        8,
        move |mut out: cosmic::iced::futures::channel::mpsc::Sender<cosmic::Action<Message>>| async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HomeMessage>();
            let worker = tokio::task::spawn_blocking(move || {
                let count = {
                    let mut on_shelf = |shelf| {
                        let _ = tx.send(HomeMessage::SimilarShelf {
                            request,
                            shelf: Box::new(shelf),
                        });
                    };
                    match similar::compute_shelves(&params, &cancel, &mut on_shelf) {
                        Ok(count) => count,
                        Err(e) => {
                            tracing::warn!("similar-artist shelves failed: {e}");
                            0
                        }
                    }
                };
                let _ = tx.send(HomeMessage::SimilarDone { request, count });
            });
            while let Some(msg) = rx.recv().await {
                if out
                    .send(cosmic::Action::App(Message::Home(msg)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _ = worker.await;
        },
    )
}
