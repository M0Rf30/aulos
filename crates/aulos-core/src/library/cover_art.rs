// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Cover art extraction from audio files and directory images.

use super::tags;
use image::{ImageBuffer, Rgba, RgbaImage};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::Path;

/// File names (without extension) of a folder's cover image, best first.
const COVER_NAMES: [&str; 7] = [
    "cover", "folder", "front", "album", "artwork", "art", "thumb",
];

/// Extensions of a folder's cover image, best first.
const COVER_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "png", "webp", "bmp"];

/// The name, among the `names` of a folder's files, of its cover image: the
/// best by [`COVER_NAMES`] then [`COVER_EXTENSIONS`], ignoring case. For
/// sources that list a folder instead of probing for each candidate name.
#[must_use]
pub fn pick_cover_file(names: &[String]) -> Option<&str> {
    let lowered: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    COVER_NAMES.iter().find_map(|name| {
        COVER_EXTENSIONS.iter().find_map(|ext| {
            let wanted = format!("{name}.{ext}");
            lowered
                .iter()
                .position(|n| *n == wanted)
                .map(|i| names[i].as_str())
        })
    })
}

/// Whether a folder name looks like one disc of a multi-disc album: CD, Disc,
/// Disk or Disco (any case), an optional space, `-` or `_`, then a number.
#[must_use]
pub fn is_disc_folder_name(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    let Some(rest) = ["disco", "disc", "disk", "cd"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
    else {
        return false;
    };
    let digits = rest.trim_start_matches([' ', '-', '_']);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// The album folder above `dir` when `dir` is a disc folder.
#[must_use]
pub fn disc_parent(dir: &Path) -> Option<&Path> {
    is_disc_folder_name(dir.file_name()?.to_str()?)
        .then(|| dir.parent())
        .flatten()
}

/// Handles cover art extraction and caching.
pub struct CoverArt;

impl CoverArt {
    /// Cheap content fingerprint of an encoded cover blob. Only compared
    /// within one process run (never persisted), so the std hasher is fine.
    /// Lets a library refresh recognise an unchanged cover and keep its
    /// existing handle without the loader having to ship (and the app keep)
    /// the full-size bytes.
    #[must_use]
    pub fn content_fingerprint(bytes: &[u8]) -> u64 {
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    }

    /// Extract embedded cover art from an audio file.
    /// Returns the raw image bytes (JPEG/PNG) if found.
    pub fn extract_from_file(path: &Path) -> Option<Vec<u8>> {
        Self::embedded_picture(tags::probe(path, true)?)
    }

    /// The embedded cover of a probed file (probed with pictures wanted):
    /// the front cover, else any picture.
    #[must_use]
    pub fn embedded_picture(probed: tags::ProbedFile) -> Option<Vec<u8>> {
        let mut pictures = probed.tags.pictures;
        let index = pictures
            .iter()
            .position(|p| p.is_front_cover)
            .or_else(|| (!pictures.is_empty()).then_some(0))?;
        Some(pictures.swap_remove(index).data)
    }

    /// Look for cover art files in the same directory as the audio file.
    /// Common names: cover.jpg, folder.jpg, front.jpg, album.jpg, etc.
    ///
    /// When the directory is a disc folder (`CD1`, `Disc 2`, ...) and has no
    /// cover of its own, the album folder above it is searched too.
    pub fn find_in_directory(audio_path: &Path) -> Option<Vec<u8>> {
        let dir = audio_path.parent()?;
        Self::find_in_dir(dir).or_else(|| {
            let parent = disc_parent(dir)?;
            Self::find_in_dir(parent)
        })
    }

    fn find_in_dir(dir: &Path) -> Option<Vec<u8>> {
        for name in &COVER_NAMES {
            for ext in &COVER_EXTENSIONS {
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
    pub const GRID_THUMBNAIL_MAX_DIM: u32 = 256;

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
    /// re-decoded on the UI thread every time the toolkit's GPU image cache
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

    /// Compute a blurred version of the cover art for use as a background.
    ///
    /// The image is resized to 400px wide (maintaining aspect ratio) and then
    /// blurred with a Gaussian filter (sigma = 30.0). Returns the blurred
    /// image as already-decoded RGBA8 pixels (`(width, height, pixels)`)
    /// rather than re-encoded bytes, so the caller can build an icon/texture
    /// handle directly -- skipping both a PNG encode here and a decode on
    /// every frame the UI's raster cache re-uploads the handle.
    ///
    /// Returns `None` if the input bytes cannot be decoded as an image.
    pub fn blurred_backdrop(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        // Load image from memory
        let img = image::load_from_memory(bytes).ok()?;

        // Resize to 400px wide, maintaining aspect ratio
        let resized = resize_to_width(&img, 400);

        // Apply Gaussian blur with sigma = 30.0. `DynamicImage`'s
        // `GenericImageView::Pixel` is always `Rgba<u8>`, so this yields an
        // `ImageBuffer<Rgba<u8>, Vec<u8>>` -- already the RGBA8 layout
        // raster handles want.
        let blurred = image::imageops::blur(&resized, 30.0);
        let (w, h) = blurred.dimensions();

        Some((w, h, blurred.into_raw()))
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

/// Resize an image to a specific width, maintaining aspect ratio.
fn resize_to_width(img: &image::DynamicImage, target_width: u32) -> image::DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w == 0 {
        return img.clone();
    }
    let ratio = target_width as f32 / w as f32;
    let target_height = (h as f32 * ratio) as u32;

    img.resize(
        target_width,
        target_height,
        image::imageops::FilterType::Triangle,
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
    fn blurred_backdrop_is_400_wide_and_keeps_aspect() {
        let bytes = encode_test_png(800, 400);
        let (w, h, pixels) = CoverArt::blurred_backdrop(&bytes).expect("decodes");
        assert_eq!((w, h), (400, 200));
        assert_eq!(pixels.len(), (w * h * 4) as usize);
    }

    #[test]
    fn blurred_backdrop_rejects_undecodable_bytes() {
        assert!(CoverArt::blurred_backdrop(b"not an image").is_none());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn picks_the_best_cover_file_ignoring_case() {
        let files = names(&["01 Song.flac", "Folder.PNG", "COVER.jpeg", "back.jpg"]);
        assert_eq!(pick_cover_file(&files), Some("COVER.jpeg"));
        let files = names(&["cover.png", "cover.jpg"]);
        assert_eq!(pick_cover_file(&files), Some("cover.jpg"));
        assert_eq!(pick_cover_file(&names(&["notes.txt", "back.jpg"])), None);
        assert_eq!(pick_cover_file(&[]), None);
    }

    #[test]
    fn disc_folder_names() {
        for yes in ["CD1", "cd 2", "Disc 01", "disk-3", "DISCO_4", "Cd10"] {
            assert!(is_disc_folder_name(yes), "{yes}");
        }
        for no in ["CD", "Disc", "Discovery", "CD1 Bonus", "Album", "2CD", ""] {
            assert!(!is_disc_folder_name(no), "{no}");
        }
    }

    #[test]
    fn finds_cover_in_parent_of_disc_folder() {
        let root = std::env::temp_dir().join(format!("aulos-disc-cover-{}", std::process::id()));
        let cd1 = root.join("Artist/Album/CD1");
        std::fs::create_dir_all(&cd1).unwrap();
        std::fs::write(root.join("Artist/Album/cover.jpg"), b"img").unwrap();
        assert_eq!(
            CoverArt::find_in_directory(&cd1.join("01.flac")).as_deref(),
            Some(&b"img"[..])
        );
        // A non-disc folder does not look upward.
        let other = root.join("Artist/Album/Bonus");
        std::fs::create_dir_all(&other).unwrap();
        assert!(CoverArt::find_in_directory(&other.join("01.flac")).is_none());
        // Own cover wins.
        std::fs::write(cd1.join("folder.png"), b"own").unwrap();
        assert_eq!(
            CoverArt::find_in_directory(&cd1.join("01.flac")).as_deref(),
            Some(&b"own"[..])
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
