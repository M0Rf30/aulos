// SPDX-License-Identifier: GPL-3.0

use super::tasks::{download_episode_task, refresh_podcast_task, resolve_and_play_radio};
use super::{
    AppModel, ContextPage, HTTP_CLIENT, Message, Page, SEARCH_INPUT_ID, now_epoch,
    open_online_store, parse_delimiters_input,
};
use crate::config::ReplayGainMode;
use crate::convert::{ConvertJob, JobKind, JobState, OutputFormat, run_job};
use crate::fl;
use crate::library::{LibraryScanner, LyricsProvider, Track};
use crate::online::podcast;
use crate::online::radio;
use crate::player::{ActiveBackend, PlaybackState};
use crate::views::{convert, providers};
use cosmic::Application;
use cosmic::prelude::*;
use cosmic::widget::{self, nav_bar};
use std::path::PathBuf;
use std::sync::Arc;
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
                self.refresh_search_filter();
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
                cover_images,
                artist_avatars,
                cover_art_bytes,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return Task::none();
                }
                self.library_scanning = false;
                self.all_tracks = tracks;
                self.all_albums = albums;
                self.cover_images = cover_images;
                self.artist_avatars = artist_avatars;
                self.rebuild_all_artists();
                self.cover_art_bytes = cover_art_bytes.into();
                // Rebuild the folder tree only if it's already in use —
                // never-opened Folders view pays nothing on reload.
                if self.folder_state.is_populated()
                    || self.nav.active_data::<Page>() == Some(&Page::Folders)
                {
                    self.folder_state
                        .set_tree(crate::views::folders::FolderTree::build(&self.all_tracks));
                }
                self.refresh_search_filter();
                // Re-trigger blur now that cover art bytes are available
                let blur_task = self.maybe_update_blurred_cover();
                return blur_task;
            }

            Message::LibraryBatch {
                generation,
                provider_id,
                albums,
                cover_images,
                cover_art_bytes,
            } => {
                if self.is_stale_reload(generation, &provider_id) {
                    return Task::none();
                }
                // Append new albums and extract tracks
                for album in &albums {
                    for track in &album.tracks {
                        self.all_tracks.push(track.clone());
                    }
                    self.all_albums.push(album.clone());
                }
                // Merge cover images
                self.cover_images.extend(cover_images);
                // Merge cover art bytes for blur
                self.cover_art_bytes.extend(cover_art_bytes);

                // Incrementally merge only the new batch into artists
                self.merge_artists_from_batch(&albums);
                self.refresh_search_filter();

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
                // Final sort
                self.all_tracks.sort_by(|a, b| a.title.cmp(&b.title));
                self.all_albums.sort_by(|a, b| a.name.cmp(&b.name));
                self.all_artists.sort_by(|a, b| a.name.cmp(&b.name));
                self.refresh_search_filter();
                tracing::info!(
                    "Library load complete: {} albums, {} tracks, {} artists",
                    self.all_albums.len(),
                    self.all_tracks.len(),
                    self.all_artists.len()
                );
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
                                .join("lyra")
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
                    if player.state() == PlaybackState::Stopped && !self.all_tracks.is_empty() {
                        // If stopped, start playing first track
                        player.set_queue(self.all_tracks.clone());
                        if player.play_index(0).is_ok() {
                            self.current_track = self.all_tracks.first().cloned();
                            self.playback_position = Duration::ZERO;
                            #[cfg(feature = "visualizer")]
                            {
                                self.viz_metadata_opacity = 1.0;
                            }
                            task = self.dispatch_mpd_after_play();
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
                if let Some(ref mut player) = self.player {
                    match player.next() {
                        Ok(Some(track)) => {
                            self.current_track = Some(track.clone());
                            self.playback_position = Duration::ZERO;
                            self.lyrics_text = None;
                            #[cfg(feature = "visualizer")]
                            {
                                self.viz_metadata_opacity = 1.0;
                            }
                            let mpd_task = self.dispatch_mpd_after_play();
                            let blur_task = self.maybe_update_blurred_cover();
                            return Task::batch([mpd_task, blur_task]);
                        }
                        Err(e) => tracing::error!("Next track failed: {e}"),
                        _ => {}
                    }
                }
            }

            Message::PreviousTrack => {
                if let Some(ref mut player) = self.player {
                    match player.previous() {
                        Ok(Some(track)) => {
                            self.current_track = Some(track.clone());
                            self.playback_position = Duration::ZERO;
                            self.lyrics_text = None;
                            #[cfg(feature = "visualizer")]
                            {
                                self.viz_metadata_opacity = 1.0;
                            }
                            let mpd_task = self.dispatch_mpd_after_play();
                            let blur_task = self.maybe_update_blurred_cover();
                            return Task::batch([mpd_task, blur_task]);
                        }
                        Err(e) => tracing::error!("Previous track failed: {e}"),
                        _ => {}
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

            // Task 111: Wire shuffle toggle for MPD
            Message::ToggleShuffle => {
                self.config.shuffle = !self.config.shuffle;
                if let Some(mpd) = self.active_mpd_provider() {
                    let enabled = self.config.shuffle;
                    if let Err(e) = mpd.send_random(enabled) {
                        tracing::error!("MPD send_random: {e}");
                    }
                }
                return self.publish_mpris();
            }

            // Task 112: Wire repeat mode for MPD
            Message::CycleRepeat => {
                self.config.repeat_mode = self.config.repeat_mode.next();
                if let Some(mpd) = self.active_mpd_provider() {
                    let (repeat, single) = match self.config.repeat_mode {
                        crate::config::RepeatMode::None => {
                            (false, mpd_client::commands::SingleMode::Disabled)
                        }
                        crate::config::RepeatMode::All => {
                            (true, mpd_client::commands::SingleMode::Disabled)
                        }
                        crate::config::RepeatMode::One => {
                            (true, mpd_client::commands::SingleMode::Enabled)
                        }
                    };
                    if let Err(e) = mpd.send_repeat(repeat) {
                        tracing::error!("MPD send_repeat: {e}");
                    }
                    if let Err(e) = mpd.send_single(single) {
                        tracing::error!("MPD send_single: {e}");
                    }
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
                // Feed polled status into the MPD backend cache.
                if let Some(player) = &mut self.player {
                    if let Some(mpd) = player.mpd_backend_mut() {
                        mpd.update_status(position, duration, state, volume);
                    }

                    // Real MPD-server playback state (not gated on which
                    // backend Lyra currently considers "active" — see the
                    // early-return below and `AppModel::mpd_playing`'s doc
                    // comment): the PipeWire capture thread's monitor
                    // fallback must only feed the visualizer while MPD
                    // itself is actually the one making sound.
                    #[cfg(feature = "visualizer")]
                    {
                        self.mpd_playing
                            .store(state == PlaybackState::Playing, std::sync::atomic::Ordering::Relaxed);
                    }

                    // The poll runs whenever an MPD backend exists, which
                    // includes while Lyra plays a local file, radio stream
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

                    // Adopt a track MPD is already playing that Lyra didn't
                    // start itself — e.g. right after switching to this MPD
                    // backend while its server was already mid-playback, or
                    // MPD's song changed out from under Lyra (another
                    // client skipped tracks). `song` is only `Some` on the
                    // tick where MPD's song identity changed.
                    if let Some(track) = song
                        && state != PlaybackState::Stopped
                    {
                        player.adopt_mpd_track(track.clone(), duration);
                        self.current_track = Some(track);
                        self.lyrics_text = None;
                        self.scrobble_now_playing_sent = false;
                        self.scrobble_sent = false;
                        #[cfg(feature = "visualizer")]
                        {
                            self.viz_metadata_opacity = 1.0;
                        }
                    }

                    // Update UI position (unless user is dragging seek slider).
                    if self.seeking_preview.is_none() {
                        self.playback_position = position;
                        if let Some(track) = &self.current_track
                            && self.playback_position > track.duration
                        {
                            self.playback_position = track.duration;
                        }
                    }

                    // Check if track ended (MPD reports Stopped after playback).
                    if player.is_finished().unwrap_or(false)
                        && let Ok(Some(track)) = player.next()
                    {
                        self.current_track = Some(track.clone());
                        self.playback_position = Duration::ZERO;
                        self.lyrics_text = None;
                        self.scrobble_now_playing_sent = false;
                        self.scrobble_sent = false;
                        #[cfg(feature = "visualizer")]
                        {
                            self.viz_metadata_opacity = 1.0;
                        }
                        // Dispatch the actual async MPD play command.
                        return self.dispatch_mpd_after_play();
                    }
                }

                // Scrobble handling for MPD tracks.
                if let Some(track) = self.current_track.clone() {
                    self.handle_scrobble(track);
                }
                return self.publish_mpris();
            }

            Message::MpdCommandError(err) => {
                tracing::error!("Async MPD command failed: {err}");
                // The next status poll will self-correct the UI state.
            }

            Message::PlaybackTick => {
                // Local/Subsonic playback — read position from the active backend.
                if let Some(ref mut player) = self.player {
                    if self.seeking_preview.is_none() {
                        self.playback_position = player.position();

                        if let Some(ref track) = self.current_track
                            && self.playback_position > track.duration
                        {
                            self.playback_position = track.duration;
                        }
                    }

                    // Check if track ended
                    if player.is_finished().unwrap_or(false)
                        && let Ok(Some(track)) = player.next()
                    {
                        self.current_track = Some(track.clone());
                        self.playback_position = Duration::ZERO;
                        self.lyrics_text = None;
                        self.scrobble_now_playing_sent = false;
                        self.scrobble_sent = false;
                        #[cfg(feature = "visualizer")]
                        {
                            self.viz_metadata_opacity = 1.0;
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
                return Task::batch([position_save_task, mpris_task]);
            }

            // -- Track selection --
            Message::PlayTrackIndex(index) => {
                return self.play_track_list(self.all_tracks.clone(), index);
            }

            Message::Folders(msg) => match msg {
                crate::views::folders::FolderMessage::Open(dir) => self.folder_state.open(dir),
                crate::views::folders::FolderMessage::Up => self.folder_state.up(),
                crate::views::folders::FolderMessage::GoTo(index) => {
                    self.folder_state.go_to(index);
                }
                crate::views::folders::FolderMessage::PlayTrack(index) => {
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
                    // Appending only makes sense on top of an existing
                    // queue; with nothing queued, "add to queue" has to
                    // start playback or the tracks would sit unreachable.
                    let can_append = self.player.as_ref().is_some_and(|p| !p.queue_is_empty());
                    if !can_append {
                        return self.play_track_list(tracks, 0);
                    }
                    let added = tracks.len();
                    if let Some(player) = self.player.as_mut() {
                        player.extend_queue(tracks);
                    }
                    return self.push_toast(widget::toaster::Toast::new(fl!(
                        "queued-tracks",
                        count = added
                    )));
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
                self.selected_album = None;
            }

            Message::SelectArtist(idx) => {
                self.selected_artist = Some(idx);
            }

            Message::BackToArtistList => {
                self.selected_artist = None;
            }

            Message::SortSongs(field) => {
                self.songs_sort = field;
                self.sort_tracks(field);
                self.refresh_search_filter();
            }

            Message::ToggleFavoritesFilter => {
                self.favorites_filter = !self.favorites_filter;
                // Clear genre filter when toggling favorites
                if self.favorites_filter {
                    self.genre_filter = None;
                }
            }

            Message::ToggleFavorite(track_id) => {
                if let Some(provider) = self.registry.active_shared() {
                    match provider.toggle_favorite(&track_id) {
                        Ok(new_state) => {
                            // Update the track's is_favorite in our local data.
                            for track in &mut self.all_tracks {
                                if track.id.to_string() == track_id {
                                    track.is_favorite = new_state;
                                }
                            }
                            for album in &mut self.all_albums {
                                for track in &mut album.tracks {
                                    if track.id.to_string() == track_id {
                                        track.is_favorite = new_state;
                                    }
                                }
                            }
                            for artist in &mut self.all_artists {
                                for album in &mut artist.albums {
                                    for track in &mut album.tracks {
                                        if track.id.to_string() == track_id {
                                            track.is_favorite = new_state;
                                        }
                                    }
                                }
                            }
                            // Also update the current playing track if it matches.
                            if let Some(ref mut ct) = self.current_track
                                && ct.id.to_string() == track_id
                            {
                                ct.is_favorite = new_state;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("toggle_favorite failed: {e}");
                        }
                    }
                }
            }

            Message::SetRating(track_id, rating) => {
                if let Some(provider) = self.registry.active_shared() {
                    match provider.set_rating(&track_id, rating) {
                        Ok(()) => {
                            let new_rating = if rating == 0 { None } else { Some(rating) };
                            for track in &mut self.all_tracks {
                                if track.id.to_string() == track_id {
                                    track.rating = new_rating;
                                }
                            }
                            for album in &mut self.all_albums {
                                for track in &mut album.tracks {
                                    if track.id.to_string() == track_id {
                                        track.rating = new_rating;
                                    }
                                }
                            }
                            for artist in &mut self.all_artists {
                                for album in &mut artist.albums {
                                    for track in &mut album.tracks {
                                        if track.id.to_string() == track_id {
                                            track.rating = new_rating;
                                        }
                                    }
                                }
                            }
                            if let Some(ref mut ct) = self.current_track
                                && ct.id.to_string() == track_id
                            {
                                ct.rating = new_rating;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("set_rating failed: {e}");
                        }
                    }
                }
            }

            Message::AddToPlaylist(track_source_uri, playlist_id) => {
                if let Some(provider) = self.registry.active_shared()
                    && let Err(e) = provider.add_to_playlist(&playlist_id, &[track_source_uri])
                {
                    tracing::warn!("add_to_playlist failed: {e}");
                }
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
                    .join("lyra")
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
                    .join("lyra")
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

            Message::UpdateConfig(config) => {
                let artist_split_changed = config.split_artist_tags
                    != self.config.split_artist_tags
                    || config.artist_tag_delimiters != self.config.artist_tag_delimiters;
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
                    self.active_provider_index = Some(index);
                    tracing::info!("Switched to provider: {id}");

                    // Recreate player with the correct backend for the new provider.
                    self.recreate_player();

                    // Clear current library data and reload from new provider
                    self.all_tracks.clear();
                    self.all_albums.clear();
                    self.all_artists.clear();
                    self.cover_images.clear();
                    self.artist_avatars.clear();
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

            Message::MpdIdleEvent(provider_id) => {
                tracing::debug!("MPD idle event from provider '{provider_id}'");
                // If this is the active provider, reload the library to pick up changes
                if self.registry.active_id() == provider_id {
                    return self.reload_library();
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
                self.expand_target = Some(1.0);
                self.expand_anim_start = Some(std::time::Instant::now());
                self.expand_anim_from = self.expand_progress;
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
                self.expand_target = Some(0.0);
                self.expand_anim_start = Some(std::time::Instant::now());
                self.expand_anim_from = self.expand_progress;
                self.lyrics_overlay_active = false;
                // Leaving the expanded view must also leave fullscreen, else the
                // header bar / nav sidebar would stay hidden with no visualizer.
                #[cfg(feature = "visualizer")]
                self.exit_viz_fullscreen();
            }

            Message::ExpandAnimTick => {
                use crate::views::now_playing::animation;

                if let (Some(target), Some(start)) = (self.expand_target, self.expand_anim_start) {
                    let elapsed = start.elapsed().as_secs_f32() * 1000.0;
                    let t = (elapsed / animation::ANIMATION_DURATION_MS).min(1.0);

                    // Apply easing based on direction
                    let eased = if target > self.expand_anim_from {
                        animation::ease_out(t)
                    } else {
                        animation::ease_in(t)
                    };

                    self.expand_progress = animation::lerp(self.expand_anim_from, target, eased);

                    // Check if animation is complete
                    if t >= 1.0 {
                        self.expand_progress = target;
                        self.expand_target = None;
                        self.expand_anim_start = None;
                    }
                }
            }

            Message::BlurReady(key, handle, accent) => {
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
                        self.blurred_cover_key = Some(key);
                    }
                    self.accent = accent;
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
                    self.exit_viz_fullscreen();
                    self.viz_browser_open = false;
                    // Force retry even if key matches — blurred_cover may be None.
                    if self.blurred_cover.is_none() {
                        self.blurred_cover_key = None;
                    }
                    return self.maybe_update_blurred_cover();
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

                // Resync the UI-local current-preset name from whatever
                // the render thread most recently set (see
                // `viz_current_preset_shared`'s doc comment). Overwrites
                // any optimistic value `LoadVizPreset` already set with
                // the render thread's own value — they converge within a
                // frame either way.
                if let Ok(name) = self.viz_current_preset_shared.lock() {
                    self.viz_current_preset_name = name.clone();
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
                // titlebar. "Fullscreen" here means hiding the header bar and the
                // nav sidebar so the visualizer fills the whole window. (The old
                // `toggle_decorations` call targeted server-side decorations,
                // which COSMIC never draws, so it was a silent no-op.)
                self.viz_fullscreen = !self.viz_fullscreen;
                if self.viz_fullscreen {
                    self.viz_hud_idle_frames = 0;
                    self.viz_prev_nav_active = self.core.nav_bar_active();
                    self.core.window.show_headerbar = false;
                    self.core.nav_bar_set_toggled(false);
                } else {
                    self.core.window.show_headerbar = true;
                    self.core.nav_bar_set_toggled(self.viz_prev_nav_active);
                }
            }

            #[cfg(feature = "visualizer")]
            Message::TogglePresetBrowser => {
                self.viz_browser_open = !self.viz_browser_open;
                if self.viz_browser_open {
                    self.viz_hud_idle_frames = 0;
                    if !self.viz_presets_scan_started {
                        self.viz_presets_scan_started = true;
                        return cosmic::task::future(async move {
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
                    }
                }
            }

            #[cfg(feature = "visualizer")]
            Message::PresetSearchInput(query) => {
                self.viz_preset_search = query;
            }

            #[cfg(feature = "visualizer")]
            Message::LoadVizPreset(path) => {
                // Optimistic: reflect the load immediately so the browser
                // highlights the new row without waiting on the render
                // thread's next frame — see `VisualizerFrameReady`.
                self.viz_current_preset_name =
                    path.file_stem().map(|s| s.to_string_lossy().into_owned());
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
                self.viz_preset_entries = entries;
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
                if let Some(provider) = self.registry.active_shared() {
                    match provider.create_playlist(&name) {
                        Ok(_) => {
                            self.new_playlist_name.clear();
                            return self.load_playlists();
                        }
                        Err(e) => {
                            tracing::error!("Failed to create playlist: {e}");
                        }
                    }
                }
            }

            Message::DeletePlaylist(idx) => {
                if let Some(playlist) = self.playlists.get(idx) {
                    let id = playlist.id.clone();
                    if let Some(provider) = self.registry.active_shared() {
                        match provider.delete_playlist(&id) {
                            Ok(()) => {
                                self.selected_playlist = None;
                                return self.load_playlists();
                            }
                            Err(e) => {
                                tracing::error!("Failed to delete playlist: {e}");
                            }
                        }
                    }
                }
            }

            Message::RenamePlaylist(idx, new_name) => {
                if let Some(playlist) = self.playlists.get(idx) {
                    let id = playlist.id.clone();
                    if let Some(provider) = self.registry.active_shared() {
                        match provider.rename_playlist(&id, &new_name) {
                            Ok(()) => {
                                return self.load_playlists();
                            }
                            Err(e) => {
                                tracing::error!("Failed to rename playlist: {e}");
                            }
                        }
                    }
                }
            }

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

            // -- Smart playlists view --
            Message::SmartPlaylists(msg) => {
                use crate::views::smart_playlists::SmartPlaylistMessage;

                match msg {
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
                                .join("lyra")
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
                                .join("lyra")
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
                                .join("lyra")
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
                                .join("lyra")
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
            Message::SelectGenre(idx) => {
                self.selected_genre = Some(idx);
                // Load tracks for the selected genre
                if let Some(genre_name) = self.all_genres.get(idx) {
                    let genre = genre_name.clone();
                    if let Some(provider) = self.registry.active_shared() {
                        let tracks = provider.get_tracks_by_genre(&genre).unwrap_or_default();
                        self.genre_tracks = tracks;
                    } else {
                        // Fall back to filtering local tracks
                        self.genre_tracks = self
                            .all_tracks
                            .iter()
                            .filter(|t| t.genre.eq_ignore_ascii_case(&genre))
                            .cloned()
                            .collect();
                    }
                }
            }

            Message::BackToGenreGrid => {
                self.selected_genre = None;
                self.genre_tracks.clear();
            }

            Message::PlayGenreTrack(idx) => {
                if !self.genre_tracks.is_empty() {
                    return self.play_track_list(self.genre_tracks.clone(), idx);
                }
            }

            Message::GenresLoaded(genres) => {
                self.all_genres = genres;
                self.refresh_search_filter();
            }

            Message::GenreTracksLoaded(tracks) => {
                self.genre_tracks = tracks;
            }

            // -- Podcasts --
            Message::PodcastSearchChanged(query) => {
                self.podcast_search_query = query;
            }

            Message::PodcastSearchSubmit => {
                let query = self.podcast_search_query.trim().to_string();
                if query.is_empty() {
                    return Task::none();
                }
                self.podcast_search_loading = true;
                return cosmic::task::future(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let client = HTTP_CLIENT.clone();
                        podcast::search_itunes(&client, &query)
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    cosmic::Action::App(Message::PodcastSearchResults(result))
                });
            }

            Message::PodcastSearchResults(result) => {
                self.podcast_search_loading = false;
                match result {
                    Ok(results) => {
                        let icon_urls: Vec<String> =
                            results.iter().map(|r| r.image.clone()).collect();
                        self.podcast_search_results = results;
                        return self.load_online_icons(icon_urls);
                    }
                    Err(e) => {
                        return self.push_toast(widget::toaster::Toast::new(fl!(
                            "toast-podcast-search-failed",
                            reason = e
                        )));
                    }
                }
            }

            Message::PodcastAddUrlChanged(url) => {
                self.podcast_add_url = url;
            }

            Message::SubscribePodcast(feed_url) => {
                let feed_url = feed_url.trim().to_string();
                if feed_url.is_empty() {
                    return Task::none();
                }
                self.podcast_add_url.clear();
                return cosmic::task::future(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let client = HTTP_CLIENT.clone();
                        let (meta, episodes) = podcast::fetch_feed(&client, &feed_url)?;
                        let store = open_online_store()?;
                        let id = store.add_podcast(&feed_url, &meta)?;
                        store.upsert_episodes(id, &episodes)?;
                        store.touch_podcast_refresh(id, &meta, now_epoch())?;
                        Ok(())
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    cosmic::Action::App(Message::PodcastSubscribed(result))
                });
            }

            Message::PodcastSubscribed(result) => match result {
                Ok(()) => {
                    self.podcast_search_results.clear();
                    self.podcast_search_query.clear();
                    return self.load_podcasts();
                }
                Err(e) => {
                    return self.push_toast(widget::toaster::Toast::new(fl!(
                        "toast-podcast-subscribe-failed",
                        reason = e
                    )));
                }
            },

            Message::PodcastsLoaded(podcasts) => {
                let icon_urls: Vec<String> = podcasts.iter().map(|p| p.image_url.clone()).collect();
                self.podcasts = podcasts;
                return self.load_online_icons(icon_urls);
            }

            Message::SelectPodcast(idx) => {
                self.selected_podcast = Some(idx);
                if let Some(podcast) = self.podcasts.get(idx) {
                    return self.load_podcast_episodes(podcast.id);
                }
            }

            Message::BackToPodcastList => {
                self.selected_podcast = None;
                self.podcast_episodes.clear();
            }

            Message::RemovePodcast(idx) => {
                if let Some(podcast) = self.podcasts.get(idx) {
                    let id = podcast.id;
                    if let Ok(store) = open_online_store()
                        && let Ok(episodes) = store.list_episodes(id)
                    {
                        for episode in episodes {
                            if !episode.downloaded_path.is_empty() {
                                let _ = std::fs::remove_file(&episode.downloaded_path);
                            }
                        }
                    }
                    match open_online_store().and_then(|store| store.remove_podcast(id)) {
                        Ok(()) => {
                            if self.selected_podcast == Some(idx) {
                                self.selected_podcast = None;
                                self.podcast_episodes.clear();
                            }
                            return self.load_podcasts();
                        }
                        Err(e) => tracing::error!("Failed to remove podcast: {e}"),
                    }
                }
            }

            Message::RefreshPodcast(idx) => {
                if let Some(podcast) = self.podcasts.get(idx) {
                    return refresh_podcast_task(podcast.id, podcast.feed_url.clone());
                }
            }

            Message::RefreshAllPodcasts => {
                let tasks: Vec<_> = self
                    .podcasts
                    .iter()
                    .map(|p| refresh_podcast_task(p.id, p.feed_url.clone()))
                    .collect();
                return Task::batch(tasks);
            }

            Message::PodcastRefreshed(id, result) => match result {
                Ok(()) => {
                    let reload_task = self.load_podcasts();
                    let is_selected = self
                        .selected_podcast
                        .and_then(|i| self.podcasts.get(i))
                        .is_some_and(|p| p.id == id);
                    let episodes_task = if is_selected {
                        self.load_podcast_episodes(id)
                    } else {
                        Task::none()
                    };
                    return Task::batch([reload_task, episodes_task]);
                }
                Err(e) => {
                    return self.push_toast(widget::toaster::Toast::new(fl!(
                        "toast-podcast-refresh-failed",
                        reason = e
                    )));
                }
            },

            Message::PodcastEpisodesLoaded(podcast_id, episodes) => {
                let is_selected = self
                    .selected_podcast
                    .and_then(|i| self.podcasts.get(i))
                    .is_some_and(|p| p.id == podcast_id);
                if is_selected {
                    self.podcast_episodes = episodes;
                }
            }

            Message::PlayPodcastEpisode(idx) => {
                let Some(podcast_idx) = self.selected_podcast else {
                    return Task::none();
                };
                let Some(episode) = self.podcast_episodes.get(idx).cloned() else {
                    return Task::none();
                };
                let Some(podcast) = self.podcasts.get(podcast_idx) else {
                    return Task::none();
                };
                let track = Track {
                    id: -1,
                    path: if episode.downloaded_path.is_empty() {
                        PathBuf::new()
                    } else {
                        PathBuf::from(&episode.downloaded_path)
                    },
                    title: episode.title.clone(),
                    artist: podcast.title.clone(),
                    album_artist: podcast.title.clone(),
                    album: podcast.title.clone(),
                    genre: String::new(),
                    track_number: 0,
                    disc_number: 0,
                    year: 0,
                    duration: Duration::from_secs(episode.duration_secs.max(0) as u64),
                    bitrate: 0,
                    sample_rate: 0,
                    provider_id: Arc::from("podcast"),
                    source_uri: episode.enclosure_url.clone(),
                    is_favorite: false,
                    rating: None,
                    rg_track_gain: None,
                    rg_album_gain: None,
                };
                let play_task = self.play_track_list(vec![track], 0);
                self.current_podcast_episode_id = Some(episode.id);
                self.last_saved_podcast_position_secs = 0;
                if episode.position_ms > 0 {
                    let resume_at = Duration::from_millis(episode.position_ms as u64);
                    if let Some(player) = &mut self.player {
                        let _ = player.seek(resume_at);
                    }
                    self.playback_position = resume_at;
                }
                return play_task;
            }

            Message::TogglePodcastEpisodePlayed(idx) => {
                if let Some(episode) = self.podcast_episodes.get(idx).cloned() {
                    let new_played = !episode.played;
                    let result = open_online_store().and_then(|store| {
                        store.save_episode_position(episode.id, episode.position_ms, new_played)
                    });
                    match result {
                        Ok(()) => {
                            if let Some(ep) = self.podcast_episodes.get_mut(idx) {
                                ep.played = new_played;
                            }
                        }
                        Err(e) => tracing::error!("Failed to toggle played: {e}"),
                    }
                }
            }

            Message::DownloadEpisode(idx) => {
                let Some(episode) = self.podcast_episodes.get(idx).cloned() else {
                    return Task::none();
                };
                if !self.downloading_episodes.insert(episode.id) {
                    // Already downloading — ignore the duplicate request.
                    return Task::none();
                }
                return download_episode_task(episode);
            }

            Message::EpisodeDownloaded(episode_id, result) => {
                self.downloading_episodes.remove(&episode_id);
                match result {
                    Ok(path) => {
                        if let Some(ep) = self
                            .podcast_episodes
                            .iter_mut()
                            .find(|e| e.id == episode_id)
                        {
                            ep.downloaded_path = path;
                        }
                    }
                    Err(e) => {
                        return self.push_toast(widget::toaster::Toast::new(fl!(
                            "toast-episode-download-failed",
                            reason = e
                        )));
                    }
                }
            }

            Message::DeleteEpisodeDownload(idx) => {
                if let Some(episode) = self.podcast_episodes.get(idx).cloned() {
                    if !episode.downloaded_path.is_empty() {
                        let _ = std::fs::remove_file(&episode.downloaded_path);
                    }
                    match open_online_store()
                        .and_then(|store| store.set_episode_downloaded_path(episode.id, ""))
                    {
                        Ok(()) => {
                            if let Some(ep) = self.podcast_episodes.get_mut(idx) {
                                ep.downloaded_path = String::new();
                            }
                        }
                        Err(e) => tracing::error!("Failed to clear episode download: {e}"),
                    }
                }
            }

            Message::OnlineIconLoaded(url, bytes) => {
                if !bytes.is_empty() {
                    self.online_icons
                        .insert(url, widget::icon::from_raster_bytes(bytes));
                }
            }

            // -- Radio --
            Message::RadioSearchChanged(query) => {
                self.radio_search_query = query;
            }

            Message::RadioSearchSubmit => {
                let query = self.radio_search_query.trim().to_string();
                if query.is_empty() {
                    return Task::none();
                }
                self.radio_search_loading = true;
                return cosmic::task::future(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let client = HTTP_CLIENT.clone();
                        radio::search_stations(&client, &query)
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    cosmic::Action::App(Message::RadioSearchResults(result))
                });
            }

            Message::RadioDiscover => {
                self.radio_search_loading = true;
                return cosmic::task::future(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let client = HTTP_CLIENT.clone();
                        radio::popular_stations(&client, 50)
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    cosmic::Action::App(Message::RadioSearchResults(result))
                });
            }

            Message::RadioSearchResults(result) => {
                self.radio_search_loading = false;
                match result {
                    Ok(results) => {
                        let icon_urls: Vec<String> =
                            results.iter().map(|r| r.favicon.clone()).collect();
                        self.radio_search_results = results;
                        return self.load_online_icons(icon_urls);
                    }
                    Err(e) => {
                        return self.push_toast(widget::toaster::Toast::new(fl!(
                            "toast-radio-search-failed",
                            reason = e
                        )));
                    }
                }
            }

            Message::RadioAddNameChanged(name) => {
                self.radio_add_name = name;
            }

            Message::RadioAddUrlChanged(url) => {
                self.radio_add_url = url;
            }

            Message::AddRadioStation {
                name,
                stream_url,
                homepage,
                favicon_url,
                tags,
            } => {
                let result = open_online_store().and_then(|store| {
                    store.add_radio_station(&name, &stream_url, &homepage, &favicon_url, &tags)
                });
                match result {
                    Ok(_) => {
                        self.radio_add_name.clear();
                        self.radio_add_url.clear();
                        return self.load_radio_stations();
                    }
                    Err(e) => tracing::error!("Failed to add radio station: {e}"),
                }
            }

            Message::AddRadioFromSearch(idx) => {
                if let Some(result) = self.radio_search_results.get(idx).cloned() {
                    return cosmic::task::message(cosmic::Action::App(Message::AddRadioStation {
                        name: result.name,
                        stream_url: result.url,
                        homepage: result.homepage,
                        favicon_url: result.favicon,
                        tags: result.tags,
                    }));
                }
            }

            Message::RadioStationsLoaded(stations) => {
                let icon_urls: Vec<String> =
                    stations.iter().map(|s| s.favicon_url.clone()).collect();
                self.radio_stations = stations;
                return self.load_online_icons(icon_urls);
            }

            Message::RemoveRadioStation(idx) => {
                if let Some(station) = self.radio_stations.get(idx) {
                    let id = station.id;
                    match open_online_store().and_then(|store| store.remove_radio_station(id)) {
                        Ok(()) => return self.load_radio_stations(),
                        Err(e) => tracing::error!("Failed to remove radio station: {e}"),
                    }
                }
            }

            Message::PlayRadioStation(idx) => {
                if let Some(station) = self.radio_stations.get(idx) {
                    return resolve_and_play_radio(
                        station.name.clone(),
                        station.stream_url.clone(),
                    );
                }
            }

            Message::PlayRadioSearchResult(idx) => {
                if let Some(result) = self.radio_search_results.get(idx) {
                    return resolve_and_play_radio(result.name.clone(), result.url.clone());
                }
            }

            Message::RadioStreamResolved { name, result } => match result {
                Ok(resolved_url) => {
                    let track = Track {
                        id: -1,
                        path: PathBuf::new(),
                        title: name,
                        artist: String::new(),
                        album_artist: String::new(),
                        album: String::new(),
                        genre: String::new(),
                        track_number: 0,
                        disc_number: 0,
                        year: 0,
                        duration: Duration::ZERO,
                        bitrate: 0,
                        sample_rate: 0,
                        provider_id: Arc::from("radio"),
                        source_uri: resolved_url,
                        is_favorite: false,
                        rating: None,
                        rg_track_gain: None,
                        rg_album_gain: None,
                    };
                    return self.play_track_list(vec![track], 0);
                }
                Err(e) => {
                    return self.push_toast(widget::toaster::Toast::new(fl!(
                        "toast-radio-play-failed",
                        reason = e
                    )));
                }
            },

            // -- Convert / transcode / rip --
            Message::ConvertAddFiles => {
                return cosmic::task::future(async {
                    let result = async {
                        use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

                        let mut filter = FileFilter::new("Audio, Video & CUE Files");
                        for ext in crate::player::engine::decoder::SUPPORTED_EXTENSIONS
                            .iter()
                            .chain(["cue", "mkv", "mov", "avi"].iter())
                        {
                            filter = filter.glob(&format!("*.{ext}"));
                        }

                        let selected = SelectedFiles::open_file()
                            .title("Select Audio/Video Files or a CUE Sheet")
                            .multiple(true)
                            .modal(true)
                            .filter(filter)
                            .send()
                            .await
                            .map_err(|e| format!("Portal request failed: {e}"))?
                            .response()
                            .map_err(|e| format!("Portal response failed: {e}"))?;

                        let mut paths = Vec::new();
                        for uri in selected.uris() {
                            let uri_str = uri.as_str();
                            let path = uri_str
                                .strip_prefix("file://")
                                .ok_or_else(|| format!("Not a local file URI: {uri_str}"))
                                .and_then(|encoded| {
                                    urlencoding::decode(encoded)
                                        .map(|d| PathBuf::from(d.as_ref()))
                                        .map_err(|e| format!("Could not decode URI path: {e}"))
                                })?;
                            paths.push(path);
                        }
                        if paths.is_empty() {
                            Err("No files selected".to_string())
                        } else {
                            Ok(paths)
                        }
                    }
                    .await;
                    cosmic::Action::App(Message::ConvertFilesPicked(result))
                });
            }

            Message::ConvertFilesPicked(result) => match result {
                Ok(paths) => {
                    let format = OutputFormat::ALL[self.convert_format_index];
                    let target_rate = convert::SAMPLE_RATE_OPTIONS[self.convert_rate_index];
                    for path in paths {
                        let kind = if path
                            .extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
                        {
                            JobKind::CueSplit
                        } else {
                            JobKind::Convert
                        };
                        let id = self.convert_next_id;
                        self.convert_next_id += 1;
                        self.convert_jobs.push(ConvertJob::new(
                            id,
                            path,
                            kind,
                            format,
                            target_rate,
                            self.convert_out_dir.clone(),
                        ));
                    }
                }
                Err(e) => tracing::warn!("convert: file picker failed: {e}"),
            },

            Message::ConvertPickOutputDir => {
                return cosmic::task::future(async {
                    let result = async {
                        use ashpd::desktop::file_chooser::SelectedFiles;

                        let selected = SelectedFiles::open_file()
                            .title("Select Output Directory")
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
                                .and_then(|encoded| {
                                    urlencoding::decode(encoded)
                                        .map(|d| PathBuf::from(d.as_ref()))
                                        .map_err(|e| format!("Could not decode URI path: {e}"))
                                })
                        } else {
                            Err("No directory selected".to_string())
                        }
                    }
                    .await;
                    cosmic::Action::App(Message::ConvertOutputDirPicked(result))
                });
            }

            Message::ConvertOutputDirPicked(result) => match result {
                Ok(path) => self.convert_out_dir = path,
                Err(e) => tracing::warn!("convert: output directory picker failed: {e}"),
            },

            Message::ConvertFormatSelected(index) => self.convert_format_index = index,

            Message::ConvertRateSelected(index) => self.convert_rate_index = index,

            Message::ConvertStart => {
                let mut tasks = Vec::new();
                for job in &mut self.convert_jobs {
                    if job.state == JobState::Queued {
                        job.state = JobState::Running;
                        let job_clone = job.clone();
                        let semaphore = Arc::clone(&self.convert_semaphore);
                        tasks.push(cosmic::task::future(async move {
                            let (id, state) = run_job(job_clone, semaphore).await;
                            cosmic::Action::App(Message::ConvertJobFinished(id, state))
                        }));
                    }
                }
                if !tasks.is_empty() {
                    return Task::batch(tasks);
                }
            }

            Message::ConvertJobFinished(id, state) => {
                if let Some(job) = self.convert_jobs.iter_mut().find(|j| j.id == id) {
                    job.state = state;
                }
            }

            Message::ConvertCancelJob(id) => {
                if let Some(job) = self.convert_jobs.iter().find(|j| j.id == id) {
                    job.request_cancel();
                }
            }

            Message::ConvertClearFinished => {
                self.convert_jobs
                    .retain(|j| matches!(j.state, JobState::Queued | JobState::Running));
            }

            Message::ConvertTick => {}
            Message::Quit => {
                return cosmic::iced::exit();
            }
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
                        MprisCommand::Pause | MprisCommand::Stop => {
                            // Lyra has no distinct "stop" state; degrade Stop
                            // to Pause, which is the closest real behavior.
                            if playing {
                                self.update(Message::TogglePlayback)
                            } else {
                                Task::none()
                            }
                        }
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
                        MprisCommand::Raise => {
                            tracing::debug!(
                                "MPRIS: Raise requested (no-op, Lyra has no window-raise hook)"
                            );
                            Task::none()
                        }
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
                    Shortcut::Stop => {
                        if let Some(player) = &mut self.player {
                            match player.stop() {
                                Ok(()) => self.playback_position = Duration::ZERO,
                                Err(e) => tracing::error!("Stop failed: {e}"),
                            }
                        }
                    }
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
                    Shortcut::ToggleExpanded => {
                        return if self.expand_progress > 0.0 || self.expand_target.is_some() {
                            self.update(Message::CollapseNowPlaying)
                        } else {
                            self.update(Message::ExpandNowPlaying)
                        };
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
        // Reset sub-view selections when switching pages
        self.selected_album = None;
        self.selected_artist = None;
        self.selected_playlist = None;
        self.selected_genre = None;
        self.selected_smart_playlist = None;
        self.smart_playlist_editor = None;

        // Collapse expanded now-playing view when navigating
        if self.expand_progress > 0.0 || self.expand_target.is_some() {
            self.lyrics_overlay_active = false;
            self.expand_target = Some(0.0);
            self.expand_anim_start = Some(std::time::Instant::now());
            self.expand_anim_from = self.expand_progress;
        }

        // Lazy-load data for Playlists and Genres pages
        let page = self.nav.active_data::<Page>().cloned();
        let page_task = match page {
            Some(Page::Playlists) => self.load_playlists(),
            Some(Page::SmartPlaylists) => {
                let db_path = dirs::data_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("lyra")
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
            Some(Page::Genres) => self.load_genres(),
            Some(Page::Folders) => {
                self.folder_state
                    .set_tree(crate::views::folders::FolderTree::build(&self.all_tracks));
                Task::none()
            }
            Some(Page::Podcasts) => self.load_podcasts(),
            Some(Page::Radio) => self.load_radio_stations(),
            _ => Task::none(),
        };

        let title_task = self.update_title();
        Task::batch([title_task, page_task])
    }
}
