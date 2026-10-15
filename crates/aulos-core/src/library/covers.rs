// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The album-cover thumbnails of the loaded library, owned by the core so a
//! UI only ever turns pixels into its own image handles.
//!
//! Covers arrive with the library data ([`CoverSet`]s inside the load
//! events). A [`CoverStore`] merges them with a fingerprint check, so a
//! reload that fetched an unchanged cover keeps the existing thumbnail (a UI
//! keeping one image handle per key then never re-uploads it), and stages a
//! non-destructive refresh off to the side so the covers already on screen
//! stay put until the refreshed library is swapped in. What a call changed
//! is reported as a [`CoverDelta`].

use super::CoverArt;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

static THUMB_MAX_DIM: AtomicU32 = AtomicU32::new(CoverArt::GRID_THUMBNAIL_MAX_DIM);

/// One album's cover: a decoded, downscaled RGBA thumbnail and a
/// fingerprint of the full-size bytes it came from. The full-size bytes are
/// deliberately not kept: they are loaded lazily for the album that needs
/// them.
#[derive(Debug, Clone)]
pub struct CoverThumb {
    pub width: u32,
    pub height: u32,
    /// Straight RGBA pixels (empty once a UI took them with
    /// [`CoverStore::take_pixels`]), at most [`CoverArt::GRID_THUMBNAIL_MAX_DIM`]
    /// per side. Reference-counted, so handing them to an image handle
    /// shares rather than copies the buffer.
    pub rgba: Bytes,
    pub fingerprint: u64,
}

impl CoverThumb {
    /// Sets the side length (px) future thumbnails are downscaled to, e.g.
    /// from the UI's pixel density. Already decoded thumbnails keep theirs.
    pub fn set_max_dim(px: u32) {
        THUMB_MAX_DIM.store(px.max(1), Ordering::Relaxed);
    }

    /// Decodes and downscales `bytes`; `None` when they are not a
    /// decodable image.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (width, height, rgba) =
            CoverArt::decode_thumbnail(bytes, THUMB_MAX_DIM.load(Ordering::Relaxed))?;
        Some(Self {
            width,
            height,
            rgba: Bytes::from(rgba),
            fingerprint: CoverArt::content_fingerprint(bytes),
        })
    }
}

/// Covers by [`CoverArt::album_key`].
pub type CoverSet = HashMap<String, CoverThumb>;

/// What a [`CoverStore`] change did to the covers a UI shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverDelta {
    /// Every cover was dropped (before `changed`, which then refills).
    pub cleared: bool,
    /// Keys whose cover is gone.
    pub removed: Vec<String>,
    /// Keys whose cover is new or different: rebuild their image handle
    /// from [`CoverStore::get`].
    pub changed: Vec<String>,
}

impl CoverDelta {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.cleared && self.removed.is_empty() && self.changed.is_empty()
    }

    /// Folds `later`, a change that happened after this one, into it: the
    /// result applies both in one go.
    pub fn absorb(&mut self, later: CoverDelta) {
        if later.cleared {
            *self = later;
            return;
        }
        for key in later.removed {
            self.changed.retain(|k| *k != key);
            if !self.removed.contains(&key) {
                self.removed.push(key);
            }
        }
        for key in later.changed {
            self.removed.retain(|k| *k != key);
            if !self.changed.contains(&key) {
                self.changed.push(key);
            }
        }
    }
}

/// The covers of the library on screen, plus those of a refresh in flight.
#[derive(Debug, Default)]
pub struct CoverStore {
    visible: CoverSet,
    staging: Option<CoverSet>,
}

impl CoverStore {
    /// The cover of album `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&CoverThumb> {
        self.visible.get(key)
    }

    /// Hands out the pixels of `key` once and frees them from the store
    /// (width, height, pixels). The entry stays, so its fingerprint still
    /// de-duplicates a reload; `None` when absent or already taken.
    pub fn take_pixels(&mut self, key: &str) -> Option<(u32, u32, Bytes)> {
        let thumb = self.visible.get_mut(key)?;
        if thumb.rgba.is_empty() {
            return None;
        }
        Some((thumb.width, thumb.height, std::mem::take(&mut thumb.rgba)))
    }

    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.visible.contains_key(key)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Drop every cover and any refresh in flight.
    pub fn clear(&mut self) -> CoverDelta {
        self.staging = None;
        let had_any = !self.visible.is_empty();
        self.visible.clear();
        CoverDelta {
            cleared: had_any,
            ..CoverDelta::default()
        }
    }

    /// Start collecting a non-destructive refresh: batches go to
    /// [`Self::stage`] and replace the visible covers at
    /// [`Self::complete_staged`].
    pub fn begin_staging(&mut self) {
        self.staging = Some(CoverSet::new());
    }

    /// Add `covers` to the refresh in flight (no-op without one).
    pub fn stage(&mut self, covers: CoverSet) {
        if let Some(staging) = self.staging.as_mut() {
            staging.extend(covers);
        }
    }

    /// Finish the refresh in flight: when `swap`, the staged covers replace
    /// the visible ones (see [`Self::replace`]); otherwise they are dropped.
    pub fn complete_staged(&mut self, swap: bool) -> CoverDelta {
        match self.staging.take() {
            Some(staged) if swap => self.replace(staged),
            _ => CoverDelta::default(),
        }
    }

    /// Replace every visible cover with `covers`, keeping the existing
    /// thumbnail of each key whose fingerprint is unchanged.
    pub fn replace(&mut self, covers: CoverSet) -> CoverDelta {
        let mut old = std::mem::take(&mut self.visible);
        let mut changed = Vec::new();
        for (key, thumb) in covers {
            match old.remove(&key) {
                Some(existing) if existing.fingerprint == thumb.fingerprint => {
                    self.visible.insert(key, existing);
                }
                _ => {
                    changed.push(key.clone());
                    self.visible.insert(key, thumb);
                }
            }
        }
        CoverDelta {
            cleared: false,
            removed: old.into_keys().collect(),
            changed,
        }
    }

    /// Add `covers` to the visible ones (progressive load), keeping the
    /// existing thumbnail of each key whose fingerprint is unchanged.
    pub fn merge(&mut self, covers: CoverSet) -> CoverDelta {
        let mut changed = Vec::new();
        for (key, thumb) in covers {
            if self
                .visible
                .get(&key)
                .is_some_and(|existing| existing.fingerprint == thumb.fingerprint)
            {
                continue;
            }
            changed.push(key.clone());
            self.visible.insert(key, thumb);
        }
        CoverDelta {
            changed,
            ..CoverDelta::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb(fingerprint: u64) -> CoverThumb {
        CoverThumb {
            width: 1,
            height: 1,
            rgba: Bytes::from_static(&[0, 0, 0, 255]),
            fingerprint,
        }
    }

    fn set(items: &[(&str, u64)]) -> CoverSet {
        items
            .iter()
            .map(|(k, f)| ((*k).to_string(), thumb(*f)))
            .collect()
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn merge_reports_only_new_or_changed_covers() {
        let mut store = CoverStore::default();
        assert_eq!(
            sorted(store.merge(set(&[("a", 1), ("b", 2)])).changed),
            ["a", "b"]
        );
        let delta = store.merge(set(&[("a", 1), ("b", 3), ("c", 4)]));
        assert_eq!(sorted(delta.changed), ["b", "c"]);
        assert_eq!(store.get("b").unwrap().fingerprint, 3);
    }

    #[test]
    fn staged_refresh_is_invisible_until_swapped() {
        let mut store = CoverStore::default();
        store.merge(set(&[("a", 1), ("b", 2)]));
        store.begin_staging();
        store.stage(set(&[("a", 1), ("c", 9)]));
        assert!(store.contains("b") && !store.contains("c"));

        let delta = store.complete_staged(true);
        assert!(!delta.cleared);
        assert_eq!(delta.changed, ["c"]);
        assert_eq!(delta.removed, ["b"]);
        assert!(store.contains("a") && store.contains("c") && !store.contains("b"));
    }

    #[test]
    fn unswapped_staging_is_dropped() {
        let mut store = CoverStore::default();
        store.merge(set(&[("a", 1)]));
        store.begin_staging();
        store.stage(set(&[("z", 5)]));
        assert!(store.complete_staged(false).is_empty());
        assert!(store.contains("a") && !store.contains("z"));
        // Nothing left to swap.
        assert!(store.complete_staged(true).is_empty());
    }

    #[test]
    fn clear_drops_covers_and_pending_staging() {
        let mut store = CoverStore::default();
        store.merge(set(&[("a", 1)]));
        store.begin_staging();
        let delta = store.clear();
        assert!(delta.cleared);
        assert!(store.is_empty());
        assert!(store.complete_staged(true).is_empty());
        // Clearing an empty store changes nothing.
        assert!(store.clear().is_empty());
    }

    #[test]
    fn unchanged_cover_keeps_its_pixel_buffer() {
        let mut store = CoverStore::default();
        store.merge(set(&[("a", 1)]));
        let before = store.get("a").unwrap().rgba.as_ptr();
        // A re-fetched, identical cover arrives as a distinct allocation.
        let fresh = CoverThumb {
            rgba: Bytes::copy_from_slice(&[0, 0, 0, 255]),
            ..thumb(1)
        };
        let delta = store.replace(HashMap::from([("a".to_string(), fresh)]));
        assert!(delta.is_empty());
        assert_eq!(store.get("a").unwrap().rgba.as_ptr(), before);
    }

    #[test]
    fn undecodable_bytes_give_no_thumbnail() {
        assert!(CoverThumb::from_bytes(b"not an image").is_none());
    }
}
