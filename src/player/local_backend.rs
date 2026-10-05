// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Local audio playback backend: direct symphonia (DSD-capable fork) + cpal,
//! driven through [`super::engine::PlaybackEngine`].
//!
//! Handles `TrackSource::LocalFile` and `TrackSource::HttpStream` by
//! decoding audio locally and outputting to the system sound device. The
//! actual decode → gain → EQ → volume → output loop, DSD/DoP handling, and
//! gapless/crossfade look-ahead all live in the engine; this file is just
//! the [`super::backend::PlaybackBackend`] adapter plus the small amount of
//! state (`state`/`volume`/EQ controller/replay gain) the engine doesn't
//! own itself.

use super::PlaybackState;
use super::backend::{PlaybackBackend, PlayerError};
use super::engine::engine::{PlaySource, PlaybackEngine};
use super::eq_source::{EqController, new_shared_coeffs};
use crate::library::TrackSource;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Local audio playback backend, driven by a dedicated [`PlaybackEngine`].
pub struct LocalBackend {
    engine: PlaybackEngine,
    state: PlaybackState,
    volume: f32,
    /// Shared blocking HTTP client for HTTP stream / live radio playback.
    http_client: reqwest::blocking::Client,
    /// UI-side EQ controller (shares its coefficients/bypass flag with the
    /// engine's per-track `EqFilter`).
    eq_controller: EqController,
    /// Replay gain to apply to the next track (in dB). Set before `play()`/
    /// `queue_next()`.
    replay_gain_db: Option<f32>,
    /// Current ICY `StreamTitle` for a live radio stream, if the server
    /// sends embedded metadata. Updated from the engine's playback thread
    /// (see `PlaySource::LiveStream`), read by the UI via [`Self::icy_title`].
    icy_title: Arc<Mutex<Option<String>>>,
}

impl LocalBackend {
    /// Create a new local backend. The output device itself is opened
    /// lazily by the engine on the first `play()` call.
    pub fn new() -> Result<Self, PlayerError> {
        let eq_coeffs = new_shared_coeffs();
        let eq_bypass = Arc::new(AtomicBool::new(true)); // EQ disabled by default
        let eq_controller = EqController::new(
            Arc::clone(&eq_coeffs),
            Arc::clone(&eq_bypass),
            44100.0, // default sample rate, updated per track
        );

        let engine = PlaybackEngine::new(eq_coeffs, eq_bypass);
        let volume = 0.8;
        engine.set_volume(volume);

        Ok(Self {
            engine,
            state: PlaybackState::Stopped,
            volume,
            http_client: reqwest::blocking::Client::new(),
            eq_controller,
            replay_gain_db: None,
            icy_title: Arc::new(Mutex::new(None)),
        })
    }

    /// Get a reference to the EQ controller for UI-thread use.
    pub fn eq_controller(&self) -> &EqController {
        &self.eq_controller
    }

    /// Current ICY `StreamTitle` for a live radio stream, if the server
    /// sent embedded metadata. `None` for non-radio sources or stations
    /// that don't embed metadata.
    pub fn icy_title(&self) -> Option<String> {
        self.icy_title.lock().clone()
    }

    /// Set crossfade duration in seconds (0 = disabled, gapless only).
    pub fn set_crossfade(&mut self, secs: f32) {
        self.engine.set_crossfade(secs.max(0.0));
    }

    /// Clear the engine's gapless/crossfade look-ahead slot without
    /// touching current playback. Call whenever the upcoming track might
    /// have changed (queue edit, shuffle/repeat toggle, jump) so a stale
    /// pre-queued source doesn't play next instead of the new prediction.
    pub fn clear_pre_queued(&mut self) {
        self.engine.clear_next();
    }

    /// Set the replay gain to apply to the next track.
    ///
    /// Call this before `play()` with the appropriate gain value from the
    /// Track's `rg_track_gain` or `rg_album_gain` based on the current
    /// `ReplayGainMode`.
    pub fn set_replay_gain_db(&mut self, gain_db: Option<f32>) {
        self.replay_gain_db = gain_db;
    }

    /// Set the shared PCM buffer for the visualizer.
    #[cfg(feature = "visualizer")]
    pub fn set_pcm_buffer(
        &mut self,
        buffer: Arc<std::sync::Mutex<crate::views::now_playing::visualizer::PcmBuffer>>,
    ) {
        self.engine.set_pcm_buffer(Some(buffer));
    }

    /// Internal: play a local file.
    fn play_local_file(&mut self, path: PathBuf) -> Result<(), PlayerError> {
        self.engine
            .play(PlaySource::LocalFile(path), self.replay_gain_db)?;
        self.state = PlaybackState::Playing;
        Ok(())
    }

    /// Internal: play an HTTP stream (e.g. Subsonic `stream` URL).
    ///
    /// The connect + format probe happens on the engine's dedicated
    /// playback thread, not here — constructing an
    /// [`super::http_range_reader::HttpRangeReader`] performs a real
    /// blocking HTTP `GET`, so doing that here would block the caller
    /// exactly like the old manual background-thread dance was written to
    /// avoid; letting the engine's own thread do it gets the same effect
    /// for free.
    fn play_http_stream(&mut self, url: String) -> Result<(), PlayerError> {
        let hint_extension = extension_hint(&url);
        let source = PlaySource::Reader {
            url,
            client: self.http_client.clone(),
            hint_extension,
        };
        self.engine.play(source, self.replay_gain_db)?;
        self.state = PlaybackState::Playing;
        Ok(())
    }

    /// Internal: play an internet radio / Shoutcast-Icecast live stream.
    ///
    /// Like [`Self::play_http_stream`], the connect + probe happens on the
    /// engine's dedicated playback thread, not here.
    fn play_live_stream(&mut self, url: String) -> Result<(), PlayerError> {
        let hint_extension = extension_hint(&url);
        let source = PlaySource::LiveStream {
            url,
            client: self.http_client.clone(),
            hint_extension,
            icy_title: Arc::clone(&self.icy_title),
        };
        self.engine.play(source, self.replay_gain_db)?;
        self.state = PlaybackState::Playing;
        Ok(())
    }
}

impl PlaybackBackend for LocalBackend {
    fn play(&mut self, source: TrackSource) -> Result<(), PlayerError> {
        // Clear any stale ICY title from a previous radio stream before
        // dispatching — a non-radio source should never show one.
        *self.icy_title.lock() = None;
        match source {
            TrackSource::LocalFile(path) => self.play_local_file(path),
            TrackSource::HttpStream(url) => self.play_http_stream(url),
            TrackSource::LiveStream(url) => self.play_live_stream(url),
            TrackSource::MpdFile(_) => Err(PlayerError(
                "MPD files should use MpdBackend, not LocalBackend".to_string(),
            )),
        }
    }

    fn pause(&mut self) -> Result<(), PlayerError> {
        if self.state == PlaybackState::Playing {
            self.engine.pause();
            self.state = PlaybackState::Paused;
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<(), PlayerError> {
        if self.state == PlaybackState::Paused {
            self.engine.resume();
            self.state = PlaybackState::Playing;
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        self.engine.stop();
        self.state = PlaybackState::Stopped;
        Ok(())
    }

    fn seek(&mut self, position: Duration) -> Result<(), PlayerError> {
        self.engine.seek(position)
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), PlayerError> {
        self.volume = volume.clamp(0.0, 1.0);
        self.engine.set_volume(self.volume);
        Ok(())
    }

    fn volume(&self) -> f32 {
        self.volume
    }

    fn state(&self) -> PlaybackState {
        self.state
    }

    fn position(&self) -> Duration {
        self.engine.position()
    }

    fn duration(&self) -> Duration {
        self.engine.duration()
    }

    fn is_finished(&self) -> Result<bool, PlayerError> {
        // Only consider "finished" if we were actively playing — Stopped/
        // Paused states are never "finished", they represent explicit user
        // actions, not natural track completion (matches the old backend's
        // exact semantics).
        if self.state != PlaybackState::Playing {
            return Ok(false);
        }
        Ok(self.engine.is_finished())
    }

    fn queue_next(&mut self, source: TrackSource) -> Result<(), PlayerError> {
        match source {
            TrackSource::LocalFile(path) => {
                self.engine
                    .queue_next(PlaySource::LocalFile(path), self.replay_gain_db);
                Ok(())
            }
            TrackSource::HttpStream(url) => {
                // Unlike the old backend (which could only gapless-pre-queue
                // local files, since the HTTP connect needed a background
                // thread), the engine's look-ahead slot opens *any* source
                // lazily on the playback thread — so an HTTP stream can now
                // be pre-queued too, fixing what used to be a silent gap (or
                // worse, a stuck "pre-queued" flag with nothing actually
                // queued) whenever a queue mixed local and remote tracks.
                let hint_extension = extension_hint(&url);
                self.engine.queue_next(
                    PlaySource::Reader {
                        url,
                        client: self.http_client.clone(),
                        hint_extension,
                    },
                    self.replay_gain_db,
                );
                Ok(())
            }
            TrackSource::LiveStream(url) => {
                let hint_extension = extension_hint(&url);
                self.engine.queue_next(
                    PlaySource::LiveStream {
                        url,
                        client: self.http_client.clone(),
                        hint_extension,
                        icy_title: Arc::clone(&self.icy_title),
                    },
                    self.replay_gain_db,
                );
                Ok(())
            }
            TrackSource::MpdFile(_) => {
                Err(PlayerError("MPD files should use MpdBackend".to_string()))
            }
        }
    }
}

/// Best-effort file-extension hint for Symphonia's format probe, extracted
/// from the URL path (ignoring any query string). Streaming API endpoints
/// (e.g. Subsonic's `/rest/stream`) often have no extension at all, in
/// which case Symphonia falls back to content sniffing — this is purely a
/// probe speed-up, never required for correctness.
fn extension_hint(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next()?;
    let (_, ext) = name.rsplit_once('.')?;
    if ext.is_empty() || ext.len() > 8 {
        None
    } else {
        Some(ext.to_string())
    }
}
