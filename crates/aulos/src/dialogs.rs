// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Native file and folder pickers, on `rfd`: the XDG desktop portal on Linux
//! (so sandboxed and native builds look the same), the Win32 common dialogs
//! on Windows and `NSOpenPanel` on macOS. All are async and never block the
//! UI thread; a cancelled dialog is simply `None` / an empty list.
//!
//! Android has no portal either: its pickers report a cancelled dialog too, and
//! the library reads the shared Music folder.
//!
//! A browser cannot hand out filesystem paths, so there every picker reports
//! a cancelled dialog, except [`pick_music_folder`]: a music folder is
//! granted through the browser and named by a virtual path (see
//! `aulos_core::web_fs`).

use std::path::PathBuf;

/// A named set of file extensions (no leading dot) the dialog offers.
#[cfg_attr(
    any(target_arch = "wasm32", target_os = "android"),
    allow(dead_code, reason = "the browser and Android have no dialog to filter")
)]
pub struct Filter<'a> {
    pub name: &'a str,
    pub extensions: Vec<&'a str>,
}

impl<'a> Filter<'a> {
    pub fn new(name: &'a str, extensions: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            name,
            extensions: extensions.into_iter().collect(),
        }
    }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
mod native {
    use super::*;

    fn dialog(title: &str, filter: Option<&Filter<'_>>) -> rfd::AsyncFileDialog {
        let dialog = rfd::AsyncFileDialog::new().set_title(title);
        match filter {
            Some(filter) => dialog.add_filter(filter.name, &filter.extensions),
            None => dialog,
        }
    }

    /// One or more files; empty when the dialog was cancelled.
    pub async fn pick_files(title: &str, filter: Option<&Filter<'_>>) -> Vec<PathBuf> {
        dialog(title, filter)
            .pick_files()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|file| file.path().to_path_buf())
            .collect()
    }

    /// A single file; `None` when cancelled.
    pub async fn pick_file(title: &str, filter: Option<&Filter<'_>>) -> Option<PathBuf> {
        dialog(title, filter)
            .pick_file()
            .await
            .map(|file| file.path().to_path_buf())
    }

    /// A folder; `None` when cancelled.
    pub async fn pick_folder(title: &str) -> Option<PathBuf> {
        dialog(title, None)
            .pick_folder()
            .await
            .map(|file| file.path().to_path_buf())
    }

    /// A file to write, pre-named `file_name`; `None` when cancelled.
    pub async fn save_file(
        title: &str,
        file_name: &str,
        filter: Option<&Filter<'_>>,
    ) -> Option<PathBuf> {
        dialog(title, filter)
            .set_file_name(file_name)
            .save_file()
            .await
            .map(|file| file.path().to_path_buf())
    }
}

#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
pub use native::*;

/// One or more files; always empty in the browser (see the module docs).
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub async fn pick_files(_title: &str, _filter: Option<&Filter<'_>>) -> Vec<PathBuf> {
    Vec::new()
}

/// A single file; always `None` in the browser.
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub async fn pick_file(_title: &str, _filter: Option<&Filter<'_>>) -> Option<PathBuf> {
    None
}

/// A folder; always `None` in the browser.
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub async fn pick_folder(_title: &str) -> Option<PathBuf> {
    None
}

/// A music folder to add to the library: `Ok(None)` when cancelled, a message
/// when the browser refused to share the folder.
#[cfg(not(target_arch = "wasm32"))]
pub async fn pick_music_folder(title: &str) -> Result<Option<PathBuf>, String> {
    Ok(pick_folder(title).await)
}

/// A music folder to add to the library: `Ok(None)` when cancelled, a message
/// when the browser refused to share the folder. The folder is named by a
/// virtual path (`/<folder name>`).
#[cfg(target_arch = "wasm32")]
pub async fn pick_music_folder(_title: &str) -> Result<Option<PathBuf>, String> {
    aulos_core::web_fs::pick_folder().await
}

/// A file to write; always `None` in the browser.
#[cfg(any(target_arch = "wasm32", target_os = "android"))]
pub async fn save_file(
    _title: &str,
    _file_name: &str,
    _filter: Option<&Filter<'_>>,
) -> Option<PathBuf> {
    None
}

/// Asks the browser to let the page read the music folder `path` again (it
/// forgets permissions between visits). Must run from a click.
#[cfg(target_arch = "wasm32")]
pub async fn allow_music_folder(path: PathBuf) -> Result<(), String> {
    use aulos_core::web_fs::{RootStatus, request_access};
    match request_access(path).await? {
        RootStatus::Ready => Ok(()),
        _ => Err("Folder access was not granted".to_owned()),
    }
}

/// Nothing to ask for: a desktop build reads every folder.
#[cfg(not(target_arch = "wasm32"))]
pub async fn allow_music_folder(_path: PathBuf) -> Result<(), String> {
    Ok(())
}
