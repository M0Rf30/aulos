// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! In-place DSP filter stage trait and the software volume filter.
//!
//! [`AudioFilter`] is the seam every in-place audio-thread DSP stage
//! implements (volume, EQ, ...). [`VolumeFilter`] reads a live
//! `Arc<AtomicU8>` (0..=100) so the volume can be changed from the UI thread
//! without touching the audio thread's filter chain.
//!
//! This module intentionally omits a hardware-mixer seam (`Mixer`/`SoftwareMixer`)
//! and an ordered `FilterChain` runner: neither is wired into any live
//! pipeline here, since aulos's engine loop applies its filters directly.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// An in-place DSP stage over interleaved f32 samples.
///
/// The slice passed to [`Self::apply`] is the same length in and out; the
/// filter mutates it in place.
pub trait AudioFilter: Send {
    /// Human-readable name used for logging / debug.
    fn name(&self) -> &str;

    /// Apply the filter to `buf` in place.
    fn apply(&mut self, buf: &mut [f32]);
}

/// Software volume control (0..=100) read live from a shared atomic.
///
/// At `volume == 100` the filter short-circuits and returns immediately (no
/// multiply). The atomic is read with `Acquire` ordering so any preceding
/// `store(Release)` from another thread is visible.
pub struct VolumeFilter {
    volume: Arc<AtomicU8>,
}

impl VolumeFilter {
    pub fn new(volume: Arc<AtomicU8>) -> Self {
        Self { volume }
    }
}

impl AudioFilter for VolumeFilter {
    fn name(&self) -> &str {
        "volume"
    }

    fn apply(&mut self, buf: &mut [f32]) {
        let v = self.volume.load(Ordering::Acquire);
        if v == 100 {
            return;
        }
        let scale = v as f32 / 100.0;
        for s in buf.iter_mut() {
            *s *= scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ones(n: usize) -> Vec<f32> {
        vec![1.0f32; n]
    }

    // VolumeFilter: v=50 halves every sample.
    #[test]
    fn volume_filter_50_halves_buffer() {
        let vol = Arc::new(AtomicU8::new(50));
        let mut f = VolumeFilter::new(Arc::clone(&vol));
        let mut buf = ones(8);
        f.apply(&mut buf);
        for s in &buf {
            assert!((*s - 0.5).abs() < f32::EPSILON, "expected 0.5, got {s}");
        }
    }

    // VolumeFilter: v=100 is a no-op (early return, buffer unchanged).
    #[test]
    fn volume_filter_100_leaves_buffer_unchanged() {
        let vol = Arc::new(AtomicU8::new(100));
        let mut f = VolumeFilter::new(Arc::clone(&vol));
        let mut buf = ones(8);
        f.apply(&mut buf);
        for s in &buf {
            assert!((*s - 1.0).abs() < f32::EPSILON, "expected 1.0, got {s}");
        }
    }

    // VolumeFilter: v=0 zeros the buffer.
    #[test]
    fn volume_filter_0_silences_buffer() {
        let vol = Arc::new(AtomicU8::new(0));
        let mut f = VolumeFilter::new(Arc::clone(&vol));
        let mut buf = ones(4);
        f.apply(&mut buf);
        for s in &buf {
            assert!(s.abs() < f32::EPSILON, "expected 0.0, got {s}");
        }
    }
}
