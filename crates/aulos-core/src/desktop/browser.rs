// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Opening things outside the app: a web page (the default browser on the
//! desktop, a new tab in the web build) or a local path.

/// Opens `url` in the default browser (a new tab in the web build) without
/// waiting for it.
///
/// # Errors
/// A message for the user when no browser could be started or the tab was
/// blocked.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub fn open_in_browser(url: &str) -> Result<(), String> {
    open::that_detached(url).map_err(|e| format!("Could not open the browser: {e}"))
}

/// What the app registers on Android: there is no `xdg-open`, a page is
/// opened with an `ACTION_VIEW` intent that only the host app can start.
#[cfg(target_os = "android")]
static URL_OPENER: std::sync::OnceLock<UrlOpener> = std::sync::OnceLock::new();

/// Starts the system browser on a URL.
#[cfg(target_os = "android")]
pub type UrlOpener = fn(&str) -> Result<(), String>;

/// Registers how Android opens a URL (called once at startup).
#[cfg(target_os = "android")]
pub fn set_url_opener(opener: UrlOpener) {
    let _ = URL_OPENER.set(opener);
}

/// See the native version.
#[cfg(target_os = "android")]
pub fn open_in_browser(url: &str) -> Result<(), String> {
    match URL_OPENER.get() {
        Some(open) => open(url),
        None => Err("Could not open the browser: no opener registered".into()),
    }
}

/// See the native version.
#[cfg(target_arch = "wasm32")]
pub fn open_in_browser(url: &str) -> Result<(), String> {
    let window = web_sys::window().ok_or("Could not open the browser: no window")?;
    match window.open_with_url_and_target(url, "_blank") {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err("Could not open the browser: the new tab was blocked".into()),
        Err(_) => Err("Could not open the browser".into()),
    }
}

/// Opens the local file or folder `path` with the system's default
/// application (an HTML page in the browser, a folder in the file manager).
///
/// # Errors
/// A message for the user when nothing could be started, or always in the web
/// build: a page cannot open files or folders of the user's machine.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_path(path: &std::path::Path) -> Result<(), String> {
    open::that_detached(path).map_err(|e| format!("Could not open {}: {e}", path.display()))
}

/// See the native version.
#[cfg(target_arch = "wasm32")]
pub fn open_path(_path: &std::path::Path) -> Result<(), String> {
    Err("Local files and folders cannot be opened from the browser".into())
}
