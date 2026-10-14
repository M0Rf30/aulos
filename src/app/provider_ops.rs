// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Provider calls that may block (network / DB mutex), dispatched off the UI
//! thread with optimistic local updates where it makes sense.

use super::message::PlaylistOp;
use super::{AppModel, Message};
use aulos_core::library::Track;
use aulos_core::provider::MusicProvider;
use cosmic::prelude::*;

impl AppModel {
    /// Apply `f` to every copy of the track with `track_id` held by the
    /// model: the library lists and the current track.
    fn for_each_track(&mut self, track_id: &str, mut f: impl FnMut(&mut Track)) {
        self.library.for_each_track(track_id, &mut f);
        if let Some(ct) = self.current_track.as_mut()
            && ct.id.to_string() == track_id
        {
            f(ct);
        }
    }

    pub(super) fn set_favorite_local(&mut self, track_id: &str, state: bool) {
        self.for_each_track(track_id, |t| t.is_favorite = state);
    }

    pub(super) fn set_rating_local(&mut self, track_id: &str, rating: Option<u8>) {
        self.for_each_track(track_id, |t| t.rating = rating);
    }

    pub(super) fn toggle_favorite_async(
        &mut self,
        track_id: String,
    ) -> Task<cosmic::Action<Message>> {
        let Some(provider) = self.registry.active_shared() else {
            return Task::none();
        };
        let optimistic = !self
            .library
            .favorite_state(self.current_track.as_ref(), &track_id)
            .unwrap_or(false);
        self.set_favorite_local(&track_id, optimistic);
        cosmic::task::future(async move {
            let id = track_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                provider.toggle_favorite(&id).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::FavoriteToggled {
                track_id,
                optimistic,
                result,
            })
        })
    }

    pub(super) fn set_rating_async(
        &mut self,
        track_id: String,
        rating: u8,
    ) -> Task<cosmic::Action<Message>> {
        let Some(provider) = self.registry.active_shared() else {
            return Task::none();
        };
        let previous = self
            .library
            .rating_state(self.current_track.as_ref(), &track_id);
        self.set_rating_local(&track_id, if rating == 0 { None } else { Some(rating) });
        cosmic::task::future(async move {
            let id = track_id.clone();
            let result = tokio::task::spawn_blocking(move || {
                provider.set_rating(&id, rating).map_err(|e| e.to_string())
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::RatingSet {
                track_id,
                previous,
                result,
            })
        })
    }

    /// Run a playlist mutation against the active provider off the UI thread.
    pub(super) fn playlist_op_async(
        &self,
        op: PlaylistOp,
        f: impl FnOnce(&dyn MusicProvider) -> Result<(), String> + Send + 'static,
    ) -> Task<cosmic::Action<Message>> {
        let Some(provider) = self.registry.active_shared() else {
            return Task::none();
        };
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || f(provider.as_ref()))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::PlaylistOpDone { op, result })
        })
    }

    /// Select a genre and load its tracks without blocking the UI thread.
    pub(super) fn select_genre(&mut self, idx: usize) -> Task<cosmic::Action<Message>> {
        self.selected_genre = Some(idx);
        self.library.genre_tracks.clear();
        let Some(genre) = self.library.genres.get(idx).cloned() else {
            return Task::none();
        };
        if let Some(provider) = self.registry.active_shared() {
            cosmic::task::future(async move {
                let tracks = tokio::task::spawn_blocking(move || {
                    provider.get_tracks_by_genre(&genre).unwrap_or_default()
                })
                .await
                .unwrap_or_default();
                cosmic::Action::App(Message::GenreTracksLoaded { idx, tracks })
            })
        } else {
            self.library.genre_tracks = self.library.tracks_in_genre(&genre);
            Task::none()
        }
    }
}
