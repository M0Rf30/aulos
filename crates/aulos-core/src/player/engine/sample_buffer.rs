// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The sample queue read by cpal output callbacks.
//!
//! Pausing and stopping work on the cpal stream itself; a seek instead bumps
//! a flush generation so the callback discards the queued (old-position)
//! audio on its next period and the new position is audible at once, without
//! rebuilding the stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;

/// A bounded sample buffer fed from a `SyncSender`/`Receiver` channel.
///
/// Used inside cpal output callbacks to decouple the decoder thread from the
/// real-time audio thread. When the current buffer is exhausted the next
/// chunk is pulled from the channel; if no data is available the buffer
/// produces silence (the `Default` value for `T`). This never blocks —
/// blocking inside a cpal callback is a correctness bug (glitches/crashes on
/// some backends).
pub struct SampleBuffer<T> {
    rx: Receiver<Vec<T>>,
    buffer: Vec<T>,
    pos: usize,
    flush_gen: Arc<AtomicU64>,
    seen_gen: u64,
}

impl<T: Default + Copy> SampleBuffer<T> {
    /// Create a new buffer backed by the receiving end of a `sync_channel`.
    /// `flush_gen` is the output's seek generation (see [`Self::poll_flush`]).
    pub fn new(rx: Receiver<Vec<T>>, flush_gen: Arc<AtomicU64>) -> Self {
        let seen_gen = flush_gen.load(Ordering::Acquire);
        Self {
            rx,
            buffer: Vec::new(),
            pos: 0,
            flush_gen,
            seen_gen,
        }
    }

    /// Drop the current chunk and everything still queued when the output's
    /// flush generation moved (a seek). Call once per callback period; the
    /// drain is non-blocking and only frees chunks the refill path would
    /// have freed anyway.
    pub fn poll_flush(&mut self) {
        let generation = self.flush_gen.load(Ordering::Acquire);
        if generation != self.seen_gen {
            self.seen_gen = generation;
            self.buffer.clear();
            self.pos = 0;
            while self.rx.try_recv().is_ok() {}
        }
    }

    /// Return the next sample, refilling from the channel when the current
    /// chunk is exhausted. Returns `T::default()` (silence) on underrun.
    #[inline]
    pub fn next_sample(&mut self) -> T {
        if self.pos >= self.buffer.len()
            && let Ok(new_samples) = self.rx.try_recv()
        {
            self.buffer = new_samples;
            self.pos = 0;
        }
        if self.pos < self.buffer.len() {
            let val = self.buffer[self.pos];
            self.pos += 1;
            val
        } else {
            T::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    fn no_flush() -> Arc<AtomicU64> {
        Arc::new(AtomicU64::new(0))
    }

    #[test]
    fn underrun_yields_silence() {
        let (_tx, rx) = sync_channel::<Vec<f32>>(1);
        let mut buf = SampleBuffer::new(rx, no_flush());
        assert_eq!(buf.next_sample(), 0.0);
    }

    #[test]
    fn drains_then_underruns() {
        let (tx, rx) = sync_channel::<Vec<i16>>(1);
        tx.send(vec![1, 2, 3]).unwrap();
        let mut buf = SampleBuffer::new(rx, no_flush());
        assert_eq!(buf.next_sample(), 1);
        assert_eq!(buf.next_sample(), 2);
        assert_eq!(buf.next_sample(), 3);
        assert_eq!(buf.next_sample(), 0);
    }

    #[test]
    fn refills_from_channel() {
        let (tx, rx) = sync_channel::<Vec<f32>>(2);
        tx.send(vec![1.0]).unwrap();
        tx.send(vec![2.0, 3.0]).unwrap();
        let mut buf = SampleBuffer::new(rx, no_flush());
        assert_eq!(buf.next_sample(), 1.0);
        assert_eq!(buf.next_sample(), 2.0);
        assert_eq!(buf.next_sample(), 3.0);
    }

    #[test]
    fn flush_discards_current_and_queued_chunks() {
        let (tx, rx) = sync_channel::<Vec<f32>>(4);
        let generation = no_flush();
        let mut buf = SampleBuffer::new(rx, generation.clone());
        tx.send(vec![1.0; 4]).unwrap();
        tx.send(vec![2.0; 4]).unwrap();
        assert_eq!(buf.next_sample(), 1.0);
        generation.fetch_add(1, Ordering::Release);
        buf.poll_flush();
        assert_eq!(buf.next_sample(), 0.0);
        tx.send(vec![3.0; 2]).unwrap();
        assert_eq!(buf.next_sample(), 3.0);
        assert_eq!(buf.next_sample(), 3.0);
        assert_eq!(buf.next_sample(), 0.0);
    }

    #[test]
    fn poll_without_a_flush_keeps_playing() {
        let (tx, rx) = sync_channel::<Vec<f32>>(2);
        let mut buf = SampleBuffer::new(rx, no_flush());
        tx.send(vec![1.0, 2.0]).unwrap();
        buf.poll_flush();
        assert_eq!(buf.next_sample(), 1.0);
        buf.poll_flush();
        assert_eq!(buf.next_sample(), 2.0);
    }

    #[test]
    fn repeated_flushes_drain_each_time() {
        let (tx, rx) = sync_channel::<Vec<f32>>(4);
        let generation = no_flush();
        let mut buf = SampleBuffer::new(rx, generation.clone());
        for value in [1.0, 2.0, 3.0] {
            tx.send(vec![value; 4]).unwrap();
            generation.fetch_add(1, Ordering::Release);
            buf.poll_flush();
            assert_eq!(buf.next_sample(), 0.0);
        }
        tx.send(vec![4.0]).unwrap();
        assert_eq!(buf.next_sample(), 4.0);
    }
}
