// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

pub mod app;
pub mod autoeq;
pub mod config;
pub mod convert;
pub mod credentials;
pub mod i18n;
pub mod keybinds;
pub mod library;
pub mod migrate;
pub mod mpris;
pub mod online;
pub mod player;
pub mod provider;
pub mod views;

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
