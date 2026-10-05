// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! 10-band equalizer definitions and presets.

use serde::{Deserialize, Serialize};

/// Standard 10-band equalizer center frequencies in Hz.
pub const BAND_FREQUENCIES: [f32; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Labels for UI display.
pub const BAND_LABELS: [&str; 10] = [
    "31", "62", "125", "250", "500", "1K", "2K", "4K", "8K", "16K",
];

/// Named EQ presets (legacy enum for built-in presets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqPreset {
    Flat,
    Rock,
    Pop,
    Jazz,
    Classical,
    Bass,
    Treble,
    Vocal,
}

impl EqPreset {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Flat => "Flat",
            Self::Rock => "Rock",
            Self::Pop => "Pop",
            Self::Jazz => "Jazz",
            Self::Classical => "Classical",
            Self::Bass => "Bass Boost",
            Self::Treble => "Treble Boost",
            Self::Vocal => "Vocal",
        }
    }

    pub fn gains(&self) -> [f32; 10] {
        match self {
            Self::Flat => [0.0; 10],
            Self::Rock => [5.0, 4.0, 2.0, 0.0, -1.0, 1.0, 3.0, 4.0, 5.0, 5.0],
            Self::Pop => [1.0, 2.0, 3.0, 3.0, 2.0, 0.0, -1.0, 0.0, 1.0, 2.0],
            Self::Jazz => [3.0, 2.0, 1.0, 2.0, -1.0, -1.0, 0.0, 1.0, 2.0, 3.0],
            Self::Classical => [4.0, 3.0, 2.0, 1.0, -1.0, -1.0, 0.0, 2.0, 3.0, 4.0],
            Self::Bass => [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            Self::Treble => [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 4.0, 5.0, 6.0],
            Self::Vocal => [-2.0, -1.0, 0.0, 2.0, 4.0, 4.0, 3.0, 1.0, 0.0, -1.0],
        }
    }

    pub const ALL: [EqPreset; 8] = [
        Self::Flat,
        Self::Rock,
        Self::Pop,
        Self::Jazz,
        Self::Classical,
        Self::Bass,
        Self::Treble,
        Self::Vocal,
    ];

    /// Convert to EqPresetData for serialization and extended features.
    pub fn to_preset_data(&self) -> EqPresetData {
        EqPresetData {
            name: self.label().to_string(),
            bands: self.gains(),
            preamp: 0.0, // Built-in presets have no preamp
            source: PresetSource::Builtin,
        }
    }
}

/// Preset data structure supporting preamp and source tracking.
///
/// # Preamp Behavior
///
/// The `preamp` field specifies a global gain adjustment in dB that should be applied
/// to all audio before the EQ bands. This is particularly important for AutoEQ profiles,
/// which often include negative preamp values to prevent clipping when the EQ boosts
/// certain frequencies.
///
/// **Application order:**
/// 1. Apply preamp gain to the input signal
/// 2. Apply EQ band filters
///
/// **Example:** If preamp is -6.5 dB and a band has +5.0 dB gain, the net effect at
/// that frequency is approximately -1.5 dB relative to the original signal.
///
/// **Audio backend integration:**
/// The preamp should be implemented as a volume adjustment before the EQ stage.
/// If the audio backend doesn't support separate preamp, it can be baked into the
/// EQ band gains, but this is less accurate for AutoEQ profiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EqPresetData {
    pub name: String,
    pub bands: [f32; 10],
    /// Preamp gain in dB (default 0.0). Negative values prevent clipping.
    #[serde(default)]
    pub preamp: f32,
    #[serde(default = "default_preset_source")]
    pub source: PresetSource,
}

/// Source type for EQ presets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PresetSource {
    Builtin,
    Custom,
    AutoEQ { headphone: String },
}

fn default_preset_source() -> PresetSource {
    PresetSource::Builtin
}
