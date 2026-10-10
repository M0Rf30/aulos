// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Provider calls that may block (network / DB mutex), dispatched off the UI
//! thread with optimistic local updates where it makes sense.

use super::message::PlaylistOp;
use super::{AppModel, Message};
use crate::provider::MusicProvider;
use cosmic::prelude::*;

/// Apply `f` to every copy of the track with `track_id` held by the model.
fn for_each_track(
    model: &mut AppModel,
    track_id: &str,
    mut f: impl FnMut(&mut crate::library::Track),
) {
    for track in &mut model.all_tracks {
        if track.id.to_string() == track_id {
            f(track);
        }
    }
    for album in &mut model.all_albums {
        for track in &mut album.tracks {
            if track.id.to_string() == track_id {
                f(track);
            }
        }
    }
    for artist in &mut model.all_artists {
        for album in &mut artist.albums {
            for track in &mut album.tracks {
                if track.id.to_string() == track_id {
                    f(track);
                }
            }
        }
    }
    if let Some(ct) = model.current_track.as_mut()
        && ct.id.to_string() == track_id
    {
        f(ct);
    }
}

impl AppModel {
    pub(super) fn set_favorite_local(&mut self, track_id: &str, state: bool) {
        for_each_track(self, track_id, |t| t.is_favorite = state);
    }

    pub(super) fn set_rating_local(&mut self, track_id: &str, rating: Option<u8>) {
        for_each_track(self, track_id, |t| t.rating = rating);
    }

    /// Current favourite state of a track, if known locally.
    fn favorite_state(&self, track_id: &str) -> Option<bool> {
        if let Some(ct) = &self.current_track
            && ct.id.to_string() == track_id
        {
            return Some(ct.is_favorite);
        }
        self.all_tracks
            .iter()
            .find(|t| t.id.to_string() == track_id)
            .map(|t| t.is_favorite)
    }

    fn rating_state(&self, track_id: &str) -> Option<u8> {
        if let Some(ct) = &self.current_track
            && ct.id.to_string() == track_id
        {
            return ct.rating;
        }
        self.all_tracks
            .iter()
            .find(|t| t.id.to_string() == track_id)
            .and_then(|t| t.rating)
    }

    pub(super) fn toggle_favorite_async(
        &mut self,
        track_id: String,
    ) -> Task<cosmic::Action<Message>> {
        let Some(provider) = self.registry.active_shared() else {
            return Task::none();
        };
        let optimistic = !self.favorite_state(&track_id).unwrap_or(false);
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
        let previous = self.rating_state(&track_id);
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
        self.genre_tracks.clear();
        let Some(genre) = self.all_genres.get(idx).cloned() else {
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
            self.genre_tracks = self
                .all_tracks
                .iter()
                .filter(|t| t.genre.eq_ignore_ascii_case(&genre))
                .cloned()
                .collect();
            Task::none()
        }
    }
}
