// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! PCM tap feeding the visualizer.

/// Shared PCM ring buffer for audio tapping.
///
/// The writer (the local engine's cpal output callback, or — once wired —
/// an MPD PipeWire capture thread) pushes stereo-interleaved samples in;
/// the visualizer render thread pulls out whatever is new since its own
/// cursor. Only one writer is ever active at a time, but reader and writer
/// always run on different threads, so every access is through a lock
/// (`Arc<std::sync::Mutex<PcmBuffer>>` at the call sites) — this type
/// itself holds no lock.
pub struct PcmBuffer {
    /// Circular buffer of interleaved stereo f32 samples.
    buffer: Vec<f32>,
    /// Write position in the buffer: the index the *next* pushed sample
    /// lands at. Always equal to `total_written % capacity`.
    write_pos: usize,
    /// Total capacity (number of f32 samples, i.e. stereo frames × 2).
    capacity: usize,
    /// Monotonic count of stereo-interleaved f32 samples ever written
    /// (never decremented, never wrapped in practice — at 44.1kHz stereo
    /// this takes millions of years to overflow a `u64`). This is what
    /// lets `read_since` tell "nothing new" apart from "reader fell behind
    /// the ring" without any wraparound ambiguity: it's real-valued,
    /// unlike `write_pos` which is only ever a position mod `capacity`.
    total_written: u64,
}

impl PcmBuffer {
    /// Create a new PCM buffer. `capacity` is in f32 samples
    /// (stereo-interleaved, so `capacity / 2` stereo frames).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            buffer: vec![0.0; capacity],
            write_pos: 0,
            capacity,
            total_written: 0,
        }
    }

    /// Write interleaved `samples` with `channels` channels (`channels`
    /// clamped to a minimum of 1), converting to stereo as they're pushed:
    /// mono is duplicated to L/R, 2-channel passes through unchanged, and
    /// any wider layout keeps only channels 0/1 (front-left/front-right)
    /// and drops the rest.
    ///
    /// Converting at write time — rather than trusting every reader to
    /// know the source channel count — is what lets `render_frame` always
    /// call `push_pcm_f32(.., 2)` correctly regardless of whether the
    /// track underneath is mono, stereo, or multichannel.
    pub fn write_interleaved(&mut self, samples: &[f32], channels: usize) {
        if samples.is_empty() {
            return;
        }
        match channels.max(1) {
            1 => {
                for &s in samples {
                    self.push(s);
                    self.push(s);
                }
            }
            2 => {
                for &s in samples {
                    self.push(s);
                }
            }
            n => {
                for frame in samples.chunks_exact(n) {
                    self.push(frame[0]);
                    self.push(frame[1]);
                }
            }
        }
    }

    #[inline]
    fn push(&mut self, sample: f32) {
        self.buffer[self.write_pos] = sample;
        self.write_pos = (self.write_pos + 1) % self.capacity;
        self.total_written += 1;
    }

    /// Stereo-interleaved samples written since `*cursor`, advancing
    /// `*cursor` to match. Returns at most `max` samples — the most
    /// recent `max` when more than that arrived since the last call (or
    /// when `*cursor` has fallen so far behind the ring that not all of
    /// the gap is still available), never more than the ring's own
    /// `capacity`. Empty when nothing new has arrived since `*cursor`.
    pub fn read_since(&self, cursor: &mut u64, max: usize) -> Vec<f32> {
        let max = max.min(self.capacity);
        let available = self.total_written.saturating_sub(*cursor);
        let count = available.min(max as u64) as usize;
        if count == 0 {
            // Nothing new — but still snap a stale/ahead cursor up to the
            // current write head so the next call's `available` reflects
            // only genuinely new data, not leftover skew.
            *cursor = self.total_written;
            return Vec::new();
        }

        // `write_pos` always equals `total_written % capacity` (they're
        // incremented in lockstep by `push`), so walking back `count`
        // slots from `write_pos` finds the ring position of the oldest
        // sample we're about to return.
        let start_pos = (self.write_pos + self.capacity - count) % self.capacity;
        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            result.push(self.buffer[(start_pos + i) % self.capacity]);
        }
        *cursor = self.total_written;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_interleaved_duplicates_mono_to_stereo() {
        let mut buf = PcmBuffer::new(16);
        buf.write_interleaved(&[1.0, 2.0, 3.0], 1);
        let mut cursor = 0u64;
        assert_eq!(
            buf.read_since(&mut cursor, 16),
            vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0]
        );
    }

    #[test]
    fn write_interleaved_passes_stereo_through_unchanged() {
        let mut buf = PcmBuffer::new(16);
        buf.write_interleaved(&[1.0, -1.0, 2.0, -2.0], 2);
        let mut cursor = 0u64;
        assert_eq!(buf.read_since(&mut cursor, 16), vec![1.0, -1.0, 2.0, -2.0]);
    }

    #[test]
    fn write_interleaved_keeps_only_front_left_right_from_multichannel() {
        let mut buf = PcmBuffer::new(16);
        // Two 4-channel frames (FL, FR, RL, RR); only FL/FR should survive.
        buf.write_interleaved(&[1.0, 2.0, 99.0, 99.0, 3.0, 4.0, 99.0, 99.0], 4);
        let mut cursor = 0u64;
        assert_eq!(buf.read_since(&mut cursor, 16), vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn read_since_returns_empty_once_caught_up() {
        let mut buf = PcmBuffer::new(16);
        buf.write_interleaved(&[1.0, 2.0], 2);
        let mut cursor = 0u64;
        assert_eq!(buf.read_since(&mut cursor, 16), vec![1.0, 2.0]);
        assert!(buf.read_since(&mut cursor, 16).is_empty());
    }

    #[test]
    fn read_since_returns_only_samples_written_after_the_cursor() {
        let mut buf = PcmBuffer::new(16);
        let mut cursor = 0u64;
        buf.write_interleaved(&[1.0, 2.0], 2);
        let _ = buf.read_since(&mut cursor, 16);
        buf.write_interleaved(&[3.0, 4.0], 2);
        assert_eq!(buf.read_since(&mut cursor, 16), vec![3.0, 4.0]);
    }

    #[test]
    fn read_since_wraps_around_the_ring() {
        let mut buf = PcmBuffer::new(4); // 2 stereo frames
        let mut cursor = 0u64;
        buf.write_interleaved(&[1.0, 2.0, 3.0, 4.0], 2); // fills exactly
        let _ = buf.read_since(&mut cursor, 4);
        // One more frame wraps, overwriting the oldest.
        buf.write_interleaved(&[5.0, 6.0], 2);
        assert_eq!(buf.read_since(&mut cursor, 4), vec![5.0, 6.0]);
    }

    #[test]
    fn read_since_clamps_a_cursor_that_fell_behind_the_ring() {
        let mut buf = PcmBuffer::new(4);
        // Write far more than the ring holds without ever reading —
        // simulates a reader that stalled for a long time.
        for i in 0..100 {
            buf.write_interleaved(&[i as f32, i as f32], 2);
        }
        let mut cursor = 0u64; // never advanced — badly behind
        let out = buf.read_since(&mut cursor, 4);
        // Only the most recent 2 stereo frames, not all 100.
        assert_eq!(out, vec![98.0, 98.0, 99.0, 99.0]);
        assert_eq!(cursor, 200);
    }
}
