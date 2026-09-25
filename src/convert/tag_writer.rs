// SPDX-License-Identifier: GPL-3.0

//! Minimal, best-effort tag writers for the two output containers the file
//! converter can produce (FLAC via `flacenc`, WAV via `hound`) — both of
//! which encode audio only and never write tags. Symphonia itself is
//! read-only (see `crate::library::tags`), so this hand-rolls just enough
//! of each container's native tag format to write back what
//! `crate::convert::pipeline` copies from the source file: title, artist,
//! album, genre, date, track/disc numbers, and a single front-cover
//! picture.
//!
//! FLAC gets a native VORBIS_COMMENT + PICTURE metadata block pair,
//! spliced in right after the existing metadata block chain (in practice
//! just STREAMINFO, since [`super::encoder::FlacSink`] never writes any
//! other block), fixing up the last-metadata-block flag so the chain stays
//! valid. WAV gets an `id3 ` RIFF chunk holding a minimal ID3v2.4 tag (text
//! frames + an APIC frame) — the conventional on-disk shape most tag
//! libraries write for WAV tagging. `crate::library::tags` has a matching
//! WAV-specific read-side fallback for this chunk, since the Symphonia
//! fork's own WAV demuxer never surfaces it.
//!
//! Both writers are best-effort: any I/O error or unexpected byte layout
//! (i.e. the file isn't what our own encoders just wrote) is logged and
//! the file is left untouched, matching `pipeline`'s existing "tagging
//! never fails the job" contract.

use std::io;
use std::path::Path;

use crate::library::tags::Picture;

/// Tags to write into a freshly-encoded output file. Every field is
/// optional — a missing one is simply omitted from the written tag.
#[derive(Debug, Clone, Default)]
pub struct WriteTags<'a> {
    pub title: Option<&'a str>,
    pub artist: Option<&'a str>,
    pub album: Option<&'a str>,
    pub genre: Option<&'a str>,
    pub date: Option<&'a str>,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_number: Option<u32>,
    pub picture: Option<&'a Picture>,
}

impl WriteTags<'_> {
    fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.genre.is_none()
            && self.date.is_none()
            && self.track_number.is_none()
            && self.track_total.is_none()
            && self.disc_number.is_none()
            && self.picture.is_none()
    }
}

/// Writes `tags` into `path`'s existing FLAC file, best-effort.
pub fn write_flac_tags(path: &Path, tags: &WriteTags<'_>) {
    if tags.is_empty() {
        return;
    }
    if let Err(e) = try_write_flac_tags(path, tags) {
        tracing::warn!("failed to write FLAC tags to {}: {e}", path.display());
    }
}

/// Writes `tags` into `path`'s existing WAV file, best-effort.
pub fn write_wav_tags(path: &Path, tags: &WriteTags<'_>) {
    if tags.is_empty() {
        return;
    }
    if let Err(e) = try_write_wav_tags(path, tags) {
        tracing::warn!("failed to write WAV tags to {}: {e}", path.display());
    }
}

fn metadata_block_header(block_type: u8, is_last: bool, len: usize) -> [u8; 4] {
    let flag = if is_last { 0x80 } else { 0x00 };
    let len_bytes = (len as u32).to_be_bytes();
    [block_type | flag, len_bytes[1], len_bytes[2], len_bytes[3]]
}

/// Builds a FLAC `VORBIS_COMMENT` metadata block body (vendor string +
/// `KEY=value` comments, little-endian length prefixes throughout — the
/// native FLAC embedding, unlike Ogg's framed variant).
fn build_vorbis_comment_block(tags: &WriteTags<'_>, is_last: bool) -> Vec<u8> {
    let vendor = concat!("lyra ", env!("CARGO_PKG_VERSION"));
    let mut comments: Vec<String> = Vec::new();
    if let Some(v) = tags.title {
        comments.push(format!("TITLE={v}"));
    }
    if let Some(v) = tags.artist {
        comments.push(format!("ARTIST={v}"));
    }
    if let Some(v) = tags.album {
        comments.push(format!("ALBUM={v}"));
    }
    if let Some(v) = tags.genre {
        comments.push(format!("GENRE={v}"));
    }
    if let Some(v) = tags.date {
        comments.push(format!("DATE={v}"));
    }
    if let Some(v) = tags.track_number {
        comments.push(format!("TRACKNUMBER={v}"));
    }
    if let Some(v) = tags.track_total {
        comments.push(format!("TRACKTOTAL={v}"));
    }
    if let Some(v) = tags.disc_number {
        comments.push(format!("DISCNUMBER={v}"));
    }

    let mut body = Vec::new();
    body.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    body.extend_from_slice(vendor.as_bytes());
    body.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in &comments {
        body.extend_from_slice(&(c.len() as u32).to_le_bytes());
        body.extend_from_slice(c.as_bytes());
    }

    let mut block = Vec::with_capacity(4 + body.len());
    block.extend_from_slice(&metadata_block_header(4, is_last, body.len()));
    block.extend_from_slice(&body);
    block
}

/// Builds a FLAC `PICTURE` metadata block body. Width/height/color-depth/
/// colors-used are all written as `0` ("unknown") — symphonia's reader
/// (like most others) treats a zero width/height as "no declared
/// dimensions" and sniffs the image itself for anything it needs, so this
/// never has to decode the picture just to fill in a hint field.
fn build_picture_block(pic: &Picture, is_last: bool) -> Vec<u8> {
    const PICTURE_TYPE_FRONT_COVER: u32 = 3;

    let mut body = Vec::new();
    body.extend_from_slice(&PICTURE_TYPE_FRONT_COVER.to_be_bytes());
    body.extend_from_slice(&(pic.mime_type.len() as u32).to_be_bytes());
    body.extend_from_slice(pic.mime_type.as_bytes());
    body.extend_from_slice(&0u32.to_be_bytes()); // description length (none)
    body.extend_from_slice(&0u32.to_be_bytes()); // width (unknown)
    body.extend_from_slice(&0u32.to_be_bytes()); // height (unknown)
    body.extend_from_slice(&0u32.to_be_bytes()); // color depth (unknown)
    body.extend_from_slice(&0u32.to_be_bytes()); // colors used (0 = not indexed)
    body.extend_from_slice(&(pic.data.len() as u32).to_be_bytes());
    body.extend_from_slice(&pic.data);

    let mut block = Vec::with_capacity(4 + body.len());
    block.extend_from_slice(&metadata_block_header(6, is_last, body.len()));
    block.extend_from_slice(&body);
    block
}

/// Splices a VORBIS_COMMENT (and, if present, a PICTURE) metadata block
/// into `path`'s FLAC metadata block chain, right after whatever's
/// already there.
fn try_write_flac_tags(path: &Path, tags: &WriteTags<'_>) -> io::Result<()> {
    let mut bytes = std::fs::read(path)?;
    if bytes.len() < 4 || &bytes[0..4] != b"fLaC" {
        return Err(io::Error::other("not a FLAC file"));
    }

    // Walk the existing metadata block chain to find where the audio
    // frames start, remembering the position of the (outgoing) last
    // block's header so its `is_last` flag can be cleared — our new
    // block(s) will follow it.
    let mut pos = 4usize;
    let mut last_header_pos;
    loop {
        if pos + 4 > bytes.len() {
            return Err(io::Error::other("truncated FLAC metadata block chain"));
        }
        let header = bytes[pos];
        let is_last = header & 0x80 != 0;
        let block_len =
            u32::from_be_bytes([0, bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
        last_header_pos = pos;
        pos = pos
            .checked_add(4 + block_len)
            .ok_or_else(|| io::Error::other("FLAC metadata block length overflow"))?;
        if is_last {
            break;
        }
    }
    let audio_start = pos;
    bytes[last_header_pos] &= 0x7F;

    let mut new_blocks = build_vorbis_comment_block(tags, tags.picture.is_none());
    if let Some(pic) = tags.picture {
        new_blocks.extend(build_picture_block(pic, true));
    }

    let mut out = Vec::with_capacity(bytes.len() + new_blocks.len());
    out.extend_from_slice(&bytes[..audio_start]);
    out.extend_from_slice(&new_blocks);
    out.extend_from_slice(&bytes[audio_start..]);

    std::fs::write(path, out)
}

/// Encodes `n` as a 28-bit ID3v2.4 "syncsafe" integer (each of the 4 bytes
/// holds 7 bits, high bit always clear) — used for both the tag header
/// size and every frame's size in v2.4.
fn syncsafe28(n: u32) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

fn id3_frame(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(10 + data.len());
    f.extend_from_slice(id);
    f.extend_from_slice(&syncsafe28(data.len() as u32));
    f.extend_from_slice(&[0, 0]); // frame flags
    f.extend_from_slice(data);
    f
}

fn id3_text_frame(id: &[u8; 4], text: &str) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + text.len());
    data.push(0x03); // encoding: UTF-8
    data.extend_from_slice(text.as_bytes());
    id3_frame(id, &data)
}

/// Builds an `APIC` (attached picture) frame body: ISO-8859-1 encoding
/// byte, a null-terminated MIME type (always Latin-1 per spec regardless
/// of the frame's text encoding byte), the picture type, an empty
/// null-terminated description, then the raw picture bytes.
fn id3_apic_frame(pic: &Picture) -> Vec<u8> {
    const PICTURE_TYPE_FRONT_COVER: u8 = 3;

    let mut data = Vec::with_capacity(4 + pic.mime_type.len() + pic.data.len());
    data.push(0x00); // encoding: ISO-8859-1
    data.extend_from_slice(pic.mime_type.as_bytes());
    data.push(0x00);
    data.push(PICTURE_TYPE_FRONT_COVER);
    data.push(0x00); // empty description, ISO-8859-1 terminator
    data.extend_from_slice(&pic.data);
    id3_frame(b"APIC", &data)
}

/// Builds a full ID3v2.4 tag (header + frames) from `tags`.
fn build_id3v2_tag(tags: &WriteTags<'_>) -> Vec<u8> {
    let mut frames = Vec::new();
    if let Some(v) = tags.title {
        frames.extend(id3_text_frame(b"TIT2", v));
    }
    if let Some(v) = tags.artist {
        frames.extend(id3_text_frame(b"TPE1", v));
    }
    if let Some(v) = tags.album {
        frames.extend(id3_text_frame(b"TALB", v));
    }
    if let Some(v) = tags.genre {
        frames.extend(id3_text_frame(b"TCON", v));
    }
    if let Some(v) = tags.date {
        frames.extend(id3_text_frame(b"TDRC", v));
    }
    if let Some(n) = tags.track_number {
        let text = match tags.track_total {
            Some(total) => format!("{n}/{total}"),
            None => n.to_string(),
        };
        frames.extend(id3_text_frame(b"TRCK", &text));
    }
    if let Some(n) = tags.disc_number {
        frames.extend(id3_text_frame(b"TPOS", &n.to_string()));
    }
    if let Some(pic) = tags.picture {
        frames.extend(id3_apic_frame(pic));
    }

    let mut tag = Vec::with_capacity(10 + frames.len());
    tag.extend_from_slice(b"ID3");
    tag.extend_from_slice(&[0x04, 0x00]); // version 2.4.0
    tag.push(0x00); // flags
    tag.extend_from_slice(&syncsafe28(frames.len() as u32));
    tag.extend_from_slice(&frames);
    tag
}

/// Appends an `id3 ` RIFF chunk (a raw ID3v2.4 tag) to `path`'s WAV file
/// and grows the RIFF header's total-size field to match. Chunks are
/// unordered in RIFF, so appending after `data` is exactly as valid as any
/// other placement — no need to parse the existing chunk list at all.
fn try_write_wav_tags(path: &Path, tags: &WriteTags<'_>) -> io::Result<()> {
    let mut bytes = std::fs::read(path)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(io::Error::other("not a RIFF/WAVE file"));
    }

    let id3 = build_id3v2_tag(tags);

    let mut chunk = Vec::with_capacity(8 + id3.len() + 1);
    chunk.extend_from_slice(b"id3 ");
    chunk.extend_from_slice(&(id3.len() as u32).to_le_bytes());
    chunk.extend_from_slice(&id3);
    if id3.len() % 2 == 1 {
        // RIFF chunks are word-aligned: an odd-length chunk gets one pad
        // byte after its data, not counted in the chunk's own size field.
        chunk.push(0);
    }

    let old_riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let new_riff_size = old_riff_size
        .checked_add(chunk.len() as u32)
        .ok_or_else(|| io::Error::other("RIFF size overflow"))?;
    bytes[4..8].copy_from_slice(&new_riff_size.to_le_bytes());

    bytes.extend_from_slice(&chunk);
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a minimal valid FLAC file (just STREAMINFO, no audio frames
    /// needed since we only exercise the metadata splice) the same way
    /// `super::encoder::FlacSink::finish` does: a bare `fLaC` stream with
    /// one `is_last` STREAMINFO block.
    fn minimal_flac_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fLaC");
        // STREAMINFO block, marked last, 34-byte body of zeros (values
        // don't matter — the splice logic never inspects them).
        bytes.push(0x80); // type 0, is_last
        bytes.extend_from_slice(&34u32.to_be_bytes()[1..]);
        bytes.extend_from_slice(&[0u8; 34]);
        // A fake "audio frame" tail so the splice can be checked to
        // preserve exactly the bytes after the metadata chain.
        bytes.extend_from_slice(b"FAKEAUDIODATA");
        bytes
    }

    #[test]
    fn flac_splice_clears_last_flag_and_preserves_audio_tail() {
        let dir =
            std::env::temp_dir().join(format!("lyra-tagwriter-flac-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.flac");
        std::fs::write(&path, minimal_flac_bytes()).unwrap();

        let pic = Picture {
            mime_type: "image/jpeg".to_string(),
            is_front_cover: true,
            data: vec![1, 2, 3, 4],
        };
        write_flac_tags(
            &path,
            &WriteTags {
                title: Some("Test Title"),
                picture: Some(&pic),
                ..Default::default()
            },
        );

        let out = std::fs::read(&path).unwrap();
        assert!(
            out.ends_with(b"FAKEAUDIODATA"),
            "audio tail must survive the splice untouched"
        );

        // STREAMINFO's is_last flag must now be cleared.
        assert_eq!(
            out[4] & 0x80,
            0,
            "STREAMINFO must no longer be the last block"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Encodes a tiny real FLAC file (via `crate::convert::encoder`,
    /// unlike `minimal_flac_bytes`'s synthetic non-decodable fixture
    /// above), tags it, and reads the tags back through the real
    /// symphonia-based `crate::library::tags::probe` — an actual
    /// end-to-end round-trip.
    #[test]
    fn flac_tags_round_trip_through_symphonia_reader() {
        let dir = std::env::temp_dir().join(format!(
            "lyra-tagwriter-flac-roundtrip-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.flac");

        let mut sink = crate::convert::encoder::create_sink(
            crate::convert::encoder::OutputFormat::Flac,
            &path,
            1,
            44_100,
            Some(16),
        )
        .unwrap();
        let samples: Vec<f32> = (0..4410)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / 44_100.0).sin())
            .collect();
        sink.write(&samples).unwrap();
        sink.finish().unwrap();

        let pic = Picture {
            mime_type: "image/jpeg".to_string(),
            is_front_cover: true,
            data: vec![1, 2, 3, 4],
        };
        write_flac_tags(
            &path,
            &WriteTags {
                title: Some("Test Title"),
                artist: Some("Test Artist"),
                album: Some("Test Album"),
                genre: Some("Rock"),
                date: Some("2024"),
                track_number: Some(3),
                track_total: Some(12),
                disc_number: Some(1),
                picture: Some(&pic),
            },
        );

        let probed = crate::library::tags::probe(&path, true).expect("probe should succeed");
        assert_eq!(probed.tags.title.as_deref(), Some("Test Title"));
        assert_eq!(probed.tags.artist.as_deref(), Some("Test Artist"));
        assert_eq!(probed.tags.album.as_deref(), Some("Test Album"));
        assert_eq!(probed.tags.genre.as_deref(), Some("Rock"));
        assert_eq!(probed.tags.date.as_deref(), Some("2024"));
        assert_eq!(probed.tags.track_number, Some(3));
        assert_eq!(probed.tags.track_total, Some(12));
        assert_eq!(probed.tags.disc_number, Some(1));
        assert_eq!(probed.tags.pictures.len(), 1);
        assert_eq!(probed.tags.pictures[0].data, vec![1, 2, 3, 4]);
        assert!(probed.tags.pictures[0].is_front_cover);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_tags_leave_file_untouched() {
        let dir =
            std::env::temp_dir().join(format!("lyra-tagwriter-empty-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.flac");
        let original = minimal_flac_bytes();
        std::fs::write(&path, &original).unwrap();

        write_flac_tags(&path, &WriteTags::default());

        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `crate::library::tags::probe` has a WAV-specific fallback
    /// (`wav_id3_fallback`) that reads exactly this `id3 ` chunk back via
    /// Symphonia's own `Id3v2Reader`, since `symphonia-format-riff`'s WAV
    /// demuxer itself never surfaces it (see that function's doc comment).
    /// This test both checks the writer's on-disk byte layout directly
    /// (RIFF size bookkeeping, chunk framing, ID3v2.4 header/frame
    /// structure) and round-trips through the real reader.
    #[test]
    fn wav_id3_chunk_round_trips_through_tags_probe() {
        let dir =
            std::env::temp_dir().join(format!("lyra-tagwriter-wav-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.wav");

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(&path, spec).unwrap();
        writer.finalize().unwrap();
        let original_len = std::fs::metadata(&path).unwrap().len();

        let cover_bytes = vec![0xFFu8, 0xD8, 0xFF, 0xD9]; // minimal fake JPEG payload
        let picture = Picture {
            mime_type: "image/jpeg".to_string(),
            is_front_cover: true,
            data: cover_bytes.clone(),
        };
        write_wav_tags(
            &path,
            &WriteTags {
                title: Some("WAV Title"),
                artist: Some("WAV Artist"),
                album: Some("WAV Album"),
                picture: Some(&picture),
                ..Default::default()
            },
        );

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");

        let riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as u64;
        assert_eq!(
            riff_size,
            bytes.len() as u64 - 8,
            "RIFF size must cover the appended chunk"
        );
        assert!(bytes.len() as u64 > original_len, "file must have grown");

        let tail = &bytes[original_len as usize..];
        assert_eq!(&tail[0..4], b"id3 ");
        let chunk_len = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]) as usize;
        let id3 = &tail[8..8 + chunk_len];
        assert_eq!(&id3[0..3], b"ID3");
        assert_eq!(id3[3], 0x04, "ID3v2.4");

        // TIT2 frame ID should appear right after the 10-byte ID3 header.
        assert_eq!(&id3[10..14], b"TIT2");

        // The real round trip: `crate::library::tags::probe` must recover
        // everything just written, via the WAV-specific `id3 `-chunk
        // fallback.
        let probed = crate::library::tags::probe(&path, true).expect("probe should succeed");
        assert_eq!(probed.tags.title.as_deref(), Some("WAV Title"));
        assert_eq!(probed.tags.artist.as_deref(), Some("WAV Artist"));
        assert_eq!(probed.tags.album.as_deref(), Some("WAV Album"));
        assert_eq!(probed.tags.pictures.len(), 1);
        assert_eq!(probed.tags.pictures[0].data, cover_bytes);
        assert!(probed.tags.pictures[0].is_front_cover);

        std::fs::remove_dir_all(&dir).ok();
    }
}
