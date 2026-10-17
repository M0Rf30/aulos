// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! What a conversion job's source file is: codec, bit depth, sample rate,
//! channels, duration and size, probed once through symphonia when the job is
//! queued (blocking — run [`SourceInfo::probe`] off the UI thread). Shown on
//! the job row (`FLAC 24/96 → Opus 160`) and used by [`super::estimate`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::JobKind;
use super::cue;
use crate::library::tech_info;

/// Probed facts about a job's source audio.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceInfo {
    /// Codec label (`FLAC`, `MP3`, `PCM`, …).
    pub codec: String,
    /// Bits per sample; `None` for lossy codecs, which have no bit depth.
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub duration: Option<Duration>,
    /// Size of the audio file (for a CUE job, the referenced audio file).
    pub bytes: u64,
    pub lossless: bool,
    /// Tracks in the CUE sheet; `None` for a plain file.
    pub cue_tracks: Option<u32>,
}

/// The audio file a CUE sheet at `cue_path` refers to, and the sheet's text.
pub(crate) fn cue_audio_path(cue_path: &Path, cue_text: &str) -> Option<PathBuf> {
    let name = cue::parse_file_name(cue_text)?;
    // Only the final component (either separator style) is honoured, so a
    // sheet can't point outside its own directory.
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(
        cue_path
            .parent()
            .map_or_else(|| PathBuf::from(name), |dir| dir.join(name)),
    )
}

impl SourceInfo {
    /// Probes `source` (for [`JobKind::CueSplit`], the `.cue` sheet: its
    /// audio file is probed and the tracks counted). `None` when the file
    /// can't be read or isn't audio — the job will fail properly when run.
    #[must_use]
    pub fn probe(source: &Path, kind: JobKind) -> Option<Self> {
        let (audio, cue_tracks) = match kind {
            JobKind::Convert => (source.to_path_buf(), None),
            JobKind::CueSplit => {
                let text = std::fs::read_to_string(source).ok()?;
                let tracks = cue::parse(&text).ok()?.len();
                (
                    cue_audio_path(source, &text)?,
                    Some(u32::try_from(tracks).unwrap_or(u32::MAX)),
                )
            }
        };
        let info = tech_info::probe(&audio).ok()?;
        Some(Self {
            codec: codec_label(info.codec_short.as_deref()?),
            bit_depth: info.bits_per_sample,
            sample_rate: info.sample_rate,
            channels: info.channels,
            duration: info.duration,
            bytes: info.file_size.unwrap_or(0),
            lossless: info.lossless.unwrap_or(false),
            cue_tracks,
        })
    }

    /// `FLAC 24/96`, `MP3 44.1 kHz`, or just the codec when the rate is
    /// unknown.
    #[must_use]
    pub fn label(&self) -> String {
        match (self.bit_depth, self.sample_rate) {
            (Some(bits), Some(rate)) => format!("{} {bits}/{}", self.codec, khz(rate)),
            (None, Some(rate)) => format!("{} {} kHz", self.codec, khz(rate)),
            _ => self.codec.clone(),
        }
    }
}

/// Display name of a symphonia codec short name.
fn codec_label(short: &str) -> String {
    match short {
        "vorbis" => "Vorbis".to_owned(),
        "wavpack" => "WavPack".to_owned(),
        s if s.starts_with("pcm") => "PCM".to_owned(),
        s if s.starts_with("dsd") => "DSD".to_owned(),
        s => s.to_ascii_uppercase(),
    }
}

/// A sample rate in kHz without a trailing `.0` (`96`, `44.1`).
#[must_use]
pub fn khz(hz: u32) -> String {
    let value = f64::from(hz) / 1000.0;
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> SourceInfo {
        SourceInfo {
            codec: "FLAC".into(),
            bit_depth: Some(24),
            sample_rate: Some(96_000),
            channels: Some(2),
            duration: Some(Duration::from_secs(10)),
            bytes: 1000,
            lossless: true,
            cue_tracks: None,
        }
    }

    #[test]
    fn label_shows_depth_and_rate_for_lossless_sources() {
        assert_eq!(info().label(), "FLAC 24/96");
        let mut cd = info();
        cd.bit_depth = Some(16);
        cd.sample_rate = Some(44_100);
        assert_eq!(cd.label(), "FLAC 16/44.1");
    }

    #[test]
    fn label_of_a_lossy_source_has_no_bit_depth() {
        let mut mp3 = info();
        mp3.codec = "MP3".into();
        mp3.bit_depth = None;
        mp3.sample_rate = Some(44_100);
        assert_eq!(mp3.label(), "MP3 44.1 kHz");
        mp3.sample_rate = None;
        assert_eq!(mp3.label(), "MP3");
    }

    #[test]
    fn codec_names_are_normalized() {
        assert_eq!(codec_label("flac"), "FLAC");
        assert_eq!(codec_label("pcm_s16le"), "PCM");
        assert_eq!(codec_label("vorbis"), "Vorbis");
        assert_eq!(codec_label("dsd_lsbf"), "DSD");
    }

    #[test]
    fn khz_drops_a_zero_fraction() {
        assert_eq!(khz(48_000), "48");
        assert_eq!(khz(44_100), "44.1");
        assert_eq!(khz(22_050), "22.05");
    }

    #[test]
    fn probe_reads_a_wav_and_a_cue_sheet() {
        let dir = std::env::temp_dir().join(format!("aulos-source-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("album.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav, spec).unwrap();
        for _ in 0..44_100 * 2 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
        let cue_path = dir.join("album.cue");
        std::fs::write(
            &cue_path,
            "FILE \"album.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:00:50\n",
        )
        .unwrap();

        let plain = SourceInfo::probe(&wav, JobKind::Convert).expect("wav probes");
        assert_eq!(plain.codec, "PCM");
        assert_eq!(plain.bit_depth, Some(16));
        assert_eq!(plain.sample_rate, Some(44_100));
        assert_eq!(plain.channels, Some(2));
        assert!(plain.bytes > 0);
        assert_eq!(plain.cue_tracks, None);

        let sheet = SourceInfo::probe(&cue_path, JobKind::CueSplit).expect("cue probes");
        assert_eq!(sheet.cue_tracks, Some(2));
        assert_eq!(sheet.bytes, plain.bytes);

        assert!(SourceInfo::probe(&dir.join("missing.wav"), JobKind::Convert).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
