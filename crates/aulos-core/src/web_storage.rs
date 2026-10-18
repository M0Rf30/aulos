// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The browser's `localStorage`, where the web build keeps what the desktop
//! build keeps in files. Text only, a few megabytes per origin: fit for the
//! settings, not for the library.

/// Key of the settings (the RON text of [`crate::config::Config`]).
pub const SETTINGS_KEY: &str = "aulos.settings.ron";

/// Where settings text that cannot be parsed is kept, so the defaults that
/// replace it are never saved over the user's data.
pub const SETTINGS_BAD_KEY: &str = "aulos.settings.ron.bad";

fn storage() -> Result<web_sys::Storage, String> {
    web_sys::window()
        .ok_or("no window")?
        .local_storage()
        .map_err(|_| "local storage is blocked".to_owned())?
        .ok_or_else(|| "no local storage".to_owned())
}

/// The text stored under `key`; `None` when absent or storage is unavailable.
#[must_use]
pub fn get(key: &str) -> Option<String> {
    storage().ok()?.get_item(key).ok().flatten()
}

/// Stores `value` under `key`.
///
/// # Errors
/// A message when storage is blocked or full.
pub fn set(key: &str, value: &str) -> Result<(), String> {
    storage()?
        .set_item(key, value)
        .map_err(|_| "the browser refused to store the data".to_owned())
}
