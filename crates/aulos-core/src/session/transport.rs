// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Playback workflows: what happens when the user starts, skips, seeks or
//! queues music, when the position ticks, when MPD or MPRIS report something
//! and when an asynchronous result comes back. They span the player, the
//! library, the config and the providers, so they live on [`Session`].
//!
//! Every workflow returns a [`PlaybackUpdate`]: the asynchronous work to run
//! (its results are fed back through [`Session::apply_playback_event`]),
//! whether the current track changed, and an optional notice for the user.

use super::playback::{
    PlaybackEvent, PlaybackNotice, PlaybackUpdate, QueuePlacement, capped_position,
};
use super::{Session, mpd};
use crate::config::{RepeatMode, ReplayGainMode};
use crate::desktop::{inhibit, notify};
use crate::library::queue_source::QueueSource;
use crate::library::{LibraryScanner, Track, owned_tracks};
use crate::mpris::{LoopMode, MprisCommand, MprisSnapshot, MprisStatus};
use crate::player::extras::{InhibitStep, toggle_party_genre, toggle_stop_after, wants_inhibit};
use crate::player::{
    ActiveBackend, PlaybackState as PlayerState, Player, PreviousResult, QueueEditOutcome,
};
use crate::provider::MusicProvider;
use crate::provider::mpd_events::MpdStatusUpdate;
use crate::task::Effect;
use std::path::PathBuf;
#[cfg(feature = "visualizer")]
use std::sync::Arc;
use std::time::Duration;
use web_time::Instant;

/// A track is scrobbled to Subsonic after half its length or this long,
/// whichever comes first.
const SCROBBLE_MAX_THRESHOLD: Duration = Duration::from_secs(240);

/// How far the seek shortcuts jump.
pub const SEEK_STEP: Duration = Duration::from_secs(5);

/// Volume change of one volume-up/down shortcut press.
pub const VOLUME_STEP: f32 = 0.05;

/// Playback time after which a Subsonic play counts as a scrobble.
fn scrobble_threshold(duration: Duration) -> Duration {
    (duration / 2).min(SCROBBLE_MAX_THRESHOLD)
}

/// The seek-bar fraction an MPRIS `Seek` (`relative`, an offset from
/// `position`) or `SetPosition` (absolute) request names; `None` when the
/// track has no duration to seek in.
fn mpris_seek_fraction(
    position: Duration,
    duration: Duration,
    offset_us: i64,
    relative: bool,
) -> Option<f32> {
    let duration_us = duration.as_micros() as i64;
    (duration_us > 0).then(|| {
        let target_us = if relative {
            position.as_micros() as i64 + offset_us
        } else {
            offset_us
        };
        (target_us.clamp(0, duration_us) as f32) / duration_us as f32
    })
}

/// The volume a mute-shortcut press moves to. Muting remembers the current
/// level in `pre_mute` so unmuting restores it rather than jumping to some
/// fixed default; with no record (the app started at zero) unmuting goes to
/// full volume.
fn mute_target(current: f32, pre_mute: &mut Option<f32>) -> f32 {
    if current > 0.0 {
        *pre_mute = Some(current);
        0.0
    } else {
        pre_mute.take().unwrap_or(1.0)
    }
}

fn release_inhibit(token: inhibit::InhibitToken) -> Effect<PlaybackEvent> {
    Effect::future(async move {
        token.release().await;
        PlaybackEvent::InhibitReleased
    })
}

impl Session {
    // -- Starting and changing tracks ----------------------------------

    /// Makes `track` current and runs the track-change bookkeeping.
    fn start_track(&mut self, track: Track) -> PlaybackUpdate {
        self.playback.begin_track(track);
        self.on_track_changed()
    }

    /// Like [`Self::start_track`] for a player result: a started track
    /// becomes current, `None` changes nothing, an error is logged as
    /// `failure`.
    fn start_or_log(
        &mut self,
        result: Result<Option<Track>, String>,
        failure: &str,
    ) -> PlaybackUpdate {
        match result {
            Ok(Some(track)) => self.start_track(track),
            Ok(None) => PlaybackUpdate::none(),
            Err(e) => {
                tracing::error!("{failure}: {e}");
                PlaybackUpdate::none()
            }
        }
    }

    /// Bookkeeping whenever the current track changes (queue advance, jump
    /// or edit, MPD adopting an externally-changed song, ...): resets the
    /// lyrics and scrobble state, starts the pending MPD track,
    /// republishes MPRIS and pops the track notification. The caller must
    /// already have set `playback.current_track` and `playback.position`.
    fn on_track_changed(&mut self) -> PlaybackUpdate {
        self.playback.reset_for_new_track();
        let mpd = self.playback.mpd_after_play();
        let mpris = self.publish_mpris();
        let notify = self.notify_track_changed();
        let mut update = PlaybackUpdate::effect(Effect::batch([mpd, mpris, notify]));
        update.track_changed = true;
        update
    }

    /// Replaces the queue with `tracks` and starts playing at
    /// `start_index`. Leaving a podcast episode persists its last position
    /// first.
    pub fn play_tracks(&mut self, tracks: Vec<Track>, start_index: usize) -> PlaybackUpdate {
        let podcast_save = match self.podcasts.current_episode_id.take() {
            Some(episode_id) => {
                let position_ms = self.playback.position.as_millis() as i64;
                self.podcasts
                    .save_position(episode_id, position_ms, false)
                    .map(|_| PlaybackEvent::PodcastPositionSaved)
            }
            None => Effect::none(),
        };
        let Some(player) = &mut self.playback.player else {
            return PlaybackUpdate::effect(podcast_save);
        };
        let result = player.set_queue(tracks, start_index);
        self.start_or_log(result, "play_track_list failed")
            .also(podcast_save)
    }

    /// Plays album `album` of the library starting at its track `start`.
    pub fn play_album(&mut self, album: usize, start: usize) -> PlaybackUpdate {
        match self.library.albums.get(album) {
            Some(album) => {
                let tracks = owned_tracks(&album.tracks);
                self.play_tracks(tracks, start)
            }
            None => PlaybackUpdate::none(),
        }
    }

    /// Plays album `album` of artist `artist` starting at its track `start`.
    pub fn play_artist_album(
        &mut self,
        artist: usize,
        album: usize,
        start: usize,
    ) -> PlaybackUpdate {
        let tracks = self
            .library
            .artists
            .get(artist)
            .and_then(|a| a.albums.get(album))
            .map(|a| owned_tracks(&a.tracks));
        match tracks {
            Some(tracks) => self.play_tracks(tracks, start),
            None => PlaybackUpdate::none(),
        }
    }

    /// Plays the whole library starting at `index`.
    pub fn play_library_track(&mut self, index: usize) -> PlaybackUpdate {
        let tracks = owned_tracks(&self.library.tracks);
        self.play_tracks(tracks, index)
    }

    /// Plays the genre detail view's tracks starting at `index`.
    pub fn play_genre_track(&mut self, index: usize) -> PlaybackUpdate {
        if self.library.genre_tracks.is_empty() {
            return PlaybackUpdate::none();
        }
        let tracks = self.library.genre_tracks.clone();
        self.play_tracks(tracks, index)
    }

    /// Turns shuffle on and plays the genre detail view's tracks from a
    /// random one.
    pub fn shuffle_genre(&mut self) -> PlaybackUpdate {
        if self.library.genre_tracks.is_empty() {
            return PlaybackUpdate::none();
        }
        if !self.config.config.shuffle {
            self.config.config.shuffle = true;
            if let Some(player) = &mut self.playback.player {
                player.set_shuffle(true);
            }
        }
        let nanos = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0);
        let start = nanos % self.library.genre_tracks.len();
        let tracks = self.library.genre_tracks.clone();
        self.play_tracks(tracks, start)
    }

    /// Reads the tags of ad-hoc audio files (outside any library) off-thread;
    /// the readable ones arrive as [`PlaybackEvent::FilesRead`] and replace
    /// the queue.
    pub fn open_files(&self, paths: Vec<PathBuf>) -> PlaybackUpdate {
        PlaybackUpdate::effect(Effect::blocking(
            move || {
                Ok(paths
                    .into_iter()
                    .filter_map(|path| match LibraryScanner::read_metadata(&path) {
                        Ok(track) => Some(track),
                        Err(e) => {
                            tracing::warn!("Skipping unreadable file {}: {e}", path.display());
                            None
                        }
                    })
                    .collect::<Vec<_>>())
            },
            |tracks| PlaybackEvent::FilesRead(tracks.unwrap_or_default()),
        ))
    }

    // -- Transport -----------------------------------------------------

    /// Play/pause; from `Stopped`, resumes the queue where it left off (the
    /// whole library only when the queue itself is empty — starting from
    /// `Stopped` must never silently replace a queue the user still has
    /// loaded).
    pub fn toggle_playback(&mut self) -> PlaybackUpdate {
        let mut update = PlaybackUpdate::none();
        if let Some(player) = &mut self.playback.player {
            if player.state() == PlayerState::Stopped {
                let result = if !player.queue_is_empty() {
                    player.resume_queue()
                } else if !self.library.tracks.is_empty() {
                    player.set_queue(owned_tracks(&self.library.tracks), 0)
                } else {
                    Ok(None)
                };
                update = self.start_or_log(result, "Resume from stopped failed");
            } else {
                let was_playing = player.state() == PlayerState::Playing;
                if let Err(e) = player.toggle_playback() {
                    tracing::error!("Playback toggle failed: {e}");
                } else if let Some(client) = self.playback.mpd_client() {
                    update = update.also(mpd::set_pause(client, was_playing));
                }
            }
        }
        // Refreshed on every path: pausing is exactly when the periodic
        // publish stops running.
        update.also(self.publish_mpris())
    }

    /// Skips to the next track.
    pub fn next_track(&mut self) -> PlaybackUpdate {
        let Some(player) = &mut self.playback.player else {
            return PlaybackUpdate::none();
        };
        let result = player.next();
        self.start_or_log(result, "Next track failed")
    }

    /// Goes to the previous track, or restarts the current one when it has
    /// played for a while.
    pub fn previous_track(&mut self) -> PlaybackUpdate {
        let position = self.playback.position;
        let Some(player) = &mut self.playback.player else {
            return PlaybackUpdate::none();
        };
        match player.previous(position) {
            Ok(Some(PreviousResult::Restarted)) => {
                self.playback.position = Duration::ZERO;
                // The MPD backend's `seek` only updates its cached position;
                // the server needs the real command.
                let seek = match self.playback.mpd_client() {
                    Some(client) => mpd::seek(client, Duration::ZERO),
                    None => Effect::none(),
                };
                PlaybackUpdate::effect(seek).also(self.publish_mpris())
            }
            Ok(Some(PreviousResult::Track(track))) => self.start_track(track),
            Ok(None) => PlaybackUpdate::none(),
            Err(e) => {
                tracing::error!("Previous track failed: {e}");
                PlaybackUpdate::none()
            }
        }
    }

    /// Stops playback, keeping the queue so a later play resumes it.
    pub fn stop(&mut self) -> PlaybackUpdate {
        let mut mpd = Effect::none();
        if let Some(player) = &mut self.playback.player {
            match player.stop() {
                Ok(()) => {
                    self.playback.position = Duration::ZERO;
                    if let Some(client) = self.playback.mpd_client() {
                        mpd = mpd::stop(client);
                    }
                }
                Err(e) => tracing::error!("Stop failed: {e}"),
            }
        }
        PlaybackUpdate::effect(mpd).also(self.publish_mpris())
    }

    /// Seeks to the seek-bar `fraction` (0..=1) of the current track.
    pub fn seek_fraction(&mut self, fraction: f32) -> PlaybackUpdate {
        let Some(duration) = self.playback.current_track.as_ref().map(|t| t.duration) else {
            return PlaybackUpdate::none();
        };
        self.seek_to(Duration::from_secs_f32(fraction * duration.as_secs_f32()))
    }

    /// Seeks the current track to `target`.
    pub fn seek_to(&mut self, target: Duration) -> PlaybackUpdate {
        let Some(player) = &mut self.playback.player else {
            return PlaybackUpdate::none();
        };
        if self.playback.current_track.is_none() {
            return PlaybackUpdate::none();
        }
        match player.seek(target) {
            Ok(()) => {
                self.playback.position = target;
                match self.playback.mpd_client() {
                    Some(client) => PlaybackUpdate::effect(mpd::seek(client, target)),
                    None => PlaybackUpdate::none(),
                }
            }
            Err(e) => {
                tracing::warn!("Seek failed: {e}");
                PlaybackUpdate::none()
            }
        }
    }

    /// Jumps `step` forward or backward within the current track (no-op for
    /// tracks without a duration, i.e. streams).
    pub fn seek_by(&mut self, forward: bool, step: Duration) -> PlaybackUpdate {
        let Some(duration) = self.playback.current_track.as_ref().map(|t| t.duration) else {
            return PlaybackUpdate::none();
        };
        if duration == Duration::ZERO {
            return PlaybackUpdate::none();
        }
        let position = self.playback.position;
        let target = if forward {
            (position + step).min(duration)
        } else {
            position.saturating_sub(step)
        };
        self.seek_fraction(target.as_secs_f32() / duration.as_secs_f32())
    }

    /// Sets the master volume, live only: the config is written by
    /// [`Self::commit_volume`] once the gesture settles, so dragging the
    /// slider doesn't write the config on every frame.
    pub fn set_volume(&mut self, volume: f32) -> PlaybackUpdate {
        if let Some(player) = &mut self.playback.player
            && let Err(e) = player.set_volume(volume)
        {
            tracing::error!("Set volume failed: {e}");
        }
        // Mirrored to MPRIS right away: the periodic publish only runs while
        // playing, so a change made while paused would read stale on D-Bus.
        let mpris = self.publish_mpris();
        let mpd = match self.playback.mpd_client() {
            Some(client) => {
                let level = (volume.clamp(0.0, 1.0) * 100.0) as u8;
                mpd::set_volume(client, level)
            }
            None => Effect::none(),
        };
        PlaybackUpdate::effect(Effect::batch([mpris, mpd]))
    }

    /// Persists the current master volume to the config.
    pub fn commit_volume(&mut self) {
        let volume = self.current_volume();
        self.config.config.volume = volume;
        self.config.save();
    }

    /// Sets and persists the volume (discrete changes: shortcuts, MPRIS).
    pub fn set_volume_committed(&mut self, volume: f32) -> PlaybackUpdate {
        let update = self.set_volume(volume);
        self.commit_volume();
        update
    }

    /// Changes the volume by `delta`, clamped to 0..=1, and persists it.
    pub fn nudge_volume(&mut self, delta: f32) -> PlaybackUpdate {
        let target = (self.current_volume() + delta).clamp(0.0, 1.0);
        self.set_volume_committed(target)
    }

    /// Mutes, or restores the level from before the mute.
    pub fn toggle_mute(&mut self) -> PlaybackUpdate {
        let current = self.current_volume();
        let target = mute_target(current, &mut self.playback.pre_mute_volume);
        self.set_volume_committed(target)
    }

    /// The player's volume (the configured one without a player).
    fn current_volume(&self) -> f32 {
        self.playback
            .player
            .as_ref()
            .map_or(self.config.config.volume, |p| p.volume())
    }

    /// Flips shuffle (config, saved, + queue).
    pub fn toggle_shuffle(&mut self) -> PlaybackUpdate {
        self.config.config.shuffle = !self.config.config.shuffle;
        if let Some(player) = &mut self.playback.player {
            player.set_shuffle(self.config.config.shuffle);
        }
        self.config.save();
        PlaybackUpdate::effect(self.publish_mpris())
    }

    /// Steps the repeat mode (config, saved, + queue).
    pub fn cycle_repeat(&mut self) -> PlaybackUpdate {
        let mode = self.config.config.repeat_mode.next();
        self.set_repeat_mode(mode)
    }

    /// Sets the repeat mode (config, saved, + queue).
    pub fn set_repeat_mode(&mut self, mode: RepeatMode) -> PlaybackUpdate {
        self.config.config.repeat_mode = mode;
        if let Some(player) = &mut self.playback.player {
            player.set_repeat_mode(mode);
        }
        self.config.save();
        PlaybackUpdate::effect(self.publish_mpris())
    }

    // -- Queue ---------------------------------------------------------

    /// Jumps to and plays the queue entry at play-order index `index`.
    pub fn queue_jump(&mut self, index: usize) -> PlaybackUpdate {
        let Some(player) = &mut self.playback.player else {
            return PlaybackUpdate::none();
        };
        let result = player.jump_to(index);
        self.start_or_log(result, "Queue jump failed")
    }

    /// Removes the queue entry at play-order index `index`.
    pub fn queue_remove(&mut self, index: usize) -> PlaybackUpdate {
        let Some(player) = &mut self.playback.player else {
            return PlaybackUpdate::none();
        };
        match player.queue_remove(index) {
            Ok(QueueEditOutcome::NowPlaying(track)) => self.start_track(track),
            Ok(QueueEditOutcome::Stopped) => {
                self.playback.current_track = None;
                self.playback.position = Duration::ZERO;
                PlaybackUpdate::effect(self.publish_mpris())
            }
            Ok(QueueEditOutcome::Unchanged) => PlaybackUpdate::none(),
            Err(e) => {
                tracing::error!("Queue remove failed: {e}");
                PlaybackUpdate::none()
            }
        }
    }

    /// Moves a queue entry between two play-order indices.
    pub fn queue_move(&mut self, from: usize, to: usize) {
        if let Some(player) = &mut self.playback.player {
            player.queue_move(from, to);
        }
    }

    /// Drops every queue entry except the one currently playing.
    pub fn queue_clear(&mut self) {
        if let Some(player) = &mut self.playback.player {
            player.queue_clear_upcoming();
        }
    }

    /// Queues `tracks` (right after the current track, or at the end). When
    /// the queue was empty this starts playback; otherwise the update
    /// carries [`PlaybackNotice::Queued`].
    pub fn queue_tracks(
        &mut self,
        tracks: Vec<Track>,
        placement: QueuePlacement,
    ) -> PlaybackUpdate {
        if tracks.is_empty() {
            return PlaybackUpdate::none();
        }
        let added = tracks.len();
        let Some(player) = &mut self.playback.player else {
            return PlaybackUpdate::none();
        };
        let (result, failure) = match placement {
            QueuePlacement::Next => (player.queue_insert_next(tracks), "Play next failed"),
            QueuePlacement::End => (player.queue_append(tracks), "Add to queue failed"),
        };
        match result {
            Ok(Some(track)) => self.start_track(track),
            Ok(None) => {
                let mut update = PlaybackUpdate::none();
                update.notice = Some(PlaybackNotice::Queued(added));
                update
            }
            Err(e) => {
                tracing::error!("{failure}: {e}");
                PlaybackUpdate::none()
            }
        }
    }

    /// Queues the tracks `source` names ("Play next" / "Add to queue" from
    /// the library views).
    pub fn queue_from(&mut self, source: QueueSource, next: bool) -> PlaybackUpdate {
        let tracks = source.resolve(&self.library, &self.playlists.playlists);
        let placement = if next {
            QueuePlacement::Next
        } else {
            QueuePlacement::End
        };
        self.queue_tracks(tracks, placement)
    }

    // -- Ticks and backend events --------------------------------------

    /// The playback tick (twice a second): refreshes the position (unless
    /// the user is dragging the seek slider, see [`Self::set_seeking`]),
    /// advances a finished track, follows radio ICY titles, saves the podcast
    /// position, drives the scrobblers and the play history and republishes
    /// MPRIS and the extras housekeeping.
    pub fn playback_tick(&mut self) -> PlaybackUpdate {
        let mut update = PlaybackUpdate::none();
        let mut failure = None;
        if let Some(player) = &mut self.playback.player {
            // Backends that load asynchronously (the browser's) report a
            // failed load here, after `play` returned.
            failure = player.take_playback_error();
            if !self.playback.seeking {
                self.playback.position =
                    capped_position(player.position(), self.playback.current_track.as_ref());
            }
            // MPD's own end-of-track advance is handled by
            // `apply_mpd_status` (which polls far more precisely and is the
            // only path that also dispatches the next async MPD command) —
            // this generic ticker fires regardless of backend, so it would
            // otherwise race that poll and double-advance the queue.
            if player.active_backend_type() != ActiveBackend::Mpd
                && player.is_finished().unwrap_or(false)
            {
                let result = player.advance_on_finish();
                update = self.start_or_log(result, "Advance on finish failed");
            }
        }

        if let Some(reason) = failure {
            update.notice = Some(PlaybackNotice::PlayFailed(reason));
        }

        let mut position_save = Effect::none();
        let mut server_scrobble = Effect::none();
        if let Some(track) = self.playback.current_track.clone() {
            match &*track.provider_id {
                "radio" => {
                    if let Some(player) = &self.playback.player
                        && let Some(title) = player.icy_title()
                        && !title.is_empty()
                        && let Some(current) = &mut self.playback.current_track
                        && current.title != title
                    {
                        current.title = title;
                    }
                }
                "podcast" => {
                    if let Some(episode_id) = self.podcasts.current_episode_id {
                        let secs = self.playback.position.as_secs();
                        if secs != self.podcasts.last_saved_position_secs && secs.is_multiple_of(5)
                        {
                            self.podcasts.last_saved_position_secs = secs;
                            let position_ms = self.playback.position.as_millis() as i64;
                            position_save = self
                                .podcasts
                                .save_position(episode_id, position_ms, false)
                                .map(|_| PlaybackEvent::PodcastPositionSaved);
                        }
                    }
                }
                _ => {}
            }
            server_scrobble = self.handle_scrobble(track);
        }
        let mpris = self.publish_mpris();
        let extras = self.playback_extras_tick();
        let history = self
            .track_play_history()
            .map(|_| PlaybackEvent::PlayRecorded);
        update.also(Effect::batch([
            position_save,
            server_scrobble,
            mpris,
            extras,
            history,
        ]))
    }

    /// Applies a polled MPD status. While the user drags the seek slider
    /// (see [`Self::set_seeking`]) the position is left alone.
    pub fn apply_mpd_status(&mut self, status: MpdStatusUpdate) -> PlaybackUpdate {
        let MpdStatusUpdate {
            position,
            duration,
            state,
            volume,
            song,
        } = status;
        let mut track_changed = false;
        if let Some(player) = &mut self.playback.player {
            if let Some(mpd) = player.mpd_backend_mut() {
                mpd.update_status(position, duration, state, volume);
            }

            // The poll runs whenever an MPD backend exists, which includes
            // while Aulos plays a local file, radio stream or podcast through
            // the local backend (opening a file from a file manager does
            // exactly that). Only let MPD drive the shared UI state when it
            // is the backend actually in charge — otherwise its position and
            // song would overwrite the locally-playing track. A stopped
            // local backend means nothing else is playing, which is precisely
            // the just-switched-to-MPD case that must still adopt.
            if player.active_backend_type() != ActiveBackend::Mpd
                && player.state() != PlayerState::Stopped
            {
                return PlaybackUpdate::none();
            }

            // Adopt a track MPD is already playing that Aulos didn't start
            // itself — e.g. right after switching to this MPD backend while
            // its server was already mid-playback, or MPD's song changed out
            // from under Aulos (another client skipped tracks). `song` is
            // only `Some` on the tick where MPD's song identity changed.
            if let Some(track) = song
                && state != PlayerState::Stopped
            {
                player.adopt_mpd_track(track.clone(), duration);
                self.playback.current_track = Some(track);
                track_changed = true;
            }

            if !self.playback.seeking {
                self.playback.position =
                    capped_position(position, self.playback.current_track.as_ref());
            }

            // MPD reports Stopped after playback — a NATURAL end, so
            // repeat-one replays and repeat-none stops per `advance(auto =
            // true)`'s contract. This is the one authoritative MPD
            // end-of-track path (see `playback_tick`, which skips it for MPD
            // to avoid double-advancing the queue).
            if player.is_finished().unwrap_or(false) {
                match player.advance_on_finish() {
                    Ok(Some(track)) => {
                        self.playback.current_track = Some(track);
                        self.playback.position = Duration::ZERO;
                        track_changed = true;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::error!("MPD advance on finish failed: {e}"),
                }
            }
        }

        let update = if track_changed {
            self.on_track_changed()
        } else {
            PlaybackUpdate::none()
        };
        let server_scrobble = match self.playback.current_track.clone() {
            Some(track) => self.handle_scrobble(track),
            None => Effect::none(),
        };
        update.also(Effect::batch([server_scrobble, self.publish_mpris()]))
    }

    /// Scrobbling for the current track: the multi-service scrobblers
    /// (ListenBrainz / Last.fm / Libre.fm) while playing, and the Subsonic
    /// server's own "now playing" (once per track) and scrobble (at 50% of
    /// the duration or 4 minutes, whichever comes first).
    fn handle_scrobble(&mut self, track: Track) -> Effect<PlaybackEvent> {
        if self.playback.is_playing() {
            self.playback
                .scrobble
                .on_playback(&track, self.playback.position);
        }
        self.scrobble_to_subsonic(&track)
    }

    /// The Subsonic half of [`Self::handle_scrobble`]: the server's own
    /// "now playing" and scrobble, when `track` comes from a Subsonic server.
    fn scrobble_to_subsonic(&mut self, track: &Track) -> Effect<PlaybackEvent> {
        let Some(provider) = self
            .providers
            .subsonic_providers
            .iter()
            .find(|p| p.id() == &*track.provider_id)
            .cloned()
        else {
            return Effect::none();
        };

        // The Subsonic song ID is stored in track.path (set by child_to_track).
        let song_id = track.path.to_string_lossy().to_string();

        let now_playing = if self.playback.scrobble_now_playing_sent {
            false
        } else {
            self.playback.scrobble_now_playing_sent = true;
            true
        };

        let mut scrobble = false;
        if !self.playback.scrobble_sent {
            let threshold = scrobble_threshold(track.duration);
            if self.playback.position >= threshold && threshold > Duration::ZERO {
                self.playback.scrobble_sent = true;
                scrobble = true;
            }
        }

        if !now_playing && !scrobble {
            return Effect::none();
        }
        Effect::future(async move {
            if now_playing {
                provider.now_playing(&song_id).await;
            }
            if scrobble {
                provider.scrobble(&song_id).await;
            }
            PlaybackEvent::ServerScrobbled
        })
    }

    /// Applies the result of earlier asynchronous work.
    pub fn apply_playback_event(&mut self, event: PlaybackEvent) -> PlaybackUpdate {
        match event {
            PlaybackEvent::MpdCommand(result) => match result {
                // The status poll confirms the command's effect, but the
                // position/state catch up right away.
                Ok(()) => self.playback_tick(),
                Err(e) => {
                    // The next status poll corrects the UI state.
                    tracing::error!("Async MPD command failed: {e}");
                    PlaybackUpdate::none()
                }
            },
            PlaybackEvent::PlayRecorded => {
                self.home.cache.invalidate();
                PlaybackUpdate::none()
            }
            PlaybackEvent::MprisArt { track_id, art_url } => {
                if let Some(handle) = &self.playback.mpris {
                    handle.cache_art_url(track_id, art_url);
                }
                PlaybackUpdate::effect(self.publish_mpris())
            }
            PlaybackEvent::Lyrics(lyrics) => {
                self.playback.lyrics_loading = false;
                self.playback.lyrics = lyrics;
                PlaybackUpdate::none()
            }
            PlaybackEvent::FilesRead(tracks) => {
                if tracks.is_empty() {
                    tracing::warn!("No playable tracks found among opened files");
                    let mut update = PlaybackUpdate::none();
                    update.notice = Some(PlaybackNotice::OpenFilesFailed);
                    return update;
                }
                self.play_tracks(tracks, 0)
            }
            PlaybackEvent::PodcastPositionSaved
            | PlaybackEvent::InhibitReleased
            | PlaybackEvent::ServerScrobbled => PlaybackUpdate::none(),
            PlaybackEvent::InhibitAcquired(token) => {
                // A token comes back only when playback stopped while the
                // request was in flight.
                let wanted = self.wants_inhibit();
                let release = self
                    .playback
                    .extras
                    .inhibit_acquired(token, wanted, Instant::now())
                    .map_or_else(Effect::none, release_inhibit);
                PlaybackUpdate::effect(release)
            }
            PlaybackEvent::NotificationSent(id) => {
                if let Some(id) = id {
                    self.playback.extras.set_notification_id(id);
                }
                PlaybackUpdate::none()
            }
        }
    }

    // -- MPRIS ---------------------------------------------------------

    /// Forwards the current player/config state to the media-session handle
    /// (MPRIS on Linux, SMTC / Now Playing elsewhere), once its backend is
    /// up. `MprisHandle::publish` diffs
    /// internally, so calling this on every tick is cheap.
    ///
    /// Cover art resolution involves blocking file I/O (tag parsing, an
    /// on-disk cache write) the first time a track's art is requested, so it
    /// is never done synchronously here: if the current track's art isn't
    /// cached on the handle yet, this publishes without `art_url` for now and
    /// returns the effect that resolves it ([`PlaybackEvent::MprisArt`]).
    pub fn publish_mpris(&self) -> Effect<PlaybackEvent> {
        let Some(handle) = self.playback.mpris.as_ref() else {
            return Effect::none();
        };
        let track = self.playback.current_track.as_ref();
        let (art_url, art_effect) = match track {
            Some(t) => match handle.cached_art_url(t.id) {
                Some(cached) => (cached, Effect::none()),
                None => {
                    let (track_id, path) = (t.id, t.path.clone());
                    (
                        None,
                        crate::mpris::resolve_art_url(track_id, path)
                            .map(move |art_url| PlaybackEvent::MprisArt { track_id, art_url }),
                    )
                }
            },
            None => (None, Effect::none()),
        };

        handle.publish(self.media_snapshot(art_url));
        art_effect
    }

    /// What the media session shows and offers right now; `art_url` is the
    /// current track's resolved cover, if any.
    pub fn media_snapshot(&self, art_url: Option<String>) -> MprisSnapshot {
        let player = self.playback.player.as_ref();
        let status = match player.map(Player::state) {
            Some(PlayerState::Playing) => MprisStatus::Playing,
            Some(PlayerState::Paused) => MprisStatus::Paused,
            Some(PlayerState::Stopped) | None => MprisStatus::Stopped,
        };
        let loop_mode = match self.config.config.repeat_mode {
            RepeatMode::None => LoopMode::None,
            RepeatMode::All => LoopMode::Playlist,
            RepeatMode::One => LoopMode::Track,
        };
        let track = self.playback.current_track.as_ref();
        MprisSnapshot {
            status,
            title: track.map(|t| t.title.clone()).unwrap_or_default(),
            artist: track.map(|t| t.artist.clone()).unwrap_or_default(),
            album: track.map(|t| t.album.clone()).unwrap_or_default(),
            album_artist: track.map(|t| t.album_artist.clone()).unwrap_or_default(),
            genre: track.map(|t| t.genre.clone()).unwrap_or_default(),
            track_id: track.map(|t| t.id).unwrap_or(0),
            length_us: track.map(|t| t.duration.as_micros() as i64).unwrap_or(0),
            position_us: self.playback.position.as_micros() as i64,
            art_url,
            volume: self.current_volume() as f64,
            shuffle: self.config.config.shuffle,
            loop_mode,
            can_go_next: player.is_some_and(Player::has_next),
            can_go_previous: player.is_some_and(Player::has_previous),
            can_seek: track.is_some_and(|t| &*t.provider_id != "radio"),
            can_play: track.is_some(),
        }
    }

    /// Takes over the media-session handle the backend announced
    /// ([`crate::mpris::MprisEvent::Ready`]) and publishes the current state
    /// through it.
    pub fn connect_media_session(&mut self, handle: crate::mpris::MprisHandle) -> PlaybackUpdate {
        self.playback.mpris = Some(handle);
        PlaybackUpdate::effect(self.publish_mpris())
    }

    /// Carries out a playback command relayed from MPRIS. The window and
    /// process commands (`Raise`, `Quit`, `OpenUri`) belong to the UI and do
    /// nothing here. The caller republishes MPRIS afterwards so clients see
    /// the result immediately.
    pub fn apply_mpris_command(&mut self, command: MprisCommand) -> PlaybackUpdate {
        match command {
            MprisCommand::Play => {
                if self.playback.is_playing() {
                    PlaybackUpdate::none()
                } else {
                    self.toggle_playback()
                }
            }
            MprisCommand::Pause => {
                if self.playback.is_playing() {
                    self.toggle_playback()
                } else {
                    PlaybackUpdate::none()
                }
            }
            MprisCommand::Stop => self.stop(),
            MprisCommand::PlayPause => self.toggle_playback(),
            MprisCommand::Next => self.next_track(),
            MprisCommand::Previous => self.previous_track(),
            MprisCommand::Seek(offset_us) | MprisCommand::SetPosition(offset_us) => {
                let relative = matches!(command, MprisCommand::Seek(_));
                let fraction = self.playback.current_track.as_ref().and_then(|track| {
                    mpris_seek_fraction(self.playback.position, track.duration, offset_us, relative)
                });
                match fraction {
                    Some(fraction) => self.seek_fraction(fraction),
                    None => PlaybackUpdate::none(),
                }
            }
            MprisCommand::SetVolume(volume) => {
                self.set_volume_committed(volume.clamp(0.0, 1.0) as f32)
            }
            MprisCommand::Shuffle(enabled) => {
                if self.config.config.shuffle == enabled {
                    PlaybackUpdate::none()
                } else {
                    self.toggle_shuffle()
                }
            }
            MprisCommand::Loop(mode) => self.set_repeat_mode(match mode {
                LoopMode::None => RepeatMode::None,
                LoopMode::Playlist => RepeatMode::All,
                LoopMode::Track => RepeatMode::One,
            }),
            MprisCommand::Raise | MprisCommand::Quit | MprisCommand::OpenUri(_) => {
                PlaybackUpdate::none()
            }
        }
    }

    // -- Playback settings ---------------------------------------------

    /// Sets the crossfade duration (config, local backend and MPD).
    pub fn set_crossfade(&mut self, secs: f32) -> Effect<PlaybackEvent> {
        self.config.config.crossfade_duration_secs = secs;
        if let Some(player) = &mut self.playback.player {
            player.set_crossfade(secs);
        }
        match self.active_mpd_provider() {
            Some(provider) => mpd::set_crossfade(provider, secs as u64),
            None => Effect::none(),
        }
    }

    /// Sets the replay gain mode (config, local backend and MPD).
    pub fn set_replay_gain_mode(&mut self, mode: ReplayGainMode) -> Effect<PlaybackEvent> {
        self.config.config.replay_gain_mode = mode;
        if let Some(player) = &mut self.playback.player {
            player.set_replay_gain_mode(mode);
        }
        match self.active_mpd_provider() {
            Some(provider) => mpd::set_replay_gain_mode(provider, mode),
            None => Effect::none(),
        }
    }

    /// The active provider, if it is an MPD one.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn active_mpd_provider(&self) -> Option<mpd::Provider> {
        let active_id = self.providers.registry.active_id();
        self.providers
            .mpd_providers
            .iter()
            .find(|p| p.id() == active_id)
            .cloned()
    }

    /// The browser has no MPD provider.
    #[cfg(target_arch = "wasm32")]
    pub fn active_mpd_provider(&self) -> Option<mpd::Provider> {
        None
    }

    /// Replaces the player with one using the active provider's backend
    /// (MPD when it is connected), re-applying the saved EQ, volume and
    /// extras settings.
    pub fn recreate_player(&mut self) {
        let active_id = self.providers.registry.active_id();
        #[cfg(not(target_arch = "wasm32"))]
        let mpd_backend = self
            .providers
            .mpd_providers
            .iter()
            .find(|p| p.id() == active_id)
            .and_then(|mpd| {
                let client = mpd.client_clone()?;
                Some(crate::player::mpd_backend::MpdBackend::new(client))
            });
        #[cfg(target_arch = "wasm32")]
        let mpd_backend: Option<crate::player::mpd_backend::MpdBackend> = {
            _ = active_id;
            None
        };
        match Player::new(mpd_backend) {
            #[allow(unused_mut)]
            Ok(mut p) => {
                // Re-wire PCM buffer for visualizer
                #[cfg(feature = "visualizer")]
                {
                    tracing::debug!("Reconnecting PCM buffer to new player instance");
                    p.set_pcm_buffer(Arc::clone(&self.playback.pcm_buffer));
                    p.set_pcm_capture(self.playback.capture_wanted);
                }

                // Apply saved EQ state to the new player's DSP.
                let eq = p.eq_controller();
                eq.set_enabled(self.config.config.equalizer_enabled);
                if self.config.config.equalizer_bands.len() == 10 {
                    let mut gains = [0.0_f32; 10];
                    gains.copy_from_slice(&self.config.config.equalizer_bands);
                    eq.set_all(&gains);
                }
                eq.set_preamp_db(self.config.config.equalizer_preamp);

                // Apply the saved master volume; a fresh `Player` hardcodes
                // 0.8, so without this a provider switch would reset the
                // level instead of preserving it.
                if let Err(e) = p.set_volume(self.config.config.volume) {
                    tracing::warn!("Failed to apply saved volume: {e}");
                }

                p.set_fade_secs(self.config.config.fade_duration_secs);
                super::preferences::apply_playback_settings(&mut p, &self.config.config);
                self.playback.player = Some(p);
            }
            Err(e) => {
                tracing::error!("Failed to recreate player: {e}");
                self.playback.player = None;
            }
        }
    }

    // -- Extras: stop-after, party, auto-play, inhibit, notifications ---

    /// Toggles "stop after the current track".
    pub fn toggle_stop_after(&mut self) {
        if let Some(player) = &mut self.playback.player {
            toggle_stop_after(player);
        }
    }

    /// Sets the fade in/out duration (seconds, clamped).
    pub fn set_fade_duration(&mut self, secs: f32) {
        let secs = secs.clamp(0.0, crate::player::engine::fade::MAX_FADE_SECS);
        self.config.config.fade_duration_secs = secs;
        if let Some(player) = &mut self.playback.player {
            player.set_fade_secs(secs);
        }
    }

    /// Sets the auto-play mode and tops the queue up for it.
    pub fn set_auto_play_mode(&mut self, mode: crate::config::AutoPlayMode) {
        self.config.config.auto_play_mode = mode;
        self.top_up_queue();
    }

    /// Switches party mode; switching it on makes the upcoming queue party
    /// material, starting playback from the pool if nothing is playing.
    pub fn set_party_mode(&mut self, on: bool) -> PlaybackUpdate {
        self.config.config.party_mode = on;
        if !on {
            return PlaybackUpdate::none();
        }
        match self.playback.extras.start_party(
            self.playback.player.as_mut(),
            self.playback.current_track.as_ref(),
            &self.library.tracks,
            &self.config.config,
        ) {
            Some(picks) => self.play_tracks(picks, 0),
            None => PlaybackUpdate::none(),
        }
    }

    /// Adds `genre` to the party pool, or removes it when already present.
    pub fn toggle_party_genre(&mut self, genre: String) {
        toggle_party_genre(&mut self.config.config.party_genres, genre);
        if self.config.config.party_mode {
            // The upcoming queue was drawn from the old pool.
            self.playback
                .extras
                .party_pool_changed(self.playback.player.as_mut());
            self.top_up_queue();
        }
    }

    /// Appends more music when the queue is about to run out (auto-random /
    /// auto-similar) or party mode wants its look-ahead refilled.
    fn top_up_queue(&mut self) {
        self.playback.extras.top_up_queue(
            self.playback.player.as_mut(),
            self.playback.current_track.as_ref(),
            &self.library.tracks,
            &self.config.config,
        );
    }

    /// Switches suspend/idle inhibition while playing.
    pub fn set_inhibit_while_playing(&mut self, on: bool) -> Effect<PlaybackEvent> {
        self.config.config.inhibit_while_playing = on;
        self.playback.extras.clear_inhibit_retry();
        self.sync_inhibit()
    }

    /// Housekeeping run by every playback tick (and, while an inhibit lock
    /// is held, by a slow timer so a pause or stop still releases it): keeps
    /// the queue topped up, the inhibit lock in step with the playback
    /// state, and the engine's fade duration in step with the (possibly
    /// externally edited) config.
    pub fn playback_extras_tick(&mut self) -> Effect<PlaybackEvent> {
        if let Some(player) = &mut self.playback.player {
            player.set_fade_secs(self.config.config.fade_duration_secs);
        }
        self.top_up_queue();
        self.sync_inhibit()
    }

    fn wants_inhibit(&self) -> bool {
        wants_inhibit(
            self.config.config.inhibit_while_playing,
            self.playback.player.as_ref(),
        )
    }

    /// Acquires/releases the suspend+idle inhibition to match playback.
    fn sync_inhibit(&mut self) -> Effect<PlaybackEvent> {
        let want = self.wants_inhibit();
        match self.playback.extras.sync_inhibit(want, Instant::now()) {
            InhibitStep::Acquire => Effect::future(async {
                PlaybackEvent::InhibitAcquired(inhibit::acquire(inhibit::REASON).await)
            }),
            InhibitStep::Release(token) => release_inhibit(token),
            InhibitStep::None => Effect::none(),
        }
    }

    /// Tells the session whether the UI window has focus; track-change
    /// notifications are only shown while it has not.
    pub fn set_window_focused(&mut self, focused: bool) {
        self.playback.window_focused = focused;
    }

    /// Tells the session whether the user is dragging the seek slider; the
    /// playback tick and MPD status leave the displayed position alone
    /// meanwhile.
    pub fn set_seeking(&mut self, seeking: bool) {
        self.playback.seeking = seeking;
    }

    /// A track was (un)favorited in Aulos: mirrors it to Last.fm when
    /// love-sync is on. Called by the library event handling.
    pub(super) fn scrobble_favorite_changed(&mut self, track_id: &str, loved: bool) {
        let track = self
            .library
            .tracks
            .iter()
            .find(|t| t.id.to_string() == track_id)
            .map(|t| &**t)
            .or(self
                .playback
                .current_track
                .as_ref()
                .filter(|t| t.id.to_string() == track_id))
            .cloned();
        if let Some(track) = track {
            self.playback
                .scrobble
                .on_favorite_changed(track_id, &track, loved);
        }
    }

    /// Pops a desktop notification for the track that just became current,
    /// when enabled and the window isn't focused (see
    /// [`Self::set_window_focused`]). The cover comes from
    /// [`Session::cover_bytes`] (only looked up when a notification is
    /// actually shown); a miss loads it from the provider inside the
    /// background job instead of keeping every cover in memory.
    fn notify_track_changed(&mut self) -> Effect<PlaybackEvent> {
        let Some(track) = self.playback.current_track.clone() else {
            return Effect::none();
        };
        let playing = self.playback.is_playing();
        let Some(replaces_id) = self.playback.extras.begin_notification(
            self.config.config.notify_track_change,
            self.playback.window_focused,
            playing,
            &track,
        ) else {
            return Effect::none();
        };

        let cached_cover = self.cover_bytes.get(&track.cover_key());
        let lazy_cover = if cached_cover.is_none() {
            self.providers.registry.active_shared().zip(
                self.library
                    .cover_hint_for(track.grouping_artist(), &track.album),
            )
        } else {
            None
        };

        Effect::future(async move {
            let for_cover = track.clone();
            let loaded = match (&cached_cover, &lazy_cover) {
                (None, Some((provider, hint))) => provider.get_cover_art(hint).await.ok().flatten(),
                _ => None,
            };
            let image = crate::task::spawn_blocking(move || {
                let bytes: Option<&[u8]> = match &cached_cover {
                    Some(b) => Some(b.as_slice()),
                    None => loaded.as_deref(),
                };
                notify::prepare_cover(&for_cover, bytes)
            })
            .await
            .ok()
            .flatten();
            let (summary, body) = notify::notification_text(&track);
            let id = match notify::send(&summary, &body, image.as_deref(), replaces_id).await {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::debug!("track notification failed: {e}");
                    None
                }
            };
            PlaybackEvent::NotificationSent(id)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{test_session, test_session_persisting, track};
    use super::*;
    use crate::config::Config;
    use std::sync::Arc;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn scrobble_threshold_is_half_the_track_capped_at_four_minutes() {
        assert_eq!(scrobble_threshold(secs(100)), secs(50));
        assert_eq!(scrobble_threshold(secs(480)), secs(240));
        assert_eq!(scrobble_threshold(secs(3600)), secs(240));
        assert_eq!(scrobble_threshold(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn mpris_seek_is_relative_or_absolute_and_clamped() {
        let (pos, dur) = (secs(30), secs(100));
        let frac = |off, rel| mpris_seek_fraction(pos, dur, off, rel).unwrap();
        assert!((frac(10_000_000, true) - 0.4).abs() < 1e-6);
        assert!((frac(10_000_000, false) - 0.1).abs() < 1e-6);
        assert_eq!(frac(-60_000_000, true), 0.0);
        assert_eq!(frac(500_000_000, false), 1.0);
    }

    #[test]
    fn mpris_seek_needs_a_duration() {
        assert_eq!(mpris_seek_fraction(secs(1), Duration::ZERO, 5, true), None);
    }

    #[test]
    fn mute_remembers_and_restores_the_level() {
        let mut pre = None;
        assert_eq!(mute_target(0.6, &mut pre), 0.0);
        assert_eq!(pre, Some(0.6));
        assert_eq!(mute_target(0.0, &mut pre), 0.6);
        assert_eq!(pre, None);
    }

    #[test]
    fn unmuting_without_a_record_goes_to_full_volume() {
        let mut pre = None;
        assert_eq!(mute_target(0.0, &mut pre), 1.0);
    }

    #[test]
    fn a_finished_mpd_command_runs_the_playback_tick_itself() {
        let mut session = test_session();
        session.playback.current_track = Some(track(7));
        // The tick feeds the play-history tracker, which starts following
        // the current track.
        let _ = session.apply_playback_event(PlaybackEvent::MpdCommand(Err("boom".into())));
        assert_eq!(session.home.tracked_track, None);
        let _ = session.apply_playback_event(PlaybackEvent::MpdCommand(Ok(())));
        assert_eq!(session.home.tracked_track, Some(7));
    }

    #[test]
    fn the_scheduled_tick_feeds_the_play_history_too() {
        let mut session = test_session();
        session.playback.current_track = Some(track(3));
        let _ = session.playback_tick();
        assert_eq!(session.home.tracked_track, Some(3));
    }

    #[test]
    fn a_confirmed_favorite_reaches_the_scrobbler() {
        use crate::session::LibraryEvent;
        let toggled = |track_id: &str| LibraryEvent::FavoriteToggled {
            track_id: track_id.into(),
            optimistic: true,
            result: Ok(true),
        };
        let mut session = test_session();
        session.library.tracks = vec![Arc::new(track(5))];
        session.playback.current_track = Some(track(6));
        for id in ["5", "6", "9"] {
            session.playback.scrobble.suppress_next_love(id.into());
        }

        // A library track, then the current track (not in the library).
        let _ = session.apply_library_event(toggled("5"));
        let _ = session.apply_library_event(toggled("6"));
        assert!(!session.playback.scrobble.is_love_suppressed("5"));
        assert!(!session.playback.scrobble.is_love_suppressed("6"));

        // An unknown track is ignored.
        let _ = session.apply_library_event(toggled("9"));
        assert!(session.playback.scrobble.is_love_suppressed("9"));
    }

    #[test]
    fn shuffle_and_repeat_changes_are_saved() {
        let mut session = test_session_persisting();
        let path = session.config.path.clone().unwrap();
        assert!(!path.exists());
        let last_saved = |session: &Session| {
            session.config.flush();
            Config::load_from(&path)
        };

        let _ = session.toggle_shuffle();
        assert!(last_saved(&session).shuffle);
        let _ = session.toggle_shuffle();
        assert!(!last_saved(&session).shuffle);

        let _ = session.set_repeat_mode(RepeatMode::One);
        assert_eq!(last_saved(&session).repeat_mode, RepeatMode::One);
        let _ = session.cycle_repeat();
        assert_eq!(last_saved(&session).repeat_mode, RepeatMode::One.next());
    }
}
