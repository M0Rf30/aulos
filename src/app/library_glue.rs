// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The UI half of the library and provider workflows, which live in
//! `aulos_core::session` (`Session::reload_library`, `apply_library_event`,
//! `switch_provider`, ...): keeping the cover image handles and the folder
//! tree in line with the session. Their results all arrive as session
//! updates (`AppModel::session_task`).

use super::tasks::session_effect;
use super::{AppModel, Message, Page};
use aulos_core::library::covers::{CoverDelta, CoverThumb};
use aulos_core::library::folder_tree::FolderTree;
use cosmic::prelude::*;
use cosmic::widget;

/// An image handle for a cover thumbnail; shares the core's pixel buffer.
fn cover_handle(thumb: &CoverThumb) -> widget::icon::Handle {
    widget::icon::Handle {
        symbolic: false,
        data: widget::icon::Data::Image(widget::image::Handle::from_rgba(
            thumb.width,
            thumb.height,
            thumb.rgba.clone(),
        )),
    }
}

impl AppModel {
    /// Bring the image handles in line with the core's covers: drop the
    /// removed ones and build handles for the new or changed ones only, so
    /// the handle of an unchanged cover (and iced's upload of it) survives.
    pub(super) fn apply_cover_delta(&mut self, delta: CoverDelta) {
        if delta.cleared {
            self.cover_images.clear();
        }
        for key in &delta.removed {
            self.cover_images.remove(key);
        }
        for key in &delta.changed {
            if let Some(thumb) = self.session.library.covers.get(key) {
                self.cover_images.insert(key.clone(), cover_handle(thumb));
            }
        }
    }

    /// The whole library was replaced: rebuild the folder tree, but only if
    /// it's already in use -- a never-opened Folders view pays nothing on
    /// reload.
    pub(super) fn rebuild_folder_tree_if_shown(&mut self) {
        if self.folder_state.is_populated()
            || self.nav.active_data::<Page>() == Some(&Page::Folders)
        {
            self.folder_state
                .set_tree(FolderTree::build(&self.session.library.tracks));
            self.session.library.folder_tree_gen = Some(self.session.library.generation());
        }
    }

    /// The header-search input changed; the session debounces the refresh
    /// of the results.
    pub(super) fn set_library_search(&mut self, query: String) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.set_search_query(query))
    }

    /// Select a genre and load its tracks without blocking the UI thread.
    pub(super) fn select_genre(&mut self, idx: usize) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.load_genre_tracks(idx))
    }

    /// Load the active provider's genres.
    pub(super) fn load_genres(&self) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.load_genres())
    }

    pub(super) fn toggle_favorite(&mut self, track_id: String) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.toggle_favorite(track_id))
    }

    pub(super) fn set_rating(
        &mut self,
        track_id: String,
        rating: u8,
    ) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.set_rating(track_id, rating))
    }

    /// Switch to provider row `index` (header dropdown).
    pub(super) fn switch_provider(&mut self, index: usize) -> Task<cosmic::Action<Message>> {
        let update = self.session.switch_provider(index);
        self.session_task(update)
    }
}
