// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Audio playback engine with pluggable backend support.

pub mod backend;
pub mod engine;
pub mod eq_presets;
pub mod eq_source;
pub mod equalizer;
mod http_range_reader;
mod icy_reader;
pub mod local_backend;
pub mod mpd_backend;
#[cfg(feature = "visualizer")]
pub mod pw_capture;
pub mod queue;

use crate::config::{RepeatMode, ReplayGainMode};
use crate::library::{Track, TrackSource};
use backend::PlaybackBackend;
pub use eq_source::EqController;
use local_backend::LocalBackend;
use mpd_backend::MpdBackend;
use queue::{PlayQueue, PreviousAction};
use std::time::Duration;

/// Represents the current playback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

/// The currently loaded track info (runtime state, not metadata).
#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub track: Track,
    pub duration: Duration,
}

/// Resolve a `Track` to its `TrackSource` based on the provider_id field.
///
/// This avoids coupling the Player to the ProviderRegistry.
fn resolve_track_source(track: &Track) -> TrackSource {
    if track.provider_id.starts_with("mpd") {
        TrackSource::MpdFile(track.source_uri.clone())
    } else if track.provider_id.starts_with("subsonic") {
        // source_uri contains the pre-built authenticated stream URL.
        TrackSource::HttpStream(track.source_uri.clone())
    } else if track.provider_id.starts_with("radio") {
        TrackSource::LiveStream(track.source_uri.clone())
    } else if track.provider_id.starts_with("podcast") {
        if track.path.as_os_str().is_empty() {
            TrackSource::HttpStream(track.source_uri.clone())
        } else {
            // A downloaded episode — play the local file instead of
            // streaming, but keep `source_uri` (checked elsewhere) intact.
            TrackSource::LocalFile(track.path.clone())
        }
    } else {
        // Default: local file
        TrackSource::LocalFile(track.path.clone())
    }
}

/// Which backend is currently active for playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveBackend {
    /// Local cpal-based playback (local files + HTTP streams).
    Local,
    /// MPD server playback.
    Mpd,
}

/// What a queue-list edit (`remove`/`insert_next`/`append`) did to
/// playback. Mirrors `queue::RemoveOutcome` but folded with the
/// backend-driving decision `Player` makes on top of it, so `update.rs`
/// gets one simple three-way result regardless of which edit caused it.
#[derive(Debug)]
// Short-lived return values, consumed immediately — boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
pub enum QueueEditOutcome {
    /// The edit didn't touch the currently-playing entry.
    Unchanged,
    /// The currently-playing entry changed; the backend is now playing
    /// this track.
    NowPlaying(Track),
    /// The queue emptied out entirely; the backend is stopped.
    Stopped,
}

/// What `Player::previous` did — restarted the current track in place, or
/// moved to (and started playing) a different one.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum PreviousResult {
    /// Seeked back to the start of the current track.
    Restarted,
    /// Now playing this track.
    Track(Track),
}

/// Core audio player that manages playback through concrete backends.
///
/// Instead of dynamic dispatch via `Box<dyn PlaybackBackend>`, we hold
/// each backend in a named field and switch via `active_backend`.
pub struct Player {
    local_backend: LocalBackend,
    mpd_backend: Option<MpdBackend>,
    active_backend: ActiveBackend,
    current_track: Option<NowPlaying>,
    volume: f32,
    /// The play queue — the single source of truth for shuffle/repeat and
    /// "what plays next". See `queue::PlayQueue` for the actual logic.
    queue: PlayQueue,
    /// Whether the next track has been pre-queued in the sink for gapless playback.
    next_pre_queued: bool,
    /// Replay gain mode for volume normalization.
    replay_gain_mode: ReplayGainMode,
}

impl Player {
    /// Create a new player instance.
    ///
    /// `mpd_backend` should be `Some(...)` when an MPD provider is active.
    pub fn new(mpd_backend: Option<MpdBackend>) -> Result<Self, String> {
        let local_backend = LocalBackend::new().map_err(|e| e.to_string())?;

        Ok(Self {
            local_backend,
            mpd_backend,
            active_backend: ActiveBackend::Local,
            current_track: None,
            volume: 0.8,
            queue: PlayQueue::new(),
            next_pre_queued: false,
            replay_gain_mode: ReplayGainMode::Off,
        })
    }

    /// Set the shared PCM buffer on the local backend for visualizer audio tapping.
    #[cfg(feature = "visualizer")]
    pub fn set_pcm_buffer(
        &mut self,
        buffer: std::sync::Arc<std::sync::Mutex<crate::views::now_playing::visualizer::PcmBuffer>>,
    ) {
        self.local_backend.set_pcm_buffer(buffer);
    }

    /// Get a reference to the currently active backend.
    fn active(&self) -> &dyn PlaybackBackend {
        match self.active_backend {
            ActiveBackend::Local => &self.local_backend,
            ActiveBackend::Mpd => self
                .mpd_backend
                .as_ref()
                .expect("MPD backend not available"),
        }
    }

    /// Get a mutable reference to the currently active backend.
    fn active_mut(&mut self) -> &mut dyn PlaybackBackend {
        match self.active_backend {
            ActiveBackend::Local => &mut self.local_backend,
            ActiveBackend::Mpd => self
                .mpd_backend
                .as_mut()
                .expect("MPD backend not available"),
        }
    }

    /// Play a track by resolving its source.
    #[tracing::instrument(skip(self, track, source), level = "debug")]
    pub fn play_track(&mut self, track: &Track, source: TrackSource) -> Result<(), String> {
        // Select the appropriate backend based on the track source.
        match &source {
            TrackSource::LocalFile(_) | TrackSource::HttpStream(_) | TrackSource::LiveStream(_) => {
                self.active_backend = ActiveBackend::Local;
                // Apply replay gain to the local backend before playing.
                let gain = self.compute_replay_gain(track);
                self.local_backend.set_replay_gain_db(gain);
            }
            TrackSource::MpdFile(_) => {
                if self.mpd_backend.is_none() {
                    return Err("MPD backend not available for MpdFile source".into());
                }
                self.active_backend = ActiveBackend::Mpd;
            }
        }

        self.active_mut().play(source).map_err(|e| e.to_string())?;
        self.volume = self.active().volume();
        self.current_track = Some(NowPlaying {
            track: track.clone(),
            duration: self.active().duration(),
        });
        Ok(())
    }

    /// Compute the replay gain adjustment for a track based on the current mode.
    fn compute_replay_gain(&self, track: &Track) -> Option<f32> {
        match self.replay_gain_mode {
            ReplayGainMode::Off => None,
            ReplayGainMode::Track => track.rg_track_gain,
            ReplayGainMode::Album => track.rg_album_gain.or(track.rg_track_gain),
            ReplayGainMode::Auto => {
                // Use album gain when playing tracks sequentially from the same album
                // (i.e. the queue appears to be an album playback), track gain otherwise.
                if self.is_playing_album_sequentially(track) {
                    track.rg_album_gain.or(track.rg_track_gain)
                } else {
                    track.rg_track_gain.or(track.rg_album_gain)
                }
            }
        }
    }

    /// Heuristic: check if we're playing tracks from the same album in order.
    fn is_playing_album_sequentially(&self, current: &Track) -> bool {
        if self.queue.len() < 2 {
            return false;
        }
        // Check if a majority of the queue is from the same album.
        let album = &current.album;
        if album.is_empty() {
            return false;
        }
        let same_album_count = self
            .queue
            .order()
            .iter()
            .filter(|t| t.album == *album)
            .count();
        same_album_count > self.queue.len() / 2
    }

    /// Toggle play/pause.
    #[tracing::instrument(skip(self), level = "debug")]
    pub fn toggle_playback(&mut self) -> Result<(), String> {
        match self.active().state() {
            PlaybackState::Playing => self.active_mut().pause().map_err(|e| e.to_string()),
            PlaybackState::Paused => self.active_mut().resume().map_err(|e| e.to_string()),
            PlaybackState::Stopped => Ok(()),
        }
    }

    /// Stop playback entirely. Keeps the queue and `current_track` intact
    /// (position 0, state `Stopped`) so a later `resume_queue()` picks up
    /// exactly where the user left off — see the `Message::Stop` contract.
    pub fn stop(&mut self) -> Result<(), String> {
        self.active_mut().stop().map_err(|e| e.to_string())?;
        self.invalidate_pre_queue();
        Ok(())
    }

    /// Set volume (0.0 - 1.0).
    pub fn set_volume(&mut self, volume: f32) -> Result<(), String> {
        let clamped = volume.clamp(0.0, 1.0);
        self.active_mut()
            .set_volume(clamped)
            .map_err(|e| e.to_string())?;
        self.volume = clamped;
        Ok(())
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Seek to a position.
    #[tracing::instrument(skip(self), level = "debug")]
    pub fn seek(&mut self, position: Duration) -> Result<(), String> {
        self.active_mut().seek(position).map_err(|e| e.to_string())
    }

    /// Get the current playback position from the active backend.
    pub fn position(&self) -> Duration {
        self.active().position()
    }

    /// Get the current track duration from the active backend.
    pub fn duration(&self) -> Duration {
        self.active().duration()
    }

    /// Get the current playback state.
    pub fn state(&self) -> PlaybackState {
        self.active().state()
    }

    /// Get current track info.
    pub fn now_playing(&self) -> Option<&NowPlaying> {
        self.current_track.as_ref()
    }

    /// Check if playback is finished (sink empty / track ended).
    pub fn is_finished(&self) -> Result<bool, String> {
        self.active().is_finished().map_err(|e| e.to_string())
    }

    // -- Queue management --

    /// Clear the engine's/local-backend's gapless look-ahead slot. Call
    /// after ANY queue edit, shuffle/repeat toggle, or jump — the upcoming
    /// track may no longer be what was pre-queued.
    fn invalidate_pre_queue(&mut self) {
        self.next_pre_queued = false;
        self.local_backend.clear_pre_queued();
    }

    /// Re-evaluate and (re-)install the gapless look-ahead slot for
    /// whatever `peek_next_auto()` now predicts. Cheap no-op when nothing
    /// changed; called after every queue mutation so edits made mid-track
    /// don't leave a stale pre-queued track sitting in the engine.
    fn refresh_pre_queue(&mut self) {
        self.invalidate_pre_queue();
        self.pre_queue_next();
    }

    /// Play whatever the queue's current entry is, without changing queue
    /// position. Used by `set_queue`, `resume_queue`, and the
    /// was-queue-empty paths of `queue_append`/`queue_insert_next`.
    fn play_current(&mut self) -> Result<Option<Track>, String> {
        let Some(track) = self.queue.current().cloned() else {
            return Ok(None);
        };
        let source = resolve_track_source(&track);
        self.play_track(&track, source)?;
        self.pre_queue_next();
        Ok(Some(track))
    }

    /// Replace the queue with `tracks`, position at `start_index`
    /// (respecting the current shuffle setting), and start playing it.
    pub fn set_queue(
        &mut self,
        tracks: Vec<Track>,
        start_index: usize,
    ) -> Result<Option<Track>, String> {
        self.queue.set(tracks, start_index);
        self.next_pre_queued = false;
        self.local_backend.clear_pre_queued();
        self.play_current()
    }

    /// Resume playback of the queue's current entry (used when
    /// `TogglePlayback` fires from `Stopped` and the queue already holds
    /// something — e.g. after `Stop` or after the queue ran out under
    /// `RepeatMode::None`).
    pub fn resume_queue(&mut self) -> Result<Option<Track>, String> {
        self.play_current()
    }

    /// Append tracks to the end of the queue. If the queue was empty,
    /// starts playing the first of them immediately (an empty "add to
    /// queue" would otherwise queue tracks nothing could ever reach).
    pub fn queue_append(&mut self, tracks: Vec<Track>) -> Result<Option<Track>, String> {
        let was_empty = self.queue.is_empty();
        self.queue.append(tracks);
        if was_empty {
            return self.play_current();
        }
        self.refresh_pre_queue();
        Ok(None)
    }

    /// Insert tracks right after the currently-playing entry. If the queue
    /// was empty, starts playing the first of them immediately.
    pub fn queue_insert_next(&mut self, tracks: Vec<Track>) -> Result<Option<Track>, String> {
        let was_empty = self.queue.is_empty();
        self.queue.insert_next(tracks);
        if was_empty {
            return self.play_current();
        }
        self.refresh_pre_queue();
        Ok(None)
    }

    /// Remove the entry at play-order index `play_idx`. If it was the
    /// currently-playing entry, plays whatever slid into its place, or
    /// stops if the queue is now empty.
    pub fn queue_remove(&mut self, play_idx: usize) -> Result<QueueEditOutcome, String> {
        let Some(outcome) = self.queue.remove(play_idx) else {
            return Ok(QueueEditOutcome::Unchanged);
        };
        self.invalidate_pre_queue();
        if !outcome.was_current {
            if !self.queue.is_empty() {
                self.pre_queue_next();
            }
            return Ok(QueueEditOutcome::Unchanged);
        }
        match outcome.new_current {
            Some(track) => {
                let source = resolve_track_source(&track);
                self.play_track(&track, source)?;
                self.pre_queue_next();
                Ok(QueueEditOutcome::NowPlaying(track))
            }
            None => {
                self.active_mut().stop().map_err(|e| e.to_string())?;
                self.current_track = None;
                Ok(QueueEditOutcome::Stopped)
            }
        }
    }

    /// Move a queue entry from one play-order position to another.
    pub fn queue_move(&mut self, from: usize, to: usize) {
        self.queue.move_item(from, to);
        self.refresh_pre_queue();
    }

    /// Drop every queue entry except the one currently playing.
    pub fn queue_clear_upcoming(&mut self) {
        self.queue.clear_upcoming();
        // Only the current track remains — nothing left to pre-queue.
        self.invalidate_pre_queue();
    }

    /// Jump to and play the entry at play-order index `play_idx`.
    pub fn jump_to(&mut self, play_idx: usize) -> Result<Option<Track>, String> {
        if !self.queue.jump(play_idx) {
            return Ok(None);
        }
        self.invalidate_pre_queue();
        let track = self
            .queue
            .current()
            .cloned()
            .expect("jump succeeded, current must exist");
        let source = resolve_track_source(&track);
        self.play_track(&track, source)?;
        self.pre_queue_next();
        Ok(Some(track))
    }

    /// Whether the queue holds no tracks.
    pub fn queue_is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The full queue in PLAY order (shuffle already applied).
    pub fn queue(&self) -> &[Track] {
        self.queue.order()
    }

    /// Current position in PLAY order.
    pub fn queue_index(&self) -> usize {
        self.queue.index()
    }

    /// Whether shuffle is currently enabled.
    pub fn shuffle_enabled(&self) -> bool {
        self.queue.shuffle_enabled()
    }

    /// Current repeat mode.
    pub fn repeat_mode(&self) -> RepeatMode {
        self.queue.repeat()
    }

    /// Whether an explicit "Next" has anywhere to go.
    pub fn has_next(&self) -> bool {
        self.queue.has_next()
    }

    /// Whether "Previous" has anywhere to go.
    pub fn has_previous(&self) -> bool {
        self.queue.has_previous()
    }

    /// Enable/disable shuffle for the play queue.
    pub fn set_shuffle(&mut self, enabled: bool) {
        self.queue.set_shuffle(enabled);
        self.refresh_pre_queue();
    }

    /// Change the repeat mode for the play queue.
    pub fn set_repeat_mode(&mut self, mode: RepeatMode) {
        self.queue.set_repeat(mode);
        self.refresh_pre_queue();
    }

    /// Advance the queue, either because the track ended naturally
    /// (`auto = true`) or because the user/MPRIS explicitly asked for the
    /// next track (`auto = false`). Returns the new current track, or
    /// `None` only when the queue itself is empty — reaching the end under
    /// `RepeatMode::None` still returns `Some` (the backend stops, but
    /// `current_track` is kept per the `Message::Stop`-style contract).
    fn advance_track(&mut self, auto: bool) -> Result<Option<Track>, String> {
        if self.queue.is_empty() {
            return Ok(None);
        }

        // Gapless fast path: the engine already has the predicted next
        // track loaded in its look-ahead slot (see `pre_queue_next`, which
        // always follows `peek_next_auto`) — on a natural end this is
        // exactly what `advance(true)` selects, so just move the
        // bookkeeping index/metadata instead of a full `play_track()`
        // (which would restart audio that's already playing).
        if auto && self.next_pre_queued {
            self.next_pre_queued = false;
            if let Some(next) = self.queue.advance(true) {
                self.current_track = Some(NowPlaying {
                    duration: next.duration,
                    track: next.clone(),
                });
                self.pre_queue_next();
                return Ok(Some(next));
            }
            // `peek_next_auto`/`advance` disagreed with what got
            // pre-queued — shouldn't happen, but stay safe and stop
            // cleanly using the same "queue finished" bookkeeping as the
            // non-preloaded path below.
            self.local_backend.clear_pre_queued();
            self.active_mut().stop().map_err(|e| e.to_string())?;
            let kept = self.queue.current().cloned();
            self.current_track = kept.clone().map(|t| NowPlaying {
                duration: t.duration,
                track: t,
            });
            return Ok(kept);
        }

        self.invalidate_pre_queue();
        match self.queue.advance(auto) {
            Some(track) => {
                let source = resolve_track_source(&track);
                self.play_track(&track, source)?;
                self.pre_queue_next();
                Ok(Some(track))
            }
            None => {
                // `RepeatMode::None` reached the end of the queue: stop
                // the backend but keep the queue and `current_track`
                // intact (`queue.advance` already rewound its index to 0)
                // so a later resume restarts the queue from the top.
                self.active_mut().stop().map_err(|e| e.to_string())?;
                let kept = self.queue.current().cloned();
                self.current_track = kept.clone().map(|t| NowPlaying {
                    duration: t.duration,
                    track: t,
                });
                Ok(kept)
            }
        }
    }

    /// Play the next track in the queue (explicit "Next" — user pressed
    /// next / MPRIS `Next`). See `advance_track`'s `auto` parameter.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Track>, String> {
        self.advance_track(false)
    }

    /// Natural end-of-track advance: the active backend reports the track
    /// finished on its own. `RepeatMode::One` replays here only; explicit
    /// `next()` always moves on regardless of repeat mode.
    pub fn advance_on_finish(&mut self) -> Result<Option<Track>, String> {
        self.advance_track(true)
    }

    /// Play the previous track, honoring the standard 3-second restart
    /// rule: `position` is how far into the current track playback has
    /// gotten.
    pub fn previous(&mut self, position: Duration) -> Result<Option<PreviousResult>, String> {
        if self.queue.is_empty() {
            return Ok(None);
        }
        match self.queue.previous(position) {
            Some(PreviousAction::Restart) => {
                self.active_mut()
                    .seek(Duration::ZERO)
                    .map_err(|e| e.to_string())?;
                Ok(Some(PreviousResult::Restarted))
            }
            Some(PreviousAction::Track(track)) => {
                self.invalidate_pre_queue();
                let source = resolve_track_source(&track);
                self.play_track(&track, source)?;
                self.pre_queue_next();
                Ok(Some(PreviousResult::Track(track)))
            }
            None => Ok(None),
        }
    }

    /// Pre-queue the predicted next track (per `peek_next_auto`, which
    /// honors shuffle/repeat) for gapless playback.
    ///
    /// Only works for `LocalBackend`; MPD's own backend has no gapless
    /// pre-queue support, and a queue entry resolving to an MPD source
    /// can't be gapless-preloaded through the local engine either.
    pub fn pre_queue_next(&mut self) {
        if self.active_backend != ActiveBackend::Local {
            return;
        }
        let Some(next_track) = self.queue.peek_next_auto().cloned() else {
            return;
        };
        let source = resolve_track_source(&next_track);
        if matches!(source, TrackSource::MpdFile(_)) {
            return;
        }
        match self.local_backend.queue_next(source) {
            Ok(()) => {
                self.next_pre_queued = true;
            }
            Err(e) => {
                tracing::warn!("Failed to pre-queue next track: {e}");
                self.next_pre_queued = false;
            }
        }
    }

    /// Which backend type is currently active.
    pub fn active_backend_type(&self) -> ActiveBackend {
        self.active_backend
    }

    /// Get a reference to the MPD backend (if present).
    pub fn mpd_backend_ref(&self) -> Option<&MpdBackend> {
        self.mpd_backend.as_ref()
    }

    /// Get a mutable reference to the MPD backend (if present).
    pub fn mpd_backend_mut(&mut self) -> Option<&mut MpdBackend> {
        self.mpd_backend.as_mut()
    }

    /// Adopt a track MPD is already playing without Lyra having started it
    /// itself — e.g. right after switching to an MPD backend whose server
    /// was already mid-playback. Makes the MPD backend the active one and
    /// marks it as having been playing, so `is_finished()` doesn't
    /// immediately report the freshly-adopted track as ended. No-op if
    /// there is no MPD backend.
    pub fn adopt_mpd_track(&mut self, track: Track, duration: Duration) {
        let Some(mpd) = self.mpd_backend.as_mut() else {
            return;
        };
        mpd.mark_playing();
        self.active_backend = ActiveBackend::Mpd;
        self.current_track = Some(NowPlaying { track, duration });
    }

    /// Get a reference to the local backend's EQ controller.
    pub fn eq_controller(&self) -> &EqController {
        self.local_backend.eq_controller()
    }

    /// Current ICY `StreamTitle` for a live radio stream, if the server
    /// sent embedded metadata. `None` for non-radio playback or stations
    /// that don't embed metadata.
    pub fn icy_title(&self) -> Option<String> {
        self.local_backend.icy_title()
    }

    /// Set crossfade duration on the local backend.
    pub fn set_crossfade(&mut self, secs: f32) {
        self.local_backend.set_crossfade(secs);
    }

    /// Set the replay gain mode.
    pub fn set_replay_gain_mode(&mut self, mode: ReplayGainMode) {
        self.replay_gain_mode = mode;
    }
}
