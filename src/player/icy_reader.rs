// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! ICY (Shoutcast/Icecast) in-band metadata stripping.
//!
//! Replaces `icy_metadata::IcyMetadataReader` (crate version 0.6.0). That
//! type's `parse_next_metadata` does:
//!
//! ```ignore
//! let written = self.inner.read(&mut buf[..self.next_metadata])?;
//! ```
//!
//! without ever clamping `self.next_metadata` to `buf.len()` on this path
//! (only the *second* read in the same function clamps, via `.min(to_fill)`)
//! — see `~/.cargo/registry/src/*/icy-metadata-0.6.0/src/reader.rs:120`.
//! `self.next_metadata` is reset to (up to) the station's `icy-metaint` every
//! time a metadata block is consumed, so as soon as `icy-metaint` exceeds the
//! caller's read-buffer size, the *next* call within the same
//! `parse_metadata_from_stream` loop iteration panics with a slice-index
//! out-of-range. Symphonia's `MediaSourceStream` always reads through a
//! fixed ~32 KiB internal buffer, and plenty of real stations advertise a
//! larger `icy-metaint` (SomaFM: 45000) — so this reliably panics a few
//! seconds into playback (`metaint / byte-rate`), on the engine's dedicated
//! playback thread, which nothing wraps in `catch_unwind`: the thread simply
//! dies mid-track with no error surfaced anywhere, which is the direct cause
//! of "radio streams play for a few seconds and then stop suddenly".
//!
//! This reader implements the same wire format (a length-prefixed metadata
//! block — the length byte times 16 — every `metaint` bytes of audio; see
//! <https://gist.github.com/niko/2a1d7b2d109ebe7f7ca2f860c3505ef0>) but never
//! requests more than `buf.len()` bytes of audio from the inner reader per
//! call, regardless of how large `metaint` is relative to the caller's
//! buffer — so it can never trigger that class of bug.
//!
//! Metadata *parsing* still goes through `icy_metadata::IcyMetadata`'s
//! `FromStr` impl, which has real key=value/escaping logic worth reusing;
//! only the buggy buffer bookkeeping is reimplemented here.

use std::io::{self, Read};

use icy_metadata::IcyMetadata;

/// Wraps a live-stream [`Read`] source, stripping interleaved ICY metadata
/// blocks and reporting each parsed block (or `None` for an empty one, or
/// one that failed to parse) through `on_metadata`.
pub struct IcyStrippingReader<R> {
    inner: R,
    metaint: usize,
    /// Bytes of audio still to deliver before the next metadata block is
    /// due. `0` means a metadata block must be consumed before any more
    /// audio bytes are read.
    until_metadata: usize,
    on_metadata: Box<dyn FnMut(Option<IcyMetadata>) + Send + Sync>,
}

impl<R: Read> IcyStrippingReader<R> {
    /// `metaint` is the station's `icy-metaint` header value: the number of
    /// audio bytes between each embedded metadata block. Must be nonzero
    /// (callers already only construct this when the header parsed to a
    /// `NonZeroUsize`).
    pub fn new(
        inner: R,
        metaint: usize,
        on_metadata: impl FnMut(Option<IcyMetadata>) + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            metaint: metaint.max(1),
            until_metadata: metaint.max(1),
            on_metadata: Box::new(on_metadata),
        }
    }

    /// Read and dispatch exactly one metadata block: a one-byte length
    /// (times 16 per the ICY wire format), then that many bytes of payload.
    /// A length of zero is the common case (most intervals carry no title
    /// change) and dispatches `None` without a further read.
    fn consume_metadata_block(&mut self) -> io::Result<()> {
        let mut len_byte = [0u8; 1];
        self.inner.read_exact(&mut len_byte)?;
        let len = len_byte[0] as usize * 16;
        if len == 0 {
            (self.on_metadata)(None);
            return Ok(());
        }
        let mut payload = vec![0u8; len];
        self.inner.read_exact(&mut payload)?;
        let metadata = String::from_utf8(payload)
            .ok()
            .map(|s| s.trim_end_matches('\0').to_string())
            .and_then(|s| s.parse::<IcyMetadata>().ok());
        (self.on_metadata)(metadata);
        Ok(())
    }
}

impl<R: Read> Read for IcyStrippingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.until_metadata == 0 {
            self.consume_metadata_block()?;
            self.until_metadata = self.metaint;
        }
        // The one invariant that avoids the upstream crate's bug: never ask
        // the inner reader for more than the SMALLER of the caller's buffer
        // and the audio bytes actually remaining before the next metadata
        // block, no matter how large `metaint` is.
        let want = buf.len().min(self.until_metadata);
        let n = self.inner.read(&mut buf[..want])?;
        self.until_metadata -= n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::io::Cursor;
    use std::sync::Arc;

    /// Builds a raw ICY-over-HTTP byte stream: `metaint` bytes of audio,
    /// then a length-prefixed metadata block, repeated for each entry in
    /// `blocks` (`None` = a zero-length block, `Some(s)` = literal bytes
    /// padded to a multiple of 16 with trailing NULs, as real servers do).
    fn build_stream(metaint: usize, audio_fill: u8, blocks: &[Option<&str>]) -> Vec<u8> {
        let mut out = Vec::new();
        for block in blocks {
            out.extend(std::iter::repeat_n(audio_fill, metaint));
            match block {
                None => out.push(0),
                Some(s) => {
                    let mut payload = s.as_bytes().to_vec();
                    while payload.len() % 16 != 0 {
                        payload.push(0);
                    }
                    let len_units = payload.len() / 16;
                    out.push(len_units as u8);
                    out.extend(payload);
                }
            }
        }
        out
    }

    fn read_all_in_chunks<R: Read>(mut r: R, chunk: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; chunk];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) => panic!("unexpected read error: {e}"),
            }
        }
        out
    }

    #[test]
    fn strips_metadata_and_preserves_audio_bytes() {
        let raw = build_stream(64, 0xAB, &[Some("StreamTitle='Track One';"), None, Some("StreamTitle='Track Two';")]);
        let titles: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let titles2 = Arc::clone(&titles);
        let reader = IcyStrippingReader::new(Cursor::new(raw), 64, move |m| {
            titles2.lock().push(m.and_then(|m| m.stream_title().map(str::to_string)));
        });
        let audio = read_all_in_chunks(reader, 4096);
        assert_eq!(audio.len(), 64 * 3);
        assert!(audio.iter().all(|&b| b == 0xAB));
        assert_eq!(
            *titles.lock(),
            vec![Some("Track One".to_string()), None, Some("Track Two".to_string())]
        );
    }

    /// Regression test for the exact upstream panic: `metaint` (analogous to
    /// SomaFM's real 45000) far exceeds the buffer size the caller reads
    /// with (analogous to Symphonia's ~32 KiB `MediaSourceStream` buffer).
    /// The buggy reader panics on this input; this one must not.
    #[test]
    fn survives_metaint_larger_than_read_buffer() {
        let metaint = 45_000;
        let raw = build_stream(metaint, 0x5A, &[Some("StreamTitle='Groove Salad';"), Some("StreamTitle='Next Track';")]);
        let reader = IcyStrippingReader::new(Cursor::new(raw), metaint, |_| {});
        // Read with a buffer smaller than metaint, exactly like Symphonia's
        // internal MediaSourceStream fill buffer.
        let audio = read_all_in_chunks(reader, 32_768);
        assert_eq!(audio.len(), metaint * 2);
        assert!(audio.iter().all(|&b| b == 0x5A));
    }

    #[test]
    fn survives_reads_much_smaller_than_metaint() {
        let raw = build_stream(100, 0x11, &[Some("StreamTitle='X';"), None]);
        let reader = IcyStrippingReader::new(Cursor::new(raw), 100, |_| {});
        // Adversarially tiny reads, smaller than both the metadata length
        // byte's neighborhood and the audio run.
        let audio = read_all_in_chunks(reader, 3);
        assert_eq!(audio.len(), 200);
        assert!(audio.iter().all(|&b| b == 0x11));
    }

    #[test]
    fn zero_length_metadata_block_dispatches_none() {
        let raw = build_stream(16, 0x22, &[None]);
        let calls = Arc::new(Mutex::new(0));
        let calls2 = Arc::clone(&calls);
        let reader = IcyStrippingReader::new(Cursor::new(raw), 16, move |m| {
            assert!(m.is_none());
            *calls2.lock() += 1;
        });
        let _ = read_all_in_chunks(reader, 16);
        assert_eq!(*calls.lock(), 1);
    }

    #[test]
    fn real_eof_mid_audio_propagates_as_ok_zero_not_a_panic() {
        // Truncated stream: audio run cut short, no trailing metadata byte.
        let mut raw = build_stream(64, 0x33, &[]);
        raw.extend(std::iter::repeat_n(0x33u8, 30)); // partial audio run only
        let reader = IcyStrippingReader::new(Cursor::new(raw), 64, |_| {});
        let audio = read_all_in_chunks(reader, 4096);
        assert_eq!(audio.len(), 30);
    }
}
