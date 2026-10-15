// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Clock bookkeeping of the browser's Web Audio output.
//!
//! The output schedules decoded chunks back to back on the `AudioContext`
//! timeline; nothing reports a "position" or "track ended", so both are
//! derived from the context clock and from where each chunk was placed. This
//! module is that arithmetic only (no JS objects), so it is tested natively.
//!
//! Time is counted in *frames at the context's sample rate* since `base`, the
//! context time of frame 0, so chunks placed one after the other are exactly
//! contiguous (gapless) instead of accumulating floating-point drift. A
//! [`Span`] is one track's stretch of that timeline; the front span is the
//! track being heard, and the later ones are tracks already scheduled behind
//! it (gapless).

#![cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser backend drives the timeline")
)]

use std::collections::VecDeque;

/// One track's place on the timeline.
#[derive(Debug, Clone, PartialEq)]
struct Span {
    /// Frame its first scheduled sample sits at.
    origin: u64,
    /// Track position (seconds) of that sample: non-zero after a seek.
    offset: f64,
    /// Track length (seconds), when the container tells it.
    duration: Option<f64>,
    /// Frame its audio ends at, once the decoder ran out of data.
    end: Option<u64>,
}

/// Where the scheduled audio sits on the context clock.
#[derive(Debug, Clone)]
pub struct Timeline {
    /// The context's sample rate (frames per second).
    rate: f64,
    /// Context time (seconds) of frame 0.
    base: f64,
    /// Frames scheduled so far: where the next chunk goes.
    frames: u64,
    spans: VecDeque<Span>,
}

impl Timeline {
    /// An empty timeline for a context running at `rate` Hz.
    pub fn new(rate: u32) -> Self {
        Self {
            rate: f64::from(rate.max(1)),
            base: 0.0,
            frames: 0,
            spans: VecDeque::new(),
        }
    }

    /// Context time of `frame`.
    fn time_of(&self, frame: u64) -> f64 {
        self.base + frame as f64 / self.rate
    }

    /// Forgets everything scheduled.
    pub fn clear(&mut self) {
        self.frames = 0;
        self.spans.clear();
    }

    /// Starts over with one track whose sample at `offset` seconds is heard
    /// `lead` seconds after `now` (the previous audio must be stopped).
    pub fn start(&mut self, now: f64, lead: f64, offset: f64, duration: Option<f64>) {
        self.clear();
        self.base = now + lead;
        self.spans.push_back(Span {
            origin: 0,
            offset,
            duration,
            end: None,
        });
    }

    /// Context time the next chunk must start at. When the decoder fell
    /// behind the clock (the chunk would start in the past) the timeline
    /// restarts `lead` seconds after `now`: the audible gap is not counted as
    /// played.
    pub fn next_start(&mut self, now: f64, lead: f64) -> f64 {
        let at = self.time_of(self.frames);
        if at < now {
            self.base += now + lead - at;
            return now + lead;
        }
        at
    }

    /// A chunk of `frames` frames was scheduled at [`Self::next_start`].
    pub fn commit(&mut self, frames: u64) {
        self.frames += frames;
    }

    /// Frames scheduled so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Seconds of audio scheduled beyond `now`.
    pub fn ahead(&self, now: f64) -> f64 {
        (self.time_of(self.frames) - now).max(0.0)
    }

    /// The decoder of the last track ran out of data: its audio ends at the
    /// frames scheduled so far.
    pub fn end_feed(&mut self) {
        let frames = self.frames;
        if let Some(last) = self.spans.back_mut()
            && last.end.is_none()
        {
            last.end = Some(frames);
        }
    }

    /// Whether the last track has all its audio scheduled.
    pub fn feed_ended(&self) -> bool {
        self.spans.back().is_some_and(|s| s.end.is_some())
    }

    /// Continues with another track right behind the last one (gapless).
    pub fn begin_next(&mut self, duration: Option<f64>) {
        self.end_feed();
        self.spans.push_back(Span {
            origin: self.frames,
            offset: 0.0,
            duration,
            end: None,
        });
    }

    /// Whether a track is scheduled behind the one being heard.
    pub fn has_upcoming(&self) -> bool {
        self.spans.len() > 1
    }

    /// Drops the tracks scheduled behind the one being heard. Returns the
    /// frame their audio started at: chunks from there on must be stopped.
    pub fn cancel_upcoming(&mut self) -> Option<u64> {
        let first = self.spans.get(1)?.origin;
        self.spans.truncate(1);
        self.frames = first;
        Some(first)
    }

    /// Moves on to the next track once its first frame is reached. Returns
    /// whether the heard track changed.
    pub fn advance(&mut self, now: f64) -> bool {
        let mut passed = false;
        while self
            .spans
            .get(1)
            .is_some_and(|next| now >= self.time_of(next.origin))
        {
            self.spans.pop_front();
            passed = true;
        }
        passed
    }

    /// Whether the last track's audio has all been heard.
    pub fn ended(&self, now: f64) -> bool {
        self.spans.len() == 1
            && self.spans[0]
                .end
                .is_some_and(|end| now >= self.time_of(end))
    }

    /// Position (seconds) within the track being heard.
    pub fn position(&self, now: f64) -> f64 {
        let Some(span) = self.spans.front() else {
            return 0.0;
        };
        let mut heard = (now - self.time_of(span.origin)).max(0.0);
        if let Some(end) = span.end {
            heard = heard.min((end - span.origin) as f64 / self.rate);
        }
        let position = span.offset + heard;
        match span.duration {
            Some(duration) => position.min(duration),
            None => position,
        }
    }

    /// Length (seconds) of the track being heard, when known.
    pub fn duration(&self) -> Option<f64> {
        self.spans.front().and_then(|s| s.duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Schedules `secs` seconds of audio in half-second chunks.
    fn feed(t: &mut Timeline, now: f64, secs: u64) {
        for _ in 0..secs * 2 {
            let _ = t.next_start(now, 0.05);
            t.commit(u64::from(RATE / 2));
        }
    }

    #[test]
    fn position_follows_the_clock_after_the_lead() {
        let mut t = Timeline::new(RATE);
        t.start(10.0, 0.05, 0.0, Some(100.0));
        feed(&mut t, 10.0, 4);
        assert_eq!(t.position(10.0), 0.0);
        assert!((t.position(10.05 + 1.5) - 1.5).abs() < 1e-9);
        assert!((t.ahead(10.0) - 4.05).abs() < 1e-9);
    }

    #[test]
    fn chunks_are_exactly_contiguous() {
        let mut t = Timeline::new(44_100);
        t.start(0.0, 0.0, 0.0, None);
        let mut expected = 0u64;
        for _ in 0..1000 {
            let at = t.next_start(0.0, 0.0);
            assert_eq!(at, expected as f64 / 44_100.0);
            t.commit(22_050);
            expected += 22_050;
        }
    }

    #[test]
    fn a_seek_offsets_the_position() {
        let mut t = Timeline::new(RATE);
        t.start(5.0, 0.0, 30.0, Some(100.0));
        feed(&mut t, 5.0, 2);
        assert!((t.position(6.0) - 31.0).abs() < 1e-9);
    }

    #[test]
    fn the_position_stops_at_the_end_of_the_audio() {
        let mut t = Timeline::new(RATE);
        t.start(0.0, 0.0, 0.0, None);
        feed(&mut t, 0.0, 2);
        t.end_feed();
        assert!(t.feed_ended());
        assert!((t.position(10.0) - 2.0).abs() < 1e-9);
        assert!(!t.ended(1.9));
        assert!(t.ended(2.0));
    }

    #[test]
    fn the_position_never_passes_the_duration() {
        let mut t = Timeline::new(RATE);
        t.start(0.0, 0.0, 0.0, Some(1.5));
        feed(&mut t, 0.0, 2);
        assert_eq!(t.position(1.9), 1.5);
    }

    #[test]
    fn a_gapless_track_takes_over_at_the_boundary() {
        let mut t = Timeline::new(RATE);
        t.start(0.0, 0.0, 0.0, Some(2.0));
        feed(&mut t, 0.0, 2);
        t.begin_next(Some(3.0));
        assert!(t.has_upcoming());
        feed(&mut t, 0.0, 3);
        // Still the first track one frame before the boundary...
        assert!(!t.advance(2.0 - 1.0 / f64::from(RATE)));
        assert_eq!(t.duration(), Some(2.0));
        // ...and the second from the boundary on, at its start.
        assert!(t.advance(2.0));
        assert_eq!(t.duration(), Some(3.0));
        assert!(!t.has_upcoming());
        assert!((t.position(2.5) - 0.5).abs() < 1e-9);
        assert!(!t.ended(4.9));
    }

    #[test]
    fn the_last_track_ends_only_once_its_data_is_complete() {
        let mut t = Timeline::new(RATE);
        t.start(0.0, 0.0, 0.0, Some(1.0));
        feed(&mut t, 0.0, 1);
        assert!(!t.ended(5.0), "still decoding: nothing ended");
        t.end_feed();
        assert!(!t.ended(0.5));
        assert!(t.ended(1.0));
        assert!(!t.advance(1.0), "no track follows: nothing to move on to");
    }

    #[test]
    fn cancelling_the_upcoming_track_rewinds_the_schedule() {
        let mut t = Timeline::new(RATE);
        t.start(0.0, 0.0, 0.0, Some(2.0));
        feed(&mut t, 0.0, 2);
        t.begin_next(Some(3.0));
        feed(&mut t, 0.0, 1);
        assert_eq!(t.cancel_upcoming(), Some(u64::from(RATE) * 2));
        assert_eq!(t.frames(), u64::from(RATE) * 2);
        assert!(!t.has_upcoming());
        assert!(t.feed_ended(), "the first track still ends where it did");
        assert_eq!(t.cancel_upcoming(), None);
    }

    #[test]
    fn an_underrun_restarts_the_schedule_and_does_not_count_the_gap() {
        let mut t = Timeline::new(RATE);
        t.start(0.0, 0.05, 0.0, None);
        feed(&mut t, 0.0, 1);
        // The decoder stalls: at t = 3 s only 1.05 s of audio had been placed.
        let at = t.next_start(3.0, 0.05);
        assert!((at - 3.05).abs() < 1e-9);
        t.commit(u64::from(RATE / 2));
        // The next chunk follows the restarted one without a gap.
        assert!((t.next_start(3.1, 0.05) - 3.55).abs() < 1e-9);
        // The audio heard so far is 1 s (first part) + what played since 3.05.
        assert!((t.position(3.05) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn an_empty_timeline_has_no_position() {
        let t = Timeline::new(RATE);
        assert_eq!(t.position(7.0), 0.0);
        assert_eq!(t.duration(), None);
        assert!(!t.ended(7.0));
    }
}
