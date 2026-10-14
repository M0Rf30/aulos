// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The blocking half of playlist import/export: writing a track list as
//! M3U8 and reading playlist files into new library playlists. Parsing,
//! rendering and matching live in [`super::m3u`]; file dialogs and result
//! messages belong to the UI.

use super::m3u;
use super::smart_playlist::SmartPlaylist;
use super::{LibraryDb, Track};
use std::path::{Path, PathBuf};

/// Result of a finished playlist export.
#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub path: PathBuf,
    pub written: usize,
    /// Tracks not written because they are not local files.
    pub skipped: usize,
}

/// Result of importing one playlist file.
#[derive(Debug, Clone)]
pub struct ImportSummary {
    pub name: String,
    pub matched: usize,
    pub unmatched: usize,
}

/// What an export writes: a ready track list, or a smart playlist whose
/// tracks are evaluated against the library database at export time.
pub enum ExportSource {
    Tracks(Vec<Track>),
    Smart(SmartPlaylist),
}

impl ExportSource {
    /// Resolves the tracks (evaluating a smart playlist against the library
    /// database) and writes them to `path` as M3U8. With `relative`, entries
    /// are written relative to the file's directory.
    pub fn export(
        self,
        path: &Path,
        db_path: &Path,
        relative: bool,
    ) -> Result<ExportOutcome, String> {
        let tracks = match self {
            Self::Tracks(tracks) => tracks,
            Self::Smart(playlist) => {
                LibraryDb::open(db_path).and_then(|db| db.smart_playlist_tracks(&playlist, None))?
            }
        };
        write_export(path, &tracks, relative)
    }
}

/// File-name-safe version of a playlist name.
#[must_use]
pub fn sanitize_file_name(name: &str) -> String {
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

/// Reads each playlist file, matches its entries against the local library
/// and stores the result as a new playlist.
pub fn import_blocking(paths: &[PathBuf], db_path: &Path) -> Result<Vec<ImportSummary>, String> {
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
