// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! M3U/M3U8/PLS playlist import and M3U export: portal file dialogs, the
//! background read/write/match work, and the result toasts. The parsing,
//! writing and matching logic itself lives in `crate::library::m3u`.

use super::{AppModel, Message, online_db_path};
use crate::fl;
use crate::library::m3u;
use crate::library::smart_playlist::SmartPlaylist;
use crate::library::{LibraryDb, Track};
use cosmic::prelude::*;
use cosmic::widget;
use std::path::{Path, PathBuf};

/// Messages of the playlist import/export feature.
#[derive(Debug, Clone)]
pub enum PlaylistIo {
    /// Export the playlist at this index of `AppModel::playlists`.
    Export(usize),
    /// Export the smart playlist at this index of `AppModel::smart_playlists`.
    ExportSmart(usize),
    /// Export finished (`Ok(None)` = the dialog was cancelled).
    Exported(Result<Option<ExportOutcome>, String>),
    /// Show the open-file dialog to pick playlist files to import.
    PickFiles,
    /// Import these playlist files (from the dialog, the CLI or `OpenUri`).
    Import(Vec<PathBuf>),
    /// Import finished, one summary per playlist file.
    Imported(Result<Vec<ImportSummary>, String>),
}

#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub path: PathBuf,
    pub written: usize,
    /// Tracks not written because they are not local files.
    pub skipped: usize,
}

#[derive(Debug, Clone)]
pub struct ImportSummary {
    pub name: String,
    pub matched: usize,
    pub unmatched: usize,
}

enum ExportSource {
    Tracks(Vec<Track>),
    Smart(SmartPlaylist),
}

impl AppModel {
    pub(super) fn update_playlist_io(&mut self, msg: PlaylistIo) -> Task<cosmic::Action<Message>> {
        match msg {
            PlaylistIo::Export(index) => {
                let Some(playlist) = self.playlists.get(index) else {
                    return Task::none();
                };
                if playlist.tracks.is_empty() {
                    return self.push_toast(widget::toaster::Toast::new(fl!(
                        "toast-playlist-export-empty"
                    )));
                }
                export_task(
                    playlist.name.clone(),
                    ExportSource::Tracks(playlist.tracks.clone()),
                    self.config.m3u_relative_paths,
                )
            }
            PlaylistIo::ExportSmart(index) => {
                let Some(playlist) = self.smart_playlists.get(index) else {
                    return Task::none();
                };
                export_task(
                    playlist.name.clone(),
                    ExportSource::Smart(playlist.clone()),
                    self.config.m3u_relative_paths,
                )
            }
            PlaylistIo::Exported(Ok(Some(outcome))) => {
                let file = outcome
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let text = if outcome.skipped > 0 {
                    fl!(
                        "toast-playlist-exported-partial",
                        count = outcome.written,
                        skipped = outcome.skipped,
                        file = file
                    )
                } else {
                    fl!(
                        "toast-playlist-exported",
                        count = outcome.written,
                        file = file
                    )
                };
                self.push_toast(widget::toaster::Toast::new(text))
            }
            PlaylistIo::Exported(Ok(None)) => Task::none(),
            PlaylistIo::Exported(Err(reason)) => self.push_toast(widget::toaster::Toast::new(fl!(
                "toast-playlist-export-failed",
                reason = reason
            ))),
            PlaylistIo::PickFiles => cosmic::task::future(async {
                let paths = pick_playlist_files().await;
                cosmic::Action::App(match paths {
                    Ok(paths) => Message::PlaylistIo(PlaylistIo::Import(paths)),
                    Err(e) => Message::PlaylistIo(PlaylistIo::Imported(Err(e))),
                })
            }),
            PlaylistIo::Import(paths) => self.import_playlist_files(paths),
            PlaylistIo::Imported(Ok(summaries)) => {
                let toast = import_toast_text(&summaries);
                let reload = self.load_playlists();
                let toast_task = self.push_toast(widget::toaster::Toast::new(toast));
                Task::batch([reload, toast_task])
            }
            PlaylistIo::Imported(Err(reason)) => self.push_toast(widget::toaster::Toast::new(fl!(
                "toast-playlist-import-failed",
                reason = reason
            ))),
        }
    }

    /// Imports playlist files into new library playlists (background task).
    /// Also the entry point for playlist files given on the command line or
    /// via MPRIS `OpenUri` (see `open_files`).
    pub(super) fn import_playlist_files(
        &mut self,
        paths: Vec<PathBuf>,
    ) -> Task<cosmic::Action<Message>> {
        if paths.is_empty() {
            return Task::none();
        }
        let db_path = online_db_path();
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || import_blocking(&paths, &db_path))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::PlaylistIo(PlaylistIo::Imported(result)))
        })
    }
}

fn export_task(
    name: String,
    source: ExportSource,
    relative: bool,
) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let result = export_flow(&name, source, relative).await;
        cosmic::Action::App(Message::PlaylistIo(PlaylistIo::Exported(result)))
    })
}

/// File-name-safe version of a playlist name.
fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    if cleaned.is_empty() {
        "playlist".to_string()
    } else {
        cleaned.to_string()
    }
}

async fn export_flow(
    name: &str,
    source: ExportSource,
    relative: bool,
) -> Result<Option<ExportOutcome>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let title = fl!("export-playlist-dialog-title");
    let default_name = format!("{}.m3u8", sanitize_file_name(name));
    let filter = FileFilter::new("M3U playlist").glob("*.m3u8").glob("*.m3u");

    let request = SelectedFiles::save_file()
        .title(title.as_str())
        .current_name(default_name.as_str())
        .modal(true)
        .filter(filter)
        .send()
        .await
        .map_err(|e| format!("Portal request failed: {e}"))?;
    let selected = match request.response() {
        Ok(selected) => selected,
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => return Ok(None),
        Err(e) => return Err(format!("Portal response failed: {e}")),
    };
    let Some(mut path) = selected
        .uris()
        .first()
        .and_then(|uri| crate::file_uri_to_path(uri.as_str()))
    else {
        return Ok(None);
    };
    if path.extension().is_none() {
        path.set_extension("m3u8");
    }

    tokio::task::spawn_blocking(move || {
        let tracks = match source {
            ExportSource::Tracks(tracks) => tracks,
            ExportSource::Smart(playlist) => LibraryDb::open(&online_db_path())
                .and_then(|db| db.smart_playlist_tracks(&playlist, None))?,
        };
        write_export(&path, &tracks, relative).map(Some)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn write_export(path: &Path, tracks: &[Track], relative: bool) -> Result<ExportOutcome, String> {
    let base = if relative { path.parent() } else { None };
    let text = m3u::write_m3u(tracks, base);
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    let written = m3u::exportable_count(tracks);
    Ok(ExportOutcome {
        path: path.to_path_buf(),
        written,
        skipped: tracks.len() - written,
    })
}

async fn pick_playlist_files() -> Result<Vec<PathBuf>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let title = fl!("import-playlist-dialog-title");
    let mut filter = FileFilter::new("Playlists (M3U, M3U8, PLS)");
    for ext in m3u::PLAYLIST_EXTENSIONS {
        filter = filter.glob(&format!("*.{ext}"));
    }
    let request = SelectedFiles::open_file()
        .title(title.as_str())
        .multiple(true)
        .modal(true)
        .filter(filter)
        .send()
        .await
        .map_err(|e| format!("Portal request failed: {e}"))?;
    match request.response() {
        Ok(selected) => Ok(selected
            .uris()
            .iter()
            .filter_map(|uri| crate::file_uri_to_path(uri.as_str()))
            .collect()),
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => Ok(Vec::new()),
        Err(e) => Err(format!("Portal response failed: {e}")),
    }
}

/// Reads each playlist file, matches its entries against the local library
/// and stores the result as a new playlist.
fn import_blocking(paths: &[PathBuf], db_path: &Path) -> Result<Vec<ImportSummary>, String> {
    let db = LibraryDb::open(db_path)?;
    let library = db.all_tracks(Some("local"))?;
    let mut summaries = Vec::new();
    let mut last_error = None;
    for path in paths {
        let (name, entries) = match m3u::read_playlist_file(path) {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::warn!("Playlist import failed for {}: {e}", path.display());
                last_error = Some(e);
                continue;
            }
        };
        let dir = path.parent().unwrap_or_else(|| Path::new("/"));
        let result = m3u::match_entries(&entries, dir, &library);
        let playlist = db.create_playlist(&name)?;
        if !result.matched.is_empty() {
            let ids: Vec<String> = result.matched.iter().map(|id| id.to_string()).collect();
            db.add_to_playlist(&playlist.id, &ids)?;
        }
        summaries.push(ImportSummary {
            name,
            matched: result.matched.len(),
            unmatched: result.unmatched,
        });
    }
    match (summaries.is_empty(), last_error) {
        (true, Some(e)) => Err(e),
        _ => Ok(summaries),
    }
}

fn import_toast_text(summaries: &[ImportSummary]) -> String {
    let matched: usize = summaries.iter().map(|s| s.matched).sum();
    let unmatched: usize = summaries.iter().map(|s| s.unmatched).sum();
    match summaries {
        [one] if unmatched == 0 => fl!(
            "toast-playlist-imported",
            name = one.name.clone(),
            count = matched
        ),
        [one] => fl!(
            "toast-playlist-imported-partial",
            name = one.name.clone(),
            count = matched,
            missing = unmatched
        ),
        many if unmatched == 0 => fl!(
            "toast-playlists-imported",
            playlists = many.len(),
            count = matched
        ),
        many => fl!(
            "toast-playlists-imported-partial",
            playlists = many.len(),
            count = matched,
            missing = unmatched
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_file_name("A/B: C?"), "A_B_ C_");
        assert_eq!(sanitize_file_name("  ..  "), "playlist");
        assert_eq!(sanitize_file_name("Road Trip"), "Road Trip");
    }
}
