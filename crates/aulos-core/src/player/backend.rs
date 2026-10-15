// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Playback backend trait definition.
//!
//! All audio playback implementations (local cpal-based, MPD, etc.) implement
//! this trait. The trait is synchronous — for backends that need async I/O
//! (like MPD), the implementation bridges to async internally.

use crate::library::TrackSource;
use std::time::Duration;

use super::PlaybackState;

/// Errors from playback backend operations.
#[derive(Debug)]
pub struct PlayerError(pub String);

impl std::fmt::Display for PlayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PlayerError {}

impl From<String> for PlayerError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Trait for audio playback backends.
///
/// Each backend handles playing audio from a [`TrackSource`].
/// `LocalBackend` decodes audio locally via rodio.
/// `MpdBackend` sends commands to an MPD server (future).
pub trait PlaybackBackend: Send {
    /// Start playing from the given source.
    fn play(&mut self, source: TrackSource) -> Result<(), PlayerError>;

    /// Pause playback.
    fn pause(&mut self) -> Result<(), PlayerError>;

    /// Resume playback from paused state.
    fn resume(&mut self) -> Result<(), PlayerError>;

    /// Stop playback entirely.
    fn stop(&mut self) -> Result<(), PlayerError>;

    /// Seek to a position within the current track.
    fn seek(&mut self, position: Duration) -> Result<(), PlayerError>;

    /// Set volume (0.0 to 1.0).
    fn set_volume(&mut self, volume: f32) -> Result<(), PlayerError>;

    /// Get current volume (0.0 to 1.0).
    fn volume(&self) -> f32;

    /// Get current playback state.
    fn state(&self) -> PlaybackState;

    /// Get estimated playback position within the current track.
    fn position(&self) -> Duration;

    /// Get total duration of the current track.
    fn duration(&self) -> Duration;

    /// Check if the current track has finished playing.
    fn is_finished(&self) -> Result<bool, PlayerError>;

    /// Pre-queue the next track for gapless playback.
    /// Default implementation is a no-op (backends that don't support gapless ignore this).
    fn queue_next(&mut self, _source: crate::library::TrackSource) -> Result<(), PlayerError> {
        Ok(())
    }

    /// Takes the error of a play that failed after `play` had returned (the
    /// browser loads and decodes asynchronously). `None` when there is none;
    /// backends that report every failure from `play` keep the default.
    fn take_error(&mut self) -> Option<String> {
        None
    }
}

/// Best-effort file-extension hint for Symphonia's format probe, extracted
/// from the URL path (ignoring any query string). Streaming API endpoints
/// (e.g. Subsonic's `/rest/stream`) often have no extension at all, in
/// which case Symphonia falls back to content sniffing — this is purely a
/// probe speed-up, never required for correctness.
pub(super) fn extension_hint(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next()?;
    let (_, ext) = name.rsplit_once('.')?;
    if ext.is_empty() || ext.len() > 8 {
        None
    } else {
        Some(ext.to_string())
    }
}
