// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Editable (string-backed) copies of the provider config entries.
//!
//! Text inputs bind to plain strings, so the settings form edits these
//! mirrors and converts back to [`MpdConfigEntry`] / [`SubsonicConfigEntry`]
//! on save. Kept UI-free so any front end can reuse the round-trip rules.

use super::{MpdConfigEntry, SubsonicConfigEntry};

/// Editing state for a single MPD server entry.
///
/// Kept as strings so text inputs can bind directly without
/// lifetime issues around temporary conversions.
#[derive(Debug, Clone)]
pub struct MpdEditState {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: String,
    pub password: String,
}

impl MpdEditState {
    /// Create from a config entry.
    ///
    /// When `password_in_keyring` is set the password is fetched from the
    /// system keyring so the edit form can show (and round-trip) the real value.
    pub fn from_config(entry: &MpdConfigEntry) -> Self {
        let password = if entry.password_in_keyring {
            crate::credentials::retrieve_password(&entry.id)
                .ok()
                .flatten()
                .unwrap_or_default()
        } else {
            entry.password.clone().unwrap_or_default()
        };

        Self {
            id: entry.id.clone(),
            name: entry.name.clone(),
            host: entry.host.clone(),
            port: entry.port.to_string(),
            password,
        }
    }

    /// Convert back to a config entry (port defaults to 6600 on parse failure).
    ///
    /// The password is always stored as plaintext here so that the startup
    /// migration code can move it to the keyring on the next launch.
    pub fn to_config(&self) -> MpdConfigEntry {
        MpdConfigEntry {
            id: self.id.clone(),
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port.parse().unwrap_or(6600),
            password: if self.password.is_empty() {
                None
            } else {
                Some(self.password.clone())
            },
            // Always reset to false so the startup migration path stores it in
            // the keyring (or the plaintext fallback is used if unavailable).
            password_in_keyring: false,
        }
    }

    /// Create a new empty entry with a generated id.
    pub fn new_default<'a>(taken: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            id: unique_id("mpd", taken),
            name: "MPD Server".to_string(),
            host: "localhost".to_string(),
            port: "6600".to_string(),
            password: String::new(),
        }
    }
}

/// Editing state for a single Subsonic/Navidrome server entry.
#[derive(Debug, Clone)]
pub struct SubsonicEditState {
    pub id: String,
    pub name: String,
    pub url: String,
    pub username: String,
    pub password: String,
    pub accept_invalid_certs: bool,
    /// Transcoding max bitrate (None = original quality).
    pub transcoding_max_bitrate: Option<u32>,
    /// Transcoding format (None = original format).
    pub transcoding_format: Option<String>,
}

impl SubsonicEditState {
    /// Create from a config entry.
    ///
    /// When `password_in_keyring` is set the password is fetched from the
    /// system keyring so the edit form can show (and round-trip) the real value.
    pub fn from_config(entry: &SubsonicConfigEntry) -> Self {
        let password = if entry.password_in_keyring {
            crate::credentials::retrieve_password(&entry.id)
                .ok()
                .flatten()
                .unwrap_or_default()
        } else {
            entry.password.clone().unwrap_or_default()
        };

        Self {
            id: entry.id.clone(),
            name: entry.name.clone(),
            url: entry.url.clone(),
            username: entry.username.clone(),
            password,
            accept_invalid_certs: entry.accept_invalid_certs,
            transcoding_max_bitrate: entry.transcoding_max_bitrate,
            transcoding_format: entry.transcoding_format.clone(),
        }
    }

    /// Convert back to a config entry.
    ///
    /// The password is always stored as plaintext here so that the startup
    /// migration code can move it to the keyring on the next launch (or
    /// immediately if the keyring is available).
    pub fn to_config(&self) -> SubsonicConfigEntry {
        SubsonicConfigEntry {
            id: self.id.clone(),
            name: self.name.clone(),
            url: self.url.clone(),
            username: self.username.clone(),
            password: if self.password.is_empty() {
                None
            } else {
                Some(self.password.clone())
            },
            // Always reset to false so the startup migration path stores it in
            // the keyring (or the plaintext fallback is used if unavailable).
            password_in_keyring: false,
            accept_invalid_certs: self.accept_invalid_certs,
            transcoding_max_bitrate: self.transcoding_max_bitrate,
            transcoding_format: self.transcoding_format.clone(),
        }
    }

    /// Create a new empty entry with a generated id.
    pub fn new_default<'a>(taken: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            id: unique_id("subsonic", taken),
            name: "Subsonic Server".to_string(),
            url: "https://".to_string(),
            username: String::new(),
            password: String::new(),
            accept_invalid_certs: false,
            transcoding_max_bitrate: None,
            transcoding_format: None,
        }
    }
}

/// First `<prefix>-<n>` id not present in `taken`.
fn unique_id<'a>(prefix: &str, taken: impl IntoIterator<Item = &'a str>) -> String {
    let taken: std::collections::HashSet<&str> = taken.into_iter().collect();
    (0usize..)
        .map(|n| format!("{prefix}-{n}"))
        .find(|id| !taken.contains(id.as_str()))
        .expect("unbounded range")
}

/// Whether a stored connection-test status string means "connected".
///
/// Statuses are kept as the localised label, so the check compares against
/// the same translation the test result was written with.
#[must_use]
pub fn is_connected_status(status: &str) -> bool {
    status == crate::fl!("connected")
}
