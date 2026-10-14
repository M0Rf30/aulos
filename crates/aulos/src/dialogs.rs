// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Native file and folder pickers, on `rfd`: the XDG desktop portal on Linux
//! (so sandboxed and native builds look the same), the Win32 common dialogs
//! on Windows and `NSOpenPanel` on macOS. All are async and never block the
//! UI thread; a cancelled dialog is simply `None` / an empty list.

use std::path::PathBuf;

/// A named set of file extensions (no leading dot) the dialog offers.
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
