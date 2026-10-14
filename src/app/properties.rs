// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Update logic for the Track/Album Properties drawer.
//!
//! Opening the drawer returns an off-thread task that reads the technical
//! details (a file probe, a server round trip, or an album-wide summary).
//! The request-id bookkeeping that drops stale results lives in
//! [`aulos_core::library::properties::PropertiesState`].

use super::{AppModel, ContextPage, Message};
use crate::views::track_properties::{self, PropertiesMessage};
use aulos_core::fl;
use aulos_core::library::properties::{
    Loadable, Subject, known_from_track, needs_probe, probe_track,
};
use aulos_core::library::tech_info::{self, AlbumSummary};
use aulos_core::library::{Album, Track};
use cosmic::prelude::*;
use cosmic::widget;

impl AppModel {
    /// Open the Properties drawer on `track` and read its technical details
    /// in the background.
    pub(super) fn open_track_properties(&mut self, track: Track) -> Task<cosmic::Action<Message>> {
        self.show_properties_page(ContextPage::TrackProperties);
        if !needs_probe(&track) {
            let info = Loadable::Loaded(known_from_track(&track));
            self.properties.begin_track(track, info);
            return Task::none();
        }
        let request_id = self
            .properties
            .begin_track(track.clone(), Loadable::Loading);
        let provider = self.registry.get_shared(&track.provider_id);
        cosmic::task::future(async move {
            let result =
                tokio::task::spawn_blocking(move || probe_track(&track, provider.as_deref()))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::PropertiesLoaded { request_id, result })
        })
    }

    /// Open the Properties drawer on `album` and summarise its tracks in the
    /// background.
    pub(super) fn open_album_properties(&mut self, album: Album) -> Task<cosmic::Action<Message>> {
        self.show_properties_page(ContextPage::AlbumProperties);
        let tracks = album.tracks.clone();
        let request_id = self.properties.begin_album(album);
        cosmic::task::future(async move {
            let summary = tokio::task::spawn_blocking(move || tech_info::summarize_album(&tracks))
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("album summary task failed: {e}");
                    AlbumSummary::default()
                });
            cosmic::Action::App(Message::AlbumPropertiesLoaded {
                request_id,
                summary,
            })
        })
    }

    fn show_properties_page(&mut self, page: ContextPage) {
        self.context_page = page;
        self.core.window.show_context = true;
    }

    /// Handle an action inside the drawer.
    pub(super) fn update_properties(
        &mut self,
        msg: PropertiesMessage,
    ) -> Task<cosmic::Action<Message>> {
        match msg {
            PropertiesMessage::OpenContainingFolder(path) => {
                if let Some(dir) = path.parent()
                    && let Err(e) = open::that_detached(dir)
                {
                    tracing::warn!("could not open {}: {e}", dir.display());
                }
                Task::none()
            }
            PropertiesMessage::EditTags(track) => {
                self.core.window.show_context = false;
                self.open_in_tagger(vec![*track], aulos_core::tagger::TagAction::Edit)
            }
        }
    }

    /// Body of the Track/Album Properties drawer.
    pub(super) fn properties_content(&self) -> cosmic::Element<'_, Message> {
        let page = match self.properties.subject() {
            Some(Subject::Track(track)) => {
                track_properties::track_view(track, self.properties.track_info().as_ref())
            }
            Some(Subject::Album(album)) => {
                track_properties::album_view(album, self.properties.album_summary().as_ref())
            }
            None => return widget::text::body(fl!("props-no-track")).into(),
        };
        page.map(Message::Properties)
    }
}
