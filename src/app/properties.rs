// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! State and update logic for the Track/Album Properties drawer.
//!
//! Opening the drawer returns an off-thread task that reads the technical
//! details (a file probe, a server round trip, or an album-wide summary).
//! Every opening takes a fresh, monotonically increasing request id and
//! the result message carries it back, so a slow result from a drawer the
//! user has since closed or re-opened on another subject is dropped instead
//! of overwriting the current one.

use super::{AppModel, ContextPage, Message};
use crate::fl;
use crate::library::tech_info::{self, AlbumSummary, TechInfo};
use crate::library::{Album, Track};
use crate::provider::MusicProvider;
use crate::views::track_properties::{
    self, Loadable, PropertiesMessage, SourceKind, known_from_track, source_kind,
};
use cosmic::prelude::*;
use cosmic::widget;

/// What the drawer currently describes.
#[derive(Debug, Clone)]
pub(super) enum Subject {
    Track(Track),
    Album(Album),
}

/// Drawer state: the subject, the id of the request that may still fill it
/// in, and the loading state of each kind of data.
#[derive(Debug, Default)]
pub(super) struct PropertiesState {
    /// Id of the latest opening; results carrying an older id are stale.
    request_id: u64,
    subject: Option<Subject>,
    track_info: Loadable<TechInfo>,
    album_summary: Loadable<AlbumSummary>,
}

impl PropertiesState {
    /// Start describing `track`; returns the id its result must carry.
    fn begin_track(&mut self, track: Track, initial: Loadable<TechInfo>) -> u64 {
        self.request_id += 1;
        self.subject = Some(Subject::Track(track));
        self.track_info = initial;
        self.album_summary = Loadable::Idle;
        self.request_id
    }

    /// Start describing `album`; returns the id its summary must carry.
    fn begin_album(&mut self, album: Album) -> u64 {
        self.request_id += 1;
        self.subject = Some(Subject::Album(album));
        self.track_info = Loadable::Idle;
        self.album_summary = Loadable::Loading;
        self.request_id
    }

    /// Whether a result tagged `request_id` still belongs to the drawer.
    fn accepts(&self, request_id: u64) -> bool {
        request_id == self.request_id
    }

    /// Record a finished track probe; a stale one is dropped.
    pub(super) fn finish_track(&mut self, request_id: u64, result: Result<TechInfo, String>) {
        if !self.accepts(request_id) || !matches!(self.subject, Some(Subject::Track(_))) {
            tracing::debug!(request_id, "dropping stale track properties");
            return;
        }
        self.track_info = match result {
            Ok(info) => Loadable::Loaded(info),
            Err(error) => Loadable::Failed(error),
        };
    }

    /// Record a finished album summary; a stale one is dropped.
    pub(super) fn finish_album(&mut self, request_id: u64, summary: AlbumSummary) {
        if !self.accepts(request_id) || !matches!(self.subject, Some(Subject::Album(_))) {
            tracing::debug!(request_id, "dropping stale album properties");
            return;
        }
        self.album_summary = Loadable::Loaded(summary);
    }
}

/// Whether reading a track's properties needs work off the UI thread.
/// Streams (radio, streamed podcasts) have nothing to read beyond what the
/// [`Track`] carries.
fn needs_probe(track: &Track) -> bool {
    source_kind(track) != SourceKind::Stream
}

/// Reads one track's technical details (blocking). The single place that
/// dispatches on where the track lives: local files are probed directly,
/// servers are asked through their provider, streams only report what the
/// track already knows.
fn probe_track(track: &Track, provider: Option<&dyn MusicProvider>) -> Result<TechInfo, String> {
    match source_kind(track) {
        SourceKind::Local => tech_info::probe(&track.path),
        SourceKind::Subsonic | SourceKind::Mpd => {
            let provider = provider.ok_or_else(|| fl!("props-provider-unavailable"))?;
            provider
                .track_tech_info(track)
                .map_err(|e| e.to_string())
                .map(|info| info.unwrap_or_else(|| known_from_track(track)))
        }
        SourceKind::Stream => Ok(known_from_track(track)),
    }
}

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
                self.open_in_tagger(vec![*track], crate::tagger::TagAction::Edit)
            }
        }
    }

    /// Body of the Track/Album Properties drawer.
    pub(super) fn properties_content(&self) -> cosmic::Element<'_, Message> {
        let page = match &self.properties.subject {
            Some(Subject::Track(track)) => {
                track_properties::track_view(track, self.properties.track_info.as_ref())
            }
            Some(Subject::Album(album)) => {
                track_properties::album_view(album, self.properties.album_summary.as_ref())
            }
            None => return widget::text::body(fl!("props-no-track")).into(),
        };
        page.map(Message::Properties)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn track(provider: &str, path: &str) -> Track {
        Track {
            id: 0,
            path: PathBuf::from(path),
            title: String::new(),
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
            provider_id: Arc::from(provider),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    fn info(rate: u32) -> TechInfo {
        TechInfo {
            sample_rate: Some(rate),
            ..TechInfo::default()
        }
    }

    #[test]
    fn request_ids_increase_with_every_opening() {
        let mut state = PropertiesState::default();
        let a = state.begin_track(track("local", "/a.flac"), Loadable::Loading);
        let b = state.begin_track(track("local", "/b.flac"), Loadable::Loading);
        assert!(b > a);
    }

    #[test]
    fn a_result_of_an_older_opening_is_dropped() {
        let mut state = PropertiesState::default();
        let old = state.begin_track(track("local", "/a.flac"), Loadable::Loading);
        let current = state.begin_track(track("local", "/b.flac"), Loadable::Loading);

        state.finish_track(old, Ok(info(44_100)));
        assert!(matches!(state.track_info, Loadable::Loading));

        state.finish_track(current, Ok(info(96_000)));
        match &state.track_info {
            Loadable::Loaded(i) => assert_eq!(i.sample_rate, Some(96_000)),
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    #[test]
    fn a_track_result_never_lands_on_an_album_subject() {
        let mut state = PropertiesState::default();
        let track_id = state.begin_track(track("local", "/a.flac"), Loadable::Loading);
        let album_id =
            state.begin_album(Album::new("A".into(), "B".into(), 2000, Vec::new(), None));
        state.finish_track(track_id, Ok(info(44_100)));
        assert!(matches!(state.track_info, Loadable::Idle));

        state.finish_album(track_id, AlbumSummary::default());
        assert!(matches!(state.album_summary, Loadable::Loading));
        state.finish_album(album_id, AlbumSummary::default());
        assert!(matches!(state.album_summary, Loadable::Loaded(_)));
    }

    #[test]
    fn a_failed_probe_is_kept_for_display() {
        let mut state = PropertiesState::default();
        let id = state.begin_track(track("local", "/a.flac"), Loadable::Loading);
        state.finish_track(id, Err("unreadable".into()));
        assert!(matches!(&state.track_info, Loadable::Failed(e) if e == "unreadable"));
    }

    #[test]
    fn only_streams_skip_the_probe() {
        assert!(needs_probe(&track("local", "/a.flac")));
        assert!(needs_probe(&track("subsonic-home", "42")));
        assert!(needs_probe(&track("mpd-home", "")));
        assert!(!needs_probe(&track("radio", "")));
        // A downloaded podcast episode is a file; a streamed one is not.
        assert!(needs_probe(&track("podcast", "/ep.mp3")));
        assert!(!needs_probe(&track("podcast", "")));
    }

    #[test]
    fn a_remote_track_without_its_provider_reports_an_error() {
        assert!(probe_track(&track("subsonic-home", "42"), None).is_err());
    }
}
