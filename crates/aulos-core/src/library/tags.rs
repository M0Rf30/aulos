// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Shared Symphonia-based tag/audio-property reading, used by the library
//! scanner, cover art extractor, lyrics provider, and the file converter's
//! tag copier. Replaces the previous tag-reading library (now removed).
//!
//! Ports the probe/tag-mapping approach from the sibling `rmpd` project's
//! `rmpd-library::metadata` (same M0Rf30 Symphonia fork this crate already
//! depends on for playback, rev `2c160a8`), trimmed to the fields aulos
//! actually surfaces (`crate::library::Track` has no bit-depth/channel
//! fields — those are read independently by
//! [`crate::convert::pipeline::AudioSource`] for the converter's own
//! needs). Symphonia is read-only: writing tags is handled by
//! `crate::library::tagio`.

use std::fs;
use std::path::Path;
use std::time::Duration;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatId, MediaInfo, Track, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::{
    Metadata, MetadataOptions, MetadataRevision, RawValue, StandardTag, StandardVisualKey,
    Tag as SymTag, Visual,
};
use symphonia::core::units::Duration as SymDuration;

use super::sparse::SparseFile;
use crate::player::engine::decoder::{chain_duration_secs, local_file_format_options};

/// An embedded picture (cover art), as extracted from a tag.
#[derive(Debug, Clone)]
pub struct Picture {
    pub mime_type: String,
    pub is_front_cover: bool,
    pub data: Vec<u8>,
}

/// Tag fields aulos's library/converter care about. Every field is `None`
/// (or empty) when the container/tag doesn't carry it — callers apply
/// their own fallbacks (e.g. filename-as-title in the scanner).
#[derive(Debug, Clone, Default)]
pub struct AudioTags {
    pub title: Option<String>,
    /// Every `StandardTag::Artist` value found, joined with `"; "` — the
    /// top delimiter in `crate::library::artist_tags::DEFAULT_DELIMITERS`
    /// — so a file with multiple ARTIST= comments (multi-valued Vorbis
    /// tags) round-trips into one string that the existing artist-tag
    /// splitter can still fan back out at aggregation time.
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    /// Full release date string as the container stored it (e.g. `"2024"`
    /// or `"2024-05-01"`).
    pub date: Option<String>,
    /// Just the leading 4-digit year, for `Track::year`.
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_number: Option<u32>,
    pub disc_total: Option<u32>,
    /// Embedded lyrics text (ID3v2 USLT, Vorbis LYRICS/UNSYNCEDLYRICS, MP4
    /// `\u{a9}lyr`, ...) — whatever the container maps to
    /// `StandardTag::Lyrics`.
    pub lyrics: Option<String>,
    pub rg_track_gain: Option<f32>,
    pub rg_album_gain: Option<f32>,
    /// `true` when the file's compilation flag is set (ID3 `TCMP`, MP4
    /// `cpil`, Vorbis `COMPILATION=1`).
    pub compilation: bool,
    /// Every `StandardTag::MusicBrainzReleaseType` value (e.g. `"album"`,
    /// `"live"`), lowercased and joined with `"; "`; empty when untagged.
    /// See [`crate::library::release_kind::ReleaseKind::from_tag`].
    pub release_type: String,
    pub pictures: Vec<Picture>,
}

/// Audio properties independent of tags. Fields default to `0` when
/// symphonia can't determine them, matching the previous tag reader's
/// `unwrap_or(0)` fallbacks.
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioProperties {
    pub duration: Duration,
    pub sample_rate: u32,
    /// Bits per sample of a lossless stream; `0` for lossy or unknown.
    pub bit_depth: u32,
    /// Average bitrate of the audio payload (file size minus every tag and
    /// metadata block, see [`super::tech_info`]) over duration, in kbps —
    /// symphonia exposes no bitrate field directly. Computed by the same
    /// function the Properties drawer uses, so both always agree.
    pub bitrate: u32,
}

/// Result of probing one file: its tags and audio properties.
#[derive(Debug, Clone, Default)]
pub struct ProbedFile {
    pub tags: AudioTags,
    pub properties: AudioProperties,
}

/// Probes `path` and reads everything aulos needs from its tags and default
/// audio track. `want_pictures` gates collecting embedded picture bytes —
/// pass `false` when only text tags are needed (the scanner, lyrics
/// provider), since cloning artwork bytes is wasted work otherwise.
/// Returns `None` if the file can't be opened or probed.
pub fn probe(path: &Path, want_pictures: bool) -> Option<ProbedFile> {
    let file = fs::File::open(path).ok()?;
    let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
    // Positional header reads only (no seek), so the stream below still starts at 0.
    let payload_bytes = super::tech_info::payload_bytes(&file, file_size);
    probe_source(Box::new(file), payload_bytes, path, want_pictures)
}

/// [`probe`] for a file of which only the ranges the probe needs were loaded
/// (see [`SparseFile`]); `path` only supplies the extension hint. `None`
/// when the probe fails. A `Some` can still rest on guesses when the probe
/// read bytes that were not loaded: take [`SparseFile::hole_flag`] before the
/// call and distrust the result when it is raised.
pub fn probe_sparse(path: &Path, file: SparseFile, want_pictures: bool) -> Option<ProbedFile> {
    let payload_bytes = super::tech_info::payload_bytes(&file, file.size());
    probe_source(Box::new(file), payload_bytes, path, want_pictures)
}

/// Probes `source`, a file (or a stand-in for one) named `path`, whose audio
/// payload measures `payload_bytes`.
fn probe_source(
    source: Box<dyn MediaSource>,
    payload_bytes: u64,
    path: &Path,
    want_pictures: bool,
) -> Option<ProbedFile> {
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mss = MediaSourceStream::new(source, Default::default());

    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            local_file_format_options(path),
            MetadataOptions::default(),
        )
        .ok()?;

    let (duration, sample_rate, bit_depth, track_id) = {
        let track = reader.default_track(TrackType::Audio);
        let audio = track.and_then(|t| match t.codec_params.as_ref() {
            Some(CodecParameters::Audio(a)) => Some(a),
            _ => None,
        });
        let sample_rate = audio.and_then(|a| a.sample_rate);
        let bit_depth = audio.and_then(super::tech_info::lossless_bit_depth);
        let duration = track_duration(track, reader.media_info(), reader.format_info().format);
        let track_id = track.map(|t| u64::from(t.id));
        (duration, sample_rate, bit_depth, track_id)
    };

    // The fork's WAV demuxer reads `id3 ` and `LIST INFO` chunks wherever they sit (including
    // after `data`, where Mp3tag and foobar2000 put them), so the
    // container's own revision already carries them.
    let (raw_tags, pictures) = drain_metadata(&mut reader.metadata(), want_pictures, track_id);

    let bitrate = duration.and_then(|d| super::tech_info::bitrate_kbps(payload_bytes, d));

    let mut tags = AudioTags {
        pictures,
        ..Default::default()
    };
    apply_tags(&raw_tags, &mut tags);

    Some(ProbedFile {
        tags,
        properties: AudioProperties {
            duration: duration.unwrap_or_default(),
            sample_rate: sample_rate.unwrap_or(0),
            bit_depth: bit_depth.unwrap_or(0),
            bitrate: bitrate.unwrap_or(0),
        },
    })
}

/// Compute a track's duration, falling back through every level of
/// precision symphonia exposes: the track's own frame count, then its
/// declared duration in timebase units, then the reader's overall media
/// duration (the only value some demuxers ever populate).
pub(crate) fn track_duration(
    track: Option<&Track>,
    media_info: &MediaInfo,
    format: FormatId,
) -> Option<Duration> {
    // A chained Ogg stream reports the whole chain's duration at the media level; the track
    // only describes the first link.
    if let Some(secs) = chain_duration_secs(format, media_info)
        && let Ok(d) = Duration::try_from_secs_f64(secs)
    {
        return Some(d);
    }
    let from_track = track.and_then(|t| {
        let tb = t.time_base?;
        let dur = match t.num_frames {
            Some(n) => SymDuration::from(n),
            None => t.duration?,
        };
        Some((tb, dur))
    });

    let (tb, dur) = from_track.or_else(|| Some((media_info.time_base?, media_info.duration?)))?;

    tb.calc_duration(dur)
        .and_then(|time| Duration::try_from_secs_f64(time.as_secs_f64()).ok())
}

/// Drain the metadata log into `(tags, pictures)`.
///
/// `tags` merges every revision, newest first (checking the default audio
/// track's per-track tags too, since e.g. Matroska routes track-targeted tag
/// elements there instead of to the media-level tag list). A field (standard
/// tag kind, or raw key when unparsed) is taken only from the newest revision
/// that has it, so a stale/truncated older block (ID3v1's 30-char fields)
/// never shadows or duplicates the real one, while a sparse newer block (an
/// encoder-only leading ID3v2 before an appended tag) no longer hides the
/// older block's fields.
///
/// The `pictures` list is only populated — cloning picture bytes — when a
/// caller actually wants them.
pub(crate) fn drain_metadata(
    log: &mut Metadata<'_>,
    want_pictures: bool,
    track_id: Option<u64>,
) -> (Vec<SymTag>, Vec<Picture>) {
    let mut older: Vec<MetadataRevision> = Vec::new();
    while let Some(discarded) = log.pop() {
        older.push(discarded);
    }

    let mut tags: Vec<SymTag> = Vec::new();
    let mut seen: std::collections::HashSet<TagField> = std::collections::HashSet::new();
    for rev in log.current().into_iter().chain(older.iter().rev()) {
        let rev_tags = revision_tags(rev, track_id);
        let fields: std::collections::HashSet<TagField> =
            rev_tags.iter().map(TagField::of).collect();
        tags.extend(
            rev_tags
                .into_iter()
                .filter(|t| !seen.contains(&TagField::of(t))),
        );
        seen.extend(fields);
    }

    let mut pictures = Vec::new();
    if want_pictures {
        if let Some(rev) = log.current() {
            push_pictures(rev.media.visuals.clone(), &mut pictures);
        }
        for rev in older {
            push_pictures(rev.media.visuals, &mut pictures);
        }
    }

    (tags, pictures)
}

/// Identity of a tag field for merging revisions: the standard tag kind, or
/// the lowercase raw key of an unparsed tag.
#[derive(PartialEq, Eq, Hash)]
enum TagField {
    Std(std::mem::Discriminant<StandardTag>),
    Raw(String),
}

impl TagField {
    fn of(tag: &SymTag) -> Self {
        match &tag.std {
            Some(std) => Self::Std(std::mem::discriminant(std)),
            None => Self::Raw(tag.raw.key.to_lowercase()),
        }
    }
}

fn revision_tags(rev: &MetadataRevision, track_id: Option<u64>) -> Vec<SymTag> {
    let mut tags = rev.media.tags.clone();
    if let Some(id) = track_id {
        for pt in &rev.per_track {
            if pt.track_id == id {
                tags.extend(pt.metadata.tags.iter().cloned());
            }
        }
    }
    tags
}

fn push_pictures(visuals: Vec<Visual>, out: &mut Vec<Picture>) {
    for v in visuals {
        let mime_type = v
            .media_type
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| infer_mime(&v.data).to_owned());
        out.push(Picture {
            mime_type,
            is_front_cover: matches!(v.usage, Some(StandardVisualKey::FrontCover)),
            data: Vec::from(v.data),
        });
    }
}

/// Sniff an image's MIME type from its magic bytes, for pictures whose
/// container didn't record one.
pub(crate) fn infer_mime(data: &[u8]) -> &'static str {
    if data.starts_with(b"\xFF\xD8\xFF") {
        "image/jpeg"
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if data.starts_with(b"GIF8") {
        "image/gif"
    } else if data.len() > 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "application/octet-stream"
    }
}

/// Parse a ReplayGain value string like "-6.5 dB" or "-6.5" into an f32.
fn parse_replay_gain(s: &str) -> Option<f32> {
    s.trim()
        .trim_end_matches(" dB")
        .trim_end_matches(" db")
        .trim()
        .parse::<f32>()
        .ok()
}

/// Maps the raw `symphonia` tags collected by [`drain_metadata`] into
/// [`AudioTags`]'s named fields.
///
/// Single-valued fields (title/album/album artist/genre/lyrics) keep the
/// *first* occurrence, matching the previous tag reader's
/// `tag.title()`/`tag.get_string(...)` (which returned the first matching
/// item). `artist` is the one exception — see [`AudioTags::artist`].
fn apply_tags(raw: &[SymTag], out: &mut AudioTags) {
    let mut artists: Vec<&str> = Vec::new();
    // Date and year tags by precedence: recording, then release (APEv2 `Year`
    // and ID3 `TDRL` map here), then original release.
    let mut dates: [Option<&str>; 3] = [None; 3];
    let mut years: [Option<u16>; 3] = [None; 3];

    for tag in raw {
        let Some(std) = tag.std.as_ref() else {
            // RIFF INFO writers (ffmpeg, ...) store the track as `3/12`,
            // which symphonia's RIFF map rejects as a plain number; the raw
            // tag survives, so read the track number and total from it.
            if matches!(tag.raw.key.to_ascii_uppercase().as_str(), "ITRK" | "IPRT")
                && let RawValue::String(v) = &tag.raw.value
                && let Some((number, total)) = v.split_once('/')
            {
                if let Ok(n) = number.trim().parse::<u32>() {
                    out.track_number.get_or_insert(n);
                }
                if let Ok(t) = total.trim().parse::<u32>() {
                    out.track_total.get_or_insert(t);
                }
            }
            continue;
        };
        match std {
            StandardTag::TrackTitle(v) => out.title.get_or_insert_with(|| v.to_string()),
            StandardTag::AlbumArtist(v) => out.album_artist.get_or_insert_with(|| v.to_string()),
            StandardTag::Album(v) => out.album.get_or_insert_with(|| v.to_string()),
            StandardTag::Genre(v) => out.genre.get_or_insert_with(|| v.to_string()),
            StandardTag::Lyrics(v) => out.lyrics.get_or_insert_with(|| v.to_string()),
            StandardTag::Artist(v) => {
                artists.push(v.as_str());
                continue;
            }
            StandardTag::TrackNumber(n) => {
                out.track_number.get_or_insert(*n as u32);
                continue;
            }
            StandardTag::TrackTotal(n) => {
                out.track_total.get_or_insert(*n as u32);
                continue;
            }
            StandardTag::DiscNumber(n) => {
                out.disc_number.get_or_insert(*n as u32);
                continue;
            }
            StandardTag::DiscTotal(n) => {
                out.disc_total.get_or_insert(*n as u32);
                continue;
            }
            StandardTag::RecordingDate(v) if !v.is_empty() => {
                dates[0].get_or_insert(v.as_str());
                continue;
            }
            StandardTag::RecordingYear(y) => {
                years[0].get_or_insert(*y);
                continue;
            }
            StandardTag::ReleaseDate(v) if !v.is_empty() => {
                dates[1].get_or_insert(v.as_str());
                continue;
            }
            StandardTag::ReleaseYear(y) => {
                years[1].get_or_insert(*y);
                continue;
            }
            StandardTag::OriginalReleaseDate(v) | StandardTag::OriginalRecordingDate(v)
                if !v.is_empty() =>
            {
                dates[2].get_or_insert(v.as_str());
                continue;
            }
            StandardTag::OriginalReleaseYear(y) | StandardTag::OriginalRecordingYear(y) => {
                years[2].get_or_insert(*y);
                continue;
            }
            StandardTag::MusicBrainzReleaseType(v) => {
                let v = v.trim().to_lowercase();
                if !v.is_empty() && !out.release_type.split("; ").any(|t| t == v) {
                    if !out.release_type.is_empty() {
                        out.release_type.push_str("; ");
                    }
                    out.release_type.push_str(&v);
                }
                continue;
            }
            StandardTag::CompilationFlag(flag) => {
                if *flag {
                    out.compilation = true;
                }
                continue;
            }
            StandardTag::ReplayGainTrackGain(v) => {
                if out.rg_track_gain.is_none() {
                    out.rg_track_gain = parse_replay_gain(v);
                }
                continue;
            }
            StandardTag::ReplayGainAlbumGain(v) => {
                if out.rg_album_gain.is_none() {
                    out.rg_album_gain = parse_replay_gain(v);
                }
                continue;
            }
            _ => continue,
        };
    }

    if !artists.is_empty() {
        out.artist = Some(artists.join("; "));
    }

    if let Some((date, year)) = dates
        .into_iter()
        .zip(years)
        .find(|(date, year)| date.is_some() || year.is_some())
    {
        out.date = date
            .map(str::to_string)
            .or_else(|| year.map(|y| y.to_string()));
        out.year = date
            .and_then(|d| d.get(0..4))
            .and_then(|y| y.parse::<u32>().ok())
            .or_else(|| year.map(u32::from));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mono 8 kHz 16-bit WAV with a RIFF `LIST`/`INFO` chunk of `entries`.
    fn wav_with_info(entries: &[(&[u8; 4], &str)]) -> Vec<u8> {
        let mut info = b"INFO".to_vec();
        for (id, value) in entries {
            let mut text = value.as_bytes().to_vec();
            text.push(0);
            if text.len() % 2 == 1 {
                text.push(0);
            }
            info.extend_from_slice(*id);
            info.extend_from_slice(&(text.len() as u32).to_le_bytes());
            info.extend_from_slice(&text);
        }
        let mut body = b"WAVEfmt ".to_vec();
        body.extend_from_slice(&16u32.to_le_bytes());
        for field in [1u16, 1] {
            body.extend_from_slice(&field.to_le_bytes());
        }
        body.extend_from_slice(&8000u32.to_le_bytes());
        body.extend_from_slice(&16000u32.to_le_bytes());
        for field in [2u16, 16] {
            body.extend_from_slice(&field.to_le_bytes());
        }
        body.extend_from_slice(b"data");
        body.extend_from_slice(&8u32.to_le_bytes());
        body.extend_from_slice(&[0; 8]);
        body.extend_from_slice(b"LIST");
        body.extend_from_slice(&(info.len() as u32).to_le_bytes());
        body.extend_from_slice(&info);
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&(body.len() as u32).to_le_bytes());
        wav.extend_from_slice(&body);
        wav
    }

    #[test]
    fn riff_info_track_of_total_is_split() {
        let path = std::env::temp_dir().join(format!("aulos-riff-itrk-{}.wav", std::process::id()));
        fs::write(&path, wav_with_info(&[(b"INAM", "T"), (b"ITRK", "3/12")])).unwrap();
        let tags = probe(&path, false).expect("probe wav").tags;
        let _ = fs::remove_file(&path);
        assert_eq!(tags.title.as_deref(), Some("T"));
        assert_eq!((tags.track_number, tags.track_total), (Some(3), Some(12)));
    }

    fn std_tag(std: StandardTag) -> SymTag {
        SymTag::new_from_parts("k", "v", Some(std))
    }

    fn applied(tags: &[SymTag]) -> AudioTags {
        let mut out = AudioTags::default();
        apply_tags(tags, &mut out);
        out
    }

    #[test]
    fn year_falls_back_from_recording_to_release_to_original() {
        use std::sync::Arc;
        let date = |s: &str| Arc::new(s.to_string());
        // APEv2 `Year` (Musepack, WavPack, TTA, Monkey's) is a release date.
        let ape = applied(&[std_tag(StandardTag::ReleaseDate(date("1986")))]);
        assert_eq!((ape.date.as_deref(), ape.year), (Some("1986"), Some(1986)));
        // Recording beats release beats original, whatever the tag order.
        let all = applied(&[
            std_tag(StandardTag::OriginalReleaseDate(date("1970-01-01"))),
            std_tag(StandardTag::ReleaseDate(date("1999-05-05"))),
            std_tag(StandardTag::RecordingDate(date("2001-02-03"))),
        ]);
        assert_eq!(
            (all.date.as_deref(), all.year),
            (Some("2001-02-03"), Some(2001))
        );
        let no_recording = applied(&[
            std_tag(StandardTag::OriginalReleaseYear(1970)),
            std_tag(StandardTag::ReleaseYear(1999)),
        ]);
        assert_eq!(
            (no_recording.date.as_deref(), no_recording.year),
            (Some("1999"), Some(1999))
        );
        let original = applied(&[std_tag(StandardTag::OriginalReleaseYear(1970))]);
        assert_eq!(original.year, Some(1970));
        assert_eq!(applied(&[]).year, None);
    }

    /// A file probed from memory (the browser's way) reads like one probed
    /// from disk, whether it was loaded whole or in two overlapping chunks.
    #[test]
    fn probing_sparse_memory_matches_probing_the_file() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagio");
        for entry in std::fs::read_dir(dir).expect("fixtures") {
            let path = entry.expect("entry").path();
            let bytes = std::fs::read(&path).expect("read");
            let expected = format!("{:?}", probe(&path, true));
            let whole = SparseFile::whole(bytes.clone());
            assert_eq!(
                format!("{:?}", probe_sparse(&path, whole, true)),
                expected,
                "{}",
                path.display()
            );
            let mid = bytes.len() / 2;
            let mut chunks = SparseFile::new(bytes.len() as u64);
            chunks.insert(0, bytes[..=mid].to_vec());
            chunks.insert(mid as u64, bytes[mid..].to_vec());
            assert!(chunks.is_complete());
            assert_eq!(
                format!("{:?}", probe_sparse(&path, chunks, true)),
                expected,
                "{}",
                path.display()
            );
        }
    }

    /// MPEG-1 layer 3 frames between a leading ID3v2.4 tag with only `TIT2`,
    /// and an ID3v1 tag with a (30-char truncated) title and an artist.
    fn mp3_with_id3v2_and_id3v1(title: &str) -> Vec<u8> {
        let mut frame = b"TIT2".to_vec();
        frame.extend_from_slice(&(title.len() as u32 + 1).to_be_bytes()); // < 128: syncsafe
        frame.extend_from_slice(&[0, 0, 3]);
        frame.extend_from_slice(title.as_bytes());

        let mut v = b"ID3\x04\x00\x00".to_vec();
        v.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        v.extend_from_slice(&frame);
        for _ in 0..32 {
            v.extend_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
            v.extend_from_slice(&[0u8; 413]);
        }

        let mut v1 = [0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        v1[3..33].copy_from_slice(&title.as_bytes()[..30]);
        v1[33..44].copy_from_slice(b"ID3v1 Artst");
        v1[127] = 255;
        v.extend_from_slice(&v1);
        v
    }

    #[test]
    fn newer_tag_wins_per_field_and_older_fills_the_rest() {
        let title = "Leading ID3v2 title longer than thirty characters";
        let path = std::env::temp_dir().join(format!("aulos-tag-merge-{}.mp3", std::process::id()));
        fs::write(&path, mp3_with_id3v2_and_id3v1(title)).unwrap();
        let probed = probe(&path, false);
        let _ = fs::remove_file(&path);
        let tags = probed.expect("probe mp3").tags;

        assert_eq!(tags.title.as_deref(), Some(title));
        assert_eq!(tags.artist.as_deref(), Some("ID3v1 Artst"));
    }

    /// The scanner's stored bitrate and the Properties drawer's must come from
    /// the same payload (file minus ID3v2/ID3v1) so the two never disagree.
    #[test]
    fn scanner_bitrate_matches_tech_info_payload_bitrate() {
        let title = "Bitrate agreement title longer than thirty chars";
        let path = std::env::temp_dir().join(format!("aulos-tag-rate-{}.mp3", std::process::id()));
        fs::write(&path, mp3_with_id3v2_and_id3v1(title)).unwrap();
        let probed = probe(&path, false).expect("probe mp3");
        let info = crate::library::tech_info::probe(&path);
        let _ = fs::remove_file(&path);
        let info = info.expect("tech_info probe");

        // 32 frames of 417 bytes; the ID3v2 and ID3v1 tags are not payload.
        assert_eq!(info.payload_bytes, Some(32 * 417));
        assert_eq!(info.tag_formats, ["ID3v2.4", "ID3v1"]);
        assert!(probed.properties.duration > Duration::ZERO);
        assert_eq!(Some(probed.properties.bitrate), info.bitrate_kbps);
        assert_eq!(
            Some(probed.properties.bitrate),
            crate::library::tech_info::bitrate_kbps(32 * 417, probed.properties.duration)
        );
    }
}
