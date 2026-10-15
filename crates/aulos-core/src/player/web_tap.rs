// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Clock arithmetic of the visualizer's PCM tap in the browser.
//!
//! The browser backend decodes and schedules audio seconds ahead of what is
//! heard, so the visualizer cannot be fed at decode time: it would draw the
//! future. The tap instead follows the *audible* clock -- the context time of
//! the sample leaving the speakers -- and, on every tick, hands over the
//! stretch of already scheduled audio that was played since the last one.
//! This module is that bookkeeping only (no JS objects), so it is tested
//! natively.

#![cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser backend runs the tap")
)]

use std::ops::Range;
use std::time::Duration;

/// How often the tap hands over audio while the visualizer is on screen.
pub const TICK: Duration = Duration::from_millis(16);

/// Longest stretch (seconds) one tick hands over. After a stall (a throttled
/// tab, a long frame) only the newest part is still worth drawing.
const MAX_WINDOW: f64 = 0.1;

/// The context time of the sample being heard right now.
///
/// `now` is the context clock, `stamp` the `contextTime` of
/// `AudioContext.getOutputTimestamp()` (the frame the output device is
/// playing; absent or zero until the browser has one), `base` and `output`
/// the context's `baseLatency` and `outputLatency` (zero where the browser
/// does not report them). Without a usable stamp the clock minus the two
/// latencies is the best estimate.
pub fn heard_time(now: f64, stamp: Option<f64>, base: f64, output: f64) -> f64 {
    match stamp {
        Some(t) if t > 0.0 && t <= now => t,
        _ => (now - base - output).max(0.0),
    }
}

/// How far the tap has handed audio over, in heard context time.
#[derive(Debug, Default)]
pub struct Cursor(Option<f64>);

impl Cursor {
    /// Forgets the position: the next [`advance`](Self::advance) starts
    /// afresh at the present, so audio played while nobody watched is never
    /// replayed.
    pub fn reset(&mut self) {
        self.0 = None;
    }

    /// The stretch `(from, to)` of context time heard since the previous
    /// call, or `None` when nothing new was (the clock is paused, or this is
    /// the first call since [`reset`](Self::reset)).
    pub fn advance(&mut self, heard: f64) -> Option<(f64, f64)> {
        let Some(previous) = self.0 else {
            self.0 = Some(heard);
            return None;
        };
        if heard <= previous {
            return None;
        }
        self.0 = Some(heard);
        Some((previous.max(heard - MAX_WINDOW), heard))
    }
}

/// The frames of a chunk that lie in the context time `[from, to)`.
///
/// The chunk starts at context time `start` and holds `frames` frames of
/// `rate` Hz. Both ends round the same way, so two windows that meet at a
/// time `t` split a chunk (or two adjacent chunks) without a gap or an
/// overlap.
pub fn frames_within(start: f64, frames: usize, rate: f64, from: f64, to: f64) -> Range<usize> {
    let index = |t: f64| ((t - start) * rate).round().clamp(0.0, frames as f64) as usize;
    let (first, last) = (index(from), index(to));
    first..last.max(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heard_time_prefers_the_output_stamp() {
        assert_eq!(heard_time(10.0, Some(9.9), 0.01, 0.05), 9.9);
    }

    #[test]
    fn heard_time_without_a_stamp_subtracts_the_latencies() {
        let t = heard_time(10.0, None, 0.01, 0.05);
        assert!((t - 9.94).abs() < 1e-9);
    }

    #[test]
    fn heard_time_ignores_an_unusable_stamp() {
        // Zero before the first render; a stamp ahead of the clock is bogus.
        let expected = 10.0 - 0.01;
        assert!((heard_time(10.0, Some(0.0), 0.01, 0.0) - expected).abs() < 1e-9);
        assert!((heard_time(10.0, Some(11.0), 0.01, 0.0) - expected).abs() < 1e-9);
    }

    #[test]
    fn heard_time_never_goes_negative() {
        assert_eq!(heard_time(0.01, None, 0.02, 0.05), 0.0);
    }

    #[test]
    fn first_advance_only_sets_the_position() {
        let mut cursor = Cursor::default();
        assert_eq!(cursor.advance(5.0), None);
        assert_eq!(cursor.advance(5.016), Some((5.0, 5.016)));
    }

    #[test]
    fn a_paused_clock_hands_over_nothing() {
        let mut cursor = Cursor::default();
        cursor.advance(5.0);
        assert_eq!(cursor.advance(5.0), None);
        assert_eq!(cursor.advance(4.9), None);
        // Resuming continues where it stopped.
        assert_eq!(cursor.advance(5.01), Some((5.0, 5.01)));
    }

    #[test]
    fn a_long_stall_is_clamped_to_the_newest_audio() {
        let mut cursor = Cursor::default();
        cursor.advance(5.0);
        let (from, to) = cursor.advance(9.0).expect("time passed");
        assert_eq!(to, 9.0);
        assert!((to - from - MAX_WINDOW).abs() < 1e-9);
    }

    #[test]
    fn reset_drops_the_backlog() {
        let mut cursor = Cursor::default();
        cursor.advance(5.0);
        cursor.reset();
        assert_eq!(cursor.advance(7.0), None);
        assert_eq!(cursor.advance(7.016), Some((7.0, 7.016)));
    }

    #[test]
    fn frames_within_clips_to_the_chunk() {
        // 1 s chunk of 100 frames at 100 Hz starting at t = 2.
        assert_eq!(frames_within(2.0, 100, 100.0, 2.25, 2.5), 25..50);
        assert_eq!(frames_within(2.0, 100, 100.0, 1.0, 2.1), 0..10);
        assert_eq!(frames_within(2.0, 100, 100.0, 2.9, 5.0), 90..100);
    }

    #[test]
    fn frames_within_is_empty_outside_the_chunk() {
        assert!(frames_within(2.0, 100, 100.0, 0.0, 1.5).is_empty());
        assert!(frames_within(2.0, 100, 100.0, 3.5, 4.0).is_empty());
    }

    #[test]
    fn consecutive_windows_cover_every_frame_once() {
        // 48 kHz, windows at an awkward 16.3 ms pace across two chunks that
        // meet at 1.0 + 4800 / 48000.
        let rate = 48_000.0;
        let first = (1.0, 4800);
        let second = (1.0 + 4800.0 / rate, 24_000);
        let mut taken = [0usize; 2];
        let mut t = 1.0;
        while t < 1.3 {
            let next = t + 0.0163;
            taken[0] += frames_within(first.0, first.1, rate, t, next).len();
            taken[1] += frames_within(second.0, second.1, rate, t, next).len();
            t = next;
        }
        // The loop stops somewhere inside the second chunk; everything before
        // the end of the first one has been taken exactly once.
        assert_eq!(taken[0], 4800);
        assert_eq!(taken[1], ((t - second.0) * rate).round() as usize);
    }
}
