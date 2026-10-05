// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! EQ preset manager for saving, loading, and deleting custom presets.
//!
//! Custom presets are stored as individual JSON files in a presets directory
//! (typically `~/.config/lyra/eq_presets/`). Built-in presets are hardcoded
//! and cannot be saved or deleted.

use super::equalizer::{EqPreset, EqPresetData, PresetSource};
use std::path::PathBuf;

/// Manages EQ presets (built-in + custom on disk).
pub struct EqPresetManager {
    presets_dir: PathBuf,
}

impl EqPresetManager {
    /// Create a new preset manager. Creates the presets directory if it doesn't exist.
    pub fn new(presets_dir: PathBuf) -> std::io::Result<Self> {
        if !presets_dir.exists() {
            std::fs::create_dir_all(&presets_dir)?;
        }
        Ok(Self { presets_dir })
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

        let filename = sanitize_filename(&preset.name) + ".json";
        let path = self.presets_dir.join(filename);

        let json = serde_json::to_string_pretty(preset)
            .map_err(|e| format!("Failed to serialize preset: {}", e))?;

        std::fs::write(path, json).map_err(|e| format!("Failed to write preset file: {}", e))?;

        Ok(())
    }

    /// Delete a custom preset. Refuses to delete built-in presets.
    pub fn delete_preset(&self, name: &str) -> Result<(), String> {
        if self.is_builtin_name(name) {
            return Err(format!("Cannot delete built-in preset '{}'", name));
        }

        let filename = sanitize_filename(name) + ".json";
        let path = self.presets_dir.join(filename);

        if !path.exists() {
            return Err(format!("Preset file not found: '{}'", name));
        }

        std::fs::remove_file(path).map_err(|e| format!("Failed to delete preset file: {}", e))?;

        Ok(())
    }

    /// Check if a name matches any built-in preset (case-insensitive).
    pub fn is_builtin_name(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        EqPreset::ALL
            .iter()
            .any(|p| p.label().to_lowercase() == lower)
    }

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

/// Sanitize a string for use as a filename: keep only `[a-zA-Z0-9\-_ ]`.
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
        let dir = std::env::temp_dir().join("lyra_test_presets");
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
