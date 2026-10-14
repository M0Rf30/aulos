// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Saved playlists (provider-backed), smart playlists (rule-based, evaluated
//! against the library database) and playlist file import.
//!
//! Workflows return an [`Effect`] of [`PlaylistEvent`]s; the UI feeds each
//! event back through [`Session::apply_playlist_event`].

use super::Session;
use crate::library::playlist_io::{self, ExportSource, ImportSummary};
use crate::library::smart_playlist::{EditorState, SmartPlaylist};
use crate::library::{LibraryDb, Playlist, Track};
use crate::task::Effect;
use std::path::PathBuf;

/// Which provider playlist mutation a [`PlaylistEvent::OpDone`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistOp {
    AddTrack,
    Create,
    Delete,
    Rename,
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
    /// Resolved tracks of the selected smart playlist (detail view).
    SmartTracksLoaded(Vec<Track>),
    /// Resolved tracks of a smart playlist, to play right away.
    SmartPlayResolved(Vec<Track>),
    /// Playlist files imported, one summary per file.
    Imported(Result<Vec<ImportSummary>, String>),
}

/// What the UI must do after [`Session::apply_playlist_event`], besides
/// running the returned effect.
#[derive(Debug, Clone)]
pub enum PlaylistOutcome {
    /// Nothing.
    None,
    /// `PlaylistsState::playlists` was replaced.
    PlaylistsChanged,
    /// A playlist was created.
    Created,
    /// A playlist was deleted.
    Deleted,
    /// A provider mutation failed; the UI shows `reason`.
    Failed(String),
    /// Smart-playlist tracks to start playing (never empty).
    Play(Vec<Track>),
    /// Playlist files were imported.
    Imported(Vec<ImportSummary>),
    /// Importing playlist files failed.
    ImportFailed(String),
}

/// Saved playlists and smart (rule-based) playlists.
#[derive(Default)]
pub struct PlaylistsState {
    pub playlists: Vec<Playlist>,
    pub smart_playlists: Vec<SmartPlaylist>,
    /// Resolved tracks for the currently viewed smart playlist.
    pub smart_playlist_tracks: Vec<Track>,
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
        match self.smart_playlists.get(idx).cloned() {
            Some(playlist) => smart_tracks_effect(playlist, PlaylistEvent::SmartTracksLoaded),
            None => Effect::none(),
        }
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
            let playlists = tokio::task::spawn_blocking(move || {
                let mut playlists = provider.list_playlists().unwrap_or_else(|e| {
                    tracing::warn!("list_playlists failed: {e}");
                    Vec::new()
                });
                // `list_playlists` returns summaries only (no tracks) for
                // the local provider; fetch each playlist's tracks so the
                // detail view, "play all" and M3U export have them.
                for playlist in playlists.iter_mut() {
                    if playlist.tracks.is_empty()
                        && playlist.track_count > 0
                        && let Ok(full) = provider.get_playlist(&playlist.id)
                    {
                        playlist.tracks = full.tracks;
                    }
                }
                playlists
            })
            .await
            .unwrap_or_default();
            PlaylistEvent::Loaded(playlists)
        })
    }

    /// Runs a playlist mutation against the active provider off the UI
    /// thread.
    fn playlist_op(
        &self,
        op: PlaylistOp,
        f: impl FnOnce(&dyn crate::provider::MusicProvider) -> Result<(), String> + Send + 'static,
    ) -> Effect<PlaylistEvent> {
        let Some(provider) = self.providers.registry.active_shared() else {
            return Effect::none();
        };
        Effect::blocking(
            move || f(provider.as_ref()),
            move |result| PlaylistEvent::OpDone { op, result },
        )
    }

    /// Creates a playlist on the active provider.
    pub fn create_playlist(&self, name: String) -> Effect<PlaylistEvent> {
        self.playlist_op(PlaylistOp::Create, move |p| {
            p.create_playlist(&name)
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    /// Deletes the playlist at `idx` of `playlists.playlists`.
    pub fn delete_playlist(&self, idx: usize) -> Effect<PlaylistEvent> {
        let Some(playlist) = self.playlists.playlists.get(idx) else {
            return Effect::none();
        };
        let id = playlist.id.clone();
        self.playlist_op(PlaylistOp::Delete, move |p| {
            p.delete_playlist(&id).map_err(|e| e.to_string())
        })
    }

    /// Renames the playlist at `idx` of `playlists.playlists`.
    pub fn rename_playlist(&self, idx: usize, new_name: String) -> Effect<PlaylistEvent> {
        let Some(playlist) = self.playlists.playlists.get(idx) else {
            return Effect::none();
        };
        let id = playlist.id.clone();
        self.playlist_op(PlaylistOp::Rename, move |p| {
            p.rename_playlist(&id, &new_name).map_err(|e| e.to_string())
        })
    }

    /// Appends a track (by source URI) to the playlist `playlist_id`.
    pub fn add_to_playlist(
        &self,
        playlist_id: String,
        track_source_uri: String,
    ) -> Effect<PlaylistEvent> {
        self.playlist_op(PlaylistOp::AddTrack, move |p| {
            p.add_to_playlist(&playlist_id, &[track_source_uri])
                .map_err(|e| e.to_string())
        })
    }

    /// Removing a track from a playlist needs provider-specific support that
    /// isn't uniformly available yet: logs and reloads the playlists.
    pub fn remove_playlist_track(&self) -> Effect<PlaylistEvent> {
        tracing::info!("Track removal from playlist not yet implemented");
        self.load_playlists()
    }

    /// Applies an async playlist result; returns the follow-up work to run
    /// and what the UI must react to.
    pub fn apply_playlist_event(
        &mut self,
        event: PlaylistEvent,
    ) -> (Effect<PlaylistEvent>, PlaylistOutcome) {
        match event {
            PlaylistEvent::Loaded(playlists) => {
                self.playlists.playlists = playlists;
                (Effect::none(), PlaylistOutcome::PlaylistsChanged)
            }
            PlaylistEvent::OpDone { op, result } => match result {
                Ok(()) => match op {
                    PlaylistOp::AddTrack => (Effect::none(), PlaylistOutcome::None),
                    PlaylistOp::Create => (self.load_playlists(), PlaylistOutcome::Created),
                    PlaylistOp::Delete => (self.load_playlists(), PlaylistOutcome::Deleted),
                    PlaylistOp::Rename => (self.load_playlists(), PlaylistOutcome::None),
                },
                Err(e) => {
                    tracing::error!("Playlist operation {op:?} failed: {e}");
                    (Effect::none(), PlaylistOutcome::Failed(e))
                }
            },
            PlaylistEvent::SmartLoaded(playlists) => {
                self.playlists.smart_playlists = playlists;
                (Effect::none(), PlaylistOutcome::None)
            }
            PlaylistEvent::SmartTracksLoaded(tracks) => {
                self.playlists.smart_playlist_tracks = tracks;
                (Effect::none(), PlaylistOutcome::None)
            }
            PlaylistEvent::SmartPlayResolved(tracks) => (
                Effect::none(),
                if tracks.is_empty() {
                    PlaylistOutcome::None
                } else {
                    PlaylistOutcome::Play(tracks)
                },
            ),
            PlaylistEvent::Imported(Ok(summaries)) => {
                (self.load_playlists(), PlaylistOutcome::Imported(summaries))
            }
            PlaylistEvent::Imported(Err(reason)) => {
                (Effect::none(), PlaylistOutcome::ImportFailed(reason))
            }
        }
    }
}

/// Runs a smart-playlist list query on the blocking pool; a failure is
/// logged (prefixed with `what`) and yields an empty list.
fn smart_list_effect(
    what: &'static str,
    f: impl FnOnce() -> Result<Vec<SmartPlaylist>, String> + Send + 'static,
) -> Effect<PlaylistEvent> {
    Effect::future(async move {
        let playlists = tokio::task::spawn_blocking(move || {
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

/// Evaluates `playlist` against the library database on the blocking pool;
/// failures log and yield no tracks.
fn smart_tracks_effect(
    playlist: SmartPlaylist,
    to_event: fn(Vec<Track>) -> PlaylistEvent,
) -> Effect<PlaylistEvent> {
    let db_path = crate::paths::library_db_path();
    Effect::future(async move {
        let tracks = tokio::task::spawn_blocking(move || {
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
    tokio::task::spawn_blocking(move || {
        source.export(&path, &crate::paths::library_db_path(), relative)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
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
