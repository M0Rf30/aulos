// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::tasks::resolve_mpris_art_task;
use super::{AppModel, Message};
use aulos_core::config::Config;
use aulos_core::fl;
use aulos_core::library::{Album, LibraryDb, LibraryScanner, Track};
use aulos_core::player::mpd_backend::MpdBackend;
use aulos_core::player::{ActiveBackend, PlaybackState, Player};
use aulos_core::provider::MusicProvider;
use aulos_core::provider::local::LocalProvider;
use aulos_core::provider::mpd::{MpdConfig, MpdProvider};
use aulos_core::provider::subsonic::{SubsonicConfig, SubsonicProvider};
use cosmic::prelude::*;
use cosmic::widget;
use futures_util::SinkExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The UI half of a non-destructive library refresh (the data half is
/// `LibraryState`'s reload staging): cover handles accumulate here while
/// the previously loaded covers stay on screen.
/// `Message::LibraryLoadComplete` swaps this into
/// `cover_images`/`cover_fingerprints` atomically together with the staged
/// library data -- see `AppModel::reload_library` and the
/// `Message::LibraryBatch`/`LibraryLoadComplete` handlers.
pub(super) struct CoverStaging {
    pub(super) cover_images: HashMap<String, widget::icon::Handle>,
    pub(super) cover_fingerprints: HashMap<String, u64>,
}

/// Build a `widget::icon::Handle` for a cover-art byte blob, preferring a
/// pre-decoded, downscaled RGBA thumbnail (`from_raster_pixels`) over the
/// original encoded bytes (`from_raster_bytes`) -- the latter forces a
/// full-resolution JPEG/PNG decode on the UI thread every time iced's
/// wgpu raster cache evicts and re-uploads the handle (e.g. every page
/// switch). Falls back to the encoded bytes if decoding fails, so a
/// corrupt/unusual cover blob is not silently dropped.
fn cover_thumbnail_handle(bytes: &[u8]) -> widget::icon::Handle {
    match aulos_core::library::CoverArt::decode_thumbnail(
        bytes,
        aulos_core::library::CoverArt::GRID_THUMBNAIL_MAX_DIM,
    ) {
        Some((w, h, pixels)) => widget::icon::from_raster_pixels(w, h, pixels),
        None => widget::icon::from_raster_bytes(bytes.to_vec()),
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
    /// Aulos drives MPD one track at a time from its own [`aulos_core::player`]
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

    /// True when an async library result tagged with `generation`/
    /// `provider_id` is stale — superseded by a later reload/scan or a
    /// provider switch — and must be ignored without mutating state.
    pub(super) fn is_stale_reload(&self, generation: u64, provider_id: &str) -> bool {
        self.library
            .is_stale_reload(self.registry.active_id(), generation, provider_id)
    }

    pub(super) fn reload_library(&mut self) -> Task<cosmic::Action<Message>> {
        let provider = match self.registry.active_shared() {
            Some(p) => p,
            None => return Task::none(),
        };
        let provider_type = provider.provider_type();
        let generation = self.library.begin_reload_generation();

        match provider_type {
            aulos_core::provider::ProviderType::Local => {
                self.reload_library_local(provider, generation)
            }
            aulos_core::provider::ProviderType::Mpd
            | aulos_core::provider::ProviderType::Subsonic => {
                self.library_scanning = true;
                if self.library.begin_incremental_reload(generation) {
                    // A library is already showing: keep it fully visible
                    // and accumulate the refreshed data off to the side.
                    // `Message::LibraryLoadComplete` swaps it in atomically
                    // once the whole reload finishes, so the view never
                    // goes empty mid-reload (see `CoverStaging`).
                    self.cover_staging = Some(CoverStaging {
                        cover_images: HashMap::new(),
                        cover_fingerprints: HashMap::new(),
                    });
                } else {
                    // First load (or the library is genuinely empty): there
                    // is nothing on screen to flash away from, so populate
                    // progressively straight into the visible fields as
                    // batches arrive, same as before.
                    self.cover_staging = None;
                    self.cover_images.clear();
                    self.cover_fingerprints.clear();
                    self.cover_art_bytes.clear();
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
                    let key = aulos_core::library::CoverArt::album_key(&album.artist, &album.name);
                    album.tracks.first().map(|track| (key, track.path.clone()))
                })
                .map(|(key, path)| {
                    tokio::task::spawn_blocking(
                        move || -> Option<(String, widget::icon::Handle, u64)> {
                            let bytes = aulos_core::library::CoverArt::get_cover_art(&path)?;
                            // Decode + downscale here too -- this is CPU-bound
                            // work and must stay off the async task (which
                            // runs on a tokio worker thread shared with other
                            // futures, not a dedicated blocking thread).
                            // The full-size bytes are dropped right here;
                            // they are re-read lazily for whichever album
                            // actually needs them (blur, hero, notification).
                            let handle = cover_thumbnail_handle(&bytes);
                            Some((
                                key,
                                handle,
                                aulos_core::library::CoverArt::content_fingerprint(&bytes),
                            ))
                        },
                    )
                })
                .collect();

            let mut cover_images = HashMap::new();
            let mut cover_fingerprints = HashMap::new();
            for task in cover_tasks {
                if let Ok(Some((key, handle, fingerprint))) = task.await {
                    cover_images.insert(key.clone(), handle);
                    cover_fingerprints.insert(key, fingerprint);
                }
            }

            cosmic::Action::App(Message::LibraryLoaded {
                generation,
                provider_id,
                tracks,
                albums,
                artists,
                cover_images,
                cover_fingerprints,
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
        provider_type: aulos_core::provider::ProviderType,
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
                    aulos_core::provider::ProviderType::Mpd => {
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
                                    let key = aulos_core::library::CoverArt::album_key(
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
                                                (handle, aulos_core::library::CoverArt::content_fingerprint(&bytes))
                                            })
                                        });
                                        (key, result)
                                    })
                                })
                                .collect();

                            let mut cover_images = HashMap::new();
                            let mut cover_fingerprints = HashMap::new();
                            for task in cover_tasks {
                                if let Ok((key, Ok(Some((handle, fingerprint))))) = task.await {
                                    cover_images.insert(key.clone(), handle);
                                    cover_fingerprints.insert(key, fingerprint);
                                }
                            }

                            _ = emitter
                                .send(cosmic::Action::App(Message::LibraryBatch {
                                    generation,
                                    provider_id: active_id.clone(),
                                    albums,
                                    cover_images,
                                    cover_fingerprints,
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

                    aulos_core::provider::ProviderType::Subsonic => {
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
                                    let key = aulos_core::library::CoverArt::album_key(
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
                                                (handle, aulos_core::library::CoverArt::content_fingerprint(&bytes))
                                            })
                                        });
                                        (key, result)
                                    })
                                })
                                .collect();

                            let mut cover_images = HashMap::new();
                            let mut cover_fingerprints = HashMap::new();
                            for task in cover_tasks {
                                if let Ok((key, Ok(Some((handle, fingerprint))))) = task.await {
                                    cover_images.insert(key.clone(), handle);
                                    cover_fingerprints.insert(key, fingerprint);
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
                                    cover_fingerprints,
                                }))
                                .await;

                            if !has_more {
                                break;
                            }
                            offset += page_size;
                        }
                    }

                    aulos_core::provider::ProviderType::Local => {
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

    /// Persist the current config. Non-blocking: the write happens on the
    /// config writer thread, coalesced and limited to changed keys.
    pub(super) fn save_config(&self) {
        if let Some(writer) = &self.config_writer {
            let mut recent = self.recent_saves.borrow_mut();
            if recent.back() == Some(&self.config) {
                return; // unchanged since the last save request
            }
            if recent.len() == 8 {
                recent.pop_front();
            }
            recent.push_back(self.config.clone());
            writer.save(&self.config);
        }
    }

    /// Whether `config` is one of our own recent saves coming back through
    /// the config watcher (so it must not overwrite newer in-memory state).
    pub(super) fn is_own_config_echo(&self, config: &Config) -> bool {
        self.recent_saves.borrow().iter().any(|c| c == config)
    }

    /// Wait for queued config writes to reach disk (before exiting).
    pub(super) fn flush_config(&self) {
        if let Some(writer) = &self.config_writer {
            writer.flush();
        }
    }

    /// Re-initialize all MPD providers from the current config.
    ///
    /// Removes old MPD providers from the registry, creates new ones,
    /// and rebuilds the provider list for the header dropdown.
    pub(super) fn reinit_mpd_providers(&mut self) -> Task<cosmic::Action<Message>> {
        // Remove existing MPD providers from registry
        self.registry
            .remove_by_type(aulos_core::provider::ProviderType::Mpd);
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
            .map(aulos_core::config::edit_state::MpdEditState::from_config)
            .collect();
        self.mpd_connection_status = vec![None; self.mpd_edit_states.len()];

        // Don't reload library here — for MPD providers, the idle
        // subscription will fire MpdConnected once connected, which
        // triggers reload. For local, reload immediately.
        if self
            .registry
            .active()
            .is_some_and(|p| p.provider_type() == aulos_core::provider::ProviderType::Local)
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
            .remove_by_type(aulos_core::provider::ProviderType::Subsonic);
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
            .map(aulos_core::config::edit_state::SubsonicEditState::from_config)
            .collect();
        self.subsonic_connection_status = vec![None; self.subsonic_edit_states.len()];

        // For Subsonic providers, we can reload the library immediately
        // since they connect on demand (no idle subscription).
        let reload_task = if self
            .registry
            .active()
            .is_some_and(|p| p.provider_type() == aulos_core::provider::ProviderType::Subsonic)
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
        self.music_dirs_present = self.config.music_dirs.iter().any(|d| d.is_dir());
        self.registry
            .remove_by_type(aulos_core::provider::ProviderType::Local);

        let db_path = aulos_core::paths::library_db_path();

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
            .is_some_and(|p| p.state() == aulos_core::player::PlaybackState::Playing)
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
            Some(PlaybackState::Playing) => aulos_core::mpris::MprisStatus::Playing,
            Some(PlaybackState::Paused) => aulos_core::mpris::MprisStatus::Paused,
            Some(PlaybackState::Stopped) | None => aulos_core::mpris::MprisStatus::Stopped,
        };

        let loop_mode = match self.config.repeat_mode {
            aulos_core::config::RepeatMode::None => aulos_core::mpris::LoopMode::None,
            aulos_core::config::RepeatMode::All => aulos_core::mpris::LoopMode::Playlist,
            aulos_core::config::RepeatMode::One => aulos_core::mpris::LoopMode::Track,
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

        handle.publish(aulos_core::mpris::MprisSnapshot {
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

    /// Recomputes the search-filtered library caches (see
    /// `LibraryState::refresh_search`) and drops the album-filter memo,
    /// since the albums slice the Albums page shows may have changed.
    pub(super) fn refresh_search_filter(&mut self) {
        self.extras.album_cache.invalidate();
        self.library
            .refresh_search(&self.playlists, !self.config.show_compilations_in_artists);
    }

    /// Every track under the folder view's current directory, recursively,
    /// in path order. Shared by "play folder" and "add folder to queue".
    pub(super) fn current_folder_tracks(&self) -> Vec<Track> {
        let dir = self.folder_state.current().to_path_buf();
        self.folder_state
            .tree()
            .tracks_in(&dir, true)
            .into_iter()
            .filter_map(|i| self.library.tracks.get(i).cloned())
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

    /// Keep the already-displayed handle for every cover whose content
    /// fingerprint is unchanged, so iced's raster cache never re-uploads it
    /// after a reload.
    pub(super) fn reuse_unchanged_cover_handles(
        &self,
        new_images: &mut HashMap<String, widget::icon::Handle>,
        new_fingerprints: &HashMap<String, u64>,
    ) {
        for (key, fingerprint) in new_fingerprints {
            if self.cover_fingerprints.get(key) == Some(fingerprint)
                && let Some(handle) = self.cover_images.get(key)
            {
                new_images.insert(key.clone(), handle.clone());
            }
        }
    }

    /// Load full-size cover bytes for an album key into the LRU on a
    /// blocking thread, unless they are cached / in flight / known absent.
    /// Replies with `Message::CoverBytesLoaded`. `hint` is only evaluated
    /// when a load is actually going to start.
    pub(super) fn request_cover_bytes(
        &mut self,
        key: String,
        hint: impl FnOnce(&Self) -> Option<Album>,
    ) -> Task<cosmic::Action<Message>> {
        if !self.cover_art_bytes.should_load(&key) {
            return Task::none();
        }
        let Some(provider) = self.registry.active_shared() else {
            return Task::none();
        };
        let Some(hint) = hint(self) else {
            // Album not (yet) in the library -- e.g. mid incremental load.
            // Don't mark it missing; the next library event retries.
            return Task::none();
        };
        if !self.cover_art_bytes.begin_load(&key) {
            return Task::none();
        }
        cosmic::task::future(async move {
            let bytes =
                tokio::task::spawn_blocking(move || provider.get_cover_art(&hint).ok().flatten())
                    .await
                    .ok()
                    .flatten();
            cosmic::Action::App(Message::CoverBytesLoaded(key, bytes))
        })
    }

    /// [`Self::request_cover_bytes`] for the album identified by its
    /// `(artist, album)` pair (how the current track maps to a cover key).
    pub(super) fn request_cover_bytes_for(
        &mut self,
        artist: &str,
        album_name: &str,
    ) -> Task<cosmic::Action<Message>> {
        let key = aulos_core::library::CoverArt::album_key(artist, album_name);
        self.request_cover_bytes(key, |m| m.library.cover_hint_for(artist, album_name))
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

        // Keyed by album artist (falling back to the track artist) to match
        // how albums store cover art.
        let key = track.cover_key();

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

        // Look up raw bytes. If they are not cached yet, load them lazily
        // (the library load no longer carries full-size covers), reset the
        // key so we retry when `CoverBytesLoaded` arrives, but keep the
        // current blurred_cover showing (previous track's blur) rather than
        // blanking the background immediately.
        let bytes = match self.cover_art_bytes.get(&key) {
            Some(b) => b,
            None => {
                self.blurred_cover_key = None; // ensure retry on next bytes-ready event
                // Do NOT clear blurred_cover — keep the old blur visible.
                let artist = track.grouping_artist().to_string();
                let album = track.album.clone();
                return self.request_cover_bytes_for(&artist, &album);
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
                let blurred = aulos_core::library::CoverArt::blurred_backdrop(&bytes);
                let accent = aulos_core::library::palette::extract(&bytes);
                let cover_large = aulos_core::library::CoverArt::decode_thumbnail(
                    &bytes,
                    aulos_core::library::CoverArt::EXPANDED_COVER_MAX_DIM,
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
                        let bytes = aulos_core::online::HTTP_CLIENT
                            .clone()
                            .get(&fetch_url)
                            .send()
                            .ok()
                            .and_then(|r| r.bytes().ok())
                            .map(|b| b.to_vec())?;
                        aulos_core::library::CoverArt::decode_thumbnail(
                            &bytes,
                            aulos_core::library::CoverArt::ONLINE_ICON_MAX_DIM,
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
            .partition(|p| aulos_core::library::m3u::is_playlist_path(p));
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
        let is_subsonic = provider.provider_type() == aulos_core::provider::ProviderType::Subsonic;
        if !is_subsonic && !self.config.fetch_artist_info {
            return Task::none();
        }

        let names: Vec<String> = self
            .library
            .artists
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
        let is_subsonic = provider.provider_type() == aulos_core::provider::ProviderType::Subsonic;
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
    /// `aulos_core::convert::run_job`'s acquire-then-`spawn_blocking` pattern
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
                    let now = aulos_core::unix_now();
                    let name_for_panic = name.clone();
                    let outcome = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        if is_subsonic {
                            aulos_core::library::artist_info::resolve_via_provider(
                                &store,
                                provider.as_ref(),
                                &provider_id,
                                &name,
                                now,
                            )
                        } else {
                            aulos_core::library::artist_info::resolve_via_agents(&store, &name, now)
                        }
                    })
                    .await
                    .unwrap_or_else(|_| {
                        aulos_core::library::artist_info::ArtistInfoOutcome::empty(name_for_panic)
                    });
                    cosmic::Action::App(Message::ArtistInfoLoaded(outcome))
                })
            })
            .collect();

        Task::batch(tasks)
    }
}
