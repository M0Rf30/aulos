// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Positional byte access shared by the tag probe and the container layout
//! reader, and [`SparseFile`]: a file of which only some byte ranges are
//! loaded.
//!
//! The browser cannot seek a file cheaply (every read is an asynchronous
//! `Blob` slice), so the web scanner loads the head and the tail of each
//! file, which is where tags, stream headers and trailing tag blocks live,
//! into a [`SparseFile`] and probes that. A read into a range that was not
//! loaded fails and raises [`SparseFile::hole_flag`], so the caller can load
//! the whole file and probe again instead of trusting a partial result.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use symphonia::core::io::MediaSource;

/// Reading bytes at an absolute offset through a shared reference.
pub(crate) trait PositionalRead {
    /// Up to `len` bytes starting at `offset`: fewer at the end of the data
    /// (or where it is missing), none past it. Never fails.
    fn read_up_to(&self, offset: u64, len: usize) -> Vec<u8>;
}

impl PositionalRead for File {
    // Seek, then read, on a shared `&File`: portable where `read_at` /
    // `seek_read` are not. The file is never read concurrently here.
    fn read_up_to(&self, offset: u64, len: usize) -> Vec<u8> {
        let mut file = self;
        let mut buf = vec![0u8; len];
        let mut got = 0;
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return Vec::new();
        }
        while got < len {
            match file.read(&mut buf[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        buf.truncate(got);
        buf
    }
}

/// A file of `size` bytes with only some ranges loaded (see the module docs).
#[derive(Debug)]
pub struct SparseFile {
    size: u64,
    /// Loaded ranges, sorted by offset, never overlapping or adjacent.
    chunks: Vec<(u64, Vec<u8>)>,
    pos: u64,
    hole: Arc<AtomicBool>,
}

impl SparseFile {
    /// A file of `size` bytes with nothing loaded yet.
    #[must_use]
    pub fn new(size: u64) -> Self {
        Self {
            size,
            chunks: Vec::new(),
            pos: 0,
            hole: Arc::default(),
        }
    }

    /// A file whose `bytes` are all loaded.
    #[must_use]
    pub fn whole(bytes: Vec<u8>) -> Self {
        let mut file = Self::new(bytes.len() as u64);
        file.insert(0, bytes);
        file
    }

    /// The file's total size in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Whether every byte is loaded.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.size == 0 || self.chunks.len() == 1 && self.chunks[0].1.len() as u64 == self.size
    }

    /// Raised when a read touched bytes that are not loaded. Shared, so it
    /// stays readable after the file moved into a probe.
    #[must_use]
    pub fn hole_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.hole)
    }

    /// Loads `bytes` at `offset` (clipped to the file size), merging with
    /// the ranges it touches; where they overlap the new bytes win.
    pub fn insert(&mut self, offset: u64, mut bytes: Vec<u8>) {
        if offset >= self.size || bytes.is_empty() {
            return;
        }
        bytes.truncate((self.size - offset) as usize);
        let mut start = offset;
        let mut end = offset + bytes.len() as u64;
        let (touching, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.chunks)
            .into_iter()
            .partition(|(o, b)| *o <= end && o + b.len() as u64 >= start);
        self.chunks = rest;
        let mut merged = Vec::new();
        for (o, b) in &touching {
            start = start.min(*o);
            end = end.max(o + b.len() as u64);
        }
        merged.resize((end - start) as usize, 0);
        for (o, b) in touching {
            let at = (o - start) as usize;
            merged[at..at + b.len()].copy_from_slice(&b);
        }
        let at = (offset - start) as usize;
        merged[at..at + bytes.len()].copy_from_slice(&bytes);
        let index = self.chunks.partition_point(|(o, _)| *o < start);
        self.chunks.insert(index, (start, merged));
    }

    /// The chunk holding `offset` and where in it.
    fn chunk_at(&self, offset: u64) -> Option<(&[u8], usize)> {
        let index = self.chunks.partition_point(|(o, _)| *o <= offset);
        let (o, bytes) = self.chunks.get(index.checked_sub(1)?)?;
        let at = usize::try_from(offset - o).ok()?;
        (at < bytes.len()).then_some((bytes.as_slice(), at))
    }
}

impl PositionalRead for SparseFile {
    fn read_up_to(&self, offset: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = offset;
        let end = offset.saturating_add(len as u64).min(self.size);
        while at < end {
            let Some((bytes, i)) = self.chunk_at(at) else {
                self.hole.store(true, Ordering::Relaxed);
                break;
            };
            let take = (bytes.len() - i).min((end - at) as usize);
            out.extend_from_slice(&bytes[i..i + take]);
            at += take as u64;
        }
        out
    }
}

impl Read for SparseFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.size {
            return Ok(0);
        }
        let Some((bytes, i)) = self.chunk_at(self.pos) else {
            self.hole.store(true, Ordering::Relaxed);
            return Err(io::Error::other("this part of the file is not loaded"));
        };
        let n = (bytes.len() - i).min(buf.len());
        buf[..n].copy_from_slice(&bytes[i..i + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SparseFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.size.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        let target = target.ok_or_else(|| io::Error::other("seek before the start"))?;
        self.pos = target;
        Ok(target)
    }
}

impl MediaSource for SparseFile {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn reads_loaded_ranges_and_flags_holes() {
        let all = data(100);
        let mut f = SparseFile::new(100);
        f.insert(0, all[..10].to_vec());
        f.insert(90, all[90..].to_vec());
        assert!(!f.is_complete());
        let flag = f.hole_flag();

        assert_eq!(f.read_up_to(5, 5), all[5..10]);
        assert_eq!(f.read_up_to(92, 100), all[92..]);
        assert!(!flag.load(Ordering::Relaxed));

        // Stops at the hole, and says so.
        assert_eq!(f.read_up_to(8, 10), all[8..10]);
        assert!(flag.swap(false, Ordering::Relaxed));

        let mut buf = [0u8; 4];
        f.seek(SeekFrom::Start(50)).unwrap();
        assert!(f.read(&mut buf).is_err());
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn read_and_seek_follow_the_cursor() {
        let all = data(64);
        let mut f = SparseFile::new(64);
        f.insert(0, all[..16].to_vec());
        f.insert(48, all[48..].to_vec());
        let mut buf = [0u8; 4];
        assert_eq!(f.seek(SeekFrom::End(-4)).unwrap(), 60);
        f.read_exact(&mut buf).unwrap();
        assert_eq!(buf, all[60..]);
        assert_eq!(f.read(&mut buf).unwrap(), 0);
        f.seek(SeekFrom::Start(14)).unwrap();
        let mut two = [0u8; 2];
        f.read_exact(&mut two).unwrap();
        assert_eq!(two, all[14..16]);
        assert_eq!(f.seek(SeekFrom::Current(-2)).unwrap(), 14);
        assert!(f.seek(SeekFrom::Current(-20)).is_err());
        assert_eq!(f.byte_len(), Some(64));
    }

    #[test]
    fn overlapping_and_adjacent_ranges_merge() {
        let all = data(30);
        let mut f = SparseFile::new(30);
        f.insert(20, all[20..].to_vec());
        f.insert(0, all[..12].to_vec());
        assert!(!f.is_complete());
        f.insert(10, all[10..22].to_vec());
        assert!(f.is_complete());
        assert_eq!(f.read_up_to(0, 30), all);
        // Bytes past the end are clipped, nothing is lost.
        let mut g = SparseFile::new(10);
        g.insert(5, data(50));
        g.insert(0, data(5));
        assert!(g.is_complete());
    }

    #[test]
    fn whole_file_is_complete() {
        let f = SparseFile::whole(data(7));
        assert!(f.is_complete());
        assert_eq!(f.size(), 7);
        assert!(SparseFile::new(0).is_complete());
    }
}
