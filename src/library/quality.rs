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
use std::path::Path;

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
    /// - `CdLossless`: `media-optical-cd-audio-symbolic` — already the
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
            Self::CdLossless => "media-optical-cd-audio-symbolic",
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

/// MPD and Subsonic populate `Track::path` with a relative path or a
/// streaming URI rather than a real filesystem path, and a streaming URI
/// may carry a `?query` suffix (auth tokens, transcode params) that would
/// otherwise get mistaken for part of the extension. Strip it before
/// asking `Path` for the extension.
fn extension_of(path: &Path) -> Option<String> {
    let raw = path.to_string_lossy();
    let without_query = raw.split('?').next().unwrap_or(&raw);
    Path::new(without_query)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

/// Classify a track's audio quality tier from its file extension and
/// sample rate.
///
/// `bitrate` is accepted so every call site can pass all three metadata
/// values it has on hand uniformly, but today's rules don't need it —
/// tier is decided entirely by container/codec and sample rate.
///
/// `m4a` is the one genuinely ambiguous extension: it holds either ALAC
/// (lossless) or AAC (lossy), and nothing on `Track` says which. This
/// treats it as lossy unless the sample rate is above 48 kHz, since AAC
/// almost never ships above that while hi-res ALAC commonly does — CD-rate
/// ALAC (44.1/48 kHz) is misclassified as `Lossy` under this heuristic.
#[must_use]
pub fn classify(path: &Path, sample_rate: u32, _bitrate: u32) -> AudioQuality {
    let Some(ext) = extension_of(path) else {
        return AudioQuality::Unknown;
    };
    let is_hi_res = sample_rate > 48_000;
    match ext.as_str() {
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
pub fn album_quality(tracks: &[Track]) -> AudioQuality {
    tracks
        .iter()
        .map(|track| classify(&track.path, track.sample_rate, track.bitrate))
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
        }
    }

    #[test]
    fn flac_at_cd_rate_is_cd_lossless() {
        assert_eq!(
            classify(Path::new("song.flac"), 44_100, 900),
            AudioQuality::CdLossless
        );
    }

    #[test]
    fn flac_above_cd_rate_is_hi_res() {
        assert_eq!(
            classify(Path::new("song.flac"), 96_000, 2304),
            AudioQuality::HiRes
        );
    }

    #[test]
    fn dsf_is_dsd() {
        assert_eq!(
            classify(Path::new("song.dsf"), 2_822_400, 0),
            AudioQuality::Dsd
        );
    }

    #[test]
    fn mp3_is_lossy() {
        assert_eq!(
            classify(Path::new("song.mp3"), 44_100, 320),
            AudioQuality::Lossy
        );
    }

    #[test]
    fn m4a_at_cd_rate_is_treated_as_lossy_aac() {
        assert_eq!(
            classify(Path::new("song.m4a"), 44_100, 256),
            AudioQuality::Lossy
        );
    }

    #[test]
    fn m4a_above_cd_rate_is_treated_as_hi_res_alac() {
        assert_eq!(
            classify(Path::new("song.m4a"), 96_000, 0),
            AudioQuality::HiRes
        );
    }

    #[test]
    fn no_extension_is_unknown() {
        assert_eq!(
            classify(Path::new("song"), 44_100, 320),
            AudioQuality::Unknown
        );
    }

    #[test]
    fn zero_sample_rate_never_promotes_to_hi_res() {
        assert_eq!(
            classify(Path::new("song.flac"), 0, 0),
            AudioQuality::CdLossless
        );
    }

    #[test]
    fn query_suffix_on_a_streaming_uri_still_classifies() {
        assert_eq!(
            classify(
                Path::new("http://host/stream/song.flac?token=abc123"),
                48_000,
                0
            ),
            AudioQuality::CdLossless
        );
    }

    #[test]
    fn album_quality_picks_the_best_of_a_mixed_slice() {
        let tracks = vec![
            track("a.mp3", 44_100, 320),
            track("b.flac", 96_000, 2304),
            track("c", 0, 0),
        ];
        assert_eq!(album_quality(&tracks), AudioQuality::HiRes);
    }

    #[test]
    fn album_quality_of_empty_slice_is_unknown() {
        assert_eq!(album_quality(&[]), AudioQuality::Unknown);
    }
}
