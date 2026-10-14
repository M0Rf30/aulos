// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::Session;
use crate::autoeq::{AutoEQManager, AutoEQProfile, AutoEQProfileMetadata};
use crate::config::Config;
use crate::player::eq_presets::EqPresetManager;
use crate::player::equalizer::{EqPreset, EqPresetData, PresetSource};
use crate::task::Effect;
use std::time::Duration;

/// How long an AutoEQ index/profile download may take.
const AUTOEQ_TIMEOUT: Duration = Duration::from_secs(30);

/// Async results of the AutoEQ workflows; feed them to
/// [`Session::apply_equalizer_event`].
#[derive(Debug, Clone)]
pub enum EqualizerEvent {
    /// The AutoEQ profile index finished downloading.
    IndexLoaded(Result<Vec<AutoEQProfileMetadata>, String>),
    /// An AutoEQ profile finished downloading.
    ProfileLoaded(Result<AutoEQProfile, String>),
}

/// Equalizer presets and their editing/dirty bookkeeping.
pub struct EqualizerState {
    pub preset: Option<EqPreset>,
    pub preset_manager: EqPresetManager,
    pub all_presets: Vec<EqPresetData>,
    pub active_preset_name: Option<String>,
    pub dirty: bool,
    /// AutoEQ profiles loaded from GitHub, available in the preset dropdown.
    pub autoeq_profiles: Vec<AutoEQProfileMetadata>,
    pub autoeq_loading: bool,
}

impl EqualizerState {
    /// Loads every stored preset and restores the active preset name from
    /// the config.
    ///
    /// # Panics
    /// If the presets directory cannot be created.
    pub(super) fn new(config: &Config) -> Self {
        let presets_dir = crate::paths::config_dir().join("eq_presets");
        let preset_manager =
            EqPresetManager::new(presets_dir).expect("Failed to create EQ presets directory");
        let all_presets = preset_manager.load_all();
        let active_preset_name = (!config.active_eq_preset_name.is_empty())
            .then(|| config.active_eq_preset_name.clone());
        Self {
            preset: None,
            preset_manager,
            all_presets,
            active_preset_name,
            dirty: false,
            autoeq_profiles: Vec::new(),
            autoeq_loading: false,
        }
    }
}

/// Equalizer workflows: they edit the persisted band/preamp values in
/// [`Config`], mirror them into the live DSP of the player and keep the
/// preset bookkeeping of [`EqualizerState`]. Persisting the config is up to
/// the caller.
impl Session {
    /// Sets one band's gain (clamped to ±12 dB).
    pub fn eq_set_band(&mut self, index: usize, gain_db: f32) {
        let clamped = gain_db.clamp(-12.0, 12.0);
        if index < self.config.config.equalizer_bands.len() {
            self.config.config.equalizer_bands[index] = clamped;
        }
        // Update the live DSP filter.
        if let Some(player) = &self.playback.player {
            player.eq_controller().set_band(index, clamped);
        }
        self.equalizer.preset = None;
        self.equalizer.dirty = true;
    }

    /// Applies a built-in shape to all bands.
    pub fn eq_set_preset(&mut self, preset: EqPreset) {
        let gains = preset.gains();
        self.config.config.equalizer_bands = gains.to_vec();
        // Update all 10 bands in the live DSP.
        if let Some(player) = &self.playback.player {
            player.eq_controller().set_all(&gains);
        }
        self.equalizer.preset = Some(preset);
    }

    /// Enables or bypasses the equalizer.
    pub fn eq_set_enabled(&mut self, enabled: bool) {
        self.config.config.equalizer_enabled = enabled;
        if let Some(player) = &self.playback.player {
            player.eq_controller().set_enabled(enabled);
        }
    }

    /// Sets the preamp (clamped to -20..+10 dB).
    pub fn eq_set_preamp(&mut self, preamp_db: f32) {
        let clamped = preamp_db.clamp(-20.0, 10.0);
        self.config.config.equalizer_preamp = clamped;
        // TODO: Apply preamp to audio backend if supported
        tracing::debug!("Preamp set to {:+.1} dB", clamped);
        self.equalizer.preset = None;
        self.equalizer.dirty = true;
    }

    /// Loads the preset named `name` from the combined preset list; a no-op
    /// for an unknown name.
    pub fn eq_select_preset(&mut self, name: String) {
        let Some(preset) = self.equalizer.all_presets.iter().find(|p| p.name == name) else {
            return;
        };
        self.config.config.equalizer_bands = preset.bands.to_vec();
        self.config.config.equalizer_preamp = preset.preamp;
        if let Some(player) = &self.playback.player {
            player.eq_controller().set_all(&preset.bands);
        }
        self.equalizer.active_preset_name = Some(name.clone());
        self.config.config.active_eq_preset_name = name;
        self.equalizer.dirty = false;
        self.equalizer.preset = None; // clear legacy preset tracking
    }

    /// The current band/preamp values as a custom preset called `name`.
    fn eq_snapshot(&self, name: String) -> EqPresetData {
        let mut bands = [0.0_f32; 10];
        for (slot, v) in bands.iter_mut().zip(&self.config.config.equalizer_bands) {
            *slot = *v;
        }
        EqPresetData {
            name,
            bands,
            preamp: self.config.config.equalizer_preamp,
            source: PresetSource::Custom,
        }
    }

    /// Overwrites the active custom preset with the current values.
    pub fn eq_save_preset(&mut self) {
        let Some(name) = self.equalizer.active_preset_name.clone() else {
            return;
        };
        if self.equalizer.preset_manager.is_builtin_name(&name) {
            tracing::warn!("Cannot overwrite built-in preset '{}'", name);
            return;
        }
        let preset = self.eq_snapshot(name);
        if let Err(e) = self.equalizer.preset_manager.save_preset(&preset) {
            tracing::error!("Failed to save preset: {}", e);
        } else {
            self.equalizer.all_presets = self.equalizer.preset_manager.load_all();
            self.equalizer.dirty = false;
        }
    }

    /// Saves the current values as a new custom preset called `name` and
    /// makes it the active one.
    pub fn eq_save_preset_as(&mut self, name: String) {
        if name.trim().is_empty() {
            tracing::warn!("Cannot save preset with empty name");
        } else if self.equalizer.preset_manager.is_builtin_name(&name) {
            tracing::warn!("Cannot use the name of a built-in preset: '{}'", name);
        } else {
            let preset = self.eq_snapshot(name.clone());
            if let Err(e) = self.equalizer.preset_manager.save_preset(&preset) {
                tracing::error!("Failed to save preset: {}", e);
            } else {
                self.equalizer.all_presets = self.equalizer.preset_manager.load_all();
                self.equalizer.active_preset_name = Some(name.clone());
                self.config.config.active_eq_preset_name = name;
                self.equalizer.dirty = false;
            }
        }
    }

    /// Deletes the active custom preset.
    pub fn eq_delete_preset(&mut self) {
        let Some(name) = self.equalizer.active_preset_name.clone() else {
            return;
        };
        if self.equalizer.preset_manager.is_builtin_name(&name) {
            tracing::warn!("Cannot delete built-in preset '{}'", name);
        } else if let Err(e) = self.equalizer.preset_manager.delete_preset(&name) {
            tracing::error!("Failed to delete preset: {}", e);
        } else {
            self.equalizer.all_presets = self.equalizer.preset_manager.load_all();
            self.equalizer.active_preset_name = None;
            self.config.config.active_eq_preset_name = String::new();
            self.equalizer.dirty = false;
        }
    }

    /// Resets to Flat: all bands 0, preamp 0, no preset selected.
    pub fn eq_reset(&mut self) {
        self.eq_apply_bands(&[0.0; 10], 0.0);
    }

    /// Sets bands + preamp from an unnamed source (Flat, an AutoEQ profile):
    /// clears the preset selection (the user can "Save As" to keep it).
    fn eq_apply_bands(&mut self, bands: &[f32; 10], preamp: f32) {
        self.config.config.equalizer_bands = bands.to_vec();
        self.config.config.equalizer_preamp = preamp;
        if let Some(player) = &self.playback.player {
            player.eq_controller().set_all(bands);
        }
        self.equalizer.active_preset_name = None;
        self.config.config.active_eq_preset_name = String::new();
        self.equalizer.preset = None;
        self.equalizer.dirty = false;
    }

    /// Downloads the AutoEQ profile index; a no-op while one is already
    /// being fetched.
    pub fn fetch_autoeq_index(&mut self) -> Effect<EqualizerEvent> {
        if self.equalizer.autoeq_loading {
            return Effect::none();
        }
        self.equalizer.autoeq_loading = true;
        Effect::future(async move {
            let result = match autoeq_manager() {
                Ok(mut manager) => manager.fetch_index().await.map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            EqualizerEvent::IndexLoaded(result)
        })
    }

    /// Downloads the AutoEQ profile at repository path `path`.
    pub fn fetch_autoeq_profile(&self, path: String) -> Effect<EqualizerEvent> {
        let name = self
            .equalizer
            .autoeq_profiles
            .iter()
            .find(|p| p.path == path)
            .map(|p| p.name.as_str())
            .unwrap_or_default();
        tracing::info!("Fetching AutoEQ profile: {} ({})", name, path);
        Effect::future(async move {
            let result = match autoeq_manager() {
                Ok(mut manager) => manager
                    .fetch_profile(&path)
                    .await
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            EqualizerEvent::ProfileLoaded(result)
        })
    }

    /// Applies an async AutoEQ result.
    pub fn apply_equalizer_event(&mut self, event: EqualizerEvent) {
        match event {
            EqualizerEvent::IndexLoaded(result) => {
                self.equalizer.autoeq_loading = false;
                match result {
                    Ok(profiles) => {
                        tracing::info!("Loaded {} AutoEQ profiles", profiles.len());
                        self.equalizer.autoeq_profiles = profiles;
                    }
                    Err(e) => tracing::error!("Failed to fetch AutoEQ index: {}", e),
                }
            }
            EqualizerEvent::ProfileLoaded(result) => match result {
                Ok(profile) => {
                    self.eq_apply_bands(&profile.bands, profile.preamp);
                    tracing::info!(
                        "Applied AutoEQ profile: {} (preamp: {:+.1} dB)",
                        profile.name,
                        profile.preamp
                    );
                }
                Err(e) => tracing::error!("Failed to load AutoEQ profile: {}", e),
            },
        }
    }
}

fn autoeq_manager() -> Result<AutoEQManager, String> {
    AutoEQManager::new(crate::paths::cache_dir().join("autoeq"), AUTOEQ_TIMEOUT)
        .map_err(|e| e.to_string())
}
