// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Saved playlists (provider-backed), smart playlists (rule-based, evaluated
//! against the library database) and playlist file import.
//!
//! Workflows return an [`Effect`] of [`PlaylistEvent`]s; the UI feeds each
//! event back through [`Session::apply_playlist_event`].

use super::{PlaybackUpdate, Session};
use crate::library::playlist_io::{self, ExportSource, ImportSummary};
use crate::library::smart_playlist::{EditorState, SmartPlaylist};
use crate::library::{LibraryDb, Playlist, Track};
use crate::provider::{MusicProvider, ProviderError};
use crate::task::Effect;
use std::path::PathBuf;
use std::sync::Arc;

/// How long a rule edit settles before its match count is queried.
const MATCH_COUNT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

/// Which provider playlist mutation a [`PlaylistEvent::OpDone`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistOp {
    AddTrack,
    Create,
    Delete,
    Rename,
    RemoveTrack,
    MoveTrack,
}

/// What the active provider lets the UI do to the tracks of a playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaylistEditing {
    pub remove: bool,
    pub reorder: bool,
}

/// Async results of the playlist workflows.
#[derive(Debug, Clone)]
pub enum PlaylistEvent {
    /// The active provider's playlists, tracks included.
    Loaded(Vec<Playlist>),
    /// Result of a provider playlist mutation (add/create/delete/rename).
    OpDone {
        op: PlaylistOp,
        result: Result<(), String>,
    },
    /// Saved smart playlists (with resolved track counts).
    SmartLoaded(Vec<SmartPlaylist>),
    /// Resolved tracks of the selected smart playlist (detail view), and
    /// how many tracks its rules match before the row limit cuts them.
    SmartTracksLoaded { tracks: Vec<Track>, total: usize },
    /// How many tracks the in-editor rules at `revision` match (limit
    /// ignored); `None` when the query failed or a newer edit superseded it.
    SmartMatchCount { revision: u64, count: Option<usize> },
    /// Resolved tracks of a smart playlist, to play right away.
    SmartPlayResolved(Vec<Track>),
    /// Playlist files imported, one summary per file.
    Imported(Result<Vec<ImportSummary>, String>),
}

/// What the UI must show or reset after [`Session::apply_playlist_event`].
#[derive(Debug, Clone)]
pub enum PlaylistOutcome {
    /// Nothing.
    None,
    /// A playlist was created.
    Created,
    /// A playlist was deleted.
    Deleted,
    /// A provider mutation failed; the UI shows `reason`.
    Failed(String),
    /// Playlist files were imported.
    Imported(Vec<ImportSummary>),
    /// Importing playlist files failed.
    ImportFailed(String),
}

/// What a UI must do after [`Session::apply_playlist_event`].
#[must_use]
pub struct PlaylistUpdate {
    /// Follow-up work; its events go back into `apply_playlist_event`.
    pub effect: Effect<PlaylistEvent>,
    /// Set when the event started playback (a smart playlist to play): the
    /// UI runs it like any other playback update.
    pub playback: Option<PlaybackUpdate>,
    pub outcome: PlaylistOutcome,
}

impl PlaylistUpdate {
    fn outcome(outcome: PlaylistOutcome) -> Self {
        Self::new(Effect::none(), outcome)
    }

    fn new(effect: Effect<PlaylistEvent>, outcome: PlaylistOutcome) -> Self {
        Self {
            effect,
            playback: None,
            outcome,
        }
    }
}

/// Saved playlists and smart (rule-based) playlists.
#[derive(Default)]
pub struct PlaylistsState {
    pub playlists: Vec<Playlist>,
    pub smart_playlists: Vec<SmartPlaylist>,
    /// Resolved tracks for the currently viewed smart playlist.
    pub smart_playlist_tracks: Vec<Track>,
    /// How many tracks the viewed smart playlist's rules match before its
    /// row limit applies (equals the track count when it has no limit or
    /// the limit is not reached); `0` until resolved.
    pub smart_playlist_total: usize,
    /// The smart-playlist rules editor, while open.
    pub smart_playlist_editor: Option<EditorState>,
}

impl PlaylistsState {
    /// The tracks of the playlist at `idx`, if it exists and has any.
    #[must_use]
    pub fn playlist_tracks(&self, idx: usize) -> Option<&[Track]> {
        self.playlists
            .get(idx)
            .map(|p| p.tracks.as_slice())
            .filter(|t| !t.is_empty())
    }

    /// Drops the track at `track` of playlist `idx` from the loaded copy.
    /// Returns false when either index is out of range.
    pub fn remove_track_local(&mut self, idx: usize, track: usize) -> bool {
        let Some(playlist) = self.playlists.get_mut(idx) else {
            return false;
        };
        if track >= playlist.tracks.len() {
            return false;
        }
        playlist.tracks.remove(track);
        playlist.refresh_totals();
        true
    }

    /// Moves a track of the loaded copy of playlist `idx` to final index
    /// `to`. Returns false when an index is out of range or nothing moves.
    pub fn move_track_local(&mut self, idx: usize, from: usize, to: usize) -> bool {
        let Some(playlist) = self.playlists.get_mut(idx) else {
            return false;
        };
        let len = playlist.tracks.len();
        if from >= len || to >= len || from == to {
            return false;
        }
        let track = playlist.tracks.remove(from);
        playlist.tracks.insert(to, track);
        true
    }

    /// What to export for the playlist at `idx`: its name and tracks.
    #[must_use]
    pub fn export_tracks(&self, idx: usize) -> Option<(String, ExportSource)> {
        let playlist = self.playlists.get(idx)?;
        Some((
            playlist.name.clone(),
            ExportSource::Tracks(playlist.tracks.clone()),
        ))
    }

    /// What to export for the smart playlist at `idx`: its name and rules.
    #[must_use]
    pub fn export_smart(&self, idx: usize) -> Option<(String, ExportSource)> {
        let playlist = self.smart_playlists.get(idx)?;
        Some((playlist.name.clone(), ExportSource::Smart(playlist.clone())))
    }

    /// Opens the editor on a brand-new smart playlist.
    pub fn new_smart_playlist(&mut self) {
        self.smart_playlist_editor = Some(EditorState::new());
    }

    /// Opens the editor on the smart playlist at `idx` (no-op if absent).
    pub fn edit_smart_playlist(&mut self, idx: usize) {
        if let Some(playlist) = self.smart_playlists.get(idx) {
            self.smart_playlist_editor = Some(EditorState::from_existing(playlist.clone()));
        }
    }

    /// Discards the in-progress smart-playlist edit.
    pub fn cancel_smart_editor(&mut self) {
        self.smart_playlist_editor = None;
    }

    /// Saves the edited smart playlist (create if new, else update) when the
    /// editor is valid, closes the editor and reloads the list.
    pub fn save_smart_editor(&mut self) -> Effect<PlaylistEvent> {
        let Some(state) = &self.smart_playlist_editor else {
            return Effect::none();
        };
        if !state.errors.is_empty() {
            return Effect::none();
        }
        let playlist = state.playlist.clone();
        self.smart_playlist_editor = None;
        let db_path = crate::paths::library_db_path();
        smart_list_effect("Save smart playlist failed", move || {
            let db = LibraryDb::open(&db_path)?;
            if playlist.id == 0 {
                db.create_smart_playlist(&playlist)?;
            } else {
                db.update_smart_playlist(&playlist)?;
            }
            db.list_smart_playlists()
        })
    }

    /// Deletes the smart playlist at `idx` and reloads the list; `None` if
    /// there is no such playlist.
    pub fn delete_smart_playlist(&self, idx: usize) -> Option<Effect<PlaylistEvent>> {
        let id = self.smart_playlists.get(idx)?.id;
        let db_path = crate::paths::library_db_path();
        Some(smart_list_effect(
            "Delete smart playlist failed",
            move || {
                let db = LibraryDb::open(&db_path)?;
                db.delete_smart_playlist(id)?;
                db.list_smart_playlists()
            },
        ))
    }

    /// Reloads the saved smart playlists.
    pub fn load_smart_playlists(&self) -> Effect<PlaylistEvent> {
        let db_path = crate::paths::library_db_path();
        smart_list_effect("list_smart_playlists failed", move || {
            LibraryDb::open(&db_path)?.list_smart_playlists()
        })
    }

    /// Clears the detail view's tracks and resolves those of the smart
    /// playlist at `idx`.
    pub fn select_smart_playlist(&mut self, idx: usize) -> Effect<PlaylistEvent> {
        self.smart_playlist_tracks.clear();
        self.smart_playlist_total = 0;
        match self.smart_playlists.get(idx).cloned() {
            Some(playlist) => smart_detail_effect(playlist),
            None => Effect::none(),
        }
    }

    /// Asks for the match count of the editor's current rules: once per
    /// edit, after a short pause (an edit that arrives meanwhile cancels
    /// it), and only while the rules are valid. Its answer is
    /// [`PlaylistEvent::SmartMatchCount`]; call this every frame the editor
    /// is open.
    pub fn request_match_count(&mut self) -> Effect<PlaylistEvent> {
        let Some(request) = self
            .smart_playlist_editor
            .as_mut()
            .and_then(EditorState::take_match_request)
        else {
            return Effect::none();
        };
        let db_path = crate::paths::library_db_path();
        Effect::future(async move {
            let revision = request.revision;
            crate::task::sleep(MATCH_COUNT_DEBOUNCE).await;
            if request.is_superseded() {
                return PlaylistEvent::SmartMatchCount {
                    revision,
                    count: None,
                };
            }
            let count = crate::task::spawn_blocking(move || {
                LibraryDb::open(&db_path)
                    .and_then(|db| db.smart_playlist_match_count(&request.playlist))
                    .inspect_err(|e| tracing::warn!("smart_playlist_match_count failed: {e}"))
                    .ok()
            })
            .await
            .ok()
            .flatten();
            PlaylistEvent::SmartMatchCount { revision, count }
        })
    }

    /// Resolves the smart playlist at `idx` for immediate playback.
    pub fn resolve_smart_playlist(&self, idx: usize) -> Effect<PlaylistEvent> {
        match self.smart_playlists.get(idx).cloned() {
            Some(playlist) => smart_tracks_effect(playlist, PlaylistEvent::SmartPlayResolved),
            None => Effect::none(),
        }
    }
}

impl Session {
    /// Loads the active provider's playlists, including each one's tracks.
    pub fn load_playlists(&self) -> Effect<PlaylistEvent> {
        let Some(provider) = self.providers.registry.active_shared() else {
            return Effect::none();
        };
        Effect::future(async move {
            let mut playlists = provider.list_playlists().await.unwrap_or_else(|e| {
                tracing::warn!("list_playlists failed: {e}");
                Vec::new()
            });
            // `list_playlists` returns summaries only (no tracks) for the
            // local provider; fetch each playlist's tracks so the detail
            // view, "play all" and M3U export have them.
            for playlist in playlists.iter_mut() {
                if playlist.tracks.is_empty()
                    && playlist.track_count > 0
                    && let Ok(full) = provider.get_playlist(&playlist.id).await
                {
                    playlist.tracks = full.tracks;
                }
            }
            PlaylistEvent::Loaded(playlists)
        })
    }

    /// The provider a playlist mutation runs against.
    fn playlist_provider(&self) -> Option<Arc<dyn MusicProvider>> {
        self.providers.registry.active_shared()
    }

    /// Creates a playlist on the active provider.
    pub fn create_playlist(&self, name: String) -> Effect<PlaylistEvent> {
        let Some(provider) = self.playlist_provider() else {
            return Effect::none();
        };
        Effect::future(async move {
            let result = provider.create_playlist(&name).await.map(|_| ());
            op_done(PlaylistOp::Create, result)
        })
    }

    /// Deletes the playlist at `idx` of `playlists.playlists`.
    pub fn delete_playlist(&self, idx: usize) -> Effect<PlaylistEvent> {
        let Some(playlist) = self.playlists.playlists.get(idx) else {
            return Effect::none();
        };
        let id = playlist.id.clone();
        let Some(provider) = self.playlist_provider() else {
            return Effect::none();
        };
        Effect::future(
            async move { op_done(PlaylistOp::Delete, provider.delete_playlist(&id).await) },
        )
    }

    /// Renames the playlist at `idx` of `playlists.playlists`.
    pub fn rename_playlist(&self, idx: usize, new_name: String) -> Effect<PlaylistEvent> {
        let Some(playlist) = self.playlists.playlists.get(idx) else {
            return Effect::none();
        };
        let id = playlist.id.clone();
        let Some(provider) = self.playlist_provider() else {
            return Effect::none();
        };
        Effect::future(async move {
            let result = provider.rename_playlist(&id, &new_name).await;
            op_done(PlaylistOp::Rename, result)
        })
    }

    /// Appends a track (by source URI) to the playlist `playlist_id`.
    pub fn add_to_playlist(
        &self,
        playlist_id: String,
        track_source_uri: String,
    ) -> Effect<PlaylistEvent> {
        let Some(provider) = self.playlist_provider() else {
            return Effect::none();
        };
        Effect::future(async move {
            let result = provider
                .add_to_playlist(&playlist_id, &[track_source_uri])
                .await;
            op_done(PlaylistOp::AddTrack, result)
        })
    }

    /// What the active provider lets the UI do to a playlist's tracks.
    #[must_use]
    pub fn playlist_editing(&self) -> PlaylistEditing {
        use crate::provider::ProviderType;
        let remove = self.providers.active_is(ProviderType::Local)
            || self.providers.active_is(ProviderType::Mpd)
            || self.providers.active_is(ProviderType::Subsonic);
        let reorder = self.providers.active_is(ProviderType::Local)
            || self.providers.active_is(ProviderType::Mpd);
        PlaylistEditing { remove, reorder }
    }

    /// Removes the track at `track` (0-based) from the playlist at `idx` of
    /// `playlists.playlists`. The loaded copy changes at once; the provider
    /// call then reloads the playlists (reverting the copy on failure).
    pub fn remove_track_from_playlist(
        &mut self,
        idx: usize,
        track: usize,
    ) -> Effect<PlaylistEvent> {
        if !self.playlist_editing().remove || !self.playlists.remove_track_local(idx, track) {
            return Effect::none();
        }
        let id = self.playlists.playlists[idx].id.clone();
        let Some(provider) = self.playlist_provider() else {
            return Effect::none();
        };
        Effect::future(async move {
            let result = provider.remove_from_playlist(&id, track).await;
            op_done(PlaylistOp::RemoveTrack, result)
        })
    }

    /// Moves the track at `from` of the playlist at `idx` so it ends up at
    /// `to` (0-based final index); same optimistic update as removal.
    pub fn move_playlist_track(
        &mut self,
        idx: usize,
        from: usize,
        to: usize,
    ) -> Effect<PlaylistEvent> {
        if !self.playlist_editing().reorder || !self.playlists.move_track_local(idx, from, to) {
            return Effect::none();
        }
        let id = self.playlists.playlists[idx].id.clone();
        let Some(provider) = self.playlist_provider() else {
            return Effect::none();
        };
        Effect::future(async move {
            let result = provider.move_in_playlist(&id, from, to).await;
            op_done(PlaylistOp::MoveTrack, result)
        })
    }

    /// Applies an async playlist result. Replacing the playlists refreshes
    /// the library search results (they include playlists), and resolved
    /// smart-playlist tracks start playing.
    pub fn apply_playlist_event(&mut self, event: PlaylistEvent) -> PlaylistUpdate {
        match event {
            PlaylistEvent::Loaded(playlists) => {
                self.playlists.playlists = playlists;
                self.refresh_search();
                PlaylistUpdate::outcome(PlaylistOutcome::None)
            }
            PlaylistEvent::OpDone { op, result } => match result {
                Ok(()) => match op {
                    PlaylistOp::AddTrack => PlaylistUpdate::outcome(PlaylistOutcome::None),
                    PlaylistOp::Create => {
                        PlaylistUpdate::new(self.load_playlists(), PlaylistOutcome::Created)
                    }
                    PlaylistOp::Delete => {
                        PlaylistUpdate::new(self.load_playlists(), PlaylistOutcome::Deleted)
                    }
                    PlaylistOp::Rename | PlaylistOp::RemoveTrack | PlaylistOp::MoveTrack => {
                        PlaylistUpdate::new(self.load_playlists(), PlaylistOutcome::None)
                    }
                },
                Err(e) => {
                    tracing::error!("Playlist operation {op:?} failed: {e}");
                    // A failed track edit leaves the optimistic copy wrong:
                    // reload the real order.
                    let effect = match op {
                        PlaylistOp::RemoveTrack | PlaylistOp::MoveTrack => self.load_playlists(),
                        _ => Effect::none(),
                    };
                    PlaylistUpdate::new(effect, PlaylistOutcome::Failed(e))
                }
            },
            PlaylistEvent::SmartLoaded(playlists) => {
                self.playlists.smart_playlists = playlists;
                PlaylistUpdate::outcome(PlaylistOutcome::None)
            }
            PlaylistEvent::SmartTracksLoaded { tracks, total } => {
                self.playlists.smart_playlist_tracks = tracks;
                self.playlists.smart_playlist_total = total;
                PlaylistUpdate::outcome(PlaylistOutcome::None)
            }
            PlaylistEvent::SmartMatchCount { revision, count } => {
                if let (Some(editor), Some(count)) =
                    (self.playlists.smart_playlist_editor.as_mut(), count)
                {
                    editor.set_match_count(revision, count);
                }
                PlaylistUpdate::outcome(PlaylistOutcome::None)
            }
            PlaylistEvent::SmartPlayResolved(tracks) => {
                let mut update = PlaylistUpdate::outcome(PlaylistOutcome::None);
                if !tracks.is_empty() {
                    update.playback = Some(self.play_tracks(tracks, 0));
                }
                update
            }
            PlaylistEvent::Imported(Ok(summaries)) => {
                PlaylistUpdate::new(self.load_playlists(), PlaylistOutcome::Imported(summaries))
            }
            PlaylistEvent::Imported(Err(reason)) => {
                PlaylistUpdate::outcome(PlaylistOutcome::ImportFailed(reason))
            }
        }
    }
}

/// Runs a smart-playlist list query on the blocking pool; a failure is
/// logged (prefixed with `what`) and yields an empty list.
/// The event of a finished playlist mutation.
fn op_done(op: PlaylistOp, result: Result<(), ProviderError>) -> PlaylistEvent {
    PlaylistEvent::OpDone {
        op,
        result: result.map_err(|e| e.to_string()),
    }
}

fn smart_list_effect(
    what: &'static str,
    f: impl FnOnce() -> Result<Vec<SmartPlaylist>, String> + Send + 'static,
) -> Effect<PlaylistEvent> {
    Effect::future(async move {
        let playlists = crate::task::spawn_blocking(move || {
            f().unwrap_or_else(|e| {
                tracing::warn!("{what}: {e}");
                Vec::new()
            })
        })
        .await
        .unwrap_or_default();
        PlaylistEvent::SmartLoaded(playlists)
    })
}

/// Resolves `playlist` for the detail view: its tracks (limit applied) and
/// the number of tracks its rules match before the limit. Failures log and
/// yield nothing.
fn smart_detail_effect(playlist: SmartPlaylist) -> Effect<PlaylistEvent> {
    let db_path = crate::paths::library_db_path();
    Effect::future(async move {
        let (tracks, total) = crate::task::spawn_blocking(move || {
            LibraryDb::open(&db_path)
                .and_then(|db| {
                    let tracks = db.smart_playlist_tracks(&playlist, None)?;
                    let total = db.smart_playlist_match_count(&playlist)?;
                    Ok((tracks, total))
                })
                .unwrap_or_else(|e| {
                    tracing::warn!("smart playlist detail failed: {e}");
                    (Vec::new(), 0)
                })
        })
        .await
        .unwrap_or_default();
        PlaylistEvent::SmartTracksLoaded { tracks, total }
    })
}

/// Evaluates `playlist` against the library database on the blocking pool;
/// failures log and yield no tracks.
fn smart_tracks_effect(
    playlist: SmartPlaylist,
    to_event: fn(Vec<Track>) -> PlaylistEvent,
) -> Effect<PlaylistEvent> {
    let db_path = crate::paths::library_db_path();
    Effect::future(async move {
        let tracks = crate::task::spawn_blocking(move || {
            LibraryDb::open(&db_path)
                .and_then(|db| db.smart_playlist_tracks(&playlist, None))
                .unwrap_or_else(|e| {
                    tracing::warn!("smart_playlist_tracks failed: {e}");
                    Vec::new()
                })
        })
        .await
        .unwrap_or_default();
        to_event(tracks)
    })
}

/// Imports playlist files into new library playlists.
pub fn import_playlist_files(paths: Vec<PathBuf>) -> Effect<PlaylistEvent> {
    if paths.is_empty() {
        return Effect::none();
    }
    let db_path = crate::paths::library_db_path();
    Effect::blocking(
        move || playlist_io::import_blocking(&paths, &db_path),
        PlaylistEvent::Imported,
    )
}

/// Writes `source` to the M3U8 file at `path` on the blocking pool.
/// `relative` makes the entries relative to the file's directory.
pub async fn export_playlist(
    source: ExportSource,
    path: PathBuf,
    relative: bool,
) -> Result<playlist_io::ExportOutcome, String> {
    crate::task::spawn_blocking(move || {
        source.export(&path, &crate::paths::library_db_path(), relative)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::super::test_support::test_session;
    use super::*;
    use std::time::Duration;

    fn track() -> Track {
        Track {
            id: 1,
            path: PathBuf::from("/m/a.flac"),
            title: "t".into(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            genre: String::new(),
            track_number: 1,
            disc_number: 1,
            year: 0,
            duration: Duration::from_secs(1),
            bitrate: 0,
            sample_rate: 0,
            provider_id: "local".into(),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
            bit_depth: 0,
            release_type: String::new(),
            added_at: 0,
            favorited_at: 0,
        }
    }

    fn playlist(name: &str, tracks: Vec<Track>) -> Playlist {
        Playlist {
            id: name.into(),
            name: name.into(),
            track_count: tracks.len() as u32,
            tracks,
            total_duration: Duration::ZERO,
        }
    }

    #[test]
    fn playlist_tracks_is_none_for_missing_or_empty_playlists() {
        let state = PlaylistsState {
            playlists: vec![playlist("empty", vec![]), playlist("full", vec![track()])],
            ..Default::default()
        };
        assert!(state.playlist_tracks(0).is_none());
        assert_eq!(state.playlist_tracks(1).map(<[Track]>::len), Some(1));
        assert!(state.playlist_tracks(2).is_none());
        assert!(state.export_tracks(2).is_none());
        assert_eq!(
            state.export_tracks(0).map(|(n, _)| n).as_deref(),
            Some("empty")
        );
    }

    fn named(name: &str) -> Track {
        Track {
            path: PathBuf::from(format!("/m/{name}.flac")),
            title: name.into(),
            provider_id: "local".into(),
            source_uri: format!("/m/{name}.flac"),
            ..track()
        }
    }

    fn titles(state: &PlaylistsState) -> Vec<String> {
        state.playlists[0]
            .tracks
            .iter()
            .map(|t| t.title.clone())
            .collect()
    }

    #[test]
    fn local_remove_and_move_update_the_loaded_copy_and_totals() {
        let mut state = PlaylistsState {
            playlists: vec![playlist("p", vec![named("a"), named("b"), named("c")])],
            ..Default::default()
        };
        assert!(state.move_track_local(0, 2, 0));
        assert_eq!(titles(&state), ["c", "a", "b"]);
        assert!(state.remove_track_local(0, 1));
        assert_eq!(titles(&state), ["c", "b"]);
        assert_eq!(state.playlists[0].track_count, 2);
        assert_eq!(state.playlists[0].total_duration, Duration::from_secs(2));
        assert!(!state.remove_track_local(0, 2));
        assert!(!state.remove_track_local(1, 0));
        assert!(!state.move_track_local(0, 0, 0));
        assert!(!state.move_track_local(0, 0, 2));
    }

    #[test]
    fn edits_need_a_provider_that_supports_them() {
        let mut session = test_session();
        session.playlists.playlists = vec![playlist("p", vec![named("a"), named("b")])];
        assert_eq!(
            session.playlist_editing(),
            PlaylistEditing {
                remove: false,
                reorder: false
            }
        );
        assert!(session.remove_track_from_playlist(0, 0).is_none());
        assert!(session.move_playlist_track(0, 0, 1).is_none());
        assert_eq!(titles(&session.playlists), ["a", "b"]);
    }

    #[test]
    fn session_edits_reach_the_local_database() {
        use crate::provider::MusicProvider;
        use crate::provider::local::LocalProvider;
        use futures::StreamExt;
        use std::sync::Arc;

        let db = LibraryDb::open_memory().expect("db");
        for name in ["a", "b", "c"] {
            db.upsert_track(&named(name), 0).expect("track");
        }
        let ids: Vec<String> = db
            .all_tracks(None)
            .expect("tracks")
            .iter()
            .map(|t| t.id.to_string())
            .collect();
        let provider = Arc::new(LocalProvider::new(db, Vec::new()));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        let created = rt
            .block_on(provider.create_playlist("mix"))
            .expect("playlist");
        rt.block_on(provider.add_to_playlist(&created.id, &ids))
            .expect("add");

        let mut session = test_session();
        session.providers.registry.register(provider.clone());
        assert!(session.providers.registry.set_active("local"));
        assert_eq!(
            session.playlist_editing(),
            PlaylistEditing {
                remove: true,
                reorder: true
            }
        );

        rt.block_on(async {
            let load = session.load_playlists().into_stream().expect("load");
            for event in load.collect::<Vec<_>>().await {
                let _ = session.apply_playlist_event(event);
            }
            assert_eq!(titles(&session.playlists), ["a", "b", "c"]);

            // Optimistic copy first, then the provider's own order.
            let effect = session.move_playlist_track(0, 0, 2);
            assert_eq!(titles(&session.playlists), ["b", "c", "a"]);
            for event in effect
                .into_stream()
                .expect("move")
                .collect::<Vec<_>>()
                .await
            {
                let update = session.apply_playlist_event(event);
                if let Some(reload) = update.effect.into_stream() {
                    for event in reload.collect::<Vec<_>>().await {
                        let _ = session.apply_playlist_event(event);
                    }
                }
            }
            assert_eq!(titles(&session.playlists), ["b", "c", "a"]);

            let effect = session.remove_track_from_playlist(0, 1);
            for event in effect
                .into_stream()
                .expect("remove")
                .collect::<Vec<_>>()
                .await
            {
                let update = session.apply_playlist_event(event);
                if let Some(reload) = update.effect.into_stream() {
                    for event in reload.collect::<Vec<_>>().await {
                        let _ = session.apply_playlist_event(event);
                    }
                }
            }
            assert_eq!(titles(&session.playlists), ["b", "a"]);
            let stored = provider.get_playlist(&created.id).await.expect("stored");
            let stored: Vec<_> = stored.tracks.iter().map(|t| t.title.as_str()).collect();
            assert_eq!(stored, ["b", "a"]);
        });
    }

    #[test]
    fn invalid_editor_stays_open_and_saves_nothing() {
        let mut state = PlaylistsState::default();
        state.new_smart_playlist();
        // A new playlist has no name yet, so it does not validate.
        assert!(state.save_smart_editor().is_none());
        assert!(state.smart_playlist_editor.is_some());
        state.cancel_smart_editor();
        assert!(state.smart_playlist_editor.is_none());
    }

    #[test]
    fn resolved_smart_playlist_tracks_start_playing() {
        let mut session = test_session();
        let update = session.apply_playlist_event(PlaylistEvent::SmartPlayResolved(Vec::new()));
        assert!(update.playback.is_none());

        let update =
            session.apply_playlist_event(PlaylistEvent::SmartPlayResolved(vec![track(), track()]));
        assert!(update.playback.is_some());
        assert!(matches!(update.outcome, PlaylistOutcome::None));
    }

    #[test]
    fn loaded_playlists_refresh_the_search_results() {
        let mut session = test_session();
        drop(session.set_search_query("mix".into()));
        let names = |session: &Session| {
            session
                .library
                .visible_playlists(&session.playlists.playlists)
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
        };

        let loaded = vec![playlist("Road Mix", vec![]), playlist("Other", vec![])];
        let update = session.apply_playlist_event(PlaylistEvent::Loaded(loaded));
        assert!(matches!(update.outcome, PlaylistOutcome::None));
        assert_eq!(names(&session), ["Road Mix"]);
    }

    #[test]
    fn smart_detail_event_stores_the_tracks_and_the_total_before_the_limit() {
        let mut session = test_session();
        let update = session.apply_playlist_event(PlaylistEvent::SmartTracksLoaded {
            tracks: vec![track(), track()],
            total: 118,
        });
        assert!(matches!(update.outcome, PlaylistOutcome::None));
        assert_eq!(session.playlists.smart_playlist_tracks.len(), 2);
        assert_eq!(session.playlists.smart_playlist_total, 118);
    }

    #[test]
    fn selecting_a_smart_playlist_clears_the_previous_total() {
        let mut state = PlaylistsState {
            smart_playlist_tracks: vec![track()],
            smart_playlist_total: 9,
            ..Default::default()
        };
        assert!(state.select_smart_playlist(0).is_none());
        assert!(state.smart_playlist_tracks.is_empty());
        assert_eq!(state.smart_playlist_total, 0);
    }

    #[test]
    fn match_counts_reach_the_editor_unless_stale() {
        let mut session = test_session();
        session.playlists.new_smart_playlist();

        // Once per revision: the first call yields work, the second none.
        assert!(!session.playlists.request_match_count().is_none());
        assert!(session.playlists.request_match_count().is_none());

        let _ = session.apply_playlist_event(PlaylistEvent::SmartMatchCount {
            revision: 0,
            count: Some(118),
        });
        let editor = session.playlists.smart_playlist_editor.as_mut().unwrap();
        assert_eq!(editor.match_count, Some(118));

        // An edit moves the revision on: the old answer is dropped, a
        // failed (None) one changes nothing.
        editor.add_rule();
        let _ = session.apply_playlist_event(PlaylistEvent::SmartMatchCount {
            revision: 0,
            count: Some(5),
        });
        let _ = session.apply_playlist_event(PlaylistEvent::SmartMatchCount {
            revision: 1,
            count: None,
        });
        let editor = session.playlists.smart_playlist_editor.as_ref().unwrap();
        assert_eq!(editor.match_count, Some(118));
    }

    #[test]
    fn match_count_without_an_editor_does_nothing() {
        let mut session = test_session();
        assert!(session.playlists.request_match_count().is_none());
        let update = session.apply_playlist_event(PlaylistEvent::SmartMatchCount {
            revision: 0,
            count: Some(3),
        });
        assert!(matches!(update.outcome, PlaylistOutcome::None));
    }

    #[test]
    fn smart_playlist_actions_ignore_unknown_indices() {
        let mut state = PlaylistsState::default();
        state.edit_smart_playlist(0);
        assert!(state.smart_playlist_editor.is_none());
        assert!(state.delete_smart_playlist(0).is_none());
        assert!(state.select_smart_playlist(0).is_none());
        assert!(state.resolve_smart_playlist(0).is_none());
        assert!(import_playlist_files(Vec::new()).is_none());
    }
}
