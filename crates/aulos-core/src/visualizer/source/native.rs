// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The desktop's presets and textures: plain files under the preset search
//! directories.

use super::{TextureImage, texture_name};
use crate::visualizer::presets::{PresetEntry, preset_search_dirs, scan_presets};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Every preset under the search directories, sorted for display.
pub fn find_presets() -> Vec<PresetEntry> {
    let dirs = preset_search_dirs(None);
    let found = scan_presets(&dirs);
    tracing::info!("lactea: {} presets from {dirs:?}", found.len());
    found
}

/// The bytes of the preset `path` (an [`PresetEntry::path`]).
///
/// # Errors
/// The reason the file cannot be read.
pub fn read_preset(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| e.to_string())
}

/// The user textures presets may sample: the images under the preset search
/// directories (and their sibling `textures` directories), by lowercase file
/// stem. The renderer supplies its own noise textures.
pub struct Textures {
    /// `(lowercase stem, path)`, sorted.
    files: Vec<(String, PathBuf)>,
}

impl Textures {
    /// Finds the textures.
    pub fn find() -> Self {
        let dirs = preset_search_dirs(None);
        let mut roots = dirs.clone();
        roots.extend(
            dirs.iter()
                .filter_map(|d| d.parent().map(|p| p.join("textures"))),
        );
        let mut files = Vec::new();
        for root in roots.iter().filter(|r| r.is_dir()) {
            for entry in WalkDir::new(root).follow_links(true).into_iter().flatten() {
                if entry.file_type().is_file()
                    && let Some(name) = texture_name(entry.path())
                {
                    files.push((name, entry.path().to_path_buf()));
                }
            }
        }
        files.sort();
        files.dedup();
        tracing::info!("lactea: {} user textures", files.len());
        Self { files }
    }

    /// The texture named `name` (case-insensitive), decoded.
    pub fn load(&self, name: &str) -> Option<TextureImage> {
        let lower = name.to_ascii_lowercase();
        self.files
            .iter()
            .filter(|(stem, _)| *stem == lower)
            .find_map(|(_, path)| {
                let rgba = image::open(path).ok()?.to_rgba8();
                let (width, height) = rgba.dimensions();
                Some(TextureImage {
                    width,
                    height,
                    rgba8: rgba.into_raw(),
                })
            })
    }

    /// Every texture name, for presets that pick a random one.
    pub fn names(&self) -> Vec<String> {
        self.files.iter().map(|(stem, _)| stem.clone()).collect()
    }
}
