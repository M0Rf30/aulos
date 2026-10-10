// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Accent colour extraction from cover art.
//!
//! Aulos already keeps the raw encoded cover-art bytes for the current album
//! around for blur processing (`AppModel.cover_art_bytes`); this module
//! reuses that same byte source to pick a small, legible accent colour so
//! the now-playing surfaces can visually match the artwork instead of
//! always showing the fixed COSMIC system accent. There is no dependency on
//! `auto-palette` (Euphonica's choice) — `extract` works directly off the
//! `image` crate already in the dependency tree with a coarse HSL
//! histogram, which is more than adequate at the fixed 32x32 working
//! resolution used here.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// An accent colour derived from cover art, plus a pre-computed
/// black/white pairing that stays legible drawn on top of it.
///
/// Both fields are linear 0.0..=1.0 sRGB-ish triples (matching how the rest
/// of the UI layer hands colours to `cosmic::iced::Color::from_rgb`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Accent {
    pub color: [f32; 3],
    pub on_color: [f32; 3],
}

/// Number of hue buckets used to histogram pixels. 12 buckets (30° each) is
/// coarse enough to be cheap and to merge near-identical hues, but fine
/// enough to keep genuinely different dominant colours (e.g. red vs.
/// orange) apart.
const HUE_BUCKETS: usize = 12;

/// Pixels with saturation at or below this are treated as achromatic and
/// excluded from hue voting entirely — this is what makes a fully gray/
/// black/white image resolve to "no dominant hue" rather than being
/// assigned an arbitrary bucket.
const CHROMA_EPSILON: f32 = 0.05;

/// Lightness band the winning bucket's average lightness is clamped into
/// before rebuilding the final colour. Keeps the accent out of the
/// near-black/near-white range where it would either vanish against dark
/// UI chrome or blow out against light chrome.
const NORMALIZED_LIGHTNESS_MIN: f32 = 0.35;
const NORMALIZED_LIGHTNESS_MAX: f32 = 0.62;

/// Minimum saturation the final colour is raised to, so a slightly muted
/// dominant hue still reads as a deliberate accent rather than a tint of
/// gray.
const SATURATION_FLOOR: f32 = 0.55;

/// Bounded LRU cache of raw (still-encoded) cover-art bytes, keyed by album
/// key (`CoverArt::album_key`).
///
/// This lives here rather than in `cover_art.rs` because `cover_art` is a
/// private module (`mod cover_art;` in `library/mod.rs`, which this change
/// must not touch) and `AppModel` in `app.rs` needs to name the cache type
/// directly — `palette` is already `pub mod palette;`, so this is the one
/// spot reachable from outside `library` without adding a re-export.
///
/// Only the *encoded* bytes are cached, never decoded pixel buffers: decode,
/// blur and accent extraction are all cheap, on-demand, one-shot operations
/// run only for whichever album is currently playing (see
/// `AppModel::maybe_update_blurred_cover`), so there is nothing to gain by
/// keeping decoded data around, and caching the smaller encoded form keeps
/// the bound meaningful. The on-disk `cover_cache` table remains the actual
/// source of truth for art the in-memory cache has evicted — a miss here
/// just means re-reading from disk (or re-fetching from the provider) the
/// next time that album's art is needed, not data loss.
///
/// Entries are stamped with a monotonically increasing generation counter
/// instead of being kept in a recency list: a hit is an O(1) stamp update (no
/// list scan/reshuffle) and only an eviction -- bounded by the tiny capacity --
/// has to look for the smallest stamp.
///
/// Besides the bytes themselves the cache tracks which keys have a load in
/// flight and which are known to have no art at all, so callers can request
/// bytes lazily (`begin_load`/`finish_load`) without spawning duplicate or
/// endlessly retried loads. The loaded bytes are `Arc`-shared so handing them
/// to a background job does not copy the (possibly multi-MB) buffer.
pub struct CoverByteCache {
    entries: HashMap<String, CoverEntry>,
    generation: u64,
    pending: HashSet<String>,
    missing: HashSet<String>,
}

struct CoverEntry {
    bytes: Arc<Vec<u8>>,
    stamp: u64,
}

/// 64 albums comfortably covers "everything played this session" for
/// typical listening while bounding worst-case memory: full-size embedded
/// covers are commonly tens to a few hundred KB, so 64 of them tops out in
/// the tens-of-MB range regardless of how many thousands of albums the
/// library actually has -- unlike the unbounded `HashMap` this replaces,
/// which grew with the whole library.
const COVER_BYTE_CACHE_CAPACITY: usize = 64;

impl CoverByteCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            generation: 0,
            pending: HashSet::new(),
            missing: HashSet::new(),
        }
    }

    fn next_stamp(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Insert or update `key`, evicting the least-recently-used entry first
    /// if the cache is already at capacity.
    pub fn insert(&mut self, key: String, bytes: Vec<u8>) {
        if self.entries.len() >= COVER_BYTE_CACHE_CAPACITY && !self.entries.contains_key(&key) {
            let lru_key = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.stamp)
                .map(|(k, _)| k.clone());
            if let Some(lru_key) = lru_key {
                self.entries.remove(&lru_key);
            }
        }
        self.missing.remove(&key);
        let stamp = self.next_stamp();
        self.entries.insert(
            key,
            CoverEntry {
                bytes: Arc::new(bytes),
                stamp,
            },
        );
    }

    /// Look up `key`, marking it most-recently-used on a hit so it survives
    /// future evictions longer than entries that are not being played.
    pub fn get(&mut self, key: &str) -> Option<Arc<Vec<u8>>> {
        let stamp = self.next_stamp();
        let entry = self.entries.get_mut(key)?;
        entry.stamp = stamp;
        Some(Arc::clone(&entry.bytes))
    }

    /// Whether `key` is cached; does not touch the recency order.
    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Claim the right to load `key`'s bytes. Returns `false` when they are
    /// already cached, already being loaded, or a previous load found no
    /// art; the caller must then call [`Self::finish_load`] when its load
    /// ends.
    pub fn begin_load(&mut self, key: &str) -> bool {
        if !self.should_load(key) {
            return false;
        }
        self.pending.insert(key.to_string());
        true
    }

    /// Non-mutating pre-check for [`Self::begin_load`]: `true` when `key`
    /// is neither cached, in flight, nor known to have no art.
    pub fn should_load(&self, key: &str) -> bool {
        !(self.entries.contains_key(key)
            || self.pending.contains(key)
            || self.missing.contains(key))
    }

    /// Record the outcome of a load started via [`Self::begin_load`]: cache
    /// the bytes, or remember that `key` has no art so it is not re-requested.
    pub fn finish_load(&mut self, key: String, bytes: Option<Vec<u8>>) {
        self.pending.remove(&key);
        match bytes {
            Some(bytes) => self.insert(key, bytes),
            None => {
                self.missing.insert(key);
            }
        }
    }

    /// Drop everything (cached bytes, in-flight and "no art" markers), e.g.
    /// after a library reload when the underlying art may have changed.
    /// Loads still in flight will simply re-populate their key when they end.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.missing.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for CoverByteCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert an sRGB triple (0.0..=1.0 per channel) to HSL.
///
/// `cover_art::hsl_to_rgb` already covers the inverse direction; this is
/// the one new bit of colour maths this module needs.
fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let delta = max - min;

    if delta < 1e-6 {
        return (0.0, 0.0, l);
    }

    let s = if l < 0.5 {
        delta / (max + min)
    } else {
        delta / (2.0 - max - min)
    };

    let mut h = if (max - r).abs() < 1e-6 {
        60.0 * (((g - b) / delta) % 6.0)
    } else if (max - g).abs() < 1e-6 {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }

    (h, s, l)
}

/// Score multiplier that fades a hue bucket out as its average lightness
/// approaches pure black or pure white — this is what stops letterboxing
/// bars or a mostly-white cover from winning purely on pixel count.
fn lightness_penalty(l: f32) -> f32 {
    const EDGE: f32 = 0.12;
    if l < EDGE {
        l / EDGE
    } else if l > 1.0 - EDGE {
        (1.0 - l) / EDGE
    } else {
        1.0
    }
}

/// Extract a legible accent colour from encoded cover-art bytes.
///
/// Cost is bounded regardless of the source image's resolution: the image
/// is decoded once, then downscaled to a fixed 32x32 working set before any
/// per-pixel analysis. Pixels are bucketed into a 12-way hue histogram,
/// each bucket scored by `count * average_saturation *
/// lightness_penalty(average_lightness)`, and the winning bucket's hue
/// (circular mean of contributing pixels) and average saturation/lightness
/// are normalized — saturation raised to a floor, lightness clamped to a
/// mid band — into the final colour. Pixels at or below `CHROMA_EPSILON`
/// saturation never vote for a hue, so an image with no sufficiently
/// chromatic pixels (solid black, solid white, or gray) leaves every
/// bucket empty and this returns `None`.
pub fn extract(image_bytes: &[u8]) -> Option<Accent> {
    let img = image::load_from_memory(image_bytes).ok()?;
    let small = img
        .resize_exact(32, 32, image::imageops::FilterType::Triangle)
        .to_rgba8();

    let mut counts = [0u32; HUE_BUCKETS];
    let mut sum_s = [0f32; HUE_BUCKETS];
    let mut sum_l = [0f32; HUE_BUCKETS];
    // Circular mean accumulators for the bucket's true average hue (a plain
    // arithmetic mean of angles would break across the 0°/360° wrap).
    let mut sum_cos = [0f32; HUE_BUCKETS];
    let mut sum_sin = [0f32; HUE_BUCKETS];

    for pixel in small.pixels() {
        let [r, g, b, a] = pixel.0;
        if a == 0 {
            continue;
        }
        let (h, s, l) = rgb_to_hsl(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
        if s <= CHROMA_EPSILON {
            continue;
        }
        let bucket = ((h / 30.0) as usize).min(HUE_BUCKETS - 1);
        let rad = h.to_radians();
        counts[bucket] += 1;
        sum_s[bucket] += s;
        sum_l[bucket] += l;
        sum_cos[bucket] += rad.cos();
        sum_sin[bucket] += rad.sin();
    }

    let mut best: Option<(usize, f32)> = None;
    for i in 0..HUE_BUCKETS {
        if counts[i] == 0 {
            continue;
        }
        let avg_s = sum_s[i] / counts[i] as f32;
        let avg_l = sum_l[i] / counts[i] as f32;
        let score = counts[i] as f32 * avg_s * lightness_penalty(avg_l);
        if score > best.map_or(0.0, |(_, best_score)| best_score) {
            best = Some((i, score));
        }
    }

    let (bucket, _) = best?;

    let avg_s = (sum_s[bucket] / counts[bucket] as f32).clamp(SATURATION_FLOOR, 1.0);
    let avg_l = (sum_l[bucket] / counts[bucket] as f32)
        .clamp(NORMALIZED_LIGHTNESS_MIN, NORMALIZED_LIGHTNESS_MAX);
    let mut hue = sum_sin[bucket].atan2(sum_cos[bucket]).to_degrees();
    if hue < 0.0 {
        hue += 360.0;
    }

    let (r, g, b) = super::cover_art::hsl_to_rgb(hue, avg_s, avg_l);
    let color = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0];
    let on_color = on_color_for(color);
    Some(Accent { color, on_color })
}

/// Pick black or white for legible text/icons drawn on top of `color`,
/// using WCAG relative luminance: linearize each sRGB channel, combine with
/// the standard luminance weights, then compare the contrast ratio against
/// black vs. against white and keep whichever is higher. This is the same
/// rule browsers/design tools use for "does this need light or dark text".
pub fn on_color_for(color: [f32; 3]) -> [f32; 3] {
    fn linearize(c: f32) -> f32 {
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    let luminance =
        0.2126 * linearize(color[0]) + 0.7152 * linearize(color[1]) + 0.0722 * linearize(color[2]);

    let contrast_with_white = 1.05 / (luminance + 0.05);
    let contrast_with_black = (luminance + 0.05) / 0.05;

    if contrast_with_black >= contrast_with_white {
        [0.0, 0.0, 0.0]
    } else {
        [1.0, 1.0, 1.0]
    }
}

fn relative_luminance(color: [f32; 3]) -> f32 {
    fn linearize(c: f32) -> f32 {
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linearize(color[0]) + 0.7152 * linearize(color[1]) + 0.0722 * linearize(color[2])
}

impl Accent {
    /// This accent adjusted to stand out against the window background: on
    /// a dark theme a deep cover colour (navy, maroon…) is lifted toward
    /// white until it reaches a minimum luminance, on a light theme a pale
    /// one is pushed toward black. Hue is preserved; `on_color` is
    /// recomputed for the adjusted colour.
    #[must_use]
    pub fn legible(self, dark_theme: bool) -> Self {
        const MIN_ON_DARK: f32 = 0.22;
        const MAX_ON_LIGHT: f32 = 0.35;
        let target = if dark_theme { [1.0; 3] } else { [0.0; 3] };
        let ok = |c: [f32; 3]| {
            let l = relative_luminance(c);
            if dark_theme {
                l >= MIN_ON_DARK
            } else {
                l <= MAX_ON_LIGHT
            }
        };
        let mut color = self.color;
        let mut step = 0;
        while !ok(color) && step < 20 {
            for i in 0..3 {
                color[i] += (target[i] - color[i]) * 0.1;
            }
            step += 1;
        }
        Self {
            color,
            on_color: on_color_for(color),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    use std::io::Cursor;

    fn png_bytes(img: &RgbaImage) -> Vec<u8> {
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .expect("encode test PNG");
        buf
    }

    fn solid_image(size: u32, pixel: [u8; 4]) -> RgbaImage {
        RgbaImage::from_fn(size, size, |_, _| Rgba(pixel))
    }

    #[test]
    fn solid_red_yields_red_dominant_accent() {
        let img = solid_image(16, [255, 0, 0, 255]);
        let accent = extract(&png_bytes(&img)).expect("red image has a dominant hue");
        assert!(accent.color[0] > accent.color[1]);
        assert!(accent.color[0] > accent.color[2]);
        assert!(accent.color[0] > 0.5, "red channel should dominate");
    }

    /// A fully black image has zero saturation everywhere, so every hue
    /// bucket stays empty and `extract` returns `None` — this is the
    /// documented behaviour for achromatic input, not an implementation
    /// accident.
    #[test]
    fn solid_black_returns_none() {
        let img = solid_image(16, [0, 0, 0, 255]);
        assert!(extract(&png_bytes(&img)).is_none());
    }

    #[test]
    fn half_black_half_blue_picks_blue_not_black() {
        let img = RgbaImage::from_fn(16, 16, |x, _| {
            if x < 8 {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([20, 20, 230, 255]) // saturated blue
            }
        });
        let accent = extract(&png_bytes(&img)).expect("blue half should dominate");
        assert!(accent.color[2] > accent.color[0]);
        assert!(accent.color[2] > accent.color[1]);
    }

    #[test]
    fn on_color_is_white_for_dark_and_black_for_light() {
        assert_eq!(on_color_for([0.05, 0.05, 0.1]), [1.0, 1.0, 1.0]);
        assert_eq!(on_color_for([0.95, 0.95, 0.9]), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn cover_cache_evicts_least_recently_used() {
        let mut c = CoverByteCache::new();
        for i in 0..COVER_BYTE_CACHE_CAPACITY {
            c.insert(format!("k{i}"), vec![i as u8]);
        }
        // Touch k0 so k1 becomes the LRU entry.
        assert!(c.get("k0").is_some());
        c.insert("new".into(), vec![9]);
        assert_eq!(c.len(), COVER_BYTE_CACHE_CAPACITY);
        assert!(c.contains("k0"));
        assert!(!c.contains("k1"));
        assert!(c.contains("new"));
    }

    #[test]
    fn cover_cache_reinsert_does_not_evict() {
        let mut c = CoverByteCache::new();
        for i in 0..COVER_BYTE_CACHE_CAPACITY {
            c.insert(format!("k{i}"), vec![0]);
        }
        c.insert("k5".into(), vec![7]);
        assert_eq!(c.len(), COVER_BYTE_CACHE_CAPACITY);
        assert_eq!(c.get("k5").as_deref(), Some(&vec![7]));
        assert!(c.contains("k0"));
    }

    #[test]
    fn cover_cache_contains_does_not_refresh_recency() {
        let mut c = CoverByteCache::new();
        for i in 0..COVER_BYTE_CACHE_CAPACITY {
            c.insert(format!("k{i}"), vec![0]);
        }
        assert!(c.contains("k0"));
        c.insert("new".into(), vec![1]);
        assert!(!c.contains("k0"), "contains must not mark k0 as recent");
    }

    #[test]
    fn cover_cache_lazy_load_lifecycle() {
        let mut c = CoverByteCache::new();
        assert!(c.begin_load("a"));
        assert!(!c.begin_load("a"), "second request while in flight");
        c.finish_load("a".into(), Some(vec![1, 2]));
        assert!(!c.begin_load("a"), "already cached");
        assert_eq!(c.get("a").as_deref(), Some(&vec![1, 2]));

        assert!(c.begin_load("b"));
        c.finish_load("b".into(), None);
        assert!(!c.begin_load("b"), "known to have no art");
        c.clear();
        assert!(c.is_empty());
        assert!(c.begin_load("b"), "clear forgets the no-art marker");
    }
}
