// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The presets and textures a browser session holds in memory: the set
//! bundled with Aulos (passed in, so it is not part of this logic) and the
//! files the user imported.
//!
//! The files have no directory to live in, so each gets a virtual path under
//! one of two pretend directories, [`BUNDLED_DIR`] and [`IMPORTED_DIR`]. The
//! preset browser files a preset under the name of its parent directory, so
//! these double as the two category headings. This is plain code (the
//! browser glue is in `web.rs`), so it is tested natively.

#![cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser build keeps its presets here")
)]

use super::{TextureImage, texture_name};
use crate::visualizer::presets::{PresetEntry, is_preset_file, sort_entries};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Parent of the presets bundled with Aulos; the browser lists them under
/// "bundled".
pub const BUNDLED_DIR: &str = "/presets_bundled";
/// Parent of the presets the user imported; listed under "imported".
pub const IMPORTED_DIR: &str = "/presets_imported";

/// What an imported file is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A `.milk` / `.milk2` preset.
    Preset,
    /// An image a preset may sample.
    Texture,
}

impl Kind {
    /// What a file of this name is, if anything the visualizer can use.
    pub fn of(name: &str) -> Option<Self> {
        let path = Path::new(name);
        if is_preset_file(path) {
            Some(Self::Preset)
        } else {
            texture_name(path).map(|_| Self::Texture)
        }
    }
}

/// A bundled preset: file name (with extension) and contents.
pub type Bundled = (&'static str, &'static [u8]);

/// An imported image a preset may sample.
pub struct TextureFile {
    /// What presets call it: the lowercase file stem.
    pub name: String,
    /// The file name it was imported as (its extension gives the format).
    pub file: String,
    pub bytes: Arc<[u8]>,
}

/// The files the user imported, by file name.
#[derive(Default)]
pub struct Registry {
    imported: BTreeMap<String, Arc<[u8]>>,
}

impl Registry {
    pub const fn new() -> Self {
        Self {
            imported: BTreeMap::new(),
        }
    }

    /// Adds (or replaces) an imported file and says what it was taken for;
    /// `None` and nothing added when it is neither a preset nor an image.
    pub fn add(&mut self, name: &str, bytes: Vec<u8>) -> Option<Kind> {
        let kind = Kind::of(name)?;
        self.imported.insert(name.to_owned(), bytes.into());
        Some(kind)
    }

    pub fn is_empty(&self) -> bool {
        self.imported.is_empty()
    }

    pub fn clear(&mut self) {
        self.imported.clear();
    }

    /// Every imported file, to save them.
    pub fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.imported
            .iter()
            .map(|(name, bytes)| (name.as_str(), &bytes[..]))
    }

    /// The virtual path of the imported preset `name`.
    pub fn imported_path(name: &str) -> PathBuf {
        Path::new(IMPORTED_DIR).join(name)
    }

    /// The bundled and the imported presets, sorted for display.
    pub fn presets(&self, bundled: &[Bundled]) -> Vec<PresetEntry> {
        let bundled = bundled
            .iter()
            .map(|(name, _)| Path::new(BUNDLED_DIR).join(name));
        let imported = self
            .imported
            .keys()
            .filter(|name| is_preset_file(Path::new(name)))
            .map(|name| Self::imported_path(name));
        let mut entries: Vec<PresetEntry> = bundled
            .chain(imported)
            .map(PresetEntry::from_path)
            .collect();
        sort_entries(&mut entries);
        entries
    }

    /// The contents of the preset at the virtual path `path`.
    pub fn read(&self, bundled: &[Bundled], path: &Path) -> Option<Vec<u8>> {
        let name = path.file_name()?.to_str()?;
        match path.parent()? {
            dir if dir == Path::new(BUNDLED_DIR) => bundled
                .iter()
                .find(|(known, _)| *known == name)
                .map(|(_, bytes)| bytes.to_vec()),
            dir if dir == Path::new(IMPORTED_DIR) => {
                self.imported.get(name).map(|bytes| bytes.to_vec())
            }
            _ => None,
        }
    }

    /// The imported images, sorted by name.
    pub fn textures(&self) -> Vec<TextureFile> {
        let mut found: Vec<TextureFile> = self
            .imported
            .iter()
            .filter_map(|(file, bytes)| {
                Some(TextureFile {
                    name: texture_name(Path::new(file))?,
                    file: file.clone(),
                    bytes: Arc::clone(bytes),
                })
            })
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }
}

/// Decodes the image `bytes` of the file `name`; the format comes from the
/// extension (TGA has no signature to tell it by).
pub fn decode_texture(name: &str, bytes: &[u8]) -> Option<TextureImage> {
    let format = image::ImageFormat::from_path(name).ok()?;
    let rgba = image::load_from_memory_with_format(bytes, format)
        .ok()?
        .to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(TextureImage {
        width,
        height,
        rgba8: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const BUNDLED: [Bundled; 2] = [("Zeta.milk", b"zeta"), ("alpha.milk2", b"alpha")];

    #[test]
    fn kinds_follow_the_extension() {
        assert_eq!(Kind::of("a.milk"), Some(Kind::Preset));
        assert_eq!(Kind::of("a.MILK2"), Some(Kind::Preset));
        assert_eq!(Kind::of("a.tga"), Some(Kind::Texture));
        assert_eq!(Kind::of("a.txt"), None);
        assert_eq!(Kind::of("milk"), None);
    }

    #[test]
    fn only_presets_and_images_are_kept() {
        let mut registry = Registry::new();
        assert_eq!(registry.add("p.milk", b"p".to_vec()), Some(Kind::Preset));
        assert_eq!(registry.add("t.png", b"t".to_vec()), Some(Kind::Texture));
        assert_eq!(registry.add("readme.txt", b"r".to_vec()), None);
        assert_eq!(registry.files().count(), 2);
    }

    #[test]
    fn adding_a_name_again_replaces_the_file() {
        let mut registry = Registry::new();
        registry.add("p.milk", b"old".to_vec());
        registry.add("p.milk", b"new".to_vec());
        assert_eq!(
            registry.read(&BUNDLED, &Registry::imported_path("p.milk")),
            Some(b"new".to_vec())
        );
        assert_eq!(registry.files().count(), 1);
    }

    #[test]
    fn presets_list_bundled_then_imported_by_category() {
        let mut registry = Registry::new();
        registry.add("Mine.milk", b"m".to_vec());
        registry.add("wall.png", b"w".to_vec());
        let listed: Vec<_> = registry
            .presets(&BUNDLED)
            .into_iter()
            .map(|e| (e.category, e.name))
            .collect();
        assert_eq!(
            listed,
            [
                ("bundled".to_owned(), "alpha".to_owned()),
                ("bundled".to_owned(), "Zeta".to_owned()),
                ("imported".to_owned(), "Mine".to_owned()),
            ]
        );
    }

    #[test]
    fn read_resolves_both_directories_and_nothing_else() {
        let mut registry = Registry::new();
        registry.add("Mine.milk", b"mine".to_vec());
        let read = |path: &str| registry.read(&BUNDLED, Path::new(path));
        assert_eq!(read("/presets_bundled/Zeta.milk"), Some(b"zeta".to_vec()));
        assert_eq!(read("/presets_imported/Mine.milk"), Some(b"mine".to_vec()));
        assert_eq!(read("/presets_bundled/Mine.milk"), None);
        assert_eq!(read("/presets_imported/Zeta.milk"), None);
        assert_eq!(read("/elsewhere/Zeta.milk"), None);
    }

    #[test]
    fn textures_are_named_by_lowercase_stem() {
        let mut registry = Registry::new();
        registry.add("Wall.PNG", b"w".to_vec());
        registry.add("a.milk", b"a".to_vec());
        let names: Vec<_> = registry.textures().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["wall"]);
    }

    #[test]
    fn decode_texture_reads_the_format_the_extension_names() {
        // A 2x1 PNG made by the encoder itself.
        let mut png = Vec::new();
        image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255])
            .unwrap()
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let decoded = decode_texture("wall.png", &png).expect("a PNG decodes");
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.rgba8, [255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(decode_texture("wall.png", b"not an image").is_none());
        assert!(decode_texture("wall.xyz", &png).is_none());
    }
}
