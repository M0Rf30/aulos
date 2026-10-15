// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The browser's presets and textures: no folders to scan, so a process-wide
//! [`Registry`] holds what there is.
//!
//! * **Bundled** presets are compiled in (`assets/presets/`, about 250 KB):
//!   twelve original MilkDrop 2 / MilkDrop 3 presets by Gianluca Boiano
//!   (M0Rf30), taken from <https://github.com/M0Rf30/projectm-presets> at
//!   commit `ec5b039` (the `projectm-presets-m0rf30-git` package) under its
//!   MIT license, which sits next to them as `assets/presets/LICENSE`. Six
//!   are plain `.milk`, six are MilkDrop 3 `.milk2` double presets; none
//!   uses an image beyond the renderer's own noise textures.
//! * **Imported** files are the `.milk` / `.milk2` presets (and the images
//!   they sample) the user picks with [`import_from_picker`]. They are kept in
//!   IndexedDB ([`crate::web_fs::store_files`]) and put back by [`restore`]
//!   at startup; where IndexedDB is refused (private windows, a full disk)
//!   they last for the session and the import reports why.

use super::TextureImage;
use super::registry::{Bundled, Kind, Registry, TextureFile, decode_texture};
use crate::visualizer::presets::PresetEntry;
use crate::web_fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Bundles `assets/presets/<name>` for each file name given.
macro_rules! bundled {
    ($($name:literal),+ $(,)?) => {
        [$(($name, include_bytes!(concat!("../../../assets/presets/", $name)) as &[u8])),+]
    };
}

const BUNDLED: [Bundled; 12] = bundled![
    "M0Rf30 - Aurora Cathedral.milk",
    "M0Rf30 - Bioluminescent Abyss.milk",
    "M0Rf30 - Crystal Cathedral.milk2",
    "M0Rf30 - Cymatic Prism.milk2",
    "M0Rf30 - Event Horizon.milk2",
    "M0Rf30 - Fractal Sanctum.milk2",
    "M0Rf30 - Hyperspace.milk2",
    "M0Rf30 - Liquid Chrome Kaleidoscope.milk",
    "M0Rf30 - Liquid Silk.milk2",
    "M0Rf30 - Nebula Forge.milk",
    "M0Rf30 - Neon Sacred Geometry.milk",
    "M0Rf30 - Tramonto sul Golfo.milk",
];

/// IndexedDB key of the imported files.
const STORE_KEY: &str = "visualizer-imports";
/// What the import dialog offers: presets, and the images they may sample.
const ACCEPT: &str = ".milk,.milk2,.jpg,.jpeg,.png,.tga,.bmp,.dib";
/// Largest file an import reads (a preset is some tens of kilobytes).
const IMPORT_LIMIT: u64 = 8 << 20;

static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The bundled and the imported presets, sorted for display.
pub fn find_presets() -> Vec<PresetEntry> {
    registry().presets(&BUNDLED)
}

/// The bytes of the preset `path` (a [`PresetEntry::path`]).
///
/// # Errors
/// A message when no bundled or imported preset has that path.
pub fn read_preset(path: &Path) -> Result<Vec<u8>, String> {
    registry()
        .read(&BUNDLED, path)
        .ok_or_else(|| format!("{} is not a preset of this session", path.display()))
}

/// The imported images presets may sample, by lowercase file stem. The
/// renderer supplies its own noise textures.
pub struct Textures {
    files: Vec<TextureFile>,
}

impl Textures {
    /// Takes stock of the imported images.
    pub fn find() -> Self {
        let files = registry().textures();
        tracing::info!("lactea: {} imported textures", files.len());
        Self { files }
    }

    /// The texture named `name` (case-insensitive), decoded.
    pub fn load(&self, name: &str) -> Option<TextureImage> {
        let lower = name.to_ascii_lowercase();
        self.files
            .iter()
            .filter(|texture| texture.name == lower)
            .find_map(|texture| decode_texture(&texture.file, &texture.bytes))
    }

    /// Every texture name, for presets that pick a random one.
    pub fn names(&self) -> Vec<String> {
        self.files.iter().map(|t| t.name.clone()).collect()
    }
}

/// What an import added.
pub struct Imported {
    /// The new presets' paths, in the order they were picked.
    pub presets: Vec<PathBuf>,
    /// How many images came with them.
    pub textures: usize,
    /// How many files were left out: neither a preset nor an image, or too
    /// large.
    pub skipped: usize,
    /// Why the files could not be kept for the next visit, when so.
    pub not_saved: Option<String>,
}

/// Lets the user pick preset files (and images) and adds them. Nothing is
/// added when the dialog is cancelled. Must be called from a user gesture.
///
/// # Errors
/// A message when the dialog cannot be opened or a file cannot be read.
pub async fn import_from_picker() -> Result<Imported, String> {
    let picked = web_fs::pick_files(ACCEPT, IMPORT_LIMIT).await?;
    let mut imported = Imported {
        presets: Vec::new(),
        textures: 0,
        skipped: picked.too_large,
        not_saved: None,
    };
    {
        let mut registry = registry();
        for (name, bytes) in picked.files {
            match registry.add(&name, bytes) {
                Some(Kind::Preset) => imported.presets.push(Registry::imported_path(&name)),
                Some(Kind::Texture) => imported.textures += 1,
                None => imported.skipped += 1,
            }
        }
    }
    if !imported.presets.is_empty() || imported.textures > 0 {
        imported.not_saved = save().await.err();
    }
    Ok(imported)
}

/// Whether the user has imported anything (and so has something to
/// [`forget_imported`]).
pub fn has_imported() -> bool {
    !registry().is_empty()
}

/// Drops everything the user imported, here and in IndexedDB.
///
/// # Errors
/// A message when the saved copy could not be removed (the files are gone
/// for this session regardless).
pub async fn forget_imported() -> Result<(), String> {
    registry().clear();
    save().await
}

/// Puts back what earlier visits imported. Call once at startup.
pub async fn restore() {
    match web_fs::load_files(STORE_KEY).await {
        Ok(files) => {
            let mut registry = registry();
            for (name, bytes) in files {
                registry.add(&name, bytes);
            }
        }
        Err(reason) => tracing::warn!("visualizer: cannot read the imported presets: {reason}"),
    }
}

/// Replaces the saved copy with what the registry holds now.
async fn save() -> Result<(), String> {
    let files = registry()
        .files()
        .map(|(name, bytes)| (name.to_owned(), bytes.to_vec()))
        .collect();
    web_fs::store_files(STORE_KEY, files).await
}
