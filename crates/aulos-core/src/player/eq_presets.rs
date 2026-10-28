// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! EQ preset manager for saving, loading, and deleting custom presets.
//!
//! Custom presets are stored as individual JSON files in a presets directory
//! (typically `~/.config/aulos/eq_presets/`); in the browser, as one JSON list
//! in `localStorage`. Built-in presets are hardcoded and cannot be saved or
//! deleted.

#[cfg(not(target_arch = "wasm32"))]
use super::equalizer::PresetSource;
use super::equalizer::{EqPreset, EqPresetData};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

/// `localStorage` key of the custom presets.
#[cfg(target_arch = "wasm32")]
const STORAGE_KEY: &str = "aulos.eq_presets";

/// Manages EQ presets (built-in + custom, stored per platform).
pub struct EqPresetManager {
    #[cfg(not(target_arch = "wasm32"))]
    presets_dir: PathBuf,
}

impl EqPresetManager {
    /// Create a new preset manager. Creates the presets directory if it doesn't exist.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(presets_dir: PathBuf) -> std::io::Result<Self> {
        if !presets_dir.exists() {
            std::fs::create_dir_all(&presets_dir)?;
        }
        Ok(Self::at(presets_dir))
    }

    /// A manager over `presets_dir` that does not create it. Listing then
    /// yields only the built-in presets and saving reports an error until
    /// the directory exists; used when [`Self::new`] cannot create it.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn at(presets_dir: PathBuf) -> Self {
        Self { presets_dir }
    }

    /// A manager over the presets kept in the browser's `localStorage`.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn in_browser() -> Self {
        Self {}
    }

    /// Load all presets: built-in first (sorted), then custom (sorted).
    /// Custom presets have ` *` suffix stripped from display — the suffix is
    /// added by the UI layer, not stored on disk.
    pub fn load_all(&self) -> Vec<EqPresetData> {
        let mut presets = Vec::new();

        // Built-in presets
        for p in &EqPreset::ALL {
            presets.push(p.to_preset_data());
        }

        // Custom presets from disk
        let mut custom = self.load_custom_presets();
        custom.sort_by_key(|a| a.name.to_lowercase());
        presets.extend(custom);

        presets
    }

    /// Save a preset to disk. Refuses to overwrite built-in presets.
    pub fn save_preset(&self, preset: &EqPresetData) -> Result<(), String> {
        if self.is_builtin_name(&preset.name) {
            return Err(format!(
                "Cannot save over built-in preset '{}'",
                preset.name
            ));
        }

        self.write_custom(preset)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn write_custom(&self, preset: &EqPresetData) -> Result<(), String> {
        let filename = sanitize_filename(&preset.name) + ".json";
        let path = self.presets_dir.join(filename);

        let json = serde_json::to_string_pretty(preset)
            .map_err(|e| format!("Failed to serialize preset: {}", e))?;

        std::fs::write(path, json).map_err(|e| format!("Failed to write preset file: {}", e))?;

        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    fn write_custom(&self, preset: &EqPresetData) -> Result<(), String> {
        let mut presets = self.load_custom_presets();
        presets.retain(|p| p.name != preset.name);
        let mut preset = preset.clone();
        preset.source = super::equalizer::PresetSource::Custom;
        presets.push(preset);
        store_presets(&presets)
    }

    /// Delete a custom preset. Refuses to delete built-in presets.
    pub fn delete_preset(&self, name: &str) -> Result<(), String> {
        if self.is_builtin_name(name) {
            return Err(format!("Cannot delete built-in preset '{}'", name));
        }

        self.remove_custom(name)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn remove_custom(&self, name: &str) -> Result<(), String> {
        let filename = sanitize_filename(name) + ".json";
        let path = self.presets_dir.join(filename);

        if !path.exists() {
            return Err(format!("Preset file not found: '{}'", name));
        }

        std::fs::remove_file(path).map_err(|e| format!("Failed to delete preset file: {}", e))?;

        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    fn remove_custom(&self, name: &str) -> Result<(), String> {
        let mut presets = self.load_custom_presets();
        let before = presets.len();
        presets.retain(|p| p.name != name);
        if presets.len() == before {
            return Err(format!("Preset not found: '{}'", name));
        }
        store_presets(&presets)
    }

    /// Check if a name matches any built-in preset (case-insensitive).
    pub fn is_builtin_name(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        EqPreset::ALL
            .iter()
            .any(|p| p.label().to_lowercase() == lower)
    }

    #[cfg(target_arch = "wasm32")]
    fn load_custom_presets(&self) -> Vec<EqPresetData> {
        let Some(json) = crate::web_storage::get(STORAGE_KEY) else {
            return Vec::new();
        };
        serde_json::from_str(&json).unwrap_or_else(|e| {
            tracing::warn!("Failed to parse the stored EQ presets: {e}");
            Vec::new()
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_custom_presets(&self) -> Vec<EqPresetData> {
        let mut presets = Vec::new();

        let entries = match std::fs::read_dir(&self.presets_dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!("Failed to read presets directory: {}", e);
                return presets;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<EqPresetData>(&content) {
                    Ok(mut preset) => {
                        // Ensure custom presets are marked as Custom source
                        if preset.source == PresetSource::Builtin {
                            preset.source = PresetSource::Custom;
                        }
                        presets.push(preset);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse preset {:?}: {}", path, e);
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to read preset {:?}: {}", path, e);
                }
            }
        }

        presets
    }
}

#[cfg(target_arch = "wasm32")]
fn store_presets(presets: &[EqPresetData]) -> Result<(), String> {
    let json = serde_json::to_string(presets)
        .map_err(|e| format!("Failed to serialize presets: {}", e))?;
    crate::web_storage::set(STORAGE_KEY, &json)
}

/// Sanitize a string for use as a filename: keep only `[a-zA-Z0-9\-_ ]`.
#[cfg(not(target_arch = "wasm32"))]
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("Rock"), "Rock");
        assert_eq!(sanitize_filename("My Preset 1"), "My Preset 1");
        assert_eq!(
            sanitize_filename("HD 650 (oratory1990)"),
            "HD 650 oratory1990"
        );
        assert_eq!(sanitize_filename("Sennheiser/HD 650"), "SennheiserHD 650");
    }

    #[test]
    fn test_is_builtin_name() {
        let dir = std::env::temp_dir().join("aulos_test_presets");
        let _ = std::fs::create_dir_all(&dir);
        let manager = EqPresetManager::new(dir).unwrap();

        assert!(manager.is_builtin_name("Flat"));
        assert!(manager.is_builtin_name("flat"));
        assert!(manager.is_builtin_name("Rock"));
        assert!(manager.is_builtin_name("BASS BOOST"));
        assert!(!manager.is_builtin_name("My Custom"));
        assert!(!manager.is_builtin_name("HD 650"));
    }
}
