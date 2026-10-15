// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Keeps the library database across page loads.
//!
//! In the browser SQLite runs on its in-memory file system. This module
//! saves a snapshot of that database file into IndexedDB shortly after it
//! changes (and when the page is hidden or closed), and puts the snapshot
//! back before the first connection opens at the next visit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sqlite_wasm_rs::{MemVfsUtil, WasmOsCallback};
use wasm_bindgen::prelude::*;

use super::idb;

const KEY: &str = "library.db";

/// How long after a change the snapshot is saved (changes within it share one
/// save).
const DEBOUNCE: Duration = Duration::from_secs(3);

static DIRTY: AtomicBool = AtomicBool::new(false);
static SAVING: AtomicBool = AtomicBool::new(false);

fn db_name() -> String {
    crate::paths::library_db_path()
        .to_string_lossy()
        .into_owned()
}

fn memvfs() -> MemVfsUtil<WasmOsCallback> {
    MemVfsUtil::new()
}

/// The database changed: save it soon.
pub fn mark_dirty() {
    DIRTY.store(true, Ordering::Relaxed);
}

/// Puts the saved snapshot (if any) into the in-memory file system. Runs
/// before the first connection is opened.
pub(super) async fn restore() {
    let name = db_name();
    let vfs = memvfs();
    if vfs.exists(&name) {
        return;
    }
    let bytes = match idb::get(KEY).await {
        Ok(Some(bytes)) => js_sys::Uint8Array::new(&bytes).to_vec(),
        Ok(None) => return,
        Err(error) => {
            tracing::warn!("The saved library could not be read: {error}");
            return;
        }
    };
    match vfs.import_db(&name, &bytes) {
        Ok(()) => tracing::info!("Restored the library ({} KiB)", bytes.len() / 1024),
        Err(error) => tracing::warn!("The saved library is unusable: {error}"),
    }
}

/// Saves the snapshot now when the database changed since the last save.
pub async fn flush() {
    if !DIRTY.load(Ordering::Relaxed) || SAVING.swap(true, Ordering::Relaxed) {
        return;
    }
    DIRTY.store(false, Ordering::Relaxed);
    let saved = match memvfs().export_db(&db_name()) {
        Ok(bytes) => idb::put(KEY, &js_sys::Uint8Array::from(bytes.as_slice())).await,
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = saved {
        tracing::warn!("The library could not be saved: {error}");
        mark_dirty();
    }
    SAVING.store(false, Ordering::Relaxed);
}

/// Starts the periodic save and the saves on page hide.
pub(super) fn start() {
    wasm_bindgen_futures::spawn_local(async {
        loop {
            crate::task::sleep(DEBOUNCE).await;
            flush().await;
        }
    });

    let Some(window) = web_sys::window() else {
        return;
    };
    let on_hide = Closure::<dyn FnMut()>::new(|| {
        let hidden = web_sys::window()
            .and_then(|w| w.document())
            .is_none_or(|d| d.visibility_state() == web_sys::VisibilityState::Hidden);
        if hidden {
            wasm_bindgen_futures::spawn_local(flush());
        }
    });
    let on_page_hide = Closure::<dyn FnMut()>::new(|| wasm_bindgen_futures::spawn_local(flush()));
    if let Some(document) = window.document() {
        let _ = document
            .add_event_listener_with_callback("visibilitychange", on_hide.as_ref().unchecked_ref());
    }
    let _ =
        window.add_event_listener_with_callback("pagehide", on_page_hide.as_ref().unchecked_ref());
    on_hide.forget();
    on_page_hide.forget();
}
