// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Decision logic and runtime state of the playback / desktop extras:
//! "stop after this track", party mode and auto-play queue top-up,
//! suspend/idle inhibition while playing, track-change notification
//! bookkeeping and the window-close policy.
//!
//! Everything here is a pure state transition over [`Player`], the config
//! and the track list; the UI turns the returned decisions into its own
//! asynchronous work (acquiring/releasing the inhibit lock, showing the
//! notification, minimizing the window).

use super::{ActiveBackend, PlaybackState, Player, party};
use crate::config::Config;
use crate::desktop::{inhibit, notify};
use crate::library::Track;
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// How long to wait before retrying a failed inhibit request.
pub const INHIBIT_RETRY: Duration = Duration::from_secs(60);

/// Where the suspend/idle inhibit lock currently stands.
enum InhibitPhase {
    Idle,
    /// A request is in flight.
    Acquiring,
    Held(inhibit::InhibitToken),
}

/// What the UI must do after [`PlaybackExtrasState::sync_inhibit`].
#[must_use = "an Acquire/Release decision does nothing unless the UI carries it out"]
pub enum InhibitStep {
    /// Nothing to do.
    None,
    /// Start an asynchronous [`inhibit::acquire`]; feed its result to
    /// [`PlaybackExtrasState::inhibit_acquired`].
    Acquire,
    /// Release this token in the background.
    Release(inhibit::InhibitToken),
}

/// What closing the main window should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// Keep playing in the background: minimize the window.
    Minimize,
    /// Flush the config and quit.
    Quit,
}

/// Runtime state for the extras (nothing here is persisted).
pub struct PlaybackExtrasState {
    rng: party::Rng,
    inhibit: InhibitPhase,
    /// Don't retry a failed inhibit before this instant.
    inhibit_retry_at: Option<Instant>,
    /// Server id of the last track notification, for `replaces_id`.
    notification_id: u32,
    /// Key of the last track a notification was shown for.
    last_notified: Option<String>,
    /// `(track id, upcoming)` a top-up already came up empty for, so an
    /// unfillable queue doesn't rescan the library on every tick.
    top_up_dry: Option<(i64, usize)>,
}

impl Default for PlaybackExtrasState {
    fn default() -> Self {
        Self {
            rng: party::Rng::seeded(),
            inhibit: InhibitPhase::Idle,
            inhibit_retry_at: None,
            notification_id: 0,
            last_notified: None,
            top_up_dry: None,
        }
    }
}

/// Toggles "stop after the current track" on the local backend.
pub fn toggle_stop_after(player: &mut Player) {
    if player.active_backend_type() != ActiveBackend::Local {
        return;
    }
    let on = !player.stop_after_current();
    // Arming only makes sense while something is playing;
    // disarming is always allowed.
    if !on || player.state() != PlaybackState::Stopped {
        player.set_stop_after_current(on);
    }
}

/// Adds `genre` to the party pool, or removes it when already present.
pub fn toggle_party_genre(genres: &mut Vec<String>, genre: String) {
    if let Some(pos) = genres.iter().position(|g| *g == genre) {
        genres.remove(pos);
    } else {
        genres.push(genre);
    }
}

/// Whether the suspend/idle inhibition should be active right now.
#[must_use]
pub fn wants_inhibit(enabled: bool, player: Option<&Player>) -> bool {
    player.is_some_and(|p| {
        inhibit::should_inhibit(
            enabled,
            p.state(),
            p.active_backend_type() == ActiveBackend::Local,
        )
    })
}

/// Closing the window: keep playing in the background (minimized) when that
/// is enabled, music is actually playing and there is a window to minimize,
/// otherwise quit.
#[must_use]
pub fn close_action(
    background_playback: bool,
    player: Option<&Player>,
    has_main_window: bool,
) -> CloseAction {
    let playing = player.is_some_and(|p| p.state() == PlaybackState::Playing);
    if background_playback && playing && has_main_window {
        CloseAction::Minimize
    } else {
        CloseAction::Quit
    }
}

impl PlaybackExtrasState {
    // -- Auto-play / party ---------------------------------------------

    /// Append more music when the queue is about to run out (auto-random /
    /// auto-similar) or party mode wants its look-ahead refilled.
    ///
    /// Runs while the *last* queued track plays, so the engine's gapless
    /// look-ahead carries straight on into what is appended here.
    pub fn top_up_queue(
        &mut self,
        player: Option<&mut Player>,
        current: Option<&Track>,
        all_tracks: &[Track],
        config: &Config,
    ) {
        let Some(player) = player else {
            return;
        };
        self.top_up(player, current, all_tracks, config);
    }

    fn top_up(
        &mut self,
        player: &mut Player,
        current: Option<&Track>,
        all_tracks: &[Track],
        config: &Config,
    ) {
        if player.active_backend_type() != ActiveBackend::Local
            || player.state() != PlaybackState::Playing
            || player.queue_is_empty()
            || player.stop_after_current()
        {
            return;
        }
        let Some(current) = current else {
            return;
        };
        // Radio and podcasts have their own (endless / per-feed) flow.
        if matches!(&*current.provider_id, "radio" | "podcast") {
            return;
        }
        let upcoming = player.upcoming_len();
        let Some(kind) = party::plan_top_up(
            upcoming,
            player.repeat_mode(),
            config.party_mode,
            config.auto_play_mode,
        ) else {
            return;
        };
        if self.top_up_dry == Some((current.id, upcoming)) {
            return;
        }

        let exclude: HashSet<i64> = player.queue().iter().map(|t| t.id).collect();
        let picks = party::continuation(
            kind,
            Some(current),
            all_tracks,
            &config.party_genres,
            &exclude,
            &mut self.rng,
        );
        if picks.is_empty() {
            self.top_up_dry = Some((current.id, upcoming));
            return;
        }
        self.top_up_dry = None;
        let added = picks.len();
        match player.queue_append(picks) {
            Ok(_) => tracing::debug!("auto-play: queued {added} track(s) ({kind:?})"),
            Err(e) => tracing::warn!("auto-play: failed to extend the queue: {e}"),
        }
    }

    /// The party genre pool changed: the upcoming queue was drawn from the
    /// old pool, so drop it and forget that a top-up came up empty. The
    /// caller follows up with [`Self::top_up_queue`].
    pub fn party_pool_changed(&mut self, player: Option<&mut Player>) {
        if let Some(player) = player
            && player.active_backend_type() == ActiveBackend::Local
        {
            player.queue_clear_upcoming();
        }
        self.top_up_dry = None;
    }

    /// Party mode was just switched on: make the upcoming queue party
    /// material. When nothing is playing, returns the party picks the UI
    /// must start playing (from index 0); otherwise the queue is refilled
    /// here and `None` is returned.
    pub fn start_party(
        &mut self,
        player: Option<&mut Player>,
        current: Option<&Track>,
        all_tracks: &[Track],
        config: &Config,
    ) -> Option<Vec<Track>> {
        self.top_up_dry = None;
        let player = player?;
        if player.active_backend_type() == ActiveBackend::Mpd {
            return None;
        }
        let idle = player.queue_is_empty() || player.state() == PlaybackState::Stopped;
        if idle {
            let picks = party::continuation(
                party::TopUp::Party,
                None,
                all_tracks,
                &config.party_genres,
                &HashSet::new(),
                &mut self.rng,
            );
            return (!picks.is_empty()).then_some(picks);
        }
        player.queue_clear_upcoming();
        self.top_up(player, current, all_tracks, config);
        None
    }

    // -- Inhibit -------------------------------------------------------

    /// Whether an inhibit lock is held or being taken (the UI then needs its
    /// slow housekeeping tick to release it after a pause/stop).
    #[must_use]
    pub fn inhibit_busy(&self) -> bool {
        !matches!(self.inhibit, InhibitPhase::Idle)
    }

    /// Forget a failed-request back-off, so the next sync retries at once.
    pub fn clear_inhibit_retry(&mut self) {
        self.inhibit_retry_at = None;
    }

    /// Decides whether to acquire/release the suspend+idle inhibition to
    /// match playback (`want`, see [`wants_inhibit`]).
    pub fn sync_inhibit(&mut self, want: bool, now: Instant) -> InhibitStep {
        match (&self.inhibit, want) {
            (InhibitPhase::Idle, true) => {
                if self.inhibit_retry_at.is_some_and(|at| now < at) {
                    return InhibitStep::None;
                }
                self.inhibit = InhibitPhase::Acquiring;
                InhibitStep::Acquire
            }
            (InhibitPhase::Held(_), false) => {
                match std::mem::replace(&mut self.inhibit, InhibitPhase::Idle) {
                    InhibitPhase::Held(token) => InhibitStep::Release(token),
                    _ => InhibitStep::None,
                }
            }
            _ => InhibitStep::None,
        }
    }

    /// Result of the request started by an [`InhibitStep::Acquire`]. Returns
    /// a token the UI must release right away when playback stopped while
    /// the request was in flight.
    #[must_use]
    pub fn inhibit_acquired(
        &mut self,
        token: Option<inhibit::InhibitToken>,
        wanted: bool,
        now: Instant,
    ) -> Option<inhibit::InhibitToken> {
        match token {
            Some(token) if wanted => {
                self.inhibit = InhibitPhase::Held(token);
                None
            }
            Some(token) => {
                self.inhibit = InhibitPhase::Idle;
                Some(token)
            }
            None => {
                self.inhibit = InhibitPhase::Idle;
                self.inhibit_retry_at = Some(now + INHIBIT_RETRY);
                None
            }
        }
    }

    // -- Notifications -------------------------------------------------

    /// Decides whether `track` becoming current deserves a desktop
    /// notification. When it does, records it as notified and returns the
    /// server id of the previous notification, to replace (`0` = none).
    pub fn begin_notification(
        &mut self,
        enabled: bool,
        focused: bool,
        playing: bool,
        track: &Track,
    ) -> Option<u32> {
        let key = notify::track_key(track);
        if !notify::should_notify(
            enabled,
            focused,
            playing,
            &key,
            self.last_notified.as_deref(),
        ) {
            return None;
        }
        self.last_notified = Some(key);
        Some(self.notification_id)
    }

    /// The notification server's id for the last notification, to replace.
    pub fn set_notification_id(&mut self, id: u32) {
        self.notification_id = id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inhibit_is_acquired_once_while_wanted() {
        let mut state = PlaybackExtrasState::default();
        let now = Instant::now();
        assert!(!state.inhibit_busy());
        assert!(matches!(
            state.sync_inhibit(true, now),
            InhibitStep::Acquire
        ));
        assert!(state.inhibit_busy());
        // The request is in flight: further syncs do nothing.
        assert!(matches!(state.sync_inhibit(true, now), InhibitStep::None));
        assert!(matches!(state.sync_inhibit(false, now), InhibitStep::None));
    }

    #[test]
    fn a_failed_request_backs_off_until_the_retry_instant() {
        let mut state = PlaybackExtrasState::default();
        let now = Instant::now();
        assert!(matches!(
            state.sync_inhibit(true, now),
            InhibitStep::Acquire
        ));
        assert!(state.inhibit_acquired(None, true, now).is_none());
        assert!(!state.inhibit_busy());
        assert!(matches!(state.sync_inhibit(true, now), InhibitStep::None));
        assert!(matches!(
            state.sync_inhibit(true, now + INHIBIT_RETRY),
            InhibitStep::Acquire
        ));
    }

    #[test]
    fn clearing_the_retry_allows_an_immediate_new_request() {
        let mut state = PlaybackExtrasState::default();
        let now = Instant::now();
        let _ = state.sync_inhibit(true, now);
        let _ = state.inhibit_acquired(None, true, now);
        state.clear_inhibit_retry();
        assert!(matches!(
            state.sync_inhibit(true, now),
            InhibitStep::Acquire
        ));
    }

    #[test]
    fn party_genres_toggle_in_and_out_of_the_pool() {
        let mut genres = vec!["rock".to_string()];
        toggle_party_genre(&mut genres, "jazz".into());
        assert_eq!(genres, ["rock", "jazz"]);
        toggle_party_genre(&mut genres, "rock".into());
        assert_eq!(genres, ["jazz"]);
    }

    #[test]
    fn closing_without_a_player_or_window_quits() {
        assert_eq!(close_action(true, None, true), CloseAction::Quit);
        assert_eq!(close_action(false, None, true), CloseAction::Quit);
        assert!(!wants_inhibit(true, None));
    }
}
