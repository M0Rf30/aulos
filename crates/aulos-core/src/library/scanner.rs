// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Scans directories for audio files and extracts metadata via symphonia
//! (see `super::tags`).

use super::scan_progress::ScanProgress;
use super::tags;
use super::{Track, db::LibraryDb};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Supported audio file extensions.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "m4a", "aac", "wav", "wma", "ape", "wv", "dsf", "dff",
];

/// Scans music directories and populates the library database.
pub struct LibraryScanner;

impl LibraryScanner {
    /// Scan the given directories and upsert tracks into the database.
    /// Returns the number of new or updated tracks.
    ///
    /// `progress` is reset, then filled with the number of audio files found
    /// (a first, cheap walk) and advanced as each one is processed.
    #[tracing::instrument(skip(db, progress), level = "debug")]
    pub fn scan(
        db: &LibraryDb,
        dirs: &[PathBuf],
        progress: &ScanProgress,
    ) -> Result<usize, String> {
        let mut count = 0;
        let mut complete_roots = Vec::new();
        let known_mtimes = db.track_mtimes("local")?;

        progress.reset();
        let roots: Vec<&PathBuf> = dirs.iter().filter(|d| Self::is_usable_root(d)).collect();
        for dir in &roots {
            progress.set_root(dir);
            let found = WalkDir::new(dir)
                .follow_links(true)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|e| Self::is_audio_file(e.path()))
                .count();
            progress.add_total(found);
        }

        for dir in roots {
            progress.set_root(dir);
            let mut complete = true;
            for entry in WalkDir::new(dir).follow_links(true) {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        tracing::warn!(
                            "Failed to traverse music directory {}: {error}",
                            dir.display()
                        );
                        complete = false;
                        continue;
                    }
                };
                let path = entry.path();
                if !Self::is_audio_file(path) {
                    continue;
                }
                progress.advance();

                let metadata = match std::fs::metadata(path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        tracing::warn!("Cannot access track {}: {error}", path.display());
                        complete = false;
                        continue;
                    }
                };
                let mtime = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs() as i64)
                    .unwrap_or(0);

                let path_str = path.to_string_lossy();
                if let Some(&existing_mtime) = known_mtimes.get(path_str.as_ref())
                    && existing_mtime == mtime
                {
                    continue;
                }

                match Self::read_metadata(path) {
                    Ok(track) => {
                        if let Err(e) = db.upsert_track(&track, mtime) {
                            tracing::error!("Failed to insert track {}: {e}", path.display());
                        } else {
                            count += 1;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to read metadata for {}: {e}", path.display());
                    }
                }
            }
            if complete {
                complete_roots.push(dir.clone());
            }
        }

        let removed = db.remove_missing_tracks(&complete_roots)?;
        if removed > 0 {
            tracing::info!("Removed {removed} missing tracks from library");
        }
        Ok(count)
    }

    /// Whether `dir` can be walked: it exists and is a directory.
    fn is_usable_root(dir: &Path) -> bool {
        match std::fs::metadata(dir) {
            Ok(metadata) if metadata.is_dir() => true,
            Ok(_) => {
                tracing::warn!("Music root is not a directory: {}", dir.display());
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!("Music directory does not exist: {}", dir.display());
                false
            }
            Err(error) => {
                tracing::warn!("Cannot access music directory {}: {error}", dir.display());
                false
            }
        }
    }

    /// Incremental scan of specific paths.
    ///
    /// Unlike `scan()` which walks entire directories, this method processes
    /// only the given paths — typically received from a filesystem watcher.
    /// For each path:
    /// - If the file exists and is a supported audio file, upsert it (add/update).
    /// - If the file no longer exists, remove it from the database.
    ///
    /// Returns the number of tracks added, updated, or removed.
    #[tracing::instrument(skip(db), level = "debug")]
    pub fn scan_paths(db: &LibraryDb, paths: &[PathBuf]) -> Result<usize, String> {
        let mut count = 0;

        for path in paths {
            if path.exists() {
                // File exists — add or update if it's an audio file.
                if !Self::is_audio_file(path) {
                    continue;
                }

                let mtime = std::fs::metadata(path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);

                let path_str = path.to_string_lossy();
                if let Some(existing_mtime) = db.get_track_mtime(&path_str)
                    && existing_mtime == mtime
                {
                    continue; // File hasn't changed
                }

                match Self::read_metadata(path) {
                    Ok(track) => {
                        if let Err(e) = db.upsert_track(&track, mtime) {
                            tracing::error!("Failed to upsert track {}: {e}", path.display());
                        } else {
                            count += 1;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to read metadata for {}: {e}", path.display());
                    }
                }
            } else {
                // File was deleted — remove from database.
                let path_str = path.to_string_lossy();
                if db.get_track_mtime(&path_str).is_some() {
                    if let Err(e) = db.remove_track_by_path(&path_str) {
                        tracing::error!("Failed to remove track {}: {e}", path.display());
                    } else {
                        count += 1;
                    }
                }
            }
        }

        Ok(count)
    }

    /// Check if a path is a supported audio file.
    fn is_audio_file(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
    }

    /// Read metadata from an audio file via symphonia (see `super::tags`).
    pub fn read_metadata(path: &Path) -> Result<Track, String> {
        let probed = tags::probe(path, false).ok_or_else(|| "Cannot probe file".to_string())?;
        let t = probed.tags;

        // Use filename as title if tag is missing.
        let title = t.title.filter(|s| !s.is_empty()).unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string()
        });

        let artist = t.artist.unwrap_or_default();

        // Fall back album_artist -> artist.
        let album_artist = t.album_artist.filter(|s| !s.is_empty()).unwrap_or_else(|| {
            if t.compilation {
                super::compilations::VARIOUS_ARTISTS.to_string()
            } else {
                artist.clone()
            }
        });

        let source_uri = path.to_string_lossy().to_string();
        Ok(Track {
            id: 0,
            path: path.to_path_buf(),
            title,
            artist,
            album_artist,
            album: t.album.unwrap_or_default(),
            genre: t.genre.unwrap_or_default(),
            track_number: t.track_number.unwrap_or(0),
            disc_number: t.disc_number.unwrap_or(0),
            year: t.year.unwrap_or(0),
            duration: probed.properties.duration,
            bitrate: probed.properties.bitrate,
            sample_rate: probed.properties.sample_rate,
            bit_depth: probed.properties.bit_depth,
            provider_id: Arc::from("local"),
            source_uri,
            is_favorite: false,
            rating: None,
            rg_track_gain: t.rg_track_gain,
            rg_album_gain: t.rg_album_gain,
            release_type: t.release_type,
            added_at: 0,
            favorited_at: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aulos-scanner-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn local_track(path: PathBuf) -> Track {
        Track {
            id: 0,
            path: path.clone(),
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
            provider_id: Arc::from("local"),
            source_uri: path.to_string_lossy().into_owned(),
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

    #[test]
    fn unavailable_root_preserves_existing_tracks() {
        let db = LibraryDb::open_memory().unwrap();
        let root = temp_root("missing-root");
        db.upsert_track(&local_track(root.join("gone.mp3")), 0)
            .unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            LibraryScanner::scan(&db, &[root], &ScanProgress::default()).unwrap(),
            0
        );
        assert_eq!(db.all_tracks(None).unwrap().len(), 1);
    }

    #[test]
    fn successful_root_scan_removes_deleted_tracks() {
        let db = LibraryDb::open_memory().unwrap();
        let root = temp_root("deleted-track");
        db.upsert_track(&local_track(root.join("gone.mp3")), 0)
            .unwrap();

        assert_eq!(
            LibraryScanner::scan(&db, std::slice::from_ref(&root), &ScanProgress::default())
                .unwrap(),
            0
        );
        assert!(db.all_tracks(None).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
