// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The UI half of the library and provider workflows, which live in
//! `aulos_core::session` (`Session::reload_library`, `apply_library_event`,
//! `switch_provider`, ...): turning their results into cover image handles,
//! the folder tree, the search filter, toasts and follow-up tasks.

use super::tasks::effect;
use super::{AppModel, Message, Page};
use aulos_core::fl;
use aulos_core::library::folder_tree::FolderTree;
use aulos_core::provider::mpd_events::IdleSubsystem;
use aulos_core::session::{
    CoverImage, CoverReset, CoverSet, LibraryApplied, LibraryEvent, MpdIdleOutcome, ProviderReinit,
    Reload,
};
use cosmic::prelude::*;
use cosmic::widget;
use std::collections::HashMap;
use std::path::PathBuf;

/// The UI half of a non-destructive library refresh (the data half is
/// `LibraryState`'s reload staging): cover handles accumulate here while
/// the previously loaded covers stay on screen.
/// `LibraryApplied::Completed` swaps this into
/// `cover_images`/`cover_fingerprints` atomically together with the staged
/// library data.
pub(super) struct CoverStaging {
    pub(super) cover_images: HashMap<String, widget::icon::Handle>,
    pub(super) cover_fingerprints: HashMap<String, u64>,
}

/// An image handle for a cover thumbnail.
fn cover_handle(image: CoverImage) -> widget::icon::Handle {
    match image {
        CoverImage::Pixels {
            width,
            height,
            rgba,
        } => widget::icon::from_raster_pixels(width, height, rgba),
        CoverImage::Encoded(bytes) => widget::icon::from_raster_bytes(bytes),
    }
}

/// Runs a library effect as an iced task.
fn library_task(library: aulos_core::task::Effect<LibraryEvent>) -> Task<cosmic::Action<Message>> {
    effect(library.map(Message::Library)).map(cosmic::Action::App)
}

impl AppModel {
    /// Start `reload`: apply what it does to the cover handles, then run
    /// its effect.
    pub(super) fn run_reload(&mut self, reload: Reload) -> Task<cosmic::Action<Message>> {
        match reload.covers {
            CoverReset::Keep => {}
            // A library is already showing: keep it fully visible and
            // accumulate the refreshed covers off to the side; they are
            // swapped in atomically once the whole reload finishes.
            CoverReset::Stage => {
                self.cover_staging = Some(CoverStaging {
                    cover_images: HashMap::new(),
                    cover_fingerprints: HashMap::new(),
                });
            }
            // First load: nothing on screen to flash away from; covers
            // populate progressively as batches arrive.
            CoverReset::Clear => {
                self.cover_staging = None;
                self.cover_images.clear();
                self.cover_fingerprints.clear();
                self.cover_art_bytes.clear();
            }
        }
        library_task(reload.effect)
    }

    /// Scan the active provider's library (startup, music dir changes).
    pub(super) fn scan_library(&mut self) -> Task<cosmic::Action<Message>> {
        let reload = self.session.scan_library();
        self.run_reload(reload)
    }

    /// The filesystem watcher reported changed `paths`.
    pub(super) fn files_changed(&mut self, paths: Vec<PathBuf>) -> Task<cosmic::Action<Message>> {
        library_task(self.session.files_changed(paths))
    }

    /// Image handles for `covers`, keeping the already-displayed handle of
    /// every cover whose content fingerprint is unchanged, so iced's raster
    /// cache never re-uploads it after a reload.
    fn cover_handles(
        &self,
        covers: CoverSet,
    ) -> (HashMap<String, widget::icon::Handle>, HashMap<String, u64>) {
        let mut images = HashMap::with_capacity(covers.len());
        let mut fingerprints = HashMap::with_capacity(covers.len());
        for (key, thumb) in covers {
            let reused = (self.cover_fingerprints.get(&key) == Some(&thumb.fingerprint))
                .then(|| self.cover_images.get(&key).cloned())
                .flatten();
            images.insert(
                key.clone(),
                reused.unwrap_or_else(|| cover_handle(thumb.image)),
            );
            fingerprints.insert(key, thumb.fingerprint);
        }
        (images, fingerprints)
    }

    /// Apply the async result of a library workflow.
    pub(super) fn handle_library_event(
        &mut self,
        event: LibraryEvent,
    ) -> Task<cosmic::Action<Message>> {
        match self.session.apply_library_event(event) {
            LibraryApplied::Stale | LibraryApplied::Done => Task::none(),
            LibraryApplied::ScanComplete { reload } => self.run_reload(reload),
            LibraryApplied::Loaded { covers } => {
                let (images, fingerprints) = self.cover_handles(covers);
                self.cover_images = images;
                self.cover_fingerprints = fingerprints;
                // Full-size bytes are loaded lazily; drop any stale ones.
                self.cover_art_bytes.clear();
                // Rebuild the folder tree only if it's already in use —
                // never-opened Folders view pays nothing on reload.
                if self.folder_state.is_populated()
                    || self.nav.active_data::<Page>() == Some(&Page::Folders)
                {
                    self.folder_state
                        .set_tree(FolderTree::build(&self.session.library.tracks));
                    self.session.library.folder_tree_gen = Some(self.session.library.generation());
                }
                self.refresh_search_filter();
                // Re-trigger blur now that cover art bytes are available.
                let blur_task = self.maybe_update_blurred_cover();
                let home_task = self.load_home(true);
                Task::batch([blur_task, home_task])
            }
            LibraryApplied::Batch {
                covers,
                staged,
                needs_refilter,
            } => {
                let (images, fingerprints) = self.cover_handles(covers);
                if staged {
                    if let Some(staging) = self.cover_staging.as_mut() {
                        staging.cover_images.extend(images);
                        staging.cover_fingerprints.extend(fingerprints);
                    }
                    return Task::none();
                }
                self.cover_images.extend(images);
                self.cover_fingerprints.extend(fingerprints);
                if needs_refilter {
                    self.refresh_search_filter();
                }
                // Re-trigger blur in case the current track's cover just arrived.
                self.maybe_update_blurred_cover()
            }
            LibraryApplied::Completed { swapped } => {
                let staged_cover = self.cover_staging.take();
                if swapped && let Some(staging) = staged_cover {
                    self.cover_images = staging.cover_images;
                    self.cover_fingerprints = staging.cover_fingerprints;
                    self.cover_art_bytes.clear();
                }
                self.refresh_search_filter();
                // Re-trigger blur in case the current track's cover changed
                // as part of a staged refresh swap.
                let blur_task = self.maybe_update_blurred_cover();
                let home_task = self.load_home(true);
                Task::batch([blur_task, home_task])
            }
            LibraryApplied::GenresLoaded => {
                self.refresh_search_filter();
                Task::none()
            }
            LibraryApplied::GenreTracks { idx, tracks } => {
                if self.selected_genre == Some(idx) {
                    self.session.library.genre_tracks = tracks;
                }
                Task::none()
            }
            LibraryApplied::FavoriteChanged { track_id, state } => {
                self.scrobble_favorite_changed(&track_id, state);
                Task::none()
            }
            LibraryApplied::ActionFailed(reason) => self.push_toast(widget::toaster::Toast::new(
                fl!("toast-provider-action-failed", reason = reason),
            )),
        }
    }

    /// Select a genre and load its tracks without blocking the UI thread.
    pub(super) fn select_genre(&mut self, idx: usize) -> Task<cosmic::Action<Message>> {
        self.selected_genre = Some(idx);
        library_task(self.session.load_genre_tracks(idx))
    }

    /// Load the active provider's genres.
    pub(super) fn load_genres(&self) -> Task<cosmic::Action<Message>> {
        library_task(self.session.load_genres())
    }

    pub(super) fn toggle_favorite(&mut self, track_id: String) -> Task<cosmic::Action<Message>> {
        library_task(self.session.toggle_favorite(track_id))
    }

    pub(super) fn set_rating(
        &mut self,
        track_id: String,
        rating: u8,
    ) -> Task<cosmic::Action<Message>> {
        library_task(self.session.set_rating(track_id, rating))
    }

    /// Switch to provider row `index` (header dropdown).
    pub(super) fn switch_provider(&mut self, index: usize) -> Task<cosmic::Action<Message>> {
        let Some(reload) = self.session.switch_provider(index) else {
            return Task::none();
        };
        // Recreate player with the correct backend for the new provider.
        self.recreate_player();
        self.cover_images.clear();
        self.cover_fingerprints.clear();
        self.cover_art_bytes.clear();
        self.artist_photos.clear();
        self.run_reload(reload)
    }

    /// Surface the Subsonic servers that failed to construct, then run the
    /// library reload.
    pub(super) fn finish_provider_reinit(
        &mut self,
        reinit: ProviderReinit,
    ) -> Task<cosmic::Action<Message>> {
        let ProviderReinit { reload, errors } = reinit;
        let mut tasks: Vec<_> = errors
            .into_iter()
            .map(|(name, reason)| {
                self.push_toast(widget::toaster::Toast::new(fl!(
                    "toast-provider-connect-failed",
                    provider = name,
                    reason = reason
                )))
            })
            .collect();
        tasks.push(self.run_reload(reload));
        Task::batch(tasks)
    }

    /// An MPD server connected.
    pub(super) fn handle_mpd_connected(
        &mut self,
        provider_id: &str,
    ) -> Task<cosmic::Action<Message>> {
        let connected = self.session.on_mpd_connected(provider_id);
        if connected.recreate_player {
            self.recreate_player();
        }
        self.run_reload(connected.reload)
    }

    /// An MPD server failed to connect.
    pub(super) fn handle_mpd_connection_failed(
        &mut self,
        provider_id: &str,
        error: &str,
    ) -> Task<cosmic::Action<Message>> {
        match self.session.on_mpd_connection_failed(provider_id, error) {
            Some((provider, reason)) => self.push_toast(widget::toaster::Toast::new(fl!(
                "toast-provider-connect-failed",
                provider = provider,
                reason = reason
            ))),
            None => Task::none(),
        }
    }

    /// An MPD idle event.
    pub(super) fn handle_mpd_idle(
        &mut self,
        provider_id: &str,
        subsystem: IdleSubsystem,
    ) -> Task<cosmic::Action<Message>> {
        match self.session.on_mpd_idle(provider_id, subsystem) {
            MpdIdleOutcome::Ignore => Task::none(),
            MpdIdleOutcome::Reload(reload) => self.run_reload(reload),
            MpdIdleOutcome::ReloadPlaylists => self.load_playlists(),
        }
    }
}
