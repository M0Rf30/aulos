// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Opening the Track/Album Properties drawer: the technical details are read
//! off-thread (a file probe, a server round trip, or an album-wide summary)
//! and come back as a [`PropertiesEvent`]. Stale results are dropped by
//! [`PropertiesState`](crate::library::properties::PropertiesState)'s
//! request ids.

use super::Session;
use crate::library::properties::{Loadable, known_from_track, needs_probe, probe_track};
use crate::library::tech_info::{self, AlbumSummary, TechInfo};
use crate::library::{Album, Track};
use crate::task::Effect;

/// The off-thread result of opening the drawer. `request_id` identifies the
/// opening it belongs to.
#[derive(Debug, Clone)]
pub enum PropertiesEvent {
    Track {
        request_id: u64,
        result: Result<Box<TechInfo>, String>,
    },
    Album {
        request_id: u64,
        summary: AlbumSummary,
    },
}

impl Session {
    /// Start describing `track`, reading its technical details in the
    /// background when it has anything to read.
    pub fn open_track_properties(&mut self, track: Track) -> Effect<PropertiesEvent> {
        if !needs_probe(&track) {
            let info = Loadable::Loaded(known_from_track(&track));
            self.properties.begin_track(track, info);
            return Effect::none();
        }
        let request_id = self
            .properties
            .begin_track(track.clone(), Loadable::Loading);
        let provider = self.providers.registry.get_shared(&track.provider_id);
        Effect::future(async move {
            let result =
                tokio::task::spawn_blocking(move || probe_track(&track, provider.as_deref()))
                    .await
                    .map_or_else(|e| Err(e.to_string()), |r| r.map(Box::new));
            PropertiesEvent::Track { request_id, result }
        })
    }

    /// Start describing `album`, summarising its tracks in the background.
    pub fn open_album_properties(&mut self, album: Album) -> Effect<PropertiesEvent> {
        let tracks = album.tracks.clone();
        let request_id = self.properties.begin_album(album);
        Effect::future(async move {
            let summary = tokio::task::spawn_blocking(move || tech_info::summarize_album(&tracks))
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("album summary task failed: {e}");
                    AlbumSummary::default()
                });
            PropertiesEvent::Album {
                request_id,
                summary,
            }
        })
    }

    /// Record a finished probe/summary; a stale one is dropped.
    pub fn apply_properties_event(&mut self, event: PropertiesEvent) {
        match event {
            PropertiesEvent::Track { request_id, result } => {
                self.properties
                    .finish_track(request_id, result.map(|info| *info));
            }
            PropertiesEvent::Album {
                request_id,
                summary,
            } => self.properties.finish_album(request_id, summary),
        }
    }
}
