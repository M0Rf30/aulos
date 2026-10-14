// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Decoded cover thumbnails of the editor's pictures.
//!
//! The cache is generic over the image type the UI draws (`H`: an egui texture,
//! an image handle, …), so the UI keeps a stable image per picture
//! and only the bookkeeping — which pictures still need decoding, each
//! handed out once — lives here.

use super::TaggerState;
use std::collections::{HashMap, HashSet};

/// Longest side, in pixels, of a decoded thumbnail.
pub const THUMB_MAX_DIM: u32 = 160;

/// Thumbnails by [`super::edit::picture_hash`].
#[derive(Debug)]
pub struct ThumbCache<H> {
    /// `None` when the picture could not be decoded.
    decoded: HashMap<u64, Option<H>>,
    pending: HashSet<u64>,
}

impl<H> Default for ThumbCache<H> {
    fn default() -> Self {
        Self {
            decoded: HashMap::new(),
            pending: HashSet::new(),
        }
    }
}

impl<H> ThumbCache<H> {
    /// The thumbnail of the picture with `hash`, once decoded.
    #[must_use]
    pub fn get(&self, hash: u64) -> Option<&H> {
        self.decoded.get(&hash).and_then(Option::as_ref)
    }

    /// Pictures `state` shows that have no thumbnail yet, as `(hash, image
    /// bytes)` to decode (see [`decode`]); each is handed out once.
    pub fn take_missing(&mut self, state: &TaggerState) -> Vec<(u64, Vec<u8>)> {
        let mut wanted = Vec::new();
        for row in state.pictures() {
            if self.decoded.contains_key(&row.hash) || self.pending.contains(&row.hash) {
                continue;
            }
            if let Some(data) = state.picture_data(row.hash) {
                self.pending.insert(row.hash);
                wanted.push((row.hash, data));
            }
        }
        wanted
    }

    /// Records the thumbnail of the picture with `hash`, or (`None`) that it
    /// could not be decoded.
    pub fn insert(&mut self, hash: u64, thumb: Option<H>) {
        self.pending.remove(&hash);
        self.decoded.insert(hash, thumb);
    }
}

/// Decodes picture `bytes` into a thumbnail `(width, height, RGBA pixels)`.
/// Blocking; run it off the UI thread.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    crate::library::CoverArt::decode_thumbnail(bytes, THUMB_MAX_DIM)
}
