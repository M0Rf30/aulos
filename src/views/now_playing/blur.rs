// SPDX-License-Identifier: GPL-3.0

//! Blurred cover art generation and caching for the playback bar background.

use image::DynamicImage;

/// Compute a blurred version of the cover art for use as a background.
///
/// The image is resized to 400px wide (maintaining aspect ratio) and then
/// blurred with a Gaussian filter (sigma = 30.0). Returns the blurred
/// image as already-decoded RGBA8 pixels (`(width, height, pixels)`)
/// rather than re-encoded bytes, so the caller can build a
/// `widget::icon::Handle` with `widget::icon::from_raster_pixels`
/// directly -- skipping both a PNG encode here and a decode on every
/// frame iced's wgpu raster cache re-uploads the handle.
///
/// Returns `None` if the input bytes cannot be decoded as an image.
pub fn compute_blurred_cover(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    // Load image from memory
    let img = image::load_from_memory(bytes).ok()?;

    // Resize to 400px wide, maintaining aspect ratio
    let resized = resize_to_width(&img, 400);

    // Apply Gaussian blur with sigma = 30.0. `DynamicImage`'s
    // `GenericImageView::Pixel` is always `Rgba<u8>`, so this yields an
    // `ImageBuffer<Rgba<u8>, Vec<u8>>` -- already the RGBA8 layout
    // `from_raster_pixels` wants.
    let blurred = image::imageops::blur(&resized, 30.0);
    let (w, h) = blurred.dimensions();

    Some((w, h, blurred.into_raw()))
}

/// Resize an image to a specific width, maintaining aspect ratio.
fn resize_to_width(img: &DynamicImage, target_width: u32) -> DynamicImage {
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
