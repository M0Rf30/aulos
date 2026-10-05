// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! AutoEQ integration module for loading headphone equalization profiles.
//!
//! This module provides functionality to fetch, parse, and apply AutoEQ profiles
//! from the AutoEQ GitHub repository (https://github.com/jaakkopasanen/AutoEq).
//!
//! AutoEQ provides scientifically measured frequency response corrections for
//! thousands of headphone models, allowing users to achieve neutral, accurate
//! sound reproduction.

mod manager;
mod parser;

pub use manager::AutoEQManager;
pub use parser::{parse_fixed_band_eq, parse_index};

use serde::{Deserialize, Serialize};

/// An AutoEQ profile containing equalizer settings for a specific headphone model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoEQProfile {
    /// Display name of the headphone (e.g., "Sennheiser HD 650")
    pub name: String,
    /// Repository path (e.g., "oratory1990/over-ear/Sennheiser HD 650")
    pub path: String,
    /// Measurement source (e.g., "oratory1990", "crinacle", "rtings")
    pub source: String,
    /// Headphone type (e.g., "over-ear", "in-ear", "earbuds")
    pub type_: String,
    /// Preamp gain in dB (often negative to prevent clipping)
    pub preamp: f32,
    /// 10-band equalizer gains in dB at frequencies:
    /// [31Hz, 62Hz, 125Hz, 250Hz, 500Hz, 1kHz, 2kHz, 4kHz, 8kHz, 16kHz]
    pub bands: [f32; 10],
}

/// Metadata for an AutoEQ profile (without EQ data).
///
/// Used for browsing profiles before fetching full data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoEQProfileMetadata {
    /// Display name of the headphone
    pub name: String,
    /// Repository path
    pub path: String,
    /// Measurement source
    pub source: String,
    /// Headphone type
    pub type_: String,
}

/// Errors that can occur during AutoEQ operations.
#[derive(Debug, thiserror::Error)]
pub enum AutoEQError {
    #[error("Profile not found: {0}")]
    ProfileNotFound(String),

    #[error("Invalid AutoEQ format: {0}")]
    InvalidFormat(String),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Operation timed out")]
    Timeout,
}

pub type Result<T> = std::result::Result<T, AutoEQError>;
