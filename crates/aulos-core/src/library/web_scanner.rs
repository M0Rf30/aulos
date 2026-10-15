// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The library scan in the browser: the counterpart of
//! [`LibraryScanner::scan`](super::LibraryScanner::scan) over the music
//! folders the user picked (see [`crate::web_fs`]).
//!
//! Reading a file in a page is an asynchronous `Blob` slice, so a track is
//! probed from its first and last bytes only (where tags, stream headers and
//! trailing tag blocks live), loaded into a [`SparseFile`]. A probe that needs
//! more (a cover bigger than the head, an MP4 with its index at the end of a
//! long file) is repeated on the whole file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::scan_progress::ScanProgress;
use super::sparse::SparseFile;
use super::tags::{self, ProbedFile};
use super::{LibraryDb, LibraryScanner};
use crate::virtual_path;
use crate::web_fs::{self, RootStatus, Yielder};

/// Bytes loaded from the start of a file for the probe.
const HEAD_BYTES: u64 = 4 * 1024 * 1024;
/// Bytes loaded from the end of a file for the probe.
const TAIL_BYTES: u64 = 1024 * 1024;

/// Probes the browser file `file`, named `path`.
///
/// # Errors
/// A message when the file cannot be read or is not audio the probe knows.
pub async fn probe_file(
    path: &Path,
    file: &web_sys::File,
    want_pictures: bool,
) -> Result<ProbedFile, String> {
    let size = web_fs::info_of(file).0;
    if size > HEAD_BYTES + TAIL_BYTES {
        let mut sparse = SparseFile::new(size);
        sparse.insert(0, web_fs::read_range(file, 0, HEAD_BYTES).await?);
        sparse.insert(
            size - TAIL_BYTES,
            web_fs::read_range(file, size - TAIL_BYTES, TAIL_BYTES).await?,
        );
        let hole = sparse.hole_flag();
        let probed = tags::probe_sparse(path, sparse, want_pictures);
        if let Some(probed) = probed
            && !hole.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(probed);
        }
    }
    let whole = SparseFile::whole(web_fs::read_range(file, 0, size).await?);
    tags::probe_sparse(path, whole, want_pictures).ok_or_else(|| "Cannot probe file".to_owned())
}

/// Scans the music folders `dirs`, upserting new and changed tracks (a file
/// whose modification time is unchanged is skipped) and removing tracks whose
/// file vanished from a folder that was read completely. Folders that cannot
/// be read now (no permission yet, not picked again after a reload) are
/// skipped, leaving their tracks alone; the tracks of folders no longer listed
/// are dropped. Returns the number of tracks added, updated or removed.
/// `progress` is reset, then filled and advanced as native scans do.
///
/// # Errors
/// A message when the library database cannot be read or written.
pub async fn scan(
    db_path: &Path,
    dirs: &[PathBuf],
    progress: &ScanProgress,
) -> Result<usize, String> {
    let db = LibraryDb::open(db_path)?;
    let known_mtimes = db.track_mtimes("local")?;

    progress.reset();
    let mut listings = Vec::new();
    for dir in dirs {
        progress.set_root(dir);
        match web_fs::status(dir) {
            RootStatus::Ready => {}
            RootStatus::NeedsPermission | RootStatus::NeedsRepick => {
                tracing::info!("Skipping {}: no access to it yet", dir.display());
                continue;
            }
        }
        match web_fs::list_audio(dir).await {
            Ok(listing) => {
                progress.add_total(listing.files.len());
                listings.push((dir, listing));
            }
            Err(error) => tracing::warn!("Cannot read {}: {error}", dir.display()),
        }
    }

    let mut count = 0;
    let mut complete_roots = Vec::new();
    let mut present: HashSet<PathBuf> = HashSet::new();
    let mut yielder = Yielder::new();
    for (dir, listing) in listings {
        progress.set_root(dir);
        for listed in &listing.files {
            progress.advance();
            present.insert(listed.path.clone());
            let mtime = listed.info().1;
            let path_str = listed.path.to_string_lossy();
            if known_mtimes.get(path_str.as_ref()) == Some(&mtime) {
                continue;
            }
            match probe_file(&listed.path, &listed.file, false).await {
                Ok(probed) => {
                    let track = LibraryScanner::track_from_probed(&listed.path, probed);
                    match db.upsert_track(&track, mtime) {
                        Ok(()) => count += 1,
                        Err(e) => {
                            tracing::error!("Failed to insert track {}: {e}", listed.path.display())
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to read metadata for {}: {e}", listed.path.display())
                }
            }
            yielder.tick().await;
        }
        if listing.complete {
            complete_roots.push(dir.clone());
        }
    }

    let mut removed =
        db.remove_local_tracks_beneath(&complete_roots, |path| !present.contains(path))?;
    // Tracks of folders that are no longer listed cannot be read any more.
    let listed: HashSet<String> = dirs
        .iter()
        .filter_map(|dir| virtual_path::split(dir).map(|(root, _)| root))
        .collect();
    removed += db.remove_local_tracks_beneath(&[PathBuf::from("/")], |path| {
        virtual_path::split(path).is_none_or(|(root, _)| !listed.contains(&root))
    })?;
    if removed > 0 {
        tracing::info!("Removed {removed} missing tracks from library");
    }
    web_fs::persist::flush().await;
    Ok(count + removed)
}
