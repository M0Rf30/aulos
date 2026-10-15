// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Technical properties of a track (codec, bitrate, sample rate, tag blocks…)
//! for the Properties drawer, and the single payload-based bitrate computation
//! shared with the library scanner.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag, Tag as SymTag};

use super::{Track, tags};
use crate::player::engine::decoder::local_file_format_options;

mod layout;

/// How the audio bitrate behaves over the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitrateMode {
    Cbr,
    Vbr,
    Abr,
    Lossless,
}

/// ReplayGain values exactly as stored in the tags (e.g. `"-6.50 dB"`,
/// `"0.988553"`), so the drawer shows what is in the file rather than a
/// re-formatted float.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayGain {
    pub track_gain: Option<String>,
    pub track_peak: Option<String>,
    pub album_gain: Option<String>,
    pub album_peak: Option<String>,
}

impl ReplayGain {
    pub fn is_empty(&self) -> bool {
        self.track_gain.is_none()
            && self.track_peak.is_none()
            && self.album_gain.is_none()
            && self.album_peak.is_none()
    }
}

/// Everything the Properties drawer can show about one track. Every field is
/// optional: remote sources (Subsonic, MPD, radio, podcasts) only know a
/// subset, and the drawer renders a missing value as an em dash — never `0`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TechInfo {
    /// Container / file format (`"FLAC"`, `"MPEG-4 (M4A)"`, `"Ogg"`, `"MP3"`…).
    pub container: Option<String>,
    /// Codec long name (`"Free Lossless Audio Codec (FLAC)"`).
    pub codec: Option<String>,
    /// Codec short name (`"flac"`, `"mp3"`, `"aac"`).
    pub codec_short: Option<String>,
    /// `"LC"`, `"HE-AAC"`, `"MPEG-1 Layer III"`…
    pub profile: Option<String>,
    pub sample_rate: Option<u32>,
    /// `None` for lossy codecs (the drawer shows "n/a" when `lossless == Some(false)`).
    pub bits_per_sample: Option<u32>,
    pub lossless: Option<bool>,
    pub channels: Option<u32>,
    /// `"Stereo"`, `"5.1"`, …
    pub channel_layout: Option<String>,
    pub duration: Option<Duration>,
    /// Size of the whole file in bytes.
    pub file_size: Option<u64>,
    /// File size minus tag/metadata bytes — the bytes the bitrate is computed from.
    pub payload_bytes: Option<u64>,
    /// Average bitrate in kbit/s (`payload_bytes` over `duration`).
    pub bitrate_kbps: Option<u32>,
    pub bitrate_mode: Option<BitrateMode>,
    /// Nominal bitrate from the first MP3 frame header, or the value a remote
    /// server reports.
    pub nominal_bitrate_kbps: Option<u32>,
    /// Encoder / vendor string (`"LAME3.100"`, Vorbis vendor, `©too`, `TSSE`).
    pub encoder: Option<String>,
    /// Tag blocks present (`"ID3v2.4"`, `"ID3v1"`, `"APEv2"`, `"Vorbis comment"`, `"iTunes (ilst)"`, …).
    pub tag_formats: Vec<String>,
    pub replay_gain: ReplayGain,
    /// FLAC STREAMINFO audio MD5, lowercase hex, when it is non-zero.
    pub md5: Option<String>,
    /// Release/recording date string exactly as tagged.
    pub date: Option<String>,
    pub original_date: Option<String>,
    /// MIME type / content type (remote servers).
    pub mime_type: Option<String>,
    /// Server-side path (Subsonic `path`, MPD relative path).
    pub remote_path: Option<String>,
    /// Composer tag (`Track` carries no composer field).
    pub composer: Option<String>,
}

/// Aggregate over the tracks of one album, for the Properties drawer's album mode.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AlbumSummary {
    pub track_count: usize,
    pub total_duration: Duration,
    /// Sum of file sizes of the tracks whose size is known.
    pub total_size: Option<u64>,
    /// Distinct container/codec labels, sorted.
    pub formats: Vec<String>,
    /// Distinct sample rates in Hz, ascending.
    pub sample_rates: Vec<u32>,
    /// Distinct bit depths, ascending (lossy tracks contribute none).
    pub bit_depths: Vec<u32>,
    pub bitrate_min_kbps: Option<u32>,
    pub bitrate_max_kbps: Option<u32>,
    /// Size-weighted average bitrate over the album's tracks.
    pub bitrate_avg_kbps: Option<u32>,
    /// Earliest and latest tagged year among the tracks.
    pub year_min: Option<u32>,
    pub year_max: Option<u32>,
    /// Number of tracks that could not be probed (remote or unreadable).
    pub unprobed: usize,
}

/// Average bitrate in kbit/s of `payload_bytes` of audio over `duration`,
/// rounded to the nearest integer. The one bitrate formula shared by the
/// library scanner (stored `Track::bitrate`) and the Properties drawer, so the
/// two always agree. `None` when the duration is unknown or zero.
pub fn bitrate_kbps(payload_bytes: u64, duration: Duration) -> Option<u32> {
    let secs = duration.as_secs_f64();
    (secs > 0.0 && payload_bytes > 0)
        .then(|| (payload_bytes as f64 * 8.0 / secs / 1000.0).round() as u32)
}

/// Audio payload bytes of an open file (see [`layout`] for what counts as
/// payload per container). Header-only reads; used by the scanner.
pub(crate) fn payload_bytes(file: &File, size: u64) -> u64 {
    layout::measure(file, size, false).map_or(size, |l| l.payload_bytes)
}

/// Codecs whose output is bit-exact with the source.
fn is_lossless_codec(short_name: &str) -> bool {
    matches!(
        short_name,
        "flac" | "alac" | "ape" | "wavpack" | "tta" | "shorten"
    ) || short_name.starts_with("pcm")
        || short_name.starts_with("dsd")
}

/// Bit depth of a stream: `params.bits_per_sample` for lossless codecs
/// only. Containers like Matroska report a nominal sample width for lossy
/// codecs too, and a lossy stream has no bit depth.
pub(crate) fn lossless_bit_depth(
    params: &symphonia::core::codecs::audio::AudioCodecParameters,
) -> Option<u32> {
    let bits = params.bits_per_sample.filter(|&b| b > 0)?;
    let codec = symphonia::default::get_codecs()
        .get_audio_decoder(params.codec)
        .map(|r| r.codec.info)?;
    is_lossless_codec(codec.short_name).then_some(bits)
}

fn channel_layout_name(count: usize) -> Option<&'static str> {
    Some(match count {
        0 => return None,
        1 => "Mono",
        2 => "Stereo",
        3 => "3.0",
        4 => "Quad",
        5 => "5.0",
        6 => "5.1",
        7 => "6.1",
        8 => "7.1",
        _ => "Multichannel",
    })
}

/// AAC audio object type from the first bits of an AudioSpecificConfig, for the
/// HE-AAC flavours Symphonia's profile table does not distinguish.
fn aac_profile_from_asc(asc: &[u8]) -> Option<&'static str> {
    match asc.first()? >> 3 {
        1 => Some("Main"),
        2 => Some("LC"),
        3 => Some("SSR"),
        4 => Some("LTP"),
        5 => Some("HE-AAC (SBR)"),
        29 => Some("HE-AAC v2 (SBR+PS)"),
        _ => None,
    }
}

fn std_string(tag: &StandardTag) -> Option<&str> {
    use StandardTag as T;
    match tag {
        T::RecordingDate(v)
        | T::ReleaseDate(v)
        | T::RecordingTime(v)
        | T::OriginalRecordingDate(v)
        | T::OriginalReleaseDate(v)
        | T::OriginalRecordingTime(v)
        | T::OriginalReleaseTime(v)
        | T::Encoder(v)
        | T::EncoderSettings(v)
        | T::Composer(v)
        | T::ReplayGainTrackGain(v)
        | T::ReplayGainTrackPeak(v)
        | T::ReplayGainAlbumGain(v)
        | T::ReplayGainAlbumPeak(v) => Some(v.as_str()).filter(|s| !s.trim().is_empty()),
        _ => None,
    }
}

/// Fills the tag-derived fields of `info` from the merged Symphonia tags.
fn apply_tags(raw: &[SymTag], info: &mut TechInfo) {
    use StandardTag as T;
    let mut recording_year = None;
    let mut original_year = None;
    let mut encoder = None;
    let mut encoder_settings = None;
    for tag in raw {
        let Some(std) = tag.std.as_ref() else {
            continue;
        };
        let text = std_string(std).map(str::trim);
        match std {
            T::RecordingDate(_) | T::RecordingTime(_) => {
                info.date
                    .get_or_insert_with(|| text.unwrap_or_default().to_owned());
            }
            T::ReleaseDate(_) => {
                info.date
                    .get_or_insert_with(|| text.unwrap_or_default().to_owned());
            }
            T::OriginalRecordingDate(_)
            | T::OriginalRecordingTime(_)
            | T::OriginalReleaseDate(_)
            | T::OriginalReleaseTime(_) => {
                info.original_date
                    .get_or_insert_with(|| text.unwrap_or_default().to_owned());
            }
            T::RecordingYear(y) => recording_year = recording_year.or(Some(*y)),
            T::ReleaseYear(y) => recording_year = recording_year.or(Some(*y)),
            T::OriginalRecordingYear(y) | T::OriginalReleaseYear(y) => {
                original_year = original_year.or(Some(*y));
            }
            T::Encoder(_) => encoder = encoder.or(text.map(str::to_owned)),
            T::EncoderSettings(_) => {
                encoder_settings = encoder_settings.or(text.map(str::to_owned));
            }
            T::Composer(_) => {
                if info.composer.is_none() {
                    info.composer = text.map(str::to_owned);
                }
            }
            T::ReplayGainTrackGain(_) => {
                info.replay_gain.track_gain = info
                    .replay_gain
                    .track_gain
                    .take()
                    .or(text.map(str::to_owned));
            }
            T::ReplayGainTrackPeak(_) => {
                info.replay_gain.track_peak = info
                    .replay_gain
                    .track_peak
                    .take()
                    .or(text.map(str::to_owned));
            }
            T::ReplayGainAlbumGain(_) => {
                info.replay_gain.album_gain = info
                    .replay_gain
                    .album_gain
                    .take()
                    .or(text.map(str::to_owned));
            }
            T::ReplayGainAlbumPeak(_) => {
                info.replay_gain.album_peak = info
                    .replay_gain
                    .album_peak
                    .take()
                    .or(text.map(str::to_owned));
            }
            _ => {}
        }
    }
    info.date = info
        .date
        .take()
        .filter(|d| !d.is_empty())
        .or_else(|| recording_year.map(|y| y.to_string()));
    info.original_date = info
        .original_date
        .take()
        .filter(|d| !d.is_empty())
        .or_else(|| original_year.map(|y| y.to_string()));
    // Tag-level encoder info is the fallback; container headers win (see `probe`).
    info.encoder = encoder.or(encoder_settings);
}

/// Probes `path` (blocking).
///
/// Container layout (payload bytes, tag blocks, FLAC MD5, vendor/encoder, MP3
/// Xing/LAME) comes from [`layout`]; codec, sample rate, bit depth, channels,
/// duration and tags come from the Symphonia fork. The average bitrate uses
/// the same payload-based formula the scanner stores ([`bitrate_kbps`]).
pub fn probe(path: &Path) -> Result<TechInfo, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    let layout = layout::measure(&file, size, true).map_err(|e| e.to_string())?;

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            local_file_format_options(path),
            MetadataOptions::default(),
        )
        .map_err(|e| e.to_string())?;

    let format = *reader.format_info();
    let (params, track_id, duration) = {
        let track = reader.default_track(TrackType::Audio);
        let params = track.and_then(|t| match t.codec_params.as_ref() {
            Some(CodecParameters::Audio(a)) => Some(a.clone()),
            _ => None,
        });
        let duration = tags::track_duration(track, reader.media_info(), format.format);
        (params, track.map(|t| u64::from(t.id)), duration)
    };
    let (raw_tags, _) = tags::drain_metadata(&mut reader.metadata(), false, track_id);

    let mut info = TechInfo {
        file_size: Some(size),
        duration,
        ..TechInfo::default()
    };
    apply_tags(&raw_tags, &mut info);

    info.container = layout
        .container
        .clone()
        .or_else(|| Some(format.long_name.to_owned()));

    if let Some(params) = &params {
        let codec = symphonia::default::get_codecs()
            .get_audio_decoder(params.codec)
            .map(|r| r.codec.info);
        if let Some(codec) = codec {
            info.codec = Some(codec.long_name.to_owned());
            info.codec_short = Some(codec.short_name.to_owned());
            info.profile = params
                .profile
                .and_then(|p| codec.profiles.iter().find(|i| i.profile == p))
                .map(|p| p.long_name.to_owned());
            if codec.short_name == "aac"
                && let Some(p) = params.extra_data.as_deref().and_then(aac_profile_from_asc)
                && p.starts_with("HE")
            {
                info.profile = Some(p.to_owned());
            }
            info.lossless = Some(is_lossless_codec(codec.short_name));
        }
        info.sample_rate = params.sample_rate.filter(|&r| r > 0);
        // Containers like Matroska report a nominal sample width for lossy
        // codecs too; a lossy stream has no bit depth.
        let lossy = info.lossless == Some(false);
        info.bits_per_sample = params.bits_per_sample.filter(|&b| b > 0 && !lossy);
        if let Some(ch) = &params.channels {
            let n = ch.count();
            info.channels = u32::try_from(n).ok().filter(|&n| n > 0);
            info.channel_layout = channel_layout_name(n).map(str::to_owned);
        }
    }

    if let Some(mp3) = &layout.mp3 {
        info.profile = Some(mp3.profile.clone());
        info.nominal_bitrate_kbps = Some(mp3.nominal_kbps);
        info.bitrate_mode = Some(mp3.mode);
    }
    if let Some((nominal, mode)) = layout.declared_bitrate {
        info.nominal_bitrate_kbps = Some(nominal);
        info.bitrate_mode = Some(mode);
    }
    if info.lossless == Some(true) {
        info.bitrate_mode = Some(BitrateMode::Lossless);
    }
    if let Some(encoder) = layout.encoder.clone() {
        info.encoder = Some(encoder);
    }
    info.md5 = layout.md5.clone();
    info.tag_formats = layout.tag_formats;
    info.payload_bytes = Some(layout.payload_bytes);
    info.bitrate_kbps = duration.and_then(|d| bitrate_kbps(layout.payload_bytes, d));
    Ok(info)
}

/// Short label for one track's format in the album summary (`FLAC`, `MP3`, …).
fn format_label(info: &TechInfo) -> Option<String> {
    let short = info.codec_short.as_deref()?;
    Some(match short {
        "vorbis" => "Vorbis".to_owned(),
        "wavpack" => "WavPack".to_owned(),
        s if s.starts_with("pcm") => info
            .container
            .clone()
            .unwrap_or_else(|| s.to_ascii_uppercase()),
        s => s.to_ascii_uppercase(),
    })
}

/// Summarises an album's tracks (blocking: probes every local file).
///
/// Remote tracks and unreadable files contribute only what the library already
/// stores on [`Track`] (sample rate, bitrate, year, duration) and count towards
/// [`AlbumSummary::unprobed`].
pub fn summarize_album(tracks: &[Track]) -> AlbumSummary {
    let mut s = AlbumSummary {
        track_count: tracks.len(),
        ..AlbumSummary::default()
    };
    let mut formats = BTreeSet::new();
    let mut rates = BTreeSet::new();
    let mut depths = BTreeSet::new();
    let mut payload_total = 0u64;
    let mut payload_secs = 0.0f64;
    for track in tracks {
        s.total_duration += track.duration;
        if track.year > 0 {
            s.year_min = Some(s.year_min.map_or(track.year, |y| y.min(track.year)));
            s.year_max = Some(s.year_max.map_or(track.year, |y| y.max(track.year)));
        }
        let probed = (track.provider_id.as_ref() == "local")
            .then(|| probe(&track.path).ok())
            .flatten();
        let Some(info) = probed else {
            s.unprobed += 1;
            if track.sample_rate > 0 {
                rates.insert(track.sample_rate);
            }
            if track.bit_depth > 0 {
                depths.insert(track.bit_depth);
            }
            if track.bitrate > 0 {
                s.bitrate_min_kbps = Some(
                    s.bitrate_min_kbps
                        .map_or(track.bitrate, |b| b.min(track.bitrate)),
                );
                s.bitrate_max_kbps = Some(
                    s.bitrate_max_kbps
                        .map_or(track.bitrate, |b| b.max(track.bitrate)),
                );
            }
            continue;
        };
        if let Some(size) = info.file_size {
            *s.total_size.get_or_insert(0) += size;
        }
        if let Some(label) = format_label(&info) {
            formats.insert(label);
        }
        rates.extend(info.sample_rate);
        depths.extend(info.bits_per_sample);
        if let Some(kbps) = info.bitrate_kbps {
            s.bitrate_min_kbps = Some(s.bitrate_min_kbps.map_or(kbps, |b| b.min(kbps)));
            s.bitrate_max_kbps = Some(s.bitrate_max_kbps.map_or(kbps, |b| b.max(kbps)));
        }
        if let (Some(payload), Some(d)) = (info.payload_bytes, info.duration) {
            payload_total += payload;
            payload_secs += d.as_secs_f64();
        }
    }
    s.formats = formats.into_iter().collect();
    s.sample_rates = rates.into_iter().collect();
    s.bit_depths = depths.into_iter().collect();
    s.bitrate_avg_kbps = bitrate_kbps(payload_total, Duration::from_secs_f64(payload_secs));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_rounds_and_rejects_unknown_duration() {
        // 40 000 bytes over 1 s = 320 kbps.
        assert_eq!(bitrate_kbps(40_000, Duration::from_secs(1)), Some(320));
        assert_eq!(bitrate_kbps(39_990, Duration::from_secs(1)), Some(320));
        assert_eq!(bitrate_kbps(40_000, Duration::ZERO), None);
        assert_eq!(bitrate_kbps(0, Duration::from_secs(1)), None);
    }

    #[test]
    fn aac_asc_profiles() {
        assert_eq!(aac_profile_from_asc(&[0x12, 0x10]), Some("LC"));
        assert_eq!(aac_profile_from_asc(&[0x2b, 0x92]), Some("HE-AAC (SBR)"));
        assert_eq!(
            aac_profile_from_asc(&[0xeb, 0x92]),
            Some("HE-AAC v2 (SBR+PS)")
        );
    }

    #[test]
    fn channel_names() {
        assert_eq!(channel_layout_name(2), Some("Stereo"));
        assert_eq!(channel_layout_name(6), Some("5.1"));
        assert_eq!(channel_layout_name(0), None);
    }
}
