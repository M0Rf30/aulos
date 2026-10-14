// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Update logic for the Track/Album Properties drawer.
//!
//! Opening the drawer returns an off-thread task that reads the technical
//! details (a file probe, a server round trip, or an album-wide summary).
//! The request-id bookkeeping that drops stale results lives in
//! [`aulos_core::library::properties::PropertiesState`].

use super::tasks::session_effect;
use super::{AppModel, ContextPage, Message};
use crate::views::track_properties::{self, PropertiesMessage};
use aulos_core::fl;
use aulos_core::library::properties::Subject;
use aulos_core::library::{Album, Track};
use cosmic::prelude::*;
use cosmic::widget;

impl AppModel {
    /// Open the Properties drawer on `track` and read its technical details
    /// in the background.
    pub(super) fn open_track_properties(&mut self, track: Track) -> Task<cosmic::Action<Message>> {
        self.show_properties_page(ContextPage::TrackProperties);
        session_effect(self.session.open_track_properties(track))
    }

    /// Open the Properties drawer on `album` and summarise its tracks in the
    /// background.
    pub(super) fn open_album_properties(&mut self, album: Album) -> Task<cosmic::Action<Message>> {
        self.show_properties_page(ContextPage::AlbumProperties);
        session_effect(self.session.open_album_properties(album))
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
        let page = match self.session.properties.subject() {
            Some(Subject::Track(track)) => {
                track_properties::track_view(track, self.session.properties.track_info().as_ref())
            }
            Some(Subject::Album(album)) => track_properties::album_view(
                album,
                self.session.properties.album_summary().as_ref(),
            ),
            None => return widget::text::body(fl!("props-no-track")).into(),
        };
        page.map(Message::Properties)
    }
}
