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
pub mod i18n;
pub mod library;
pub mod menu;
pub mod migrate;
pub mod mpris;
pub mod online;
pub mod paths;
pub mod player;
pub mod provider;
pub mod settings_search;
pub mod tagger;
pub mod task;
#[cfg(feature = "visualizer")]
pub mod visualizer;

/// Decodes a `file://` URI into a filesystem path. Shared by `main`'s
/// CLI-argument parsing (`Exec=aulos %U`) and MPRIS's `OpenUri`, which
/// always receives a URI rather than a bare path. Returns `None` for any
/// other scheme, which callers should treat as unsupported.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    urlencoding::decode(rest)
        .ok()
        .map(|decoded| PathBuf::from(decoded.into_owned()))
}

/// Seconds since the Unix epoch.
#[must_use]
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
