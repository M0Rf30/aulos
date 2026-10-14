// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::tasks::session_effect;
use super::{AppModel, ContextPage, Message, Page, SEARCH_INPUT_ID};
use aulos_core::library::artist_tags::parse_delimiters_input;
use aulos_core::session::{QueuePlacement, SEEK_STEP, VOLUME_STEP};
use aulos_core::tagger::taggable_tracks;
use cosmic::Application;
use cosmic::prelude::*;
use cosmic::widget::{self, nav_bar};
use std::path::PathBuf;

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

            // -- Properties drawer --
            Message::ShowTrackProperties(track) => return self.open_track_properties(track),
            Message::Properties(msg) => return self.update_properties(msg),

            Message::TextInputFocused(focused) => {
                self.text_input_focused = focused;
            }

            // -- Library search --
            Message::LibrarySearchChanged(query) => return self.set_library_search(query),

            Message::ToggleLibrarySearch => {
                self.search_active = !self.search_active;
                if self.search_active {
                    return cosmic::widget::text_input::focus(widget::Id::new(SEARCH_INPUT_ID))
                        .map(cosmic::Action::App);
                }
                self.session.clear_search();
            }

            Message::ClearLibrarySearch => {
                self.session.clear_search();
                self.search_active = false;
            }

            Message::CloseToast(id) => {
                self.toasts.remove(id);
            }

            // -- Library --
            Message::ScanLibrary => {
                let reload = self.session.scan_library();
                return self.session_task(reload);
            }

            // -- Everything the backend session reports --
            Message::Session(event) => {
                let update = self.session.apply_session_event(event);
                return self.session_task(update);
            }

            // -- Playback --
            Message::TogglePlayback => {
                let update = self.session.toggle_playback();
                return self.session_task(update);
            }

            Message::NextTrack => {
                let update = self.session.next_track();
                return self.session_task(update);
            }

            Message::PreviousTrack => {
                let update = self.session.previous_track();
                return self.session_task(update);
            }

            Message::SeekPreview(fraction) => {
                // Visual-only: store the preview fraction so the slider and
                // time label reflect the drag position without touching the
                // audio backend. This avoids the rapid seek storm that
                // causes stuttering, snapback, and restarts.
                self.seeking_preview = Some(fraction);
                self.session.set_seeking(true);
            }

            Message::SeekCommit => {
                // Mouse released on the seek slider — perform the actual seek.
                if let Some(fraction) = self.seeking_preview.take() {
                    self.session.set_seeking(false);
                    let update = self.session.seek_fraction(fraction);
                    return self.session_task(update);
                }
            }

            // Live-apply only (the slider drags through this): the level is
            // persisted by `VolumeCommit` once the gesture settles.
            Message::SetVolume(vol) => {
                let update = self.session.set_volume(vol);
                return self.session_task(update);
            }

            // Persist the master volume. Emitted on slider release.
            Message::VolumeCommit => self.session.commit_volume(),

            Message::ToggleShuffle => {
                let update = self.session.toggle_shuffle();
                return self.session_task(update);
            }

            Message::CycleRepeat => {
                let update = self.session.cycle_repeat();
                return self.session_task(update);
            }

            Message::Stop => {
                let update = self.session.stop();
                return self.session_task(update);
            }

            Message::QueueJump(idx) => {
                let update = self.session.queue_jump(idx);
                return self.session_task(update);
            }

            Message::QueueRemove(idx) => {
                let update = self.session.queue_remove(idx);
                return self.session_task(update);
            }

            Message::QueueMove { from, to } => self.session.queue_move(from, to),

            Message::QueueClear => self.session.queue_clear(),

            Message::PlayNext(tracks) => {
                let update = self.session.queue_tracks(tracks, QueuePlacement::Next);
                return self.session_task(update);
            }

            Message::AddToQueue(tracks) => {
                let update = self.session.queue_tracks(tracks, QueuePlacement::End);
                return self.session_task(update);
            }

            Message::QueueFrom { source, next } => {
                let update = self.session.queue_from(source, next);
                return self.session_task(update);
            }

            // -- Track selection --
            Message::PlayTrackIndex(index) => {
                let update = self.session.play_library_track(index);
                return self.session_task(update);
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
                            .filter_map(|&i| self.session.library.tracks.get(i).cloned())
                            .collect();
                        return self.play_track_list(tracks, position);
                    }
                    return self.play_track_list(self.session.library.tracks.clone(), index);
                }
                crate::views::folders::FolderMessage::PlayFolder => {
                    let tracks = self.current_folder_tracks();
                    if !tracks.is_empty() {
                        return self.play_track_list(tracks, 0);
                    }
                }
                crate::views::folders::FolderMessage::QueueFolder => {
                    let tracks = self.current_folder_tracks();
                    let update = self.session.queue_tracks(tracks, QueuePlacement::End);
                    return self.session_task(update);
                }
                crate::views::folders::FolderMessage::ToggleFavorite(id) => {
                    return self.update(Message::ToggleFavorite(id));
                }
                crate::views::folders::FolderMessage::SetRating(id, r) => {
                    return self.update(Message::SetRating(id, r));
                }
                crate::views::folders::FolderMessage::ShowProperties(track) => {
                    return self.open_track_properties(track);
                }
                crate::views::folders::FolderMessage::Tag(track, action) => {
                    return self.open_in_tagger(vec![track], action);
                }
            },

            Message::PlayAlbum(album_idx) => {
                let update = self.session.play_album(album_idx, 0);
                return self.session_task(update);
            }

            Message::PlayAlbumTrack(album_idx, track_idx) => {
                let update = self.session.play_album(album_idx, track_idx);
                return self.session_task(update);
            }

            Message::PlayArtistAlbum(artist_idx, album_idx) => {
                let update = self.session.play_artist_album(artist_idx, album_idx, 0);
                return self.session_task(update);
            }

            Message::PlayArtistTrack(artist_idx, album_idx, track_idx) => {
                let update = self
                    .session
                    .play_artist_album(artist_idx, album_idx, track_idx);
                return self.session_task(update);
            }

            Message::ShowAlbumProperties(album_idx) => {
                if let Some(album) = self.session.library.albums.get(album_idx).cloned() {
                    return self.open_album_properties(album);
                }
            }

            Message::ShowArtistAlbumProperties(artist_idx, album_idx) => {
                if let Some(album) = self
                    .session
                    .library
                    .artists
                    .get(artist_idx)
                    .and_then(|artist| artist.albums.get(album_idx))
                    .cloned()
                {
                    return self.open_album_properties(album);
                }
            }

            Message::TagTrack(track, action) => return self.open_in_tagger(vec![track], action),

            Message::TagAlbum(album_idx, action) => {
                if let Some(album) = self.session.library.albums.get(album_idx) {
                    return self.open_in_tagger(taggable_tracks(&album.tracks), action);
                }
            }

            Message::TagArtistAlbum(artist_idx, album_idx, action) => {
                if let Some(album) = self
                    .session
                    .library
                    .artists
                    .get(artist_idx)
                    .and_then(|artist| artist.albums.get(album_idx))
                {
                    return self.open_in_tagger(taggable_tracks(&album.tracks), action);
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
                if let Some(name) = self
                    .session
                    .library
                    .artists
                    .get(idx)
                    .map(|a| a.name.clone())
                {
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

            Message::DetailArtReady(key, blurred, accent) => {
                self.apply_detail_art(key, blurred, accent);
            }

            Message::Navigate(route) => {
                return self.navigate(route);
            }

            Message::ToggleArtistBioExpanded => {
                self.artist_bio_expanded = !self.artist_bio_expanded;
            }

            Message::SortSongs(field) => {
                self.songs_sort = field;
                self.session.sort_tracks(field);
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

            Message::ToggleFavorite(track_id) => return self.toggle_favorite(track_id),

            Message::SettingsSearch(query) => self.settings_search = query,
            Message::Scrobble(msg) => return self.handle_scrobble_message(msg),

            Message::SetRating(track_id, rating) => return self.set_rating(track_id, rating),

            Message::AddToPlaylist(track_source_uri, playlist_id) => {
                return session_effect(self.session.add_to_playlist(playlist_id, track_source_uri));
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
                self.session.playback.load_embedded_lyrics();

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
                let effect = self.session.playback.fetch_lyrics_online();
                return session_effect(effect);
            }

            // -- Equalizer --
            Message::EqSetBand(index, value) => self.session.eq_set_band(index, value),

            Message::EqSetPreset(preset) => self.session.eq_set_preset(preset),

            Message::EqToggle(enabled) => self.session.eq_set_enabled(enabled),

            Message::EqSetPreamp(value) => self.session.eq_set_preamp(value),

            Message::EqSelectPreset(name) => self.session.eq_select_preset(name),

            Message::EqSavePreset => self.session.eq_save_preset(),

            Message::EqSavePresetAs(name) => self.session.eq_save_preset_as(name),

            Message::EqDeletePreset => self.session.eq_delete_preset(),

            Message::EqSaveAsNameChanged(name) => {
                self.save_as_name = name;
            }

            Message::EqResetPreset => self.session.eq_reset(),

            // -- AutoEQ --
            Message::AutoEQSearchChanged(query) => {
                self.autoeq_search = query;
            }

            Message::FetchAutoEQIndex => {
                return session_effect(self.session.fetch_autoeq_index());
            }

            Message::EqSelectAutoEQ(path) => {
                return session_effect(self.session.fetch_autoeq_profile(path));
            }

            Message::SetGridScale(scale) => {
                crate::views::common::set_grid_scale(scale);
                self.session.config.config.grid_scale = crate::views::common::grid_scale();
                self.session.config.save();
            }

            Message::ToggleAlbumsViewMode => {
                self.session.config.config.albums_view_mode =
                    self.session.config.config.albums_view_mode.toggled();
                self.session.config.save();
            }
            Message::ToggleArtistsViewMode => {
                self.session.config.config.artists_view_mode =
                    self.session.config.config.artists_view_mode.toggled();
                self.session.config.save();
            }
            Message::ToggleGenresViewMode => {
                self.session.config.config.genres_view_mode =
                    self.session.config.config.genres_view_mode.toggled();
                self.session.config.save();
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

            Message::DirPickerResult(result) => match result {
                Ok(path) => {
                    if self.session.add_music_dir(path) {
                        // Trigger a library rescan.
                        return cosmic::task::message(cosmic::Action::App(Message::ScanLibrary));
                    }
                }
                Err(e) => {
                    tracing::warn!("Directory picker failed: {e}");
                }
            },

            Message::RemoveMusicDir(index) => {
                if self.session.remove_music_dir(index) {
                    // Trigger a library rescan.
                    return cosmic::task::message(cosmic::Action::App(Message::ScanLibrary));
                }
            }

            Message::SetSplitArtistTags(enabled) => self.session.set_split_artist_tags(enabled),

            Message::SetExperimentalConverter(enabled) => {
                self.session.config.config.experimental_converter = enabled;
                self.session.config.save();
                self.set_convert_nav_entry(enabled);
            }

            Message::SetAcoustidApiKey(key) => {
                self.session.config.config.acoustid_api_key = key;
                self.session.config.save();
            }

            Message::EditAcoustidUserKey(text) => {
                self.acoustid_user_key_input = text;
            }

            Message::SaveAcoustidUserKey => {
                let key = self.acoustid_user_key_input.trim().to_string();
                self.acoustid_user_key_input.clear();
                return session_effect(self.session.config.save_acoustid_user_key(key));
            }

            Message::SetTaggerUseCaaCover(enabled) => {
                self.session.config.config.tagger_use_caa_cover = enabled;
                self.session.config.save();
            }

            Message::SetFetchArtistInfo(enabled) => {
                self.session.config.config.fetch_artist_info = enabled;
                self.session.config.save();
                if enabled && self.nav.active_data::<Page>() == Some(&Page::Artists) {
                    return self.load_artist_info_for_visible();
                }
            }

            Message::ArtistTagDelimitersInputChanged(text) => {
                self.artist_tag_delimiters_input = text;
            }

            Message::SubmitArtistTagDelimiters(text) => {
                self.artist_tag_delimiters_input = text.clone();
                self.session
                    .set_artist_tag_delimiters(parse_delimiters_input(&text));
            }

            Message::ResetArtistTagDelimiters => {
                self.session.set_artist_tag_delimiters(
                    aulos_core::library::artist_tags::DEFAULT_DELIMITERS
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                );
                self.artist_tag_delimiters_input =
                    self.session.config.config.artist_tag_delimiters.join(" | ");
            }

            // -- Provider switching --
            Message::SwitchProvider(index) => return self.switch_provider(index),

            // -- MPD server configuration --
            Message::MpdAddServer => self.session.providers.add_mpd_card(),

            Message::MpdEditName(i, v) => {
                if let Some(state) = self.session.providers.mpd_edit_states.get_mut(i) {
                    state.name = v;
                }
            }

            Message::MpdEditHost(i, v) => {
                if let Some(state) = self.session.providers.mpd_edit_states.get_mut(i) {
                    state.host = v;
                }
            }

            Message::MpdEditPort(i, v) => {
                if let Some(state) = self.session.providers.mpd_edit_states.get_mut(i) {
                    state.port = v;
                }
            }

            Message::MpdEditPassword(i, v) => {
                if let Some(state) = self.session.providers.mpd_edit_states.get_mut(i) {
                    state.password = v;
                }
            }

            Message::MpdSaveServer(i) => {
                let reload = self.session.save_mpd_server(i);
                return self.session_task(reload);
            }

            Message::MpdRemoveServer(i) => {
                let reload = self.session.remove_mpd_server(i);
                return self.session_task(reload);
            }

            Message::MpdTestConnection(i) => {
                return session_effect(self.session.providers.test_mpd_connection(i));
            }

            // -- Subsonic server configuration --
            Message::SubsonicAddServer => self.session.providers.add_subsonic_card(),

            Message::SubsonicEditName(i, v) => {
                if let Some(state) = self.session.providers.subsonic_edit_states.get_mut(i) {
                    state.name = v;
                }
            }

            Message::SubsonicEditUrl(i, v) => {
                if let Some(state) = self.session.providers.subsonic_edit_states.get_mut(i) {
                    state.url = v;
                }
            }

            Message::SubsonicEditUsername(i, v) => {
                if let Some(state) = self.session.providers.subsonic_edit_states.get_mut(i) {
                    state.username = v;
                }
            }

            Message::SubsonicEditPassword(i, v) => {
                if let Some(state) = self.session.providers.subsonic_edit_states.get_mut(i) {
                    state.password = v;
                }
            }

            Message::SubsonicToggleCerts(i, v) => {
                if let Some(state) = self.session.providers.subsonic_edit_states.get_mut(i) {
                    state.accept_invalid_certs = v;
                }
            }

            Message::SubsonicSaveServer(i) => {
                let reinit = self.session.save_subsonic_server(i);
                return self.session_task(reinit);
            }

            Message::SubsonicRemoveServer(i) => {
                let reinit = self.session.remove_subsonic_server(i);
                return self.session_task(reinit);
            }

            Message::SubsonicTestConnection(i) => {
                return session_effect(self.session.providers.test_subsonic_connection(i));
            }

            Message::SubsonicTranscodingBitrate(i, bitrate) => {
                if let Some(state) = self.session.providers.subsonic_edit_states.get_mut(i) {
                    state.transcoding_max_bitrate = bitrate;
                }
            }
            Message::SubsonicTranscodingFormat(i, format) => {
                if let Some(state) = self.session.providers.subsonic_edit_states.get_mut(i) {
                    state.transcoding_format = format;
                }
            }

            // Task 113: Wire crossfade config change to player and MPD
            Message::SetCrossfade(secs) => {
                let effect = self.session.set_crossfade(secs);
                return session_effect(effect);
            }

            // Task 114: Wire replay gain mode change to player and MPD
            Message::SetReplayGainMode(mode) => {
                let effect = self.session.set_replay_gain_mode(mode);
                return session_effect(effect);
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
                let current_key = self
                    .session
                    .playback
                    .current_track
                    .as_ref()
                    .map(aulos_core::library::Track::cover_key);
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
                    .send(aulos_core::visualizer::VizCommand::NextPreset);
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
            Message::VisualizerFailed(reason) => {
                // Fall back to the blurred-cover backdrop and tell the user
                // why, instead of leaving a permanently black panel.
                self.visualizer_active = false;
                self.viz_browser_open = false;
                let fs_task = self.exit_viz_fullscreen();
                if self.blurred_cover.is_none() {
                    self.blurred_cover_key = None;
                }
                let toast = self.push_toast(widget::toaster::Toast::new(aulos_core::fl!(
                    "viz-init-failed",
                    reason = reason
                )));
                return Task::batch([fs_task, self.maybe_update_blurred_cover(), toast]);
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
                        let dirs = aulos_core::visualizer::presets::preset_search_dirs(
                            dirs::data_dir().map(|d| d.join("lactea").join("presets")),
                        );
                        let entries = tokio::task::spawn_blocking(move || {
                            aulos_core::visualizer::presets::scan_presets(&dirs)
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
                    .send(aulos_core::visualizer::VizCommand::LoadPreset(path));
            }

            #[cfg(feature = "visualizer")]
            Message::SetVizLocked(locked) => {
                self.viz_locked = locked;
                let _ = self
                    .viz_cmd_tx
                    .send(aulos_core::visualizer::VizCommand::SetLocked(locked));
            }

            #[cfg(feature = "visualizer")]
            Message::SetVizBeatSensitivity(sensitivity) => {
                self.viz_beat_sensitivity = sensitivity;
                let _ =
                    self.viz_cmd_tx
                        .send(aulos_core::visualizer::VizCommand::SetBeatSensitivity(
                            sensitivity,
                        ));
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
                    .session
                    .playlists
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
                return session_effect(self.session.create_playlist(name));
            }

            Message::DeletePlaylist(idx) => {
                return session_effect(self.session.delete_playlist(idx));
            }

            Message::RenamePlaylist(idx, new_name) => {
                return session_effect(self.session.rename_playlist(idx, new_name));
            }

            Message::PlayPlaylist(idx) => {
                if let Some(tracks) = self.session.playlists.playlist_tracks(idx) {
                    return self.play_track_list(tracks.to_vec(), 0);
                }
            }

            Message::PlayPlaylistTrack(pl_idx, track_idx) => {
                if let Some(playlist) = self.session.playlists.playlists.get(pl_idx) {
                    return self.play_track_list(playlist.tracks.clone(), track_idx);
                }
            }

            Message::RemovePlaylistTrack(_pl_idx, _track_idx) => {
                return session_effect(self.session.remove_playlist_track());
            }

            Message::NewPlaylistNameChanged(name) => {
                self.new_playlist_name = name;
            }

            Message::RenamePlaylistInput(_idx, text) => {
                self.rename_playlist_input = text;
            }

            // -- Playlist import/export, mini player, compilations, album filters --
            Message::PlaylistIo(msg) => return self.update_playlist_io(msg),
            Message::Mini(msg) => return self.update_mini_player(msg),
            Message::AlbumFilter(msg) => msg.apply(&mut self.extras.album_filter),
            Message::SetShowCompilationsInArtists(show) => {
                self.session.set_show_compilations_in_artists(show);
            }
            Message::SetM3uRelativePaths(relative) => {
                self.session.config.config.m3u_relative_paths = relative;
                self.session.config.save();
            }

            // -- Smart playlists view --
            Message::SmartPlaylists(msg) => {
                use crate::views::smart_playlists::SmartPlaylistMessage;

                match msg {
                    SmartPlaylistMessage::Navigate(route) => {
                        return self.navigate(route);
                    }
                    SmartPlaylistMessage::ShowProperties(track) => {
                        return self.open_track_properties(track);
                    }
                    SmartPlaylistMessage::Tag(track, action) => {
                        return self.open_in_tagger(vec![track], action);
                    }
                    // List view
                    SmartPlaylistMessage::New => self.session.playlists.new_smart_playlist(),
                    SmartPlaylistMessage::Edit(idx) => {
                        self.session.playlists.edit_smart_playlist(idx);
                    }
                    SmartPlaylistMessage::EditorCancel => {
                        self.session.playlists.cancel_smart_editor();
                    }
                    SmartPlaylistMessage::EditorSave => {
                        return session_effect(self.session.playlists.save_smart_editor());
                    }
                    SmartPlaylistMessage::Delete(idx) => {
                        if let Some(effect) = self.session.playlists.delete_smart_playlist(idx) {
                            if self.selected_smart_playlist == Some(idx) {
                                self.selected_smart_playlist = None;
                            }
                            return session_effect(effect);
                        }
                    }

                    // Selecting / playing a saved smart playlist
                    SmartPlaylistMessage::Select(idx) => {
                        self.selected_smart_playlist = Some(idx);
                        return session_effect(self.session.playlists.select_smart_playlist(idx));
                    }
                    SmartPlaylistMessage::BackToList => {
                        self.selected_smart_playlist = None;
                        self.session.playlists.cancel_smart_editor();
                    }
                    SmartPlaylistMessage::Play(idx) => {
                        return session_effect(self.session.playlists.resolve_smart_playlist(idx));
                    }
                    SmartPlaylistMessage::Export(idx) => {
                        return self
                            .update_playlist_io(super::playlist_io::PlaylistIo::ExportSmart(idx));
                    }
                    SmartPlaylistMessage::PlayTrack(idx) => {
                        if !self.session.playlists.smart_playlist_tracks.is_empty() {
                            return self.play_track_list(
                                self.session.playlists.smart_playlist_tracks.clone(),
                                idx,
                            );
                        }
                    }

                    // Detail view: favorite/rating on a resolved track
                    SmartPlaylistMessage::ToggleFavorite(track_id) => {
                        if let Some(track) = self
                            .session
                            .playlists
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
                            .session
                            .playlists
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
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_name(name);
                        }
                    }
                    SmartPlaylistMessage::EditorMatchModeChanged(i) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_match_mode(i);
                        }
                    }
                    SmartPlaylistMessage::EditorAddRule => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.add_rule();
                        }
                    }
                    SmartPlaylistMessage::EditorRemoveRule(i) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.remove_rule(i);
                        }
                    }
                    SmartPlaylistMessage::EditorRuleFieldChanged(i, f) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_rule_field(i, f);
                        }
                    }
                    SmartPlaylistMessage::EditorRuleOpChanged(i, o) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_rule_op(i, o);
                        }
                    }
                    SmartPlaylistMessage::EditorRuleValueChanged(i, v) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_rule_value(i, v);
                        }
                    }
                    SmartPlaylistMessage::EditorRuleValue2Changed(i, v) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_rule_value2(i, v);
                        }
                    }
                    SmartPlaylistMessage::EditorOrderByChanged(i) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_order_by(i);
                        }
                    }
                    SmartPlaylistMessage::EditorOrderDescToggled(v) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_order_desc(v);
                        }
                    }
                    SmartPlaylistMessage::EditorLimitToggled(v) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_limit_enabled(v);
                        }
                    }
                    SmartPlaylistMessage::EditorLimitChanged(v) => {
                        if let Some(state) = &mut self.session.playlists.smart_playlist_editor {
                            state.set_limit_input(v);
                        }
                    }
                }
            }

            // -- Genres view --
            Message::SelectGenre(idx) => return self.select_genre(idx),

            Message::BackToGenreGrid => {
                self.session.library.genre_tracks.clear();
                if let Some(task) = self.go_back() {
                    return task;
                }
                self.session.library.selected_genre = None;
            }

            Message::PlayGenreTrack(idx) => {
                let update = self.session.play_genre_track(idx);
                return self.session_task(update);
            }

            Message::ShuffleGenre => {
                let update = self.session.shuffle_genre();
                return self.session_task(update);
            }

            // -- Podcasts --
            Message::Home(msg) => return self.update_home(msg),
            Message::Startup(msg) => return self.update_startup(msg),
            Message::Podcast(msg) => return self.update_podcast(msg),

            // -- Radio --
            Message::Radio(msg) => return self.update_radio(msg),

            // -- Convert / transcode / rip --
            Message::Convert(msg) => return self.update_convert(msg),
            Message::ConvertPicked(picked) => return self.update_convert_picked(picked),

            // -- Tag editor --
            Message::Tagger(msg) => return self.update_tagger(msg),
            Message::TaggerEvent(event) => return self.update_tagger_event(event),
            Message::Quit => {
                self.session.config.flush();
                return cosmic::iced::exit();
            }
            Message::Playback(msg) => return self.update_playback_extras(msg),
            Message::Mpris(event) => match event {
                aulos_core::mpris::MprisEvent::Ready(handle) => {
                    self.session.playback.mpris = Some(handle);
                    return self.publish_mpris();
                }
                aulos_core::mpris::MprisEvent::Command(cmd) => {
                    use aulos_core::mpris::MprisCommand;

                    // The snapshot is republished after every command so
                    // clients observe the result immediately; without this,
                    // properties changed while stopped or paused would stay
                    // stale until the next playback tick, which only fires
                    // while playing. Playback commands live in the session;
                    // window and process commands are the UI's.
                    let task = match cmd {
                        MprisCommand::Raise => self.raise_window(),
                        MprisCommand::Quit => self.update(Message::Quit),
                        MprisCommand::OpenUri(uri) => match aulos_core::file_uri_to_path(&uri) {
                            Some(path) => self.update(Message::OpenFiles(vec![path])),
                            None => {
                                tracing::warn!("MPRIS OpenUri: unsupported or invalid URI: {uri}");
                                Task::none()
                            }
                        },
                        cmd => {
                            let update = self.session.apply_mpris_command(cmd);
                            self.session_task(update)
                        }
                    };
                    let mpris_task = self.publish_mpris();
                    return Task::batch([task, mpris_task]);
                }
            },
            Message::OpenFiles(paths) => {
                return self.open_files(paths);
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
                        let update = self
                            .session
                            .seek_by(shortcut == Shortcut::SeekForward, SEEK_STEP);
                        return self.session_task(update);
                    }
                    Shortcut::VolumeUp | Shortcut::VolumeDown => {
                        let delta = if shortcut == Shortcut::VolumeUp {
                            VOLUME_STEP
                        } else {
                            -VOLUME_STEP
                        };
                        let update = self.session.nudge_volume(delta);
                        return self.session_task(update);
                    }
                    Shortcut::Mute => {
                        let update = self.session.toggle_mute();
                        return self.session_task(update);
                    }
                    Shortcut::ToggleShuffle => return self.update(Message::ToggleShuffle),
                    Shortcut::CycleRepeat => return self.update(Message::CycleRepeat),
                    Shortcut::ToggleFavorite => {
                        if let Some(id) = self
                            .session
                            .playback
                            .current_track
                            .as_ref()
                            .map(|t| t.id.to_string())
                        {
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
        self.session.library.selected_genre = None;
        self.selected_smart_playlist = None;
        self.session.playlists.cancel_smart_editor();
        self.session.podcasts.close_show();

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
                session_effect(self.session.playlists.load_smart_playlists())
            }
            Some(Page::Home) => self.enter_home(),
            Some(Page::Genres) => self.load_genres(),
            Some(Page::Artists) => self.load_artist_info_for_visible(),
            Some(Page::Folders) => {
                if !self.folder_state.is_populated()
                    || self.session.library.folder_tree_gen
                        != Some(self.session.library.generation())
                {
                    self.folder_state.set_tree(
                        aulos_core::library::folder_tree::FolderTree::build(
                            &self.session.library.tracks,
                        ),
                    );
                    self.session.library.folder_tree_gen = Some(self.session.library.generation());
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
        use aulos_core::visualizer::browser as rows_model;
        let rows = rows_model::build_rows(&self.viz_preset_entries, &self.viz_preset_search);
        let y = rows_model::current_row(
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
