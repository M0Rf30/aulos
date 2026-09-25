// SPDX-License-Identifier: GPL-3.0

//! Shared Symphonia-based tag/audio-property reading, used by the library
//! scanner, cover art extractor, lyrics provider, and the file converter's
//! tag copier. Replaces the previous tag-reading library (now removed).
//!
//! Ports the probe/tag-mapping approach from the sibling `rmpd` project's
//! `rmpd-library::metadata` (same M0Rf30 Symphonia fork this crate already
//! depends on for playback, rev `2c160a8`), trimmed to the fields lyra
//! actually surfaces (`crate::library::Track` has no bit-depth/channel
//! fields — those are read independently by
//! [`crate::convert::pipeline::AudioSource`] for the converter's own
//! needs). Symphonia is read-only: writing tags into the file converter's
//! output is handled separately by `crate::convert::tag_writer`.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, MediaInfo, Track, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{
    Metadata, MetadataOptions, MetadataReader, MetadataRevision, StandardTag, StandardVisualKey,
    Tag as SymTag, Visual,
};
use symphonia::core::units::Duration as SymDuration;
use symphonia::default::meta::Id3v2Reader;

/// An embedded picture (cover art), as extracted from a tag.
#[derive(Debug, Clone)]
pub struct Picture {
    pub mime_type: String,
    pub is_front_cover: bool,
    pub data: Vec<u8>,
}

/// Tag fields lyra's library/converter care about. Every field is `None`
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
    /// or `"2024-05-01"`), preserved for `crate::convert::tag_writer`.
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
    pub pictures: Vec<Picture>,
}

/// Audio properties independent of tags. Fields default to `0` when
/// symphonia can't determine them, matching the previous tag reader's
/// `unwrap_or(0)` fallbacks.
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioProperties {
    pub duration: Duration,
    pub sample_rate: u32,
    /// Derived from the audio-bitstream portion of the file (file size
    /// minus embedded-artwork bytes) over duration, in kbps — symphonia
    /// exposes no bitrate field directly.
    pub bitrate: u32,
}

/// Result of probing one file: its tags and audio properties.
#[derive(Debug, Clone, Default)]
pub struct ProbedFile {
    pub tags: AudioTags,
    pub properties: AudioProperties,
}

/// Probes `path` and reads everything lyra needs from its tags and default
/// audio track. `want_pictures` gates collecting embedded picture bytes —
/// pass `false` when only text tags are needed (the scanner, lyrics
/// provider), since cloning artwork bytes is wasted work otherwise.
/// Returns `None` if the file can't be opened or probed.
pub fn probe(path: &Path, want_pictures: bool) -> Option<ProbedFile> {
    let file = fs::File::open(path).ok()?;
    let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;

    let (duration, sample_rate, track_id) = {
        let track = reader.default_track(TrackType::Audio);
        let audio = track.and_then(|t| match t.codec_params.as_ref() {
            Some(CodecParameters::Audio(a)) => Some(a),
            _ => None,
        });
        let sample_rate = audio.and_then(|a| a.sample_rate);
        let duration = track_duration(track, reader.media_info());
        let track_id = track.map(|t| u64::from(t.id));
        (duration, sample_rate, track_id)
    };

    let (mut raw_tags, mut pictures, mut visual_bytes) =
        drain_metadata(&mut reader.metadata(), want_pictures, track_id);

    // WAV-only fallback: see `wav_id3_fallback`'s docs for why this
    // fork's own WAV demuxer never surfaces an `id3 ` chunk's tags.
    // Cheap no-op for every other container (an immediate magic-byte
    // mismatch inside `find_wav_id3_chunk`).
    if let Some((id3_tags, id3_pictures, id3_visual_bytes)) = wav_id3_fallback(path, want_pictures)
    {
        // Prepended so ID3 wins per-field over whatever the container's
        // own revision (RIFF INFO) provided — `apply_tags` folds a tag
        // list by keeping each field's *first* occurrence.
        raw_tags = id3_tags.into_iter().chain(raw_tags).collect();
        pictures = id3_pictures.into_iter().chain(pictures).collect();
        visual_bytes += id3_visual_bytes;
    }

    let bitrate = duration.filter(|d| d.as_secs_f64() > 0.0).map(|d| {
        let audio_bytes = file_size.saturating_sub(visual_bytes);
        ((audio_bytes as f64 * 8.0) / d.as_secs_f64() / 1000.0) as u32
    });

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
            bitrate: bitrate.unwrap_or(0),
        },
    })
}

/// For a RIFF/WAVE file, looks for an `id3 ` (or `ID3 `) top-level chunk
/// and parses it with Symphonia's own [`Id3v2Reader`] — the fork's WAV
/// demuxer (`symphonia-format-riff`) recognizes only `fmt `/`LIST`/`fact`/
/// `data` as chunk types and returns as soon as it parses `data` (see its
/// `WavReader::try_new`), so a trailing `id3 ` chunk — where
/// Mp3tag/foobar2000/`crate::convert::tag_writer` all place it — is
/// otherwise silently skipped as an unknown RIFF chunk and its tags never
/// reach a `MetadataRevision` at all.
///
/// Returns `None` for anything that isn't a RIFF/WAVE file, has no `id3 `
/// chunk, or whose ID3 payload doesn't parse.
fn wav_id3_fallback(path: &Path, want_pictures: bool) -> Option<(Vec<SymTag>, Vec<Picture>, u64)> {
    let mut file = fs::File::open(path).ok()?;
    let id3_bytes = find_wav_id3_chunk(&mut file)?;

    let cursor = io::Cursor::new(id3_bytes);
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let mut reader = Id3v2Reader::try_new(mss, MetadataOptions::default()).ok()?;
    let rev = reader.read_all().ok()?.revision;

    let visual_bytes = rev.media.visuals.iter().map(|v| v.data.len() as u64).sum();
    let mut pictures = Vec::new();
    if want_pictures {
        push_pictures(rev.media.visuals, &mut pictures);
    }
    Some((rev.media.tags, pictures, visual_bytes))
}

/// Finds a top-level `id3 `/`ID3 ` RIFF chunk's raw payload bytes in a
/// RIFF/WAVE file. Walks the chunk chain by seeking past each chunk's
/// declared length (plus a pad byte for odd lengths) rather than reading
/// payloads into memory — `data` in particular can be the entire rest of
/// a large file, and is never read here just to skip past it.
fn find_wav_id3_chunk(file: &mut fs::File) -> Option<Vec<u8>> {
    let file_len = file.metadata().ok()?.len();

    file.seek(SeekFrom::Start(0)).ok()?;
    let mut riff_header = [0u8; 12];
    file.read_exact(&mut riff_header).ok()?;
    if &riff_header[0..4] != b"RIFF" || &riff_header[8..12] != b"WAVE" {
        return None;
    }
    let riff_size = u32::from_le_bytes(riff_header[4..8].try_into().ok()?) as u64;
    let end = 8u64.checked_add(riff_size)?.min(file_len);

    let mut pos: u64 = 12;
    loop {
        if pos.checked_add(8)? > end {
            return None;
        }
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut chunk_header = [0u8; 8];
        file.read_exact(&mut chunk_header).ok()?;
        let chunk_id = &chunk_header[0..4];
        let chunk_len = u32::from_le_bytes(chunk_header[4..8].try_into().ok()?) as u64;
        let data_start = pos.checked_add(8)?;

        if chunk_id.eq_ignore_ascii_case(b"id3 ") {
            let mut buf = vec![0u8; chunk_len as usize];
            file.seek(SeekFrom::Start(data_start)).ok()?;
            file.read_exact(&mut buf).ok()?;
            return Some(buf);
        }

        let padded_len = chunk_len.checked_add(chunk_len & 1)?;
        pos = data_start.checked_add(padded_len)?;
    }
}

/// Compute a track's duration, falling back through every level of
/// precision symphonia exposes: the track's own frame count, then its
/// declared duration in timebase units, then the reader's overall media
/// duration (the only value some demuxers ever populate).
fn track_duration(track: Option<&Track>, media_info: &MediaInfo) -> Option<Duration> {
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

/// Drain the metadata log into `(tags, pictures, visual_byte_total)`.
///
/// `tags` comes from exactly one revision: the newest one that actually
/// has tags (checking the default audio track's per-track tags too, since
/// e.g. Matroska routes track-targeted tag elements there instead of to
/// the media-level tag list). Symphonia probes standalone leading/trailing
/// tag readers (e.g. ID3v1) before the container's own reader runs, so
/// merging every revision would let a stale/truncated tag block shadow, or
/// duplicate, the real one.
///
/// `visual_byte_total` is summed from every revision regardless of
/// `want_pictures` (only picture lengths are read, never copied); the
/// `pictures` list itself is only populated — cloning picture bytes —
/// when a caller actually wants them.
fn drain_metadata(
    log: &mut Metadata<'_>,
    want_pictures: bool,
    track_id: Option<u64>,
) -> (Vec<SymTag>, Vec<Picture>, u64) {
    let mut older: Vec<MetadataRevision> = Vec::new();
    while let Some(discarded) = log.pop() {
        older.push(discarded);
    }

    let mut visual_bytes: u64 = older.iter().map(revision_visual_bytes).sum();
    if let Some(rev) = log.current() {
        visual_bytes += revision_visual_bytes(rev);
    }

    let tags = match log.current() {
        Some(rev) if revision_has_tags(rev, track_id) => revision_tags(rev, track_id),
        _ => older
            .iter()
            .rev()
            .find(|r| revision_has_tags(r, track_id))
            .map(|r| revision_tags(r, track_id))
            .unwrap_or_default(),
    };

    let mut pictures = Vec::new();
    if want_pictures {
        if let Some(rev) = log.current() {
            push_pictures(rev.media.visuals.clone(), &mut pictures);
        }
        for rev in older {
            push_pictures(rev.media.visuals, &mut pictures);
        }
    }

    (tags, pictures, visual_bytes)
}

fn revision_visual_bytes(rev: &MetadataRevision) -> u64 {
    rev.media.visuals.iter().map(|v| v.data.len() as u64).sum()
}

fn revision_has_tags(rev: &MetadataRevision, track_id: Option<u64>) -> bool {
    !rev.media.tags.is_empty()
        || track_id.is_some_and(|id| {
            rev.per_track
                .iter()
                .any(|pt| pt.track_id == id && !pt.metadata.tags.is_empty())
        })
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
    let mut recording_date: Option<&str> = None;
    let mut recording_year: Option<u16> = None;

    for tag in raw {
        let Some(std) = tag.std.as_ref() else {
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
                recording_date.get_or_insert(v.as_str());
                continue;
            }
            StandardTag::RecordingYear(y) => {
                recording_year.get_or_insert(*y);
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

    out.date = recording_date
        .map(str::to_string)
        .or_else(|| recording_year.map(|y| y.to_string()));
    out.year = recording_date
        .and_then(|d| d.get(0..4))
        .and_then(|y| y.parse::<u32>().ok())
        .or_else(|| recording_year.map(u32::from));
}
