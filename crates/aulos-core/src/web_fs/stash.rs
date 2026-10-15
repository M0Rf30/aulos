// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Named files kept in IndexedDB across page loads (the visualizer's
//! presets the user added). One list of `[name, bytes]` pairs per key,
//! stored as a structured-cloneable array, so nothing is encoded or copied
//! twice.

use super::idb;
use js_sys::{Array, Uint8Array};
use wasm_bindgen::JsValue;

/// The files stored under `key` (none when nothing was stored).
///
/// # Errors
/// A message when IndexedDB is unavailable or the read failed.
pub(super) async fn load(key: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let Some(value) = idb::get(key).await? else {
        return Ok(Vec::new());
    };
    Ok(Array::from(&value)
        .iter()
        .filter_map(|pair| {
            let pair = Array::from(&pair);
            Some((
                pair.get(0).as_string()?,
                Uint8Array::new(&pair.get(1)).to_vec(),
            ))
        })
        .collect())
}

/// Replaces what is stored under `key` with `files`.
///
/// # Errors
/// A message when IndexedDB is unavailable, full, or refused the value.
pub(super) async fn store<'a>(
    key: &str,
    files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<(), String> {
    let list = Array::new();
    for (name, bytes) in files {
        list.push(&Array::of2(
            &JsValue::from_str(name),
            &Uint8Array::from(bytes),
        ));
    }
    idb::put(key, &list).await
}
