// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::helpers::clamp_display_position;
use super::message::PlaylistOp;
use super::{AppModel, ContextPage, Message, Page, SEARCH_INPUT_ID, parse_delimiters_input};
use crate::config::ReplayGainMode;
use crate::fl;
use crate::library::{LibraryScanner, LyricsProvider};
use crate::player::{ActiveBackend, PlaybackState};
use crate::views::providers;
use cosmic::Application;
use cosmic::prelude::*;
use cosmic::widget::{self, nav_bar};
use std::path::PathBuf;
use std::time::Duration;

impl AppModel {
    pub(super) fn handle_message(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::LaunchUrl(url) => {
                open::that_detached(&url).ok();
            }

            Message::ToggleContextPage(page) => {
                if self.context_page == page {
                    self.core.window.show_context = !self.core.window.show_context;
                } else {
                    self.context_page = page;
                    self.core.window.show_context = true;
                }
            }

            Message::TextInputFocused(focused) => {
                self.text_input_focused = focused;
            }

            // -- Library search --
            Message::LibrarySearchChanged(query) => {
                self.library_search = query;
                self.search_debounce_gen += 1;
                if self.library_search.trim().is_empty() {
                    // Clearing is cheap and should feel instant.
                    self.refresh_search_filter();
                } else {
                    let pending = self.search_debounce_gen;
                    return cosmic::task::future(async move {
                        tokio::time::sleep(Duration::from_millis(
                            super::search_index::SEARCH_DEBOUNCE_MS,
                        ))
                        .await;
                        cosmic::Action::App(Message::LibrarySearchDebounced(pending))
                    });
                }
            }

            Message::LibrarySearchDebounced(pending) => {
                if super::search_index::is_latest_generation(pending, self.search_debounce_gen) {
                    self.refresh_search_filter();
                }
            }

            Message::ToggleLibrarySearch => {
                self.search_active = !self.search_active;
                if self.search_active {
                    return cosmic::widget::text_input::focus(widget::Id::new(SEARCH_INPUT_ID))
                        .map(cosmic::Action::App);
                }
                self.library_search.clear();
                self.refresh_search_filter();
            }

            Message::ClearLibrarySearch => {
                self.library_search.clear();
                self.search_active = false;
                self.refresh_search_filter();
            }

            Message::CloseToast(id) => {
                self.toasts.remove(id);
            }

            // -- Library --
            Message::ScanLibrary => {
                if let Some(provider) = self.registry.active_shared() {
                    match provider.provider_type() {
                        crate::provider::ProviderType::Local => {
                            self.library_scanning = true;
                            let generation = self.begin_reload_generation();
                            let provider_id = provider.id().to_string();
                            return cosmic::task::future(async move {
                                let count = tokio::task::spawn_blocking(move || {
                                    provider.sync_library().unwrap_or_else(|e| {
                                        tracing::error!("sync_library failed: {e}");
                                        0
                                    })
                                })
                                .await
                                .unwrap_or(0);
                                cosmic::Action::App(Message::LibraryScanComplete {
                                    generation,
                                    provider_id,
                                    count,
                                })
                            });
                        }
                        crate::provider::ProviderType::Subsonic => {
                            // Subsonic providers connect on demand — no idle subscription.
                            // Trigger a library reload directly (sets library_scanning).
                            return self.reload_library();
                        }
                        crate::provider::ProviderType::Mpd => {
                            // MPD providers: don't scan at startup; the idle
                            // subscription fires MpdConnected which triggers reload.
                            tracing::info!(
                                "Skipping scan for MPD provider '{}' — waiting for connection",
                                self.registry.active_id()
                            );
                        }
                    }
                }
            }

            Message::LibraryScanComplete {
                generation,
                provider_id,
                count,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return Task::none();
                }
                self.library_scanning = false;
                tracing::info!("Library scan complete: {count} tracks updated");
                // Only reload if tracks actually changed to avoid unnecessary
                // view rebuilds (which reset scroll position).
                if count > 0 || self.all_tracks.is_empty() {
                    return self.reload_library();
                }
            }

            Message::LibraryLoaded {
                generation,
                provider_id,
                tracks,
                albums,
                artists: _artists,
                mut cover_images,
                cover_fingerprints,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return Task::none();
                }
                self.library_scanning = false;
                self.all_tracks = tracks;
                self.all_albums = albums;
                // Keep the existing handle for covers whose content did not
                // change so iced's raster cache never re-uploads them.
                self.reuse_unchanged_cover_handles(&mut cover_images, &cover_fingerprints);
                self.cover_images = cover_images;
                self.cover_fingerprints = cover_fingerprints;
                self.rebuild_all_artists();
                // Full-size bytes are loaded lazily; drop any stale ones.
                self.cover_art_bytes.clear();
                // Rebuild the folder tree only if it's already in use —
                // never-opened Folders view pays nothing on reload.
                if self.folder_state.is_populated()
                    || self.nav.active_data::<Page>() == Some(&Page::Folders)
                {
                    self.folder_state
                        .set_tree(crate::views::folders::FolderTree::build(&self.all_tracks));
                    self.folder_tree_gen = Some(self.library_gen);
                }
                self.refresh_search_filter();
                // Re-trigger blur now that cover art bytes are available
                let blur_task = self.maybe_update_blurred_cover();
                let home_task = self.load_home(true);
                return Task::batch([blur_task, home_task]);
            }

            Message::LibraryBatch {
                generation,
                provider_id,
                albums,
                cover_images,
                cover_fingerprints,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return Task::none();
                }

                let refreshing_loaded_library = self
                    .library_reload_staging
                    .as_ref()
                    .is_some_and(|s| s.generation == generation);

                if refreshing_loaded_library {
                    // Non-destructive refresh: accumulate into the staging
                    // buffer while the previously loaded library stays
                    // fully visible. Reuse the currently-displayed handle
                    // for any album whose cover bytes are unchanged, so
                    // iced's wgpu raster cache keeps the same handle id
                    // and never re-uploads that cover.
                    for (key, fingerprint) in cover_fingerprints {
                        let reused = if self.cover_fingerprints.get(&key) == Some(&fingerprint) {
                            self.cover_images.get(&key).cloned()
                        } else {
                            None
                        };
                        let handle = reused.or_else(|| cover_images.get(&key).cloned());
                        if let Some(staging) = self.library_reload_staging.as_mut() {
                            if let Some(handle) = handle {
                                staging.cover_images.insert(key.clone(), handle);
                            }
                            staging.cover_fingerprints.insert(key, fingerprint);
                        }
                    }
                    if let Some(staging) = self.library_reload_staging.as_mut() {
                        for album in &albums {
                            for track in &album.tracks {
                                staging.tracks.push(track.clone());
                            }
                        }
                        staging.albums.extend(albums);
                    }
                    return Task::none();
                }

                // Progressive first-load path: the library was empty when
                // this reload started, so there is nothing to flash away
                // from -- populate the visible fields directly as batches
                // arrive.
                for album in &albums {
                    for track in &album.tracks {
                        self.all_tracks.push(track.clone());
                    }
                    self.all_albums.push(album.clone());
                }
                self.cover_images.extend(cover_images);
                self.cover_fingerprints.extend(cover_fingerprints);
                self.merge_artists_from_batch(&albums);
                if !self.library_search.trim().is_empty()
                    || (!self.config.show_compilations_in_artists
                        && albums
                            .iter()
                            .any(|a| crate::library::compilations::is_various_artists(&a.artist)))
                {
                    self.refresh_search_filter();
                }

                // Re-trigger blur in case the current track's cover just arrived
                let blur_task = self.maybe_update_blurred_cover();
                return blur_task;
            }

            Message::LibraryLoadComplete {
                generation,
                provider_id,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return Task::none();
                }
                self.library_scanning = false;

                if let Some(staging) = self.library_reload_staging.take()
                    && staging.generation == generation
                {
                    // Swap the staged refresh in atomically -- the
                    // previously displayed library was never cleared, so
                    // there is no empty-view flash between old and new data.
                    self.all_tracks = staging.tracks;
                    self.all_albums = staging.albums;
                    self.cover_images = staging.cover_images;
                    self.cover_fingerprints = staging.cover_fingerprints;
                    self.cover_art_bytes.clear();
                    self.rebuild_all_artists();
                }

                // Final sort
                self.all_tracks.sort_by(|a, b| a.title.cmp(&b.title));
                self.all_albums.sort_by(|a, b| a.name.cmp(&b.name));
                self.all_artists.sort_by(|a, b| a.name.cmp(&b.name));
                self.library_gen += 1;
                self.refresh_search_filter();
                tracing::info!(
                    "Library load complete: {} albums, {} tracks, {} artists",
                    self.all_albums.len(),
                    self.all_tracks.len(),
                    self.all_artists.len()
                );
                // Re-trigger blur in case the current track's cover
                // changed as part of a staged refresh swap.
                let blur_task = self.maybe_update_blurred_cover();
                let home_task = self.load_home(true);
                return Task::batch([blur_task, home_task]);
            }

            // -- Filesystem watcher --
            Message::FilesChanged(paths) => {
                // Filter out directories and non-existent-but-not-deleted paths.
                let paths: Vec<PathBuf> = paths
                    .into_iter()
                    .filter(|p| p.is_file() || !p.exists())
                    .collect();

                if paths.is_empty() {
                    return Task::none();
                }

                tracing::info!("Filesystem watcher detected {} changed paths", paths.len());

                // Run incremental scan on the changed paths in a background task.
                if let Some(provider) = self.registry.active_shared()
                    && provider.provider_type() == crate::provider::ProviderType::Local
                {
                    self.library_scanning = true;
                    let generation = self.begin_reload_generation();
                    let provider_id = provider.id().to_string();
                    return cosmic::task::future(async move {
                        let count = tokio::task::spawn_blocking(move || {
                            // The LocalProvider wraps LibraryDb in a Mutex.
                            // For incremental scan, we access the DB through the
                            // same path used by sync_library — open a temporary DB
                            // connection for the scan, or re-use the provider's scan.
                            // Since LocalProvider::sync_library calls LibraryScanner::scan,
                            // we open the DB directly for scan_paths.
                            let db_path = dirs::data_dir()
                                .unwrap_or_else(|| PathBuf::from("."))
                                .join("aulos")
                                .join("library.db");

                            match crate::library::LibraryDb::open(&db_path) {
                                Ok(db) => {
                                    LibraryScanner::scan_paths(&db, &paths).unwrap_or_else(|e| {
                                        tracing::error!("scan_paths failed: {e}");
                                        0
                                    })
                                }
                                Err(e) => {
                                    tracing::error!("Failed to open DB for incremental scan: {e}");
                                    0
                                }
                            }
                        })
                        .await
                        .unwrap_or(0);
                        cosmic::Action::App(Message::LibraryScanComplete {
                            generation,
                            provider_id,
                            count,
                        })
                    });
                }
            }

            // -- Playback --
            Message::TogglePlayback => {
                // Collect the follow-up task rather than returning inline, so
                // the MPRIS snapshot is refreshed on every path — pausing is
                // exactly when the periodic publish stops running.
                let mut task = Task::none();
                if let Some(player) = &mut self.player {
                    if player.state() == PlaybackState::Stopped {
                        // Resume wherever the queue left off (e.g. after
                        // `Stop` or after `RepeatMode::None` ran out).
                        // Only fall back to the full library when the
                        // queue itself is empty — starting from `Stopped`
                        // must never silently replace a queue the user
                        // still has loaded.
                        let result = if !player.queue_is_empty() {
                            player.resume_queue()
                        } else if !self.all_tracks.is_empty() {
                            player.set_queue(self.all_tracks.clone(), 0)
                        } else {
                            Ok(None)
                        };
                        match result {
                            Ok(Some(track)) => {
                                self.current_track = Some(track);
                                self.playback_position = Duration::ZERO;
                                task = self.on_track_changed();
                            }
                            Ok(None) => {}
                            Err(e) => tracing::error!("Resume from stopped failed: {e}"),
                        }
                    } else {
                        let was_playing = player.state() == PlaybackState::Playing;
                        if let Err(e) = player.toggle_playback() {
                            tracing::error!("Playback toggle failed: {e}");
                        } else if let Some(client) = self.mpd_client() {
                            // Dispatch async SetPause to MPD.
                            task = self.dispatch_mpd(async move {
                                client
                                    .command(mpd_client::commands::SetPause(was_playing))
                                    .await
                                    .map_err(|e| format!("MPD set_pause: {e}"))
                            });
                        }
                    }
                }
                let mpris_task = self.publish_mpris();
                return Task::batch([task, mpris_task]);
            }

            Message::NextTrack => {
                if let Some(player) = &mut self.player {
                    match player.next() {
                        Ok(Some(track)) => {
                            self.current_track = Some(track);
                            self.playback_position = Duration::ZERO;
                            return self.on_track_changed();
                        }
                        Ok(None) => {}
                        Err(e) => tracing::error!("Next track failed: {e}"),
                    }
                }
            }

            Message::PreviousTrack => {
                if let Some(player) = &mut self.player {
                    match player.previous(self.playback_position) {
                        Ok(Some(crate::player::PreviousResult::Restarted)) => {
                            self.playback_position = Duration::ZERO;
                            // The MPD backend's `seek` only updates its cached
                            // position; the server needs the real command.
                            let seek_task = match self.mpd_client() {
                                Some(client) => self.dispatch_mpd(async move {
                                    client
                                        .command(mpd_client::commands::Seek(
                                            mpd_client::commands::SeekMode::Absolute(
                                                Duration::ZERO,
                                            ),
                                        ))
                                        .await
                                        .map_err(|e| format!("MPD seek: {e}"))
                                }),
                                None => Task::none(),
                            };
                            return Task::batch([seek_task, self.publish_mpris()]);
                        }
                        Ok(Some(crate::player::PreviousResult::Track(track))) => {
                            self.current_track = Some(track);
                            self.playback_position = Duration::ZERO;
                            return self.on_track_changed();
                        }
                        Ok(None) => {}
                        Err(e) => tracing::error!("Previous track failed: {e}"),
                    }
                }
            }

            Message::SeekPreview(fraction) => {
                // Visual-only: store the preview fraction so the slider and
                // time label reflect the drag position without touching the
                // audio backend. This avoids the rapid seek storm that
                // causes stuttering, snapback, and restarts.
                self.seeking_preview = Some(fraction);
            }

            Message::SeekCommit => {
                // Mouse released on the seek slider — perform the actual seek.
                if let Some(fraction) = self.seeking_preview.take()
                    && let Some(ref mut player) = self.player
                    && let Some(ref track) = self.current_track
                {
                    let target = Duration::from_secs_f32(fraction * track.duration.as_secs_f32());
                    match player.seek(target) {
                        Ok(()) => {
                            self.playback_position = target;
                            // Dispatch async Seek to MPD.
                            if let Some(client) = self.mpd_client() {
                                return self.dispatch_mpd(async move {
                                    client
                                        .command(mpd_client::commands::Seek(
                                            mpd_client::commands::SeekMode::Absolute(target),
                                        ))
                                        .await
                                        .map_err(|e| format!("MPD seek: {e}"))
                                });
                            }
                        }
                        Err(e) => tracing::warn!("Seek failed: {e}"),
                    }
                }
            }

            Message::SetVolume(vol) => {
                if let Some(player) = &mut self.player
                    && let Err(e) = player.set_volume(vol)
                {
                    tracing::error!("Set volume failed: {e}");
                }
                // Live-apply only: the actual level is read back from
                // `player.volume()` by the UI/MPRIS snapshot, so we don't
                // touch `config.volume` here. Persisting on every drag
                // frame would trigger a full `write_entry` transaction per
                // pixel of slider movement; `VolumeCommit` does the actual
                // save once the gesture settles.
                // Mirror the new level to MPRIS right away: the periodic
                // publish only runs while playing, so a volume change made
                // while paused/stopped would otherwise read stale on D-Bus.
                let mpris_task = self.publish_mpris();
                // Dispatch async SetVolume to MPD.
                if let Some(client) = self.mpd_client() {
                    let vol_u8 = (vol.clamp(0.0, 1.0) * 100.0) as u8;
                    return Task::batch([
                        mpris_task,
                        self.dispatch_mpd(async move {
                            client
                                .command(mpd_client::commands::SetVolume(vol_u8))
                                .await
                                .map_err(|e| format!("MPD set_volume: {e}"))
                        }),
                    ]);
                }
                return mpris_task;
            }

            // Persist the master volume. Emitted on slider release and on
            // discrete events (keyboard shortcuts, MPRIS `SetVolume`) so the
            // level survives a restart without writing config on every
            // drag frame.
            Message::VolumeCommit => {
                let v = self
                    .player
                    .as_ref()
                    .map_or(self.config.volume, |p| p.volume());
                if let Some(ctx) = &self.config_context
                    && let Err(e) = self.config.set_volume(ctx, v)
                {
                    tracing::error!("Failed to persist volume: {e}");
                }
            }

            Message::ToggleShuffle => {
                self.config.shuffle = !self.config.shuffle;
                if let Some(player) = &mut self.player {
                    player.set_shuffle(self.config.shuffle);
                }
                return self.publish_mpris();
            }

            Message::CycleRepeat => {
                self.config.repeat_mode = self.config.repeat_mode.next();
                if let Some(player) = &mut self.player {
                    player.set_repeat_mode(self.config.repeat_mode);
                }
                return self.publish_mpris();
            }

            Message::MpdStatusUpdate {
                position,
                duration,
                state,
                volume,
                song,
            } => {
                let mut track_changed = false;
                if let Some(player) = &mut self.player {
                    if let Some(mpd) = player.mpd_backend_mut() {
                        mpd.update_status(position, duration, state, volume);
                    }

                    // Real MPD-server playback state (not gated on which
                    // backend Aulos currently considers "active" — see the
                    // early-return below and `AppModel::mpd_playing`'s doc
                    // comment): the PipeWire capture thread's monitor
                    // fallback must only feed the visualizer while MPD
                    // itself is actually the one making sound.
                    #[cfg(feature = "visualizer")]
                    {
                        self.mpd_playing.store(
                            state == PlaybackState::Playing,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }

                    // The poll runs whenever an MPD backend exists, which
                    // includes while Aulos plays a local file, radio stream
                    // or podcast through the local backend (opening a file
                    // from a file manager does exactly that). Only let MPD
                    // drive the shared UI state when it is the backend
                    // actually in charge — otherwise its position and song
                    // would overwrite the locally-playing track. A stopped
                    // local backend means nothing else is playing, which is
                    // precisely the just-switched-to-MPD case that must
                    // still adopt.
                    if player.active_backend_type() != ActiveBackend::Mpd
                        && player.state() != PlaybackState::Stopped
                    {
                        return Task::none();
                    }

                    // Adopt a track MPD is already playing that Aulos didn't
                    // start itself — e.g. right after switching to this MPD
                    // backend while its server was already mid-playback, or
                    // MPD's song changed out from under Aulos (another
                    // client skipped tracks). `song` is only `Some` on the
                    // tick where MPD's song identity changed.
                    if let Some(track) = song
                        && state != PlaybackState::Stopped
                    {
                        player.adopt_mpd_track(track.clone(), duration);
                        self.current_track = Some(track);
                        track_changed = true;
                    }

                    // Update UI position (unless user is dragging seek slider).
                    if self.seeking_preview.is_none() {
                        self.playback_position = position;
                        if let Some(track) = &self.current_track {
                            self.playback_position =
                                clamp_display_position(self.playback_position, track.duration);
                        }
                    }

                    // Check if track ended (MPD reports Stopped after
                    // playback) — this is a NATURAL end, so repeat-one
                    // replays and repeat-none stops per `advance(auto =
                    // true)`'s contract. This is the one authoritative
                    // MPD end-of-track path (see the `PlaybackTick` guard
                    // below, which deliberately skips it for MPD to avoid
                    // double-advancing the queue).
                    if player.is_finished().unwrap_or(false) {
                        match player.advance_on_finish() {
                            Ok(Some(track)) => {
                                self.current_track = Some(track);
                                self.playback_position = Duration::ZERO;
                                track_changed = true;
                            }
                            Ok(None) => {}
                            Err(e) => tracing::error!("MPD advance on finish failed: {e}"),
                        }
                    }
                }

                let extra_task = if track_changed {
                    self.on_track_changed()
                } else {
                    Task::none()
                };

                // Scrobble handling for MPD tracks.
                if let Some(track) = self.current_track.clone() {
                    self.handle_scrobble(track);
                }
                return Task::batch([extra_task, self.publish_mpris()]);
            }

            Message::MpdCommandError(err) => {
                tracing::error!("Async MPD command failed: {err}");
                // The next status poll will self-correct the UI state.
            }

            Message::PlaybackTick => {
                // Local/Subsonic playback — read position from the active backend.
                let mut track_changed_task = Task::none();
                if let Some(player) = &mut self.player {
                    if self.seeking_preview.is_none() {
                        self.playback_position = player.position();

                        if let Some(track) = &self.current_track {
                            self.playback_position =
                                clamp_display_position(self.playback_position, track.duration);
                        }
                    }

                    // MPD's own end-of-track advance is handled by
                    // `MpdStatusUpdate` (which polls far more precisely
                    // and is the only path that also dispatches the next
                    // async MPD command) — this generic 500ms ticker fires
                    // regardless of backend, so it would otherwise race
                    // that poll and double-advance the queue.
                    if player.active_backend_type() != ActiveBackend::Mpd
                        && player.is_finished().unwrap_or(false)
                    {
                        match player.advance_on_finish() {
                            Ok(Some(track)) => {
                                self.current_track = Some(track);
                                self.playback_position = Duration::ZERO;
                                track_changed_task = self.on_track_changed();
                            }
                            Ok(None) => {}
                            Err(e) => tracing::error!("Advance on finish failed: {e}"),
                        }
                    }
                }

                let mut position_save_task = Task::none();
                if let Some(track) = self.current_track.clone() {
                    match &*track.provider_id {
                        "radio" => {
                            if let Some(player) = &self.player
                                && let Some(title) = player.icy_title()
                                && !title.is_empty()
                                && let Some(current) = &mut self.current_track
                                && current.title != title
                            {
                                current.title = title;
                            }
                        }
                        "podcast" => {
                            if let Some(episode_id) = self.current_podcast_episode_id {
                                let secs = self.playback_position.as_secs();
                                if secs != self.last_saved_podcast_position_secs
                                    && secs.is_multiple_of(5)
                                {
                                    self.last_saved_podcast_position_secs = secs;
                                    let position_ms = self.playback_position.as_millis() as i64;
                                    position_save_task =
                                        self.save_podcast_position(episode_id, position_ms, false);
                                }
                            }
                        }
                        _ => {}
                    }
                    self.handle_scrobble(track);
                }
                let mpris_task = self.publish_mpris();
                let history_task = self.track_play_history();
                let extras_task = self.playback_extras_tick();
                return Task::batch([
                    position_save_task,
                    track_changed_task,
                    mpris_task,
                    history_task,
                    extras_task,
                ]);
            }

            Message::Stop => {
                let mut mpd_task = Task::none();
                if let Some(player) = &mut self.player {
                    match player.stop() {
                        Ok(()) => {
                            self.playback_position = Duration::ZERO;
                            if let Some(client) = self.mpd_client() {
                                mpd_task = self.dispatch_mpd(async move {
                                    client
                                        .command(mpd_client::commands::Stop)
                                        .await
                                        .map_err(|e| format!("MPD stop: {e}"))
                                });
                            }
                        }
                        Err(e) => tracing::error!("Stop failed: {e}"),
                    }
                }
                return Task::batch([mpd_task, self.publish_mpris()]);
            }

            Message::QueueJump(idx) => {
                if let Some(player) = &mut self.player {
                    match player.jump_to(idx) {
                        Ok(Some(track)) => {
                            self.current_track = Some(track);
                            self.playback_position = Duration::ZERO;
                            return self.on_track_changed();
                        }
                        Ok(None) => {}
                        Err(e) => tracing::error!("Queue jump failed: {e}"),
                    }
                }
            }

            Message::QueueRemove(idx) => {
                if let Some(player) = &mut self.player {
                    match player.queue_remove(idx) {
                        Ok(crate::player::QueueEditOutcome::NowPlaying(track)) => {
                            self.current_track = Some(track);
                            self.playback_position = Duration::ZERO;
                            return self.on_track_changed();
                        }
                        Ok(crate::player::QueueEditOutcome::Stopped) => {
                            self.current_track = None;
                            self.playback_position = Duration::ZERO;
                            return self.publish_mpris();
                        }
                        Ok(crate::player::QueueEditOutcome::Unchanged) => {}
                        Err(e) => tracing::error!("Queue remove failed: {e}"),
                    }
                }
            }

            Message::QueueMove { from, to } => {
                if let Some(player) = &mut self.player {
                    player.queue_move(from, to);
                }
            }

            Message::QueueClear => {
                if let Some(player) = &mut self.player {
                    player.queue_clear_upcoming();
                }
            }

            Message::PlayNext(tracks) => {
                if tracks.is_empty() {
                    return Task::none();
                }
                let added = tracks.len();
                if let Some(player) = &mut self.player {
                    match player.queue_insert_next(tracks) {
                        Ok(Some(track)) => {
                            self.current_track = Some(track);
                            self.playback_position = Duration::ZERO;
                            return self.on_track_changed();
                        }
                        Ok(None) => {
                            return self.push_toast(widget::toaster::Toast::new(fl!(
                                "queued-tracks",
                                count = added
                            )));
                        }
                        Err(e) => tracing::error!("Play next failed: {e}"),
                    }
                }
            }

            Message::AddToQueue(tracks) => {
                if tracks.is_empty() {
                    return Task::none();
                }
                let added = tracks.len();
                if let Some(player) = &mut self.player {
                    match player.queue_append(tracks) {
                        Ok(Some(track)) => {
                            self.current_track = Some(track);
                            self.playback_position = Duration::ZERO;
                            return self.on_track_changed();
                        }
                        Ok(None) => {
                            return self.push_toast(widget::toaster::Toast::new(fl!(
                                "queued-tracks",
                                count = added
                            )));
                        }
                        Err(e) => tracing::error!("Add to queue failed: {e}"),
                    }
                }
            }

            Message::QueueFrom { source, next } => {
                let tracks = self.queue_source_tracks(source);
                return self.handle_message(if next {
                    Message::PlayNext(tracks)
                } else {
                    Message::AddToQueue(tracks)
                });
            }

            // -- Track selection --
            Message::PlayTrackIndex(index) => {
                return self.play_track_list(self.all_tracks.clone(), index);
            }

            Message::Folders(msg) => match msg {
                crate::views::folders::FolderMessage::Navigate(route) => {
                    return self.navigate(route);
                }
                crate::views::folders::FolderMessage::Open(dir) => self.folder_state.open(dir),
                crate::views::folders::FolderMessage::Up => self.folder_state.up(),
                crate::views::folders::FolderMessage::GoTo(index) => {
                    self.folder_state.go_to(index);
                }
                crate::views::folders::FolderMessage::PlayTrack(index) => {
                    // Play the folder's own tracks (not the whole library)
                    // starting at the clicked one.
                    let dir = self.folder_state.effective_current();
                    let direct = self.folder_state.tree().direct_tracks(&dir);
                    if let Some(position) = direct.iter().position(|&i| i == index) {
                        let tracks: Vec<_> = direct
                            .iter()
                            .filter_map(|&i| self.all_tracks.get(i).cloned())
                            .collect();
                        return self.play_track_list(tracks, position);
                    }
                    return self.play_track_list(self.all_tracks.clone(), index);
                }
                crate::views::folders::FolderMessage::PlayFolder => {
                    let tracks = self.current_folder_tracks();
                    if !tracks.is_empty() {
                        return self.play_track_list(tracks, 0);
                    }
                }
                crate::views::folders::FolderMessage::QueueFolder => {
                    let tracks = self.current_folder_tracks();
                    if tracks.is_empty() {
                        return Task::none();
                    }
                    let added = tracks.len();
                    let Some(player) = self.player.as_mut() else {
                        return Task::none();
                    };
                    match player.queue_append(tracks) {
                        Ok(Some(track)) => {
                            // The queue was empty — appending started
                            // playback immediately.
                            self.current_track = Some(track);
                            self.playback_position = Duration::ZERO;
                            return self.on_track_changed();
                        }
                        Ok(None) => {
                            return self.push_toast(widget::toaster::Toast::new(fl!(
                                "queued-tracks",
                                count = added
                            )));
                        }
                        Err(e) => {
                            tracing::error!("Queue folder failed: {e}");
                            return Task::none();
                        }
                    }
                }
                crate::views::folders::FolderMessage::ToggleFavorite(id) => {
                    return self.update(Message::ToggleFavorite(id));
                }
                crate::views::folders::FolderMessage::SetRating(id, r) => {
                    return self.update(Message::SetRating(id, r));
                }
            },

            Message::PlayAlbum(album_idx) => {
                if let Some(album) = self.all_albums.get(album_idx) {
                    return self.play_track_list(album.tracks.clone(), 0);
                }
            }

            Message::PlayAlbumTrack(album_idx, track_idx) => {
                if let Some(album) = self.all_albums.get(album_idx) {
                    return self.play_track_list(album.tracks.clone(), track_idx);
                }
            }

            Message::PlayArtistAlbum(artist_idx, album_idx) => {
                if let Some(artist) = self.all_artists.get(artist_idx)
                    && let Some(album) = artist.albums.get(album_idx)
                {
                    return self.play_track_list(album.tracks.clone(), 0);
                }
            }

            Message::PlayArtistTrack(artist_idx, album_idx, track_idx) => {
                if let Some(artist) = self.all_artists.get(artist_idx)
                    && let Some(album) = artist.albums.get(album_idx)
                {
                    return self.play_track_list(album.tracks.clone(), track_idx);
                }
            }

            // -- View navigation --
            Message::SelectAlbum(idx) => {
                self.selected_album = Some(idx);
            }

            Message::BackToAlbumGrid => {
                if let Some(task) = self.go_back() {
                    return task;
                }
                self.selected_album = None;
            }

            Message::SelectArtist(idx) => {
                self.selected_artist = Some(idx);
                self.artist_bio_expanded = false;
                if let Some(name) = self.all_artists.get(idx).map(|a| a.name.clone()) {
                    return self.load_artist_info_for_selected(&name);
                }
            }

            Message::BackToArtistList => {
                self.artist_bio_expanded = false;
                if let Some(task) = self.go_back() {
                    return task;
                }
                self.selected_artist = None;
            }

            Message::CredentialsChecked(updates) => {
                if super::init::apply_credential_updates(&mut self.config, &updates) {
                    self.save_config();
                }
            }

            Message::CoverBytesLoaded(key, bytes) => {
                self.cover_art_bytes.finish_load(key, bytes);
                // Whoever asked for the bytes (blur / detail hero) retries
                // now that they are cached; detail art is re-evaluated after
                // every update anyway.
                return self.maybe_update_blurred_cover();
            }

            Message::DetailArtReady(key, blurred, accent) => {
                self.apply_detail_art(key, blurred, accent);
            }

            Message::Navigate(route) => {
                return self.navigate(route);
            }

            Message::ToggleArtistBioExpanded => {
                self.artist_bio_expanded = !self.artist_bio_expanded;
            }

            Message::ArtistInfoLoaded(outcome) => {
                self.artist_info_pending.remove(&outcome.name);
                let mut resolved = false;
                if let Some(bio) = outcome.bio {
                    self.artist_bios.insert(outcome.name.clone(), bio);
                    resolved = true;
                }
                if let Some((w, h, pixels)) = outcome.image {
                    let handle = widget::image::Handle::from_rgba(w, h, pixels);
                    self.artist_photos.insert(outcome.name.clone(), handle);
                    resolved = true;
                }
                if !resolved && !outcome.name.is_empty() {
                    self.artist_info_negative.insert(outcome.name);
                }
            }

            Message::SortSongs(field) => {
                self.songs_sort = field;
                self.sort_tracks(field);
                self.refresh_search_filter();
            }

            Message::SongsScrolled(offset) => {
                self.songs_scroll_offset = offset;
            }

            Message::GridScrolled(area, offset) => {
                self.extras.grid_scroll.set(area, offset);
            }

            Message::ToggleFavoritesFilter => {
                self.favorites_filter = !self.favorites_filter;
                // Clear genre filter when toggling favorites
                if self.favorites_filter {
                    self.genre_filter = None;
                }
            }

            Message::ToggleFavorite(track_id) => {
                return self.toggle_favorite_async(track_id);
            }

            Message::FavoriteToggled {
                track_id,
                optimistic,
                result,
            } => match result {
                Ok(new_state) => {
                    if new_state != optimistic {
                        self.set_favorite_local(&track_id, new_state);
                    }
                    self.scrobble_favorite_changed(&track_id, new_state);
                }
                Err(e) => {
                    tracing::warn!("toggle_favorite failed: {e}");
                    self.set_favorite_local(&track_id, !optimistic);
                    return self.push_toast(widget::toaster::Toast::new(fl!(
                        "toast-provider-action-failed",
                        reason = e
                    )));
                }
            },

            Message::SettingsSearch(query) => self.settings_search = query,
            Message::Scrobble(msg) => return self.handle_scrobble_message(msg),

            Message::SetRating(track_id, rating) => {
                return self.set_rating_async(track_id, rating);
            }

            Message::RatingSet {
                track_id,
                previous,
                result,
            } => {
                if let Err(e) = result {
                    tracing::warn!("set_rating failed: {e}");
                    self.set_rating_local(&track_id, previous);
                    return self.push_toast(widget::toaster::Toast::new(fl!(
                        "toast-provider-action-failed",
                        reason = e
                    )));
                }
            }

            Message::AddToPlaylist(track_source_uri, playlist_id) => {
                return self.playlist_op_async(PlaylistOp::AddTrack, move |p| {
                    p.add_to_playlist(&playlist_id, &[track_source_uri])
                        .map_err(|e| e.to_string())
                });
            }

            Message::FilterByGenre(genre) => {
                if genre.is_empty() {
                    self.genre_filter = None;
                } else {
                    self.genre_filter = Some(genre);
                    self.favorites_filter = false;
                    // Navigate to Songs view.
                    // Collect entity IDs first to avoid borrow conflict.
                    let entities: Vec<_> = self.nav.iter().collect();
                    for entity in entities {
                        if self
                            .nav
                            .data::<Page>(entity)
                            .is_some_and(|p| *p == Page::Songs)
                        {
                            self.nav.activate(entity);
                            break;
                        }
                    }
                }
            }

            // -- Lyrics --
            Message::ShowLyrics => {
                // Try to load embedded lyrics, regardless of which
                // presentation (overlay vs sidebar) ends up showing them.
                if let Some(track) = &self.current_track {
                    self.lyrics_text = LyricsProvider::from_tags(&track.path)
                        .or_else(|| LyricsProvider::from_lrc_file(&track.path));
                }

                if self.expand_progress > 0.0 {
                    // Expanded now-playing is active: toggle the in-view
                    // overlay (drawn over the cover art / visualizer)
                    // instead of opening the generic sidebar, which would
                    // break the immersive full view.
                    self.lyrics_overlay_active = !self.lyrics_overlay_active;
                } else {
                    self.context_page = ContextPage::Lyrics;
                    self.core.window.show_context = true;
                }
            }

            Message::FetchLyricsOnline => {
                if let Some(ref track) = self.current_track {
                    self.lyrics_loading = true;
                    let artist = track.artist.clone();
                    let title = track.title.clone();
                    return cosmic::task::future(async move {
                        let result = LyricsProvider::fetch_online(&artist, &title).await;
                        cosmic::Action::App(Message::LyricsLoaded(result))
                    });
                }
            }

            Message::LyricsLoaded(text) => {
                self.lyrics_loading = false;
                self.lyrics_text = text;
            }

            // -- Equalizer --
            Message::EqSetBand(index, value) => {
                let clamped = value.clamp(-12.0, 12.0);
                if index < self.config.equalizer_bands.len() {
                    self.config.equalizer_bands[index] = clamped;
                }
                // Update the live DSP filter.
                if let Some(ref player) = self.player {
                    player.eq_controller().set_band(index, clamped);
                }
                self.eq_preset = None;
                self.eq_dirty = true;
            }

            Message::EqSetPreset(preset) => {
                let gains = preset.gains();
                self.config.equalizer_bands = gains.to_vec();
                // Update all 10 bands in the live DSP.
                if let Some(ref player) = self.player {
                    player.eq_controller().set_all(&gains);
                }
                self.eq_preset = Some(preset);
            }

            Message::EqToggle(enabled) => {
                self.config.equalizer_enabled = enabled;
                // Enable/bypass the live DSP.
                if let Some(ref player) = self.player {
                    player.eq_controller().set_enabled(enabled);
                }
            }

            Message::EqSetPreamp(value) => {
                let clamped = value.clamp(-20.0, 10.0);
                self.config.equalizer_preamp = clamped;
                // TODO: Apply preamp to audio backend if supported
                tracing::debug!("Preamp set to {:+.1} dB", clamped);
                self.eq_preset = None;
                self.eq_dirty = true;
            }

            Message::EqSelectPreset(name) => {
                if let Some(preset) = self.all_presets.iter().find(|p| p.name == name) {
                    self.config.equalizer_bands = preset.bands.to_vec();
                    self.config.equalizer_preamp = preset.preamp;
                    if let Some(ref player) = self.player {
                        player.eq_controller().set_all(&preset.bands);
                    }
                    self.active_preset_name = Some(name.clone());
                    self.config.active_eq_preset_name = name;
                    self.eq_dirty = false;
                    self.eq_preset = None; // clear legacy preset tracking
                }
            }

            Message::EqSavePreset => {
                if let Some(ref name) = self.active_preset_name {
                    if self.preset_manager.is_builtin_name(name) {
                        tracing::warn!("Cannot overwrite built-in preset '{}'", name);
                    } else {
                        let preset = crate::player::equalizer::EqPresetData {
                            name: name.clone(),
                            bands: {
                                let mut b = [0.0_f32; 10];
                                for (i, v) in
                                    self.config.equalizer_bands.iter().enumerate().take(10)
                                {
                                    b[i] = *v;
                                }
                                b
                            },
                            preamp: self.config.equalizer_preamp,
                            source: crate::player::equalizer::PresetSource::Custom,
                        };
                        if let Err(e) = self.preset_manager.save_preset(&preset) {
                            tracing::error!("Failed to save preset: {}", e);
                        } else {
                            self.all_presets = self.preset_manager.load_all();
                            self.eq_dirty = false;
                        }
                    }
                }
            }

            Message::EqSavePresetAs(name) => {
                if name.trim().is_empty() {
                    tracing::warn!("Cannot save preset with empty name");
                } else if self.preset_manager.is_builtin_name(&name) {
                    tracing::warn!("Cannot use the name of a built-in preset: '{}'", name);
                } else {
                    let preset = crate::player::equalizer::EqPresetData {
                        name: name.clone(),
                        bands: {
                            let mut b = [0.0_f32; 10];
                            for (i, v) in self.config.equalizer_bands.iter().enumerate().take(10) {
                                b[i] = *v;
                            }
                            b
                        },
                        preamp: self.config.equalizer_preamp,
                        source: crate::player::equalizer::PresetSource::Custom,
                    };
                    if let Err(e) = self.preset_manager.save_preset(&preset) {
                        tracing::error!("Failed to save preset: {}", e);
                    } else {
                        self.all_presets = self.preset_manager.load_all();
                        self.active_preset_name = Some(name.clone());
                        self.config.active_eq_preset_name = name;
                        self.eq_dirty = false;
                    }
                }
            }

            Message::EqDeletePreset => {
                if let Some(ref name) = self.active_preset_name {
                    if self.preset_manager.is_builtin_name(name) {
                        tracing::warn!("Cannot delete built-in preset '{}'", name);
                    } else if let Err(e) = self.preset_manager.delete_preset(name) {
                        tracing::error!("Failed to delete preset: {}", e);
                    } else {
                        self.all_presets = self.preset_manager.load_all();
                        self.active_preset_name = None;
                        self.config.active_eq_preset_name = String::new();
                        self.eq_dirty = false;
                    }
                }
            }

            Message::EqSaveAsNameChanged(name) => {
                self.save_as_name = name;
            }

            Message::EqResetPreset => {
                self.config.equalizer_bands = vec![0.0; 10];
                self.config.equalizer_preamp = 0.0;
                if let Some(ref player) = self.player {
                    player.eq_controller().set_all(&[0.0; 10]);
                }
                self.active_preset_name = None;
                self.config.active_eq_preset_name = String::new();
                self.eq_preset = None;
                self.eq_dirty = false;
            }

            // -- AutoEQ --
            Message::AutoEQSearchChanged(query) => {
                self.autoeq_search = query;
            }

            Message::FetchAutoEQIndex => {
                if self.autoeq_loading {
                    return Task::none(); // already fetching
                }
                self.autoeq_loading = true;

                let cache_dir = dirs::cache_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("aulos")
                    .join("autoeq");
                let timeout = std::time::Duration::from_secs(30);

                return cosmic::task::future(async move {
                    let result = match crate::autoeq::AutoEQManager::new(cache_dir, timeout) {
                        Ok(mut manager) => manager.fetch_index().await.map_err(|e| e.to_string()),
                        Err(e) => Err(e.to_string()),
                    };
                    cosmic::Action::App(Message::AutoEQIndexLoaded(result))
                });
            }

            Message::AutoEQIndexLoaded(result) => {
                self.autoeq_loading = false;
                match result {
                    Ok(profiles) => {
                        tracing::info!("Loaded {} AutoEQ profiles", profiles.len());
                        self.autoeq_profiles = profiles;
                    }
                    Err(e) => {
                        tracing::error!("Failed to fetch AutoEQ index: {}", e);
                    }
                }
            }

            Message::EqSelectAutoEQ(path) => {
                // Find the profile metadata to get the name
                let name = self
                    .autoeq_profiles
                    .iter()
                    .find(|p| p.path == path)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();

                tracing::info!("Fetching AutoEQ profile: {} ({})", name, path);

                let cache_dir = dirs::cache_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("aulos")
                    .join("autoeq");
                let timeout = std::time::Duration::from_secs(30);

                return cosmic::task::future(async move {
                    let result = match crate::autoeq::AutoEQManager::new(cache_dir, timeout) {
                        Ok(mut manager) => manager
                            .fetch_profile(&path)
                            .await
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e.to_string()),
                    };
                    cosmic::Action::App(Message::AutoEQProfileLoaded(result))
                });
            }

            Message::AutoEQProfileLoaded(result) => {
                match result {
                    Ok(profile) => {
                        // Apply bands + preamp
                        self.config.equalizer_bands = profile.bands.to_vec();
                        self.config.equalizer_preamp = profile.preamp;
                        if let Some(ref player) = self.player {
                            player.eq_controller().set_all(&profile.bands);
                        }
                        // Clear preset selection (user can "Save As" to keep)
                        self.active_preset_name = None;
                        self.config.active_eq_preset_name = String::new();
                        self.eq_dirty = false;
                        self.eq_preset = None;

                        tracing::info!(
                            "Applied AutoEQ profile: {} (preamp: {:+.1} dB)",
                            profile.name,
                            profile.preamp
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to load AutoEQ profile: {}", e);
                    }
                }
            }

            Message::SetGridScale(scale) => {
                crate::views::common::set_grid_scale(scale);
                self.config.grid_scale = crate::views::common::grid_scale();
                self.save_config();
            }

            Message::ToggleAlbumsViewMode => {
                self.config.albums_view_mode = self.config.albums_view_mode.toggled();
                self.save_config();
            }
            Message::ToggleArtistsViewMode => {
                self.config.artists_view_mode = self.config.artists_view_mode.toggled();
                self.save_config();
            }
            Message::ToggleGenresViewMode => {
                self.config.genres_view_mode = self.config.genres_view_mode.toggled();
                self.save_config();
            }

            // -- Settings --
            Message::AddMusicDir => {
                // Launch the XDG Desktop Portal directory picker asynchronously.
                return cosmic::task::future(async {
                    let result = async {
                        use ashpd::desktop::file_chooser::SelectedFiles;

                        let selected = SelectedFiles::open_file()
                            .title("Select Music Directory")
                            .directory(true)
                            .modal(true)
                            .send()
                            .await
                            .map_err(|e| format!("Portal request failed: {e}"))?
                            .response()
                            .map_err(|e| format!("Portal response failed: {e}"))?;

                        let uris = selected.uris();
                        if let Some(uri) = uris.first() {
                            let uri_str = uri.as_str();
                            uri_str
                                .strip_prefix("file://")
                                .ok_or_else(|| format!("Not a local file URI: {uri_str}"))
                                .and_then(|encoded_path| {
                                    urlencoding::decode(encoded_path)
                                        .map(|decoded| PathBuf::from(decoded.as_ref()))
                                        .map_err(|e| format!("Could not decode URI path: {e}"))
                                })
                        } else {
                            Err("No directory selected".to_string())
                        }
                    }
                    .await;
                    cosmic::Action::App(Message::DirPickerResult(result))
                });
            }

            Message::DirPickerResult(result) => {
                match result {
                    Ok(path) => {
                        // Deduplicate: check if the path is already in music_dirs.
                        if self.config.music_dirs.contains(&path) {
                            tracing::info!("Directory already in music_dirs: {}", path.display());
                        } else {
                            tracing::info!("Adding music directory: {}", path.display());
                            self.config.music_dirs.push(path);
                            self.save_config();
                            // Re-register the Local provider with updated scan dirs.
                            self.reinit_local_provider();
                            // Trigger a library rescan.
                            return cosmic::task::message(cosmic::Action::App(
                                Message::ScanLibrary,
                            ));
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Directory picker failed: {e}");
                    }
                }
            }

            Message::RemoveMusicDir(index) => {
                if index < self.config.music_dirs.len() {
                    let removed = self.config.music_dirs.remove(index);
                    tracing::info!("Removed music directory: {}", removed.display());
                    self.save_config();
                    // Re-register the Local provider with updated scan dirs.
                    self.reinit_local_provider();
                    // Trigger a library rescan.
                    return cosmic::task::message(cosmic::Action::App(Message::ScanLibrary));
                }
            }

            Message::UpdateConfig(config) if self.is_own_config_echo(&config) => {}
            Message::UpdateConfig(config) => {
                let artist_split_changed = config.split_artist_tags
                    != self.config.split_artist_tags
                    || config.artist_tag_delimiters != self.config.artist_tag_delimiters;
                if config.music_dirs != self.config.music_dirs {
                    self.music_dirs_present = config.music_dirs.iter().any(|d| d.is_dir());
                }
                self.config = config;
                if artist_split_changed {
                    self.artist_tag_delimiters_input =
                        self.config.artist_tag_delimiters.join(" | ");
                    self.rebuild_all_artists();
                    self.refresh_search_filter();
                }
            }

            Message::SetSplitArtistTags(enabled) => {
                self.config.split_artist_tags = enabled;
                self.save_config();
                self.rebuild_all_artists();
                self.refresh_search_filter();
            }

            Message::SetExperimentalConverter(enabled) => {
                self.config.experimental_converter = enabled;
                self.save_config();
                self.set_convert_nav_entry(enabled);
            }

            Message::SetFetchArtistInfo(enabled) => {
                self.config.fetch_artist_info = enabled;
                self.save_config();
                if enabled && self.nav.active_data::<Page>() == Some(&Page::Artists) {
                    return self.load_artist_info_for_visible();
                }
            }

            Message::ArtistTagDelimitersInputChanged(text) => {
                self.artist_tag_delimiters_input = text;
            }

            Message::SubmitArtistTagDelimiters(text) => {
                self.artist_tag_delimiters_input = text.clone();
                self.config.artist_tag_delimiters = parse_delimiters_input(&text);
                self.save_config();
                self.rebuild_all_artists();
                self.refresh_search_filter();
            }

            Message::ResetArtistTagDelimiters => {
                self.config.artist_tag_delimiters = crate::library::artist_tags::DEFAULT_DELIMITERS
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                self.artist_tag_delimiters_input = self.config.artist_tag_delimiters.join(" | ");
                self.save_config();
                self.rebuild_all_artists();
                self.refresh_search_filter();
            }

            // -- Provider switching --
            Message::SwitchProvider(index) => {
                if let Some((id, _name)) = self.provider_list.get(index)
                    && self.registry.set_active(id)
                {
                    let provider_id = id.clone();
                    self.active_provider_index = Some(index);
                    self.provider_manually_switched = true;
                    self.config.active_provider = Some(provider_id.clone());
                    self.save_config();
                    tracing::info!("Switched to provider: {provider_id}");

                    // Recreate player with the correct backend for the new provider.
                    self.recreate_player();

                    // Clear current library data and reload from new provider
                    self.all_tracks.clear();
                    self.all_albums.clear();
                    self.all_artists.clear();
                    self.cover_images.clear();
                    self.cover_fingerprints.clear();
                    self.cover_art_bytes.clear();
                    self.artist_photos.clear();
                    self.artist_bios.clear();
                    self.artist_info_pending.clear();
                    self.artist_info_negative.clear();
                    return self.reload_library();
                }
            }

            // -- MPD server configuration --
            Message::MpdAddServer => {
                let idx = self.mpd_edit_states.len();
                self.mpd_edit_states
                    .push(providers::MpdEditState::new_default(idx));
                self.mpd_connection_status.push(None);
            }

            Message::MpdEditName(i, v) => {
                if let Some(state) = self.mpd_edit_states.get_mut(i) {
                    state.name = v;
                }
            }

            Message::MpdEditHost(i, v) => {
                if let Some(state) = self.mpd_edit_states.get_mut(i) {
                    state.host = v;
                }
            }

            Message::MpdEditPort(i, v) => {
                if let Some(state) = self.mpd_edit_states.get_mut(i) {
                    state.port = v;
                }
            }

            Message::MpdEditPassword(i, v) => {
                if let Some(state) = self.mpd_edit_states.get_mut(i) {
                    state.password = v;
                }
            }

            Message::MpdSaveServer(i) => {
                if let Some(state) = self.mpd_edit_states.get(i) {
                    let mut entry = state.to_config();
                    // Store password to keyring immediately on save so it is
                    // never left as plaintext in the config file.
                    if let Some(pw) = entry.password.as_deref().filter(|p| !p.is_empty()) {
                        match crate::credentials::store_password(&entry.id, pw) {
                            Ok(()) => {
                                entry.password_in_keyring = true;
                                entry.password = None;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to store MPD password for '{}' in keyring, \
                                     keeping plaintext in config: {e}",
                                    entry.id
                                );
                            }
                        }
                    }
                    // Update or add in the config
                    if i < self.config.mpd_servers.len() {
                        self.config.mpd_servers[i] = entry;
                    } else {
                        self.config.mpd_servers.push(entry);
                    }
                    tracing::info!("MPD server config saved: {}", state.name);
                    // Persist config via cosmic-config
                    self.save_config();
                    // Re-initialize providers
                    return self.reinit_mpd_providers();
                }
            }

            Message::MpdRemoveServer(i) => {
                if i < self.mpd_edit_states.len() {
                    self.mpd_edit_states.remove(i);
                    self.mpd_connection_status.remove(i);
                    if i < self.config.mpd_servers.len() {
                        self.config.mpd_servers.remove(i);
                    }
                    tracing::info!("MPD server removed at index {i}");
                    self.save_config();
                    return self.reinit_mpd_providers();
                }
            }

            Message::MpdTestConnection(i) => {
                if let Some(state) = self.mpd_edit_states.get(i) {
                    let host = state.host.clone();
                    let port: u16 = state.port.parse().unwrap_or(6600);
                    let password = if state.password.is_empty() {
                        None
                    } else {
                        Some(state.password.clone())
                    };

                    return cosmic::task::future(async move {
                        let addr = format!("{host}:{port}");
                        let result = async {
                            let stream = tokio::net::TcpStream::connect(&addr)
                                .await
                                .map_err(|e| format!("TCP: {e}"))?;
                            mpd_client::Client::connect_with_password_opt(
                                stream,
                                password.as_deref(),
                            )
                            .await
                            .map_err(|e| format!("MPD: {e}"))?;
                            Ok(())
                        }
                        .await;
                        cosmic::Action::App(Message::MpdTestResult(i, result))
                    });
                }
            }

            Message::MpdTestResult(i, result) => {
                let status = match result {
                    Ok(()) => fl!("connected"),
                    Err(e) => format!("{}: {e}", fl!("connection-failed")),
                };
                if let Some(s) = self.mpd_connection_status.get_mut(i) {
                    *s = Some(status);
                }
            }

            // -- MPD provider events --
            Message::MpdConnected(provider_id) => {
                tracing::info!("MPD provider '{provider_id}' is now connected");

                // Update connection status for the matching provider card.
                if let Some(idx) = self
                    .mpd_edit_states
                    .iter()
                    .position(|s| s.id == provider_id)
                    && let Some(s) = self.mpd_connection_status.get_mut(idx)
                {
                    *s = Some(fl!("connected"));
                }

                // Restore the persisted active provider once it actually
                // connects, unless the user has since switched providers
                // manually — a manual choice always wins over the saved
                // one, even across a later reconnect of this server.
                if !self.provider_manually_switched
                    && self.config.startup_provider_choice() == Some(provider_id.as_str())
                    && self.registry.active_id() != provider_id
                    && self.registry.set_active(&provider_id)
                {
                    self.rebuild_provider_list();
                }

                // If this is the active provider, attach an MpdBackend and
                // reload the library. Recreating the player throws away the
                // queue and current track, so never do it mid-playback: a
                // file opened from a file manager starts playing before a
                // configured MPD server finishes connecting, and would
                // otherwise be silently killed a moment later. The backend
                // gets attached on the next provider switch instead.
                if self.registry.active_id() == provider_id {
                    if self
                        .player
                        .as_ref()
                        .is_none_or(|p| p.state() == PlaybackState::Stopped)
                    {
                        self.recreate_player();
                    }
                    return self.reload_library();
                }
            }

            Message::MpdConnectionFailed(provider_id, error) => {
                tracing::error!("MPD provider '{provider_id}' failed to connect: {error}");

                // Update connection status for the matching provider card, and
                // surface a toast only on the transition into this failure
                // state — the idle/command reconnect loop retries every 5s
                // and would otherwise spam a toast on every retry.
                let new_status = format!("{}: {error}", fl!("connection-failed"));
                if let Some(idx) = self
                    .mpd_edit_states
                    .iter()
                    .position(|s| s.id == provider_id)
                {
                    let already_failed = self
                        .mpd_connection_status
                        .get(idx)
                        .is_some_and(|s| s.as_deref() == Some(new_status.as_str()));

                    if let Some(s) = self.mpd_connection_status.get_mut(idx) {
                        *s = Some(new_status);
                    }

                    if !already_failed {
                        let provider_name = self
                            .mpd_edit_states
                            .get(idx)
                            .map(|s| s.name.clone())
                            .filter(|n| !n.is_empty())
                            .unwrap_or(provider_id);
                        return self.push_toast(widget::toaster::Toast::new(fl!(
                            "toast-provider-connect-failed",
                            provider = provider_name,
                            reason = error
                        )));
                    }
                }
            }

            Message::MpdIdleEvent(provider_id, subsystem) => {
                if self.registry.active_id() != provider_id {
                    return Task::none();
                }
                match crate::app::subscriptions::idle_action(subsystem) {
                    crate::app::subscriptions::IdleAction::ReloadLibrary => {
                        tracing::debug!(
                            "MPD idle event from provider '{provider_id}': reloading library"
                        );
                        return self.reload_library();
                    }
                    crate::app::subscriptions::IdleAction::ReloadPlaylists => {
                        tracing::debug!(
                            "MPD idle event from provider '{provider_id}': reloading playlists"
                        );
                        return self.load_playlists();
                    }
                    crate::app::subscriptions::IdleAction::None => {}
                }
            }

            // -- Subsonic server configuration --
            Message::SubsonicAddServer => {
                let idx = self.subsonic_edit_states.len();
                self.subsonic_edit_states
                    .push(providers::SubsonicEditState::new_default(idx));
                self.subsonic_connection_status.push(None);
            }

            Message::SubsonicEditName(i, v) => {
                if let Some(state) = self.subsonic_edit_states.get_mut(i) {
                    state.name = v;
                }
            }

            Message::SubsonicEditUrl(i, v) => {
                if let Some(state) = self.subsonic_edit_states.get_mut(i) {
                    state.url = v;
                }
            }

            Message::SubsonicEditUsername(i, v) => {
                if let Some(state) = self.subsonic_edit_states.get_mut(i) {
                    state.username = v;
                }
            }

            Message::SubsonicEditPassword(i, v) => {
                if let Some(state) = self.subsonic_edit_states.get_mut(i) {
                    state.password = v;
                }
            }

            Message::SubsonicToggleCerts(i, v) => {
                if let Some(state) = self.subsonic_edit_states.get_mut(i) {
                    state.accept_invalid_certs = v;
                }
            }

            Message::SubsonicSaveServer(i) => {
                if let Some(state) = self.subsonic_edit_states.get(i) {
                    let mut entry = state.to_config();
                    // Store password to keyring immediately on save so it is
                    // never left as plaintext in the config file.
                    if let Some(pw) = entry.password.as_deref().filter(|p| !p.is_empty()) {
                        match crate::credentials::store_password(&entry.id, pw) {
                            Ok(()) => {
                                entry.password_in_keyring = true;
                                entry.password = None;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to store Subsonic password for '{}' in keyring, \
                                     keeping plaintext in config: {e}",
                                    entry.id
                                );
                            }
                        }
                    }
                    if i < self.config.subsonic_servers.len() {
                        self.config.subsonic_servers[i] = entry;
                    } else {
                        self.config.subsonic_servers.push(entry);
                    }
                    tracing::info!("Subsonic server config saved: {}", state.name);
                    self.save_config();
                    return self.reinit_subsonic_providers();
                }
            }

            Message::SubsonicRemoveServer(i) => {
                if i < self.subsonic_edit_states.len() {
                    self.subsonic_edit_states.remove(i);
                    self.subsonic_connection_status.remove(i);
                    if i < self.config.subsonic_servers.len() {
                        self.config.subsonic_servers.remove(i);
                    }
                    tracing::info!("Subsonic server removed at index {i}");
                    self.save_config();
                    return self.reinit_subsonic_providers();
                }
            }

            Message::SubsonicTestConnection(i) => {
                if let Some(state) = self.subsonic_edit_states.get(i) {
                    let url = state.url.clone();
                    let username = state.username.clone();
                    let password = state.password.clone();
                    let accept_invalid_certs = state.accept_invalid_certs;

                    return cosmic::task::future(async move {
                        let result = async {
                            let auth = opensubsonic::Auth::token(&username, &password);
                            let mut client = opensubsonic::Client::new(&url, auth)
                                .map_err(|e| format!("Client: {e}"))?;
                            if accept_invalid_certs {
                                client = client
                                    .with_danger_accept_invalid_certs()
                                    .map_err(|e| format!("TLS: {e}"))?;
                            }
                            client.ping().await.map_err(|e| format!("Ping: {e}"))?;
                            Ok(())
                        }
                        .await;
                        cosmic::Action::App(Message::SubsonicTestResult(i, result))
                    });
                }
            }

            Message::SubsonicTestResult(i, result) => {
                let status = match result {
                    Ok(()) => fl!("connected"),
                    Err(e) => format!("{}: {e}", fl!("connection-failed")),
                };
                if let Some(s) = self.subsonic_connection_status.get_mut(i) {
                    *s = Some(status);
                }
            }

            // Task 109: Subsonic transcoding bitrate/format changes
            Message::SubsonicTranscodingBitrate(i, bitrate) => {
                if let Some(state) = self.subsonic_edit_states.get_mut(i) {
                    state.transcoding_max_bitrate = bitrate;
                }
            }
            Message::SubsonicTranscodingFormat(i, format) => {
                if let Some(state) = self.subsonic_edit_states.get_mut(i) {
                    state.transcoding_format = format;
                }
            }

            // Task 113: Wire crossfade config change to player and MPD
            Message::SetCrossfade(secs) => {
                self.config.crossfade_duration_secs = secs;

                // Apply to local backend
                if let Some(player) = &mut self.player {
                    player.set_crossfade(secs);
                }

                // Forward to MPD if active
                if let Some(mpd) = self.active_mpd_provider() {
                    let seconds = secs as u64;
                    return self.dispatch_mpd(async move {
                        mpd.send_crossfade(seconds)
                            .map_err(|e| format!("crossfade: {e}"))
                    });
                }
            }

            // Task 114: Wire replay gain mode change to player and MPD
            Message::SetReplayGainMode(mode) => {
                self.config.replay_gain_mode = mode;

                // Apply to local backend
                if let Some(player) = &mut self.player {
                    player.set_replay_gain_mode(mode);
                }

                // Forward to MPD if active — convert config type to mpd_client type
                if let Some(mpd) = self.active_mpd_provider() {
                    use mpd_client::commands::ReplayGainMode as MpdReplayGainMode;
                    let mpd_mode = match mode {
                        ReplayGainMode::Off => MpdReplayGainMode::Off,
                        ReplayGainMode::Track => MpdReplayGainMode::Track,
                        ReplayGainMode::Album => MpdReplayGainMode::Album,
                        ReplayGainMode::Auto => MpdReplayGainMode::Auto,
                    };
                    return self.dispatch_mpd(async move {
                        mpd.send_replay_gain_mode(mpd_mode)
                            .map_err(|e| format!("replay_gain_mode: {e}"))
                    });
                }
            }

            Message::ExpandNowPlaying => {
                // Mount the sheet immediately; it animates itself open.
                self.expand_progress = 1.0;
                return self.begin_expand_transition(1.0);
            }

            Message::CollapseNowPlaying => {
                // If the preset browser is open, Escape (or the collapse
                // button) closes it first instead of collapsing the whole
                // view. Handled here rather than branching in the Escape
                // subscription itself, since `listen_with` only accepts a
                // non-capturing `fn` pointer — it can't read
                // `viz_browser_open`.
                #[cfg(feature = "visualizer")]
                if self.viz_browser_open {
                    self.viz_browser_open = false;
                    return Task::none();
                }
                self.lyrics_overlay_active = false;
                let transition = self.begin_expand_transition(0.0);
                // Leaving the expanded view must also leave fullscreen, else the
                // header bar / nav sidebar would stay hidden with no visualizer.
                #[cfg(feature = "visualizer")]
                return transition.chain(self.exit_viz_fullscreen());
                #[cfg(not(feature = "visualizer"))]
                return transition;
            }

            Message::ExpandAnimTick => {
                // Fired once per transition, after `sheet::DURATION`. Ignore
                // ticks from a transition that has since been superseded.
                if let (Some(target), Some(start)) = (self.expand_target, self.expand_anim_start)
                    && start.elapsed() >= crate::views::now_playing::sheet::DURATION
                {
                    self.expand_progress = target;
                    self.expand_target = None;
                    self.expand_anim_start = None;
                }
            }

            Message::BlurReady(key, handle, accent, large_handle) => {
                // The job that produced this result has finished either
                // way (stale or not) -- clear the in-flight guard so a
                // future call for the same key can spawn again if needed.
                if self.blur_pending_key.as_deref() == Some(key.as_str()) {
                    self.blur_pending_key = None;
                }

                // Guard against stale results: only apply if this blur is still
                // for the current track's album. A slow computation may finish
                // after the user has already moved to a different track.
                let current_key = self.current_track.as_ref().map(|t| {
                    let artist = if t.album_artist.is_empty() {
                        &t.artist
                    } else {
                        &t.album_artist
                    };
                    crate::library::CoverArt::album_key(artist, &t.album)
                });
                if current_key.as_ref() == Some(&key) {
                    // Blur only applies on success — a failed decode leaves
                    // the previous blur/black base showing and
                    // `blurred_cover_key` unset so the next trigger retries.
                    // The accent always reflects this key's result
                    // (including `None`): it has no equivalent "keep the
                    // old value and retry" behaviour to preserve.
                    if let Some(handle) = handle {
                        self.blurred_cover = Some(handle);
                        self.blurred_cover_key = Some(key.clone());
                    }
                    // Lift/darken deep or pale cover colours so the accent
                    // (seek bar, play button, current lyric) stays visible
                    // against the window background.
                    let dark = cosmic::theme::active().cosmic().is_dark;
                    self.accent = accent.map(|a| a.legible(dark));
                    // Same "keep the old value on failure" behavior as
                    // the blur handle above.
                    if let Some(large) = large_handle {
                        self.current_cover_large = Some((key, large));
                    }
                }
                // If stale, discard silently — the correct blur is either already
                // cached or will be requested by the next maybe_update_blurred_cover call.
            }

            // -- Visualizer messages (cfg-gated) --
            #[cfg(feature = "visualizer")]
            Message::ToggleVisualizer => {
                self.visualizer_active = !self.visualizer_active;
                // When turning the visualizer off, the blurred cover background
                // takes over. If it was never computed (e.g. track changed while
                // viz was active and bytes weren't cached yet), trigger it now.
                if !self.visualizer_active {
                    let fs_task = self.exit_viz_fullscreen();
                    self.viz_browser_open = false;
                    // Force retry even if key matches — blurred_cover may be None.
                    if self.blurred_cover.is_none() {
                        self.blurred_cover_key = None;
                    }
                    return Task::batch([fs_task, self.maybe_update_blurred_cover()]);
                }
            }

            #[cfg(feature = "visualizer")]
            Message::NextVisualizerPreset => {
                let _ = self
                    .viz_cmd_tx
                    .send(crate::views::now_playing::visualizer::VizCommand::NextPreset);
            }

            #[cfg(feature = "visualizer")]
            Message::VisualizerFrameReady => {
                // The shared VizFrameBuffer already has the new pixels.
                // This message just triggers a view redraw so the Shader
                // widget picks them up in its next prepare() call.

                // Resync the UI-local current-preset path from whatever
                // the render thread most recently put on screen (see
                // `viz_current_preset_shared`'s doc comment).
                if let Ok(current) = self.viz_current_preset_shared.lock()
                    && *current != self.viz_current_preset
                {
                    self.viz_current_preset = current.clone();
                }

                // Decay metadata overlay (~4 seconds at 30 fps = 120 frames).
                // No inner cfg needed — this arm is only reachable with the visualizer feature.
                if self.viz_metadata_opacity > 0.0 {
                    self.viz_metadata_opacity =
                        (self.viz_metadata_opacity - (1.0 / 120.0)).max(0.0);
                }

                // Tick the HUD auto-hide idle counter (only while
                // fullscreen, and not while the preset browser forces the
                // HUD visible — no point counting otherwise, and it avoids
                // a stale huge count instantly hiding the HUD the moment
                // the browser closes).
                if self.viz_fullscreen && !self.viz_hud_pointer_over && !self.viz_browser_open {
                    self.viz_hud_idle_frames = self.viz_hud_idle_frames.saturating_add(1);
                }
            }

            #[cfg(feature = "visualizer")]
            Message::VizHudActivity => {
                self.viz_hud_idle_frames = 0;
            }

            #[cfg(feature = "visualizer")]
            Message::VizHudPointerEnter => {
                self.viz_hud_pointer_over = true;
                self.viz_hud_idle_frames = 0;
            }

            #[cfg(feature = "visualizer")]
            Message::VizHudPointerExit => {
                self.viz_hud_pointer_over = false;
                self.viz_hud_idle_frames = 0;
            }

            #[cfg(feature = "visualizer")]
            Message::ToggleVisualizerFullscreen => {
                // COSMIC uses client-side decorations, so the header bar *is* the
                // titlebar; hiding it (plus the nav sidebar) is necessary but not
                // sufficient for a real fullscreen — the compositor's panels/dock
                // stay visible unless the window itself enters fullscreen mode, so
                // this also drives the iced window `set_mode` command. (The old
                // `toggle_decorations` call targeted server-side decorations,
                // which COSMIC never draws, so it was a silent no-op.)
                self.viz_fullscreen = !self.viz_fullscreen;
                let mode = if self.viz_fullscreen {
                    self.viz_hud_idle_frames = 0;
                    self.viz_prev_nav_active = self.core.nav_bar_active();
                    self.core.window.show_headerbar = false;
                    self.core.nav_bar_set_toggled(false);
                    cosmic::iced::window::Mode::Fullscreen
                } else {
                    self.core.window.show_headerbar = true;
                    self.core.nav_bar_set_toggled(self.viz_prev_nav_active);
                    cosmic::iced::window::Mode::Windowed
                };
                if let Some(id) = self.core.main_window_id() {
                    return cosmic::iced::window::set_mode(id, mode);
                }
            }

            #[cfg(feature = "visualizer")]
            Message::TogglePresetBrowser => {
                self.viz_browser_open = !self.viz_browser_open;
                if self.viz_browser_open {
                    self.viz_hud_idle_frames = 0;
                    // Rescan on every open: cheap, and picks up presets
                    // installed since the last one. The list on screen stays
                    // as is until the result lands.
                    let scan = cosmic::task::future(async move {
                        let dirs = crate::views::now_playing::visualizer::preset_search_dirs(
                            dirs::data_dir().map(|d| d.join("projectm").join("presets")),
                        );
                        let entries = tokio::task::spawn_blocking(move || {
                            crate::views::now_playing::visualizer::scan_presets(&dirs)
                        })
                        .await
                        .unwrap_or_default();
                        cosmic::Action::App(Message::VizPresetsScanned(entries))
                    });
                    return Task::batch([scan, self.scroll_preset_list_to_current()]);
                }
            }

            #[cfg(feature = "visualizer")]
            Message::PresetSearchInput(query) => {
                self.viz_preset_search = query;
                // The result list is a different length: show it from the top.
                self.viz_preset_scroll = 0.0;
                return cosmic::iced::widget::scrollable::scroll_to(
                    crate::views::now_playing::preset_browser::list_scroll_id(),
                    cosmic::iced::widget::scrollable::AbsoluteOffset {
                        x: None,
                        y: Some(0.0),
                    },
                );
            }

            #[cfg(feature = "visualizer")]
            Message::LoadVizPreset(path) => {
                // Not mirrored into `viz_current_preset` here: the render
                // thread is the authority and reports the preset only once
                // it is really on screen, so a failed load never leaves the
                // browser highlighting something that isn't playing.
                let _ = self
                    .viz_cmd_tx
                    .send(crate::views::now_playing::visualizer::VizCommand::LoadPreset(path));
            }

            #[cfg(feature = "visualizer")]
            Message::SetVizLocked(locked) => {
                self.viz_locked = locked;
                let _ = self
                    .viz_cmd_tx
                    .send(crate::views::now_playing::visualizer::VizCommand::SetLocked(locked));
            }

            #[cfg(feature = "visualizer")]
            Message::SetVizBeatSensitivity(sensitivity) => {
                self.viz_beat_sensitivity = sensitivity;
                let _ = self.viz_cmd_tx.send(
                    crate::views::now_playing::visualizer::VizCommand::SetBeatSensitivity(
                        sensitivity,
                    ),
                );
            }

            #[cfg(feature = "visualizer")]
            Message::VizPresetsScanned(entries) => {
                let first_delivery = self.viz_preset_entries.is_empty();
                self.viz_preset_entries = entries;
                self.viz_presets_scanned = true;
                // The browser may have opened before the list existed, so
                // there was nothing to scroll to; do it now. Later rescans
                // leave the user's scroll position alone.
                if first_delivery && self.viz_browser_open {
                    return self.scroll_preset_list_to_current();
                }
            }

            #[cfg(feature = "visualizer")]
            Message::PresetListScrolled(offset) => {
                self.viz_preset_scroll = offset.max(0.0);
            }

            // -- Playlists view --
            Message::SelectPlaylist(idx) => {
                self.selected_playlist = Some(idx);
                self.rename_playlist_input = self
                    .playlists
                    .get(idx)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
            }

            Message::BackToPlaylistList => {
                self.selected_playlist = None;
                self.rename_playlist_input.clear();
            }

            Message::CreatePlaylist(name) => {
                return self.playlist_op_async(PlaylistOp::Create, move |p| {
                    p.create_playlist(&name)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
            }

            Message::DeletePlaylist(idx) => {
                if let Some(playlist) = self.playlists.get(idx) {
                    let id = playlist.id.clone();
                    return self.playlist_op_async(PlaylistOp::Delete, move |p| {
                        p.delete_playlist(&id).map_err(|e| e.to_string())
                    });
                }
            }

            Message::RenamePlaylist(idx, new_name) => {
                if let Some(playlist) = self.playlists.get(idx) {
                    let id = playlist.id.clone();
                    return self.playlist_op_async(PlaylistOp::Rename, move |p| {
                        p.rename_playlist(&id, &new_name).map_err(|e| e.to_string())
                    });
                }
            }

            Message::PlaylistOpDone { op, result } => match result {
                Ok(()) => match op {
                    PlaylistOp::AddTrack => {}
                    PlaylistOp::Create => {
                        self.new_playlist_name.clear();
                        return self.load_playlists();
                    }
                    PlaylistOp::Delete => {
                        self.selected_playlist = None;
                        return self.load_playlists();
                    }
                    PlaylistOp::Rename => return self.load_playlists(),
                },
                Err(e) => {
                    tracing::error!("Playlist operation {op:?} failed: {e}");
                    return self.push_toast(widget::toaster::Toast::new(fl!(
                        "toast-provider-action-failed",
                        reason = e
                    )));
                }
            },

            Message::PlayPlaylist(idx) => {
                if let Some(playlist) = self.playlists.get(idx)
                    && !playlist.tracks.is_empty()
                {
                    return self.play_track_list(playlist.tracks.clone(), 0);
                }
            }

            Message::PlayPlaylistTrack(pl_idx, track_idx) => {
                if let Some(playlist) = self.playlists.get(pl_idx) {
                    return self.play_track_list(playlist.tracks.clone(), track_idx);
                }
            }

            Message::RemovePlaylistTrack(_pl_idx, _track_idx) => {
                // Track removal from playlists requires provider-specific
                // support that isn't uniformly available yet. Log and reload.
                tracing::info!("Track removal from playlist not yet implemented");
                return self.load_playlists();
            }

            Message::NewPlaylistNameChanged(name) => {
                self.new_playlist_name = name;
            }

            Message::RenamePlaylistInput(_idx, text) => {
                self.rename_playlist_input = text;
            }

            Message::PlaylistsLoaded(playlists) => {
                self.playlists = playlists;
                self.refresh_search_filter();
            }

            // -- Playlist import/export, mini player, compilations, album filters --
            Message::PlaylistIo(msg) => return self.update_playlist_io(msg),
            Message::Mini(msg) => return self.update_mini_player(msg),
            Message::AlbumFilter(msg) => self.extras.album_filter.update(msg),
            Message::SetShowCompilationsInArtists(show) => {
                self.config.show_compilations_in_artists = show;
                self.save_config();
                self.refresh_search_filter();
            }
            Message::SetM3uRelativePaths(relative) => {
                self.config.m3u_relative_paths = relative;
                self.save_config();
            }

            // -- Smart playlists view --
            Message::SmartPlaylists(msg) => {
                use crate::views::smart_playlists::SmartPlaylistMessage;

                match msg {
                    SmartPlaylistMessage::Navigate(route) => {
                        return self.navigate(route);
                    }
                    // List view
                    SmartPlaylistMessage::New => {
                        self.smart_playlist_editor =
                            Some(crate::views::smart_playlists::EditorState::new());
                    }
                    SmartPlaylistMessage::Edit(idx) => {
                        if let Some(playlist) = self.smart_playlists.get(idx) {
                            self.smart_playlist_editor =
                                Some(crate::views::smart_playlists::EditorState::from_existing(
                                    playlist.clone(),
                                ));
                        }
                    }
                    SmartPlaylistMessage::EditorCancel => {
                        self.smart_playlist_editor = None;
                    }
                    SmartPlaylistMessage::EditorSave => {
                        if let Some(state) = &self.smart_playlist_editor
                            && state.errors.is_empty()
                        {
                            let playlist = state.playlist.clone();
                            self.smart_playlist_editor = None;
                            let db_path = dirs::data_dir()
                                .unwrap_or_else(|| PathBuf::from("."))
                                .join("aulos")
                                .join("library.db");
                            return cosmic::task::future(async move {
                                let playlists = tokio::task::spawn_blocking(move || {
                                    let outcome: Result<
                                        Vec<crate::library::smart_playlist::SmartPlaylist>,
                                        String,
                                    > = (|| {
                                        let db = crate::library::LibraryDb::open(&db_path)?;
                                        if playlist.id == 0 {
                                            db.create_smart_playlist(&playlist)?;
                                        } else {
                                            db.update_smart_playlist(&playlist)?;
                                        }
                                        db.list_smart_playlists()
                                    })();
                                    outcome.unwrap_or_else(|e| {
                                        tracing::warn!("Save smart playlist failed: {e}");
                                        Vec::new()
                                    })
                                })
                                .await
                                .unwrap_or_default();
                                cosmic::Action::App(Message::SmartPlaylists(
                                    SmartPlaylistMessage::Loaded(playlists),
                                ))
                            });
                        }
                    }
                    SmartPlaylistMessage::Delete(idx) => {
                        if let Some(playlist) = self.smart_playlists.get(idx) {
                            let id = playlist.id;
                            if self.selected_smart_playlist == Some(idx) {
                                self.selected_smart_playlist = None;
                            }
                            let db_path = dirs::data_dir()
                                .unwrap_or_else(|| PathBuf::from("."))
                                .join("aulos")
                                .join("library.db");
                            return cosmic::task::future(async move {
                                let playlists = tokio::task::spawn_blocking(move || {
                                    let outcome: Result<
                                        Vec<crate::library::smart_playlist::SmartPlaylist>,
                                        String,
                                    > = (|| {
                                        let db = crate::library::LibraryDb::open(&db_path)?;
                                        db.delete_smart_playlist(id)?;
                                        db.list_smart_playlists()
                                    })();
                                    outcome.unwrap_or_else(|e| {
                                        tracing::warn!("Delete smart playlist failed: {e}");
                                        Vec::new()
                                    })
                                })
                                .await
                                .unwrap_or_default();
                                cosmic::Action::App(Message::SmartPlaylists(
                                    SmartPlaylistMessage::Loaded(playlists),
                                ))
                            });
                        }
                    }
                    SmartPlaylistMessage::Loaded(playlists) => {
                        self.smart_playlists = playlists;
                    }

                    // Selecting / playing a saved smart playlist
                    SmartPlaylistMessage::Select(idx) => {
                        self.selected_smart_playlist = Some(idx);
                        self.smart_playlist_tracks.clear();
                        if let Some(playlist) = self.smart_playlists.get(idx).cloned() {
                            let db_path = dirs::data_dir()
                                .unwrap_or_else(|| PathBuf::from("."))
                                .join("aulos")
                                .join("library.db");
                            return cosmic::task::future(async move {
                                let tracks = tokio::task::spawn_blocking(move || {
                                    crate::library::LibraryDb::open(&db_path)
                                        .and_then(|db| db.smart_playlist_tracks(&playlist, None))
                                        .unwrap_or_else(|e| {
                                            tracing::warn!("smart_playlist_tracks failed: {e}");
                                            Vec::new()
                                        })
                                })
                                .await
                                .unwrap_or_default();
                                cosmic::Action::App(Message::SmartPlaylists(
                                    SmartPlaylistMessage::TracksLoaded(tracks),
                                ))
                            });
                        }
                    }
                    SmartPlaylistMessage::BackToList => {
                        self.selected_smart_playlist = None;
                        self.smart_playlist_editor = None;
                    }
                    SmartPlaylistMessage::TracksLoaded(tracks) => {
                        self.smart_playlist_tracks = tracks;
                    }
                    SmartPlaylistMessage::Play(idx) => {
                        if let Some(playlist) = self.smart_playlists.get(idx).cloned() {
                            let db_path = dirs::data_dir()
                                .unwrap_or_else(|| PathBuf::from("."))
                                .join("aulos")
                                .join("library.db");
                            return cosmic::task::future(async move {
                                let tracks = tokio::task::spawn_blocking(move || {
                                    crate::library::LibraryDb::open(&db_path)
                                        .and_then(|db| db.smart_playlist_tracks(&playlist, None))
                                        .unwrap_or_else(|e| {
                                            tracing::warn!("smart_playlist_tracks failed: {e}");
                                            Vec::new()
                                        })
                                })
                                .await
                                .unwrap_or_default();
                                cosmic::Action::App(Message::SmartPlaylists(
                                    SmartPlaylistMessage::PlayResolved(tracks),
                                ))
                            });
                        }
                    }
                    SmartPlaylistMessage::PlayResolved(tracks) => {
                        if !tracks.is_empty() {
                            return self.play_track_list(tracks, 0);
                        }
                    }
                    SmartPlaylistMessage::Export(idx) => {
                        return self
                            .update_playlist_io(super::playlist_io::PlaylistIo::ExportSmart(idx));
                    }
                    SmartPlaylistMessage::PlayTrack(idx) => {
                        if !self.smart_playlist_tracks.is_empty() {
                            return self.play_track_list(self.smart_playlist_tracks.clone(), idx);
                        }
                    }

                    // Detail view: favorite/rating on a resolved track
                    SmartPlaylistMessage::ToggleFavorite(track_id) => {
                        if let Some(track) = self
                            .smart_playlist_tracks
                            .iter_mut()
                            .find(|t| t.id.to_string() == track_id)
                        {
                            track.is_favorite = !track.is_favorite;
                        }
                        return self.update(Message::ToggleFavorite(track_id));
                    }
                    SmartPlaylistMessage::SetRating(track_id, rating) => {
                        let new_rating = if rating == 0 { None } else { Some(rating) };
                        if let Some(track) = self
                            .smart_playlist_tracks
                            .iter_mut()
                            .find(|t| t.id.to_string() == track_id)
                        {
                            track.rating = new_rating;
                        }
                        return self.update(Message::SetRating(track_id, rating));
                    }

                    // Rules editor
                    SmartPlaylistMessage::EditorNameChanged(name) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_name(name);
                        }
                    }
                    SmartPlaylistMessage::EditorMatchModeChanged(i) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_match_mode(i);
                        }
                    }
                    SmartPlaylistMessage::EditorAddRule => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.add_rule();
                        }
                    }
                    SmartPlaylistMessage::EditorRemoveRule(i) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.remove_rule(i);
                        }
                    }
                    SmartPlaylistMessage::EditorRuleFieldChanged(i, f) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_rule_field(i, f);
                        }
                    }
                    SmartPlaylistMessage::EditorRuleOpChanged(i, o) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_rule_op(i, o);
                        }
                    }
                    SmartPlaylistMessage::EditorRuleValueChanged(i, v) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_rule_value(i, v);
                        }
                    }
                    SmartPlaylistMessage::EditorRuleValue2Changed(i, v) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_rule_value2(i, v);
                        }
                    }
                    SmartPlaylistMessage::EditorOrderByChanged(i) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_order_by(i);
                        }
                    }
                    SmartPlaylistMessage::EditorOrderDescToggled(v) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_order_desc(v);
                        }
                    }
                    SmartPlaylistMessage::EditorLimitToggled(v) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_limit_enabled(v);
                        }
                    }
                    SmartPlaylistMessage::EditorLimitChanged(v) => {
                        if let Some(state) = &mut self.smart_playlist_editor {
                            state.set_limit_input(v);
                        }
                    }
                }
            }

            // -- Genres view --
            Message::SelectGenre(idx) => return self.select_genre(idx),

            Message::BackToGenreGrid => {
                self.genre_tracks.clear();
                if let Some(task) = self.go_back() {
                    return task;
                }
                self.selected_genre = None;
            }

            Message::PlayGenreTrack(idx) => {
                if !self.genre_tracks.is_empty() {
                    return self.play_track_list(self.genre_tracks.clone(), idx);
                }
            }

            Message::ShuffleGenre => {
                if !self.genre_tracks.is_empty() {
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
                    let start = nanos % self.genre_tracks.len();
                    return self.play_track_list(self.genre_tracks.clone(), start);
                }
            }

            Message::GenresLoaded(genres) => {
                self.all_genres = genres;
                self.refresh_search_filter();
            }

            Message::GenreTracksLoaded { idx, tracks } => {
                if self.selected_genre == Some(idx) {
                    self.genre_tracks = tracks;
                }
            }

            // -- Podcasts --
            Message::Home(msg) => return self.update_home(msg),
            Message::Startup(msg) => return self.update_startup(msg),
            Message::Podcast(msg) => return self.update_podcast(msg),
            Message::PodcastEvent(event) => return self.handle_podcast_event(event),

            Message::OnlineIconLoaded(url, decoded) => {
                if let Some((w, h, pixels)) = decoded {
                    self.online_icons
                        .insert(url, widget::icon::from_raster_pixels(w, h, pixels));
                }
            }

            // -- Radio --
            Message::Radio(msg) => return self.update_radio(msg),
            Message::RadioEvent(event) => return self.handle_radio_event(event),

            // -- Convert / transcode / rip --
            Message::Convert(msg) => return self.update_convert(msg),
            Message::ConvertEvent(event) => return self.update_convert_event(event),
            Message::Quit => {
                self.flush_config();
                return cosmic::iced::exit();
            }
            Message::Playback(msg) => return self.update_playback_extras(msg),
            Message::Mpris(event) => match event {
                crate::mpris::MprisEvent::Ready(handle) => {
                    self.mpris = Some(handle);
                    return self.publish_mpris();
                }
                crate::mpris::MprisEvent::Command(cmd) => {
                    use crate::mpris::{LoopMode, MprisCommand};

                    let playing = self
                        .player
                        .as_ref()
                        .map(|p| p.state() == PlaybackState::Playing)
                        .unwrap_or(false);

                    // Each branch yields the task to run; the snapshot is
                    // republished afterwards so clients observe the result
                    // immediately. Without this, properties changed while
                    // stopped or paused would stay stale until the next
                    // playback tick — which only fires while playing.
                    // `Seek` is relative and `SetPosition` absolute; capture
                    // the distinction before the match so both can share one
                    // arm without cloning `cmd` on every command.
                    let relative_seek = matches!(cmd, MprisCommand::Seek(_));
                    let task = match cmd {
                        MprisCommand::Play => {
                            if playing {
                                Task::none()
                            } else {
                                self.update(Message::TogglePlayback)
                            }
                        }
                        MprisCommand::Pause => {
                            if playing {
                                self.update(Message::TogglePlayback)
                            } else {
                                Task::none()
                            }
                        }
                        MprisCommand::Stop => self.update(Message::Stop),
                        MprisCommand::PlayPause => self.update(Message::TogglePlayback),
                        MprisCommand::Next => self.update(Message::NextTrack),
                        MprisCommand::Previous => self.update(Message::PreviousTrack),
                        MprisCommand::Seek(offset_us) | MprisCommand::SetPosition(offset_us) => {
                            let seek = self.current_track.as_ref().and_then(|track| {
                                let duration_us = track.duration.as_micros() as i64;
                                (duration_us > 0).then(|| {
                                    let target_us = if relative_seek {
                                        self.playback_position.as_micros() as i64 + offset_us
                                    } else {
                                        offset_us
                                    };
                                    (target_us.clamp(0, duration_us) as f32) / duration_us as f32
                                })
                            });
                            match seek {
                                Some(fraction) => {
                                    self.seeking_preview = Some(fraction);
                                    self.update(Message::SeekCommit)
                                }
                                None => Task::none(),
                            }
                        }
                        MprisCommand::SetVolume(vol) => Task::batch([
                            self.update(Message::SetVolume(vol.clamp(0.0, 1.0) as f32)),
                            self.update(Message::VolumeCommit),
                        ]),
                        MprisCommand::Shuffle(enabled) => {
                            if self.config.shuffle == enabled {
                                Task::none()
                            } else {
                                self.update(Message::ToggleShuffle)
                            }
                        }
                        MprisCommand::Loop(mode) => {
                            let desired = match mode {
                                LoopMode::None => crate::config::RepeatMode::None,
                                LoopMode::Playlist => crate::config::RepeatMode::All,
                                LoopMode::Track => crate::config::RepeatMode::One,
                            };
                            // `CycleRepeat` only steps one position at a time
                            // around the 3-variant cycle; drive it around
                            // until it lands on `desired`, reusing its exact
                            // MPD-dispatch logic at each step.
                            let mut tasks = Vec::new();
                            for _ in 0..3 {
                                if self.config.repeat_mode == desired {
                                    break;
                                }
                                tasks.push(self.update(Message::CycleRepeat));
                            }
                            Task::batch(tasks)
                        }
                        MprisCommand::Raise => self.raise_window(),
                        MprisCommand::Quit => self.update(Message::Quit),
                        MprisCommand::OpenUri(uri) => match crate::file_uri_to_path(&uri) {
                            Some(path) => self.update(Message::OpenFiles(vec![path])),
                            None => {
                                tracing::warn!("MPRIS OpenUri: unsupported or invalid URI: {uri}");
                                Task::none()
                            }
                        },
                    };
                    let mpris_task = self.publish_mpris();
                    return Task::batch([task, mpris_task]);
                }
            },
            Message::OpenFiles(paths) => {
                return self.open_files(paths);
            }
            Message::OpenFilesScanned(tracks) => {
                if tracks.is_empty() {
                    tracing::warn!("No playable tracks found among opened files");
                    return self
                        .push_toast(widget::toaster::Toast::new(fl!("toast-open-files-failed")));
                }
                return self.play_track_list(tracks, 0);
            }
            Message::MprisArtResolved(track_id, art_url) => {
                if let Some(handle) = self.mpris.as_ref() {
                    handle.cache_art_url(track_id, art_url);
                }
                return self.publish_mpris();
            }
            // Global keyboard shortcuts resolved by `crate::keybinds::resolve`
            // (see `on_key_press` in `subscription()`). The resolver is a bare
            // `fn` pointer with no access to `self`, so every shortcut arrives
            // here as a self-describing `Shortcut` and is interpreted against
            // the current application state -- each arm below just forwards to
            // the existing message/helper that already does the real work.
            Message::Shortcut(shortcut) => {
                use crate::keybinds::Shortcut;

                // The library-search field can be visible without holding
                // keyboard focus (a focused text input already captures its
                // own key presses before `on_key_press` ever sees them), so
                // this is the extra guard that stops transport/navigation
                // shortcuts from firing while the user is meant to be typing
                // a query. `FocusSearch`/`Escape` must keep working.
                if self.search_active
                    && !matches!(shortcut, Shortcut::FocusSearch | Shortcut::Escape)
                {
                    return Task::none();
                }

                match shortcut {
                    Shortcut::PlayPause => return self.update(Message::TogglePlayback),
                    Shortcut::Stop => return self.update(Message::Stop),
                    Shortcut::Next => return self.update(Message::NextTrack),
                    Shortcut::Previous => return self.update(Message::PreviousTrack),
                    Shortcut::SeekForward | Shortcut::SeekBackward => {
                        if let Some(track) = &self.current_track
                            && track.duration > Duration::ZERO
                        {
                            let step = Duration::from_secs(5);
                            let target = if shortcut == Shortcut::SeekForward {
                                (self.playback_position + step).min(track.duration)
                            } else {
                                self.playback_position.saturating_sub(step)
                            };
                            self.seeking_preview =
                                Some(target.as_secs_f32() / track.duration.as_secs_f32());
                            return self.update(Message::SeekCommit);
                        }
                    }
                    Shortcut::VolumeUp | Shortcut::VolumeDown => {
                        let current = self
                            .player
                            .as_ref()
                            .map_or(self.config.volume, |p| p.volume());
                        let step = 0.05;
                        let target = if shortcut == Shortcut::VolumeUp {
                            (current + step).min(1.0)
                        } else {
                            (current - step).max(0.0)
                        };
                        return Task::batch([
                            self.update(Message::SetVolume(target)),
                            self.update(Message::VolumeCommit),
                        ]);
                    }
                    Shortcut::Mute => {
                        let current = self
                            .player
                            .as_ref()
                            .map_or(self.config.volume, |p| p.volume());
                        let target = if current > 0.0 {
                            // Remember the level so unmuting restores it
                            // rather than jumping to some fixed default.
                            self.pre_mute_volume = Some(current);
                            0.0
                        } else {
                            // Fall back to full volume only when we have no
                            // record of a pre-mute level (e.g. the app
                            // started at zero).
                            self.pre_mute_volume.take().unwrap_or(1.0)
                        };
                        return Task::batch([
                            self.update(Message::SetVolume(target)),
                            self.update(Message::VolumeCommit),
                        ]);
                    }
                    Shortcut::ToggleShuffle => return self.update(Message::ToggleShuffle),
                    Shortcut::CycleRepeat => return self.update(Message::CycleRepeat),
                    Shortcut::ToggleFavorite => {
                        if let Some(id) = self.current_track.as_ref().map(|t| t.id.to_string()) {
                            return self.update(Message::ToggleFavorite(id));
                        }
                    }
                    Shortcut::ToggleLyrics => return self.update(Message::ShowLyrics),
                    Shortcut::ToggleQueue => {
                        return self.update(Message::ToggleContextPage(ContextPage::Queue));
                    }
                    Shortcut::ToggleExpanded => {
                        return if self.expand_progress > 0.0 || self.expand_target.is_some() {
                            self.update(Message::CollapseNowPlaying)
                        } else {
                            self.update(Message::ExpandNowPlaying)
                        };
                    }
                    Shortcut::ToggleMiniPlayer => {
                        return self
                            .update(Message::Mini(super::view_extras::MiniPlayerMsg::Toggle));
                    }
                    Shortcut::FocusSearch => return self.update(Message::ToggleLibrarySearch),
                    Shortcut::NavPage(n) => {
                        // Bind the entity first: `nav.iter()` borrows `self`
                        // immutably and `on_nav_select` needs it mutably.
                        let target = self.nav.iter().nth((n as usize).saturating_sub(1));
                        if let Some(entity) = target {
                            return self.on_nav_select(entity);
                        }
                    }
                    Shortcut::Escape => {
                        if self.extras.mini_player {
                            return self
                                .update(Message::Mini(super::view_extras::MiniPlayerMsg::Toggle));
                        }
                        if self.core.window.show_context {
                            self.core.window.show_context = false;
                        } else if self.expand_progress > 0.0 || self.expand_target.is_some() {
                            return self.update(Message::CollapseNowPlaying);
                        } else if self.search_active {
                            return self.update(Message::ClearLibrarySearch);
                        }
                    }
                }
            }
        }

        Task::none()
    }

    pub(super) fn select_nav(&mut self, id: nav_bar::Id) -> Task<cosmic::Action<Message>> {
        self.nav.activate(id);
        self.remember_page();
        self.clear_nav_history();
        // Reset sub-view selections when switching pages
        self.selected_album = None;
        self.selected_artist = None;
        self.selected_playlist = None;
        self.selected_genre = None;
        self.selected_smart_playlist = None;
        self.smart_playlist_editor = None;
        self.selected_podcast = None;

        // Collapse expanded now-playing view when navigating
        let collapse_task = if self.expand_progress > 0.0 || self.expand_target.is_some() {
            self.lyrics_overlay_active = false;
            let transition = self.begin_expand_transition(0.0);
            #[cfg(feature = "visualizer")]
            let transition = transition.chain(self.exit_viz_fullscreen());
            transition
        } else {
            Task::none()
        };

        // Lazy-load data for Playlists and Genres pages
        let page = self.nav.active_data::<Page>().cloned();
        let page_task = match page {
            Some(Page::Playlists) => self.load_playlists(),
            Some(Page::SmartPlaylists) => {
                let db_path = dirs::data_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("aulos")
                    .join("library.db");
                cosmic::task::future(async move {
                    let playlists = tokio::task::spawn_blocking(move || {
                        crate::library::LibraryDb::open(&db_path)
                            .and_then(|db| db.list_smart_playlists())
                            .unwrap_or_else(|e| {
                                tracing::warn!("list_smart_playlists failed: {e}");
                                Vec::new()
                            })
                    })
                    .await
                    .unwrap_or_default();
                    cosmic::Action::App(Message::SmartPlaylists(
                        crate::views::smart_playlists::SmartPlaylistMessage::Loaded(playlists),
                    ))
                })
            }
            Some(Page::Home) => self.enter_home(),
            Some(Page::Genres) => self.load_genres(),
            Some(Page::Artists) => self.load_artist_info_for_visible(),
            Some(Page::Folders) => {
                if !self.folder_state.is_populated()
                    || self.folder_tree_gen != Some(self.library_gen)
                {
                    self.folder_state
                        .set_tree(crate::views::folders::FolderTree::build(&self.all_tracks));
                    self.folder_tree_gen = Some(self.library_gen);
                }
                Task::none()
            }
            Some(Page::Podcasts) => self.load_podcasts(),
            Some(Page::Radio) => self.load_radio_stations(),
            Some(Page::Convert) => self.detect_ffmpeg_once(),
            _ => Task::none(),
        };

        let title_task = self.update_title();
        Task::batch([title_task, page_task, collapse_task])
    }

    /// Start an expand (`target = 1.0`) or collapse (`0.0`) transition of
    /// the now-playing sheet. The sheet animates itself at draw time; this
    /// only schedules a single `ExpandAnimTick` for when it has finished,
    /// which unmounts the sheet after a collapse.
    pub(super) fn begin_expand_transition(&mut self, target: f32) -> Task<cosmic::Action<Message>> {
        self.expand_target = Some(target);
        self.expand_anim_start = Some(std::time::Instant::now());
        cosmic::task::future(async {
            tokio::time::sleep(crate::views::now_playing::sheet::DURATION).await;
            cosmic::Action::App(Message::ExpandAnimTick)
        })
    }

    /// Scrolls the preset browser list so the playing preset (matched by
    /// path) is centered, or to the top when it isn't in the filtered list.
    /// `viz_preset_scroll` is set immediately so the first rebuild already
    /// builds the right window, before the scroll operation lands.
    #[cfg(feature = "visualizer")]
    fn scroll_preset_list_to_current(&mut self) -> Task<cosmic::Action<Message>> {
        use crate::views::now_playing::preset_browser as browser;
        let rows = browser::build_rows(&self.viz_preset_entries, &self.viz_preset_search);
        let y = browser::current_row(
            &rows,
            &self.viz_preset_entries,
            self.viz_current_preset.as_deref(),
        )
        .map_or(0.0, browser::scroll_offset_for_row);
        self.viz_preset_scroll = y;
        cosmic::iced::widget::scrollable::scroll_to(
            browser::list_scroll_id(),
            cosmic::iced::widget::scrollable::AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        )
    }
}
