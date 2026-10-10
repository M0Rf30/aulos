// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::tasks::resolve_mpris_art_task;
use super::{AppModel, HTTP_CLIENT, Message, reload_result_is_stale};
use crate::fl;
use crate::library::{Album, Artist, LibraryDb, LibraryScanner, Track};
use crate::player::mpd_backend::MpdBackend;
use crate::player::{ActiveBackend, PlaybackState, Player};
use crate::provider::MusicProvider;
use crate::provider::local::LocalProvider;
use crate::provider::mpd::{MpdConfig, MpdProvider};
use crate::provider::subsonic::{SubsonicConfig, SubsonicProvider};
use crate::views::{providers, songs};
use cosmic::cosmic_config::CosmicConfigEntry;
use cosmic::prelude::*;
use cosmic::widget;
use futures_util::SinkExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Buffers a non-destructive library refresh: batches accumulate here
/// while the previously loaded library stays fully visible on screen, so
/// switching pages, opening a menu, etc. never sees an empty view mid
/// reload. `Message::LibraryLoadComplete` swaps this into
/// `all_tracks`/`all_albums`/`cover_images`/`cover_art_bytes` atomically
/// once the whole reload finishes -- see `AppModel::reload_library` and
/// the `Message::LibraryBatch`/`LibraryLoadComplete` handlers.
pub(super) struct LibraryReloadStaging {
    pub(super) generation: u64,
    pub(super) tracks: Vec<Track>,
    pub(super) albums: Vec<Album>,
    pub(super) cover_images: HashMap<String, widget::icon::Handle>,
    pub(super) cover_art_bytes: HashMap<String, Vec<u8>>,
}

/// Build a `widget::icon::Handle` for a cover-art byte blob, preferring a
/// pre-decoded, downscaled RGBA thumbnail (`from_raster_pixels`) over the
/// original encoded bytes (`from_raster_bytes`) -- the latter forces a
/// full-resolution JPEG/PNG decode on the UI thread every time iced's
/// wgpu raster cache evicts and re-uploads the handle (e.g. every page
/// switch). Falls back to the encoded bytes if decoding fails, so a
/// corrupt/unusual cover blob is not silently dropped.
fn cover_thumbnail_handle(bytes: &[u8]) -> widget::icon::Handle {
    match crate::library::CoverArt::decode_thumbnail(
        bytes,
        crate::library::CoverArt::GRID_THUMBNAIL_MAX_DIM,
    ) {
        Some((w, h, pixels)) => widget::icon::from_raster_pixels(w, h, pixels),
        None => widget::icon::from_raster_bytes(bytes.to_vec()),
    }
}

/// Clamp a live-updating playback position display value to a track's
/// known duration. `Duration::ZERO` duration means "unknown/unbounded" —
/// a live radio track (whose `Track::duration` is always `Duration::ZERO`,
/// see `radio_page.rs`), or any track whose duration genuinely hasn't been
/// probed yet — never "the track is zero seconds long", so it must not
/// clamp in that case: doing so would pin the seek-bar/position display at
/// zero forever even while a live stream keeps playing indefinitely. Used
/// by `PlaybackTick`/`MpdStatusUpdate` in `update.rs`, the only two
/// call sites that update the UI-displayed playback position.
pub(super) fn clamp_display_position(position: Duration, duration: Duration) -> Duration {
    if duration > Duration::ZERO && position > duration {
        duration
    } else {
        position
    }
}

impl AppModel {
    /// Rebuild `provider_list` and `active_provider_index` from the registry.
    ///
    /// Call after any change to the set of registered providers (init,
    /// reinit_mpd_providers, reinit_subsonic_providers).
    pub(super) fn rebuild_provider_list(&mut self) {
        let entries = self.registry.list();
        self.provider_list = entries
            .iter()
            .map(|(id, name, _)| (id.clone(), name.clone()))
            .collect();
        self.active_provider_index = self
            .provider_list
            .iter()
            .position(|(id, _)| id == self.registry.active_id());
    }

    /// Get the MPD `Client` if the active backend is MPD.
    pub(super) fn mpd_client(&self) -> Option<mpd_client::Client> {
        let player = self.player.as_ref()?;
        if player.active_backend_type() != ActiveBackend::Mpd {
            return None;
        }
        Some(player.mpd_backend_ref()?.client())
    }

    /// Get the active MPD provider (if the active provider is MPD).
    ///
    /// Used by Tasks 111-112 to wire shuffle/repeat toggles to MPD.
    pub(super) fn active_mpd_provider(&self) -> Option<Arc<MpdProvider>> {
        let active_id = self.registry.active_id();
        self.mpd_providers
            .iter()
            .find(|p| p.id() == active_id)
            .cloned()
    }

    /// Load playlists from the active provider asynchronously.
    ///
    /// Used by Task 119 to refresh playlists after CRUD operations.
    pub(super) fn load_playlists(&self) -> Task<cosmic::Action<Message>> {
        if let Some(provider) = self.registry.active_shared() {
            cosmic::task::future(async move {
                let playlists = tokio::task::spawn_blocking(move || {
                    let mut playlists = provider.list_playlists().unwrap_or_else(|e| {
                        tracing::warn!("list_playlists failed: {e}");
                        Vec::new()
                    });
                    // `list_playlists` returns summaries only (no tracks) for
                    // the local provider; fetch each playlist's tracks so the
                    // detail view, "play all" and M3U export have them.
                    for playlist in playlists.iter_mut() {
                        if playlist.tracks.is_empty()
                            && playlist.track_count > 0
                            && let Ok(full) = provider.get_playlist(&playlist.id)
                        {
                            playlist.tracks = full.tracks;
                        }
                    }
                    playlists
                })
                .await
                .unwrap_or_default();
                cosmic::Action::App(Message::PlaylistsLoaded(playlists))
            })
        } else {
            Task::none()
        }
    }

    /// Dispatch an async MPD command, mapping errors to `MpdCommandError`.
    pub(super) fn dispatch_mpd<F>(&self, future: F) -> Task<cosmic::Action<Message>>
    where
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        cosmic::task::future(async move {
            if let Err(e) = future.await {
                cosmic::Action::App(Message::MpdCommandError(e))
            } else {
                // No-op message — command succeeded, status poll will confirm.
                cosmic::Action::App(Message::PlaybackTick)
            }
        })
    }

    /// Dispatch an async MPD play command for a URI (ClearQueue + Add +
    /// Play).
    ///
    /// Aulos drives MPD one track at a time from its own [`crate::player`]
    /// queue — shuffle/repeat live entirely in that queue, never in MPD
    /// itself. Forcing `random`/`repeat`/`single` off on every dispatch
    /// guards against a stale `repeat 1` (set by an external MPD client,
    /// or left over from before Aulos started driving this server): with a
    /// single-song MPD queue, `repeat 1` would otherwise loop that one
    /// song forever and `is_finished()` would never fire, silently
    /// freezing playback on the current track.
    ///
    /// Sent as one command list: one round trip, and no other client can
    /// interleave commands between `clear` and `play`.
    pub(super) fn dispatch_mpd_play(&self, uri: String) -> Task<cosmic::Action<Message>> {
        use mpd_client::commands::{
            Add, ClearQueue, Play, SetRandom, SetRepeat, SetSingle, SingleMode,
        };
        if let Some(client) = self.mpd_client() {
            self.dispatch_mpd(async move {
                client
                    .command_list((
                        ClearQueue,
                        Add::uri(&uri),
                        SetRandom(false),
                        SetRepeat(false),
                        SetSingle(SingleMode::Disabled),
                        Play::current(),
                    ))
                    .await
                    .map_err(|e| format!("MPD play {uri}: {e}"))?;
                Ok(())
            })
        } else {
            Task::none()
        }
    }

    /// After `Player` sets optimistic state, take the pending URI and dispatch.
    pub(super) fn dispatch_mpd_after_play(&mut self) -> Task<cosmic::Action<Message>> {
        if let Some(ref mut player) = self.player
            && let Some(mpd) = player.mpd_backend_mut()
            && let Some(uri) = mpd.take_play_uri()
        {
            return self.dispatch_mpd_play(uri);
        }
        Task::none()
    }

    pub(super) fn update_title(&mut self) -> Task<cosmic::Action<Message>> {
        let mut title = fl!("app-title");

        if let Some(page) = self.nav.text(self.nav.active()) {
            title.push_str(" — ");
            title.push_str(page);
        }

        if let Some(id) = self.core.main_window_id() {
            self.set_window_title(title, id)
        } else {
            Task::none()
        }
    }

    /// Bumps the library reload generation counter, invalidating any
    /// in-flight async result tagged with an older generation. Call this
    /// once at the start of every new reload/scan that should supersede
    /// earlier work.
    pub(super) fn begin_reload_generation(&mut self) -> u64 {
        self.reload_generation += 1;
        self.reload_generation
    }

    /// True when an async library result tagged with `generation`/
    /// `provider_id` is stale — superseded by a later reload/scan or a
    /// provider switch — and must be ignored without mutating state.
    pub(super) fn is_stale_reload(&self, generation: u64, provider_id: &str) -> bool {
        reload_result_is_stale(
            self.reload_generation,
            self.registry.active_id(),
            generation,
            provider_id,
        )
    }

    pub(super) fn reload_library(&mut self) -> Task<cosmic::Action<Message>> {
        let provider = match self.registry.active_shared() {
            Some(p) => p,
            None => return Task::none(),
        };
        let provider_type = provider.provider_type();
        let generation = self.begin_reload_generation();

        match provider_type {
            crate::provider::ProviderType::Local => self.reload_library_local(provider, generation),
            crate::provider::ProviderType::Mpd | crate::provider::ProviderType::Subsonic => {
                self.library_scanning = true;
                if self.all_tracks.is_empty() && self.all_albums.is_empty() {
                    // First load (or the library is genuinely empty): there
                    // is nothing on screen to flash away from, so populate
                    // progressively straight into the visible fields as
                    // batches arrive, same as before.
                    self.library_reload_staging = None;
                    self.all_artists.clear();
                    self.cover_images.clear();
                } else {
                    // A library is already showing: keep it fully visible
                    // and accumulate the refreshed data off to the side.
                    // `Message::LibraryLoadComplete` swaps it in atomically
                    // once the whole reload finishes, so the view never
                    // goes empty mid-reload (see `LibraryReloadStaging`).
                    self.library_reload_staging = Some(LibraryReloadStaging {
                        generation,
                        tracks: Vec::new(),
                        albums: Vec::new(),
                        cover_images: HashMap::new(),
                        cover_art_bytes: HashMap::new(),
                    });
                }
                self.reload_library_incremental(provider, provider_type, generation)
            }
        }
    }

    /// Single-shot library reload for the local provider (reads from local DB).
    pub(super) fn reload_library_local(
        &self,
        provider: Arc<dyn MusicProvider + Send + Sync>,
        generation: u64,
    ) -> Task<cosmic::Action<Message>> {
        let provider_id = provider.id().to_string();
        cosmic::task::future(async move {
            let provider_clone = Arc::clone(&provider);
            let (tracks, albums, artists) = tokio::task::spawn_blocking(move || {
                let tracks = provider_clone.browse_tracks().unwrap_or_else(|e| {
                    tracing::error!("browse_tracks failed: {e}");
                    Vec::new()
                });
                let albums = provider_clone.browse_albums().unwrap_or_else(|e| {
                    tracing::error!("browse_albums failed: {e}");
                    Vec::new()
                });
                let artists = provider_clone.browse_artists().unwrap_or_else(|e| {
                    tracing::error!("browse_artists failed: {e}");
                    Vec::new()
                });
                (tracks, albums, artists)
            })
            .await
            .unwrap_or_default();

            // Extract cover art in parallel
            let cover_tasks: Vec<_> = albums
                .iter()
                .filter_map(|album| {
                    let key = crate::library::CoverArt::album_key(&album.artist, &album.name);
                    album.tracks.first().map(|track| (key, track.path.clone()))
                })
                .map(|(key, path)| {
                    tokio::task::spawn_blocking(
                        move || -> Option<(String, widget::icon::Handle, Vec<u8>)> {
                            let bytes = crate::library::CoverArt::get_cover_art(&path)?;
                            // Decode + downscale here too -- this is CPU-bound
                            // work and must stay off the async task (which
                            // runs on a tokio worker thread shared with other
                            // futures, not a dedicated blocking thread).
                            let handle = cover_thumbnail_handle(&bytes);
                            Some((key, handle, bytes))
                        },
                    )
                })
                .collect();

            let mut cover_images = HashMap::new();
            let mut cover_art_bytes = HashMap::new();
            for task in cover_tasks {
                if let Ok(Some((key, handle, bytes))) = task.await {
                    cover_images.insert(key.clone(), handle);
                    cover_art_bytes.insert(key, bytes);
                }
            }

            cosmic::Action::App(Message::LibraryLoaded {
                generation,
                provider_id,
                tracks,
                albums,
                artists,
                cover_images,
                cover_art_bytes,
            })
        })
    }

    /// Incremental library reload for remote providers (MPD, Subsonic).
    ///
    /// Fetches albums in batches and sends a `LibraryBatch` message after
    /// each batch so the UI populates progressively. Cover art for each
    /// batch is fetched inline. Finishes with `LibraryLoadComplete`.
    pub(super) fn reload_library_incremental(
        &self,
        provider: Arc<dyn MusicProvider + Send + Sync>,
        provider_type: crate::provider::ProviderType,
        generation: u64,
    ) -> Task<cosmic::Action<Message>> {
        // Downcast to concrete provider types for paged access.
        // We clone the Arc'd provider references from self.
        let mpd_providers = self.mpd_providers.clone();
        let subsonic_providers = self.subsonic_providers.clone();
        let active_id = self.registry.active_id().to_string();

        let stream = cosmic::iced::stream::channel(
            8,
            move |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<
                cosmic::Action<Message>,
            >| async move {
                const BATCH_SIZE: usize = 50;

                match provider_type {
                    crate::provider::ProviderType::Mpd => {
                        // Find the matching MpdProvider by id.
                        let mpd = match mpd_providers.iter().find(|p| p.id() == active_id) {
                            Some(p) => Arc::clone(p),
                            None => return,
                        };

                        // Step 1: Get all album names (single fast command).
                        let album_names = match mpd.list_album_names().await {
                            Ok(names) => names,
                            Err(e) => {
                                tracing::error!("MPD list_album_names failed: {e}");
                                _ = emitter
                                    .send(cosmic::Action::App(Message::LibraryLoadComplete {
                                        generation,
                                        provider_id: active_id.clone(),
                                    }))
                                    .await;
                                return;
                            }
                        };

                        tracing::info!(
                            "MPD incremental load: {} albums in batches of {BATCH_SIZE}",
                            album_names.len()
                        );

                        // Step 2: Process in batches. A failed chunk is logged and
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

                            // Fetch cover art for this batch in parallel.
                            let prov = Arc::clone(&provider);
                            let cover_tasks: Vec<_> = albums
                                .iter()
                                .map(|album| {
                                    let key = crate::library::CoverArt::album_key(
                                        &album.artist,
                                        &album.name,
                                    );
                                    let prov2 = Arc::clone(&prov);
                                    let hint = album.cover_hint();
                                    tokio::task::spawn_blocking(move || {
                                        // Decode + downscale here too, inside
                                        // the blocking closure -- CPU-bound
                                        // work must not run on the async
                                        // task's tokio worker thread.
                                        let result = prov2.get_cover_art(&hint).map(|opt| {
                                            opt.map(|bytes| {
                                                let handle = cover_thumbnail_handle(&bytes);
                                                (handle, bytes)
                                            })
                                        });
                                        (key, result)
                                    })
                                })
                                .collect();

                            let mut cover_images = HashMap::new();
                            let mut cover_art_bytes = HashMap::new();
                            for task in cover_tasks {
                                if let Ok((key, Ok(Some((handle, bytes))))) = task.await {
                                    cover_images.insert(key.clone(), handle);
                                    cover_art_bytes.insert(key, bytes);
                                }
                            }

                            _ = emitter
                                .send(cosmic::Action::App(Message::LibraryBatch {
                                    generation,
                                    provider_id: active_id.clone(),
                                    albums,
                                    cover_images,
                                    cover_art_bytes,
                                }))
                                .await;
                        }

                        if failed_chunks > 0 {
                            tracing::warn!(
                                "MPD incremental load: skipped {failed_chunks} chunk(s) \
                                 ({failed_albums} album(s)) due to browse_albums_batch errors"
                            );
                        }
                    }

                    crate::provider::ProviderType::Subsonic => {
                        // Find the matching SubsonicProvider by id.
                        let subsonic = match subsonic_providers.iter().find(|p| p.id() == active_id)
                        {
                            Some(p) => Arc::clone(p),
                            None => return,
                        };

                        tracing::info!("Subsonic incremental load: batches of {BATCH_SIZE}");

                        let mut offset: i32 = 0;
                        let page_size = BATCH_SIZE as i32;

                        loop {
                            let (albums, has_more) =
                                match subsonic.browse_albums_page(offset, page_size).await {
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

                            // Fetch cover art for this batch in parallel.
                            let prov = Arc::clone(&provider);
                            let cover_tasks: Vec<_> = albums
                                .iter()
                                .map(|album| {
                                    let key = crate::library::CoverArt::album_key(
                                        &album.artist,
                                        &album.name,
                                    );
                                    let prov2 = Arc::clone(&prov);
                                    let hint = album.cover_hint();
                                    tokio::task::spawn_blocking(move || {
                                        // Decode + downscale here too, inside
                                        // the blocking closure -- CPU-bound
                                        // work must not run on the async
                                        // task's tokio worker thread.
                                        let result = prov2.get_cover_art(&hint).map(|opt| {
                                            opt.map(|bytes| {
                                                let handle = cover_thumbnail_handle(&bytes);
                                                (handle, bytes)
                                            })
                                        });
                                        (key, result)
                                    })
                                })
                                .collect();

                            let mut cover_images = HashMap::new();
                            let mut cover_art_bytes = HashMap::new();
                            for task in cover_tasks {
                                if let Ok((key, Ok(Some((handle, bytes))))) = task.await {
                                    cover_images.insert(key.clone(), handle);
                                    cover_art_bytes.insert(key, bytes);
                                }
                            }

                            tracing::debug!(
                                "Subsonic batch: offset={offset}, albums={batch_count}"
                            );

                            _ = emitter
                                .send(cosmic::Action::App(Message::LibraryBatch {
                                    generation,
                                    provider_id: active_id.clone(),
                                    albums,
                                    cover_images,
                                    cover_art_bytes,
                                }))
                                .await;

                            if !has_more {
                                break;
                            }
                            offset += page_size;
                        }
                    }

                    crate::provider::ProviderType::Local => {
                        // Should not reach here — local uses reload_library_local.
                        unreachable!("Local provider should not use incremental reload");
                    }
                }

                _ = emitter
                    .send(cosmic::Action::App(Message::LibraryLoadComplete {
                        generation,
                        provider_id: active_id.clone(),
                    }))
                    .await;
            },
        );

        cosmic::task::stream(stream)
    }

    /// Persist the current config via cosmic-config.
    pub(super) fn save_config(&self) {
        if let Some(ref context) = self.config_context
            && let Err(e) = self.config.write_entry(context)
        {
            tracing::error!("Failed to save config: {e:?}");
        }
    }

    /// Re-initialize all MPD providers from the current config.
    ///
    /// Removes old MPD providers from the registry, creates new ones,
    /// and rebuilds the provider list for the header dropdown.
    pub(super) fn reinit_mpd_providers(&mut self) -> Task<cosmic::Action<Message>> {
        // Remove existing MPD providers from registry
        self.registry
            .remove_by_type(crate::provider::ProviderType::Mpd);
        self.mpd_providers.clear();

        // Re-create from config
        let rt_handle = tokio::runtime::Handle::current();
        for entry in &self.config.mpd_servers {
            let mpd_config: MpdConfig = entry.clone().into();
            let provider = Arc::new(MpdProvider::new(mpd_config, rt_handle.clone()));
            self.mpd_providers.push(Arc::clone(&provider));
            self.registry
                .register(Arc::clone(&provider) as Arc<dyn MusicProvider>);
        }

        self.rebuild_provider_list();

        // Rebuild edit states
        self.mpd_edit_states = self
            .config
            .mpd_servers
            .iter()
            .map(providers::MpdEditState::from_config)
            .collect();
        self.mpd_connection_status = vec![None; self.mpd_edit_states.len()];

        // Don't reload library here — for MPD providers, the idle
        // subscription will fire MpdConnected once connected, which
        // triggers reload. For local, reload immediately.
        if self
            .registry
            .active()
            .is_some_and(|p| p.provider_type() == crate::provider::ProviderType::Local)
        {
            self.reload_library()
        } else {
            Task::none()
        }
    }

    /// Re-initialize all Subsonic providers from the current config.
    ///
    /// Removes old Subsonic providers from the registry, creates new ones,
    /// and rebuilds the provider list for the header dropdown.
    pub(super) fn reinit_subsonic_providers(&mut self) -> Task<cosmic::Action<Message>> {
        // Remove existing Subsonic providers from registry
        self.registry
            .remove_by_type(crate::provider::ProviderType::Subsonic);
        self.subsonic_providers.clear();

        // Re-create from config (clone first: iterating `&self.config...`
        // while calling `self.push_toast()` inside the loop would otherwise
        // hold an immutable borrow of `self` across a mutable one).
        let rt_handle = tokio::runtime::Handle::current();
        let mut toast_tasks = Vec::new();
        let subsonic_servers = self.config.subsonic_servers.clone();
        for entry in &subsonic_servers {
            let subsonic_config: SubsonicConfig = entry.clone().into();
            match SubsonicProvider::new(subsonic_config, rt_handle.clone()) {
                Ok(provider) => {
                    let provider = Arc::new(provider);
                    self.subsonic_providers.push(Arc::clone(&provider));
                    self.registry
                        .register(Arc::clone(&provider) as Arc<dyn MusicProvider>);
                }
                Err(e) => {
                    tracing::error!("Failed to create Subsonic provider '{}': {e}", entry.name);
                    toast_tasks.push(self.push_toast(widget::toaster::Toast::new(fl!(
                        "toast-provider-connect-failed",
                        provider = entry.name.clone(),
                        reason = e.to_string()
                    ))));
                }
            }
        }

        self.rebuild_provider_list();

        // Rebuild edit states
        self.subsonic_edit_states = self
            .config
            .subsonic_servers
            .iter()
            .map(providers::SubsonicEditState::from_config)
            .collect();
        self.subsonic_connection_status = vec![None; self.subsonic_edit_states.len()];

        // For Subsonic providers, we can reload the library immediately
        // since they connect on demand (no idle subscription).
        let reload_task = if self
            .registry
            .active()
            .is_some_and(|p| p.provider_type() == crate::provider::ProviderType::Subsonic)
        {
            self.reload_library()
        } else {
            Task::none()
        };
        toast_tasks.push(reload_task);

        Task::batch(toast_tasks)
    }

    /// Re-initialize the Local provider with the current `config.music_dirs`.
    ///
    /// Removes the old Local provider from the registry, creates a new one
    /// with the updated scan directories, and rebuilds the provider list.
    pub(super) fn reinit_local_provider(&mut self) {
        self.registry
            .remove_by_type(crate::provider::ProviderType::Local);

        let db_path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("aulos")
            .join("library.db");

        if let Ok(db) = LibraryDb::open(&db_path) {
            let local = LocalProvider::new(db, self.config.music_dirs.clone());
            self.registry.register(Arc::new(local));
        } else {
            tracing::error!("Failed to open library database for reinit");
        }

        self.rebuild_provider_list();
    }

    /// Try to create an `MpdBackend` for the currently active provider.
    ///
    /// Returns `Some(MpdBackend)` if the active provider is an MPD provider
    /// and has a connected client. Returns `None` otherwise.
    pub(super) fn make_mpd_backend(&self) -> Option<MpdBackend> {
        let active_id = self.registry.active_id();
        self.mpd_providers
            .iter()
            .find(|p| p.id() == active_id)
            .and_then(|mpd| {
                let client = mpd.client_clone()?;
                Some(MpdBackend::new(client))
            })
    }

    /// Recreate the Player with the appropriate backend for the current provider.
    pub(super) fn recreate_player(&mut self) {
        let mpd_backend = self.make_mpd_backend();
        match Player::new(mpd_backend) {
            #[allow(unused_mut)]
            Ok(mut p) => {
                // Re-wire PCM buffer for visualizer
                #[cfg(feature = "visualizer")]
                if let Some(ref buf) = self.pcm_buffer {
                    tracing::debug!("Reconnecting PCM buffer to new player instance");
                    p.set_pcm_buffer(Arc::clone(buf));
                } else {
                    tracing::warn!("PCM buffer is None - visualizer will not receive audio");
                }

                // Apply saved EQ state to the new player's DSP.
                let eq = p.eq_controller();
                eq.set_enabled(self.config.equalizer_enabled);
                if self.config.equalizer_bands.len() == 10 {
                    let mut gains = [0.0_f32; 10];
                    gains.copy_from_slice(&self.config.equalizer_bands);
                    eq.set_all(&gains);
                }

                // Apply the saved master volume; a fresh `Player` hardcodes
                // 0.8, so without this a provider switch would reset the
                // level instead of preserving it.
                if let Err(e) = p.set_volume(self.config.volume) {
                    tracing::warn!("Failed to apply saved volume: {e}");
                }

                self.player = Some(p);
                self.apply_playback_extras_config();
            }
            Err(e) => {
                tracing::error!("Failed to recreate player: {e}");
                self.player = None;
            }
        }
    }

    /// Handle scrobble logic for the current track.
    ///
    /// Sends a "now playing" notification on first call for a track, then
    /// scrobbles when playback reaches 50% of duration or 4 minutes
    /// (whichever comes first). Only applies to Subsonic tracks.
    pub(super) fn handle_scrobble(&mut self, track: Track) {
        // ListenBrainz / Last.fm / Libre.fm (independent of the Subsonic
        // server-side scrobble below).
        if self
            .player
            .as_ref()
            .is_some_and(|p| p.state() == crate::player::PlaybackState::Playing)
        {
            self.scrobble.on_playback(&track, self.playback_position);
        }
        // Only scrobble Subsonic tracks.
        let provider = match self
            .subsonic_providers
            .iter()
            .find(|p| p.id() == &*track.provider_id)
        {
            Some(p) => Arc::clone(p),
            None => return,
        };

        // The Subsonic song ID is stored in track.path (set by child_to_track).
        let song_id = track.path.to_string_lossy().to_string();

        // Send "now playing" notification once per track.
        if !self.scrobble_now_playing_sent {
            self.scrobble_now_playing_sent = true;
            provider.now_playing(&song_id);
        }

        // Scrobble at 50% of duration or 4 minutes, whichever is first.
        if !self.scrobble_sent {
            let half_duration = track.duration / 2;
            let four_minutes = Duration::from_secs(240);
            let threshold = half_duration.min(four_minutes);

            if self.playback_position >= threshold && threshold > Duration::ZERO {
                self.scrobble_sent = true;
                provider.scrobble(&song_id);
            }
        }
    }

    /// Incrementally merge new albums into `all_artists`, splitting each
    /// album's primary-artist tag into individual collaborators when
    /// `split_artist_tags` is enabled — one album can then contribute to
    /// several `Artist` entries so browsing by any collaborator finds it.
    /// Albums keep a single primary attribution (`album.artist`, set by
    /// the provider) for the Albums view; only the artist index widens.
    /// Only processes the `new_albums` slice (the batch that just arrived),
    /// appending to existing artists or creating new ones. Avatars aren't
    /// generated here at all — the grid/list/detail views build the
    /// initials placeholder on the fly (see
    /// `crate::views::common::artist_avatar`), and real photos are fetched
    /// lazily by `load_artist_info_for_visible` when the Artists page is
    /// opened or an artist is selected.
    pub(super) fn merge_artists_from_batch(&mut self, new_albums: &[Album]) {
        // Build an index over the current artists list for O(1) lookup.
        let mut index: HashMap<String, usize> = self
            .all_artists
            .iter()
            .enumerate()
            .map(|(i, a)| (a.name.clone(), i))
            .collect();

        let split_enabled = self.config.split_artist_tags;
        let delimiters = self.config.artist_tag_delimiters.clone();

        for album in new_albums {
            let names: Vec<String> = if split_enabled {
                crate::library::artist_tags::split(&album.artist, &delimiters)
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            } else {
                vec![album.artist.clone()]
            };

            for name in names {
                if let Some(&idx) = index.get(&name) {
                    self.all_artists[idx].albums.push(album.clone());
                } else {
                    let idx = self.all_artists.len();
                    index.insert(name.clone(), idx);
                    self.all_artists.push(Artist {
                        name: name.clone(),
                        albums: vec![album.clone()],
                    });
                }
            }
        }
    }

    /// Rebuild `all_artists` from scratch by re-running the batch-merge
    /// aggregation (`merge_artists_from_batch`) over every album currently
    /// in `all_albums`. This is the single aggregation path — used
    /// whenever `split_artist_tags` or `artist_tag_delimiters` changes, so
    /// the artist index picks up the new split rules immediately, without
    /// a rescan.
    pub(super) fn rebuild_all_artists(&mut self) {
        self.all_artists.clear();
        let albums = std::mem::take(&mut self.all_albums);
        self.merge_artists_from_batch(&albums);
        self.all_artists.sort_by(|a, b| a.name.cmp(&b.name));
        self.all_albums = albums;
    }

    /// Start playback from the given queue at `start_index`.
    ///
    /// Takes ownership of the track list to avoid an extra clone — the
    /// caller is responsible for providing an owned `Vec<Track>`.
    pub(super) fn play_track_list(
        &mut self,
        tracks: Vec<Track>,
        start_index: usize,
    ) -> Task<cosmic::Action<Message>> {
        // Switching away from a podcast episode (to a different episode or
        // any other track) — persist its last known position first.
        let podcast_save_task = match self.current_podcast_episode_id.take() {
            Some(episode_id) => {
                let position_ms = self.playback_position.as_millis() as i64;
                self.save_podcast_position(episode_id, position_ms, false)
            }
            None => Task::none(),
        };

        if let Some(player) = &mut self.player {
            match player.set_queue(tracks, start_index) {
                Ok(Some(track)) => {
                    self.current_track = Some(track);
                    self.playback_position = Duration::ZERO;
                    let track_changed_task = self.on_track_changed();
                    return Task::batch([podcast_save_task, track_changed_task]);
                }
                Ok(None) => {}
                Err(e) => tracing::error!("play_track_list failed: {e}"),
            }
        }
        podcast_save_task
    }

    /// Common bookkeeping whenever the current track changes (queue
    /// advance/jump/edit, MPD adopting an externally-changed song, ...).
    /// Resets lyrics/scrobble state and the visualizer metadata fade-in,
    /// then kicks off the blur + async-MPD-dispatch + MPRIS-publish
    /// follow-up tasks. Callers must already have set `self.current_track`
    /// (and typically `self.playback_position = Duration::ZERO`) before
    /// calling this.
    pub(super) fn on_track_changed(&mut self) -> Task<cosmic::Action<Message>> {
        self.lyrics_text = None;
        self.scrobble_now_playing_sent = false;
        self.scrobble_sent = false;
        #[cfg(feature = "visualizer")]
        {
            self.viz_metadata_opacity = 1.0;
        }
        let mpd_task = self.dispatch_mpd_after_play();
        let blur_task = self.maybe_update_blurred_cover();
        let mpris_task = self.publish_mpris();
        let notify_task = self.notify_track_changed();
        Task::batch([mpd_task, blur_task, mpris_task, notify_task])
    }

    /// Builds an `MprisSnapshot` from current player/config state and
    /// forwards it to the MPRIS D-Bus handle, if the session-bus server is
    /// up. `MprisHandle::publish` diffs internally, so calling this
    /// unconditionally on every tick is cheap.
    ///
    /// Cover art resolution involves blocking file I/O (tag parsing, an
    /// on-disk cache write) the first time a track's art is requested, so
    /// it is never done synchronously here: if the current track's art
    /// isn't already cached on `handle`, this publishes without `art_url`
    /// for now and returns a background task that resolves it and
    /// republishes via `Message::MprisArtResolved` once ready.
    pub(super) fn publish_mpris(&self) -> Task<cosmic::Action<Message>> {
        let Some(handle) = self.mpris.as_ref() else {
            return Task::none();
        };

        let status = match self.player.as_ref().map(|p| p.state()) {
            Some(PlaybackState::Playing) => crate::mpris::MprisStatus::Playing,
            Some(PlaybackState::Paused) => crate::mpris::MprisStatus::Paused,
            Some(PlaybackState::Stopped) | None => crate::mpris::MprisStatus::Stopped,
        };

        let loop_mode = match self.config.repeat_mode {
            crate::config::RepeatMode::None => crate::mpris::LoopMode::None,
            crate::config::RepeatMode::All => crate::mpris::LoopMode::Playlist,
            crate::config::RepeatMode::One => crate::mpris::LoopMode::Track,
        };

        let track = self.current_track.as_ref();
        let (art_url, art_task) = match track {
            Some(t) => match handle.cached_art_url(t.id) {
                Some(cached) => (cached, Task::none()),
                None => (None, resolve_mpris_art_task(t.id, t.path.clone())),
            },
            None => (None, Task::none()),
        };
        let can_go_next = self.player.as_ref().is_some_and(|p| p.has_next());
        let can_go_previous = self.player.as_ref().is_some_and(|p| p.has_previous());

        handle.publish(crate::mpris::MprisSnapshot {
            status,
            title: track.map(|t| t.title.clone()).unwrap_or_default(),
            artist: track.map(|t| t.artist.clone()).unwrap_or_default(),
            album: track.map(|t| t.album.clone()).unwrap_or_default(),
            album_artist: track.map(|t| t.album_artist.clone()).unwrap_or_default(),
            genre: track.map(|t| t.genre.clone()).unwrap_or_default(),
            track_id: track.map(|t| t.id).unwrap_or(0),
            length_us: track.map(|t| t.duration.as_micros() as i64).unwrap_or(0),
            position_us: self.playback_position.as_micros() as i64,
            art_url,
            volume: self
                .player
                .as_ref()
                .map(|p| p.volume() as f64)
                .unwrap_or(self.config.volume as f64),
            shuffle: self.config.shuffle,
            loop_mode,
            can_go_next,
            can_go_previous,
            can_seek: track.is_some_and(|t| &*t.provider_id != "radio"),
            can_play: track.is_some(),
        });
        art_task
    }

    pub(super) fn sort_tracks(&mut self, field: songs::SortField) {
        match field {
            songs::SortField::Title => self.all_tracks.sort_by(|a, b| a.title.cmp(&b.title)),
            songs::SortField::Artist => self.all_tracks.sort_by(|a, b| a.artist.cmp(&b.artist)),
            songs::SortField::Album => self.all_tracks.sort_by(|a, b| a.album.cmp(&b.album)),
            songs::SortField::Duration => self.all_tracks.sort_by_key(|a| a.duration),
        }
    }

    /// Recomputes the search-filtered library caches from `library_search`.
    ///
    /// Called whenever the query changes and whenever the underlying library
    /// data is reloaded or re-sorted, so the filtered caches — and the index
    /// maps that translate a filtered position back to its index in the
    /// corresponding unfiltered vector — never go stale. When the query is
    /// empty the caches are cleared; `view()` then reads directly from the
    /// unfiltered vectors.
    pub(super) fn refresh_search_filter(&mut self) {
        let query = self.library_search.trim().to_lowercase();

        self.filtered_albums.clear();
        self.filtered_album_map.clear();
        self.filtered_artists.clear();
        self.filtered_artist_map.clear();
        self.filtered_tracks.clear();
        self.filtered_track_map.clear();
        self.filtered_playlists.clear();
        self.filtered_playlist_map.clear();
        self.filtered_genres.clear();
        self.filtered_genre_map.clear();

        // "Various Artists" can be hidden from the Artists view; that is
        // implemented through the same filtered cache the search uses.
        let hide_compilations = !self.config.show_compilations_in_artists;

        if query.is_empty() {
            if hide_compilations {
                for (i, artist) in self.all_artists.iter().enumerate() {
                    if !crate::library::compilations::is_various_artists(&artist.name) {
                        self.filtered_artists.push(artist.clone());
                        self.filtered_artist_map.push(i);
                    }
                }
            }
            return;
        }

        for (i, album) in self.all_albums.iter().enumerate() {
            if album.name.to_lowercase().contains(&query)
                || album.artist.to_lowercase().contains(&query)
            {
                self.filtered_albums.push(album.clone());
                self.filtered_album_map.push(i);
            }
        }

        for (i, artist) in self.all_artists.iter().enumerate() {
            if artist.name.to_lowercase().contains(&query)
                && !(hide_compilations
                    && crate::library::compilations::is_various_artists(&artist.name))
            {
                self.filtered_artists.push(artist.clone());
                self.filtered_artist_map.push(i);
            }
        }

        for (i, track) in self.all_tracks.iter().enumerate() {
            if track.title.to_lowercase().contains(&query)
                || track.artist.to_lowercase().contains(&query)
                || track.album.to_lowercase().contains(&query)
            {
                self.filtered_tracks.push(track.clone());
                self.filtered_track_map.push(i);
            }
        }

        for (i, playlist) in self.playlists.iter().enumerate() {
            if playlist.name.to_lowercase().contains(&query) {
                self.filtered_playlists.push(playlist.clone());
                self.filtered_playlist_map.push(i);
            }
        }

        for (i, genre) in self.all_genres.iter().enumerate() {
            if genre.to_lowercase().contains(&query) {
                self.filtered_genres.push(genre.clone());
                self.filtered_genre_map.push(i);
            }
        }
    }

    /// Every track under the folder view's current directory, recursively,
    /// in path order. Shared by "play folder" and "add folder to queue".
    pub(super) fn current_folder_tracks(&self) -> Vec<Track> {
        let dir = self.folder_state.current().to_path_buf();
        self.folder_state
            .tree()
            .tracks_in(&dir, true)
            .into_iter()
            .filter_map(|i| self.all_tracks.get(i).cloned())
            .collect()
    }

    /// Pushes a toast notification, mapping its auto-dismiss timer task into
    /// the `cosmic::Action`-wrapped message type `update()` returns.
    pub(super) fn push_toast(
        &mut self,
        toast: widget::toaster::Toast<Message>,
    ) -> Task<cosmic::Action<Message>> {
        self.toasts.push(toast).map(cosmic::Action::App)
    }

    /// Exit visualizer fullscreen if active: restore the COSMIC header bar,
    /// nav sidebar, and the real OS-level window mode (back to `Windowed`).
    /// No-op (returns `Task::none()`) when not in fullscreen. Called
    /// whenever the expanded now-playing view is left or the visualizer is
    /// turned off.
    #[cfg(feature = "visualizer")]
    pub(super) fn exit_viz_fullscreen(&mut self) -> Task<cosmic::Action<Message>> {
        if self.viz_fullscreen {
            self.viz_fullscreen = false;
            self.core.window.show_headerbar = true;
            self.core.nav_bar_set_toggled(self.viz_prev_nav_active);
            if let Some(id) = self.core.main_window_id() {
                return cosmic::iced::window::set_mode(id, cosmic::iced::window::Mode::Windowed);
            }
        }
        Task::none()
    }

    /// Trigger blur + accent-colour computation for the current track if
    /// the album changed.
    ///
    /// Checks if the current track's album key differs from the cached blurred
    /// cover key. If so, looks up the raw bytes and spawns a background task
    /// that computes both the blur and the cover-art accent colour from the
    /// same bytes (see `library::palette::extract`). Returns a Task that
    /// sends `Message::BlurReady`.
    pub(super) fn maybe_update_blurred_cover(&mut self) -> Task<cosmic::Action<Message>> {
        let track = match self.current_track.as_ref() {
            Some(t) => t,
            None => {
                // No track — clear everything.
                self.blurred_cover = None;
                self.blurred_cover_key = None;
                self.blur_pending_key = None;
                self.current_cover_large = None;
                self.accent = None;
                return Task::none();
            }
        };

        // Use album_artist to match how albums store cover art.
        // Falls back to track.artist when album_artist is empty.
        let artist = if track.album_artist.is_empty() {
            &track.artist
        } else {
            &track.album_artist
        };
        let key = crate::library::CoverArt::album_key(artist, &track.album);

        // Already computed and cached for this album — nothing to do.
        if self.blurred_cover_key.as_ref() == Some(&key) {
            return Task::none();
        }

        // A blur job for this exact album is already in flight (spawned
        // by an earlier call -- e.g. the previous `LibraryBatch` in an
        // incremental reload -- but `BlurReady` hasn't arrived yet).
        // Don't spawn a duplicate: each spawn would build a brand-new
        // (if identical) handle, defeating the point of caching by key.
        if self.blur_pending_key.as_ref() == Some(&key) {
            return Task::none();
        }

        // Look up raw bytes. If they are not available yet (still loading),
        // reset the key so we retry when bytes arrive, but keep the current
        // blurred_cover showing (previous track's blur) rather than blanking
        // the background immediately. The blur will update as soon as bytes
        // are ready and maybe_update_blurred_cover is called again.
        let bytes = match self.cover_art_bytes.get(&key) {
            Some(b) => b.clone(),
            None => {
                self.blurred_cover_key = None; // ensure retry on next bytes-ready event
                // Do NOT clear blurred_cover — keep the old blur visible.
                return Task::none();
            }
        };

        // Bytes are available — start the async blur+accent+large-cover
        // computation. Clear the cached-result key now so a concurrent
        // track change will not skip the next blur computation (BlurReady
        // carries the key and will only apply if it still matches the
        // current track); mark this key as pending so a second call
        // before the job finishes (see the guard above) does not spawn a
        // duplicate.
        self.blurred_cover_key = None;
        self.blur_pending_key = Some(key.clone());

        let key_clone = key.clone();
        cosmic::task::future(async move {
            // Compute blur, accent and a larger expanded-view cover in the
            // same blocking task, off the async runtime, from the same
            // bytes. Accent extraction is bounded-cost (fixed 32x32
            // working set) regardless of source resolution, so it adds no
            // meaningful overhead next to the blur/resize work.
            let (blurred, accent, cover_large) = tokio::task::spawn_blocking(move || {
                let blurred = crate::views::now_playing::blur::compute_blurred_cover(&bytes);
                let accent = crate::library::palette::extract(&bytes);
                let cover_large = crate::library::CoverArt::decode_thumbnail(
                    &bytes,
                    crate::library::CoverArt::EXPANDED_COVER_MAX_DIM,
                );
                (blurred, accent, cover_large)
            })
            .await
            .unwrap_or((None, None, None));

            let handle =
                blurred.map(|(w, h, pixels)| widget::icon::from_raster_pixels(w, h, pixels));
            let large_handle =
                cover_large.map(|(w, h, pixels)| widget::icon::from_raster_pixels(w, h, pixels));
            cosmic::Action::App(Message::BlurReady(key_clone, handle, accent, large_handle))
        })
    }

    /// Load genres from the active provider and dispatch a GenresLoaded message.
    pub(super) fn load_genres(&self) -> Task<cosmic::Action<Message>> {
        if let Some(provider) = self.registry.active_shared() {
            cosmic::task::future(async move {
                let genres = tokio::task::spawn_blocking(move || {
                    provider.list_genres().unwrap_or_else(|e| {
                        tracing::debug!("list_genres: {e}");
                        Vec::new()
                    })
                })
                .await
                .unwrap_or_default();
                cosmic::Action::App(Message::GenresLoaded(genres))
            })
        } else {
            Task::none()
        }
    }

    /// Fetch each not-yet-cached icon URL and dispatch `OnlineIconLoaded` for
    /// it, used for podcast artwork and radio station favicons alike.
    pub(super) fn load_online_icons(&self, urls: Vec<String>) -> Task<cosmic::Action<Message>> {
        let tasks: Vec<_> = urls
            .into_iter()
            .filter(|url| !url.is_empty() && !self.online_icons.contains_key(url))
            .map(|url| {
                let fetch_url = url.clone();
                cosmic::task::future(async move {
                    // Fetch and decode/downscale in the same blocking task —
                    // both are CPU/IO work that must never run on the async
                    // runtime's reactor thread, and there's nothing useful
                    // to do with the raw bytes on the way back except decode
                    // them, so there is no reason to hop back to `.await`
                    // in between.
                    let decoded = tokio::task::spawn_blocking(move || {
                        let bytes = HTTP_CLIENT
                            .clone()
                            .get(&fetch_url)
                            .send()
                            .ok()
                            .and_then(|r| r.bytes().ok())
                            .map(|b| b.to_vec())?;
                        crate::library::CoverArt::decode_thumbnail(
                            &bytes,
                            crate::library::CoverArt::ONLINE_ICON_MAX_DIM,
                        )
                    })
                    .await
                    .ok()
                    .flatten();
                    cosmic::Action::App(Message::OnlineIconLoaded(url, decoded))
                })
            })
            .collect();
        Task::batch(tasks)
    }

    /// Reads tags for a set of ad-hoc files -- double-clicked in a file
    /// manager via `Exec=aulos %U`, passed on the command line, or
    /// forwarded from another running instance's MPRIS `OpenUri` -- and
    /// queues the readable ones for playback. Deliberately bypasses the
    /// library database: these files need not live in any configured
    /// library directory, matching how every other desktop media player
    /// treats "open with".
    pub(super) fn open_files(&mut self, paths: Vec<PathBuf>) -> Task<cosmic::Action<Message>> {
        // Playlist files (.m3u/.m3u8/.pls) are imported as new playlists
        // instead of being played as audio.
        let (playlist_files, paths): (Vec<PathBuf>, Vec<PathBuf>) = paths
            .into_iter()
            .partition(|p| crate::library::m3u::is_playlist_path(p));
        let import_task = self.import_playlist_files(playlist_files);
        if paths.is_empty() {
            return import_task;
        }
        let play_task = cosmic::task::future(async move {
            let tracks = tokio::task::spawn_blocking(move || {
                paths
                    .into_iter()
                    .filter_map(|path| match LibraryScanner::read_metadata(&path) {
                        Ok(track) => Some(track),
                        Err(e) => {
                            tracing::warn!("Skipping unreadable file {}: {e}", path.display());
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            cosmic::Action::App(Message::OpenFilesScanned(tracks))
        });
        Task::batch([import_task, play_task])
    }

    /// Lazily fetches artist bio/image for the Artists page — called
    /// from `select_nav` when the page becomes active. Only artists not
    /// already resolved in memory, not already in flight, and not
    /// confirmed-negative this session are considered, capped per visit
    /// so opening the page never fires a burst of requests for a huge
    /// library; the remainder are picked up on a later visit, or
    /// immediately if individually selected (see
    /// `load_artist_info_for_selected`). A no-op in Local/MPD mode when
    /// `Config::fetch_artist_info` is off; always runs in Subsonic mode,
    /// whose data comes from the server, not the network toggle.
    pub(super) fn load_artist_info_for_visible(&mut self) -> Task<cosmic::Action<Message>> {
        /// Cap on artists newly dispatched per page visit — a courtesy
        /// bound independent of `ARTIST_INFO_CONCURRENCY` (which bounds
        /// how many run *at once*, not how many get queued).
        const MAX_PER_VISIT: usize = 40;

        let Some(provider) = self.registry.active_shared() else {
            return Task::none();
        };
        let is_subsonic = provider.provider_type() == crate::provider::ProviderType::Subsonic;
        if !is_subsonic && !self.config.fetch_artist_info {
            return Task::none();
        }

        let names: Vec<String> = self
            .all_artists
            .iter()
            .map(|a| a.name.clone())
            .filter(|name| self.artist_info_wanted(name))
            .take(MAX_PER_VISIT)
            .collect();

        self.dispatch_artist_info_fetch(provider, is_subsonic, names)
    }

    /// Fetches artist info for a single artist immediately (e.g. when
    /// selected from the detail view), bypassing the per-visit cap so a
    /// deep-linked or scrolled-past artist still gets its info without
    /// waiting for a later page visit.
    pub(super) fn load_artist_info_for_selected(
        &mut self,
        name: &str,
    ) -> Task<cosmic::Action<Message>> {
        let Some(provider) = self.registry.active_shared() else {
            return Task::none();
        };
        let is_subsonic = provider.provider_type() == crate::provider::ProviderType::Subsonic;
        if !is_subsonic && !self.config.fetch_artist_info {
            return Task::none();
        }
        if !self.artist_info_wanted(name) {
            return Task::none();
        }
        self.dispatch_artist_info_fetch(provider, is_subsonic, vec![name.to_string()])
    }

    /// True when `name` has neither a cached photo/bio nor an in-flight
    /// or confirmed-negative lookup — i.e. still worth dispatching.
    fn artist_info_wanted(&self, name: &str) -> bool {
        !self.artist_photos.contains_key(name)
            && !self.artist_bios.contains_key(name)
            && !self.artist_info_negative.contains(name)
            && !self.artist_info_pending.contains(name)
    }

    /// Marks `names` pending and dispatches one `cosmic::task::future`
    /// per artist, each acquiring `artist_info_semaphore` before its
    /// blocking resolve (cache hit or network fetch) — mirroring
    /// `crate::convert::run_job`'s acquire-then-`spawn_blocking` pattern
    /// — so the actual concurrency bound lives in one shared semaphore
    /// rather than in how many tasks happen to be dispatched at once.
    fn dispatch_artist_info_fetch(
        &mut self,
        provider: Arc<dyn MusicProvider>,
        is_subsonic: bool,
        names: Vec<String>,
    ) -> Task<cosmic::Action<Message>> {
        if names.is_empty() {
            return Task::none();
        }
        for name in &names {
            self.artist_info_pending.insert(name.clone());
        }

        let provider_id = provider.id().to_string();
        let tasks: Vec<Task<cosmic::Action<Message>>> = names
            .into_iter()
            .map(|name| {
                let provider = Arc::clone(&provider);
                let store = Arc::clone(&self.artist_info_store);
                let semaphore = Arc::clone(&self.artist_info_semaphore);
                let provider_id = provider_id.clone();
                cosmic::task::future(async move {
                    let permit = semaphore.acquire_owned().await.ok();
                    let now = super::now_epoch();
                    let name_for_panic = name.clone();
                    let outcome = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        if is_subsonic {
                            crate::library::artist_info::resolve_via_provider(
                                &store,
                                provider.as_ref(),
                                &provider_id,
                                &name,
                                now,
                            )
                        } else {
                            crate::library::artist_info::resolve_via_agents(&store, &name, now)
                        }
                    })
                    .await
                    .unwrap_or_else(|_| {
                        crate::library::artist_info::ArtistInfoOutcome::empty(name_for_panic)
                    });
                    cosmic::Action::App(Message::ArtistInfoLoaded(outcome))
                })
            })
            .collect();

        Task::batch(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_duration_live_track_position_is_never_clamped() {
        // A live radio track always reports `duration: Duration::ZERO`
        // (see `radio_page.rs`) — its ever-increasing playback position
        // must never get clamped back down to zero, or the UI would show
        // 0:00 forever despite the stream actually playing.
        let position = Duration::from_secs(3600);
        assert_eq!(clamp_display_position(position, Duration::ZERO), position);
    }

    #[test]
    fn known_duration_clamps_overshoot() {
        let duration = Duration::from_secs(180);
        let position = Duration::from_secs(181);
        assert_eq!(clamp_display_position(position, duration), duration);
    }

    #[test]
    fn known_duration_leaves_in_range_position_untouched() {
        let duration = Duration::from_secs(180);
        let position = Duration::from_secs(90);
        assert_eq!(clamp_display_position(position, duration), position);
    }
}
