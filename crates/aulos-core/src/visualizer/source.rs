// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Where the visualizer's presets and textures come from.
//!
//! On the desktop they are files: the presets found under the search
//! directories ([`super::presets::preset_search_dirs`]), and the images next
//! to them. The browser has no folders to scan, so there they are held in
//! memory: the set bundled with Aulos plus the files the user picked, kept in
//! IndexedDB between visits (`source/web.rs`). Either way a preset is named
//! by its [`PresetEntry::path`](super::presets::PresetEntry::path) (a virtual
//! one in the browser) and its bytes are read with [`read_preset`], so the
//! renderer, the preset browser and the playlist do not care which it is.

use std::path::Path;

#[cfg_attr(target_arch = "wasm32", path = "source/web.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "source/native.rs")]
mod host;
// The browser's in-memory set; plain code, so it is tested natively.
#[cfg(any(target_arch = "wasm32", test))]
mod registry;

#[cfg(target_arch = "wasm32")]
pub use host::{Imported, forget_imported, has_imported, import_from_picker, restore};
pub use host::{Textures, find_presets, read_preset};

/// Image extensions a preset `sampler_*` texture may use.
const TEXTURE_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "tga", "bmp", "dib"];

/// A decoded texture image.
pub struct TextureImage {
    pub width: u32,
    pub height: u32,
    /// `width * height` RGBA pixels, 8 bits each.
    pub rgba8: Vec<u8>,
}

/// The name a preset refers to the texture file `path` by: its stem, in
/// lowercase. `None` when the file is not an image a preset can use.
fn texture_name(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    TEXTURE_EXTENSIONS
        .iter()
        .any(|known| ext.eq_ignore_ascii_case(known))
        .then(|| path.file_stem()?.to_str().map(str::to_ascii_lowercase))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_names_are_lowercase_stems_of_images() {
        assert_eq!(
            texture_name(Path::new("/x/Clouds.JPG")),
            Some("clouds".to_owned())
        );
        assert_eq!(
            texture_name(Path::new("sub/Stone Wall.png")),
            Some("stone wall".to_owned())
        );
    }

    #[test]
    fn other_files_are_not_textures() {
        assert_eq!(texture_name(Path::new("preset.milk")), None);
        assert_eq!(texture_name(Path::new("png")), None);
        assert_eq!(texture_name(Path::new("notes.txt")), None);
    }
}
