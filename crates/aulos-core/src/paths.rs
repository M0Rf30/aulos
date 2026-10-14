// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Where Aulos keeps its files. Each falls back to the working directory
//! when the XDG base directory is unknown.

use std::path::PathBuf;

const APP_DIR: &str = "aulos";

/// `$XDG_DATA_HOME/aulos`: the library database and other persistent data.
#[must_use]
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
}

/// `$XDG_CONFIG_HOME/aulos`: user files besides the settings (which
/// cosmic-config keeps under its own app id).
#[must_use]
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
}

/// `$XDG_CACHE_HOME/aulos`: data that can be fetched or computed again.
#[must_use]
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
}

/// The library database, shared by the library, the online store (podcasts,
/// radio) and play history.
#[must_use]
pub fn library_db_path() -> PathBuf {
    data_dir().join("library.db")
}
