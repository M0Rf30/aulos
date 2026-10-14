// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! What the converter offers (output sample rates, per-format quality
//! presets) and the localized words for those choices and for job
//! state, so every UI presents the same menu with the same labels.

use super::encoder::{FlacBitDepth, Mp3Mode};
use super::{JobKind, JobState, OutputFormat};
use crate::fl;

/// Sample-rate dropdown options: `None` keeps the source rate.
pub const SAMPLE_RATE_OPTIONS: &[Option<u32>] = &[
    None,
    Some(22_050),
    Some(32_000),
    Some(44_100),
    Some(48_000),
    Some(88_200),
    Some(96_000),
    Some(176_400),
    Some(192_000),
];

/// FLAC compression-level dropdown options (0 fastest/largest .. 8
/// slowest/smallest — see `encoder::apply_compression_level`).
pub const FLAC_COMPRESSION_OPTIONS: [u8; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];

/// FLAC bit-depth dropdown options.
pub const FLAC_BIT_DEPTH_OPTIONS: [FlacBitDepth; 3] = [
    FlacBitDepth::Auto,
    FlacBitDepth::Bits16,
    FlacBitDepth::Bits24,
];

/// MP3 mode/quality dropdown options: a handful of common VBR presets plus
/// common CBR bitrates, rather than exposing the raw 0-9/kbps numbers.
pub const MP3_MODE_OPTIONS: [Mp3Mode; 8] = [
    Mp3Mode::Vbr(0),
    Mp3Mode::Vbr(2),
    Mp3Mode::Vbr(4),
    Mp3Mode::Vbr(6),
    Mp3Mode::Cbr(128),
    Mp3Mode::Cbr(192),
    Mp3Mode::Cbr(256),
    Mp3Mode::Cbr(320),
];

/// Bitrate dropdown options shared by AAC and Opus.
pub const BITRATE_KBPS_OPTIONS: [u32; 6] = [96, 128, 160, 192, 256, 320];

/// Ogg Vorbis `-q:a` quality dropdown options (-1.0 lowest .. 10.0 highest).
pub const VORBIS_QUALITY_OPTIONS: [f32; 7] = [-1.0, 0.0, 2.0, 4.0, 6.0, 8.0, 10.0];

/// Localized label for a format dropdown entry. The five `ffmpeg`-backed
/// formats always get a "(needs ffmpeg)" suffix, regardless of whether
/// `ffmpeg` is currently available — the UI shows a separate
/// warning caption for the unavailable case.
pub fn format_label(format: OutputFormat) -> String {
    let base = format_short_label(format);
    if format.requires_ffmpeg() {
        fl!("convert-format-needs-ffmpeg", format = base)
    } else {
        base
    }
}

/// Localized format name without the "(needs ffmpeg)" suffix — used on the
/// job rows' target-format chips, where the suffix would just be noise.
pub fn format_short_label(format: OutputFormat) -> String {
    match format {
        OutputFormat::Flac => fl!("convert-format-flac"),
        OutputFormat::Wav16 => fl!("convert-format-wav16"),
        OutputFormat::Wav24 => fl!("convert-format-wav24"),
        OutputFormat::Wav32Float => fl!("convert-format-wav32float"),
        OutputFormat::Aiff16 => fl!("convert-format-aiff16"),
        OutputFormat::Aiff24 => fl!("convert-format-aiff24"),
        OutputFormat::Mp3 => fl!("convert-format-mp3"),
        OutputFormat::Aac => fl!("convert-format-aac"),
        OutputFormat::Opus => fl!("convert-format-opus"),
        OutputFormat::OggVorbis => fl!("convert-format-vorbis"),
        OutputFormat::Alac => fl!("convert-format-alac"),
    }
}

/// Localized label for a sample-rate dropdown entry.
pub fn rate_label(rate: Option<u32>) -> String {
    match rate {
        None => fl!("convert-rate-source"),
        Some(hz) => fl!("convert-rate-hz", hz = hz),
    }
}

/// Localized label for a FLAC bit-depth dropdown entry.
pub fn flac_bit_depth_label(depth: FlacBitDepth) -> String {
    match depth {
        FlacBitDepth::Auto => fl!("convert-flac-bitdepth-auto"),
        FlacBitDepth::Bits16 => fl!("convert-flac-bitdepth-16"),
        FlacBitDepth::Bits24 => fl!("convert-flac-bitdepth-24"),
    }
}

/// Localized label for an MP3 mode/quality dropdown entry.
pub fn mp3_mode_label(mode: Mp3Mode) -> String {
    match mode {
        Mp3Mode::Vbr(q) => fl!("convert-mp3-vbr", q = q),
        Mp3Mode::Cbr(kbps) => fl!("convert-mp3-cbr", kbps = kbps),
    }
}

/// Localized label for a kbps bitrate dropdown entry (AAC/Opus). A named
/// function rather than an inline `fl!()` call inside `.map()`: nested
/// directly in a `.map(...).collect::<Vec<_>>()` closure, `fl!`'s
/// internal `FluentValue` conversion can't infer `*hz`'s type even though
/// it's already the concrete `u32` `BITRATE_KBPS_OPTIONS` always yields.
pub fn bitrate_kbps_label(kbps: u32) -> String {
    fl!("convert-rate-hz-kbps", kbps = kbps)
}

/// Localized label for a job's kind.
pub fn kind_label(kind: JobKind) -> String {
    match kind {
        JobKind::Convert => fl!("convert-kind-convert"),
        JobKind::CueSplit => fl!("convert-kind-cuesplit"),
    }
}

/// Localized label for a job's lifecycle state.
pub fn state_label(state: &JobState) -> String {
    match state {
        JobState::Queued => fl!("convert-state-queued"),
        JobState::Running => fl!("convert-state-running"),
        JobState::Done => fl!("convert-state-done"),
        JobState::Failed(_) => fl!("convert-state-failed-chip"),
        JobState::Cancelled => fl!("convert-state-cancelled"),
    }
}
