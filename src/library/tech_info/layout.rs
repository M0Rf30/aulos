// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Byte-level layout of an audio file: how many bytes are audio payload and
//! how many are tag/metadata, plus the few facts that only live in container
//! headers Symphonia does not surface (FLAC STREAMINFO MD5, Vorbis/Opus vendor
//! string, MP4 `©too`, MP3 Xing/Info/VBRI/LAME headers, ID3v1/APE/ID3v2 tag
//! blocks).
//!
//! Everything is a handful of positional reads (`pread`) of headers — never a
//! scan of the audio — so the library scanner can call it for every file
//! without changing its I/O profile ([`measure`] with `full == false` reads
//! only what the payload size needs).
//!
//! Payload accounting, per container (the "approximation" lives here):
//! * MP3/AAC/APE/WavPack/MPC/Matroska…: file size minus leading ID3v2 tags and
//!   trailing ID3v1 / APEv2 / appended-ID3v2 blocks. The MP3 Xing/Info frame is
//!   counted as payload (it is a real, silent, frame).
//! * FLAC: file size minus the metadata blocks (STREAMINFO, tags, pictures,
//!   padding, seek tables…) and trailing tags.
//! * Ogg (Vorbis/Opus/FLAC-in-Ogg): file size minus the leading header pages
//!   (identification, comment, setup — everything before the first page with a
//!   positive granule position). Page framing overhead (~1%) stays in the
//!   payload; it is part of the audio stream.
//! * MP4/M4A: sum of the `mdat` box payloads, so `moov`, `udta`, cover art and
//!   `free` padding are excluded.
//! * WAV/AIFF/DSF/DFF: the size of the `data`/`SSND`/`DSD ` chunk.
//! * ASF/WMA: file size minus the ASF header object (the data object's 50-byte
//!   header stays in the payload).

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

use super::BitrateMode;

/// What [`measure`] found.
#[derive(Debug, Default)]
pub(super) struct Layout {
    /// Container label when the magic bytes identified it.
    pub container: Option<String>,
    /// Audio payload bytes (see the module docs). Never zero for a non-empty file.
    pub payload_bytes: u64,
    /// Tag blocks found, in file order.
    pub tag_formats: Vec<String>,
    /// Encoder/vendor string from the container (full mode only).
    pub encoder: Option<String>,
    /// FLAC STREAMINFO MD5 as lowercase hex (full mode only).
    pub md5: Option<String>,
    /// MP3 frame-header facts (full mode only).
    pub mp3: Option<Mp3Info>,
    /// Nominal bitrate and mode declared by the container header (Vorbis).
    pub declared_bitrate: Option<(u32, BitrateMode)>,
}

/// Facts from the first MP3 frame and its Xing/Info/VBRI/LAME header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mp3Info {
    /// `"MPEG-1 Layer III"`, `"MPEG-2 Layer III"`, …
    pub profile: String,
    /// Bitrate declared in the first frame header, kbit/s.
    pub nominal_kbps: u32,
    pub mode: BitrateMode,
}

fn read_at(file: &File, off: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    file.read_exact_at(&mut buf, off)?;
    Ok(buf)
}

fn read_at_most(file: &File, off: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let mut got = 0;
    while got < len {
        match file.read_at(&mut buf[got..], off + got as u64) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    buf.truncate(got);
    buf
}

fn synchsafe(b: &[u8]) -> u64 {
    b.iter()
        .take(4)
        .fold(0u64, |a, &x| (a << 7) | u64::from(x & 0x7f))
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn u32_be(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn u64_le(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

fn u64_be(b: &[u8]) -> u64 {
    u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Measures `file` (of `size` bytes). `full == false` skips everything that is
/// not needed for the payload size (vendor strings, MP3 header analysis).
pub(super) fn measure(file: &File, size: u64, full: bool) -> io::Result<Layout> {
    let mut layout = Layout::default();

    // Leading ID3v2 tags (some files stack more than one).
    let mut lead = 0u64;
    for _ in 0..4 {
        let Ok(h) = read_at(file, lead, 10) else {
            break;
        };
        if &h[..3] != b"ID3"
            || h[3] == 0xff
            || h[4] == 0xff
            || h[6..10].iter().any(|b| b & 0x80 != 0)
        {
            break;
        }
        let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
        let len = 10 + synchsafe(&h[6..10]) + footer;
        if lead + len > size {
            break;
        }
        layout.tag_formats.push(format!("ID3v2.{}", h[3]));
        lead += len;
    }

    // Trailing ID3v1 / APEv2 / appended ID3v2 blocks, outermost first.
    let mut tail = 0u64;
    for _ in 0..4 {
        let end = size.saturating_sub(tail);
        if end < lead + 10 {
            break;
        }
        if end >= lead + 128
            && let Ok(t) = read_at(file, end - 128, 128)
            && &t[..3] == b"TAG"
        {
            let v11 = t[125] == 0 && t[126] != 0;
            layout
                .tag_formats
                .push(if v11 { "ID3v1.1" } else { "ID3v1" }.to_owned());
            tail += 128;
            if end >= lead + 128 + 227
                && read_at(file, end - 128 - 227, 4).is_ok_and(|x| x == b"TAG+")
            {
                tail += 227;
            }
            continue;
        }
        if end >= lead + 32
            && let Ok(f) = read_at(file, end - 32, 32)
            && &f[..8] == b"APETAGEX"
        {
            let version = u32_le(&f[8..12]);
            let body = u64::from(u32_le(&f[12..16]));
            let has_header = u32_le(&f[20..24]) & 0x8000_0000 != 0;
            let total = body + if has_header { 32 } else { 0 };
            if total <= end - lead {
                layout
                    .tag_formats
                    .push(if version >= 2000 { "APEv2" } else { "APEv1" }.to_owned());
                tail += total;
                continue;
            }
        }
        if let Ok(f) = read_at(file, end - 10, 10)
            && &f[..3] == b"3DI"
        {
            let total = 20 + synchsafe(&f[6..10]);
            if total <= end - lead {
                layout
                    .tag_formats
                    .push(format!("ID3v2.{} (appended)", f[3]));
                tail += total;
                continue;
            }
        }
        break;
    }

    let head = read_at_most(file, lead, 16);
    let audio_end = size.saturating_sub(tail);
    let mut payload = audio_end.saturating_sub(lead);

    if head.starts_with(b"fLaC") {
        layout.container = Some("FLAC".into());
        let (meta_end, found) = walk_flac(file, lead + 4, audio_end, full, &mut layout);
        if found {
            payload = audio_end.saturating_sub(meta_end);
        }
    } else if head.starts_with(b"OggS") {
        layout.container = Some("Ogg".into());
        layout.tag_formats.push("Vorbis comment".into());
        let headers_end = walk_ogg(file, lead, size, full, &mut layout);
        payload = audio_end.saturating_sub(headers_end);
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" {
        payload = walk_mp4(file, size, full, &head, &mut layout).unwrap_or(payload);
    } else if head.len() >= 12 && (head.starts_with(b"RIFF") || head.starts_with(b"RF64")) {
        if &head[8..12] == b"WAVE" {
            layout.container = Some("WAVE".into());
            payload = walk_chunks(file, 12, size, false, b"data", &mut layout).unwrap_or(payload);
        }
    } else if head.len() >= 12 && head.starts_with(b"FORM") {
        if &head[8..11] == b"AIF" {
            layout.container = Some("AIFF".into());
            payload = walk_chunks(file, 12, size, true, b"SSND", &mut layout).unwrap_or(payload);
        }
    } else if head.starts_with(b"DSD ") {
        layout.container = Some("DSF".into());
        payload = walk_dsf(file, size, &mut layout).unwrap_or(payload);
    } else if head.starts_with(b"FRM8") {
        layout.container = Some("DSDIFF".into());
        payload = walk_dff(file, size, &mut layout).unwrap_or(payload);
    } else if head.starts_with(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]) {
        layout.container = Some("ASF".into());
        if let Ok(h) = read_at(file, 16, 8) {
            let header = u64_le(&h);
            if header < size {
                payload = size - header;
                layout.tag_formats.push("ASF content description".into());
            }
        }
    } else if head.starts_with(b"MAC ") {
        layout.container = Some("Monkey's Audio".into());
    } else if head.starts_with(b"wvpk") {
        layout.container = Some("WavPack".into());
    } else if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        layout.container = Some("Matroska".into());
    } else if full && let Some(info) = mp3_info(file, lead, audio_end) {
        layout.container = Some(
            if info.0.profile.ends_with("III") {
                "MP3"
            } else if info.0.profile.ends_with("II") {
                "MP2"
            } else {
                "MP1"
            }
            .into(),
        );
        layout.encoder = info.1;
        layout.mp3 = Some(info.0);
    }

    // A zero payload for a non-empty file would turn into a 0 kbps bitrate;
    // fall back to the whole file instead.
    if payload == 0 {
        payload = size;
    }
    layout.payload_bytes = payload;
    Ok(layout)
}

// --- FLAC -------------------------------------------------------------------

/// Returns `(end of metadata blocks, walked ok)`.
fn walk_flac(file: &File, mut pos: u64, end: u64, full: bool, layout: &mut Layout) -> (u64, bool) {
    let mut seen_comment = false;
    for _ in 0..4096 {
        let Ok(h) = read_at(file, pos, 4) else {
            return (pos, false);
        };
        let last = h[0] & 0x80 != 0;
        let kind = h[0] & 0x7f;
        let len = u64::from(u32::from_be_bytes([0, h[1], h[2], h[3]]));
        if pos + 4 + len > end {
            return (pos, false);
        }
        if full {
            match kind {
                0 if len >= 34 => {
                    if let Ok(si) = read_at(file, pos + 4, 34) {
                        let md5 = &si[18..34];
                        if md5.iter().any(|&b| b != 0) {
                            layout.md5 = Some(hex(md5));
                        }
                    }
                }
                4 if len >= 4 && !seen_comment => {
                    let probe = read_at_most(file, pos + 4, len.min(512) as usize);
                    if probe.len() >= 4 {
                        let vlen = u32_le(&probe) as usize;
                        if let Some(v) = probe.get(4..4 + vlen) {
                            layout.encoder = clean_text(v);
                        }
                    }
                }
                _ => {}
            }
        }
        if kind == 4 && !seen_comment {
            seen_comment = true;
            layout.tag_formats.push("Vorbis comment".into());
        }
        pos += 4 + len;
        if last {
            return (pos, true);
        }
    }
    (pos, false)
}

fn clean_text(bytes: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!s.is_empty()).then(|| s.to_owned())
}

// --- Ogg --------------------------------------------------------------------

/// Walks the leading header pages; returns the offset of the first audio page.
fn walk_ogg(file: &File, start: u64, size: u64, full: bool, layout: &mut Layout) -> u64 {
    let mut pos = start;
    for index in 0..64 {
        let Ok(h) = read_at(file, pos, 27) else { break };
        if &h[..4] != b"OggS" {
            break;
        }
        let granule = u64_le(&h[6..14]);
        let nseg = usize::from(h[26]);
        let Ok(segs) = read_at(file, pos + 27, nseg) else {
            break;
        };
        let body: u64 = segs.iter().map(|&s| u64::from(s)).sum();
        let page_len = 27 + nseg as u64 + body;
        if pos + page_len > size {
            break;
        }
        // Audio pages have a positive granule position; header pages carry 0
        // (or -1 while a packet is still spanning pages).
        if index > 0 && granule != 0 && granule != u64::MAX {
            break;
        }
        if full && index <= 1 {
            let data = read_at_most(file, pos + 27 + nseg as u64, body.min(512) as usize);
            if index == 0 {
                parse_vorbis_ident(&data, layout);
            } else {
                parse_ogg_comment_vendor(&data, layout);
            }
        }
        pos += page_len;
    }
    pos
}

fn parse_vorbis_ident(data: &[u8], layout: &mut Layout) {
    // 0x01 "vorbis" version(4) channels(1) rate(4) max(4) nominal(4) min(4)
    if data.len() >= 28 && data[0] == 1 && &data[1..7] == b"vorbis" {
        let rd = |o: usize| i32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
        let (max, nominal, min) = (rd(16), rd(20), rd(24));
        if nominal > 0 {
            let mode = if max == nominal && min == nominal {
                BitrateMode::Cbr
            } else if max <= 0 && min <= 0 {
                BitrateMode::Vbr
            } else {
                BitrateMode::Abr
            };
            layout.declared_bitrate = Some(((nominal as u32 + 500) / 1000, mode));
        }
    }
}

fn parse_ogg_comment_vendor(data: &[u8], layout: &mut Layout) {
    let rest = if data.len() > 7 && data[0] == 3 && &data[1..7] == b"vorbis" {
        &data[7..]
    } else if data.len() > 8 && &data[..8] == b"OpusTags" {
        &data[8..]
    } else if data.len() > 4 && data[0] == 0x84 {
        // FLAC-in-Ogg: a Vorbis-comment metadata block header precedes the body.
        &data[4..]
    } else {
        return;
    };
    if rest.len() >= 4 {
        let vlen = u32_le(rest) as usize;
        if let Some(v) = rest.get(4..4 + vlen) {
            layout.encoder = clean_text(v);
        }
    }
}

// --- MP4 --------------------------------------------------------------------

/// A box header: `(type, payload start, box end)`.
fn box_header(file: &File, pos: u64, limit: u64) -> Option<([u8; 4], u64, u64)> {
    if pos + 8 > limit {
        return None;
    }
    let h = read_at(file, pos, 8).ok()?;
    let size32 = u64::from(u32_be(&h[..4]));
    let kind = [h[4], h[5], h[6], h[7]];
    let (header, size) = match size32 {
        0 => (8, limit - pos),
        1 => {
            let ext = read_at(file, pos + 8, 8).ok()?;
            (16, u64_be(&ext))
        }
        n => (8, n),
    };
    if size < header || pos + size > limit {
        return None;
    }
    Some((kind, pos + header, pos + size))
}

fn find_child(file: &File, start: u64, end: u64, name: &[u8; 4]) -> Option<(u64, u64)> {
    let mut pos = start;
    for _ in 0..512 {
        let (kind, data, box_end) = box_header(file, pos, end)?;
        if &kind == name {
            return Some((data, box_end));
        }
        pos = box_end;
    }
    None
}

fn walk_mp4(file: &File, size: u64, full: bool, head: &[u8], layout: &mut Layout) -> Option<u64> {
    layout.container = Some(
        match &head[8..12] {
            b"M4A " => "MPEG-4 Audio (M4A)",
            b"M4B " => "MPEG-4 Audiobook (M4B)",
            b"qt  " => "QuickTime",
            _ => "MPEG-4",
        }
        .to_owned(),
    );
    let mut pos = 0u64;
    let mut mdat = 0u64;
    let mut found_mdat = false;
    for _ in 0..256 {
        let Some((kind, data, end)) = box_header(file, pos, size) else {
            break;
        };
        match &kind {
            b"mdat" => {
                mdat += end - data;
                found_mdat = true;
            }
            b"moov" => walk_moov(file, data, end, full, layout),
            _ => {}
        }
        pos = end;
    }
    found_mdat.then_some(mdat)
}

fn walk_moov(file: &File, start: u64, end: u64, full: bool, layout: &mut Layout) {
    let Some((udta, udta_end)) = find_child(file, start, end, b"udta") else {
        return;
    };
    let Some((meta, meta_end)) = find_child(file, udta, udta_end, b"meta") else {
        return;
    };
    // `meta` is a full box: 4 bytes of version/flags precede its children.
    let Some((ilst, ilst_end)) = find_child(file, meta + 4, meta_end, b"ilst") else {
        return;
    };
    layout.tag_formats.push("iTunes metadata (ilst)".into());
    if !full {
        return;
    }
    if let Some((item, item_end)) = find_child(file, ilst, ilst_end, &[0xA9, b't', b'o', b'o'])
        && let Some((data, data_end)) = find_child(file, item, item_end, b"data")
        && data_end > data + 8
    {
        let len = (data_end - data - 8).min(256) as usize;
        layout.encoder = clean_text(&read_at_most(file, data + 8, len));
    }
}

// --- RIFF / AIFF / DSD ------------------------------------------------------

/// Walks IFF-style chunks from `pos`; returns the size of `data_id`.
fn walk_chunks(
    file: &File,
    mut pos: u64,
    size: u64,
    big_endian: bool,
    data_id: &[u8; 4],
    layout: &mut Layout,
) -> Option<u64> {
    let mut data = None;
    for _ in 0..4096 {
        let Ok(h) = read_at(file, pos, 8) else { break };
        let len = if big_endian {
            u64::from(u32_be(&h[4..8]))
        } else {
            u64::from(u32_le(&h[4..8]))
        };
        let id = [h[0], h[1], h[2], h[3]];
        let body = pos + 8;
        let mut len = len;
        if &id == data_id {
            // A streamed file may declare 0 or 0xFFFFFFFF; the data then runs to EOF.
            if len == 0 || len == 0xFFFF_FFFF || body + len > size {
                len = size.saturating_sub(body);
            }
            data = Some(len);
        }
        match &id {
            b"LIST" => {
                if read_at(file, body, 4).is_ok_and(|t| &t == b"INFO") {
                    layout.tag_formats.push("RIFF INFO".into());
                }
            }
            b"id3 " | b"ID3 " => layout.tag_formats.push("ID3v2 chunk".into()),
            b"bext" => layout.tag_formats.push("BWF bext".into()),
            b"NAME" | b"AUTH" | b"(c) " | b"ANNO"
                if !layout.tag_formats.iter().any(|t| t == "AIFF text chunks") =>
            {
                layout.tag_formats.push("AIFF text chunks".into());
            }
            _ => {}
        }
        // Chunks are padded to even sizes.
        let next = body + len + (len & 1);
        if next >= size || next <= pos {
            break;
        }
        pos = next;
    }
    data
}

fn walk_dsf(file: &File, size: u64, layout: &mut Layout) -> Option<u64> {
    let h = read_at(file, 0, 28).ok()?;
    let metadata_ptr = u64_le(&h[20..28]);
    if metadata_ptr != 0 && metadata_ptr < size {
        layout.tag_formats.push("ID3v2".into());
    }
    let mut pos = 28u64;
    for _ in 0..16 {
        let c = read_at(file, pos, 12).ok()?;
        let len = u64_le(&c[4..12]);
        if &c[..4] == b"data" {
            return len.checked_sub(12).filter(|&n| pos + len <= size && n > 0);
        }
        if len < 12 {
            return None;
        }
        pos += len;
    }
    None
}

fn walk_dff(file: &File, size: u64, layout: &mut Layout) -> Option<u64> {
    let mut pos = 16u64;
    let mut data = None;
    for _ in 0..64 {
        let Ok(c) = read_at(file, pos, 12) else { break };
        let len = u64_be(&c[4..12]);
        match &c[..4] {
            b"DSD " => data = Some(len.min(size.saturating_sub(pos + 12))),
            b"ID3 " => layout.tag_formats.push("ID3v2 chunk".into()),
            _ => {}
        }
        let next = pos + 12 + len + (len & 1);
        if next >= size {
            break;
        }
        pos = next;
    }
    data
}

// --- MP3 --------------------------------------------------------------------

const MPEG1_L1: [u32; 15] = [
    0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
];
const MPEG1_L2: [u32; 15] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
];
const MPEG1_L3: [u32; 15] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const MPEG2_L1: [u32; 15] = [
    0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
];
const MPEG2_L23: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

#[derive(Debug, Clone, Copy)]
struct FrameHeader {
    /// 1 for MPEG-1, 2 for MPEG-2, 25 for MPEG-2.5.
    version: u8,
    layer: u8,
    bitrate_kbps: u32,
    sample_rate: u32,
    padding: u32,
    crc: bool,
    mono: bool,
}

impl FrameHeader {
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 4 || b[0] != 0xff || b[1] & 0xe0 != 0xe0 {
            return None;
        }
        let version = match (b[1] >> 3) & 3 {
            0 => 25,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let layer = match (b[1] >> 1) & 3 {
            1 => 3,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let br_index = usize::from(b[2] >> 4);
        let sr_index = usize::from((b[2] >> 2) & 3);
        if br_index == 0 || br_index == 15 || sr_index == 3 {
            return None;
        }
        let table = match (version, layer) {
            (1, 1) => &MPEG1_L1,
            (1, 2) => &MPEG1_L2,
            (1, _) => &MPEG1_L3,
            (_, 1) => &MPEG2_L1,
            _ => &MPEG2_L23,
        };
        let base_rate = [44_100, 48_000, 32_000][sr_index];
        let sample_rate = match version {
            1 => base_rate,
            2 => base_rate / 2,
            _ => base_rate / 4,
        };
        Some(Self {
            version,
            layer,
            bitrate_kbps: table[br_index],
            sample_rate,
            padding: u32::from((b[2] >> 1) & 1),
            crc: b[1] & 1 == 0,
            mono: (b[3] >> 6) == 3,
        })
    }

    fn frame_len(&self) -> u32 {
        let bits = self.bitrate_kbps * 1000;
        match (self.layer, self.version) {
            (1, _) => (12 * bits / self.sample_rate + self.padding) * 4,
            (3, v) if v != 1 => 72 * bits / self.sample_rate + self.padding,
            _ => 144 * bits / self.sample_rate + self.padding,
        }
    }

    fn side_info_len(&self) -> u64 {
        match (self.version, self.mono) {
            (1, true) => 17,
            (1, false) => 32,
            (_, true) => 9,
            (_, false) => 17,
        }
    }

    fn profile(&self) -> String {
        let version = match self.version {
            1 => "MPEG-1",
            2 => "MPEG-2",
            _ => "MPEG-2.5",
        };
        let layer = match self.layer {
            1 => "I",
            2 => "II",
            _ => "III",
        };
        format!("{version} Layer {layer}")
    }
}

/// Finds the first valid MP3 frame at or after `from` and reads its Xing/Info/
/// VBRI/LAME header; without one, compares the bitrates of the next frames to
/// tell CBR from VBR. Returns the facts and the encoder string.
fn mp3_info(file: &File, from: u64, end: u64) -> Option<(Mp3Info, Option<String>)> {
    let window = read_at_most(file, from, 16 * 1024);
    // Sync + a second header at the computed frame length rules out false syncs.
    let (offset, first) = (0..window.len().saturating_sub(4)).find_map(|i| {
        let h = FrameHeader::parse(&window[i..])?;
        let next = i + h.frame_len() as usize;
        let n = FrameHeader::parse(window.get(next..next + 4)?)?;
        (n.layer == h.layer && n.version == h.version && n.sample_rate == h.sample_rate)
            .then_some((from + i as u64, h))
    })?;

    let xing_at = offset + 4 + if first.crc { 2 } else { 0 } + first.side_info_len();
    let mut mode = None;
    let mut encoder = None;
    if let Ok(x) = read_at(file, xing_at, 4)
        && let tag @ (b"Xing" | b"Info") = &x[..]
    {
        mode = Some(if tag == b"Info" {
            BitrateMode::Cbr
        } else {
            BitrateMode::Vbr
        });
        if let Some((enc, method)) = read_lame(file, xing_at) {
            encoder = enc;
            match method {
                Some(1 | 8) => mode = Some(BitrateMode::Cbr),
                Some(2 | 9) => mode = Some(BitrateMode::Abr),
                Some(3..=7) => mode = Some(BitrateMode::Vbr),
                _ => {}
            }
        }
    }
    if mode.is_none() && read_at(file, offset + 4 + 32, 4).is_ok_and(|v| &v == b"VBRI") {
        mode = Some(BitrateMode::Vbr);
    }
    let mode = mode.unwrap_or_else(|| {
        // No info frame: walk the following frames and compare bitrates.
        let pos = offset;
        let mut bitrates = std::collections::BTreeSet::new();
        let buf = read_at_most(file, offset, 96 * 1024);
        let mut i = 0usize;
        for _ in 0..64 {
            let Some(h) = buf.get(i..).and_then(FrameHeader::parse) else {
                break;
            };
            bitrates.insert(h.bitrate_kbps);
            let len = h.frame_len() as usize;
            if len == 0 || pos + (i + len) as u64 >= end {
                break;
            }
            i += len;
        }
        if bitrates.len() > 1 {
            BitrateMode::Vbr
        } else {
            BitrateMode::Cbr
        }
    });

    Some((
        Mp3Info {
            profile: first.profile(),
            nominal_kbps: first.bitrate_kbps,
            mode,
        },
        encoder,
    ))
}

/// Reads the encoder string and VBR method after a Xing/Info header at `at`.
fn read_lame(file: &File, at: u64) -> Option<(Option<String>, Option<u8>)> {
    let h = read_at(file, at + 4, 4).ok()?;
    let flags = u32_be(&h);
    let mut pos = at + 8;
    if flags & 1 != 0 {
        pos += 4;
    }
    if flags & 2 != 0 {
        pos += 4;
    }
    if flags & 4 != 0 {
        pos += 100;
    }
    if flags & 8 != 0 {
        pos += 4;
    }
    let tag = read_at(file, pos, 10).ok()?;
    let name = &tag[..9];
    let printable =
        name[0].is_ascii_alphabetic() && name.iter().all(|&b| b == 0 || (0x20..0x7f).contains(&b));
    if !printable {
        return Some((None, None));
    }
    let encoder = clean_text(name);
    // Only LAME writes a method nibble; "Lavf" and others reuse the field for text.
    let method = encoder
        .as_deref()
        .filter(|e| e.starts_with("LAME") || e.starts_with("GOGO"))
        .map(|_| tag[9] & 0x0f);
    Some((encoder, method))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(label: &str, bytes: &[u8]) -> (std::path::PathBuf, File) {
        let path = std::env::temp_dir().join(format!(
            "aulos-layout-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        File::create(&path).unwrap().write_all(bytes).unwrap();
        let file = File::open(&path).unwrap();
        (path, file)
    }

    fn id3v2(body: usize) -> Vec<u8> {
        let mut v = b"ID3\x04\x00\x00".to_vec();
        v.extend_from_slice(&[
            ((body >> 21) & 0x7f) as u8,
            ((body >> 14) & 0x7f) as u8,
            ((body >> 7) & 0x7f) as u8,
            (body & 0x7f) as u8,
        ]);
        v.extend(std::iter::repeat_n(0u8, body));
        v
    }

    /// 128 kbit/s 44.1 kHz MPEG-1 Layer III stereo frames (417-byte frames).
    fn mp3_frames(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for _ in 0..n {
            v.extend_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
            v.extend(std::iter::repeat_n(0u8, 413));
        }
        v
    }

    #[test]
    fn mp3_payload_excludes_id3v2_and_id3v1() {
        let mut bytes = id3v2(500);
        bytes.extend(mp3_frames(40));
        let mut v1 = [0u8; 128];
        v1[..3].copy_from_slice(b"TAG");
        bytes.extend_from_slice(&v1);
        let (path, file) = tmp("mp3", &bytes);
        let l = measure(&file, bytes.len() as u64, true).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(l.payload_bytes, 40 * 417);
        assert_eq!(l.tag_formats, ["ID3v2.4", "ID3v1"]);
        let mp3 = l.mp3.unwrap();
        assert_eq!(mp3.nominal_kbps, 128);
        assert_eq!(mp3.profile, "MPEG-1 Layer III");
        assert_eq!(mp3.mode, BitrateMode::Cbr);
    }

    #[test]
    fn apev2_footer_is_excluded() {
        let mut bytes = mp3_frames(20);
        let audio = bytes.len() as u64;
        let mut ape = Vec::new();
        ape.extend_from_slice(b"APETAGEX");
        ape.extend_from_slice(&2000u32.to_le_bytes());
        ape.extend_from_slice(&(32u32 + 40).to_le_bytes()); // body + footer
        ape.extend_from_slice(&1u32.to_le_bytes());
        ape.extend_from_slice(&0u32.to_le_bytes()); // no header
        ape.extend_from_slice(&[0u8; 8]);
        let mut body = vec![0u8; 40];
        body.extend_from_slice(&ape);
        bytes.extend_from_slice(&body);
        let (path, file) = tmp("ape", &bytes);
        let l = measure(&file, bytes.len() as u64, false).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(l.payload_bytes, audio);
        assert_eq!(l.tag_formats, ["APEv2"]);
    }

    #[test]
    fn flac_payload_excludes_metadata_blocks_and_reads_md5_and_vendor() {
        let mut bytes = b"fLaC".to_vec();
        // STREAMINFO (not last)
        bytes.extend_from_slice(&[0x00, 0, 0, 34]);
        let mut si = vec![0u8; 34];
        si[18..34].copy_from_slice(&[0xab; 16]);
        bytes.extend_from_slice(&si);
        // VORBIS_COMMENT (last), vendor "libFLAC test"
        let vendor = b"libFLAC test";
        let mut vc = (vendor.len() as u32).to_le_bytes().to_vec();
        vc.extend_from_slice(vendor);
        vc.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&[0x84, 0, 0, vc.len() as u8]);
        bytes.extend_from_slice(&vc);
        let meta_len = bytes.len() as u64;
        bytes.extend(std::iter::repeat_n(0xAAu8, 1000));
        let (path, file) = tmp("flac", &bytes);
        let l = measure(&file, bytes.len() as u64, true).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(l.payload_bytes, bytes.len() as u64 - meta_len);
        assert_eq!(l.md5.as_deref(), Some("abababababababababababababababab"));
        assert_eq!(l.encoder.as_deref(), Some("libFLAC test"));
        assert_eq!(l.tag_formats, ["Vorbis comment"]);
    }

    #[test]
    fn mp4_payload_is_the_mdat_size() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0, 0, 0, 20]);
        bytes.extend_from_slice(b"ftypM4A ");
        bytes.extend_from_slice(&[0u8; 8]);
        bytes.extend_from_slice(&[0, 0, 0, 40]);
        bytes.extend_from_slice(b"moov");
        bytes.extend_from_slice(&[0u8; 32]);
        bytes.extend_from_slice(&(8u32 + 500).to_be_bytes());
        bytes.extend_from_slice(b"mdat");
        bytes.extend(std::iter::repeat_n(1u8, 500));
        let (path, file) = tmp("mp4", &bytes);
        let l = measure(&file, bytes.len() as u64, false).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(l.payload_bytes, 500);
        assert_eq!(l.container.as_deref(), Some("MPEG-4 Audio (M4A)"));
    }

    #[test]
    fn wav_payload_is_the_data_chunk() {
        let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 16]);
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&1000u32.to_le_bytes());
        bytes.extend(std::iter::repeat_n(0u8, 1000));
        bytes.extend_from_slice(b"LIST");
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(b"INFO");
        let (path, file) = tmp("wav", &bytes);
        let l = measure(&file, bytes.len() as u64, false).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(l.payload_bytes, 1000);
        assert_eq!(l.tag_formats, ["RIFF INFO"]);
    }

    #[test]
    fn ogg_payload_excludes_header_pages() {
        fn page(granule: i64, body: &[u8], first: bool) -> Vec<u8> {
            let mut p = b"OggS\0".to_vec();
            p.push(if first { 2 } else { 0 });
            p.extend_from_slice(&granule.to_le_bytes());
            p.extend_from_slice(&[0u8; 12]);
            p.push(1);
            p.push(body.len() as u8);
            p.extend_from_slice(body);
            p
        }
        let mut ident = vec![1u8];
        ident.extend_from_slice(b"vorbis");
        ident.extend_from_slice(&0u32.to_le_bytes());
        ident.push(2);
        ident.extend_from_slice(&44_100u32.to_le_bytes());
        for v in [-1i32, 160_000, -1] {
            ident.extend_from_slice(&v.to_le_bytes());
        }
        let mut comment = vec![3u8];
        comment.extend_from_slice(b"vorbis");
        comment.extend_from_slice(&9u32.to_le_bytes());
        comment.extend_from_slice(b"Xiph test");
        comment.extend_from_slice(&0u32.to_le_bytes());
        let mut bytes = page(0, &ident, true);
        bytes.extend(page(0, &comment, false));
        let headers = bytes.len() as u64;
        bytes.extend(page(4096, &[7u8; 200], false));
        let (path, file) = tmp("ogg", &bytes);
        let l = measure(&file, bytes.len() as u64, true).unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(l.payload_bytes, bytes.len() as u64 - headers);
        assert_eq!(l.encoder.as_deref(), Some("Xiph test"));
        assert_eq!(l.declared_bitrate, Some((160, BitrateMode::Vbr)));
    }
}
