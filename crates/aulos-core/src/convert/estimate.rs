// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Rough output-size estimates for the converter's queue footer
//! (`11 files · ~1.2 GB → ~186 MB`).
//!
//! Lossy targets are sized from their bitrate × the source's duration (the
//! VBR presets use typical average bitrates). Lossless targets are sized from
//! the PCM they decode to — duration × rate × channels × sample width — times
//! a typical compression ratio for FLAC/ALAC; WAV/AIFF are exactly that PCM
//! size. These are estimates only: real files vary with the material.

use super::channels::ChannelMode;
use super::encoder::{FlacBitDepth, FlacOptions, LossyOptions, Mp3Mode, OutputFormat};
use super::source::SourceInfo;

/// Typical FLAC/ALAC size relative to the raw PCM.
const LOSSLESS_RATIO: f64 = 0.6;

/// Typical average bitrate (kbps) of `libmp3lame`'s `-q:a` VBR presets 0..=9.
const MP3_VBR_KBPS: [u32; 10] = [245, 225, 190, 175, 165, 130, 115, 100, 85, 65];

/// Typical average bitrate (kbps) of `libvorbis`'s `-q:a` -1..=10.
const VORBIS_KBPS: [u32; 12] = [45, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 500];

/// Every setting that decides the size of the output.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub format: OutputFormat,
    pub target_rate: Option<u32>,
    pub channels: ChannelMode,
    pub flac: FlacOptions,
    pub lossy: LossyOptions,
}

/// Total source size and estimated total output size of a set of files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueEstimate {
    pub source_bytes: u64,
    pub output_bytes: u64,
}

/// Average bitrate in kbps of a lossy `format`, `None` for lossless ones.
fn lossy_kbps(format: OutputFormat, lossy: LossyOptions) -> Option<u32> {
    match format {
        OutputFormat::Mp3 => Some(match lossy.mp3_mode {
            Mp3Mode::Cbr(kbps) => kbps,
            Mp3Mode::Vbr(q) => MP3_VBR_KBPS[(q as usize).min(MP3_VBR_KBPS.len() - 1)],
        }),
        OutputFormat::Aac => Some(lossy.aac_bitrate_kbps),
        OutputFormat::Opus => Some(lossy.opus_bitrate_kbps),
        OutputFormat::OggVorbis => {
            let index = (lossy.vorbis_quality.round() as i32 + 1)
                .clamp(0, VORBIS_KBPS.len() as i32 - 1) as usize;
            Some(VORBIS_KBPS[index])
        }
        _ => None,
    }
}

/// Estimated size in bytes of converting `source` to `target`, or `None`
/// when the source's duration (or, for lossless output, its format) isn't
/// known.
#[must_use]
pub fn output_bytes(source: &SourceInfo, target: Target) -> Option<u64> {
    let secs = source.duration?.as_secs_f64();
    if secs <= 0.0 {
        return None;
    }
    if let Some(kbps) = lossy_kbps(target.format, target.lossy) {
        return Some((secs * f64::from(kbps) * 1000.0 / 8.0) as u64);
    }

    let rate = target.target_rate.or(source.sample_rate)?;
    let src_channels = source.channels.and_then(|c| u16::try_from(c).ok())?;
    let channels = target.channels.output_channels(src_channels);
    let bits = match target.format {
        OutputFormat::Wav16 | OutputFormat::Aiff16 => 16,
        OutputFormat::Wav24 | OutputFormat::Aiff24 => 24,
        OutputFormat::Wav32Float => 32,
        // FLAC (auto depth follows the source, see the encoder) and ALAC.
        _ => match (target.format, target.flac.bit_depth) {
            (OutputFormat::Flac, FlacBitDepth::Bits16) => 16,
            (OutputFormat::Flac, FlacBitDepth::Bits24) => 24,
            _ if source.bit_depth.is_some_and(|b| b <= 16) => 16,
            _ => 24,
        },
    };
    let pcm = secs * f64::from(rate) * f64::from(channels) * f64::from(bits) / 8.0;
    let ratio = match target.format {
        OutputFormat::Flac | OutputFormat::Alac => LOSSLESS_RATIO,
        _ => 1.0,
    };
    Some((pcm * ratio) as u64)
}

/// Sums the sizes of `sources` (those that probed) and their estimated
/// output. `None` when no source could be estimated.
#[must_use]
pub fn queue_estimate<'a>(
    sources: impl IntoIterator<Item = &'a SourceInfo>,
    target: Target,
) -> Option<QueueEstimate> {
    let mut total: Option<QueueEstimate> = None;
    for source in sources {
        let Some(out) = output_bytes(source, target) else {
            continue;
        };
        let t = total.get_or_insert(QueueEstimate {
            source_bytes: 0,
            output_bytes: 0,
        });
        t.source_bytes += source.bytes;
        t.output_bytes += out;
    }
    total
}

/// `3.6 MB`, `186 MB`, `1.2 GB`: decimal units, one decimal below 100.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A remaining time as a clock: `00:41`, `12:05`, `1:02:03`.
#[must_use]
pub fn format_clock(duration: std::time::Duration) -> String {
    let secs = duration.as_secs() + u64::from(duration.subsec_millis() >= 500);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn clock_pads_minutes_and_adds_hours_when_needed() {
        assert_eq!(format_clock(Duration::from_secs(41)), "00:41");
        assert_eq!(format_clock(Duration::from_secs(725)), "12:05");
        assert_eq!(format_clock(Duration::from_secs(3723)), "1:02:03");
        assert_eq!(format_clock(Duration::from_millis(40_600)), "00:41");
        assert_eq!(format_clock(Duration::ZERO), "00:00");
    }

    fn source(secs: u64) -> SourceInfo {
        SourceInfo {
            codec: "FLAC".into(),
            bit_depth: Some(24),
            sample_rate: Some(96_000),
            channels: Some(2),
            duration: Some(Duration::from_secs(secs)),
            bytes: 50_000_000,
            lossless: true,
            cue_tracks: None,
        }
    }

    fn target(format: OutputFormat) -> Target {
        Target {
            format,
            target_rate: None,
            channels: ChannelMode::Source,
            flac: FlacOptions::default(),
            lossy: LossyOptions::default(),
        }
    }

    #[test]
    fn lossy_size_is_bitrate_times_duration() {
        // Default Opus: 160 kbps -> 20 kB/s.
        assert_eq!(
            output_bytes(&source(100), target(OutputFormat::Opus)),
            Some(2_000_000)
        );
        let mut mp3 = target(OutputFormat::Mp3);
        mp3.lossy.mp3_mode = Mp3Mode::Cbr(320);
        assert_eq!(output_bytes(&source(100), mp3), Some(4_000_000));
    }

    #[test]
    fn vbr_presets_use_typical_bitrates() {
        let mut mp3 = target(OutputFormat::Mp3);
        mp3.lossy.mp3_mode = Mp3Mode::Vbr(0);
        assert_eq!(output_bytes(&source(8), mp3), Some(245_000));
        let mut vorbis = target(OutputFormat::OggVorbis);
        vorbis.lossy.vorbis_quality = -1.0;
        assert_eq!(output_bytes(&source(8), vorbis), Some(45_000));
        vorbis.lossy.vorbis_quality = 10.0;
        assert_eq!(output_bytes(&source(8), vorbis), Some(500_000));
    }

    #[test]
    fn wav_is_the_exact_pcm_size() {
        // 10 s of 96 kHz stereo 24-bit.
        assert_eq!(
            output_bytes(&source(10), target(OutputFormat::Wav24)),
            Some(10 * 96_000 * 2 * 3)
        );
        let mut mono_44 = target(OutputFormat::Wav16);
        mono_44.target_rate = Some(44_100);
        mono_44.channels = ChannelMode::Mono;
        assert_eq!(output_bytes(&source(10), mono_44), Some(10 * 44_100 * 2));
    }

    #[test]
    fn flac_applies_the_lossless_ratio_and_follows_the_source_depth() {
        let pcm24 = (10 * 96_000 * 2 * 3) as f64;
        assert_eq!(
            output_bytes(&source(10), target(OutputFormat::Flac)),
            Some((pcm24 * LOSSLESS_RATIO) as u64)
        );
        let mut cd = source(10);
        cd.bit_depth = Some(16);
        let pcm16 = (10 * 96_000 * 2 * 2) as f64;
        assert_eq!(
            output_bytes(&cd, target(OutputFormat::Flac)),
            Some((pcm16 * LOSSLESS_RATIO) as u64)
        );
    }

    #[test]
    fn unknown_duration_cannot_be_estimated() {
        let mut s = source(10);
        s.duration = None;
        assert_eq!(output_bytes(&s, target(OutputFormat::Opus)), None);
    }

    #[test]
    fn queue_estimate_sums_the_estimable_sources() {
        let a = source(100);
        let b = source(50);
        let mut unknown = source(0);
        unknown.duration = None;
        let est = queue_estimate([&a, &b, &unknown], target(OutputFormat::Opus)).unwrap();
        assert_eq!(est.source_bytes, 100_000_000);
        assert_eq!(est.output_bytes, 3_000_000);
        assert_eq!(queue_estimate([], target(OutputFormat::Opus)), None);
        assert_eq!(queue_estimate([&unknown], target(OutputFormat::Opus)), None);
    }

    #[test]
    fn bytes_are_formatted_with_decimal_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(3_600_000), "3.6 MB");
        assert_eq!(format_bytes(186_000_000), "186 MB");
        assert_eq!(format_bytes(1_200_000_000), "1.2 GB");
        assert_eq!(format_bytes(64_100), "64.1 kB");
    }
}
