// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Library workflows of a [`Session`]: scanning and (re)loading the active
//! provider's library, applying the async results (with the staleness rules
//! that make a provider switch or a newer reload supersede in-flight work),
//! watcher-triggered rescans, genres, favourite/rating changes against the
//! provider, and the header search (query, debounce and result refresh).
//!
//! The cover thumbnails and the search results live in the library state
//! ([`crate::library::covers::CoverStore`], `visible_*`); the UI keeps only
//! what is not domain state (image handles, folder tree). Reloads and
//! [`LibraryApplied`] outcomes carry a [`CoverDelta`] saying which handles
//! to rebuild or drop.

use super::Session;
use crate::library::album_sort::AlbumSort;
use crate::library::covers::{CachedThumb, CoverDelta, CoverSet, CoverThumb, ThumbStamp};
use crate::library::search::SEARCH_DEBOUNCE_MS;
use crate::library::state::SortField;
use crate::library::top_tracks::{ArtistTop, TOP_TRACKS_LEN, rank_top_tracks};
use crate::library::{Album, CoverArt, LibraryDb, LibraryScanner, Track};
use crate::provider::{MusicProvider, ProviderType};
use crate::task::Effect;
#[cfg(not(target_arch = "wasm32"))]
use crate::task::channel_stream;
#[cfg(not(target_arch = "wasm32"))]
use futures::SinkExt;
#[cfg(not(target_arch = "wasm32"))]
use futures::channel::mpsc;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Concurrent cover decodes while loading the local library.
#[cfg(not(target_arch = "wasm32"))]
const COVER_DECODE_CONCURRENCY: usize = 4;

/// Albums fetched per batch from a remote provider.
#[cfg(not(target_arch = "wasm32"))]
const BATCH_SIZE: usize = 50;

/// Async results of the library workflows; feed them to
/// [`Session::apply_library_event`].
#[derive(Debug, Clone)]
pub enum LibraryEvent {
    /// A scan finished; `count` tracks were updated. `generation` and
    /// `provider_id` identify the reload/scan it belongs to, so stale
    /// results (superseded by a later reload or a provider switch) are
    /// ignored.
    ScanComplete {
        generation: u64,
        provider_id: String,
        count: usize,
    },
    /// The whole library of a database-backed provider, in one piece.
    Loaded {
        generation: u64,
        provider_id: String,
        tracks: Vec<Track>,
        albums: Vec<Album>,
        covers: CoverSet,
    },
    /// An incremental batch of albums from a remote provider.
    Batch {
        generation: u64,
        provider_id: String,
        albums: Vec<Album>,
        covers: CoverSet,
    },
    /// Incremental loading is complete.
    LoadComplete {
        generation: u64,
        provider_id: String,
    },
    /// The play-count ranking of an artist's tracks (see
    /// [`Session::load_artist_top`]).
    TopTracksLoaded {
        artist: String,
        tracks: Vec<Track>,
    },
    GenresLoaded(Vec<String>),
    GenreTracksLoaded {
        idx: usize,
        tracks: Vec<Track>,
    },
    /// The debounce of the search-query change that bumped the debounce
    /// counter to this value elapsed; ignored if a newer change arrived.
    SearchDebounced(u64),
    /// Result of a favourite toggle; `optimistic` is the state already
    /// applied locally.
    FavoriteToggled {
        track_id: String,
        optimistic: bool,
        result: Result<bool, String>,
    },
    /// Result of a rating change; `previous` is restored on error.
    RatingSet {
        track_id: String,
        previous: Option<u8>,
        result: Result<(), String>,
    },
}

/// A started (or skipped) reload/scan.
#[must_use = "the effect does nothing unless the UI runs it, and `covers` must be applied first"]
pub struct Reload {
    /// What starting the reload did to the covers (a reload with nothing
    /// loaded drops them all; batches then fill them in progressively).
    pub covers: CoverDelta,
    pub effect: Effect<LibraryEvent>,
}

impl Reload {
    /// Nothing started.
    pub fn none() -> Self {
        Self::effect(Effect::none())
    }

    fn effect(effect: Effect<LibraryEvent>) -> Self {
        Self {
            covers: CoverDelta::default(),
            effect,
        }
    }
}

/// What [`Session::apply_library_event`] did, for the UI to follow up on.
pub enum LibraryApplied {
    /// Superseded by a later reload/scan or a provider switch; ignored.
    Stale,
    /// Applied; nothing for the UI to do.
    Done,
    /// A scan finished; `reload` re-loads the library when the scan changed
    /// anything.
    ScanComplete { reload: Reload },
    /// The visible lists were replaced wholesale.
    Loaded { covers: CoverDelta },
    /// A batch arrived. `staged`: it went to the staging buffer (nothing
    /// visible changed), otherwise it was appended to the visible lists.
    Batch { covers: CoverDelta, staged: bool },
    /// The reload finished and the final sort ran; `covers` is the swap of
    /// a staged refresh into the visible covers (empty without one).
    Completed { covers: CoverDelta },
    /// A provider action failed and its optimistic local change was undone.
    ActionFailed(String),
}

impl Session {
    /// True when an async library result tagged with `generation`/
    /// `provider_id` is stale: superseded by a later reload/scan or a
    /// provider switch.
    #[must_use]
    pub fn is_stale_reload(&self, generation: u64, provider_id: &str) -> bool {
        self.library
            .is_stale_reload(self.providers.registry.active_id(), generation, provider_id)
    }

    /// Apply `f` to every copy of the track with `track_id`: the library
    /// lists and the current track.
    fn for_each_track(&mut self, track_id: &str, mut f: impl FnMut(&mut Track)) {
        self.library.for_each_track(track_id, &mut f);
        if let Some(ct) = self.playback.current_track.as_mut()
            && ct.id.to_string() == track_id
        {
            f(ct);
        }
    }

    pub fn set_favorite_local(&mut self, track_id: &str, state: bool) {
        let now = crate::unix_now();
        self.for_each_track(track_id, |t| {
            t.is_favorite = state;
            t.favorited_at = if state { now } else { 0 };
        });
    }

    pub fn set_rating_local(&mut self, track_id: &str, rating: Option<u8>) {
        self.for_each_track(track_id, |t| t.rating = rating);
    }

    /// The header-search query changed (as the user types). A non-empty
    /// query refreshes the results after [`SEARCH_DEBOUNCE_MS`] of quiet
    /// (the returned effect; later changes supersede it); an empty one
    /// refreshes at once. Read the results through
    /// `LibraryState::visible_*`.
    pub fn set_search_query(&mut self, query: String) -> Effect<LibraryEvent> {
        match self.library.begin_search(query) {
            Some(generation) => Effect::future(async move {
                crate::task::sleep(Duration::from_millis(SEARCH_DEBOUNCE_MS)).await;
                LibraryEvent::SearchDebounced(generation)
            }),
            None => {
                self.refresh_search();
                Effect::none()
            }
        }
    }

    /// Drop the header-search query and show everything again.
    pub fn clear_search(&mut self) {
        // An empty query never schedules anything.
        drop(self.set_search_query(String::new()));
    }

    /// Recompute the search results for the current query, library data,
    /// playlists and compilation visibility. Done automatically whenever
    /// one of them changes through the session; call it after changing
    /// `library`, `playlists` or `config` directly.
    pub fn refresh_search(&mut self) {
        self.library.refresh_search(
            &self.playlists.playlists,
            !self.config.config.show_compilations_in_artists,
        );
    }

    /// Rebuild the artist index with the configured split rules.
    pub(super) fn rebuild_artists(&mut self) {
        self.library.rebuild_artists(&self.config.config);
        self.refresh_search();
    }

    /// Sort the library's tracks by `field`.
    pub fn sort_tracks(&mut self, field: SortField) {
        self.library.sort_tracks(field);
        self.refresh_search();
    }

    /// Show or hide "Various Artists" compilations in the Artists view.
    pub fn set_show_compilations_in_artists(&mut self, show: bool) {
        self.config.config.show_compilations_in_artists = show;
        self.config.save();
        self.refresh_search();
    }

    /// Order the Albums page by `sort` (and remember it).
    pub fn set_albums_sort(&mut self, sort: AlbumSort) {
        self.config.config.albums_sort = sort;
        self.config.save();
        self.resort_library();
    }

    /// Order the Artists page by `sort` (and remember it).
    pub fn set_artists_sort(&mut self, sort: AlbumSort) {
        self.config.config.artists_sort = sort;
        self.config.save();
        self.resort_library();
    }

    /// Order the Genres page by `sort` (and remember it).
    pub fn set_genres_sort(&mut self, sort: AlbumSort) {
        self.config.config.genres_sort = sort;
        self.config.save();
        self.resort_library();
    }

    /// Re-order the browse lists after a sort order changed.
    fn resort_library(&mut self) {
        self.library.resort(&self.config.config);
        self.refresh_search();
    }

    /// Split collaboration credits into individual artists, or not.
    pub fn set_split_artist_tags(&mut self, enabled: bool) {
        self.config.config.split_artist_tags = enabled;
        self.config.save();
        self.rebuild_artists();
    }

    /// Replace the delimiters credits are split on (when splitting is on).
    pub fn set_artist_tag_delimiters(&mut self, delimiters: Vec<String>) {
        self.config.config.artist_tag_delimiters = delimiters;
        self.config.save();
        self.rebuild_artists();
    }

    /// Startup / "rescan" entry: scans the local library, loads a Subsonic
    /// one, and waits for an MPD server to connect (its idle subscription
    /// triggers the reload).
    pub fn scan_library(&mut self) -> Reload {
        let Some(provider) = self.providers.registry.active_shared() else {
            return Reload::none();
        };
        match provider.provider_type() {
            ProviderType::Local => {
                let progress = Arc::clone(&self.library.scan_progress);
                // The browser reads its files asynchronously (see
                // `library::web_scanner`), so it scans outside the provider.
                #[cfg(target_arch = "wasm32")]
                let scan = {
                    _ = provider;
                    let dirs = self.config.config.music_dirs.clone();
                    // One thread in the page: the future is only ever polled
                    // where it was created.
                    send_wrapper::SendWrapper::new(async move {
                        crate::library::web_scanner::scan(
                            &crate::paths::library_db_path(),
                            &dirs,
                            &progress,
                        )
                        .await
                        .unwrap_or_else(|e| {
                            tracing::error!("library scan failed: {e}");
                            0
                        })
                    })
                };
                #[cfg(not(target_arch = "wasm32"))]
                let scan = async move {
                    crate::task::spawn_blocking(move || {
                        provider.sync_library(&progress).unwrap_or_else(|e| {
                            tracing::error!("sync_library failed: {e}");
                            0
                        })
                    })
                    .await
                    .unwrap_or(0)
                };
                Reload::effect(self.scan_with(scan))
            }
            // Subsonic providers connect on demand: no idle subscription.
            ProviderType::Subsonic => self.reload_library(),
            ProviderType::Mpd => {
                tracing::info!(
                    "Skipping scan for MPD provider '{}' — waiting for connection",
                    self.providers.registry.active_id()
                );
                Reload::none()
            }
        }
    }

    /// Runs `work` (returning how many tracks it updated) as the scan of
    /// the active provider's library, when that is the local one; its
    /// result reloads the library if anything changed. Does nothing (and
    /// drops `work` unpolled) for any other provider.
    pub fn rescan_local(
        &mut self,
        work: impl Future<Output = usize> + Send + 'static,
    ) -> Effect<LibraryEvent> {
        match self.providers.registry.active_shared() {
            Some(p) if p.provider_type() == ProviderType::Local => self.scan_with(work),
            _ => Effect::none(),
        }
    }

    fn scan_with(
        &mut self,
        work: impl Future<Output = usize> + Send + 'static,
    ) -> Effect<LibraryEvent> {
        self.library.scanning = true;
        self.library.scan_progress.reset();
        let generation = self.library.begin_reload_generation();
        let provider_id = self.providers.registry.active_id().to_string();
        Effect::future(async move {
            LibraryEvent::ScanComplete {
                generation,
                provider_id,
                count: work.await,
            }
        })
    }

    /// The filesystem watcher saw `paths` change: incrementally scan them
    /// when the local library is active.
    pub fn files_changed(&mut self, paths: Vec<PathBuf>) -> Effect<LibraryEvent> {
        // Filter out directories and non-existent-but-not-deleted paths.
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| p.is_file() || !p.exists())
            .collect();
        if paths.is_empty() {
            return Effect::none();
        }
        tracing::info!("Filesystem watcher detected {} changed paths", paths.len());

        let work = async move {
            crate::task::spawn_blocking(move || {
                // `LocalProvider` wraps its `LibraryDb` in a mutex; the
                // incremental scan opens a connection of its own.
                let db_path = crate::paths::library_db_path();
                match crate::library::LibraryDb::open(&db_path) {
                    Ok(db) => LibraryScanner::scan_paths(&db, &paths).unwrap_or_else(|e| {
                        tracing::error!("scan_paths failed: {e}");
                        0
                    }),
                    Err(e) => {
                        tracing::error!("Failed to open DB for incremental scan: {e}");
                        0
                    }
                }
            })
            .await
            .unwrap_or(0)
        };
        self.rescan_local(work)
    }

    /// (Re)load the active provider's library. A database-backed provider
    /// loads in one piece; remote ones stream batches, keeping an already
    /// loaded library (and its covers) fully visible until the whole reload
    /// is staged.
    pub fn reload_library(&mut self) -> Reload {
        let Some(provider) = self.providers.registry.active_shared() else {
            return Reload::none();
        };
        let provider_type = provider.provider_type();
        let generation = self.library.begin_reload_generation();
        let provider_id = provider.id().to_string();

        match provider_type {
            ProviderType::Local => Reload::effect(Effect::future(load_local(
                provider,
                generation,
                provider_id,
            ))),
            ProviderType::Mpd | ProviderType::Subsonic => {
                self.library.scanning = true;
                // A library already showing stays visible while the
                // refreshed data accumulates off to the side; otherwise
                // there is nothing to flash away from and batches populate
                // the visible lists progressively.
                let covers = if self.library.begin_incremental_reload(generation) {
                    self.library.covers.begin_staging();
                    CoverDelta::default()
                } else {
                    self.cover_bytes.clear();
                    self.library.covers.clear()
                };
                #[cfg(target_arch = "wasm32")]
                let effect = {
                    // No remote provider exists in the browser yet.
                    _ = (provider, generation, provider_id);
                    Effect::none()
                };
                #[cfg(not(target_arch = "wasm32"))]
                let effect = Effect::stream(channel_stream(8, {
                    let mpd = self
                        .providers
                        .mpd_providers
                        .iter()
                        .find(|p| p.id() == provider_id)
                        .cloned();
                    let subsonic = self
                        .providers
                        .subsonic_providers
                        .iter()
                        .find(|p| p.id() == provider_id)
                        .cloned();
                    move |tx| load_incremental(tx, provider, mpd, subsonic, generation, provider_id)
                }));
                Reload { covers, effect }
            }
        }
    }

    /// Apply the async result of a library workflow.
    pub fn apply_library_event(&mut self, event: LibraryEvent) -> LibraryApplied {
        match event {
            LibraryEvent::ScanComplete {
                generation,
                provider_id,
                count,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return LibraryApplied::Stale;
                }
                self.library.scanning = false;
                self.library.scan_progress.reset();
                tracing::info!("Library scan complete: {count} tracks updated");
                // Only reload if tracks actually changed, to avoid
                // unnecessary view rebuilds (which reset scroll position).
                let reload = if count > 0 || self.library.tracks.is_empty() {
                    self.reload_library()
                } else {
                    Reload::none()
                };
                LibraryApplied::ScanComplete { reload }
            }
            LibraryEvent::Loaded {
                generation,
                provider_id,
                tracks,
                albums,
                covers,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return LibraryApplied::Stale;
                }
                self.library.scanning = false;
                self.library
                    .replace_loaded(tracks, albums, &self.config.config);
                let covers = self.library.covers.replace(covers);
                // Full-size bytes are loaded lazily; drop any stale ones.
                self.cover_bytes.clear();
                self.refresh_search();
                LibraryApplied::Loaded { covers }
            }
            LibraryEvent::Batch {
                generation,
                provider_id,
                albums,
                covers,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return LibraryApplied::Stale;
                }
                if self.library.is_staging(generation) {
                    // Non-destructive refresh: accumulate into the staging
                    // buffer while the previously loaded library stays
                    // fully visible.
                    self.library.stage_albums(albums);
                    self.library.covers.stage(covers);
                    return LibraryApplied::Batch {
                        covers: CoverDelta::default(),
                        staged: true,
                    };
                }
                // Progressive first-load path: the library was empty when
                // this reload started, so populate the visible lists
                // directly as batches arrive.
                let needs_refilter = self.library.append_batch(&albums, &self.config.config);
                if needs_refilter {
                    self.refresh_search();
                }
                LibraryApplied::Batch {
                    covers: self.library.covers.merge(covers),
                    staged: false,
                }
            }
            LibraryEvent::LoadComplete {
                generation,
                provider_id,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return LibraryApplied::Stale;
                }
                self.library.scanning = false;
                // Swaps a staged refresh in atomically (the previously
                // displayed library was never cleared, so there is no
                // empty-view flash) and runs the final sort.
                let swapped = self
                    .library
                    .complete_reload(generation, &self.config.config);
                tracing::info!(
                    "Library load complete: {} albums, {} tracks, {} artists",
                    self.library.albums.len(),
                    self.library.tracks.len(),
                    self.library.artists.len()
                );
                let covers = self.library.covers.complete_staged(swapped);
                if swapped {
                    self.cover_bytes.clear();
                }
                self.refresh_search();
                LibraryApplied::Completed { covers }
            }
            LibraryEvent::TopTracksLoaded { artist, tracks } => {
                self.library.artist_top = Some(ArtistTop { artist, tracks });
                LibraryApplied::Done
            }
            LibraryEvent::GenresLoaded(genres) => {
                self.library.genres = genres;
                self.library.sort_browse(&self.config.config);
                self.refresh_search();
                LibraryApplied::Done
            }
            LibraryEvent::SearchDebounced(generation) => {
                if self.library.is_latest_search(generation) {
                    self.refresh_search();
                }
                LibraryApplied::Done
            }
            LibraryEvent::GenreTracksLoaded { idx, tracks } => {
                if self.library.selected_genre == Some(idx) {
                    self.library.genre_tracks = tracks;
                }
                LibraryApplied::Done
            }
            LibraryEvent::FavoriteToggled {
                track_id,
                optimistic,
                result,
            } => match result {
                Ok(state) => {
                    if state != optimistic {
                        self.set_favorite_local(&track_id, state);
                    }
                    self.scrobble_favorite_changed(&track_id, state);
                    LibraryApplied::Done
                }
                Err(e) => {
                    tracing::warn!("toggle_favorite failed: {e}");
                    self.set_favorite_local(&track_id, !optimistic);
                    LibraryApplied::ActionFailed(e)
                }
            },
            LibraryEvent::RatingSet {
                track_id,
                previous,
                result,
            } => match result {
                Ok(()) => LibraryApplied::Done,
                Err(e) => {
                    tracing::warn!("set_rating failed: {e}");
                    self.set_rating_local(&track_id, previous);
                    LibraryApplied::ActionFailed(e)
                }
            },
        }
    }

    /// Load the active provider's genres.
    pub fn load_genres(&self) -> Effect<LibraryEvent> {
        let Some(provider) = self.providers.registry.active_shared() else {
            return Effect::none();
        };
        Effect::future(async move {
            let genres = crate::task::spawn_blocking(move || {
                provider.list_genres().unwrap_or_else(|e| {
                    tracing::debug!("list_genres: {e}");
                    Vec::new()
                })
            })
            .await
            .unwrap_or_default();
            LibraryEvent::GenresLoaded(genres)
        })
    }

    /// Load the tracks of genre row `idx` from the active provider; without
    /// one, the library's own tracks of that genre fill in immediately.
    pub fn load_genre_tracks(&mut self, idx: usize) -> Effect<LibraryEvent> {
        self.library.selected_genre = Some(idx);
        self.library.genre_tracks.clear();
        let Some(genre) = self.library.genres.get(idx).cloned() else {
            return Effect::none();
        };
        let Some(provider) = self.providers.registry.active_shared() else {
            self.library.genre_tracks = self.library.tracks_in_genre(&genre);
            return Effect::none();
        };
        Effect::future(async move {
            let tracks = crate::task::spawn_blocking(move || {
                provider.get_tracks_by_genre(&genre).unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            LibraryEvent::GenreTracksLoaded { idx, tracks }
        })
    }

    /// Rank the tracks of the artist at `idx` by how often they were played
    /// (library-database providers; others rank by rating and favourites
    /// only) and deliver the best as [`LibraryEvent::TopTracksLoaded`].
    pub fn load_artist_top(&self, idx: usize) -> Effect<LibraryEvent> {
        let Some(artist) = self.library.artists.get(idx) else {
            return Effect::none();
        };
        let name = artist.name.clone();
        let tracks: Vec<Track> = artist
            .albums
            .iter()
            .flat_map(|a| a.tracks.iter().cloned())
            .collect();
        let uses_database = self.home_uses_database();
        Effect::future(async move {
            let top = crate::task::spawn_blocking(move || {
                let plays = if uses_database {
                    let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
                    LibraryDb::open(&crate::paths::library_db_path())
                        .and_then(|db| db.play_counts(&ids))
                        .unwrap_or_else(|e| {
                            tracing::warn!("Play counts failed to load: {e}");
                            Default::default()
                        })
                } else {
                    Default::default()
                };
                rank_top_tracks(&tracks, &plays, TOP_TRACKS_LEN)
            })
            .await
            .unwrap_or_default();
            LibraryEvent::TopTracksLoaded {
                artist: name,
                tracks: top,
            }
        })
    }

    /// Flip a track's favourite flag: optimistically locally, then against
    /// the active provider.
    pub fn toggle_favorite(&mut self, track_id: String) -> Effect<LibraryEvent> {
        let Some(provider) = self.providers.registry.active_shared() else {
            return Effect::none();
        };
        let optimistic = !self
            .library
            .favorite_state(self.playback.current_track.as_ref(), &track_id)
            .unwrap_or(false);
        self.set_favorite_local(&track_id, optimistic);
        Effect::future(async move {
            let id = track_id.clone();
            let result = crate::task::spawn_blocking(move || {
                provider.toggle_favorite(&id).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            LibraryEvent::FavoriteToggled {
                track_id,
                optimistic,
                result,
            }
        })
    }

    /// Rate a track (`0` clears the rating): optimistically locally, then
    /// against the active provider.
    pub fn set_rating(&mut self, track_id: String, rating: u8) -> Effect<LibraryEvent> {
        let Some(provider) = self.providers.registry.active_shared() else {
            return Effect::none();
        };
        let previous = self
            .library
            .rating_state(self.playback.current_track.as_ref(), &track_id);
        self.set_rating_local(&track_id, if rating == 0 { None } else { Some(rating) });
        Effect::future(async move {
            let id = track_id.clone();
            let result = crate::task::spawn_blocking(move || {
                provider.set_rating(&id, rating).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            LibraryEvent::RatingSet {
                track_id,
                previous,
                result,
            }
        })
    }
}

/// Fetch one album's cover through the provider (blocking).
#[cfg(not(target_arch = "wasm32"))]
fn provider_cover(provider: &dyn MusicProvider, album: &Album) -> Option<CoverThumb> {
    let bytes = provider.get_cover_art(album).ok().flatten()?;
    CoverThumb::from_bytes(&bytes)
}

/// Single-shot reload of a database-backed provider, with covers extracted
/// from the files in parallel.
async fn load_local(
    provider: Arc<dyn MusicProvider>,
    generation: u64,
    provider_id: String,
) -> LibraryEvent {
    let browsed = Arc::clone(&provider);
    let (tracks, albums) = crate::task::spawn_blocking(move || {
        let provider = browsed;
        let tracks = provider.browse_tracks().unwrap_or_else(|e| {
            tracing::error!("browse_tracks failed: {e}");
            Vec::new()
        });
        let albums = provider.browse_albums().unwrap_or_else(|e| {
            tracing::error!("browse_albums failed: {e}");
            Vec::new()
        });
        (tracks, albums)
    })
    .await
    .unwrap_or_default();

    // Thumbnails persisted by earlier launches: an album whose cover file
    // is unchanged skips the full-size decode.
    let mut cached = {
        let provider = Arc::clone(&provider);
        crate::task::spawn_blocking(move || provider.cached_thumbs())
            .await
            .unwrap_or_default()
    };
    let keep: std::collections::HashSet<String> = albums
        .iter()
        .map(|a| CoverArt::album_key(&a.artist, &a.name))
        .collect();
    let stale_cache = cached.keys().any(|k| !keep.contains(k));

    let first_tracks: Vec<(String, PathBuf)> = albums
        .iter()
        .filter_map(|album| {
            let key = CoverArt::album_key(&album.artist, &album.name);
            album.tracks.first().map(|track| (key, track.path.clone()))
        })
        .collect();
    let results = album_covers(first_tracks, &mut cached).await;

    let mut covers = CoverSet::new();
    let mut fresh = Vec::new();
    for (key, thumb, new) in results {
        covers.extend(thumb.map(|t| (key, t)));
        fresh.extend(new);
    }
    if !fresh.is_empty() || stale_cache {
        // Not awaited: the library need not wait for the cache write.
        crate::task::run_detached(move || {
            provider.store_thumbs(&fresh, &keep);
        });
    }

    LibraryEvent::Loaded {
        generation,
        provider_id,
        tracks,
        albums,
        covers,
    }
}

/// One album's cover as [`album_covers`] finds it: the album key, its
/// thumbnail (`None` for "no cover"), and the cache entry to store when what
/// the cache held was missing or stale.
type AlbumCover = (String, Option<CoverThumb>, Option<(String, CachedThumb)>);

/// What the persisted cache says about an album's cover, given the current
/// `stamp` of its first track and (when the cache holds "no cover") the
/// `coverless` stamp: `Some(thumb)` when the entry is still valid, `None`
/// when the cover must be read again.
fn cache_verdict(
    hit: Option<&CachedThumb>,
    stamp: Option<&ThumbStamp>,
    coverless: Option<&ThumbStamp>,
) -> Option<Option<CoverThumb>> {
    match (hit, stamp) {
        (
            Some(CachedThumb {
                thumb: Some(thumb),
                stamp: old,
            }),
            Some(now),
        ) if old == now => Some(Some(thumb.clone())),
        (
            Some(CachedThumb {
                thumb: None,
                stamp: old,
            }),
            _,
        ) if coverless == Some(old) => Some(None),
        _ => None,
    }
}

/// The covers of the albums whose first track is at the given path, from the
/// persisted `cached` thumbnails (consumed) or the files.
///
/// Decode + downscale run on the blocking pool: CPU-bound work must not run
/// on an async worker shared with other futures. The full-size bytes are
/// dropped right there; they are re-read lazily for whichever album needs
/// them (blur, hero, notification). At most `COVER_DECODE_CONCURRENCY`
/// full-size decodes run at once: an unbounded burst peaks at GBs and leaves
/// one malloc arena per thread behind.
#[cfg(not(target_arch = "wasm32"))]
async fn album_covers(
    albums: Vec<(String, PathBuf)>,
    cached: &mut std::collections::HashMap<String, CachedThumb>,
) -> Vec<AlbumCover> {
    let permits = Arc::new(tokio::sync::Semaphore::new(COVER_DECODE_CONCURRENCY));
    let tasks: Vec<_> = albums
        .into_iter()
        .map(|(key, path)| {
            let permits = Arc::clone(&permits);
            let hit = cached.remove(&key);
            async move {
                let _permit = permits.acquire_owned().await.ok()?;
                crate::task::spawn_blocking(move || {
                    let stamp = ThumbStamp::of(&path);
                    // A cached "no cover" is vouched for by a stamp that
                    // also covers the directory.
                    let coverless = hit
                        .as_ref()
                        .is_some_and(|h| h.thumb.is_none())
                        .then(|| ThumbStamp::of_coverless(&path))
                        .flatten();
                    if let Some(thumb) =
                        cache_verdict(hit.as_ref(), stamp.as_ref(), coverless.as_ref())
                    {
                        return Some((key, thumb, None));
                    }
                    let thumb = CoverArt::get_cover_art(&path)
                        .and_then(|bytes| CoverThumb::from_bytes(&bytes));
                    let stamp = if thumb.is_some() {
                        stamp
                    } else {
                        ThumbStamp::of_coverless(&path)
                    };
                    let fresh = stamp.map(|stamp| {
                        (
                            key.clone(),
                            CachedThumb {
                                thumb: thumb.clone(),
                                stamp,
                            },
                        )
                    });
                    Some((key, thumb, fresh))
                })
                .await
                .ok()
                .flatten()
            }
        })
        .collect();
    // The decodes are bounded by `permits` and run on the blocking pool, so
    // polling the futures together from this one task keeps them parallel.
    futures::future::join_all(tasks)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// The browser's [`album_covers`]: the files are read through `web_fs`, one
/// album at a time with the event loop given a turn between them (decoding
/// happens on the page's only thread). A folder the page cannot read now
/// (permission pending after a reload) cannot validate the cache, so its
/// albums keep their cached thumbnails.
#[cfg(target_arch = "wasm32")]
async fn album_covers(
    albums: Vec<(String, PathBuf)>,
    cached: &mut std::collections::HashMap<String, CachedThumb>,
) -> Vec<AlbumCover> {
    use crate::library::web_covers;
    let mut yielder = crate::web_fs::Yielder::new();
    let mut found = Vec::with_capacity(albums.len());
    for (key, path) in albums {
        let hit = cached.remove(&key);
        if !web_covers::files_available(&path) {
            if let Some(hit) = hit {
                found.push((key, hit.thumb, None));
            }
            continue;
        }
        let stamp = web_covers::thumb_stamp(&path).await;
        let coverless = if hit.as_ref().is_some_and(|h| h.thumb.is_none()) {
            web_covers::coverless_stamp(&path).await
        } else {
            None
        };
        if let Some(thumb) = cache_verdict(hit.as_ref(), stamp.as_ref(), coverless.as_ref()) {
            found.push((key, thumb, None));
            continue;
        }
        let thumb = web_covers::cover_thumb(&path).await;
        let stamp = if thumb.is_some() {
            stamp
        } else {
            web_covers::coverless_stamp(&path).await
        };
        let fresh = stamp.map(|stamp| {
            (
                key.clone(),
                CachedThumb {
                    thumb: thumb.clone(),
                    stamp,
                },
            )
        });
        found.push((key, thumb, fresh));
        yielder.tick().await;
    }
    found
}

/// Covers of one batch of albums, fetched in parallel.
#[cfg(not(target_arch = "wasm32"))]
async fn batch_covers(provider: &Arc<dyn MusicProvider>, albums: &[Album]) -> CoverSet {
    let tasks: Vec<_> = albums
        .iter()
        .map(|album| {
            let key = CoverArt::album_key(&album.artist, &album.name);
            let provider = Arc::clone(provider);
            let album = album.cover_hint();
            // Decode + downscale inside the blocking closure.
            crate::task::spawn_blocking(move || (key, provider_cover(provider.as_ref(), &album)))
        })
        .collect();
    let mut covers = CoverSet::new();
    for task in tasks {
        if let Ok((key, Some(thumb))) = task.await {
            covers.insert(key, thumb);
        }
    }
    covers
}

/// Incremental reload of a remote provider: albums in batches of
/// [`BATCH_SIZE`], each sent as a [`LibraryEvent::Batch`] with its covers,
/// then [`LibraryEvent::LoadComplete`]. `mpd`/`subsonic` are the concrete
/// provider behind `provider` (whichever it is), for paged access.
#[cfg(not(target_arch = "wasm32"))]
async fn load_incremental(
    mut tx: mpsc::Sender<LibraryEvent>,
    provider: Arc<dyn MusicProvider>,
    mpd: Option<Arc<crate::provider::mpd::MpdProvider>>,
    subsonic: Option<Arc<crate::provider::subsonic::SubsonicProvider>>,
    generation: u64,
    provider_id: String,
) {
    match provider.provider_type() {
        ProviderType::Mpd => {
            let Some(mpd) = mpd else { return };

            // Step 1: all album names (a single fast command).
            let album_names = match mpd.list_album_names().await {
                Ok(names) => names,
                Err(e) => {
                    tracing::error!("MPD list_album_names failed: {e}");
                    _ = tx
                        .send(LibraryEvent::LoadComplete {
                            generation,
                            provider_id,
                        })
                        .await;
                    return;
                }
            };
            tracing::info!(
                "MPD incremental load: {} albums in batches of {BATCH_SIZE}",
                album_names.len()
            );

            // Step 2: process in batches. A failed chunk is logged and
            // skipped rather than aborting the remaining batches, so one
            // bad batch does not truncate the whole library.
            let mut failed_chunks: usize = 0;
            let mut failed_albums: usize = 0;
            for (chunk_index, chunk) in album_names.chunks(BATCH_SIZE).enumerate() {
                let albums = match mpd.browse_albums_batch(chunk).await {
                    Ok(a) => a,
                    Err(e) => {
                        tracing::error!(
                            "MPD browse_albums_batch failed for chunk {chunk_index} \
                             ({} albums: {:?}): {e}",
                            chunk.len(),
                            chunk
                        );
                        failed_chunks += 1;
                        failed_albums += chunk.len();
                        continue;
                    }
                };
                let covers = batch_covers(&provider, &albums).await;
                _ = tx
                    .send(LibraryEvent::Batch {
                        generation,
                        provider_id: provider_id.clone(),
                        albums,
                        covers,
                    })
                    .await;
            }
            if failed_chunks > 0 {
                tracing::warn!(
                    "MPD incremental load: skipped {failed_chunks} chunk(s) \
                     ({failed_albums} album(s)) due to browse_albums_batch errors"
                );
            }
        }
        ProviderType::Subsonic => {
            let Some(subsonic) = subsonic else { return };
            tracing::info!("Subsonic incremental load: batches of {BATCH_SIZE}");

            let mut offset: i32 = 0;
            let page_size = BATCH_SIZE as i32;
            loop {
                let (albums, has_more) = match subsonic.browse_albums_page(offset, page_size).await
                {
                    Ok(result) => result,
                    Err(e) => {
                        tracing::error!("Subsonic browse_albums_page failed: {e}");
                        break;
                    }
                };
                if albums.is_empty() {
                    break;
                }
                let batch_count = albums.len();
                let covers = batch_covers(&provider, &albums).await;
                tracing::debug!("Subsonic batch: offset={offset}, albums={batch_count}");
                _ = tx
                    .send(LibraryEvent::Batch {
                        generation,
                        provider_id: provider_id.clone(),
                        albums,
                        covers,
                    })
                    .await;
                if !has_more {
                    break;
                }
                offset += page_size;
            }
        }
        // Database-backed providers load in one piece (`load_local`).
        ProviderType::Local => return,
    }

    _ = tx
        .send(LibraryEvent::LoadComplete {
            generation,
            provider_id,
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{test_session, track};
    use super::*;
    use crate::library::Playlist;
    use bytes::Bytes;

    fn album(name: &str, artist: &str, first_track_id: i64) -> Album {
        let mut t = track(first_track_id);
        t.title = format!("{name} song");
        t.album = name.into();
        t.artist = artist.into();
        Album::new(name.into(), artist.into(), 2000, vec![t], None)
    }

    fn thumb(fingerprint: u64) -> CoverThumb {
        CoverThumb {
            width: 1,
            height: 1,
            encoded: Bytes::from_static(&[0, 0, 0, 255]),
            fingerprint,
        }
    }

    fn covers(items: &[(&str, u64)]) -> CoverSet {
        items
            .iter()
            .map(|(k, f)| ((*k).to_string(), thumb(*f)))
            .collect()
    }

    fn loaded(session: &mut Session, albums: Vec<Album>, covers: CoverSet) -> LibraryApplied {
        let generation = session.library.begin_reload_generation();
        let tracks = albums.iter().flat_map(|a| a.tracks.clone()).collect();
        session.apply_library_event(LibraryEvent::Loaded {
            generation,
            provider_id: session.providers.registry.active_id().to_string(),
            tracks,
            albums,
            covers,
        })
    }

    fn visible_names(session: &Session) -> Vec<&str> {
        session
            .library
            .visible_albums()
            .iter()
            .map(|a| a.name.as_str())
            .collect()
    }

    #[test]
    fn search_waits_for_the_debounce_and_the_latest_change_wins() {
        let mut session = test_session();
        loaded(
            &mut session,
            vec![
                album("Abbey Road", "Beatles", 1),
                album("Thriller", "Jackson", 2),
            ],
            CoverSet::new(),
        );
        assert_eq!(visible_names(&session), ["Abbey Road", "Thriller"]);

        let first = session.set_search_query("thr".into());
        assert!(!first.is_none());
        let second = session.set_search_query("abbey".into());
        assert!(!second.is_none());
        // Nothing is filtered before a debounce lands.
        assert_eq!(visible_names(&session), ["Abbey Road", "Thriller"]);

        // The superseded debounce (generation 1) is ignored…
        session.apply_library_event(LibraryEvent::SearchDebounced(1));
        assert_eq!(visible_names(&session), ["Abbey Road", "Thriller"]);
        // …the latest one (2) refreshes.
        session.apply_library_event(LibraryEvent::SearchDebounced(2));
        assert_eq!(visible_names(&session), ["Abbey Road"]);
        assert_eq!(session.library.search_query(), "abbey");
    }

    #[test]
    fn local_load_reuses_persisted_thumbnails_until_the_file_changes() {
        use crate::provider::local::LocalProvider;
        let dir = super::super::test_support::scratch_dir("cover-cache");
        let audio = dir.join("a.mp3");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/tagio/plain.mp3"
            ),
            &audio,
        )
        .unwrap();
        let png = |shade: u8| {
            let mut out = Vec::new();
            image::ImageEncoder::write_image(
                image::codecs::png::PngEncoder::new(&mut out),
                &[shade; 8 * 8 * 3],
                8,
                8,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
            out
        };
        std::fs::write(dir.join("cover.png"), png(10)).unwrap();

        let db = LibraryDb::open_memory().unwrap();
        let mut t = track(1);
        t.path = audio.clone();
        t.source_uri = audio.to_string_lossy().into_owned();
        db.upsert_track(&t, 0).unwrap();
        let provider = Arc::new(LocalProvider::new(db, Vec::new()));
        let key = CoverArt::album_key("", "album");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let load = || async {
                let LibraryEvent::Loaded { covers, .. } =
                    load_local(provider.clone(), 1, "local".into()).await
                else {
                    panic!("not a Loaded event");
                };
                covers
            };
            // The write-back runs detached: wait for it.
            let persisted = || async {
                for _ in 0..200 {
                    let all = provider.cached_thumbs();
                    if !all.is_empty() {
                        return all;
                    }
                    crate::task::sleep(Duration::from_millis(10)).await;
                }
                panic!("thumbnail never persisted");
            };

            let first = load().await;
            let real = first[&key].fingerprint;
            let stored = persisted().await;
            assert_eq!(stored[&key].thumb.as_ref().unwrap().fingerprint, real);

            // Swap in a marker thumbnail carrying the same stamp: a second
            // load that returns it never decoded the cover again.
            let mut marked = stored[&key].clone();
            marked.thumb.as_mut().unwrap().fingerprint = 42;
            provider.store_thumbs(&[(key.clone(), marked)], &[key.clone()].into());
            assert_eq!(load().await[&key].fingerprint, 42);

            // The cover file is replaced (new content and size): the stale
            // stamp is ignored and the cover decoded afresh.
            std::fs::write(
                audio.clone(),
                [std::fs::read(&audio).unwrap(), vec![0; 7]].concat(),
            )
            .unwrap();
            let after = load().await;
            assert_ne!(after[&key].fingerprint, 42);
            assert_eq!(after[&key].fingerprint, real);
        });
    }

    #[test]
    fn local_load_remembers_coverless_albums_until_a_cover_appears() {
        use crate::provider::local::LocalProvider;
        let dir = super::super::test_support::scratch_dir("coverless");
        let audio = dir.join("a.mp3");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/tagio/plain.mp3"
            ),
            &audio,
        )
        .unwrap();
        let db = LibraryDb::open_memory().unwrap();
        let mut t = track(1);
        t.path = audio.clone();
        t.source_uri = audio.to_string_lossy().into_owned();
        db.upsert_track(&t, 0).unwrap();
        let provider = Arc::new(LocalProvider::new(db, Vec::new()));
        let key = CoverArt::album_key("", "album");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let load = || async {
                let LibraryEvent::Loaded { covers, .. } =
                    load_local(provider.clone(), 1, "local".into()).await
                else {
                    panic!("not a Loaded event");
                };
                covers
            };
            assert!(load().await.is_empty());
            let mut stored = provider.cached_thumbs();
            for _ in 0..200 {
                if !stored.is_empty() {
                    break;
                }
                crate::task::sleep(Duration::from_millis(10)).await;
                stored = provider.cached_thumbs();
            }
            assert!(
                stored[&key].thumb.is_none(),
                "no-cover result not persisted"
            );

            // Still coverless: the entry holds.
            assert!(load().await.is_empty());

            // A cover dropped next to the tracks makes the entry stale.
            let mut png = Vec::new();
            image::ImageEncoder::write_image(
                image::codecs::png::PngEncoder::new(&mut png),
                &[9u8; 8 * 8 * 3],
                8,
                8,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
            std::fs::write(dir.join("cover.png"), png).unwrap();
            assert!(load().await.contains_key(&key));
        });
    }

    #[test]
    fn debounce_effect_yields_the_generation_it_was_scheduled_for() {
        use futures::StreamExt;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let mut session = test_session();
            let mut stream = session
                .set_search_query("x".into())
                .into_stream()
                .expect("a debounce effect");
            assert!(matches!(
                stream.next().await,
                Some(LibraryEvent::SearchDebounced(1))
            ));
        });
    }

    #[test]
    fn clearing_the_query_refreshes_at_once_without_an_effect() {
        let mut session = test_session();
        loaded(
            &mut session,
            vec![
                album("Abbey Road", "Beatles", 1),
                album("Thriller", "Jackson", 2),
            ],
            CoverSet::new(),
        );
        let effect = session.set_search_query("thr".into());
        drop(effect);
        session.apply_library_event(LibraryEvent::SearchDebounced(1));
        assert_eq!(visible_names(&session), ["Thriller"]);

        assert!(session.set_search_query("  ".into()).is_none());
        assert_eq!(visible_names(&session), ["Abbey Road", "Thriller"]);

        drop(session.set_search_query("thr".into()));
        session.apply_library_event(LibraryEvent::SearchDebounced(3));
        assert_eq!(visible_names(&session), ["Thriller"]);
        session.clear_search();
        assert_eq!(visible_names(&session), ["Abbey Road", "Thriller"]);
        assert_eq!(session.library.search_query(), "");
    }

    #[test]
    fn visible_indices_map_back_to_the_library() {
        let mut session = test_session();
        loaded(
            &mut session,
            vec![
                album("Abbey Road", "Beatles", 1),
                album("Thriller", "Jackson", 2),
            ],
            CoverSet::new(),
        );
        drop(session.set_search_query("jackson".into()));
        session.apply_library_event(LibraryEvent::SearchDebounced(1));

        let lib = &session.library;
        assert_eq!(lib.visible_albums().len(), 1);
        assert_eq!(lib.albums[lib.album_index(0)].name, "Thriller");
        assert_eq!(lib.tracks[lib.track_index(0)].album, "Thriller");
        assert_eq!(lib.artists[lib.artist_index(0)].name, "Jackson");
    }

    #[test]
    fn loading_refreshes_the_active_search() {
        let mut session = test_session();
        drop(session.set_search_query("thr".into()));
        session.apply_library_event(LibraryEvent::SearchDebounced(1));
        assert!(session.library.visible_albums().is_empty());

        loaded(
            &mut session,
            vec![
                album("Abbey Road", "Beatles", 1),
                album("Thriller", "Jackson", 2),
            ],
            CoverSet::new(),
        );
        assert_eq!(visible_names(&session), ["Thriller"]);
    }

    #[test]
    fn playlists_and_genres_follow_the_query() {
        let mut session = test_session();
        session.library.genres = vec!["Rock".into(), "Pop".into()];
        session.playlists.playlists = ["Road trip", "Focus"]
            .into_iter()
            .map(|name| Playlist {
                id: name.into(),
                name: name.into(),
                tracks: Vec::new(),
                track_count: 0,
                total_duration: Duration::ZERO,
            })
            .collect();
        drop(session.set_search_query("road".into()));
        session.apply_library_event(LibraryEvent::SearchDebounced(1));
        // Genres only refresh with a genre or query event; the playlist
        // list is whatever the last refresh saw.
        session.refresh_search();
        let lib = &session.library;
        let playlists = lib.visible_playlists(&session.playlists.playlists);
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].name, "Road trip");
        assert_eq!(lib.playlist_index(0), 0);
        assert!(lib.visible_genres().is_empty());

        session.apply_library_event(LibraryEvent::GenresLoaded(vec![
            "Rock".into(),
            "Roadhouse".into(),
        ]));
        assert_eq!(session.library.visible_genres(), ["Roadhouse"]);
        // Loading sorts the genres: "Roadhouse" now precedes "Rock".
        assert_eq!(session.library.genre_index(0), 0);
    }

    #[test]
    fn favorite_changes_reach_the_search_results() {
        let mut session = test_session();
        loaded(
            &mut session,
            vec![
                album("Abbey Road", "Beatles", 1),
                album("Thriller", "Jackson", 2),
            ],
            CoverSet::new(),
        );
        drop(session.set_search_query("thriller".into()));
        session.apply_library_event(LibraryEvent::SearchDebounced(1));
        session.set_favorite_local("2", true);
        assert!(session.library.visible_tracks()[0].is_favorite);
        assert!(session.library.visible_albums()[0].tracks[0].is_favorite);
    }

    #[test]
    fn hiding_compilations_refreshes_the_artist_list() {
        let mut session = test_session();
        loaded(
            &mut session,
            vec![
                album("Hits", "Various Artists", 1),
                album("Thriller", "Jackson", 2),
            ],
            CoverSet::new(),
        );
        assert_eq!(session.library.visible_artists().len(), 2);
        session.set_show_compilations_in_artists(false);
        let artists = session.library.visible_artists();
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].name, "Jackson");
        assert_eq!(
            session.library.artists[session.library.artist_index(0)].name,
            "Jackson"
        );
        session.set_show_compilations_in_artists(true);
        assert_eq!(session.library.visible_artists().len(), 2);
    }

    #[test]
    fn single_shot_load_reports_changed_and_removed_covers() {
        let mut session = test_session();
        let LibraryApplied::Loaded { covers: delta } = loaded(
            &mut session,
            vec![album("A", "x", 1), album("B", "x", 2)],
            covers(&[("a", 1), ("b", 2)]),
        ) else {
            panic!("expected Loaded");
        };
        assert_eq!(delta.changed.len(), 2);
        assert!(session.library.covers.contains("a"));

        let LibraryApplied::Loaded { covers: delta } = loaded(
            &mut session,
            vec![album("A", "x", 1)],
            covers(&[("a", 1), ("c", 3)]),
        ) else {
            panic!("expected Loaded");
        };
        assert_eq!(delta.changed, ["c"]);
        assert_eq!(delta.removed, ["b"]);
    }

    fn batch(
        session: &mut Session,
        generation: u64,
        albums: Vec<Album>,
        c: CoverSet,
    ) -> LibraryApplied {
        session.apply_library_event(LibraryEvent::Batch {
            generation,
            provider_id: session.providers.registry.active_id().to_string(),
            albums,
            covers: c,
        })
    }

    fn complete(session: &mut Session, generation: u64) -> LibraryApplied {
        session.apply_library_event(LibraryEvent::LoadComplete {
            generation,
            provider_id: session.providers.registry.active_id().to_string(),
        })
    }

    #[test]
    fn progressive_first_load_fills_covers_batch_by_batch() {
        let mut session = test_session();
        let generation = session.library.begin_reload_generation();
        assert!(!session.library.begin_incremental_reload(generation));

        let LibraryApplied::Batch { covers, staged } = batch(
            &mut session,
            generation,
            vec![album("A", "x", 1)],
            covers(&[("a", 1)]),
        ) else {
            panic!("expected Batch");
        };
        assert!(!staged);
        assert_eq!(covers.changed, ["a"]);
        assert!(session.library.covers.contains("a"));
        assert_eq!(session.library.albums.len(), 1);

        let LibraryApplied::Completed { covers } = complete(&mut session, generation) else {
            panic!("expected Completed");
        };
        assert!(covers.is_empty());
    }

    #[test]
    fn staged_refresh_keeps_covers_visible_until_completed() {
        let mut session = test_session();
        loaded(
            &mut session,
            vec![album("A", "x", 1), album("B", "x", 2)],
            covers(&[("a", 1), ("b", 2)]),
        );
        let generation = session.library.begin_reload_generation();
        assert!(session.library.begin_incremental_reload(generation));
        session.library.covers.begin_staging();

        let LibraryApplied::Batch {
            covers: delta,
            staged,
        } = batch(
            &mut session,
            generation,
            vec![album("A", "x", 1), album("C", "x", 3)],
            covers(&[("a", 1), ("c", 3)]),
        )
        else {
            panic!("expected Batch");
        };
        assert!(staged && delta.is_empty());
        // Old library and covers still on screen.
        assert_eq!(session.library.albums.len(), 2);
        assert!(session.library.covers.contains("b") && !session.library.covers.contains("c"));

        let LibraryApplied::Completed { covers: delta } = complete(&mut session, generation) else {
            panic!("expected Completed");
        };
        assert_eq!(delta.changed, ["c"]);
        assert_eq!(delta.removed, ["b"]);
        assert!(!delta.cleared);
        assert!(session.library.covers.contains("c") && !session.library.covers.contains("b"));
        assert_eq!(visible_names(&session), ["A", "C"]);
    }

    #[test]
    fn stale_events_change_no_covers() {
        let mut session = test_session();
        let generation = session.library.begin_reload_generation();
        session.library.begin_reload_generation();
        let applied = batch(
            &mut session,
            generation,
            vec![album("A", "x", 1)],
            covers(&[("a", 1)]),
        );
        assert!(matches!(applied, LibraryApplied::Stale));
        assert!(session.library.covers.is_empty());
    }
}
