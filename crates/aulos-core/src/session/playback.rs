// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use crate::config::Config;
use crate::desktop::inhibit::InhibitToken;
use crate::library::{Lyrics, LyricsProvider, Track};
use crate::mpris::MprisHandle;
use crate::online::scrobble::ScrobbleController;
use crate::player::extras::PlaybackExtrasState;
use crate::player::{ActiveBackend, Player, clamp_display_position};
use crate::task::Effect;
#[cfg(feature = "visualizer")]
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use std::time::Duration;

/// Asynchronous results of the playback workflows; feed each to
/// [`Session::apply_playback_event`](super::Session::apply_playback_event).
#[derive(Debug, Clone)]
pub enum PlaybackEvent {
    /// A command sent to the MPD server finished. After an `Ok` the session
    /// runs a playback tick right away so the position/state catches up; the
    /// status poll corrects it otherwise.
    MpdCommand(Result<(), String>),
    /// A play was written to the library history (nothing to do beyond
    /// refreshing what depends on it).
    PlayRecorded,
    /// The MPRIS cover-art URL of a track was resolved off-thread (`None`:
    /// no art).
    MprisArt {
        track_id: i64,
        art_url: Option<String>,
    },
    /// An online lyrics lookup finished.
    Lyrics(Option<Lyrics>),
    /// The tags of the audio files given to
    /// [`Session::read_audio_files`](super::Session::read_audio_files) were
    /// read (unreadable files are skipped).
    FilesRead(Vec<Track>),
    /// A podcast position write finished (nothing to do).
    PodcastPositionSaved,
    /// An inhibit request finished; `None` when it failed.
    InhibitAcquired(Option<InhibitToken>),
    /// A released inhibit lock is gone (nothing to do).
    InhibitReleased,
    /// A track notification was sent; the server's id, to replace it next time.
    NotificationSent(Option<u32>),
}

/// Something the UI should tell the user about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackNotice {
    /// This many tracks were queued behind the current one (nothing started).
    Queued(usize),
    /// None of the opened files could be played.
    OpenFilesFailed,
}

/// Where [`Session::queue_tracks`](super::Session::queue_tracks) inserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueuePlacement {
    /// Right after the current track ("play next").
    Next,
    /// At the end of the queue.
    End,
}

/// What a playback workflow asks of the UI.
#[must_use = "run the effect and refresh the track-dependent presentation"]
pub struct PlaybackUpdate {
    /// The current track changed. The session already did its half (lyrics
    /// and scrobble reset, MPRIS, the track notification); the UI refreshes
    /// what depends on the track (cover backdrop, visualizer metadata).
    pub track_changed: bool,
    pub notice: Option<PlaybackNotice>,
    pub effect: Effect<PlaybackEvent>,
}

impl PlaybackUpdate {
    /// Nothing to do.
    pub fn none() -> Self {
        Self::effect(Effect::none())
    }

    pub(super) fn effect(effect: Effect<PlaybackEvent>) -> Self {
        Self {
            track_changed: false,
            notice: None,
            effect,
        }
    }

    /// Also run `effect`.
    pub(super) fn also(mut self, effect: Effect<PlaybackEvent>) -> Self {
        self.effect = Effect::batch([self.effect, effect]);
        self
    }
}

/// `position` as displayed: capped at the duration of `track`.
pub(super) fn capped_position(position: Duration, track: Option<&Track>) -> Duration {
    match track {
        Some(track) => clamp_display_position(position, track.duration),
        None => position,
    }
}

/// Wraps an asynchronous MPD command as an effect; failures are reported
/// through [`PlaybackEvent::MpdCommand`].
pub(super) fn mpd_command(
    command: impl Future<Output = Result<(), String>> + Send + 'static,
) -> Effect<PlaybackEvent> {
    Effect::future(async move { PlaybackEvent::MpdCommand(command.await) })
}

/// The audio player and what is currently playing.
pub struct PlaybackState {
    pub player: Option<Player>,
    pub position: Duration,
    pub current_track: Option<Track>,
    /// MPRIS2 D-Bus handle, once the session-bus server has started (see
    /// [`crate::mpris::mpris_stream`]). `None` before `Ready` arrives or if
    /// no session bus is available — media-key integration degrades quietly.
    pub mpris: Option<MprisHandle>,
    /// Volume level captured when the mute shortcut silenced playback, so
    /// unmuting restores it instead of a fixed default. `None` whenever
    /// audio is not muted-by-shortcut.
    pub pre_mute_volume: Option<f32>,
    /// Whether a "now playing" notification has been sent for the current track.
    pub scrobble_now_playing_sent: bool,
    /// Whether the current track has been scrobbled (to avoid duplicates).
    pub scrobble_sent: bool,
    /// Multi-service scrobbling (ListenBrainz / Last.fm / Libre.fm) state.
    pub scrobble: ScrobbleController,
    pub lyrics: Option<Lyrics>,
    pub lyrics_loading: bool,
    /// Runtime state of the playback/desktop extras (auto-play RNG,
    /// inhibit lock, notification bookkeeping).
    pub extras: PlaybackExtrasState,
    /// Whether the UI window has focus (see
    /// [`Session::set_window_focused`](super::Session::set_window_focused));
    /// track-change notifications are only shown while it has not.
    pub window_focused: bool,
    /// Whether the user is dragging the seek slider (see
    /// [`Session::set_seeking`](super::Session::set_seeking)); the displayed
    /// position is left alone meanwhile.
    pub seeking: bool,
    /// The launch still has to start playback ("auto-play on launch");
    /// consumed when the library has loaded.
    pub autoplay_pending: bool,
    /// The PCM tap the audio engine writes the visualizer's samples to (and
    /// the MPD capture thread, `player::pw_capture`); it
    /// follows the player across [`Session::recreate_player`](super::Session::recreate_player).
    #[cfg(feature = "visualizer")]
    pub pcm_buffer: Arc<Mutex<crate::player::pcm::PcmBuffer>>,
    /// Whether MPD itself reports `Playing`, mirrored from every applied
    /// status. The MPD capture thread reads it to gate writes made while it
    /// falls back to the default-sink monitor (no MPD stream on the audio
    /// graph yet), so other applications' audio does not drive the
    /// visualizer while MPD is paused or stopped.
    #[cfg(feature = "visualizer")]
    pub mpd_playing: Arc<AtomicBool>,
    /// Whether the UI shows a visualizer that wants audio right now (set via
    /// [`Session::set_visualizer_capture`](super::Session::set_visualizer_capture)).
    /// Gates the MPD PipeWire capture background stream.
    #[cfg(feature = "visualizer")]
    pub capture_wanted: bool,
}

impl PlaybackState {
    /// Whether the player is currently playing.
    pub fn is_playing(&self) -> bool {
        self.player
            .as_ref()
            .is_some_and(|p| p.state() == crate::player::PlaybackState::Playing)
    }

    /// The MPD client, if the active backend is MPD.
    pub fn mpd_client(&self) -> Option<mpd_client::Client> {
        let player = self.player.as_ref()?;
        if player.active_backend_type() != ActiveBackend::Mpd {
            return None;
        }
        Some(player.mpd_backend_ref()?.client())
    }

    /// Makes `track` the current one, from the start.
    pub(super) fn begin_track(&mut self, track: Track) {
        self.current_track = Some(track);
        self.position = Duration::ZERO;
    }

    /// Clears the per-track state (lyrics, scrobble flags) when the current
    /// track changed.
    pub(super) fn reset_for_new_track(&mut self) {
        self.lyrics = None;
        self.scrobble_now_playing_sent = false;
        self.scrobble_sent = false;
    }

    /// Starts the track the optimistic MPD backend is waiting to play (see
    /// [`Self::mpd_play`]).
    pub(super) fn mpd_after_play(&mut self) -> Effect<PlaybackEvent> {
        if let Some(player) = &mut self.player
            && let Some(mpd) = player.mpd_backend_mut()
            && let Some(uri) = mpd.take_play_uri()
        {
            return self.mpd_play(uri);
        }
        Effect::none()
    }

    /// Sends MPD one `ClearQueue + Add + Play` command list for `uri`.
    ///
    /// Aulos drives MPD one track at a time from its own [`crate::player`]
    /// queue — shuffle/repeat live entirely in that queue, never in MPD
    /// itself. Forcing `random`/`repeat`/`single` off on every dispatch
    /// guards against a stale `repeat 1` (set by an external MPD client,
    /// or left over from before Aulos started driving this server): with a
    /// single-song MPD queue, `repeat 1` would otherwise loop that one
    /// song forever and `is_finished()` would never fire, silently
    /// freezing playback on the current track.
    ///
    /// Sent as one command list: one round trip, and no other client can
    /// interleave commands between `clear` and `play`.
    fn mpd_play(&self, uri: String) -> Effect<PlaybackEvent> {
        use mpd_client::commands::{
            Add, ClearQueue, Play, SetRandom, SetRepeat, SetSingle, SingleMode,
        };
        let Some(client) = self.mpd_client() else {
            return Effect::none();
        };
        mpd_command(async move {
            client
                .command_list((
                    ClearQueue,
                    Add::uri(&uri),
                    SetRandom(false),
                    SetRepeat(false),
                    SetSingle(SingleMode::Disabled),
                    Play::current(),
                ))
                .await
                .map_err(|e| format!("MPD play {uri}: {e}"))?;
            Ok(())
        })
    }

    /// Loads the current track's embedded or sidecar `.lrc` lyrics.
    pub fn load_embedded_lyrics(&mut self) {
        if let Some(track) = &self.current_track {
            self.lyrics = LyricsProvider::from_tags(&track.path)
                .or_else(|| LyricsProvider::from_lrc_file(&track.path));
        }
    }

    /// Looks the current track's lyrics up online; the result arrives as
    /// [`PlaybackEvent::Lyrics`].
    pub fn fetch_lyrics_online(&mut self) -> Effect<PlaybackEvent> {
        let Some(track) = &self.current_track else {
            return Effect::none();
        };
        self.lyrics_loading = true;
        let artist = track.artist.clone();
        let title = track.title.clone();
        Effect::future(async move {
            PlaybackEvent::Lyrics(LyricsProvider::fetch_online(&artist, &title).await)
        })
    }

    /// Creates the audio player (absent when the audio device cannot be
    /// opened) with the persisted master volume applied.
    pub(super) fn new(config: &Config) -> Self {
        #[cfg(feature = "visualizer")]
        let pcm_buffer = Arc::new(Mutex::new(crate::player::pcm::PcmBuffer::new(8192)));
        let player = match Player::new(None) {
            #[allow(unused_mut)]
            Ok(mut p) => {
                // `Player::new` hardcodes 0.8 internally, so without this
                // every launch would ignore the saved level.
                if let Err(e) = p.set_volume(config.volume) {
                    tracing::warn!("Failed to apply saved volume: {e}");
                }
                p.set_fade_secs(config.fade_duration_secs);
                super::preferences::apply_playback_settings(&mut p, config);
                #[cfg(feature = "visualizer")]
                p.set_pcm_buffer(Arc::clone(&pcm_buffer));
                Some(p)
            }
            Err(e) => {
                tracing::error!("Failed to initialize audio player: {e}");
                None
            }
        };
        Self {
            player,
            position: Duration::ZERO,
            current_track: None,
            mpris: None,
            pre_mute_volume: None,
            scrobble_now_playing_sent: false,
            scrobble_sent: false,
            scrobble: ScrobbleController::new(config),
            lyrics: None,
            lyrics_loading: false,
            extras: PlaybackExtrasState::default(),
            window_focused: false,
            seeking: false,
            autoplay_pending: config.auto_play_on_launch,
            #[cfg(feature = "visualizer")]
            pcm_buffer,
            #[cfg(feature = "visualizer")]
            mpd_playing: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "visualizer")]
            capture_wanted: false,
        }
    }
}
