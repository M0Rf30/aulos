// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! MPD playback backend — fully non-blocking.
//!
//! Sends play/pause/seek/volume commands to an MPD server. **No `block_on()`
//! calls** — all async commands are dispatched via `cosmic::task::future()`
//! and the UI is updated optimistically.  Actual MPD state is polled by a
//! background subscription that pushes `Message::MpdStatusUpdate`.

use super::PlaybackState;
use super::backend::{PlaybackBackend, PlayerError};
use crate::library::TrackSource;
use mpd_client::Client;
use std::time::Duration;

/// MPD-based playback backend.
///
/// The cached status is populated externally by `update_status()`, called
/// from the `MpdStatusUpdate` message handler.  Transport commands update the
/// cache optimistically and return a future for the caller to dispatch.
pub struct MpdBackend {
    client: Client,
    // -- Cached status (set externally by update_status) --
    state: PlaybackState,
    position: Duration,
    duration: Duration,
    volume: f32,
    /// Set `true` on `play()`, cleared on `stop()`.
    /// Used by `is_finished()` to distinguish "track ended" from "never played".
    was_playing: bool,
    /// The last URI passed to `play()` — used by the caller to dispatch the
    /// async MPD command after optimistic state is set.
    last_play_uri: Option<String>,
}

impl MpdBackend {
    /// Create a new MPD backend from a connected client.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: PlaybackState::Stopped,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            volume: 1.0,
            was_playing: false,
            last_play_uri: None,
        }
    }

    /// Get a clone of the underlying MPD client for async command dispatch.
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    /// Take the URI that was passed to the last `play()` call.
    ///
    /// Used by the caller to dispatch the actual async MPD play command.
    /// Returns `None` if no play has been requested or if it was already taken.
    pub fn take_play_uri(&mut self) -> Option<String> {
        self.last_play_uri.take()
    }

    /// Update cached status from a polled `MpdStatusUpdate` message.
    /// `volume` is `None` when MPD has no mixer; the cached value is kept
    /// rather than reported as 0.
    pub fn update_status(
        &mut self,
        position: Duration,
        duration: Duration,
        state: PlaybackState,
        volume: Option<f32>,
    ) {
        self.position = position;
        self.duration = duration;
        self.state = state;
        if let Some(volume) = volume {
            self.volume = volume;
        }
    }

    /// Mark the backend as having been actively playing, without going
    /// through `play()`. Used by `Player::adopt_mpd_track` when Aulos
    /// adopts a track MPD was already playing before Aulos's `Player`
    /// existed, so `is_finished()` doesn't treat it as already ended.
    pub fn mark_playing(&mut self) {
        self.was_playing = true;
    }
}

impl PlaybackBackend for MpdBackend {
    fn play(&mut self, source: TrackSource) -> Result<(), PlayerError> {
        match source {
            TrackSource::MpdFile(uri) => {
                // Optimistic UI state — actual play dispatched async by caller.
                self.was_playing = true;
                self.state = PlaybackState::Playing;
                self.position = Duration::ZERO;
                self.last_play_uri = Some(uri);
                Ok(())
            }
            _ => Err(PlayerError(
                "MpdBackend only handles TrackSource::MpdFile".into(),
            )),
        }
    }

    fn pause(&mut self) -> Result<(), PlayerError> {
        self.state = PlaybackState::Paused;
        Ok(())
    }

    fn resume(&mut self) -> Result<(), PlayerError> {
        self.state = PlaybackState::Playing;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        self.was_playing = false;
        self.state = PlaybackState::Stopped;
        self.position = Duration::ZERO;
        self.duration = Duration::ZERO;
        Ok(())
    }

    fn seek(&mut self, position: Duration) -> Result<(), PlayerError> {
        self.position = position;
        Ok(())
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), PlayerError> {
        self.volume = volume.clamp(0.0, 1.0);
        Ok(())
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn state(&self) -> PlaybackState {
        self.state
    }

    fn position(&self) -> Duration {
        self.position
    }

    fn duration(&self) -> Duration {
        self.duration
    }

    fn is_finished(&self) -> Result<bool, PlayerError> {
        // Track ended naturally: MPD reports Stopped, and we had been playing.
        Ok(self.state == PlaybackState::Stopped && self.was_playing)
    }
}
