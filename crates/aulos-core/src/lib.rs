// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The UI-independent half of Aulos: music library and tag I/O, playback
//! engine, providers (local, MPD, Subsonic), online services, the tag
//! editor's state machine, settings and desktop integration. Contains no UI
//! toolkit code; a UI drives it through plain method calls, the message/
//! [`task::Effect`] pairs of its controllers, and the streams it exposes.

use std::path::PathBuf;

pub mod autoeq;
pub mod config;
pub mod convert;
pub mod credentials;
pub mod desktop;
pub mod format;
mod fs_atomic;
pub mod http;
pub mod i18n;
pub mod library;
pub mod menu;
pub mod migrate;
pub mod mpris;
pub mod online;
pub mod paths;
pub mod player;
pub mod provider;
pub mod session;
pub mod settings_search;
pub mod tagger;
pub mod task;
pub mod virtual_path;
#[cfg(feature = "visualizer")]
pub mod visualizer;
#[cfg(target_arch = "wasm32")]
pub mod web_fs;
#[cfg(target_arch = "wasm32")]
pub mod web_storage;

/// Decodes a `file://` URI into a filesystem path (`file:///C:/Music/a.flac`
/// becomes `C:\Music\a.flac` on Windows). Shared by `main`'s CLI-argument
/// parsing (`Exec=aulos %U`) and the media-controls `OpenUri`, which always
/// receives a URI rather than a bare path. Returns `None` for any other
/// scheme or a URI naming a remote host, which callers should treat as
/// unsupported.
#[cfg(not(target_arch = "wasm32"))]
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()
}

/// A browser has no filesystem paths to name with a `file://` URI.
#[cfg(target_arch = "wasm32")]
pub fn file_uri_to_path(_uri: &str) -> Option<PathBuf> {
    None
}

/// The `file://` URI of `path` (made absolute first): the inverse of
/// [`file_uri_to_path`]. `None` when the path cannot be expressed as a URI.
#[cfg(not(target_arch = "wasm32"))]
pub fn path_to_file_uri(path: &std::path::Path) -> Option<String> {
    let path = std::path::absolute(path).ok()?;
    url::Url::from_file_path(path).ok().map(String::from)
}

/// See the native version: always `None` in a browser.
#[cfg(target_arch = "wasm32")]
pub fn path_to_file_uri(_path: &std::path::Path) -> Option<String> {
    None
}

/// Seconds since the Unix epoch.
#[must_use]
pub fn unix_now() -> i64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn file_uris_round_trip_with_paths() {
        let path = PathBuf::from("/home/me/My Music/a#b.flac");
        let uri = path_to_file_uri(&path).unwrap();
        assert_eq!(uri, "file:///home/me/My%20Music/a%23b.flac");
        assert_eq!(file_uri_to_path(&uri), Some(path));
    }

    #[cfg(windows)]
    #[test]
    fn file_uris_round_trip_with_paths() {
        let path = PathBuf::from(r"C:\Users\me\My Music\a#b.flac");
        let uri = path_to_file_uri(&path).unwrap();
        assert_eq!(uri, "file:///C:/Users/me/My%20Music/a%23b.flac");
        assert_eq!(file_uri_to_path(&uri), Some(path));
    }

    #[test]
    fn only_local_file_uris_become_paths() {
        assert_eq!(file_uri_to_path("https://x/y"), None);
        assert_eq!(file_uri_to_path("/plain/path"), None);
        // A remote host is a UNC share on Windows only.
        #[cfg(unix)]
        assert_eq!(file_uri_to_path("file://remote-host/share/a.flac"), None);
    }
}
