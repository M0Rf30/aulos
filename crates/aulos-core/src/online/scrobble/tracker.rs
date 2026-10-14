// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Per-track play-time accounting.
//!
//! The app feeds [`PlayTracker::tick`] the playback position on every
//! playback tick; the tracker accumulates the time actually listened (seeks
//! don't count) and says when to announce "now playing" and when the track
//! has been played long enough to scrobble.

use super::is_scrobble_eligible;
use std::time::Duration;

/// Largest forward position jump still credited as continuous playback.
/// Bigger jumps are seeks and earn no play time.
const MAX_CREDITED_STEP: Duration = Duration::from_secs(5);
/// A backwards jump to within this of the start counts as the track
/// restarting (repeat-one / replay) rather than a seek.
const RESTART_WINDOW: Duration = Duration::from_secs(3);
/// ...provided the position moved back by more than this.
const RESTART_MIN_JUMP: Duration = Duration::from_secs(10);

#[derive(Debug, Default, Clone)]
pub struct PlayTracker {
    key: Option<String>,
    started_at: i64,
    last_pos: Duration,
    played: Duration,
    now_playing_sent: bool,
    scrobbled: bool,
}

/// What the caller should do after a tick.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickOutcome {
    /// Announce this track as now playing.
    pub send_now_playing: bool,
    /// Scrobble this track, with this start timestamp (unix seconds).
    pub scrobble_at: Option<i64>,
}

impl PlayTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the current track (e.g. playback stopped or scrobbling
    /// got disabled), so the next tick starts a fresh listen.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Time credited so far for the current track.
    pub fn played(&self) -> Duration {
        self.played
    }

    /// Advance the tracker. `key` identifies the track, `position` is the
    /// playback position, `duration` the track length (zero = unknown),
    /// `now` the current unix time and `allow_unknown` whether streams of
    /// unknown length may be scrobbled.
    pub fn tick(
        &mut self,
        key: &str,
        position: Duration,
        duration: Duration,
        now: i64,
        allow_unknown: bool,
    ) -> TickOutcome {
        let restarted = self.key.as_deref() == Some(key)
            && position < RESTART_WINDOW
            && self.last_pos > position + RESTART_MIN_JUMP;
        if self.key.as_deref() != Some(key) || restarted {
            self.key = Some(key.to_string());
            self.started_at = now - position.as_secs() as i64;
            self.last_pos = position;
            self.played = Duration::ZERO;
            self.now_playing_sent = false;
            self.scrobbled = false;
        } else {
            if position > self.last_pos {
                let step = position - self.last_pos;
                if step <= MAX_CREDITED_STEP {
                    self.played += step;
                }
            }
            self.last_pos = position;
        }

        let mut out = TickOutcome::default();
        if !self.now_playing_sent {
            self.now_playing_sent = true;
            out.send_now_playing = true;
        }
        if !self.scrobbled && is_scrobble_eligible(duration, self.played, allow_unknown) {
            self.scrobbled = true;
            out.scrobble_at = Some(self.started_at);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// Play `key` from `from` to `to` seconds in 1 s ticks, returning every
    /// scrobble timestamp emitted.
    fn play(
        t: &mut PlayTracker,
        key: &str,
        from: u64,
        to: u64,
        dur: u64,
        now0: i64,
    ) -> Vec<Option<i64>> {
        (from..=to)
            .map(|p| {
                t.tick(key, s(p), s(dur), now0 + p as i64, false)
                    .scrobble_at
            })
            .collect()
    }

    #[test]
    fn now_playing_once_per_track() {
        let mut t = PlayTracker::new();
        assert!(t.tick("a", s(0), s(200), 1000, false).send_now_playing);
        assert!(!t.tick("a", s(1), s(200), 1001, false).send_now_playing);
        assert!(t.tick("b", s(0), s(200), 1002, false).send_now_playing);
    }

    #[test]
    fn scrobbles_once_at_half() {
        let mut t = PlayTracker::new();
        let hits: Vec<_> = play(&mut t, "a", 0, 150, 200, 1000)
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(hits, vec![1000], "single scrobble stamped with start time");
    }

    #[test]
    fn seeking_forward_earns_no_credit() {
        let mut t = PlayTracker::new();
        t.tick("a", s(0), s(200), 1000, false);
        // Jump to 150s: only a seek, nothing credited.
        let o = t.tick("a", s(150), s(200), 1001, false);
        assert_eq!(o.scrobble_at, None);
        assert_eq!(t.played(), Duration::ZERO);
    }

    #[test]
    fn short_track_never_scrobbles() {
        let mut t = PlayTracker::new();
        assert!(play(&mut t, "a", 0, 29, 29, 0).iter().all(Option::is_none));
    }

    #[test]
    fn repeat_one_restart_scrobbles_again() {
        let mut t = PlayTracker::new();
        let first: usize = play(&mut t, "a", 0, 100, 100, 1000)
            .iter()
            .flatten()
            .count();
        assert_eq!(first, 1);
        // Same track restarts from 0.
        let second: Vec<_> = play(&mut t, "a", 0, 100, 100, 2000)
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(second, vec![2000]);
    }

    #[test]
    fn stream_needs_opt_in() {
        let mut t = PlayTracker::new();
        for p in 0..=120 {
            assert_eq!(t.tick("r", s(p), s(0), 0, false).scrobble_at, None);
        }
        let mut t = PlayTracker::new();
        let hits = (0..=120)
            .filter_map(|p| t.tick("r", s(p), s(0), 0, true).scrobble_at)
            .count();
        assert_eq!(hits, 1);
    }

    #[test]
    fn start_time_accounts_for_resume_position() {
        let mut t = PlayTracker::new();
        // Playback begins at 60 s into a 100 s track, at unix time 1000.
        t.tick("a", s(60), s(100), 1000, false);
        let mut hit = None;
        for p in 61..=120 {
            if let Some(ts) = t
                .tick("a", s(p), s(100), 1000 + (p - 60) as i64, false)
                .scrobble_at
            {
                hit = Some(ts);
            }
        }
        // Started 60 s before `now`; scrobble fires after 50 s of listening.
        assert_eq!(hit, Some(940));
    }
}
