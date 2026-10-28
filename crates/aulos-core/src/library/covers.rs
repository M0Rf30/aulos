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
    /// The thumbnail, at most [`CoverArt::GRID_THUMBNAIL_MAX_DIM`] per
    /// side, JPEG- (opaque) or PNG-encoded (see [`Self::decode`]). Kept
    /// compressed (~20 KB instead of 260 KB of RGBA) so a UI can drop and
    /// rebuild its image handles as covers scroll in and out of view.
    /// Reference-counted, so cloning it is free.
    pub encoded: Bytes,
    pub fingerprint: u64,
}

impl CoverThumb {
    /// Sets the side length (px) future thumbnails are downscaled to, e.g.
    /// from the UI's pixel density. Already decoded thumbnails keep theirs.
    pub fn set_max_dim(px: u32) {
        THUMB_MAX_DIM.store(px.max(1), Ordering::Relaxed);
    }

    /// The side length (px) new thumbnails are downscaled to.
    #[must_use]
    pub fn max_dim() -> u32 {
        THUMB_MAX_DIM.load(Ordering::Relaxed)
    }

    /// Decodes and downscales `bytes`; `None` when they are not a
    /// decodable image.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (width, height, pixels) =
            CoverArt::decode_thumbnail(bytes, THUMB_MAX_DIM.load(Ordering::Relaxed))?;
        Some(Self {
            width,
            height,
            encoded: encode(width, height, pixels)?,
            fingerprint: CoverArt::content_fingerprint(bytes),
        })
    }

    /// The straight RGBA pixels as `(width, height, pixels)`.
    #[must_use]
    pub fn decode(&self) -> Option<(u32, u32, Vec<u8>)> {
        let img = image::load_from_memory(&self.encoded).ok()?.into_rgba8();
        let (w, h) = img.dimensions();
        Some((w, h, img.into_raw()))
    }
}

/// What a persisted thumbnail was made from: the file its cover was read
/// from (path, modification time, size) and the thumbnail side length. A
/// stored thumbnail is reused only while the file on disk still has the same
/// stamp and the requested side length is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbStamp {
    pub path: String,
    pub mtime_ns: i64,
    pub size: u64,
    pub max_dim: u32,
}

impl ThumbStamp {
    /// The stamp of `path` as it is now, for the current thumbnail size;
    /// `None` when the file cannot be inspected.
    #[must_use]
    pub fn of(path: &std::path::Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        let mtime_ns = match mtime.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_nanos()).ok()?,
            Err(e) => -i64::try_from(e.duration().as_nanos()).ok()?,
        };
        Some(Self {
            path: path.to_string_lossy().into_owned(),
            mtime_ns,
            size: meta.len(),
            max_dim: CoverThumb::max_dim(),
        })
    }

    /// The stamp that vouches for "this album has no cover": like [`Self::of`],
    /// but its time also covers the file's directory, so dropping a
    /// `cover.jpg` next to the tracks makes the entry stale.
    #[must_use]
    pub fn of_coverless(path: &std::path::Path) -> Option<Self> {
        let mut stamp = Self::of(path)?;
        let dir = Self::of(path.parent()?)?;
        stamp.mtime_ns = stamp.mtime_ns.max(dir.mtime_ns);
        if let Some(album) = super::disc_parent(path.parent()?).and_then(Self::of) {
            stamp.mtime_ns = stamp.mtime_ns.max(album.mtime_ns);
        }
        Some(stamp)
    }
}

/// What a previous launch found for an album, as persisted in the library
/// database: its thumbnail, or `None` for "no cover".
#[derive(Debug, Clone)]
pub struct CachedThumb {
    pub thumb: Option<CoverThumb>,
    pub stamp: ThumbStamp,
}

/// JPEG for opaque pixels, PNG when any pixel has transparency.
fn encode(width: u32, height: u32, rgba: Vec<u8>) -> Option<Bytes> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    if rgba.as_chunks::<4>().0.iter().all(|px| px[3] == 255) {
        let rgb: Vec<u8> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|px| [px[0], px[1], px[2]])
            .collect();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
            .ok()?;
    } else {
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
            .ok()?;
    }
    Some(Bytes::from(out))
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
            encoded: Bytes::from_static(&[0, 0, 0, 255]),
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
        let before = store.get("a").unwrap().encoded.as_ptr();
        // A re-fetched, identical cover arrives as a distinct allocation.
        let fresh = CoverThumb {
            encoded: Bytes::copy_from_slice(&[0, 0, 0, 255]),
            ..thumb(1)
        };
        let delta = store.replace(HashMap::from([("a".to_string(), fresh)]));
        assert!(delta.is_empty());
        assert_eq!(store.get("a").unwrap().encoded.as_ptr(), before);
    }

    #[test]
    fn undecodable_bytes_give_no_thumbnail() {
        assert!(CoverThumb::from_bytes(b"not an image").is_none());
    }

    #[test]
    fn thumbnails_round_trip_through_their_compressed_form() {
        let opaque: Vec<u8> = (0..16).flat_map(|i| [i * 8, 40, 200, 255]).collect();
        let jpeg = encode(4, 4, opaque).unwrap();
        let thumb = CoverThumb {
            width: 4,
            height: 4,
            encoded: jpeg,
            fingerprint: 0,
        };
        let (w, h, px) = thumb.decode().unwrap();
        assert_eq!((w, h, px.len()), (4, 4, 64));

        let mut clear = vec![255u8; 64];
        clear[3] = 0;
        let png = encode(4, 4, clear.clone()).unwrap();
        let thumb = CoverThumb {
            encoded: png,
            ..thumb
        };
        assert_eq!(thumb.decode().unwrap().2, clear);
    }
}
