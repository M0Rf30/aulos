// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! HLS (`.m3u8`) live radio: a blocking [`Read`] that follows the playlist
//! and yields the concatenated segment bytes, which the symphonia decoder
//! then probes like any other stream (MPEG-TS or raw ADTS/MP3 segments).
//!
//! A master playlist is reduced to its lowest-bandwidth variant (audio-only
//! streams carry one rendition per bitrate). A live media playlist is polled
//! every half target duration; segments are fetched in order, each once.
//! Encrypted playlists and fragmented MP4 are not supported.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::time::{Duration, Instant};
use url::Url;

/// Segments of a live playlist skipped over at start, so playback begins
/// near the live edge (RFC 8216 asks for three target durations back).
const LIVE_START_SEGMENTS: usize = 2;

/// Playlist fetches without anything new before the stream counts as ended.
const MAX_IDLE_POLLS: u32 = 20;

/// A parsed playlist.
#[derive(Debug, PartialEq)]
pub(super) enum Playlist {
    /// Variant streams, `(bandwidth, absolute url)`.
    Master(Vec<(u64, Url)>),
    Media {
        media_sequence: u64,
        target_secs: f64,
        ended: bool,
        segments: Vec<Url>,
    },
}

/// Parses an HLS playlist, resolving every URI against `base`.
pub(super) fn parse(body: &str, base: &Url) -> Result<Playlist, String> {
    if !body.trim_start().starts_with("#EXTM3U") {
        return Err("Not an HLS playlist".into());
    }
    let mut variants = Vec::new();
    let mut segments = Vec::new();
    let (mut media_sequence, mut target_secs, mut ended) = (0, 6.0, false);
    let mut pending_bandwidth: Option<u64> = None;
    for line in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let bw = attrs
                .split(',')
                .find_map(|a| a.trim().strip_prefix("BANDWIDTH="))
                .and_then(|v| v.parse().ok());
            pending_bandwidth = Some(bw.unwrap_or(u64::MAX));
        } else if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_secs = v.trim().parse().unwrap_or(6.0);
        } else if line.starts_with("#EXT-X-ENDLIST") {
            ended = true;
        } else if let Some(attrs) = line.strip_prefix("#EXT-X-KEY:")
            && !attrs.contains("METHOD=NONE")
        {
            return Err("Encrypted HLS streams are not supported".into());
        } else if line.starts_with("#EXT-X-MAP:") {
            return Err("Fragmented MP4 HLS streams are not supported".into());
        } else if !line.starts_with('#') {
            let url = base
                .join(line)
                .map_err(|e| format!("Bad HLS URI {line}: {e}"))?;
            match pending_bandwidth.take() {
                Some(bw) => variants.push((bw, url)),
                None => segments.push(url),
            }
        }
    }
    if !variants.is_empty() {
        return Ok(Playlist::Master(variants));
    }
    Ok(Playlist::Media {
        media_sequence,
        target_secs,
        ended,
        segments,
    })
}

/// Streams an HLS playlist as one continuous byte stream.
pub(super) struct HlsReader {
    client: crate::http::blocking::Client,
    playlist: Url,
    /// Sequence number of the next segment to fetch.
    next_seq: Option<u64>,
    queue: VecDeque<Url>,
    current: io::Cursor<Vec<u8>>,
    ended: bool,
    poll_every: Duration,
    last_poll: Option<Instant>,
    idle_polls: u32,
}

impl HlsReader {
    /// Opens the playlist at `url` (following a master playlist down to a
    /// media one) and returns the reader with the segment extension to hint
    /// the decoder with.
    pub(super) fn open(
        client: crate::http::blocking::Client,
        url: &str,
    ) -> Result<(Self, Option<String>), String> {
        let mut playlist = Url::parse(url).map_err(|e| format!("Bad stream URL: {e}"))?;
        let mut reader = loop {
            let body = fetch_text(&client, &playlist)?;
            match parse(&body, &playlist)? {
                Playlist::Master(mut variants) => {
                    variants.sort_by_key(|(bw, _)| *bw);
                    playlist = variants.swap_remove(0).1;
                }
                Playlist::Media { .. } => {
                    break Self {
                        client,
                        playlist,
                        next_seq: None,
                        queue: VecDeque::new(),
                        current: io::Cursor::new(Vec::new()),
                        ended: false,
                        poll_every: Duration::from_secs(3),
                        last_poll: None,
                        idle_polls: 0,
                    };
                }
            }
        };
        reader.poll()?;
        let hint = reader
            .queue
            .front()
            .and_then(|u| super::backend::extension_hint(u.as_str()).filter(|e| e != "m3u8"));
        Ok((reader, hint))
    }

    /// Re-reads the media playlist and queues segments not seen yet.
    fn poll(&mut self) -> Result<(), String> {
        self.last_poll = Some(Instant::now());
        let body = fetch_text(&self.client, &self.playlist)?;
        let Playlist::Media {
            media_sequence,
            target_secs,
            ended,
            segments,
        } = parse(&body, &self.playlist)?
        else {
            return Err("Unexpected master playlist".into());
        };
        self.poll_every = Duration::from_secs_f64((target_secs / 2.0).clamp(1.0, 10.0));
        self.ended = ended;
        let first_new = match self.next_seq {
            None if ended => 0,
            None => segments.len().saturating_sub(LIVE_START_SEGMENTS),
            Some(next) => usize::try_from(next.saturating_sub(media_sequence)).unwrap_or(0),
        };
        let before = self.queue.len();
        let base_seq = media_sequence + first_new as u64;
        self.queue.extend(segments.into_iter().skip(first_new));
        self.next_seq = Some(base_seq + (self.queue.len() - before) as u64);
        self.idle_polls = if self.queue.len() > before {
            0
        } else {
            self.idle_polls + 1
        };
        Ok(())
    }
}

fn fetch_text(client: &crate::http::blocking::Client, url: &Url) -> Result<String, String> {
    client
        .get(url.as_str())
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .map_err(|e| format!("Failed to fetch HLS playlist: {e}"))
}

fn io_err(message: String) -> io::Error {
    io::Error::other(message)
}

impl Read for HlsReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let n = self.current.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            if let Some(url) = self.queue.pop_front() {
                let bytes = self
                    .client
                    .get(url.as_str())
                    .send()
                    .and_then(reqwest::blocking::Response::error_for_status)
                    .and_then(reqwest::blocking::Response::bytes)
                    .map_err(|e| io_err(format!("Failed to fetch HLS segment: {e}")))?;
                self.current = io::Cursor::new(bytes.to_vec());
                continue;
            }
            if self.ended || self.idle_polls >= MAX_IDLE_POLLS {
                return Ok(0);
            }
            if let Some(at) = self.last_poll {
                let wait = self.poll_every.saturating_sub(at.elapsed());
                std::thread::sleep(wait);
            }
            self.poll().map_err(io_err)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://x.example/live/master.m3u8?t=1").unwrap()
    }

    #[test]
    fn master_variants_resolve_relative_uris() {
        let body = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=128000\nhi/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=64000\nlo/index.m3u8\n";
        let Playlist::Master(v) = parse(body, &base()).unwrap() else {
            panic!("master expected")
        };
        assert_eq!(
            v[1],
            (
                64000,
                Url::parse("https://x.example/live/lo/index.m3u8").unwrap()
            )
        );
    }

    #[test]
    fn media_playlist_lists_segments_and_sequence() {
        let body = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:7\n#EXTINF:4,\na.aac\n#EXTINF:4,\nhttps://y.example/b.aac\n";
        let Playlist::Media {
            media_sequence,
            target_secs,
            ended,
            segments,
        } = parse(body, &base()).unwrap()
        else {
            panic!("media expected")
        };
        assert_eq!((media_sequence, target_secs, ended), (7, 4.0, false));
        assert_eq!(segments[0].as_str(), "https://x.example/live/a.aac");
        assert_eq!(segments[1].as_str(), "https://y.example/b.aac");
    }

    #[test]
    fn encrypted_streams_are_rejected() {
        let body = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:4,\na.ts\n";
        assert!(parse(body, &base()).is_err());
    }
}
