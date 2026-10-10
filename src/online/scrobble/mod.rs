// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Scrobbling to ListenBrainz, Last.fm and Libre.fm.
//!
//! Layout:
//! - this file: shared types (`Service`, `ScrobbleTrack`, `ScrobbleError`),
//!   the [`Scrobbler`] backend trait and the pure scrobble-eligibility rules;
//! - [`sign`]: Audioscrobbler `api_sig` signing;
//! - [`audioscrobbler`] / [`listenbrainz`]: the HTTP backends;
//! - [`queue`]: persistent offline queue (JSON in the data dir);
//! - [`tracker`]: per-track play-time accounting (now-playing / scrobble
//!   triggers);
//! - [`worker`]: background thread owning the queue, backends and retry
//!   backoff;
//! - [`controller`] and [`view`]: settings state, async connect flows and
//!   the settings UI section.
//!
//! # Last.fm API key
//!
//! Last.fm requires every application to use its own registered API key and
//! shared secret (<https://www.last.fm/api/account/create>). Aulos does not
//! ship one: enter yours under Settings → Scrobbling → Last.fm. The key is
//! stored in the cosmic config, the secret in the system keyring. Libre.fm
//! accepts any key, so a fixed placeholder is used.

pub mod audioscrobbler;
pub mod controller;
pub mod listenbrainz;
pub mod queue;
pub mod sign;
pub mod tracker;
pub mod view;
pub mod worker;

use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use std::time::Duration;

pub use controller::{ScrobbleController, ScrobbleMessage};

/// Tracks shorter than this are never scrobbled (Last.fm rule).
pub const MIN_TRACK_LENGTH: Duration = Duration::from_secs(30);
/// A track is scrobbled once this much of it has been played, at the latest.
pub const MAX_SCROBBLE_THRESHOLD: Duration = Duration::from_secs(240);
/// Play time after which a stream of unknown duration (radio/podcast, only
/// when the user opted in) counts as a listen.
pub const STREAM_SCROBBLE_THRESHOLD: Duration = Duration::from_secs(60);

/// Application name sent as the submission client / media player.
pub const CLIENT_NAME: &str = "Aulos";

/// A scrobbling service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    ListenBrainz,
    LastFm,
    LibreFm,
}

impl Service {
    pub const ALL: [Service; 3] = [Service::ListenBrainz, Service::LastFm, Service::LibreFm];

    /// Stable identifier used in file names and keyring entries.
    pub fn id(self) -> &'static str {
        match self {
            Service::ListenBrainz => "listenbrainz",
            Service::LastFm => "lastfm",
            Service::LibreFm => "librefm",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Service::ListenBrainz => "ListenBrainz",
            Service::LastFm => "Last.fm",
            Service::LibreFm => "Libre.fm",
        }
    }
}

/// One listen, in service-neutral form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrobbleTrack {
    pub artist: String,
    pub title: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub album_artist: String,
    /// Track length in seconds (0 = unknown).
    #[serde(default)]
    pub duration_secs: u32,
    #[serde(default)]
    pub track_number: u32,
    /// Unix time (seconds) at which playback of the track started.
    #[serde(default)]
    pub timestamp: i64,
}

/// Why a submission failed; drives the queue's retry policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrobbleError {
    /// Network error, rate limit or server trouble: keep queued, retry later.
    Transient(String),
    /// Credentials missing/invalid/expired: keep queued, ask to reconnect.
    Auth(String),
    /// The service refused the data itself: retrying cannot help, drop it.
    Rejected(String),
}

impl std::fmt::Display for ScrobbleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScrobbleError::Transient(m) | ScrobbleError::Auth(m) | ScrobbleError::Rejected(m) => {
                f.write_str(m)
            }
        }
    }
}

/// A scrobbling backend. All calls block; the worker thread runs them.
pub trait Scrobbler: Send {
    fn service(&self) -> Service;

    /// Announce what is playing right now.
    fn now_playing(&self, track: &ScrobbleTrack) -> Result<(), ScrobbleError>;

    /// Submit finished listens. `tracks.len()` never exceeds [`Scrobbler::max_batch`].
    fn submit(&self, tracks: &[ScrobbleTrack]) -> Result<(), ScrobbleError>;

    /// Largest batch accepted by one [`Scrobbler::submit`] call.
    fn max_batch(&self) -> usize;

    /// Whether [`Scrobbler::set_loved`] does anything for this service.
    fn supports_love(&self) -> bool {
        false
    }

    /// Love / unlove a track. Default: unsupported, silently succeeds.
    fn set_loved(&self, _artist: &str, _title: &str, _loved: bool) -> Result<(), ScrobbleError> {
        Ok(())
    }
}

/// Shared blocking HTTP client for every scrobbling request.
pub(crate) fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
        reqwest::blocking::Client::builder()
            .user_agent(format!("{CLIENT_NAME}/{}", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new())
    });
    &CLIENT
}

/// Map a transport-level failure to a [`ScrobbleError`].
pub(crate) fn net_error(e: reqwest::Error) -> ScrobbleError {
    ScrobbleError::Transient(format!("network error: {e}"))
}

/// Whether `provider_id` identifies a stream source (radio / podcast) that
/// is only scrobbled when the user opted in.
pub fn is_stream_provider(provider_id: &str) -> bool {
    matches!(provider_id, "radio" | "podcast")
}

/// Whether tracks from `provider_id` may be scrobbled at all.
pub fn source_allowed(provider_id: &str, allow_streams: bool) -> bool {
    !is_stream_provider(provider_id) || allow_streams
}

/// Play time required before a track of `duration` counts as a listen, or
/// `None` if it can never be scrobbled (shorter than 30 s).
///
/// Standard rule: at least half the track or 4 minutes, whichever comes
/// first, for tracks of at least 30 s. `unknown_duration` is the threshold
/// used for streams with no known length (`None` = never scrobble those).
pub fn scrobble_threshold(duration: Duration, allow_unknown: bool) -> Option<Duration> {
    if duration.is_zero() {
        return allow_unknown.then_some(STREAM_SCROBBLE_THRESHOLD);
    }
    if duration < MIN_TRACK_LENGTH {
        return None;
    }
    Some((duration / 2).min(MAX_SCROBBLE_THRESHOLD))
}

/// Whether `played` of a track of `duration` is enough to scrobble it.
pub fn is_scrobble_eligible(duration: Duration, played: Duration, allow_unknown: bool) -> bool {
    scrobble_threshold(duration, allow_unknown).is_some_and(|t| played >= t)
}

impl ScrobbleTrack {
    /// Build from a library track. Returns `None` when artist or title is
    /// missing (services reject such listens) or the source is a stream and
    /// `allow_streams` is off.
    pub fn from_track(track: &crate::library::Track, allow_streams: bool) -> Option<Self> {
        if !source_allowed(&track.provider_id, allow_streams) {
            return None;
        }
        let mut artist = track.artist.trim().to_string();
        let mut title = track.title.trim().to_string();
        if &*track.provider_id == "radio"
            && let Some((a, t)) = split_icy_title(&title)
        {
            artist = a;
            title = t;
        }
        if artist.is_empty() {
            artist = track.album_artist.trim().to_string();
        }
        if artist.is_empty() || title.is_empty() {
            return None;
        }
        Some(Self {
            artist,
            title,
            album: track.album.trim().to_string(),
            album_artist: track.album_artist.trim().to_string(),
            duration_secs: track.duration.as_secs().min(u32::MAX as u64) as u32,
            track_number: track.track_number,
            timestamp: 0,
        })
    }
}

/// Split an ICY stream title of the form `Artist - Title`.
pub fn split_icy_title(title: &str) -> Option<(String, String)> {
    let (artist, song) = title.split_once(" - ")?;
    let (artist, song) = (artist.trim(), song.trim());
    (!artist.is_empty() && !song.is_empty()).then(|| (artist.to_string(), song.to_string()))
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Keyring entry ids (see `crate::credentials`).
pub mod keys {
    use super::Service;

    /// ListenBrainz user token.
    pub fn listenbrainz_token() -> String {
        "scrobble-listenbrainz-token".to_string()
    }

    /// Audioscrobbler session key for Last.fm / Libre.fm.
    pub fn session_key(service: Service) -> String {
        format!("scrobble-{}-session", service.id())
    }

    /// The user's Last.fm API shared secret.
    pub fn lastfm_secret() -> String {
        "scrobble-lastfm-secret".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn threshold_is_half_up_to_four_minutes() {
        assert_eq!(scrobble_threshold(S(200), false), Some(S(100)));
        assert_eq!(scrobble_threshold(S(480), false), Some(S(240)));
        assert_eq!(scrobble_threshold(S(1800), false), Some(S(240)));
        assert_eq!(scrobble_threshold(S(30), false), Some(S(15)));
    }

    #[test]
    fn short_tracks_never_scrobble() {
        assert_eq!(scrobble_threshold(S(29), false), None);
        assert!(!is_scrobble_eligible(S(29), S(29), false));
    }

    #[test]
    fn unknown_duration_requires_opt_in() {
        assert_eq!(scrobble_threshold(S(0), false), None);
        assert_eq!(
            scrobble_threshold(S(0), true),
            Some(STREAM_SCROBBLE_THRESHOLD)
        );
    }

    #[test]
    fn eligibility_boundaries() {
        assert!(!is_scrobble_eligible(S(200), S(99), false));
        assert!(is_scrobble_eligible(S(200), S(100), false));
        assert!(!is_scrobble_eligible(S(600), S(239), false));
        assert!(is_scrobble_eligible(S(600), S(240), false));
    }

    #[test]
    fn stream_sources_need_opt_in() {
        assert!(source_allowed("local", false));
        assert!(source_allowed("navidrome", false));
        assert!(!source_allowed("radio", false));
        assert!(!source_allowed("podcast", false));
        assert!(source_allowed("radio", true));
        assert!(source_allowed("podcast", true));
    }

    #[test]
    fn icy_titles_split_on_dash() {
        assert_eq!(
            split_icy_title("Daft Punk - One More Time"),
            Some(("Daft Punk".into(), "One More Time".into()))
        );
        assert_eq!(split_icy_title("Just a title"), None);
        assert_eq!(split_icy_title(" - x"), None);
    }
}
