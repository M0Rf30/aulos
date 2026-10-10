// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::cosmic_config::{self, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Serializable configuration for an MPD server connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MpdConfigEntry {
    /// Unique provider ID (e.g., "mpd-home").
    pub id: String,
    /// Human-readable name (e.g., "Home MPD Server").
    pub name: String,
    /// MPD server hostname.
    pub host: String,
    /// MPD server port (default: 6600).
    #[serde(default = "default_mpd_port")]
    pub port: u16,
    /// Optional password for authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Whether the password is stored in the system keyring.
    #[serde(default)]
    pub password_in_keyring: bool,
}

fn default_mpd_port() -> u16 {
    6600
}

impl Default for MpdConfigEntry {
    fn default() -> Self {
        Self {
            id: "mpd".to_string(),
            name: "MPD Server".to_string(),
            host: "localhost".to_string(),
            port: 6600,
            password: None,
            password_in_keyring: false,
        }
    }
}

/// Serializable configuration for an OpenSubsonic/Navidrome server connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubsonicConfigEntry {
    /// Unique provider ID (e.g., "subsonic-home").
    pub id: String,
    /// Human-readable name (e.g., "Navidrome").
    pub name: String,
    /// Server base URL (e.g., "https://music.example.com").
    pub url: String,
    /// Subsonic username.
    pub username: String,
    /// Password (stored as plaintext for now; keyring TODO).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Whether the password is stored in the system keyring.
    #[serde(default)]
    pub password_in_keyring: bool,
    /// Accept invalid TLS certificates (self-signed, Tailscale, etc.).
    #[serde(default)]
    pub accept_invalid_certs: bool,
    /// Maximum bitrate for transcoding (None = original quality).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcoding_max_bitrate: Option<u32>,
    /// Transcoding format (None = original format, e.g., "mp3", "ogg", "opus").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcoding_format: Option<String>,
}

impl Default for SubsonicConfigEntry {
    fn default() -> Self {
        Self {
            id: "subsonic".to_string(),
            name: "Subsonic Server".to_string(),
            url: "https://music.example.com".to_string(),
            username: String::new(),
            password: None,
            password_in_keyring: false,
            accept_invalid_certs: false,
            transcoding_max_bitrate: None,
            transcoding_format: None,
        }
    }
}

/// Repeat mode for playback queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    None,
    All,
    One,
}

impl RepeatMode {
    /// Advance to the next repeat mode: None → All → One → None.
    pub fn next(self) -> Self {
        match self {
            Self::None => Self::All,
            Self::All => Self::One,
            Self::One => Self::None,
        }
    }

    /// Icon name for this repeat mode.
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::One => "media-playlist-repeat-song-symbolic",
            Self::All => "media-playlist-repeat-symbolic",
            Self::None => "media-playlist-no-repeat-symbolic",
        }
    }
}

/// Layout mode for library browsing views (albums, artists, genres).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    Grid,
    List,
}

impl ViewMode {
    /// The other mode — used by the view-toggle button.
    pub fn toggled(self) -> Self {
        match self {
            Self::Grid => Self::List,
            Self::List => Self::Grid,
        }
    }
}

/// Replay gain mode for volume normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayGainMode {
    Off,
    Track,
    Album,
    Auto,
}

/// What Aulos does when the play queue runs out of tracks (Lollypop's
/// `auto_random` / `auto_similar` repeat modes). Only applies to the local
/// playback engine, and only while `RepeatMode` isn't looping the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoPlayMode {
    /// Playback stops at the end of the queue.
    #[default]
    Off,
    /// Keep playing random albums from the library.
    Random,
    /// Keep playing albums by the same artist / in the same genre / era.
    Similar,
}

impl AutoPlayMode {
    /// All modes, in the order shown in menus and dropdowns.
    pub const ALL: [AutoPlayMode; 3] = [Self::Off, Self::Random, Self::Similar];
}

/// Persistent configuration stored via cosmic-config.
#[derive(Debug, Clone, CosmicConfigEntry, PartialEq)]
#[version = 2]
pub struct Config {
    /// Music library directories to scan.
    pub music_dirs: Vec<PathBuf>,
    /// Whether to split multi-artist tags (e.g. "A feat. B", "A; B") into
    /// individual artists at aggregation time. The raw tag stored per
    /// track is never modified — this only affects how albums/tracks are
    /// grouped into `Artist` entries for browsing.
    pub split_artist_tags: bool,
    /// Delimiters tried (in order) when splitting a raw artist tag; see
    /// `crate::library::artist_tags::split`.
    pub artist_tag_delimiters: Vec<String>,
    /// Master volume (0.0 - 1.0).
    pub volume: f32,
    /// Whether shuffle is enabled.
    pub shuffle: bool,
    /// Repeat mode for playback queue.
    pub repeat_mode: RepeatMode,
    /// 10-band equalizer gains in dB (-12.0 to +12.0).
    pub equalizer_bands: Vec<f32>,
    /// Whether the equalizer is enabled.
    pub equalizer_enabled: bool,
    /// Preamp gain in dB (-20.0 to +10.0), applied before EQ bands.
    pub equalizer_preamp: f32,
    /// Name of the currently active EQ preset (empty = no preset selected).
    pub active_eq_preset_name: String,
    /// Last active view ("albums", "artists", "songs", "playlists").
    pub last_view: String,
    /// Section shown at startup: `"home"` (default), `"last"` (restore
    /// `last_view`) or a page key such as `"albums"` — see
    /// `crate::app::startup`. Unknown/unavailable values fall back to Home.
    pub startup_page: String,
    /// Provider active at startup: `"last"` (default — the provider that
    /// was active when the app last exited) or a provider id (`"local"`,
    /// an MPD/Subsonic server id). Falls back via `resolve_active_provider`.
    pub startup_provider: String,
    /// Configured MPD server connections.
    pub mpd_servers: Vec<MpdConfigEntry>,
    /// Configured OpenSubsonic/Navidrome server connections.
    pub subsonic_servers: Vec<SubsonicConfigEntry>,
    /// Crossfade duration in seconds (0 = disabled).
    pub crossfade_duration_secs: f32,
    /// Replay gain mode.
    pub replay_gain_mode: ReplayGainMode,
    /// Layout mode for the albums view.
    pub albums_view_mode: ViewMode,
    /// Layout mode for the artists view.
    pub artists_view_mode: ViewMode,
    /// Layout mode for the genres view.
    pub genres_view_mode: ViewMode,
    /// Output directory for local file conversion jobs. `None` uses
    /// `dirs::audio_dir()/Converted` (falling back to `~/Converted`).
    pub convert_out_dir: Option<PathBuf>,
    /// Output format for local file conversion jobs.
    pub convert_format: crate::convert::OutputFormat,
    /// Output sample rate for local file conversion jobs; `None` keeps
    /// each source's original rate.
    pub convert_sample_rate: Option<u32>,
    /// Whether the (experimental) local file converter/transcoder/ripper
    /// is enabled. Off by default: shows/hides the Convert nav entry and
    /// gates its progress-ticker subscription (see
    /// `crate::app::init::insert_convert_nav_entry` and
    /// `crate::app::subscriptions`).
    pub experimental_converter: bool,
    /// FLAC compression level / bit-depth choice for local file
    /// conversion jobs.
    pub flac_options: crate::convert::encoder::FlacOptions,
    /// Bitrate/quality knobs for the `ffmpeg`-backed lossy formats (MP3,
    /// AAC, Opus, Ogg Vorbis) in local file conversion jobs.
    pub lossy_options: crate::convert::encoder::LossyOptions,
    /// Id of the provider that was active when the app last exited
    /// (`"local"`, an MPD server id, or a Subsonic server id). Restored on
    /// the next startup via `resolve_active_provider`; falls back to the
    /// local provider if the saved id is no longer registered.
    pub active_provider: Option<String>,
    /// Whether Aulos fetches artist images/biography from online sources
    /// (Deezer for images, Wikipedia for bios — both keyless) when
    /// browsing in Local or MPD mode. Off by default: purely opt-in
    /// network access. Ignored in Subsonic mode, which always shows the
    /// server's own artist info instead — see
    /// `crate::provider::subsonic::SubsonicProvider::get_artist_info`.
    pub fetch_artist_info: bool,
    /// Size multiplier for cards in grid views (albums, artists, genres,
    /// folders, radio, podcasts…), 0.7–1.6; 1.0 is the default size.
    pub grid_scale: f32,
    /// Whether the first-run intro jingle ("it really pipes the satyr's
    /// ass") has already been played.
    pub intro_played: bool,
    /// Fade in/out duration (seconds) applied on play, pause, stop and
    /// manual track skips. `0.0` disables the fade.
    pub fade_duration_secs: f32,
    /// Continue with random / similar music when the queue runs out.
    pub auto_play_mode: AutoPlayMode,
    /// Party mode: endless random playback restricted to `party_genres`.
    pub party_mode: bool,
    /// Genres party mode draws from (empty = the whole library).
    pub party_genres: Vec<String>,
    /// Show a desktop notification (with cover art) on track change while
    /// the window isn't focused.
    pub notify_track_change: bool,
    /// Inhibit suspend/idle while music is playing.
    pub inhibit_while_playing: bool,
    /// Keep playing (window minimized) instead of quitting when the
    /// window is closed during playback.
    pub background_playback: bool,
    /// Show the "Various Artists" compilations entry in the Artists view.
    /// When off, compilations are only reachable from the Albums page.
    pub show_compilations_in_artists: bool,
    /// Write playlist file paths relative to the exported M3U file's
    /// directory (instead of absolute paths).
    pub m3u_relative_paths: bool,
    /// Scrobble to ListenBrainz (needs a stored user token).
    pub scrobble_listenbrainz_enabled: bool,
    /// Scrobble to Last.fm (needs a session key and API key/secret).
    pub scrobble_lastfm_enabled: bool,
    /// Scrobble to Libre.fm (needs a session key).
    pub scrobble_librefm_enabled: bool,
    /// ListenBrainz user name; non-empty once a token was validated.
    pub scrobble_listenbrainz_user: String,
    /// Last.fm user name; non-empty once connected.
    pub scrobble_lastfm_user: String,
    /// Libre.fm user name; non-empty once connected.
    pub scrobble_librefm_user: String,
    /// The user's own Last.fm API key (Last.fm requires every app to use
    /// a registered key; empty by default). The matching shared secret is
    /// kept in the system keyring, never here.
    pub scrobble_lastfm_api_key: String,
    /// Also scrobble radio streams and podcasts (off by default).
    pub scrobble_streams: bool,
    /// Love/unlove on Last.fm when a track is (un)favorited in Aulos.
    pub scrobble_lastfm_love_sync: bool,
    /// Import a service's listening history right after connecting it.
    pub scrobble_import_on_connect: bool,
    /// Show "Because you listened to…" shelves (similar artists from
    /// Last.fm / ListenBrainz) on Home. Only effective while a scrobbling
    /// service is connected.
    pub home_online_suggestions: bool,
}

impl Default for Config {
    fn default() -> Self {
        // Default music directory is ~/Music
        let music_dir = dirs::audio_dir().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join("Music")
        });

        Self {
            music_dirs: vec![music_dir],
            split_artist_tags: true,
            artist_tag_delimiters: crate::library::artist_tags::DEFAULT_DELIMITERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            volume: 0.8,
            shuffle: false,
            repeat_mode: RepeatMode::None,
            // 10-band EQ: 31Hz, 62Hz, 125Hz, 250Hz, 500Hz, 1kHz, 2kHz, 4kHz, 8kHz, 16kHz
            equalizer_bands: vec![0.0; 10],
            equalizer_enabled: false,
            equalizer_preamp: 0.0,
            active_eq_preset_name: String::new(),
            last_view: "albums".to_string(),
            startup_page: "home".to_string(),
            startup_provider: "last".to_string(),
            mpd_servers: Vec::new(),
            subsonic_servers: Vec::new(),
            crossfade_duration_secs: 0.0,
            replay_gain_mode: ReplayGainMode::Off,
            albums_view_mode: ViewMode::Grid,
            artists_view_mode: ViewMode::List,
            genres_view_mode: ViewMode::Grid,
            convert_out_dir: None,
            convert_format: crate::convert::OutputFormat::Flac,
            convert_sample_rate: None,
            experimental_converter: false,
            flac_options: crate::convert::encoder::FlacOptions::default(),
            lossy_options: crate::convert::encoder::LossyOptions::default(),
            active_provider: None,
            fetch_artist_info: false,
            grid_scale: 1.0,
            intro_played: false,
            fade_duration_secs: 0.0,
            auto_play_mode: AutoPlayMode::Off,
            party_mode: false,
            party_genres: Vec::new(),
            notify_track_change: true,
            inhibit_while_playing: true,
            background_playback: false,
            show_compilations_in_artists: true,
            m3u_relative_paths: true,
            scrobble_listenbrainz_enabled: false,
            scrobble_lastfm_enabled: false,
            scrobble_librefm_enabled: false,
            scrobble_listenbrainz_user: String::new(),
            scrobble_lastfm_user: String::new(),
            scrobble_librefm_user: String::new(),
            scrobble_lastfm_api_key: String::new(),
            scrobble_streams: false,
            scrobble_lastfm_love_sync: true,
            scrobble_import_on_connect: false,
            home_online_suggestions: true,
        }
    }
}

/// The provider id the user wants active at startup: the explicit
/// `startup_provider` choice, or — for `"last"` (the default, also used
/// for an empty value) — the provider that was active when the app last
/// exited. Feed the result to [`resolve_active_provider`], which still
/// falls back to local / the first registered provider when it is gone.
pub fn startup_provider_preference<'a>(
    startup_provider: &'a str,
    last_active: Option<&'a str>,
) -> Option<&'a str> {
    match startup_provider.trim() {
        "" | "last" => last_active,
        explicit => Some(explicit),
    }
}

impl Config {
    /// See [`startup_provider_preference`].
    pub fn startup_provider_choice(&self) -> Option<&str> {
        startup_provider_preference(&self.startup_provider, self.active_provider.as_deref())
    }
}

/// Choose which provider id should become active, given the persisted
/// choice and the ids currently registered. Pure so it can be unit tested
/// without spinning up a real `ProviderRegistry`.
///
/// - If `saved` is `Some` and still present in `registered_ids`, it wins.
/// - Otherwise falls back to `"local"` if registered.
/// - Otherwise falls back to the first registered id.
/// - Returns `None` only if nothing is registered at all.
pub fn resolve_active_provider(saved: Option<&str>, registered_ids: &[String]) -> Option<String> {
    if let Some(saved) = saved
        && let Some(found) = registered_ids.iter().find(|id| id.as_str() == saved)
    {
        return Some(found.clone());
    }
    if let Some(local) = registered_ids.iter().find(|id| id.as_str() == "local") {
        return Some(local.clone());
    }
    registered_ids.first().cloned()
}

#[cfg(test)]
mod resolve_active_provider_tests {
    use super::resolve_active_provider;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn saved_id_wins_when_still_registered() {
        let registered = ids(&["local", "mpd-home", "subsonic-navidrome"]);
        assert_eq!(
            resolve_active_provider(Some("mpd-home"), &registered),
            Some("mpd-home".to_string())
        );
    }

    #[test]
    fn falls_back_to_local_when_saved_id_missing() {
        let registered = ids(&["local", "subsonic-navidrome"]);
        assert_eq!(
            resolve_active_provider(Some("mpd-gone"), &registered),
            Some("local".to_string())
        );
    }

    #[test]
    fn falls_back_to_local_when_nothing_saved() {
        let registered = ids(&["local", "mpd-home"]);
        assert_eq!(
            resolve_active_provider(None, &registered),
            Some("local".to_string())
        );
    }

    #[test]
    fn falls_back_to_first_registered_when_no_local() {
        let registered = ids(&["mpd-home", "subsonic-navidrome"]);
        assert_eq!(
            resolve_active_provider(Some("gone"), &registered),
            Some("mpd-home".to_string())
        );
    }

    #[test]
    fn none_when_nothing_registered() {
        let registered: Vec<String> = Vec::new();
        assert_eq!(resolve_active_provider(Some("local"), &registered), None);
    }
}

#[cfg(test)]
mod startup_provider_tests {
    use super::{Config, resolve_active_provider, startup_provider_preference};

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn last_uses_the_saved_active_provider() {
        assert_eq!(
            startup_provider_preference("last", Some("mpd-home")),
            Some("mpd-home")
        );
        assert_eq!(
            startup_provider_preference("", Some("mpd-home")),
            Some("mpd-home")
        );
        assert_eq!(startup_provider_preference("last", None), None);
    }

    #[test]
    fn explicit_choice_overrides_the_saved_one() {
        assert_eq!(
            startup_provider_preference("local", Some("mpd-home")),
            Some("local")
        );
    }

    #[test]
    fn config_defaults_keep_existing_behaviour() {
        let mut config = Config::default();
        assert_eq!(config.startup_page, "home");
        assert_eq!(config.startup_provider, "last");
        assert_eq!(config.startup_provider_choice(), None);
        config.active_provider = Some("nav".into());
        assert_eq!(config.startup_provider_choice(), Some("nav"));
        config.startup_provider = "local".into();
        assert_eq!(config.startup_provider_choice(), Some("local"));
    }

    #[test]
    fn missing_explicit_provider_falls_back_to_local_then_first() {
        let with_local = ids(&["mpd-home", "local"]);
        let choice = startup_provider_preference("nav-gone", Some("mpd-home"));
        assert_eq!(
            resolve_active_provider(choice, &with_local),
            Some("local".to_string())
        );
        let without_local = ids(&["mpd-home"]);
        assert_eq!(
            resolve_active_provider(choice, &without_local),
            Some("mpd-home".to_string())
        );
    }
}
