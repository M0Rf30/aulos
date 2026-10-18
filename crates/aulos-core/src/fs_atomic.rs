// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Crash-safe file replacement shared by the settings, scrobble queue,
//! similar-artist cache and file credential store.

use std::io::Write;
use std::path::Path;

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
