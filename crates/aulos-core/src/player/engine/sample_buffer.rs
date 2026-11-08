// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The sample queue read by cpal output callbacks.
//!
//! Aulos pauses and flushes by pausing/dropping the cpal stream itself, so it
//! needs none of the pause/flush-generation machinery of
//! `playcore::convert::SampleBuffer` (which rmpd's multi-output engine uses).

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
}

impl<T: Default + Copy> SampleBuffer<T> {
    /// Create a new buffer backed by the receiving end of a `sync_channel`.
    pub fn new(rx: Receiver<Vec<T>>) -> Self {
        Self {
            rx,
            buffer: Vec::new(),
            pos: 0,
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

    #[test]
    fn underrun_yields_silence() {
        let (_tx, rx) = sync_channel::<Vec<f32>>(1);
        let mut buf = SampleBuffer::new(rx);
        assert_eq!(buf.next_sample(), 0.0);
    }

    #[test]
    fn drains_then_underruns() {
        let (tx, rx) = sync_channel::<Vec<i16>>(1);
        tx.send(vec![1, 2, 3]).unwrap();
        let mut buf = SampleBuffer::new(rx);
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
        let mut buf = SampleBuffer::new(rx);
        assert_eq!(buf.next_sample(), 1.0);
        assert_eq!(buf.next_sample(), 2.0);
        assert_eq!(buf.next_sample(), 3.0);
    }
}
