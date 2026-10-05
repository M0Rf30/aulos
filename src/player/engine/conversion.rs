// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Shared sample format conversion utilities for audio output backends.
//!
//! No external dependencies beyond `std`.

use std::sync::mpsc::Receiver;

/// Convert f32 samples to s16le bytes, writing into the provided buffer.
/// The buffer is cleared and filled with the converted bytes.
pub fn samples_to_s16le_into(samples: &[f32], buf: &mut Vec<u8>) {
    buf.clear();
    buf.reserve(samples.len() * 2);
    for &s in samples {
        let v = f32_to_i16(s);
        buf.extend_from_slice(&v.to_le_bytes());
    }
}

/// Convert interleaved f32 PCM samples (range −1.0…+1.0) to little-endian
/// signed 16-bit bytes.
pub fn samples_to_s16le(samples: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(samples.len() * 2);
    samples_to_s16le_into(samples, &mut buf);
    buf
}

/// Clamp and scale a single f32 sample to `i16` range.
#[inline]
pub fn f32_to_i16(val: f32) -> i16 {
    (val.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Clamp and scale a single f32 sample to `i32` range.
#[inline]
pub fn f32_to_i32(val: f32) -> i32 {
    (f64::from(val.clamp(-1.0, 1.0)) * f64::from(i32::MAX)) as i32
}

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
    fn f32_to_i16_clamps_and_scales() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), i16::MAX);
        assert_eq!(f32_to_i16(-1.0), -i16::MAX);
        assert_eq!(f32_to_i16(2.0), i16::MAX);
        assert_eq!(f32_to_i16(-2.0), -i16::MAX);
    }

    #[test]
    fn f32_to_i32_clamps_and_scales() {
        assert_eq!(f32_to_i32(0.0), 0);
        assert_eq!(f32_to_i32(1.0), i32::MAX);
        assert_eq!(f32_to_i32(-1.0), -i32::MAX);
    }

    #[test]
    fn samples_to_s16le_round_trips_length() {
        let samples = [0.0f32, 0.5, -0.5, 1.0, -1.0];
        let bytes = samples_to_s16le(&samples);
        assert_eq!(bytes.len(), samples.len() * 2);
    }

    #[test]
    fn sample_buffer_underrun_yields_silence() {
        let (_tx, rx) = sync_channel::<Vec<f32>>(1);
        let mut buf = SampleBuffer::new(rx);
        assert_eq!(buf.next_sample(), 0.0);
    }

    #[test]
    fn sample_buffer_drains_then_underruns() {
        let (tx, rx) = sync_channel::<Vec<i16>>(1);
        tx.send(vec![1, 2, 3]).unwrap();
        let mut buf = SampleBuffer::new(rx);
        assert_eq!(buf.next_sample(), 1);
        assert_eq!(buf.next_sample(), 2);
        assert_eq!(buf.next_sample(), 3);
        // Underrun once the chunk and the channel are both exhausted.
        assert_eq!(buf.next_sample(), 0);
    }

    #[test]
    fn sample_buffer_refills_from_channel() {
        let (tx, rx) = sync_channel::<Vec<f32>>(2);
        tx.send(vec![1.0]).unwrap();
        tx.send(vec![2.0, 3.0]).unwrap();
        let mut buf = SampleBuffer::new(rx);
        assert_eq!(buf.next_sample(), 1.0);
        assert_eq!(buf.next_sample(), 2.0);
        assert_eq!(buf.next_sample(), 3.0);
    }
}
