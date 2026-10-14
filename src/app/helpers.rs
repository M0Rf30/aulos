// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::tasks::session_effect;
use super::{AppModel, Message};
use aulos_core::fl;
use aulos_core::library::{Album, Track};
use aulos_core::session::{
    ConvertNotice, PlaybackNotice, PodcastNotice, RadioNotice, SessionNotice, SessionUpdate,
};
use cosmic::prelude::*;
use cosmic::widget;
use std::path::PathBuf;

impl AppModel {
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

    /// Presents a [`SessionUpdate`]: applies what changed in the session to
    /// the UI's presentation state (image handles, derived views), shows
    /// the notices as toasts and runs the effect, whose events come back
    /// as `Message::Session`. The one path every session result takes,
    /// whether it is the answer to an action or to an asynchronous event.
    pub(super) fn session_task(
        &mut self,
        update: impl Into<SessionUpdate>,
    ) -> Task<cosmic::Action<Message>> {
        let SessionUpdate {
            effect,
            notices,
            changes,
        } = update.into();
        let mut tasks = Vec::new();

        let mut refresh_cover = changes.library_updated || changes.cover_bytes_loaded;
        self.apply_cover_delta(changes.covers);
        if changes.library_loaded {
            // The folder tree is derived from the whole library; rebuild it
            // only when it is on screen or was already built.
            self.rebuild_folder_tree_if_shown();
        }
        if changes.artist_photos_cleared {
            self.artist_photos.clear();
        }
        for photo in changes.artist_photos {
            self.artist_photos.insert(
                photo.name,
                widget::image::Handle::from_rgba(photo.width, photo.height, photo.rgba),
            );
        }
        for icon in changes.online_icons {
            self.online_icons.insert(
                icon.url,
                widget::icon::from_raster_pixels(icon.width, icon.height, icon.rgba),
            );
        }
        if changes.playlist_created {
            self.new_playlist_name.clear();
        }
        if changes.playlist_deleted {
            self.selected_playlist = None;
        }
        if changes.artist_split_changed {
            self.artist_tag_delimiters_input =
                self.session.config.config.artist_tag_delimiters.join(" | ");
        }
        if changes.track_changed {
            #[cfg(feature = "visualizer")]
            {
                self.viz_metadata_opacity = 1.0;
            }
            refresh_cover = true;
        }
        if refresh_cover {
            tasks.push(self.maybe_update_blurred_cover());
        }

        for notice in notices {
            self.reset_forms_for(&notice);
            tasks.push(self.push_toast(notice_toast(notice)));
        }
        tasks.push(session_effect(effect));
        Task::batch(tasks)
    }

    /// Republishes the playback state to MPRIS.
    pub(super) fn publish_mpris(&self) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.publish_mpris())
    }

    /// Start playback from the given queue at `start_index`.
    pub(super) fn play_track_list(
        &mut self,
        tracks: Vec<Track>,
        start_index: usize,
    ) -> Task<cosmic::Action<Message>> {
        let update = self.session.play_tracks(tracks, start_index);
        self.session_task(update)
    }

    /// Every track under the folder view's current directory, recursively,
    /// in path order. Shared by "play folder" and "add folder to queue".
    pub(super) fn current_folder_tracks(&self) -> Vec<Track> {
        let dir = self.folder_state.current().to_path_buf();
        self.folder_state
            .tree()
            .tracks_in(&dir, true)
            .into_iter()
            .filter_map(|i| self.session.library.tracks.get(i).cloned())
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

    /// Load full-size cover bytes for an album key into the LRU on a
    /// blocking thread, unless they are cached / in flight / known absent
    /// (see `Session::load_cover_bytes`). `hint` is only evaluated when a
    /// load is actually going to start.
    pub(super) fn request_cover_bytes(
        &mut self,
        key: String,
        hint: impl FnOnce(&Self) -> Option<Album>,
    ) -> Task<cosmic::Action<Message>> {
        if !self.session.cover_bytes.should_load(&key) {
            return Task::none();
        }
        let Some(hint) = hint(self) else {
            // Album not (yet) in the library -- e.g. mid incremental load.
            // Don't mark it missing; the next library event retries.
            return Task::none();
        };
        session_effect(self.session.load_cover_bytes(key, hint))
    }

    /// [`Self::request_cover_bytes`] for the album identified by its
    /// `(artist, album)` pair (how the current track maps to a cover key).
    pub(super) fn request_cover_bytes_for(
        &mut self,
        artist: &str,
        album_name: &str,
    ) -> Task<cosmic::Action<Message>> {
        let key = aulos_core::library::CoverArt::album_key(artist, album_name);
        self.request_cover_bytes(key, |m| {
            m.session.library.cover_hint_for(artist, album_name)
        })
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
        let track = match self.session.playback.current_track.as_ref() {
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
        // by an earlier call -- e.g. the previous library batch in an
        // incremental reload -- but `BlurReady` hasn't arrived yet).
        // Don't spawn a duplicate: each spawn would build a brand-new
        // (if identical) handle, defeating the point of caching by key.
        if self.blur_pending_key.as_ref() == Some(&key) {
            return Task::none();
        }

        // Look up raw bytes. If they are not cached yet, load them lazily
        // (the library load no longer carries full-size covers), reset the
        // key so we retry when `SessionEvent::CoverBytes` arrives, but keep the
        // current blurred_cover showing (previous track's blur) rather than
        // blanking the background immediately.
        let bytes = match self.session.cover_bytes.get(&key) {
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
        let play_task = session_effect(self.session.read_audio_files(paths));
        Task::batch([import_task, play_task])
    }

    /// Lazily fetches artist bio/image for the Artists page — called
    /// from `select_nav` when the page becomes active (see
    /// `Session::fetch_artist_info_for_visible`).
    pub(super) fn load_artist_info_for_visible(&mut self) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.fetch_artist_info_for_visible())
    }

    /// Fetches artist info for a single artist immediately (e.g. when
    /// selected from the detail view; see `Session::fetch_artist_info_for`).
    pub(super) fn load_artist_info_for_selected(
        &mut self,
        name: &str,
    ) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.fetch_artist_info_for(name))
    }

    /// Resets the add-forms whose content a session notice just consumed:
    /// a subscribed podcast clears the feed-URL input, a saved radio
    /// station the radio add form.
    fn reset_forms_for(&mut self, notice: &SessionNotice) {
        match notice {
            SessionNotice::Podcast(PodcastNotice::Subscribed { .. }) => self.reset_podcast_form(),
            SessionNotice::Radio(RadioNotice::Saved { .. } | RadioNotice::AlreadySaved { .. }) => {
                self.reset_radio_form();
            }
            _ => {}
        }
    }
}

/// The toast for a session notice.
fn notice_toast(notice: SessionNotice) -> widget::toaster::Toast<Message> {
    let text = match notice {
        SessionNotice::Playback(PlaybackNotice::Queued(count)) => {
            fl!("queued-tracks", count = count)
        }
        SessionNotice::Playback(PlaybackNotice::OpenFilesFailed) => fl!("toast-open-files-failed"),
        SessionNotice::Podcast(notice) => super::podcast_page::notice_text(notice),
        SessionNotice::Radio(notice) => super::radio_page::notice_text(notice),
        SessionNotice::Convert(ConvertNotice::QueueFinished { done, failed }) => {
            // Queue just drained: summarize the batch.
            if failed > 0 {
                fl!(
                    "toast-convert-queue-done-with-failures",
                    done = done,
                    failed = failed
                )
            } else {
                fl!("toast-convert-queue-done", done = done)
            }
        }
        SessionNotice::ProviderActionFailed(reason) => {
            fl!("toast-provider-action-failed", reason = reason)
        }
        SessionNotice::ProviderConnectFailed { provider, reason } => fl!(
            "toast-provider-connect-failed",
            provider = provider,
            reason = reason
        ),
        SessionNotice::PlaylistsImported(summaries) => {
            super::playlist_io::import_toast_text(&summaries)
        }
        SessionNotice::PlaylistImportFailed(reason) => {
            fl!("toast-playlist-import-failed", reason = reason)
        }
    };
    widget::toaster::Toast::new(text)
}
