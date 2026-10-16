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
//! unknown. Android has no such base: the host calls [`set_android_base`]
//! with the app's private directory before anything else reads a path.

use std::path::PathBuf;

const APP_DIR: &str = "aulos";

/// The app-private directory Android hands the activity; `config`, `data`
/// and `cache` live under it.
#[cfg(target_os = "android")]
static ANDROID_BASE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Sets the directory the config, data and cache folders live under on
/// Android. Call once, first thing in the activity; later calls are ignored.
#[cfg(target_os = "android")]
pub fn set_android_base(base: PathBuf) {
    let _ = ANDROID_BASE.set(base);
}

/// `<android base>/<kind>/aulos`, when the base was set.
#[cfg(target_os = "android")]
fn android_dir(kind: &str) -> Option<PathBuf> {
    ANDROID_BASE.get().map(|base| base.join(kind).join(APP_DIR))
}

/// `$XDG_DATA_HOME/aulos`: the library database and other persistent data.
/// The machine-local data directory, so the database does not roam with a
/// Windows profile (the same as `$XDG_DATA_HOME` and Application Support
/// elsewhere).
#[must_use]
pub fn data_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    if let Some(dir) = android_dir("data") {
        return dir;
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
}

/// `$XDG_CONFIG_HOME/aulos`: the settings (`settings.ron`, see
/// [`crate::config::Config::path`]) and other user files.
#[must_use]
pub fn config_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    if let Some(dir) = android_dir("config") {
        return dir;
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
}

/// `$XDG_CACHE_HOME/aulos`: data that can be fetched or computed again.
#[must_use]
pub fn cache_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    if let Some(dir) = android_dir("cache") {
        return dir;
    }
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
