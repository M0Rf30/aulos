// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Settings whose change has an effect beyond the stored value: gapless
//! playback, stopping after every track, launch auto-play, the online data
//! toggles. A UI calls these instead of writing [`Config`](crate::config::Config)
//! fields directly (the purely presentational ones — theme, cover tint,
//! reduced motion — it stores itself).

use super::events::SessionEvent;
use super::{Session, SessionUpdate};
use crate::config::Config;
use crate::player::Player;
use crate::task::Effect;

/// Applies the playback settings of `config` to a freshly built `player`
/// (also called when the player is recreated for another backend).
pub(super) fn apply_playback_settings(player: &mut Player, config: &Config) {
    player.set_gapless(config.gapless);
    player.set_crossfade(config.crossfade_duration_secs);
    player.set_replay_gain_mode(config.replay_gain_mode);
    player.set_stop_after_each(config.stop_after_current);
}

impl Session {
    /// Turns gapless playback on or off and saves it.
    pub fn set_gapless(&mut self, on: bool) {
        self.config.config.gapless = on;
        if let Some(player) = &mut self.playback.player {
            player.set_gapless(on);
        }
        self.config.save();
    }

    /// Stores the visualizer's lock, beat sensitivity (clamped to 0–2) and
    /// last preset, saving only when something changed. `preset` of `None`
    /// keeps the stored one.
    pub fn set_visualizer_prefs(
        &mut self,
        locked: bool,
        sensitivity: f32,
        preset: Option<&std::path::Path>,
    ) {
        let sensitivity = Config::clamp_visualizer_sensitivity(sensitivity);
        let config = &mut self.config.config;
        let preset = preset.map(std::path::Path::to_path_buf);
        let changed = config.visualizer_locked != locked
            || (config.visualizer_sensitivity - sensitivity).abs() > f32::EPSILON
            || preset
                .as_ref()
                .is_some_and(|p| config.visualizer_preset.as_ref() != Some(p));
        if !changed {
            return;
        }
        config.visualizer_locked = locked;
        config.visualizer_sensitivity = sensitivity;
        if preset.is_some() {
            config.visualizer_preset = preset;
        }
        self.config.save();
    }

    /// Turns "stop after every track" on or off and saves it. The one-off
    /// form is [`Self::toggle_stop_after`].
    pub fn set_stop_after_each_track(&mut self, on: bool) {
        self.config.config.stop_after_current = on;
        if let Some(player) = &mut self.playback.player {
            player.set_stop_after_each(on);
        }
        self.config.save();
    }

    /// Turns the music-folder watcher on or off and saves it. Take the new
    /// [`Self::background_streams`] into account afterwards.
    pub fn set_watch_for_changes(&mut self, on: bool) {
        self.config.config.watch_for_changes = on;
        self.config.save();
    }

    /// Sets which artist info may be fetched online (Local and MPD
    /// libraries) and saves it. Already fetched bios and photos are
    /// dropped, so they are fetched again under the new rules; the update
    /// tells the UI to drop its photo handles.
    pub fn set_artist_info_fetching(&mut self, bio: bool, photos: bool) -> SessionUpdate {
        let config = &mut self.config.config;
        if config.fetch_artist_bio == bio && config.fetch_artist_photos == photos {
            return SessionUpdate::none();
        }
        config.fetch_artist_bio = bio;
        config.fetch_artist_photos = photos;
        self.config.save();
        self.artist_info.clear();
        let mut update = SessionUpdate::none();
        update.changes.artist_photos_cleared = true;
        update
    }

    /// Turns radio station icon downloads on or off and saves it. Turning
    /// them on fetches the icons of the stations already listed.
    pub fn set_fetch_radio_icons(&mut self, on: bool) -> Effect<SessionEvent> {
        self.config.config.fetch_radio_icons = on;
        self.config.save();
        if !on {
            return Effect::none();
        }
        let urls = self
            .radio
            .stations
            .iter()
            .map(|s| s.favicon_url.clone())
            .chain(self.radio.search_results.iter().map(|r| r.favicon.clone()))
            .chain(std::iter::once(self.radio.now_playing_favicon.clone()))
            .collect();
        self.fetch_icons(urls)
    }

    /// Once the library of a launch is loaded, plays it when the
    /// "auto-play on launch" setting asks for it (from the first track, or
    /// a random one with shuffle on). Does nothing the second time, or with
    /// nothing to play.
    pub(super) fn autoplay_on_launch(&mut self) -> SessionUpdate {
        if !std::mem::take(&mut self.playback.autoplay_pending) || self.library.tracks.is_empty() {
            return SessionUpdate::none();
        }
        let idle = self.playback.player.as_ref().is_some_and(|p| {
            p.queue_is_empty() && p.state() == crate::player::PlaybackState::Stopped
        });
        if idle {
            self.toggle_playback().into()
        } else {
            SessionUpdate::none()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{test_session, test_session_persisting, track};
    use std::sync::Arc;

    #[test]
    fn visualizer_prefs_are_clamped_stored_and_persisted() {
        let mut session = test_session_persisting();
        let preset = std::path::Path::new("/presets/a.milk2");
        session.set_visualizer_prefs(true, 7.0, Some(preset));
        // No preset: the stored one stays.
        session.set_visualizer_prefs(true, 0.5, None);
        session.config.flush();
        let saved = crate::config::Config::load_from(session.config.path.as_ref().unwrap());
        assert!(saved.visualizer_locked);
        assert_eq!(saved.visualizer_sensitivity, 0.5);
        assert_eq!(saved.visualizer_preset.as_deref(), Some(preset));
    }

    #[test]
    fn toggles_are_stored_and_persisted() {
        let mut session = test_session_persisting();
        session.set_gapless(false);
        session.set_stop_after_each_track(true);
        session.set_watch_for_changes(false);
        session.config.flush();
        let saved = crate::config::Config::load_from(session.config.path.as_ref().unwrap());
        assert!(!saved.gapless);
        assert!(saved.stop_after_current);
        assert!(!saved.watch_for_changes);
    }

    #[test]
    fn changing_the_artist_info_rules_clears_the_photos() {
        let mut session = test_session();
        assert!(
            !session
                .set_artist_info_fetching(false, false)
                .changes
                .artist_photos_cleared
        );
        let update = session.set_artist_info_fetching(true, false);
        assert!(update.changes.artist_photos_cleared);
        let wanted = session.artist_info_wanted();
        assert!(wanted.bio && !wanted.photo);
        assert!(session.config.config.fetch_artist_bio);
    }

    #[test]
    fn auto_play_waits_for_tracks_and_runs_once() {
        let mut session = test_session();
        session.playback.autoplay_pending = true;
        // Nothing loaded yet: the flag is still consumed by the first load.
        let _ = session.autoplay_on_launch();
        assert!(!session.playback.autoplay_pending);

        session.library.tracks = vec![Arc::new(track(1))];
        session.playback.autoplay_pending = true;
        let _ = session.autoplay_on_launch();
        assert!(!session.playback.autoplay_pending);
    }
}
