// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Scans directories for audio files and extracts metadata via symphonia
//! (see `super::tags`).

use super::scan_progress::ScanProgress;
use super::tags;
use super::{Track, db::LibraryDb};
use crate::player::engine::decoder::SUPPORTED_EXTENSIONS;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Scans music directories and populates the library database.
pub struct LibraryScanner;

/// Upserts per transaction during a full scan.
const UPSERT_BATCH: usize = 200;

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

        // Paths the walk reached; anything under a complete root that is
        // not here is gone, with no second stat pass over the database.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for dir in roots {
            progress.set_root(dir);
            let mut complete = true;
            // Files that are new or changed, probed in parallel below.
            let mut todo: Vec<(PathBuf, i64)> = Vec::new();
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

                let metadata = match std::fs::metadata(path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        progress.advance();
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!("Cannot access track {}: {error}", path.display());
                        seen.insert(path.to_string_lossy().into_owned());
                        complete = false;
                        progress.advance();
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
                seen.insert(path_str.clone().into_owned());
                if known_mtimes.get(path_str.as_ref()) == Some(&mtime) {
                    progress.advance();
                    continue;
                }
                todo.push((path.to_path_buf(), mtime));
            }

            for chunk in todo.chunks(UPSERT_BATCH) {
                let probed = Self::probe_parallel(chunk, progress);
                db.begin_batch()?;
                for ((path, mtime), result) in chunk.iter().zip(probed) {
                    match result {
                        Ok(track) => match db.upsert_track(&track, *mtime) {
                            Ok(()) => count += 1,
                            Err(e) => {
                                tracing::error!("Failed to insert track {}: {e}", path.display());
                            }
                        },
                        Err(e) => {
                            tracing::warn!("Failed to read metadata for {}: {e}", path.display());
                        }
                    }
                }
                db.commit_batch()?;
            }
            if complete {
                complete_roots.push(dir.clone());
            }
        }

        let removed = db.remove_local_tracks_beneath(&complete_roots, |path| {
            !seen.contains(path.to_string_lossy().as_ref())
        })?;
        if removed > 0 {
            tracing::info!("Removed {removed} missing tracks from library");
        }
        Ok(count)
    }

    /// Read the tags of `files` on a few worker threads (tag probing is
    /// IO- and parse-bound and was strictly serial), in input order.
    fn probe_parallel(
        files: &[(PathBuf, i64)],
        progress: &ScanProgress,
    ) -> Vec<Result<Track, String>> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let workers = std::thread::available_parallelism()
            .map_or(2, std::num::NonZero::get)
            .clamp(1, 8)
            .min(files.len().max(1));
        let next = AtomicUsize::new(0);
        let results = std::sync::Mutex::new(
            std::iter::repeat_with(|| None)
                .take(files.len())
                .collect::<Vec<Option<Result<Track, String>>>>(),
        );
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((path, _)) = files.get(i) else { break };
                        let result = Self::read_metadata(path);
                        progress.advance();
                        if let Ok(mut all) = results.lock() {
                            all[i] = Some(result);
                        }
                    }
                });
            }
        });
        results
            .into_inner()
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.unwrap_or_else(|| Err("probe worker ended early".to_string())))
            .collect()
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
        Self::scan_paths_changed(db, paths).map(|changed| changed.len())
    }

    /// [`Self::scan_paths`], returning the path of every track that was
    /// added, updated or removed.
    pub fn scan_paths_changed(db: &LibraryDb, paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
        let mut changed: Vec<PathBuf> = Vec::new();

        for path in paths {
            if path.is_dir() {
                // A created or moved-in folder: index the audio beneath it.
                for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
                    if entry.file_type().is_file() && Self::scan_file(db, entry.path()) > 0 {
                        changed.push(entry.path().to_path_buf());
                    }
                }
            } else if path.exists() {
                if Self::scan_file(db, path) > 0 {
                    changed.push(path.clone());
                }
            } else if Self::is_audio_file(path) {
                // File was deleted — remove from database.
                let path_str = path.to_string_lossy();
                if db.get_track_mtime(&path_str).is_some() {
                    if let Err(e) = db.remove_track_by_path(&path_str) {
                        tracing::error!("Failed to remove track {}: {e}", path.display());
                    } else {
                        changed.push(path.clone());
                    }
                }
            } else {
                // A deleted or moved-away folder: drop what was beneath it.
                match db.remove_local_tracks_under(path) {
                    // The folder itself stands for the tracks beneath it.
                    Ok(n) => changed.extend((n > 0).then(|| path.clone())),
                    Err(e) => tracing::error!("Failed to remove {}: {e}", path.display()),
                }
            }
        }

        Ok(changed)
    }

    /// Add or update one existing file; returns `1` when the library changed.
    fn scan_file(db: &LibraryDb, path: &Path) -> usize {
        if !Self::is_audio_file(path) {
            return 0;
        }
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let path_str = path.to_string_lossy();
        if db.get_track_mtime(&path_str) == Some(mtime) {
            return 0; // File hasn't changed
        }
        match Self::read_metadata(path) {
            Ok(track) => match db.upsert_track(&track, mtime) {
                Ok(()) => return 1,
                Err(e) => tracing::error!("Failed to upsert track {}: {e}", path.display()),
            },
            Err(e) => tracing::warn!("Failed to read metadata for {}: {e}", path.display()),
        }
        0
    }

    /// Check if a path is a supported audio file.
    pub(crate) fn is_audio_file(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                SUPPORTED_EXTENSIONS
                    .iter()
                    .any(|supported| supported.eq_ignore_ascii_case(ext))
            })
    }

    /// Read metadata from an audio file via symphonia (see `super::tags`).
    pub fn read_metadata(path: &Path) -> Result<Track, String> {
        let probed = tags::probe(path, false).ok_or_else(|| "Cannot probe file".to_string())?;
        Ok(Self::track_from_probed(path, probed))
    }

    /// The library [`Track`] of the file `path` (the title falls back to the
    /// file name) from what probing it found. Shared by the native scan and
    /// the browser's, which probes through other means.
    pub(crate) fn track_from_probed(path: &Path, probed: tags::ProbedFile) -> Track {
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
        Track {
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
            suffix: super::suffix_of(&path.to_string_lossy()),
            added_at: 0,
            favorited_at: 0,
        }
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
            suffix: String::new(),
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
