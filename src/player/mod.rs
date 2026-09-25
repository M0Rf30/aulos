// SPDX-License-Identifier: GPL-3.0

//! Audio playback engine with pluggable backend support.

pub mod backend;
pub mod engine;
pub mod eq_presets;
pub mod eq_source;
pub mod equalizer;
mod http_range_reader;
pub mod local_backend;
pub mod mpd_backend;
#[cfg(feature = "visualizer")]
pub mod pw_capture;

use crate::config::ReplayGainMode;
use crate::library::{Track, TrackSource};
use backend::PlaybackBackend;
pub use eq_source::EqController;
use local_backend::LocalBackend;
use mpd_backend::MpdBackend;
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
    queue: Vec<Track>,
    queue_index: usize,
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
            queue: Vec::new(),
            queue_index: 0,
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
        let same_album_count = self.queue.iter().filter(|t| t.album == *album).count();
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

    /// Stop playback entirely.
    pub fn stop(&mut self) -> Result<(), String> {
        self.active_mut().stop().map_err(|e| e.to_string())?;
        self.current_track = None;
        self.next_pre_queued = false;
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

    /// Set the play queue from a list of tracks.
    pub fn set_queue(&mut self, tracks: Vec<Track>) {
        self.queue = tracks;
        self.queue_index = 0;
        self.next_pre_queued = false;
    }

    /// Append tracks to the end of the play queue, leaving the current
    /// track and queue position untouched.
    ///
    /// Clears `next_pre_queued` because appending invalidates the gapless
    /// look-ahead decision: when the queue was previously exhausted at the
    /// current index, the engine's pre-queued "next" slot no longer matches
    /// what now follows.
    pub fn extend_queue(&mut self, tracks: impl IntoIterator<Item = Track>) {
        self.queue.extend(tracks);
        self.next_pre_queued = false;
    }

    /// Whether the queue holds no tracks.
    pub fn queue_is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Play the next track in the queue.
    /// Returns the track that is now playing, or None if queue is empty.
    ///
    /// If gapless pre-queuing was used (`pre_queue_next()` was called earlier),
    /// the audio is already playing in the sink — this just advances the index
    /// and updates metadata. Otherwise, it does a full `play_track()`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<&Track>, String> {
        if self.queue.is_empty() {
            return Ok(None);
        }
        self.queue_index = (self.queue_index + 1) % self.queue.len();

        // If the next track was already pre-queued into the sink, we just
        // update the metadata without restarting playback.
        if self.next_pre_queued {
            self.next_pre_queued = false;
            let track = self.queue[self.queue_index].clone();
            self.current_track = Some(NowPlaying {
                duration: track.duration,
                track,
            });
            // Pre-queue the track after this one for continued gapless.
            self.pre_queue_next();
            return Ok(self.queue.get(self.queue_index));
        }

        let track = self.queue[self.queue_index].clone();
        let source = resolve_track_source(&track);
        self.play_track(&track, source)?;
        // After starting a new track, pre-queue the next one for gapless.
        self.pre_queue_next();
        Ok(self.queue.get(self.queue_index))
    }

    /// Pre-queue the next track in the queue for gapless playback.
    ///
    /// Only works for `LocalBackend` with local files (not HTTP streams or MPD).
    /// No-op if the queue has only one track or the active backend isn't local.
    pub fn pre_queue_next(&mut self) {
        if self.queue.len() <= 1 || self.active_backend != ActiveBackend::Local {
            return;
        }
        let next_idx = (self.queue_index + 1) % self.queue.len();
        let next_track = &self.queue[next_idx];
        let source = resolve_track_source(next_track);
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

    /// Play the previous track in the queue.
    pub fn previous(&mut self) -> Result<Option<&Track>, String> {
        if self.queue.is_empty() {
            return Ok(None);
        }
        self.next_pre_queued = false;
        if self.queue_index == 0 {
            self.queue_index = self.queue.len() - 1;
        } else {
            self.queue_index -= 1;
        }
        let track = self.queue[self.queue_index].clone();
        let source = resolve_track_source(&track);
        self.play_track(&track, source)?;
        self.pre_queue_next();
        Ok(self.queue.get(self.queue_index))
    }

    /// Play a specific index in the queue.
    pub fn play_index(&mut self, index: usize) -> Result<(), String> {
        if index >= self.queue.len() {
            return Err("Index out of bounds".into());
        }
        self.next_pre_queued = false;
        self.queue_index = index;
        let track = self.queue[index].clone();
        let source = resolve_track_source(&track);
        self.play_track(&track, source)?;
        self.pre_queue_next();
        Ok(())
    }

    pub fn queue(&self) -> &[Track] {
        &self.queue
    }

    pub fn queue_index(&self) -> usize {
        self.queue_index
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
