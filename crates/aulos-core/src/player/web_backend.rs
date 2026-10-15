// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The browser's local playback backend (replaces `local_backend.rs`, which
//! drives the cpal engine, on wasm).
//!
//! There is no audio output behind it yet: `play` reports that playback is
//! unavailable, so the transport stays stopped and the UI shows the error.
//! It keeps the state the player reads back (volume, EQ controller) so the
//! rest of the backend runs unchanged; the Web Audio output will fill in
//! `play`/`pause`/`seek`/`position` here.

use super::PlaybackState;
use super::backend::{PlaybackBackend, PlayerError};
use super::eq_source::{EqController, new_shared_coeffs};
use crate::library::TrackSource;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Local playback in the browser.
pub struct LocalBackend {
    volume: f32,
    eq_controller: EqController,
}

impl LocalBackend {
    pub fn new() -> Result<Self, PlayerError> {
        let eq_controller = EqController::new(
            new_shared_coeffs(),
            Arc::new(AtomicBool::new(true)),
            44100.0,
        );
        Ok(Self {
            volume: 0.8,
            eq_controller,
        })
    }

    /// The EQ controller for UI-thread use.
    pub fn eq_controller(&self) -> &EqController {
        &self.eq_controller
    }

    /// No stream metadata is read in the browser yet.
    pub fn icy_title(&self) -> Option<String> {
        None
    }

    /// Crossfade needs the output the browser does not have yet.
    pub fn set_crossfade(&mut self, _secs: f32) {}

    /// Fades need the output the browser does not have yet.
    pub fn set_fade_secs(&mut self, _secs: f32) {}

    /// Nothing is pre-queued.
    pub fn clear_pre_queued(&mut self) {}

    /// Replay gain is applied by the output.
    pub fn set_replay_gain_db(&mut self, _gain_db: Option<f32>) {}
}

fn unavailable() -> PlayerError {
    PlayerError("Playback is not available in the browser yet".to_string())
}

impl PlaybackBackend for LocalBackend {
    fn play(&mut self, _source: TrackSource) -> Result<(), PlayerError> {
        Err(unavailable())
    }

    fn pause(&mut self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn resume(&mut self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn seek(&mut self, _position: Duration) -> Result<(), PlayerError> {
        Err(unavailable())
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), PlayerError> {
        self.volume = volume.clamp(0.0, 1.0);
        Ok(())
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn state(&self) -> PlaybackState {
        PlaybackState::Stopped
    }

    fn position(&self) -> Duration {
        Duration::ZERO
    }

    fn duration(&self) -> Duration {
        Duration::ZERO
    }

    fn is_finished(&self) -> Result<bool, PlayerError> {
        Ok(false)
    }
}
