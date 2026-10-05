// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Cover art extraction from audio files and directory images.

use super::tags;
use image::{ImageBuffer, Rgba, RgbaImage};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::Path;

/// Handles cover art extraction and caching.
pub struct CoverArt;

impl CoverArt {
    /// Extract embedded cover art from an audio file.
    /// Returns the raw image bytes (JPEG/PNG) if found.
    pub fn extract_from_file(path: &Path) -> Option<Vec<u8>> {
        let probed = tags::probe(path, true)?;
        let pictures = probed.tags.pictures;

        // Prefer front cover, but take any picture.
        let pic = pictures.iter().find(|p| p.is_front_cover).or_else(|| pictures.first())?;

        Some(pic.data.clone())
    }

    /// Look for cover art files in the same directory as the audio file.
    /// Common names: cover.jpg, folder.jpg, front.jpg, album.jpg, etc.
    pub fn find_in_directory(audio_path: &Path) -> Option<Vec<u8>> {
        let dir = audio_path.parent()?;

        let cover_names = [
            "cover", "folder", "front", "album", "artwork", "art", "thumb",
        ];
        let extensions = ["jpg", "jpeg", "png", "webp", "bmp"];

        for name in &cover_names {
            for ext in &extensions {
                let candidate = dir.join(format!("{name}.{ext}"));
                if candidate.exists() {
                    return std::fs::read(&candidate).ok();
                }
                // Also check uppercase
                let candidate_upper = dir.join(format!("{}.{ext}", name.to_uppercase()));
                if candidate_upper.exists() {
                    return std::fs::read(&candidate_upper).ok();
                }
            }
        }

        None
    }

    /// Get cover art for a track: try embedded first, then directory.
    pub fn get_cover_art(audio_path: &Path) -> Option<Vec<u8>> {
        Self::extract_from_file(audio_path).or_else(|| Self::find_in_directory(audio_path))
    }

    /// Generate an album key for caching (artist + album).
    pub fn album_key(artist: &str, album: &str) -> String {
        format!("{artist}||{album}")
    }

    /// Max side length (px) for a grid/list cover-art thumbnail. Sized so
    /// a 160px (logical) grid card stays crisp on 2x HiDPI displays with
    /// headroom to spare, while remaining far smaller than typical
    /// full-resolution embedded cover art (which can be several thousand
    /// pixels per side) — see `decode_thumbnail`.
    pub const GRID_THUMBNAIL_MAX_DIM: u32 = 320;

    /// Max side length (px) for the larger cover shown in the expanded
    /// now-playing view, which can render at up to roughly half the
    /// window width.
    pub const EXPANDED_COVER_MAX_DIM: u32 = 960;

    /// Max side length (px) for podcast/radio favicons, which are
    /// rendered small (list rows / avatar-sized) and rarely larger than
    /// this to begin with.
    pub const ONLINE_ICON_MAX_DIM: u32 = 160;

    /// Decode encoded cover-art bytes and return a downscaled RGBA8
    /// thumbnail as `(width, height, pixels)`, sized to fit within
    /// `max_dim` on its longer side while preserving aspect ratio. Never
    /// upscales: an image already smaller than `max_dim` is returned at
    /// its original size.
    ///
    /// Building `widget::icon::Handle`s from this pre-decoded pixel
    /// buffer (via `widget::icon::from_raster_pixels`) instead of the
    /// original encoded bytes (`from_raster_bytes`) means the full-size
    /// JPEG/PNG is decoded here, once, off the UI thread, rather than
    /// re-decoded on the UI thread every time iced's wgpu raster cache
    /// evicts and re-uploads the handle (e.g. on every page/drawer
    /// switch that stops drawing it for one frame).
    pub fn decode_thumbnail(bytes: &[u8], max_dim: u32) -> Option<(u32, u32, Vec<u8>)> {
        let img = image::load_from_memory(bytes).ok()?;
        let (w, h) = (img.width(), img.height());
        if w == 0 || h == 0 {
            return None;
        }
        // Single scale factor applied to both dimensions preserves aspect
        // ratio exactly (modulo integer rounding); capping at 1.0 means an
        // already-small image is returned as-is instead of upscaled.
        let scale = (max_dim as f32 / w.max(h) as f32).min(1.0);
        let resized = if scale < 1.0 {
            let target_w = ((w as f32 * scale).round() as u32).max(1);
            let target_h = ((h as f32 * scale).round() as u32).max(1);
            img.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3)
        } else {
            img
        };
        let rgba = resized.into_rgba8();
        let (rw, rh) = rgba.dimensions();
        Some((rw, rh, rgba.into_raw()))
    }

    /// Generate a colored circle avatar with initials for an artist name.
    /// Returns PNG bytes. Prefer `generate_artist_avatar_pixels` for new
    /// call sites building a `widget::icon::Handle` directly — this
    /// wraps it purely for callers that need an encoded, shareable byte
    /// blob.
    pub fn generate_artist_avatar(name: &str, size: u32) -> Vec<u8> {
        let (w, h, pixels) = Self::generate_artist_avatar_pixels(name, size);
        let img: RgbaImage =
            ImageBuffer::from_raw(w, h, pixels).unwrap_or_else(|| ImageBuffer::new(w, h));
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap_or_default();
        buf
    }

    /// Generate a colored circle avatar with initials for an artist name,
    /// as raw RGBA8 pixels (`(width, height, pixels)`) rather than
    /// encoded PNG bytes — this small procedural image never needs an
    /// encode/decode round trip, so callers building a
    /// `widget::icon::Handle` should use `widget::icon::from_raster_pixels`
    /// directly on this output.
    pub fn generate_artist_avatar_pixels(name: &str, size: u32) -> (u32, u32, Vec<u8>) {
        let initials = artist_initials(name);
        let color = deterministic_color(name);

        let mut img: RgbaImage = ImageBuffer::new(size, size);
        let center = size as f32 / 2.0;
        let radius = center - 1.0;

        // Draw filled circle
        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 - center;
                let dy = y as f32 - center;
                if dx * dx + dy * dy <= radius * radius {
                    img.put_pixel(x, y, Rgba(color));
                } else {
                    img.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                }
            }
        }

        // Draw initials as a simple block pattern (no font dependency)
        // Use a lighter shade in the center area to suggest text
        let text_size = size / 3;
        let text_y_start = (size - text_size) / 2;
        let char_width = text_size / (initials.len() as u32).max(1);
        let text_x_start = (size - char_width * initials.len() as u32) / 2;

        for (i, _ch) in initials.chars().enumerate() {
            let cx = text_x_start + i as u32 * char_width + char_width / 2;
            let cy = text_y_start + text_size / 2;
            // Draw a small filled rectangle per character as a stylized block
            let block_w = char_width * 2 / 3;
            let block_h = text_size * 2 / 3;
            for dy in 0..block_h {
                for dx in 0..block_w {
                    let px = cx - block_w / 2 + dx;
                    let py = cy - block_h / 2 + dy;
                    if px < size && py < size {
                        let dist =
                            ((px as f32 - center).powi(2) + (py as f32 - center).powi(2)).sqrt();
                        if dist <= radius {
                            img.put_pixel(px, py, Rgba([255, 255, 255, 200]));
                        }
                    }
                }
            }
        }

        (size, size, img.into_raw())
    }

    /// Public accessor for the deterministic initials used by the
    /// generated avatar — shared by `crate::views::common::initials_avatar`,
    /// which renders them as widget text instead of rasterizing them, so
    /// the placeholder avatar stays crisp at any size/HiDPI scale factor.
    pub fn artist_initials(name: &str) -> String {
        artist_initials(name)
    }

    /// Public accessor for the deterministic background color used by the
    /// generated avatar, as plain RGB — see `artist_initials`.
    pub fn artist_avatar_color(name: &str) -> (u8, u8, u8) {
        let [r, g, b, _a] = deterministic_color(name);
        (r, g, b)
    }
}

fn artist_initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

fn deterministic_color(name: &str) -> [u8; 4] {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    let hash = hasher.finish();

    // HSL-like palette: pick hue from hash, keep saturation/lightness pleasant
    let hue = (hash % 360) as f32;
    let (r, g, b) = hsl_to_rgb(hue, 0.55, 0.45);
    [r, g, b, 255]
}

pub(crate) fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

#[cfg(test)]
mod thumbnail_tests {
    use super::CoverArt;

    fn encode_test_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn downscales_large_image_preserving_aspect() {
        let bytes = encode_test_png(1000, 500);
        let (w, h, pixels) = CoverArt::decode_thumbnail(&bytes, 320).expect("decodes");
        assert!(w <= 320 && h <= 320, "got {w}x{h}");
        // 2:1 source aspect ratio preserved within integer-rounding error.
        assert!(
            (w as f32 / h as f32 - 2.0).abs() < 0.05,
            "aspect ratio drifted: {w}x{h}"
        );
        assert_eq!(pixels.len(), (w * h * 4) as usize);
    }

    #[test]
    fn does_not_upscale_small_image() {
        let bytes = encode_test_png(64, 48);
        let (w, h, _) = CoverArt::decode_thumbnail(&bytes, 320).expect("decodes");
        assert_eq!((w, h), (64, 48));
    }

    #[test]
    fn rejects_undecodable_bytes() {
        assert!(CoverArt::decode_thumbnail(b"not an image", 320).is_none());
    }

    #[test]
    fn avatar_pixels_match_declared_size() {
        let (w, h, pixels) = CoverArt::generate_artist_avatar_pixels("Jane Doe", 64);
        assert_eq!((w, h), (64, 64));
        assert_eq!(pixels.len(), (64 * 64 * 4) as usize);
    }

    #[test]
    fn artist_initials_takes_first_two_word_leaders_uppercased() {
        assert_eq!(CoverArt::artist_initials("daft punk"), "DP");
        assert_eq!(CoverArt::artist_initials("Beyoncé"), "B");
        assert_eq!(CoverArt::artist_initials("  "), "");
    }

    #[test]
    fn artist_avatar_color_is_deterministic_and_name_sensitive() {
        let a1 = CoverArt::artist_avatar_color("Daft Punk");
        let a2 = CoverArt::artist_avatar_color("Daft Punk");
        let b = CoverArt::artist_avatar_color("Air");
        assert_eq!(a1, a2, "same name must always produce the same color");
        assert_ne!(a1, b, "different names should (almost always) differ");
    }
}
