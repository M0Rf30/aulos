// SPDX-License-Identifier: GPL-3.0

//! A pure, backend-free playback queue model.
//!
//! [`PlayQueue`] owns everything about "what plays next" — the track list
//! in play order, the current position, shuffle state, and repeat mode —
//! with zero knowledge of any actual audio backend. [`super::Player`]
//! drives a real backend based on what this type reports; that split keeps
//! the actually-tricky queue logic (shuffle bookkeeping, repeat-mode edge
//! cases, index adjustment on edits) unit-testable without spinning up any
//! audio device.
//!
//! # Shuffle bookkeeping
//!
//! Three parallel vectors describe the queue:
//! - `unshuffled`: the canonical (never-shuffled) track order — what
//!   turning shuffle back off restores.
//! - `order`: the actual current PLAY order (materialized, so
//!   `Player::queue()` can still hand back a plain `&[Track]`). Identical
//!   in content and sequence to `unshuffled` whenever `shuffle` is `false`.
//! - `origin`: same length as `order`; `origin[i]` is the index into
//!   `unshuffled` that `order[i]` corresponds to. Lets `set_shuffle(false)`
//!   relocate the current track back into canonical order by position,
//!   with no need to compare `Track`s by value (the type has no
//!   `PartialEq` and duplicate tracks in one queue are legal).
//!
//! `append`/`insert_next` extend all three in lockstep (new tracks always
//! land at the end of `unshuffled`); `remove` shrinks all three and shifts
//! every `origin` value past the removed canonical index down by one;
//! `move_item` (a pure play-order reorder) only ever touches `order` +
//! `origin`, never `unshuffled` — reordering the shuffled play sequence
//! shouldn't redefine what "unshuffled" means.

use crate::config::RepeatMode;
use crate::library::Track;
use std::time::Duration;

/// Restart the current track instead of moving to the previous one when
/// more than this much of it has already played — matches the "previous
/// track" convention almost every media player uses.
const RESTART_THRESHOLD: Duration = Duration::from_secs(3);

/// What [`PlayQueue::remove`] did, from the caller's perspective.
#[derive(Debug, Clone)]
pub struct RemoveOutcome {
    /// The track that was removed.
    pub removed: Track,
    /// Whether the removed entry was the currently-playing one.
    pub was_current: bool,
    /// The new current track after the removal (`None` if the queue is
    /// now empty). Only meaningful when `was_current` is `true` — when
    /// it's `false` the currently-playing track didn't change.
    pub new_current: Option<Track>,
}

/// What [`PlayQueue::previous`] wants the caller to do.
#[derive(Debug, Clone)]
// Short-lived return value, consumed immediately — boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
pub enum PreviousAction {
    /// Seek the current track back to the start rather than changing
    /// tracks (played > 3s in, or at the queue's start with no repeat).
    Restart,
    /// Move to (and play) this track.
    Track(Track),
}

/// A pure, backend-free playback queue: tracks in play order, the current
/// position, shuffle state (with the pre-shuffle order preserved for
/// un-shuffling), and repeat mode.
#[derive(Debug, Clone)]
pub struct PlayQueue {
    unshuffled: Vec<Track>,
    order: Vec<Track>,
    /// `origin[i]` = index into `unshuffled` that `order[i]` came from.
    origin: Vec<usize>,
    index: usize,
    shuffle: bool,
    repeat: RepeatMode,
    rng: XorShift64,
}

impl Default for PlayQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayQueue {
    /// An empty queue with shuffle off and no repeat.
    pub fn new() -> Self {
        Self {
            unshuffled: Vec::new(),
            order: Vec::new(),
            origin: Vec::new(),
            index: 0,
            shuffle: false,
            repeat: RepeatMode::None,
            rng: XorShift64::seeded(),
        }
    }

    // -- Accessors --

    /// Whether the queue holds no tracks.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Number of tracks in the queue (play order length == canonical
    /// length, always).
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Current position in PLAY order.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The currently-playing (or about-to-play) track.
    pub fn current(&self) -> Option<&Track> {
        self.order.get(self.index)
    }

    /// The full queue in PLAY order (shuffle already applied).
    pub fn order(&self) -> &[Track] {
        &self.order
    }

    /// Track at a given PLAY-order position.
    pub fn get(&self, play_idx: usize) -> Option<&Track> {
        self.order.get(play_idx)
    }

    /// Whether shuffle is currently enabled.
    pub fn shuffle_enabled(&self) -> bool {
        self.shuffle
    }

    /// Current repeat mode.
    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    /// Whether an explicit "Next" has anywhere to go (used for MPRIS
    /// `can_go_next` and to gray out the transport button).
    pub fn has_next(&self) -> bool {
        if self.order.is_empty() {
            return false;
        }
        self.index + 1 < self.order.len() || self.repeat != RepeatMode::None
    }

    /// Whether "Previous" has anywhere to go — with the 3s-restart rule,
    /// this is just "is there a current track at all".
    pub fn has_previous(&self) -> bool {
        !self.order.is_empty()
    }

    // -- Mutating: replace / add / remove / reorder --

    /// Replace the queue with `tracks`, positioned at `start` (clamped to
    /// the last valid index). If shuffle is currently enabled, the new
    /// queue is immediately shuffled with the `start` entry pinned as the
    /// current one.
    pub fn set(&mut self, tracks: Vec<Track>, start: usize) {
        self.unshuffled = tracks.clone();
        self.order = tracks;
        self.origin = (0..self.order.len()).collect();
        self.index = 0;
        if self.order.is_empty() {
            return;
        }
        self.index = start.min(self.order.len() - 1);
        if self.shuffle {
            self.reshuffle_pinning_current();
        }
    }

    /// Append tracks to the end of the queue (both play order and
    /// canonical order — appending has one unambiguous meaning regardless
    /// of shuffle state).
    pub fn append(&mut self, new: Vec<Track>) {
        if new.is_empty() {
            return;
        }
        let base = self.unshuffled.len();
        self.unshuffled.extend(new.iter().cloned());
        self.origin.extend(base..base + new.len());
        self.order.extend(new);
    }

    /// Insert tracks right after the currently-playing entry in PLAY
    /// order. Falls back to [`Self::append`] when the queue is empty (no
    /// "current" to insert after).
    pub fn insert_next(&mut self, new: Vec<Track>) {
        if new.is_empty() {
            return;
        }
        if self.order.is_empty() {
            return self.append(new);
        }
        let base = self.unshuffled.len();
        self.unshuffled.extend(new.iter().cloned());
        let insert_at = self.index + 1;
        let origins: Vec<usize> = (base..base + new.len()).collect();
        self.order.splice(insert_at..insert_at, new);
        self.origin.splice(insert_at..insert_at, origins);
    }

    /// Remove the entry at PLAY-order index `play_idx`. Returns `None` if
    /// out of bounds.
    pub fn remove(&mut self, play_idx: usize) -> Option<RemoveOutcome> {
        if play_idx >= self.order.len() {
            return None;
        }
        let was_current = play_idx == self.index;
        let removed = self.order.remove(play_idx);
        let canonical_idx = self.origin.remove(play_idx);
        self.unshuffled.remove(canonical_idx);
        for o in &mut self.origin {
            if *o > canonical_idx {
                *o -= 1;
            }
        }
        if self.order.is_empty() {
            self.index = 0;
        } else if play_idx < self.index {
            self.index -= 1;
        } else if was_current && self.index >= self.order.len() {
            self.index = self.order.len() - 1;
        }
        let new_current = self.order.get(self.index).cloned();
        Some(RemoveOutcome {
            removed,
            was_current,
            new_current,
        })
    }

    /// Move a PLAY-order entry from one position to another, adjusting
    /// the current index so it keeps pointing at the same *track* (not
    /// the same numeric slot) when the move crosses it.
    pub fn move_item(&mut self, from: usize, to: usize) {
        let len = self.order.len();
        if from == to || from >= len || to >= len {
            return;
        }
        let track = self.order.remove(from);
        let origin = self.origin.remove(from);
        self.order.insert(to, track);
        self.origin.insert(to, origin);
        self.index = if self.index == from {
            to
        } else if from < self.index && to >= self.index {
            self.index - 1
        } else if from > self.index && to <= self.index {
            self.index + 1
        } else {
            self.index
        };
    }

    /// Drop every entry except the currently-playing one (which becomes
    /// the sole, canonical entry at index 0).
    pub fn clear_upcoming(&mut self) {
        if self.order.is_empty() {
            return;
        }
        let current = self.order[self.index].clone();
        self.unshuffled = vec![current.clone()];
        self.order = vec![current];
        self.origin = vec![0];
        self.index = 0;
    }

    /// Jump to a PLAY-order position directly (no reordering). Returns
    /// `false` if out of bounds.
    pub fn jump(&mut self, play_idx: usize) -> bool {
        if play_idx >= self.order.len() {
            return false;
        }
        self.index = play_idx;
        true
    }

    // -- Transport --

    /// Advance the queue. `auto == true` means "the track ended on its
    /// own"; `auto == false` means "the user/MPRIS explicitly asked for
    /// the next track". The distinction only matters for
    /// [`RepeatMode::One`]: it replays the same track only on a natural
    /// end, never on an explicit Next.
    ///
    /// At the end of the queue: [`RepeatMode::All`] (and, for an explicit
    /// Next past a repeat-one track, anything other than `None`) wraps to
    /// the start; [`RepeatMode::None`] returns `None` and rewinds the
    /// index to `0` so a later resume restarts the queue from the top.
    pub fn advance(&mut self, auto: bool) -> Option<Track> {
        if self.order.is_empty() {
            return None;
        }
        if auto && self.repeat == RepeatMode::One {
            return self.order.get(self.index).cloned();
        }
        let len = self.order.len();
        if self.index + 1 < len {
            self.index += 1;
            return self.order.get(self.index).cloned();
        }
        self.index = 0;
        if self.repeat == RepeatMode::None {
            None
        } else {
            self.order.first().cloned()
        }
    }

    /// What [`Self::advance`] with `auto = true` *would* return, without
    /// mutating anything — used to gapless pre-queue the predicted next
    /// track.
    pub fn peek_next_auto(&self) -> Option<&Track> {
        if self.order.is_empty() {
            return None;
        }
        if self.repeat == RepeatMode::One {
            return self.order.get(self.index);
        }
        let len = self.order.len();
        if self.index + 1 < len {
            self.order.get(self.index + 1)
        } else if self.repeat == RepeatMode::None {
            None
        } else {
            self.order.first()
        }
    }

    /// "Previous" with the standard 3-second restart rule: past
    /// `RESTART_THRESHOLD` into the current track, restart it in place
    /// rather than changing tracks. At the very start of the queue with
    /// no wraparound repeat, restart too (there's nothing "before" the
    /// first track).
    pub fn previous(&mut self, position: Duration) -> Option<PreviousAction> {
        if self.order.is_empty() {
            return None;
        }
        if position > RESTART_THRESHOLD {
            return Some(PreviousAction::Restart);
        }
        if self.index == 0 {
            return if self.repeat == RepeatMode::All && self.order.len() > 1 {
                self.index = self.order.len() - 1;
                self.order
                    .get(self.index)
                    .cloned()
                    .map(PreviousAction::Track)
            } else {
                Some(PreviousAction::Restart)
            };
        }
        self.index -= 1;
        self.order
            .get(self.index)
            .cloned()
            .map(PreviousAction::Track)
    }

    // -- Shuffle / repeat toggles --

    /// Enable/disable shuffle. Turning it on reshuffles the entries other
    /// than the current one (Fisher-Yates), pinning the current track at
    /// PLAY-order position 0. Turning it off restores the canonical order
    /// and relocates the current track back to its original position.
    pub fn set_shuffle(&mut self, enabled: bool) {
        if self.shuffle == enabled {
            return;
        }
        self.shuffle = enabled;
        if self.order.is_empty() {
            return;
        }
        if enabled {
            self.reshuffle_pinning_current();
        } else {
            let canonical_idx = self.origin[self.index];
            self.order = self.unshuffled.clone();
            self.origin = (0..self.order.len()).collect();
            self.index = canonical_idx;
        }
    }

    /// Change the repeat mode. Takes effect starting at the next
    /// `advance`/`peek_next_auto`/`previous` call.
    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    /// Reshuffle `order`/`origin` in place, keeping whatever is currently
    /// at `self.index` pinned at position 0.
    fn reshuffle_pinning_current(&mut self) {
        let len = self.order.len();
        if len == 0 {
            self.index = 0;
            return;
        }
        self.order.swap(0, self.index);
        self.origin.swap(0, self.index);
        fisher_yates_range(&mut self.order, &mut self.origin, 1, len, &mut self.rng);
        self.index = 0;
    }
}

/// Tiny xorshift64* PRNG — avoids pulling in the `rand` crate for the one
/// place lyra needs randomness (queue shuffling).
#[derive(Debug, Clone)]
struct XorShift64(u64);

impl XorShift64 {
    fn seeded() -> Self {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        use std::time::{SystemTime, UNIX_EPOCH};

        let time_seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        // `RandomState` seeds itself from the OS RNG per-instance, so
        // hashing anything through it gives a second, independent source
        // of entropy without a `rand` dependency.
        let hash_seed = RandomState::new().build_hasher().finish();
        let seed = time_seed ^ hash_seed.rotate_left(17) ^ 0xD1B5_4A32_D192_ED03;
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform value in `0..bound`. `bound` must be > 0. Plain modulo is
    /// fine here — playback queues top out in the low thousands, far below
    /// where modulo bias would be observable.
    fn next_below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// Fisher-Yates shuffle of the half-open range `[lo, hi)`, applied in
/// lockstep to two same-length slices (keeps `order`/`origin` in sync).
fn fisher_yates_range<T, U>(a: &mut [T], b: &mut [U], lo: usize, hi: usize, rng: &mut XorShift64) {
    if hi <= lo + 1 || hi > a.len() || hi > b.len() {
        return;
    }
    for i in (lo + 1..hi).rev() {
        let span = i - lo + 1;
        let j = lo + rng.next_below(span);
        a.swap(i, j);
        b.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn track(id: i64) -> Track {
        Track {
            id,
            path: PathBuf::from(format!("/music/{id}.flac")),
            title: format!("Track {id}"),
            artist: "Artist".into(),
            album_artist: "Artist".into(),
            album: "Album".into(),
            genre: String::new(),
            track_number: id as u32,
            disc_number: 1,
            year: 2020,
            duration: Duration::from_secs(200),
            bitrate: 320,
            sample_rate: 44100,
            provider_id: "local".into(),
            source_uri: format!("/music/{id}.flac"),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    fn tracks(ids: &[i64]) -> Vec<Track> {
        ids.iter().map(|&id| track(id)).collect()
    }

    fn ids(q: &PlayQueue) -> Vec<i64> {
        q.order().iter().map(|t| t.id).collect()
    }

    // -- empty queue --

    #[test]
    fn empty_queue_is_inert() {
        let mut q = PlayQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert!(q.current().is_none());
        assert!(!q.has_next());
        assert!(!q.has_previous());
        assert!(q.advance(true).is_none());
        assert!(q.advance(false).is_none());
        assert!(q.peek_next_auto().is_none());
        assert!(q.previous(Duration::ZERO).is_none());
        assert!(q.remove(0).is_none());
        assert!(!q.jump(0));
        q.move_item(0, 1); // no-op, must not panic
        q.clear_upcoming(); // no-op, must not panic
    }

    #[test]
    fn set_on_empty_queue_then_populate() {
        let mut q = PlayQueue::new();
        q.set(Vec::new(), 0);
        assert!(q.is_empty());
        q.append(tracks(&[1]));
        assert_eq!(ids(&q), vec![1]);
        assert_eq!(q.current().unwrap().id, 1);
    }

    // -- basic set / single item --

    #[test]
    fn set_clamps_start_index() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 99);
        assert_eq!(q.index(), 2);
        assert_eq!(q.current().unwrap().id, 3);
    }

    #[test]
    fn single_item_repeat_none_stops_on_natural_end() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1]), 0);
        q.set_repeat(RepeatMode::None);
        assert!(!q.has_next());
        assert!(q.peek_next_auto().is_none());
        assert!(q.advance(true).is_none());
        // Index rewound to the start so a later resume restarts cleanly.
        assert_eq!(q.index(), 0);
        assert_eq!(q.current().unwrap().id, 1);
    }

    #[test]
    fn single_item_repeat_all_wraps_to_itself() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1]), 0);
        q.set_repeat(RepeatMode::All);
        assert!(q.has_next());
        assert_eq!(q.peek_next_auto().unwrap().id, 1);
        assert_eq!(q.advance(true).unwrap().id, 1);
        assert_eq!(q.index(), 0);
    }

    // -- repeat modes on a multi-track queue --

    #[test]
    fn repeat_none_explicit_next_stops_at_end() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 2); // already at the last entry
        q.set_repeat(RepeatMode::None);
        assert!(!q.has_next());
        assert!(q.advance(false).is_none());
        assert_eq!(q.index(), 0);
    }

    #[test]
    fn repeat_all_wraps_at_end() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 2);
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.advance(true).unwrap().id, 1);
        assert_eq!(q.index(), 0);
    }

    #[test]
    fn repeat_one_replays_only_on_natural_end() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 0);
        q.set_repeat(RepeatMode::One);
        // Natural end: stays on the same track.
        assert_eq!(q.advance(true).unwrap().id, 1);
        assert_eq!(q.index(), 0);
        assert_eq!(q.peek_next_auto().unwrap().id, 1);
        // Explicit Next: still advances normally.
        assert_eq!(q.advance(false).unwrap().id, 2);
        assert_eq!(q.index(), 1);
    }

    #[test]
    fn repeat_one_explicit_next_wraps_past_true_end() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 2);
        q.set_repeat(RepeatMode::One);
        // At the true end, an explicit Next still has somewhere to go
        // (repeat != None), so it wraps rather than stopping dead.
        assert_eq!(q.advance(false).unwrap().id, 1);
        assert_eq!(q.index(), 0);
    }

    // -- previous / restart rule --

    #[test]
    fn previous_restarts_past_threshold() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 1);
        assert!(matches!(
            q.previous(Duration::from_secs(4)),
            Some(PreviousAction::Restart)
        ));
        assert_eq!(q.index(), 1); // unchanged
    }

    #[test]
    fn previous_moves_back_within_threshold() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 2);
        match q.previous(Duration::from_secs(1)) {
            Some(PreviousAction::Track(t)) => assert_eq!(t.id, 2),
            other => panic!("expected Track(2), got {other:?}"),
        }
        assert_eq!(q.index(), 1);
    }

    #[test]
    fn previous_at_start_without_repeat_restarts() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 0);
        assert!(matches!(
            q.previous(Duration::from_secs(1)),
            Some(PreviousAction::Restart)
        ));
        assert_eq!(q.index(), 0);
    }

    #[test]
    fn previous_at_start_with_repeat_all_wraps() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 0);
        q.set_repeat(RepeatMode::All);
        match q.previous(Duration::from_secs(1)) {
            Some(PreviousAction::Track(t)) => assert_eq!(t.id, 3),
            other => panic!("expected Track(3), got {other:?}"),
        }
        assert_eq!(q.index(), 2);
    }

    // -- jump --

    #[test]
    fn jump_valid_and_invalid() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 0);
        assert!(q.jump(2));
        assert_eq!(q.index(), 2);
        assert!(!q.jump(10));
        assert_eq!(q.index(), 2); // unchanged on failure
    }

    // -- remove --

    #[test]
    fn remove_before_current_shifts_index() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3, 4]), 2); // current = id 3
        let outcome = q.remove(0).unwrap();
        assert_eq!(outcome.removed.id, 1);
        assert!(!outcome.was_current);
        assert_eq!(q.index(), 1);
        assert_eq!(q.current().unwrap().id, 3);
    }

    #[test]
    fn remove_after_current_leaves_index() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3, 4]), 1); // current = id 2
        let outcome = q.remove(3).unwrap();
        assert_eq!(outcome.removed.id, 4);
        assert!(!outcome.was_current);
        assert_eq!(q.index(), 1);
        assert_eq!(q.current().unwrap().id, 2);
    }

    #[test]
    fn remove_current_middle_slides_next_into_place() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 1); // current = id 2
        let outcome = q.remove(1).unwrap();
        assert!(outcome.was_current);
        assert_eq!(outcome.new_current.as_ref().unwrap().id, 3);
        assert_eq!(q.current().unwrap().id, 3);
    }

    #[test]
    fn remove_current_last_clamps_to_new_last() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 2); // current = id 3, last entry
        let outcome = q.remove(2).unwrap();
        assert!(outcome.was_current);
        assert_eq!(outcome.new_current.as_ref().unwrap().id, 2);
        assert_eq!(q.index(), 1);
    }

    #[test]
    fn remove_only_item_empties_queue() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1]), 0);
        let outcome = q.remove(0).unwrap();
        assert!(outcome.was_current);
        assert!(outcome.new_current.is_none());
        assert!(q.is_empty());
        assert_eq!(q.index(), 0);
    }

    #[test]
    fn remove_out_of_bounds_is_none() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2]), 0);
        assert!(q.remove(5).is_none());
    }

    // -- move --

    #[test]
    fn move_current_item_follows_it() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3, 4]), 1); // current = id 2
        q.move_item(1, 3);
        assert_eq!(q.index(), 3);
        assert_eq!(q.current().unwrap().id, 2);
        assert_eq!(ids(&q), vec![1, 3, 4, 2]);
    }

    #[test]
    fn move_across_current_forward_shifts_index_back() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3, 4]), 2); // current = id 3, index 2
        q.move_item(0, 3); // move id 1 from front to back, crossing current
        assert_eq!(q.index(), 1);
        assert_eq!(q.current().unwrap().id, 3);
    }

    #[test]
    fn move_across_current_backward_shifts_index_forward() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3, 4]), 1); // current = id 2, index 1
        q.move_item(3, 0); // move id 4 from back to front, crossing current
        assert_eq!(q.index(), 2);
        assert_eq!(q.current().unwrap().id, 2);
    }

    #[test]
    fn move_noop_out_of_bounds() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 0);
        q.move_item(0, 0);
        q.move_item(0, 99);
        assert_eq!(ids(&q), vec![1, 2, 3]);
    }

    // -- append / insert_next / clear_upcoming --

    #[test]
    fn append_adds_to_end_regardless_of_current() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2]), 0);
        q.append(tracks(&[3, 4]));
        assert_eq!(ids(&q), vec![1, 2, 3, 4]);
        assert_eq!(q.index(), 0);
    }

    #[test]
    fn insert_next_lands_right_after_current() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 0);
        q.insert_next(tracks(&[10, 11]));
        assert_eq!(ids(&q), vec![1, 10, 11, 2, 3]);
        assert_eq!(q.current().unwrap().id, 1);
    }

    #[test]
    fn insert_next_on_empty_queue_falls_back_to_append() {
        let mut q = PlayQueue::new();
        q.insert_next(tracks(&[1, 2]));
        assert_eq!(ids(&q), vec![1, 2]);
    }

    #[test]
    fn clear_upcoming_keeps_only_current() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3, 4]), 2); // current = id 3
        q.clear_upcoming();
        assert_eq!(ids(&q), vec![3]);
        assert_eq!(q.index(), 0);
        assert_eq!(q.current().unwrap().id, 3);
    }

    // -- shuffle --

    #[test]
    fn shuffle_pins_current_at_front_and_preserves_multiset() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3, 4, 5]), 2); // current = id 3
        q.set_shuffle(true);
        assert_eq!(q.index(), 0);
        assert_eq!(q.current().unwrap().id, 3);
        let mut got = ids(&q);
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn unshuffle_restores_original_order_and_current() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3, 4, 5]), 2); // current = id 3, index 2
        q.set_shuffle(true);
        q.set_shuffle(false);
        assert_eq!(ids(&q), vec![1, 2, 3, 4, 5]);
        assert_eq!(q.index(), 2);
        assert_eq!(q.current().unwrap().id, 3);
    }

    #[test]
    fn shuffle_toggle_is_idempotent_when_already_in_that_state() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3]), 1);
        q.set_shuffle(false); // already off — no-op
        assert_eq!(ids(&q), vec![1, 2, 3]);
        assert_eq!(q.index(), 1);
    }

    #[test]
    fn set_while_shuffled_pins_the_given_start() {
        let mut q = PlayQueue::new();
        q.set_shuffle(true);
        q.set(tracks(&[1, 2, 3, 4]), 3); // current = id 4
        assert_eq!(q.index(), 0);
        assert_eq!(q.current().unwrap().id, 4);
        let mut got = ids(&q);
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 3, 4]);
    }

    #[test]
    fn shuffle_survives_a_queue_edit_and_unshuffle_still_recovers_canonical_order() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2, 3, 4]), 0);
        q.set_shuffle(true);
        q.append(tracks(&[5]));
        // Newly appended track must be reachable and present exactly once.
        let mut got = ids(&q);
        got.sort_unstable();
        assert_eq!(got, vec![1, 2, 3, 4, 5]);
        q.set_shuffle(false);
        // Canonical order: original four, then the appended one at the end.
        assert_eq!(ids(&q), vec![1, 2, 3, 4, 5]);
    }

    // -- has_next / has_previous --

    #[test]
    fn has_next_reflects_repeat_mode_at_end() {
        let mut q = PlayQueue::new();
        q.set(tracks(&[1, 2]), 1); // at the last entry
        q.set_repeat(RepeatMode::None);
        assert!(!q.has_next());
        q.set_repeat(RepeatMode::All);
        assert!(q.has_next());
        q.set_repeat(RepeatMode::One);
        assert!(q.has_next());
    }

    #[test]
    fn has_previous_true_whenever_nonempty() {
        let mut q = PlayQueue::new();
        assert!(!q.has_previous());
        q.set(tracks(&[1]), 0);
        assert!(q.has_previous());
    }
}
