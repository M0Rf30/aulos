// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Where Aulos keeps its files, per OS (the `dirs` crate decides the base):
//!
//! | | Linux | Windows | macOS |
//! |---|---|---|---|
//! | config | `~/.config/aulos` | `%APPDATA%\aulos` | `~/Library/Application Support/aulos` |
//! | data | `~/.local/share/aulos` | `%LOCALAPPDATA%\aulos` | `~/Library/Application Support/aulos` |
//! | cache | `~/.cache/aulos` | `%LOCALAPPDATA%\aulos\cache` | `~/Library/Caches/aulos` |
//!
//! Each falls back to the working directory when the base directory is
//! unknown.

use std::path::PathBuf;

const APP_DIR: &str = "aulos";

/// `$XDG_DATA_HOME/aulos`: the library database and other persistent data.
/// The machine-local data directory, so the database does not roam with a
/// Windows profile (the same as `$XDG_DATA_HOME` and Application Support
/// elsewhere).
#[must_use]
pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
}

/// `$XDG_CONFIG_HOME/aulos`: the settings (`settings.ron`, see
/// [`crate::config::Config::path`]) and other user files.
#[must_use]
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
}

/// `$XDG_CACHE_HOME/aulos`: data that can be fetched or computed again.
#[must_use]
pub fn cache_dir() -> PathBuf {
    let dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR);
    // On Windows the cache base is the same folder as the data one.
    if cfg!(windows) {
        dir.join("cache")
    } else {
        dir
    }
}

/// The library database, shared by the library, the online store (podcasts,
/// radio) and play history.
#[must_use]
pub fn library_db_path() -> PathBuf {
    data_dir().join("library.db")
}
