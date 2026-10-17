// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Audio quality classification (lossy / CD-lossless / hi-res / DSD).
//!
//! Aulos never demuxes a container to inspect its actual codec — that would
//! mean pulling in a probing dependency just to paint a badge. Instead this
//! module infers a quality tier from data already sitting on [`Track`]: the
//! file extension (a reasonable proxy for container/codec) plus the sample
//! rate PCM decoders already report. It is a heuristic, not a guarantee.

use super::Track;
use std::sync::Arc;

/// A track's audio quality tier, ordered worst → best so
/// [`album_quality`] can fold a whole album down to its best tier with
/// [`Iterator::max`]. `Unknown` sorts below every known tier (rather than
/// above, as "worst" might suggest) precisely so it never wins a `max()`
/// against a track that *did* classify — an album with one untagged track
/// and the rest FLAC should still read as lossless, not unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AudioQuality {
    Unknown,
    Lossy,
    CdLossless,
    HiRes,
    Dsd,
}

impl AudioQuality {
    /// Icon for this tier, drawn from names already used elsewhere in
    /// `src/views/` (verified, not invented) rather than the private icon
    /// names Euphonica's own theme ships:
    /// - `Lossy`/`Unknown`: `audio-x-generic-symbolic` — the app's existing
    ///   generic-audio glyph (e.g. `songs.rs`'s empty state, `convert.rs`).
    /// - `CdLossless`: `media-optical-symbolic` — already the
    ///   album/CD placeholder icon in `albums.rs` and `artists.rs`; CD
    ///   quality is literally what it depicts.
    /// - `HiRes`: `audio-card-symbolic` — already used in `genres.rs` for
    ///   the "jazz" genre glyph; reused here to suggest dedicated audio
    ///   hardware, i.e. above consumer CD quality.
    /// - `Dsd`: `applications-multimedia-symbolic` — already used in
    ///   `genres.rs` for the "soundtrack" genre glyph; reused as the
    ///   distinct marker for the rare exotic format.
    #[must_use]
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Unknown | Self::Lossy => "audio-x-generic-symbolic",
            Self::CdLossless => "media-optical-symbolic",
            Self::HiRes => "audio-card-symbolic",
            Self::Dsd => "applications-multimedia-symbolic",
        }
    }

    /// Short uppercase tag for a compact badge.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "",
            Self::Lossy => "LOSSY",
            Self::CdLossless => "CD",
            Self::HiRes => "HI-RES",
            Self::Dsd => "DSD",
        }
    }

    /// Whether this tier was actually determined, as opposed to a fallback
    /// for an unrecognized or absent extension.
    #[must_use]
    pub fn is_known(self) -> bool {
        self != Self::Unknown
    }
}

/// Classify a track's audio quality tier from its file suffix (lowercase,
/// without the dot, see [`super::Track::suffix`]), sample rate and bit depth.
///
/// `bit_depth` is only ever stored for lossless streams (`0` for lossy or
/// unknown), which settles `m4a`, the one ambiguous extension: it holds
/// either ALAC (lossless) or AAC (lossy). With a bit depth it is ALAC; without
/// one it is treated as lossy unless the sample rate is above 48 kHz, since
/// AAC almost never ships above that while hi-res ALAC commonly does (a file
/// scanned before the depth was read from the ALAC header keeps `0`, and CD-rate
/// ALAC of that kind stays `Lossy` until it is rescanned).
#[must_use]
pub fn classify(suffix: &str, sample_rate: u32, bit_depth: u32) -> AudioQuality {
    let is_hi_res = sample_rate > 48_000;
    match suffix {
        "dsf" | "dff" | "dsd" => AudioQuality::Dsd,
        "flac" | "wav" | "aiff" | "aif" | "alac" | "ape" | "wv" | "tta" | "shn" | "caf" => {
            if is_hi_res {
                AudioQuality::HiRes
            } else {
                AudioQuality::CdLossless
            }
        }
        "m4a" => {
            if is_hi_res {
                AudioQuality::HiRes
            } else if bit_depth > 0 {
                AudioQuality::CdLossless
            } else {
                AudioQuality::Lossy
            }
        }
        "mp3" | "aac" | "ogg" | "oga" | "opus" | "wma" | "mpc" | "ac3" | "dts" | "m4b" => {
            AudioQuality::Lossy
        }
        _ => AudioQuality::Unknown,
    }
}

/// The best quality tier across an album's tracks — `Unknown` for an empty
/// slice, since there is nothing to grade.
#[must_use]
pub fn album_quality(tracks: &[Arc<Track>]) -> AudioQuality {
    tracks
        .iter()
        .map(|track| classify(&track.suffix, track.sample_rate, track.bit_depth))
        .max()
        .unwrap_or(AudioQuality::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn track(path: &str, sample_rate: u32, bitrate: u32) -> Track {
        Track {
            id: 0,
            path: PathBuf::from(path),
            title: String::new(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 0,
            duration: Duration::ZERO,
            bitrate,
            sample_rate,
            provider_id: Arc::from("local"),
            source_uri: path.to_string(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
            bit_depth: 0,
            release_type: String::new(),
            suffix: super::super::suffix_of(path),
            added_at: 0,
            favorited_at: 0,
        }
    }

    #[test]
    fn flac_at_cd_rate_is_cd_lossless() {
        assert_eq!(classify("flac", 44_100, 16), AudioQuality::CdLossless);
    }

    #[test]
    fn flac_above_cd_rate_is_hi_res() {
        assert_eq!(classify("flac", 96_000, 24), AudioQuality::HiRes);
    }

    #[test]
    fn dsf_is_dsd() {
        assert_eq!(classify("dsf", 2_822_400, 0), AudioQuality::Dsd);
    }

    #[test]
    fn mp3_is_lossy() {
        assert_eq!(classify("mp3", 44_100, 0), AudioQuality::Lossy);
    }

    #[test]
    fn m4a_at_cd_rate_is_treated_as_lossy_aac() {
        assert_eq!(classify("m4a", 44_100, 0), AudioQuality::Lossy);
    }

    #[test]
    fn m4a_with_a_bit_depth_is_cd_rate_alac() {
        assert_eq!(classify("m4a", 44_100, 16), AudioQuality::CdLossless);
    }

    #[test]
    fn m4a_above_cd_rate_is_treated_as_hi_res_alac() {
        assert_eq!(classify("m4a", 96_000, 0), AudioQuality::HiRes);
    }

    #[test]
    fn no_extension_is_unknown() {
        assert_eq!(classify("", 44_100, 0), AudioQuality::Unknown);
    }

    #[test]
    fn zero_sample_rate_never_promotes_to_hi_res() {
        assert_eq!(classify("flac", 0, 0), AudioQuality::CdLossless);
    }

    #[test]
    fn album_quality_picks_the_best_of_a_mixed_slice() {
        let tracks = vec![
            Arc::new(track("a.mp3", 44_100, 320)),
            Arc::new(track("b.flac", 96_000, 2304)),
            Arc::new(track("c", 0, 0)),
        ];
        assert_eq!(album_quality(&tracks), AudioQuality::HiRes);
    }

    #[test]
    fn album_quality_of_empty_slice_is_unknown() {
        assert_eq!(album_quality(&[]), AudioQuality::Unknown);
    }
}
