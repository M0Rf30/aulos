// SPDX-License-Identifier: GPL-3.0

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

    /// Generate a colored circle avatar with initials for an artist name.
    /// Returns PNG bytes.
    pub fn generate_artist_avatar(name: &str, size: u32) -> Vec<u8> {
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

        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap_or_default();
        buf
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
