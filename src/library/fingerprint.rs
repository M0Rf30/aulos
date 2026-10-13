// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Chromaprint/AcoustID audio fingerprints.
//!
//! Audio is decoded with the same Symphonia-based decoder the player uses
//! (so every container the player opens can be fingerprinted, DSD
//! included) and fed to the pure-Rust `rusty-chromaprint` port using
//! chromaprint's default algorithm (`test2`, algorithm id 1) — the one
//! AcoustID's index is built from. Like `fpcalc`, only the first
//! [`MAX_ANALYZE_SECS`] seconds are analysed while the *full* track
//! duration is reported alongside, because AcoustID uses both.

use std::path::Path;

use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};

use crate::library::tagio::base64;
use crate::player::engine::decoder::SymphoniaDecoder;

/// How much audio is analysed, matching `fpcalc`'s default `-length`.
pub const MAX_ANALYZE_SECS: u32 = 120;

/// PCM rate DSD streams are decimated to before fingerprinting.
const DSD_PCM_RATE: u32 = 44_100;

/// Samples decoded per read. A multiple of every channel count from 1 to
/// 8 (lcm = 840), so a full read always ends on a frame boundary and the
/// interleaved `i16` chunks handed to the fingerprinter never split a
/// frame regardless of the channel layout discovered at the first packet.
const CHUNK_SAMPLES: usize = 840 * 10;

/// An AcoustID fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    /// Duration of the *whole* track in seconds (rounded), as AcoustID's
    /// lookup/submit APIs expect — not just the analysed prefix.
    pub duration_secs: u32,
    /// The raw 32-bit sub-fingerprints.
    pub raw: Vec<u32>,
    /// Chromaprint's compressed encoding (algorithm-id byte, 24-bit item
    /// count, packed 3/5-bit deltas) in URL-safe unpadded base64: the
    /// string AcoustID's `fingerprint` parameter takes and `fpcalc`
    /// prints.
    pub compressed: String,
}

impl Fingerprint {
    /// Builds a fingerprint from raw sub-fingerprints, compressing them
    /// with the default algorithm.
    #[must_use]
    pub fn from_raw(duration_secs: u32, raw: Vec<u32>) -> Self {
        let config = Configuration::preset_test2();
        let bytes = FingerprintCompressor::from(&config).compress(&raw);
        Self {
            duration_secs,
            compressed: base64::encode_url_nopad(&bytes),
            raw,
        }
    }
}

/// Fingerprints `path`: decodes up to the first [`MAX_ANALYZE_SECS`]
/// seconds and runs chromaprint's default algorithm over them.
///
/// Blocking and CPU-bound; call it from a worker thread.
pub fn compute(path: &Path) -> Result<Fingerprint, String> {
    let mut decoder = SymphoniaDecoder::open(path)
        .map_err(|e| format!("Cannot decode {}: {e}", path.display()))?;
    if decoder.is_dsd() {
        decoder
            .enable_pcm_conversion(DSD_PCM_RATE)
            .map_err(|e| format!("Cannot convert DSD to PCM: {e}"))?;
    }
    let rate = decoder.sample_rate();
    let total_secs = decoder.duration();

    let config = Configuration::preset_test2();
    let mut printer = Fingerprinter::new(&config);
    let max_samples_per_channel = u64::from(rate) * u64::from(MAX_ANALYZE_SECS);

    let mut channels = 0usize;
    let mut frames_fed = 0u64;
    let mut hit_end = false;
    let mut pcm = vec![0f32; CHUNK_SAMPLES];
    let mut pcm16: Vec<i16> = Vec::with_capacity(CHUNK_SAMPLES);

    while frames_fed < max_samples_per_channel {
        let n = decoder
            .read(&mut pcm)
            .map_err(|e| format!("Decode error in {}: {e}", path.display()))?;
        if n == 0 {
            hit_end = true;
            break;
        }
        if channels == 0 {
            // Known only once the first packet is decoded.
            channels = usize::from(decoder.channels()).max(1);
            printer.start(rate, channels as u32).map_err(|e| {
                format!("Unsupported audio format ({rate} Hz, {channels} ch): {e:?}")
            })?;
        }
        // Only the end of the stream can return a partial frame.
        let usable = n - n % channels;
        pcm16.clear();
        pcm16.extend(pcm[..usable].iter().map(|&s| to_i16(s)));
        let frames = (usable / channels) as u64;
        let remaining = max_samples_per_channel - frames_fed;
        let take = frames.min(remaining) as usize * channels;
        printer.consume(&pcm16[..take]);
        frames_fed += take as u64 / channels as u64;
        if n < CHUNK_SAMPLES {
            hit_end = true;
            break;
        }
    }
    if channels == 0 {
        return Err(format!("{} contains no decodable audio", path.display()));
    }
    printer.finish();

    // Container duration when it declares one; otherwise the decoded
    // length, which is the real duration only if the stream really ended.
    let duration_secs = match total_secs.filter(|d| d.is_finite() && *d > 0.0) {
        Some(d) => d.round() as u32,
        None if hit_end => (frames_fed as f64 / f64::from(rate)).round() as u32,
        None => {
            return Err(format!(
                "{} does not declare its duration, which AcoustID needs",
                path.display()
            ));
        }
    };

    let raw = printer.fingerprint().to_vec();
    if raw.is_empty() {
        return Err(format!(
            "{} is too short or too quiet to fingerprint",
            path.display()
        ));
    }
    Ok(Fingerprint::from_raw(duration_secs, raw))
}

/// Float PCM in `[-1, 1]` to the `i16` chromaprint consumes.
fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    /// Writes `secs` seconds of a deterministic synthetic "melody" (a new
    /// pair of tones every 250 ms) as a 16-bit WAV.
    fn write_tone_wav(path: &Path, rate: u32, channels: u16, secs: u32) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..rate * secs {
            let t = i as f32 / rate as f32;
            let step = (t * 4.0) as u32;
            let f1 = 220.0 + (step * 37 % 11) as f32 * 55.0;
            let f2 = 330.0 + (step * 53 % 7) as f32 * 71.0;
            let v = 0.35 * (TAU * f1 * t).sin() + 0.25 * (TAU * f2 * t).sin();
            let s = (v * f32::from(i16::MAX)) as i16;
            for _ in 0..channels {
                w.write_sample(s).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    fn temp_wav(label: &str, rate: u32, channels: u16, secs: u32) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aulos-fingerprint-test-{label}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        write_tone_wav(&path, rate, channels, secs);
        path
    }

    /// Inverse of chromaprint's `compress`, as `chromaprint_decode_fingerprint`
    /// does it: header, then all 3-bit "normal" deltas, then the 5-bit
    /// "exceptional" extensions for every normal value that saturated at 7.
    fn decompress(bytes: &[u8]) -> (u8, Vec<u32>) {
        /// The `index`-th `bits`-wide value of an LSB-first packed array.
        fn item(data: &[u8], bits: usize, index: usize) -> u8 {
            (0..bits).fold(0u8, |v, b| {
                let pos = index * bits + b;
                v | (((data[pos / 8] >> (pos % 8)) & 1) << b)
            })
        }
        let id = bytes[0];
        let size =
            (usize::from(bytes[1]) << 16) | (usize::from(bytes[2]) << 8) | usize::from(bytes[3]);
        let body = &bytes[4..];
        // Normal values run until `size` terminating zeros have been seen.
        let mut normal = Vec::new();
        let mut zeros = 0;
        while zeros < size {
            let v = item(body, 3, normal.len());
            zeros += usize::from(v == 0);
            normal.push(v);
        }
        let exceptional = &body[(normal.len() * 3).div_ceil(8)..];
        let mut exc_index = 0;
        let mut out = Vec::with_capacity(size);
        let mut last = 0u32;
        let mut bit = 0u32;
        let mut value = 0u32;
        for v in normal {
            if v == 0 {
                last ^= value;
                out.push(last);
                value = 0;
                bit = 0;
            } else {
                let mut delta = u32::from(v);
                if v == 7 {
                    delta += u32::from(item(exceptional, 5, exc_index));
                    exc_index += 1;
                }
                bit += delta;
                value |= 1 << (bit - 1);
            }
        }
        (id, out)
    }

    #[test]
    fn fingerprints_a_decoded_file_deterministically() {
        let path = temp_wav("det", 11_025, 1, 20);
        let a = compute(&path).unwrap();
        let b = compute(&path).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.duration_secs, 20);
        // 20 s at 11025 Hz / 1365-sample items is ~ 100 sub-fingerprints
        // minus the filter delay.
        assert!(a.raw.len() > 80, "got {}", a.raw.len());
        assert!(a.raw.iter().any(|&v| v != 0));
    }

    #[test]
    fn compressed_form_has_chromaprint_header_and_round_trips() {
        let path = temp_wav("hdr", 11_025, 1, 20);
        let fp = compute(&path).unwrap();
        let bytes = base64::decode(&fp.compressed).expect("valid url-safe base64");
        assert!(!fp.compressed.contains('=') && !fp.compressed.contains('+'));
        // chromaprint_encode_fingerprint: algorithm id, then 24-bit count.
        assert_eq!(bytes[0], 1, "default algorithm id (test2)");
        let n =
            (usize::from(bytes[1]) << 16) | (usize::from(bytes[2]) << 8) | usize::from(bytes[3]);
        assert_eq!(n, fp.raw.len());
        let (id, raw) = decompress(&bytes);
        assert_eq!(id, 1);
        assert_eq!(raw, fp.raw);
    }

    #[test]
    fn round_trip_with_exceptional_deltas() {
        // Sparse words force deltas above the 3-bit range.
        let raw = vec![0x8000_0001, 0, 0x0000_0100, 0xFFFF_FFFF, 0x4000_0000];
        let fp = Fingerprint::from_raw(30, raw.clone());
        let (id, back) = decompress(&base64::decode(&fp.compressed).unwrap());
        assert_eq!(id, 1);
        assert_eq!(back, raw);
    }

    #[test]
    fn resampled_stereo_copy_has_the_same_length_and_duration() {
        let mono = compute(&temp_wav("m", 11_025, 1, 20)).unwrap();
        let stereo = compute(&temp_wav("s", 44_100, 2, 20)).unwrap();
        assert_eq!(mono.duration_secs, stereo.duration_secs);
        assert!(mono.raw.len().abs_diff(stereo.raw.len()) <= 2);
    }

    #[test]
    fn only_the_first_two_minutes_are_analysed_but_full_duration_is_reported() {
        let long = compute(&temp_wav("long", 8_000, 1, 150)).unwrap();
        assert_eq!(long.duration_secs, 150);
        let item_secs = Configuration::preset_test2().item_duration_in_seconds();
        let max_items = (f64::from(MAX_ANALYZE_SECS) / f64::from(item_secs)) as usize;
        assert!(long.raw.len() <= max_items + 1, "got {}", long.raw.len());
    }

    #[test]
    fn missing_file_is_a_readable_error() {
        let err = compute(Path::new("/nonexistent/aulos/none.flac")).unwrap_err();
        assert!(err.contains("none.flac"), "{err}");
    }
}
