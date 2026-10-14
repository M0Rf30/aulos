// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Backend for the lactea visualizer: the command protocol between the UI
//! and the render thread, preset discovery, and the shuffled preset rotation.
//! The wgpu/lactea renderer itself is UI-side.

pub mod browser;
pub mod playlist;
pub mod presets;

use std::path::PathBuf;

/// Commands sent from the UI thread to the dedicated lactea render
/// thread (see `lactea_render_stream` in the UI crate). Drained via
/// `try_recv()` once per frame, before rendering.
#[derive(Debug, Clone)]
pub enum VizCommand {
    /// Advance to the next preset via the shuffled playlist (hard cut).
    /// Replaces the old `preset_signal: AtomicBool` flag.
    NextPreset,
    /// Load a specific preset file directly, outside the shuffled rotation,
    /// with a smooth transition.
    LoadPreset(PathBuf),
    /// Lock/unlock automatic preset transitions (hard/soft cuts driven by
    /// preset duration or beat detection). Manual switches — `NextPreset`
    /// and `LoadPreset` — are always executed regardless of lock state
    /// (per lactea's `PresetControl::set_preset_locked` semantics).
    SetLocked(bool),
    /// Adjust beat-reactivity sensitivity (typical range 0.0-2.0).
    SetBeatSensitivity(f32),
}

/// Interleaved stereo f32 samples read from the PCM ring per render frame:
/// 1152 samples = 576 stereo frames. `Engine::push_pcm_f32` accepts any
/// length, so this is only the read window the render loop asks
/// [`crate::player::pcm::PcmBuffer::read_since`] for.
pub const PCM_FEED_SAMPLES: usize = 576 * 2;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::pcm::PcmBuffer;

    /// One render period at 30fps/44.1kHz is ~1470 stereo frames: a read
    /// capped at `PCM_FEED_SAMPLES` must hand back a whole 1152-sample
    /// window.
    #[test]
    fn read_since_returns_a_whole_ring_window_for_a_30fps_period() {
        let mut buf = PcmBuffer::new(8192);
        let period: Vec<f32> = (0..2940).map(|i| i as f32).collect();
        buf.write_interleaved(&period, 2);
        let mut cursor = 0u64;
        let out = buf.read_since(&mut cursor, PCM_FEED_SAMPLES);
        assert_eq!(out.len(), PCM_FEED_SAMPLES);
        // Newest window, in order, ending on the last sample written.
        assert_eq!(out[0], (2940 - PCM_FEED_SAMPLES) as f32);
        assert_eq!(*out.last().unwrap(), 2939.0);
    }
}
