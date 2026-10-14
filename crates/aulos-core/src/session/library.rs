// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Library workflows of a [`Session`]: scanning and (re)loading the active
//! provider's library, applying the async results (with the staleness rules
//! that make a provider switch or a newer reload supersede in-flight work),
//! watcher-triggered rescans, genres, and favourite/rating changes against
//! the provider.
//!
//! The UI keeps what hangs off the library but is not domain state (cover
//! image handles, folder tree, search-filter caches): the events and
//! [`LibraryApplied`] outcomes carry the raw cover thumbnails and the
//! decisions it needs to update those.

use super::Session;
use crate::library::{Album, CoverArt, LibraryScanner, Track};
use crate::provider::{MusicProvider, ProviderType};
use crate::task::{Effect, channel_stream};
use futures::SinkExt;
use futures::channel::mpsc;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Albums fetched per batch from a remote provider.
const BATCH_SIZE: usize = 50;

/// A downscaled cover, decoded off the UI thread, ready to become an image
/// handle.
#[derive(Clone)]
pub enum CoverImage {
    /// Straight RGBA pixels, at most [`CoverArt::GRID_THUMBNAIL_MAX_DIM`]
    /// per side.
    Pixels {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    /// The original encoded bytes, when decoding failed (a corrupt or
    /// unusual blob is not silently dropped).
    Encoded(Vec<u8>),
}

impl std::fmt::Debug for CoverImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pixels { width, height, .. } => write!(f, "Pixels({width}x{height})"),
            Self::Encoded(bytes) => write!(f, "Encoded({} bytes)", bytes.len()),
        }
    }
}

impl CoverImage {
    /// Prefers a pre-decoded, downscaled thumbnail over the encoded bytes:
    /// the latter force a full-resolution decode on the UI thread whenever
    /// its raster cache evicts and re-uploads the image.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        match CoverArt::decode_thumbnail(bytes, CoverArt::GRID_THUMBNAIL_MAX_DIM) {
            Some((width, height, rgba)) => Self::Pixels {
                width,
                height,
                rgba,
            },
            None => Self::Encoded(bytes.to_vec()),
        }
    }
}

/// One album's cover: the thumbnail and a fingerprint of the full-size
/// bytes, so a refresh can keep an unchanged cover's existing image handle
/// (and never re-upload it). The full-size bytes are deliberately not
/// carried: they are loaded lazily for the album that needs them.
#[derive(Debug, Clone)]
pub struct CoverThumb {
    pub image: CoverImage,
    pub fingerprint: u64,
}

impl CoverThumb {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            image: CoverImage::from_bytes(bytes),
            fingerprint: CoverArt::content_fingerprint(bytes),
        }
    }
}

/// Covers by [`CoverArt::album_key`].
pub type CoverSet = HashMap<String, CoverThumb>;

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
    GenresLoaded(Vec<String>),
    GenreTracksLoaded {
        idx: usize,
        tracks: Vec<Track>,
    },
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

/// What a reload does to the UI's cover handles before its events arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverReset {
    /// Leave them alone (database-backed single-shot reload, or no reload).
    Keep,
    /// A loaded library stays visible: collect the refreshed covers aside
    /// and swap them in with the data on [`LibraryApplied::Completed`].
    Stage,
    /// Nothing was loaded: drop every cover (and cached full-size bytes);
    /// batches fill them in progressively.
    Clear,
}

/// A started (or skipped) reload/scan.
#[must_use = "the effect does nothing unless the UI runs it, and `covers` must be applied first"]
pub struct Reload {
    pub covers: CoverReset,
    pub effect: Effect<LibraryEvent>,
}

impl Reload {
    /// Nothing started.
    pub fn none() -> Self {
        Self {
            covers: CoverReset::Keep,
            effect: Effect::none(),
        }
    }

    fn effect(effect: Effect<LibraryEvent>) -> Self {
        Self {
            covers: CoverReset::Keep,
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
    ScanComplete {
        reload: Reload,
    },
    /// The visible lists were replaced wholesale; swap in `covers`.
    Loaded {
        covers: CoverSet,
    },
    /// A batch arrived. `staged`: it went to the staging buffer (collect
    /// `covers` aside), otherwise it was appended to the visible lists
    /// (`needs_refilter`: the search filter must be refreshed).
    Batch {
        covers: CoverSet,
        staged: bool,
        needs_refilter: bool,
    },
    /// The reload finished and the final sort ran; `swapped`: a staged
    /// refresh replaced the visible data (swap the staged covers in too).
    Completed {
        swapped: bool,
    },
    GenresLoaded,
    /// Tracks of genre row `idx`; the UI applies them if that genre is
    /// still selected.
    GenreTracks {
        idx: usize,
        tracks: Vec<Track>,
    },
    /// A favourite toggle succeeded; scrobbling services may want to know.
    FavoriteChanged {
        track_id: String,
        state: bool,
    },
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
        self.for_each_track(track_id, |t| t.is_favorite = state);
    }

    pub fn set_rating_local(&mut self, track_id: &str, rating: Option<u8>) {
        self.for_each_track(track_id, |t| t.rating = rating);
    }

    /// Startup / "rescan" entry: scans the local library, loads a Subsonic
    /// one, and waits for an MPD server to connect (its idle subscription
    /// triggers the reload).
    pub fn scan_library(&mut self) -> Reload {
        let Some(provider) = self.providers.registry.active_shared() else {
            return Reload::none();
        };
        match provider.provider_type() {
            ProviderType::Local => Reload::effect(self.scan_with(async move {
                tokio::task::spawn_blocking(move || {
                    provider.sync_library().unwrap_or_else(|e| {
                        tracing::error!("sync_library failed: {e}");
                        0
                    })
                })
                .await
                .unwrap_or(0)
            })),
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
            tokio::task::spawn_blocking(move || {
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
    /// loaded library fully visible until the whole reload is staged (see
    /// [`CoverReset`]).
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
                    CoverReset::Stage
                } else {
                    CoverReset::Clear
                };
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
                    return LibraryApplied::Batch {
                        covers,
                        staged: true,
                        needs_refilter: false,
                    };
                }
                // Progressive first-load path: the library was empty when
                // this reload started, so populate the visible lists
                // directly as batches arrive.
                let needs_refilter = self.library.append_batch(&albums, &self.config.config);
                LibraryApplied::Batch {
                    covers,
                    staged: false,
                    needs_refilter,
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
                LibraryApplied::Completed { swapped }
            }
            LibraryEvent::GenresLoaded(genres) => {
                self.library.genres = genres;
                LibraryApplied::GenresLoaded
            }
            LibraryEvent::GenreTracksLoaded { idx, tracks } => {
                LibraryApplied::GenreTracks { idx, tracks }
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
                    LibraryApplied::FavoriteChanged { track_id, state }
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
            let genres = tokio::task::spawn_blocking(move || {
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
        self.library.genre_tracks.clear();
        let Some(genre) = self.library.genres.get(idx).cloned() else {
            return Effect::none();
        };
        let Some(provider) = self.providers.registry.active_shared() else {
            self.library.genre_tracks = self.library.tracks_in_genre(&genre);
            return Effect::none();
        };
        Effect::future(async move {
            let tracks = tokio::task::spawn_blocking(move || {
                provider.get_tracks_by_genre(&genre).unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            LibraryEvent::GenreTracksLoaded { idx, tracks }
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
            let result = tokio::task::spawn_blocking(move || {
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
            let result = tokio::task::spawn_blocking(move || {
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
fn provider_cover(provider: &dyn MusicProvider, album: &Album) -> Option<CoverThumb> {
    let bytes = provider.get_cover_art(album).ok().flatten()?;
    Some(CoverThumb::from_bytes(&bytes))
}

/// Single-shot reload of a database-backed provider, with covers extracted
/// from the files in parallel.
async fn load_local(
    provider: Arc<dyn MusicProvider>,
    generation: u64,
    provider_id: String,
) -> LibraryEvent {
    let (tracks, albums) = tokio::task::spawn_blocking(move || {
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

    let cover_tasks: Vec<_> = albums
        .iter()
        .filter_map(|album| {
            let key = CoverArt::album_key(&album.artist, &album.name);
            album.tracks.first().map(|track| (key, track.path.clone()))
        })
        .map(|(key, path)| {
            // Decode + downscale on the blocking pool: CPU-bound work must
            // not run on an async worker shared with other futures. The
            // full-size bytes are dropped right here; they are re-read
            // lazily for whichever album needs them (blur, hero,
            // notification).
            tokio::task::spawn_blocking(move || {
                let bytes = CoverArt::get_cover_art(&path)?;
                Some((key, CoverThumb::from_bytes(&bytes)))
            })
        })
        .collect();

    let mut covers = CoverSet::new();
    for task in cover_tasks {
        if let Ok(Some((key, thumb))) = task.await {
            covers.insert(key, thumb);
        }
    }

    LibraryEvent::Loaded {
        generation,
        provider_id,
        tracks,
        albums,
        covers,
    }
}

/// Covers of one batch of albums, fetched in parallel.
async fn batch_covers(provider: &Arc<dyn MusicProvider>, albums: &[Album]) -> CoverSet {
    let tasks: Vec<_> = albums
        .iter()
        .map(|album| {
            let key = CoverArt::album_key(&album.artist, &album.name);
            let provider = Arc::clone(provider);
            let album = album.cover_hint();
            // Decode + downscale inside the blocking closure.
            tokio::task::spawn_blocking(move || (key, provider_cover(provider.as_ref(), &album)))
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
