// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Fade in / fade out on play, pause, stop and manual track skips
//! (Lollypop's `fade` setting).
//!
//! The ramp is applied in the realtime device callback (see
//! [`super::output::CpalOutput::set_fade`]) rather than on the decode thread:
//! the output buffers ~500 ms of audio ahead of the speaker, so a decode-side
//! ramp would only become audible half a second after the user pressed the
//! button. The decode thread only *requests* ramps through the shared
//! [`FadeControl`] and waits (on its own thread) for them to finish before
//! pausing / tearing the stream down.
//!
//! The ramp state is a linear "progress" in `0.0..=1.0`; the gain actually
//! applied is [`fade_gain`] of that progress (a quadratic curve, closer to
//! perceived loudness than a raw linear amplitude ramp).
//!
//! Not applied to DoP (DSD-over-PCM) output: scaling the carrier bits would
//! corrupt the stream, so that path stays bit-exact.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Longest fade accepted from the settings UI.
pub const MAX_FADE_SECS: f32 = 2.0;

/// Gain applied for a ramp `progress` in `0.0..=1.0` (0 = silent, 1 = unity).
///
/// Quadratic: `progress²`. Monotonic, exactly `0.0` at `0.0` and exactly
/// `1.0` at `1.0` (so a finished fade-in leaves the signal bit-exact).
pub fn fade_gain(progress: f32) -> f32 {
    let t = progress.clamp(0.0, 1.0);
    t * t
}

/// Shared control block between the engine/decode thread (writer) and the
/// device callback (reader). All fields are lock-free atomics.
#[derive(Debug)]
pub struct FadeControl {
    /// User-configured fade duration in seconds (`0.0` = fades disabled).
    secs_bits: AtomicU32,
    /// Where the ramp is heading: `1.0` (audible) or `0.0` (silent).
    target_bits: AtomicU32,
    /// Duration, in seconds, a full `0 -> 1` (or `1 -> 0`) ramp takes.
    ramp_secs_bits: AtomicU32,
    /// Progress a freshly built stream starts from (`0.0` = fade in).
    initial_bits: AtomicU32,
    /// Set by `play()`, consumed by the next PCM sink: start that stream
    /// silent and fade it in.
    fade_in_pending: AtomicBool,
}

impl Default for FadeControl {
    fn default() -> Self {
        Self {
            secs_bits: AtomicU32::new(0f32.to_bits()),
            target_bits: AtomicU32::new(1f32.to_bits()),
            ramp_secs_bits: AtomicU32::new(0f32.to_bits()),
            initial_bits: AtomicU32::new(1f32.to_bits()),
            fade_in_pending: AtomicBool::new(false),
        }
    }
}

impl FadeControl {
    /// A control block with fades disabled and unity gain.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Set the configured fade duration (clamped to `0..=MAX_FADE_SECS`).
    pub fn set_secs(&self, secs: f32) {
        let secs = if secs.is_finite() {
            secs.clamp(0.0, MAX_FADE_SECS)
        } else {
            0.0
        };
        self.secs_bits.store(secs.to_bits(), Ordering::Release);
    }

    /// The configured fade duration in seconds.
    pub fn secs(&self) -> f32 {
        f32::from_bits(self.secs_bits.load(Ordering::Acquire))
    }

    /// Whether fades are enabled at all.
    pub fn enabled(&self) -> bool {
        self.secs() > 0.0
    }

    /// Start ramping toward `target` (`0.0` or `1.0`) over `secs` seconds.
    pub fn ramp_to(&self, target: f32, secs: f32) {
        self.ramp_secs_bits
            .store(secs.max(0.0).to_bits(), Ordering::Release);
        self.target_bits
            .store(target.clamp(0.0, 1.0).to_bits(), Ordering::Release);
    }

    /// Ramp back to full volume over the configured duration.
    pub fn fade_in(&self) {
        self.ramp_to(1.0, self.secs());
    }

    /// Ramp down to silence over the configured duration.
    pub fn fade_out(&self) {
        self.ramp_to(0.0, self.secs());
    }

    /// Current ramp target.
    pub fn target(&self) -> f32 {
        f32::from_bits(self.target_bits.load(Ordering::Acquire))
    }

    /// Duration of the current ramp in seconds.
    pub fn ramp_secs(&self) -> f32 {
        f32::from_bits(self.ramp_secs_bits.load(Ordering::Acquire))
    }

    /// Progress a stream built right now should start from.
    pub fn initial(&self) -> f32 {
        f32::from_bits(self.initial_bits.load(Ordering::Acquire))
    }

    /// Ask the next PCM sink to start silent and fade in.
    pub fn request_fade_in(&self) {
        self.fade_in_pending
            .store(self.enabled(), Ordering::Release);
    }

    /// Consume a pending fade-in request (`true` if one was pending).
    pub fn take_fade_in_pending(&self) -> bool {
        self.fade_in_pending.swap(false, Ordering::AcqRel)
    }

    /// Prepare for a stream that is about to be built: start silent and ramp
    /// up when `fade_in`, otherwise start at unity.
    pub fn arm_stream_start(&self, fade_in: bool) {
        if fade_in && self.enabled() {
            self.initial_bits.store(0f32.to_bits(), Ordering::Release);
            self.fade_in();
        } else {
            self.initial_bits.store(1f32.to_bits(), Ordering::Release);
            self.ramp_to(1.0, 0.0);
        }
    }
}

/// Per-stream ramp state living inside the device callback closure.
///
/// Call [`Self::begin`] once at the top of each callback period, then
/// [`Self::next_gain`] once per *sample* (interleaved): every channel of a
/// frame gets the same gain, and the ramp advances once per frame.
#[derive(Debug)]
pub struct FadeRamp {
    control: Arc<FadeControl>,
    progress: f32,
    target: f32,
    /// Progress change per frame while ramping.
    step: f32,
    frame_rate: f32,
    channels: usize,
    channel_idx: usize,
    gain: f32,
    unity: bool,
}

impl FadeRamp {
    /// `frame_rate` is the device sample rate (frames per second).
    pub fn new(control: Arc<FadeControl>, frame_rate: u32, channels: usize) -> Self {
        let progress = control.initial().clamp(0.0, 1.0);
        Self {
            progress,
            target: control.target(),
            step: 1.0,
            frame_rate: frame_rate.max(1) as f32,
            channels: channels.max(1),
            channel_idx: 0,
            gain: fade_gain(progress),
            unity: progress >= 1.0 && control.target() >= 1.0,
            control,
        }
    }

    /// Re-read the shared target/duration. Call at the start of a callback.
    pub fn begin(&mut self) {
        self.target = self.control.target();
        let secs = self.control.ramp_secs();
        self.step = step_per_frame(secs, self.frame_rate);
        self.unity = self.progress >= 1.0 && self.target >= 1.0;
    }

    /// Gain for the next interleaved sample.
    #[inline]
    pub fn next_gain(&mut self) -> f32 {
        if self.unity {
            return 1.0;
        }
        if self.channel_idx == 0 {
            self.progress = advance_progress(self.progress, self.target, self.step);
            self.gain = fade_gain(self.progress);
        }
        self.channel_idx += 1;
        if self.channel_idx >= self.channels {
            self.channel_idx = 0;
        }
        self.gain
    }

    /// Current progress (test/diagnostic helper).
    pub fn progress(&self) -> f32 {
        self.progress
    }
}

/// Progress delta per frame for a ramp lasting `secs` at `frame_rate`.
/// A zero (or negative) duration jumps straight to the target.
pub fn step_per_frame(secs: f32, frame_rate: f32) -> f32 {
    if secs <= 0.0 || frame_rate <= 0.0 {
        1.0
    } else {
        (1.0 / (secs * frame_rate)).min(1.0)
    }
}

/// Move `progress` toward `target` by at most `step`, never overshooting.
pub fn advance_progress(progress: f32, target: f32, step: f32) -> f32 {
    if progress < target {
        (progress + step).min(target)
    } else if progress > target {
        (progress - step).max(target)
    } else {
        progress
    }
}

/// Apply a ramp to an interleaved buffer in place (used by tests and as a
/// reference implementation of what the device callback does per sample).
pub fn ramp_buffer(ramp: &mut FadeRamp, buf: &mut [f32]) {
    ramp.begin();
    for s in buf.iter_mut() {
        *s *= ramp.next_gain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_curve_endpoints_are_exact() {
        assert_eq!(fade_gain(0.0), 0.0);
        assert_eq!(fade_gain(1.0), 1.0);
        assert_eq!(fade_gain(-3.0), 0.0);
        assert_eq!(fade_gain(7.0), 1.0);
    }

    #[test]
    fn gain_curve_is_monotonic_and_below_linear() {
        let mut prev = -1.0;
        for i in 0..=100 {
            let p = i as f32 / 100.0;
            let g = fade_gain(p);
            assert!(g >= prev, "not monotonic at {p}");
            assert!(
                g <= p + f32::EPSILON,
                "quadratic curve must not exceed linear"
            );
            prev = g;
        }
    }

    #[test]
    fn step_and_advance_reach_target_in_expected_frames() {
        let rate = 1000.0;
        let step = step_per_frame(0.5, rate); // 500 frames
        let mut p = 0.0;
        let mut frames = 0;
        while p < 1.0 {
            p = advance_progress(p, 1.0, step);
            frames += 1;
            assert!(frames <= 501, "ramp overran");
        }
        assert!((499..=501).contains(&frames), "took {frames} frames");
        // Never overshoots, and ramps back down symmetrically.
        assert_eq!(advance_progress(0.999, 1.0, 0.5), 1.0);
        assert_eq!(advance_progress(0.1, 0.0, 0.5), 0.0);
        assert_eq!(advance_progress(0.5, 0.5, 0.1), 0.5);
    }

    #[test]
    fn zero_duration_ramp_is_instant() {
        assert_eq!(step_per_frame(0.0, 48_000.0), 1.0);
        assert_eq!(
            advance_progress(0.0, 1.0, step_per_frame(0.0, 48_000.0)),
            1.0
        );
    }

    #[test]
    fn fade_in_stream_start_is_silent_then_reaches_unity() {
        let ctl = FadeControl::new();
        ctl.set_secs(0.01); // 10 ms
        ctl.arm_stream_start(true);
        let mut ramp = FadeRamp::new(ctl, 1000, 2); // 10 frames
        let mut buf = vec![1.0f32; 2 * 20];
        ramp_buffer(&mut ramp, &mut buf);
        // First frame is already above silence but far from unity.
        assert!(buf[0] < 0.1);
        // Both channels of every frame share a gain.
        for frame in buf.chunks(2) {
            assert_eq!(frame[0], frame[1]);
        }
        // Monotonic non-decreasing and ends at exactly unity.
        assert!(buf.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(*buf.last().unwrap(), 1.0);
        assert_eq!(ramp.progress(), 1.0);
    }

    #[test]
    fn fade_out_reaches_silence_and_stays_there() {
        let ctl = FadeControl::new();
        ctl.set_secs(0.01);
        ctl.arm_stream_start(false);
        let mut ramp = FadeRamp::new(ctl.clone(), 1000, 1);
        let mut warm = vec![1.0f32; 5];
        ramp_buffer(&mut ramp, &mut warm);
        assert!(
            warm.iter().all(|&s| s == 1.0),
            "unity until a fade is requested"
        );

        ctl.fade_out();
        let mut buf = vec![1.0f32; 30];
        ramp_buffer(&mut ramp, &mut buf);
        assert!(buf.windows(2).all(|w| w[1] <= w[0]));
        assert!(buf[0] > 0.5);
        assert_eq!(*buf.last().unwrap(), 0.0);

        // Fading back in from silence works too (resume after pause).
        ctl.fade_in();
        let mut up = vec![1.0f32; 30];
        ramp_buffer(&mut ramp, &mut up);
        assert_eq!(*up.last().unwrap(), 1.0);
        assert!(up[0] < 0.1);
    }

    #[test]
    fn disabled_fade_never_arms_a_fade_in() {
        let ctl = FadeControl::new();
        ctl.request_fade_in();
        assert!(
            !ctl.take_fade_in_pending(),
            "secs == 0 must not request a fade"
        );
        ctl.set_secs(0.3);
        ctl.request_fade_in();
        assert!(ctl.take_fade_in_pending());
        assert!(!ctl.take_fade_in_pending(), "request is consumed once");
    }

    #[test]
    fn set_secs_clamps_and_rejects_garbage() {
        let ctl = FadeControl::new();
        ctl.set_secs(99.0);
        assert_eq!(ctl.secs(), MAX_FADE_SECS);
        ctl.set_secs(-1.0);
        assert_eq!(ctl.secs(), 0.0);
        ctl.set_secs(f32::NAN);
        assert_eq!(ctl.secs(), 0.0);
        assert!(!ctl.enabled());
    }
}
