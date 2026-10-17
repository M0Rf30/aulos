// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Home page workflows: loading the shelves from the play history, the
//! "Because you listened to …" shelves, the decade grid, and recording plays
//! into the history.
//!
//! Workflows return an [`Effect`] of [`HomeEvent`]s; the UI feeds each event
//! back through [`Session::apply_home_event`].

use super::Session;
use crate::library::history::{AlbumRef, HomeData, LOCAL_PROVIDER, SHELF_LEN, exclude_albums};
use crate::library::home::{decade_albums, home_from_albums, random_album_refs};
use crate::library::home_state::DecadeView;
use crate::library::{LibraryDb, Track};
use crate::online::scrobble::Service;
use crate::online::scrobble::controller::is_connected;
use crate::online::similar::{self, SimilarShelf, SuggestParams};
use crate::player::PlaybackState as PlayerState;
use crate::provider::ProviderType;
use crate::task::{Effect, channel_stream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Async results of the Home workflows.
#[derive(Debug, Clone)]
pub enum HomeEvent {
    /// Shelves loaded; `request` ties the result to the latest load. With
    /// `keep_picks` the previous random/rediscover shelves are kept, so
    /// returning to Home doesn't reshuffle what the user just saw.
    Loaded {
        request: u64,
        keep_picks: bool,
        data: Box<HomeData>,
    },
    /// A "Because you listened to …" shelf is ready; `request` ties it to
    /// the Home load it was computed for.
    SimilarShelf {
        request: u64,
        shelf: Box<SimilarShelf>,
    },
    /// All similar-artist shelves of `request` were delivered (`count` of
    /// them) or the lookup gave up (offline).
    SimilarDone { request: u64, count: usize },
    /// A play was written to the history.
    PlayRecorded,
    /// A re-rolled random shelf.
    RandomLoaded { request: u64, albums: Vec<AlbumRef> },
    /// Albums of a decade.
    DecadeLoaded { decade: u32, albums: Vec<AlbumRef> },
}

impl Session {
    /// The name the Home greeting addresses (empty: no name). Stored as
    /// typed; [`crate::library::home_state::Greeting::text_for`] trims it.
    pub fn set_user_name(&mut self, name: &str) {
        self.config.config.user_name = name.to_string();
        self.config.save();
    }

    /// Whether the active provider's library lives in the library database
    /// (and therefore has play history).
    #[must_use]
    pub fn home_uses_database(&self) -> bool {
        self.providers.registry.active_id() == LOCAL_PROVIDER
    }

    /// The provider whose library the "Because you listened to …" shelves are
    /// matched against: the active one when its library is in the library
    /// database (local files, or a remote server with cached rows). `None`
    /// for providers without cached rows (MPD).
    #[must_use]
    pub fn suggestions_provider(&self) -> Option<&str> {
        let registry = &self.providers.registry;
        matches!(
            registry.active()?.provider_type(),
            ProviderType::Local | ProviderType::Subsonic
        )
        .then(|| registry.active_id())
    }

    /// Whether "Because you listened to …" shelves should be loaded: the
    /// setting is on (default) and a scrobbling service is connected.
    #[must_use]
    pub fn suggestions_enabled(&self) -> bool {
        self.config.config.home_online_suggestions
            && Service::ALL
                .iter()
                .any(|s| is_connected(&self.config.config, *s))
    }

    /// (Re)loads every Home shelf. Library-database providers query the play
    /// history on a blocking thread; remote providers get a history-less
    /// Home built from the albums already in memory.
    pub fn load_home(&mut self, keep_picks: bool) -> Effect<HomeEvent> {
        self.home.request += 1;
        let request = self.home.request;

        if !self.home_uses_database() {
            self.home.data = Some(home_from_albums(&self.library.albums, crate::unix_now()));
            return self.load_similar();
        }

        let db_path = crate::paths::library_db_path();
        Effect::future(async move {
            let data = crate::task::spawn_blocking(move || {
                LibraryDb::open(&db_path)
                    .and_then(|db| db.home_data(LOCAL_PROVIDER, crate::unix_now()))
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
            HomeEvent::Loaded {
                request,
                keep_picks,
                data: Box::new(data),
            }
        })
    }

    /// Called when the Home page is opened from the sidebar (or returned
    /// to): leaves any decade grid and refreshes the shelves, keeping the
    /// random picks stable.
    pub fn enter_home(&mut self) -> Effect<HomeEvent> {
        self.home.decade = None;
        // Database-backed shelves are reused while nothing they depend on
        // changed (no play recorded, same provider, within the TTL): the
        // library-load and import paths call `load_home` directly, so those
        // still refresh.
        if self.home_uses_database()
            && self.home.data.is_some()
            && self.home.cache.is_fresh(
                self.providers.registry.active_id(),
                self.suggestions_enabled(),
                web_time::Instant::now(),
            )
        {
            return Effect::none();
        }
        self.load_home(true)
    }

    /// Re-rolls only the "Random picks" shelf.
    pub fn reroll_random_picks(&mut self) -> Effect<HomeEvent> {
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
            return Effect::none();
        }

        let request = self.home.request;
        let db_path = crate::paths::library_db_path();
        Effect::future(async move {
            let picks = crate::task::spawn_blocking(move || {
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
            HomeEvent::RandomLoaded { request, albums }
        })
    }

    /// Opens the decade grid and loads the albums of `decade` for it.
    pub fn open_decade(&mut self, decade: u32) -> Effect<HomeEvent> {
        self.home.decade = Some(DecadeView {
            decade,
            albums: None,
        });
        if !self.home_uses_database() {
            if let Some(view) = &mut self.home.decade {
                view.albums = Some(decade_albums(&self.library.albums, decade));
            }
            return Effect::none();
        }

        let db_path = crate::paths::library_db_path();
        Effect::future(async move {
            let albums = crate::task::spawn_blocking(move || {
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
            HomeEvent::DecadeLoaded { decade, albums }
        })
    }

    /// Every track of the albums of the open decade grid, album by album.
    #[must_use]
    pub fn decade_tracks(&self) -> Vec<Track> {
        self.home
            .decade
            .as_ref()
            .and_then(|view| view.albums.as_ref())
            .map(|albums| {
                albums
                    .iter()
                    .filter_map(|a| self.library.find_album_exact(&a.artist, &a.name))
                    .flat_map(|idx| {
                        self.library.albums[idx]
                            .tracks
                            .iter()
                            .map(|t| Track::clone(t))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// (Re)loads the similar-artist shelves in the background, one shelf at
    /// a time as they become available. Cancels a lookup still in flight.
    pub fn load_similar(&mut self) -> Effect<HomeEvent> {
        self.home.similar_cancel.store(true, Ordering::Relaxed);
        let Some(provider) = self
            .suggestions_provider()
            .filter(|_| self.suggestions_enabled())
            .map(str::to_owned)
        else {
            self.home.similar.clear();
            return Effect::none();
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.home.similar_cancel = Arc::clone(&cancel);
        let params = SuggestParams {
            provider,
            db_path: crate::paths::library_db_path(),
            cache_path: similar::cache_path(),
            lastfm_key: self.config.config.scrobble_lastfm_api_key.clone(),
            now: crate::unix_now(),
        };
        similar_effect(params, self.home.request, cancel)
    }

    /// Applies an async Home result; returns the follow-up work to run.
    pub fn apply_home_event(&mut self, event: HomeEvent) -> Effect<HomeEvent> {
        match event {
            HomeEvent::Loaded {
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
                    self.home.cache.mark_loaded(
                        self.providers.registry.active_id(),
                        self.suggestions_enabled(),
                        web_time::Instant::now(),
                    );
                    return self.load_similar();
                }
            }
            HomeEvent::RandomLoaded { request, albums } => {
                if request == self.home.request
                    && let Some(data) = &mut self.home.data
                {
                    data.random = albums;
                }
            }
            HomeEvent::DecadeLoaded { decade, albums } => {
                if let Some(view) = &mut self.home.decade
                    && view.decade == decade
                {
                    view.albums = Some(albums);
                }
            }
            HomeEvent::SimilarShelf { request, shelf } => {
                if request == self.home.request {
                    if self.home.similar_request != request {
                        self.home.similar.clear();
                        self.home.similar_request = request;
                    }
                    self.home.similar.push(*shelf);
                }
            }
            HomeEvent::SimilarDone { request, count } => {
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
            HomeEvent::PlayRecorded => self.home.cache.invalidate(),
        }
        Effect::none()
    }

    /// Feeds the playback tick into the play-history tracker; once the
    /// current track has been listened to for half its length (or four
    /// minutes), writes one play to the library database.
    ///
    /// Only database-backed tracks are counted — remote tracks (MPD,
    /// Subsonic), radio and podcasts have no stable library id.
    pub fn track_play_history(&mut self) -> Effect<HomeEvent> {
        let Some(track) = &self.playback.current_track else {
            self.home.tracked_track = None;
            return Effect::none();
        };
        if track.id <= 0 || &*track.provider_id != LOCAL_PROVIDER {
            return Effect::none();
        }
        let (id, duration) = (track.id, track.duration);

        if self.home.tracked_track != Some(id) {
            self.home.tracker.reset();
            self.home.tracked_track = Some(id);
        }
        let playing = self
            .playback
            .player
            .as_ref()
            .is_some_and(|p| p.state() == PlayerState::Playing);

        if !self
            .home
            .tracker
            .observe(self.playback.position, duration, playing)
        {
            return Effect::none();
        }

        let db_path = crate::paths::library_db_path();
        Effect::future(async move {
            let result = crate::task::spawn_blocking(move || {
                LibraryDb::open(&db_path).and_then(|db| db.record_play(id, crate::unix_now()))
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("Failed to record play: {e}"),
                Err(e) => tracing::warn!("Record-play task failed: {e}"),
            }
            HomeEvent::PlayRecorded
        })
    }
}

/// Computes the similar-artist shelves on the blocking pool, streaming each
/// shelf as a [`HomeEvent`] and finishing with `SimilarDone`.
fn similar_effect(
    params: SuggestParams,
    request: u64,
    cancel: Arc<AtomicBool>,
) -> Effect<HomeEvent> {
    Effect::stream(channel_stream(8, move |mut out| async move {
        use futures::SinkExt;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HomeEvent>();
        let worker = crate::task::spawn_blocking(move || {
            let count = {
                let mut on_shelf = |shelf| {
                    let _ = tx.send(HomeEvent::SimilarShelf {
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
            let _ = tx.send(HomeEvent::SimilarDone { request, count });
        });
        while let Some(event) = rx.recv().await {
            if out.send(event).await.is_err() {
                return;
            }
        }
        let _ = worker.await;
    }))
}
