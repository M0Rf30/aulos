// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Local files in the browser.
//!
//! A page has no filesystem paths, so the folders the user picks are *roots*
//! and their files get virtual paths (`/<root>/<relative path>`, see
//! [`crate::virtual_path`]), which is what the library stores. This module
//! turns those paths back into browser `File`s ([`roots`]), and keeps what
//! must outlive a page load in IndexedDB: the folder handles ([`roots`]) and
//! a snapshot of the library database ([`persist`]).
//!
//! Call [`init`] once before the first library database connection opens.

mod idb;
pub mod persist;
mod roots;
mod stash;

use std::path::{Path, PathBuf};

use std::future::Future;

use send_wrapper::SendWrapper;

pub use roots::{
    ListedFile, Listing, Picked, RootStatus, forget_root, info_of, keeps_access_across_reloads,
    list_audio, list_dir, open_file, read_range, status,
};

/// Lets the user pick files of the kinds in `accept` (extensions, comma
/// separated: `.milk,.png`) and reads those up to `limit` bytes each; nothing
/// is picked on cancel. Must be called from a user gesture. Sendable like
/// [`pick_folder`].
///
/// # Errors
/// A message when the dialog cannot be opened or a file cannot be read.
pub fn pick_files(
    accept: &'static str,
    limit: u64,
) -> impl Future<Output = Result<Picked, String>> + Send {
    SendWrapper::new(roots::pick_files(accept, limit))
}

/// The named files [`store_files`] kept under `key`. Sendable like
/// [`pick_folder`].
///
/// # Errors
/// A message when IndexedDB is unavailable or the read failed.
pub fn load_files(
    key: &'static str,
) -> impl Future<Output = Result<Vec<(String, Vec<u8>)>, String>> + Send {
    SendWrapper::new(stash::load(key))
}

/// Keeps `files` (name, contents) in IndexedDB under `key`, replacing what
/// was there. Sendable like [`pick_folder`].
///
/// # Errors
/// A message when IndexedDB is unavailable, full, or refused the value.
pub fn store_files(
    key: &'static str,
    files: Vec<(String, Vec<u8>)>,
) -> impl Future<Output = Result<(), String>> + Send {
    SendWrapper::new(async move {
        stash::store(
            key,
            files
                .iter()
                .map(|(name, bytes)| (name.as_str(), bytes.as_slice())),
        )
        .await
    })
}

/// Lets the user pick a music folder and returns its root path (see
/// [`crate::virtual_path`]); `Ok(None)` when cancelled. Must be called from a
/// user gesture. The future can go to the UI's [`Spawner`](crate::task::Spawner)
/// (the page has one thread, so it never actually changes thread).
///
/// # Errors
/// A message when the browser refuses to share the folder.
pub fn pick_folder() -> impl Future<Output = Result<Option<PathBuf>, String>> + Send {
    SendWrapper::new(roots::pick_folder())
}

/// Asks the browser for read access to the folder of `path` (a click's job).
/// Sendable like [`pick_folder`].
///
/// # Errors
/// A message when the folder is not one the browser can ask about.
pub fn request_access(path: PathBuf) -> impl Future<Output = Result<RootStatus, String>> + Send {
    SendWrapper::new(async move { roots::request_access(&path).await })
}

/// Restores the saved library database and music folders, and starts saving
/// the database as it changes.
pub async fn init() {
    persist::restore().await;
    roots::restore().await;
    persist::start();
}

/// Whole contents of a virtual-path file (reads the browser File into wasm memory).
pub async fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let file = open_file(path).await?;
    read_range(&file, 0, info_of(&file).0).await
}

/// Size in bytes and last-modified unix seconds of a virtual-path file.
pub async fn file_info(path: &Path) -> Result<(u64, i64), String> {
    open_file(path).await.map(|file| info_of(&file))
}

/// Keeps long loops (probing a library, decoding covers) from starving the
/// page: the browser has one thread, so [`Yielder::tick`] hands control back
/// to the event loop (for painting and input) every few milliseconds of work.
pub struct Yielder {
    since: web_time::Instant,
}

impl Yielder {
    /// How long work may run before it yields.
    const SLICE: std::time::Duration = std::time::Duration::from_millis(12);

    #[must_use]
    pub fn new() -> Self {
        Self {
            since: web_time::Instant::now(),
        }
    }

    /// Yields to the event loop when the current slice of work is used up.
    pub async fn tick(&mut self) {
        if self.since.elapsed() >= Self::SLICE {
            crate::task::sleep(std::time::Duration::ZERO).await;
            self.since = web_time::Instant::now();
        }
    }
}

impl Default for Yielder {
    fn default() -> Self {
        Self::new()
    }
}
