// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Crash-safe file replacement shared by the settings, scrobble queue,
//! similar-artist cache, file credential store and cover caches, plus the
//! helpers for keeping unreadable files and bounding cache directories.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Replaces `path` with `data` atomically: written to `<path>.tmp` in the same
/// directory, flushed to disk, renamed over `path`, and the directory entry
/// flushed too, so a crash leaves either the old or the new file and never a
/// truncated one. Creates the directory. With `private`, the file is readable
/// by its owner only (Unix), even if a looser leftover temp file exists.
///
/// # Errors
/// If the directory or the file cannot be written.
pub(crate) fn write(path: &Path, data: &[u8], private: bool) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        {
            // Closed at the end of this block, before the rename.
            let mut file = options.open(&tmp)?;
            #[cfg(unix)]
            if private {
                use std::os::unix::fs::PermissionsExt;
                // `mode` only applies on creation; a leftover tmp may be looser.
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            #[cfg(not(unix))]
            let _ = private;
            file.write_all(data)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    sync_dir(dir);
    result
}

/// Moves an unreadable file out of the way so the defaults that replace it
/// never overwrite the user's data, and returns where it went: `<path>.bad`,
/// or `<path>.bad.1`, `<path>.bad.2`, ... when earlier backups exist (those
/// are never overwritten).
///
/// # Errors
/// If no free name is found or the file cannot be renamed.
pub(crate) fn set_aside(path: &Path) -> std::io::Result<PathBuf> {
    let name = path.file_name().unwrap_or_default();
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    for n in 0u32..=u32::MAX {
        let mut bad_name = name.to_owned();
        bad_name.push(".bad");
        if n > 0 {
            bad_name.push(format!(".{n}"));
        }
        let bad = dir.join(bad_name);
        // `symlink_metadata`: a dangling symlink also occupies the name.
        if std::fs::symlink_metadata(&bad).is_ok() {
            continue;
        }
        std::fs::rename(path, &bad)?;
        return Ok(bad);
    }
    Err(std::io::Error::other("no free backup name"))
}

/// Files kept in a cover cache directory (the notification and MPRIS covers):
/// the one just handed out plus a few recent ones.
pub(crate) const COVER_CACHE_KEEP: usize = 8;

/// Bounds a cache directory of regenerable files: keeps `current` and the
/// newest others so that at most `keep` files remain (the previous file may
/// still be read by whoever was handed it), removing the rest. Best effort:
/// entries that cannot be inspected or removed are left alone.
pub(crate) fn prune_dir(dir: &Path, current: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut others: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            let path = entry.path();
            (meta.is_file() && path != current).then(|| (meta.modified().ok(), path))
        })
        .collect();
    // Newest first; files without a modification time count as oldest.
    others.sort_by(|a, b| b.cmp(a));
    for (_, path) in others.into_iter().skip(keep.saturating_sub(1)) {
        let _ = std::fs::remove_file(path);
    }
}

/// Flushes the directory entry of a just-renamed file. Best effort: a
/// filesystem or platform that cannot sync a directory (Windows) is not an
/// error, the data itself is already on disk.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aulos-fs-atomic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn replaces_file_and_leaves_no_temp() {
        let dir = scratch("replace");
        let path = dir.join("sub").join("a.json");
        write(&path, b"one", false).unwrap();
        write(&path, b"two", true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert!(!dir.join("sub").join("a.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_aside_never_overwrites_a_backup() {
        let dir = scratch("aside");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.json");
        for (i, name) in ["a.json.bad", "a.json.bad.1", "a.json.bad.2"]
            .iter()
            .enumerate()
        {
            std::fs::write(&path, i.to_string()).unwrap();
            assert_eq!(set_aside(&path).unwrap(), dir.join(name));
            assert!(!path.exists());
        }
        assert_eq!(std::fs::read(dir.join("a.json.bad")).unwrap(), b"0");
        assert_eq!(std::fs::read(dir.join("a.json.bad.2")).unwrap(), b"2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_dir_keeps_the_current_file_and_the_newest() {
        let dir = scratch("prune");
        std::fs::create_dir_all(&dir).unwrap();
        let old = std::time::Duration::from_secs(3600);
        for n in 0..5u64 {
            let file = std::fs::File::create(dir.join(format!("{n}.jpg"))).unwrap();
            // 0 is the oldest, 4 the newest.
            file.set_modified(std::time::SystemTime::now() - old + old / 10 * n as u32)
                .unwrap();
        }
        // The oldest file is the one just handed out: it must survive.
        prune_dir(&dir, &dir.join("0.jpg"), 3);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["0.jpg", "3.jpg", "4.jpg"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn private_resets_loose_leftover_temp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.ron");
        let tmp = dir.join("s.ron.tmp");
        std::fs::write(&tmp, b"old").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        write(&path, b"secret", true).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
