// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! 10-band parametric equalizer DSP implemented as an [`AudioFilter`].
//!
//! Uses cascaded second-order IIR (biquad) peaking-EQ filters, one per band.
//! Coefficients and bypass state are shared via atomics so the audio thread
//! never blocks on a mutex when the UI adjusts a slider.
//!
//! [`AudioFilter`]: crate::player::engine::filter::AudioFilter

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::equalizer::BAND_FREQUENCIES;

/// Number of EQ bands (fixed at 10).
pub const NUM_BANDS: usize = 10;

/// Bandwidth of each peaking EQ filter in octaves.
const BANDWIDTH_OCTAVES: f32 = 1.0;

// ---------------------------------------------------------------------------
// Biquad coefficients
// ---------------------------------------------------------------------------

/// Second-order IIR (biquad) filter coefficients in Direct Form I.
///
/// Transfer function:
///   H(z) = (b0 + b1·z⁻¹ + b2·z⁻²) / (a0 + a1·z⁻¹ + a2·z⁻²)
///
/// Stored pre-normalized (divided by a0).
#[derive(Debug, Clone, Copy)]
pub struct BiquadCoeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoeffs {
    /// Identity (pass-through) coefficients.
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Pack two f32 coefficients into one u64 (`a` in the high 32 bits, `b`
    /// in the low 32 bits) for atomic storage. Five coefficients need three
    /// such slots: `[0] = (b0, b1)`, `[1] = (b2, a1)`, `[2] = (a2, unused)`.
    fn pack_pair(a: f32, b: f32) -> u64 {
        let a_bits = a.to_bits() as u64;
        let b_bits = b.to_bits() as u64;
        (a_bits << 32) | b_bits
    }

    fn unpack_pair(packed: u64) -> (f32, f32) {
        let a = f32::from_bits((packed >> 32) as u32);
        let b = f32::from_bits(packed as u32);
        (a, b)
    }

    /// Store coefficients into three atomic u64 slots.
    pub fn store(&self, slots: &[AtomicU64; 3]) {
        slots[0].store(Self::pack_pair(self.b0, self.b1), Ordering::Release);
        slots[1].store(Self::pack_pair(self.b2, self.a1), Ordering::Release);
        slots[2].store(Self::pack_pair(self.a2, 0.0), Ordering::Release);
    }

    /// Load coefficients from three atomic u64 slots.
    pub fn load(slots: &[AtomicU64; 3]) -> Self {
        let (b0, b1) = Self::unpack_pair(slots[0].load(Ordering::Acquire));
        let (b2, a1) = Self::unpack_pair(slots[1].load(Ordering::Acquire));
        let (a2, _) = Self::unpack_pair(slots[2].load(Ordering::Acquire));
        Self { b0, b1, b2, a1, a2 }
    }
}

/// Compute peaking EQ biquad coefficients.
///
/// Based on the Audio EQ Cookbook by Robert Bristow-Johnson.
///
/// # Arguments
/// - `freq_hz`: center frequency of the band
/// - `gain_db`: boost/cut in decibels (0 = unity)
/// - `sample_rate`: audio sample rate in Hz
/// - `bandwidth_octaves`: filter bandwidth in octaves
pub fn compute_peaking_eq(
    freq_hz: f32,
    gain_db: f32,
    sample_rate: f32,
    bandwidth_octaves: f32,
) -> BiquadCoeffs {
    if gain_db.abs() < 0.001 {
        return BiquadCoeffs::IDENTITY;
    }

    let a = 10.0_f32.powf(gain_db / 40.0); // sqrt(10^(dB/20))
    let w0 = 2.0 * std::f32::consts::PI * freq_hz / sample_rate;
    let sin_w0 = w0.sin();
    let cos_w0 = w0.cos();
    // alpha = sin(w0) * sinh(ln(2)/2 * BW * w0/sin(w0))
    let alpha = sin_w0 * (2.0_f32.ln() / 2.0 * bandwidth_octaves * w0 / sin_w0).sinh();

    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos_w0;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha / a;

    // Normalize by a0
    BiquadCoeffs {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

// ---------------------------------------------------------------------------
// Per-band biquad filter state (per channel)
// ---------------------------------------------------------------------------

/// Direct Form II Transposed biquad state for one channel.
#[derive(Default, Clone)]
struct BiquadState {
    z1: f32,
    z2: f32,
}

impl BiquadState {
    fn process(&mut self, input: f32, c: &BiquadCoeffs) -> f32 {
        let output = c.b0 * input + self.z1;
        self.z1 = c.b1 * input - c.a1 * output + self.z2;
        self.z2 = c.b2 * input - c.a2 * output;
        output
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Shared coefficients for all 10 bands
// ---------------------------------------------------------------------------

/// Shared atomic storage for EQ coefficients across all bands.
/// Each band uses 3 AtomicU64 slots (see `BiquadCoeffs::store/load`).
pub type SharedCoeffs = Arc<[[AtomicU64; 3]; NUM_BANDS]>;

/// Create a new shared coefficients array initialized to identity.
pub fn new_shared_coeffs() -> SharedCoeffs {
    Arc::new(std::array::from_fn(|_| {
        let slots: [AtomicU64; 3] = std::array::from_fn(|_| AtomicU64::new(0));
        BiquadCoeffs::IDENTITY.store(&slots);
        slots
    }))
}

// ---------------------------------------------------------------------------
// EqController — UI-side handle
// ---------------------------------------------------------------------------

/// Controller for adjusting EQ parameters from the UI thread.
///
/// All operations are lock-free (atomic stores) so they never block audio.
#[derive(Clone)]
pub struct EqController {
    coeffs: SharedCoeffs,
    bypass: Arc<AtomicBool>,
    /// Current sample rate — needed to recompute coefficients.
    sample_rate: f32,
}

impl EqController {
    /// Create a new controller with default sample rate.
    pub fn new(coeffs: SharedCoeffs, bypass: Arc<AtomicBool>, sample_rate: f32) -> Self {
        Self {
            coeffs,
            bypass,
            sample_rate,
        }
    }

    /// Set the gain for a single band and recompute its coefficients.
    pub fn set_band(&self, index: usize, gain_db: f32) {
        if index >= NUM_BANDS {
            return;
        }
        let c = compute_peaking_eq(
            BAND_FREQUENCIES[index],
            gain_db,
            self.sample_rate,
            BANDWIDTH_OCTAVES,
        );
        c.store(&self.coeffs[index]);
    }

    /// Set all 10 bands at once.
    pub fn set_all(&self, gains: &[f32; NUM_BANDS]) {
        for (i, &gain_db) in gains.iter().enumerate() {
            let c = compute_peaking_eq(
                BAND_FREQUENCIES[i],
                gain_db,
                self.sample_rate,
                BANDWIDTH_OCTAVES,
            );
            c.store(&self.coeffs[i]);
        }
    }

    /// Enable or disable (bypass) the EQ.
    pub fn set_enabled(&self, enabled: bool) {
        self.bypass.store(!enabled, Ordering::Release);
    }

    /// Check if EQ is currently enabled.
    pub fn is_enabled(&self) -> bool {
        !self.bypass.load(Ordering::Acquire)
    }

    /// Update sample rate (e.g. when a new track with different rate starts).
    /// This does NOT recompute coefficients — call `set_all` after if needed.
    pub fn set_sample_rate(&mut self, rate: f32) {
        self.sample_rate = rate;
    }
}

// ---------------------------------------------------------------------------
// EqFilter — AudioFilter adapter
// ---------------------------------------------------------------------------

/// An [`AudioFilter`](crate::player::engine::filter::AudioFilter) that applies
/// a 10-band parametric EQ to a whole interleaved buffer per call.
///
/// Reads coefficients from shared atomics (lock-free) and applies cascaded
/// biquad filters to each sample. When bypassed, the buffer passes through
/// unchanged.
pub struct EqFilter {
    coeffs: SharedCoeffs,
    bypass: Arc<AtomicBool>,
    /// Per-band, per-channel filter state.
    /// Indexed as `states[band][channel]`.
    states: Vec<Vec<BiquadState>>,
    /// Number of audio channels.
    channels: std::num::NonZero<u16>,
    /// Current sample index within a frame (for interleaved channel tracking).
    channel_idx: u16,
}

impl EqFilter {
    /// Create a new EQ filter for a stream with the given channel count.
    pub fn new(
        channels: std::num::NonZero<u16>,
        coeffs: SharedCoeffs,
        bypass: Arc<AtomicBool>,
    ) -> Self {
        let states = (0..NUM_BANDS)
            .map(|_| vec![BiquadState::default(); channels.get() as usize])
            .collect();

        Self {
            coeffs,
            bypass,
            states,
            channels,
            channel_idx: 0,
        }
    }

    /// Reset all filter states (e.g. after a seek).
    pub fn reset_states(&mut self) {
        for band in &mut self.states {
            for ch in band.iter_mut() {
                ch.reset();
            }
        }
    }
}

impl crate::player::engine::filter::AudioFilter for EqFilter {
    fn name(&self) -> &str {
        "eq"
    }

    fn apply(&mut self, buf: &mut [f32]) {
        // Bypass: pass through unchanged.
        if self.bypass.load(Ordering::Relaxed) {
            return;
        }

        // Snapshot coefficients once per buffer instead of once per sample:
        // the UI updates them at most a few times a second, so re-reading
        // 30 atomics per sample bought nothing but overhead in this
        // per-callback hot path.
        let coeffs: [BiquadCoeffs; NUM_BANDS] =
            std::array::from_fn(|band_idx| BiquadCoeffs::load(&self.coeffs[band_idx]));

        for sample in buf.iter_mut() {
            let ch = self.channel_idx as usize;
            self.channel_idx = (self.channel_idx + 1) % self.channels.get();

            // Cascade through all 10 bands.
            let mut out = *sample;
            for (band_idx, c) in coeffs.iter().enumerate() {
                out = self.states[band_idx][ch].process(out, c);
            }
            *sample = out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::engine::filter::AudioFilter;

    #[test]
    fn identity_coefficients_pass_through() {
        let c = BiquadCoeffs::IDENTITY;
        let mut state = BiquadState::default();
        // Identity filter should pass samples through unchanged.
        for i in 0..100 {
            let input = (i as f32) * 0.01;
            let output = state.process(input, &c);
            assert!(
                (output - input).abs() < 1e-6,
                "sample {i}: {output} != {input}"
            );
        }
    }

    #[test]
    fn coefficients_pack_unpack_roundtrip() {
        let c = compute_peaking_eq(1000.0, 6.0, 44100.0, 1.0);
        let slots: [AtomicU64; 3] = std::array::from_fn(|_| AtomicU64::new(0));
        c.store(&slots);
        let loaded = BiquadCoeffs::load(&slots);
        assert!((c.b0 - loaded.b0).abs() < 1e-6);
        assert!((c.b1 - loaded.b1).abs() < 1e-6);
        assert!((c.b2 - loaded.b2).abs() < 1e-6);
        assert!((c.a1 - loaded.a1).abs() < 1e-6);
        assert!((c.a2 - loaded.a2).abs() < 1e-6);
    }

    #[test]
    fn zero_gain_produces_identity() {
        let c = compute_peaking_eq(1000.0, 0.0, 44100.0, 1.0);
        assert!((c.b0 - 1.0).abs() < 1e-6);
        assert!(c.b1.abs() < 1e-6);
        assert!(c.b2.abs() < 1e-6);
        assert!(c.a1.abs() < 1e-6);
        assert!(c.a2.abs() < 1e-6);
    }

    #[test]
    fn eq_filter_bypass_passes_buffer_through_unchanged() {
        let coeffs = new_shared_coeffs();
        let bypass = Arc::new(AtomicBool::new(true));
        let channels = std::num::NonZero::new(2u16).unwrap();
        let mut filter = EqFilter::new(channels, coeffs, bypass);

        let mut buf: Vec<f32> = (0..100).map(|i| i as f32 * 0.01).collect();
        let expected = buf.clone();
        filter.apply(&mut buf);
        assert_eq!(buf, expected, "bypassed EqFilter must not modify samples");
    }

    #[test]
    fn eq_filter_identity_coefficients_pass_through() {
        // new_shared_coeffs() initializes every band to BiquadCoeffs::IDENTITY.
        let coeffs = new_shared_coeffs();
        let bypass = Arc::new(AtomicBool::new(false));
        let channels = std::num::NonZero::new(2u16).unwrap();
        let mut filter = EqFilter::new(channels, coeffs, bypass);

        let mut buf: Vec<f32> = (0..100).map(|i| i as f32 * 0.01).collect();
        let expected = buf.clone();
        filter.apply(&mut buf);
        for (i, (out, input)) in buf.iter().zip(expected.iter()).enumerate() {
            assert!((out - input).abs() < 1e-5, "sample {i}: {out} != {input}");
        }
    }

    #[test]
    fn eq_filter_name_is_eq() {
        let coeffs = new_shared_coeffs();
        let bypass = Arc::new(AtomicBool::new(false));
        let channels = std::num::NonZero::new(2u16).unwrap();
        let filter = EqFilter::new(channels, coeffs, bypass);
        assert_eq!(filter.name(), "eq");
    }
}
