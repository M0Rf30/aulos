// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use crate::library::album_sort::AlbumSort;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::io::Write;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::path::PathBuf;

pub mod edit_state;
// Settings persist to a file through a writer thread; in the browser they go
// to `localStorage` (see `writer_web.rs`).
#[cfg_attr(target_arch = "wasm32", path = "config/writer_web.rs")]
pub mod writer;

/// Application id the UI registers with the desktop (desktop entry, MPRIS,
/// notifications, portals).
pub const APP_ID: &str = "io.github.m0rf30.Aulos";

/// Serializable configuration for an MPD server connection.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubsonicConfigEntry {
    /// Unique provider ID (e.g., "subsonic-home").
    pub id: String,
    /// Human-readable name (e.g., "Navidrome").
    pub name: String,
    /// Server base URL (e.g., "https://music.example.com").
    pub url: String,
    /// Subsonic username.
    pub username: String,
    /// Plaintext password, only present when the system keyring is
    /// unavailable (the password is stored in the keyring first, and this
    /// stays `None` once `password_in_keyring` is set).
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

impl std::fmt::Debug for SubsonicConfigEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubsonicConfigEntry")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("password_in_keyring", &self.password_in_keyring)
            .field("accept_invalid_certs", &self.accept_invalid_certs)
            .field("transcoding_max_bitrate", &self.transcoding_max_bitrate)
            .field("transcoding_format", &self.transcoding_format)
            .finish()
    }
}

impl std::fmt::Debug for MpdConfigEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MpdConfigEntry")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("password_in_keyring", &self.password_in_keyring)
            .finish()
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

impl ReplayGainMode {
    /// All modes, in the order shown in dropdowns.
    pub const ALL: [ReplayGainMode; 4] = [Self::Off, Self::Track, Self::Album, Self::Auto];
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

/// Colour theme choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Follow the operating system's dark/light setting.
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeMode {
    /// All choices, in the order shown in the settings.
    pub const ALL: [ThemeMode; 3] = [Self::System, Self::Dark, Self::Light];

    /// Whether this choice is dark, given the OS preference (`None`: the
    /// OS did not say; dark is the fallback).
    #[must_use]
    pub fn is_dark(self, system_dark: Option<bool>) -> bool {
        match self {
            Self::System => system_dark.unwrap_or(true),
            Self::Dark => true,
            Self::Light => false,
        }
    }
}

/// Persistent configuration, stored as RON in [`Config::path`].
///
/// Every field has a default, so a file written by an older version (missing
/// the newer fields) or a newer one (carrying unknown fields) still loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
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
    /// Sort order of the albums view.
    pub albums_sort: AlbumSort,
    /// Sort order of the artists view.
    pub artists_sort: AlbumSort,
    /// Sort order of the genres view.
    pub genres_sort: AlbumSort,
    /// Name the Home page greets; empty greets without a name.
    pub user_name: String,
    /// Output directory for local file conversion jobs. `None` uses
    /// `dirs::audio_dir()/Converted` (falling back to `~/Converted`).
    pub convert_out_dir: Option<PathBuf>,
    /// Output format for local file conversion jobs.
    pub convert_format: crate::convert::OutputFormat,
    /// Output sample rate for local file conversion jobs; `None` keeps
    /// each source's original rate.
    pub convert_sample_rate: Option<u32>,
    /// Output channel layout for local file conversion jobs.
    pub convert_channels: crate::convert::ChannelMode,
    /// File name pattern of converted files, e.g. `{track:02} {title}`; see
    /// [`crate::convert::naming`] for the tokens.
    pub convert_name_pattern: String,
    /// Whether a CUE sheet is split into one file per track (otherwise the
    /// audio file it refers to is converted whole).
    pub convert_split_cue: bool,
    /// Whether converted files get the source's tags and cover art.
    pub convert_copy_tags: bool,
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
    /// Whether Aulos fetches artist biographies from Wikipedia (keyless)
    /// when browsing in Local or MPD mode. Off by default: purely opt-in
    /// network access. Ignored in Subsonic mode, which always shows the
    /// server's own artist info instead — see
    /// `crate::provider::subsonic::SubsonicProvider::get_artist_info`.
    pub fetch_artist_bio: bool,
    /// Whether Aulos fetches artist photos from Deezer (keyless) in Local
    /// or MPD mode. Off by default, and ignored in Subsonic mode like
    /// [`Self::fetch_artist_bio`].
    pub fetch_artist_photos: bool,
    /// Whether radio station icons are downloaded from the stations'
    /// servers. On by default.
    pub fetch_radio_icons: bool,
    /// Pre-load the next track so it follows without a gap. On by default.
    pub gapless: bool,
    /// Stop after every track instead of continuing with the queue.
    pub stop_after_current: bool,
    /// Start playing the library when the app launches.
    pub auto_play_on_launch: bool,
    /// Rescan the library when files in the music folders change.
    pub watch_for_changes: bool,
    /// Dark, light, or follow the operating system.
    pub theme_mode: ThemeMode,
    /// Tint the interface accent with the playing cover's colour.
    pub tint_ui_with_cover: bool,
    /// Turn off interface animations.
    pub reduce_motion: bool,
    /// Size multiplier for cards in grid views (albums, artists, genres,
    /// folders, radio, podcasts…), 0.7–1.6; 1.0 is the default size.
    pub grid_scale: f32,
    /// Interface zoom, [`Self::UI_SCALE_MIN`]–[`Self::UI_SCALE_MAX`]; 1.0
    /// follows the display's own scale factor unchanged. Loading clamps it.
    pub ui_scale: f32,
    /// Interface language as a BCP 47 tag (`"it"`, `"pt-BR"`); empty follows
    /// the system language.
    pub language: String,
    /// Visualizer: keep the current preset instead of rotating.
    pub visualizer_locked: bool,
    /// Visualizer beat sensitivity, 0.0–2.0 (1.0 is neutral). Loading clamps it.
    pub visualizer_sensitivity: f32,
    /// The preset on screen when the visualizer was last used.
    pub visualizer_preset: Option<PathBuf>,
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
    /// AcoustID *application* API key (not a secret) used for fingerprint
    /// lookups and submissions. The user's own key lives in the keyring.
    pub acoustid_api_key: String,
    /// Prefer the Cover Art Archive front cover when applying a release in
    /// the tag editor.
    pub tagger_use_caa_cover: bool,
}

impl Default for Config {
    fn default() -> Self {
        // Default music directory is ~/Music (the shared Music folder on Android); a page has no home directory,
        // its folders are picked by the user.
        #[cfg(target_os = "android")]
        let music_dirs = vec![PathBuf::from("/storage/emulated/0/Music")];
        #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
        let music_dirs = vec![dirs::audio_dir().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join("Music")
        })];
        #[cfg(target_arch = "wasm32")]
        let music_dirs = Vec::new();

        Self {
            music_dirs,
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
            albums_sort: AlbumSort::default(),
            artists_sort: AlbumSort::default(),
            genres_sort: AlbumSort::default(),
            user_name: String::new(),
            convert_out_dir: None,
            convert_format: crate::convert::OutputFormat::Flac,
            convert_sample_rate: None,
            convert_channels: crate::convert::ChannelMode::Source,
            convert_name_pattern: crate::convert::naming::DEFAULT_PATTERN.to_owned(),
            convert_split_cue: true,
            convert_copy_tags: true,
            experimental_converter: false,
            flac_options: crate::convert::encoder::FlacOptions::default(),
            lossy_options: crate::convert::encoder::LossyOptions::default(),
            active_provider: None,
            fetch_artist_bio: false,
            fetch_artist_photos: false,
            fetch_radio_icons: true,
            gapless: true,
            stop_after_current: false,
            auto_play_on_launch: false,
            watch_for_changes: true,
            theme_mode: ThemeMode::System,
            tint_ui_with_cover: true,
            reduce_motion: false,
            grid_scale: 1.0,
            ui_scale: 1.0,
            language: String::new(),
            visualizer_locked: false,
            visualizer_sensitivity: 1.0,
            visualizer_preset: None,
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
            acoustid_api_key: String::new(),
            tagger_use_caa_cover: true,
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

/// File name of the settings inside [`crate::paths::config_dir`].
#[cfg(not(target_arch = "wasm32"))]
const SETTINGS_FILE: &str = "settings.ron";

impl Config {
    /// Where the settings live: `settings.ron` in [`crate::paths::config_dir`].
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn path() -> PathBuf {
        crate::paths::config_dir().join(SETTINGS_FILE)
    }

    /// Loads the settings from [`Config::path`]; see [`Config::load_from`].
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn load() -> Config {
        Self::load_from(&Self::path())
    }

    /// Loads the settings kept in the browser's `localStorage`: defaults on
    /// the first visit, or when the stored text is not valid settings (which
    /// is first copied to `aulos.settings.ron.bad`, so the next save does not
    /// destroy it).
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn load() -> Config {
        let Some(text) = crate::web_storage::get(crate::web_storage::SETTINGS_KEY) else {
            return Config::default();
        };
        Self::parse(&text).unwrap_or_else(|e| {
            match crate::web_storage::set(crate::web_storage::SETTINGS_BAD_KEY, &text) {
                Ok(()) => tracing::error!(
                    "Invalid stored settings ({e}); keeping them as {} and using defaults",
                    crate::web_storage::SETTINGS_BAD_KEY
                ),
                Err(why) => tracing::error!(
                    "Invalid stored settings ({e}); could not keep a copy ({why}); using defaults"
                ),
            }
            Config::default()
        })
    }

    /// The settings in `text` (RON), with out-of-range values clamped.
    ///
    /// # Errors
    /// When `text` is not valid RON for [`Config`].
    pub fn parse(text: &str) -> Result<Config, ron::error::SpannedError> {
        let mut config = ron::from_str::<Config>(text)?;
        config.ui_scale = Self::clamp_ui_scale(config.ui_scale);
        config.visualizer_sensitivity =
            Self::clamp_visualizer_sensitivity(config.visualizer_sensitivity);
        Ok(config)
    }

    /// The settings as RON text, the stored form.
    ///
    /// # Errors
    /// When serialisation fails.
    pub fn to_text(&self) -> Result<String, ron::Error> {
        ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default())
    }

    /// Loads the settings stored at `path`. Defaults when the file does not
    /// exist (first launch) or cannot be read; a file that is not valid RON
    /// is moved aside to `<path>.bad` (so the next save does not destroy it)
    /// and the defaults are used.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn load_from(path: &Path) -> Config {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    Self::set_aside(path, &format!("unreadable: {e}"));
                }
                return Config::default();
            }
        };
        match Self::parse(&text) {
            Ok(config) => config,
            Err(e) => {
                Self::set_aside(path, &format!("invalid: {e}"));
                Config::default()
            }
        }
    }

    /// Moves a settings file that cannot be used to `<path>.bad`, so the
    /// defaults that replace it are never written over the user's data.
    #[cfg(not(target_arch = "wasm32"))]
    fn set_aside(path: &Path, why: &str) {
        let mut bad = path.as_os_str().to_owned();
        bad.push(".bad");
        tracing::error!(
            "Settings {} {why}; keeping it as {} and using defaults",
            path.display(),
            Path::new(&bad).display()
        );
        if let Err(e) = std::fs::rename(path, &bad) {
            tracing::warn!("Could not move the unusable settings aside: {e}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600));
        }
    }

    /// Smallest interface zoom.
    pub const UI_SCALE_MIN: f32 = 0.75;
    /// Largest interface zoom.
    pub const UI_SCALE_MAX: f32 = 2.0;

    /// `scale` limited to the supported zoom range (1.0 for NaN).
    #[must_use]
    pub fn clamp_ui_scale(scale: f32) -> f32 {
        if scale.is_finite() {
            scale.clamp(Self::UI_SCALE_MIN, Self::UI_SCALE_MAX)
        } else {
            1.0
        }
    }

    /// `sensitivity` limited to the visualizer's 0.0–2.0 range (1.0 for NaN).
    #[must_use]
    pub fn clamp_visualizer_sensitivity(sensitivity: f32) -> f32 {
        if sensitivity.is_finite() {
            sensitivity.clamp(0.0, 2.0)
        } else {
            1.0
        }
    }

    /// Writes the settings to `path` atomically: serialised to a temporary
    /// file in the same directory, flushed to disk, then renamed over `path`,
    /// so a crash never leaves a truncated file. Creates the directory.
    ///
    /// # Errors
    /// If the directory or the file cannot be written.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let text = self.to_text().map_err(std::io::Error::other)?;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
        tmp_name.push(".tmp");
        let tmp = dir.join(tmp_name);
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                // The file may hold plaintext passwords (keyring fallback).
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // `mode` only applies on creation; a leftover tmp may be looser.
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    /// See [`startup_provider_preference`].
    pub fn startup_provider_choice(&self) -> Option<&str> {
        startup_provider_preference(&self.startup_provider, self.active_provider.as_deref())
    }

    /// The AcoustID application API key, trimmed, when one is set.
    pub fn acoustid_key(&self) -> Option<&str> {
        let key = self.acoustid_api_key.trim();
        (!key.is_empty()).then_some(key)
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
    fn tagger_defaults_and_acoustid_key_trimming() {
        let mut config = Config::default();
        assert!(config.tagger_use_caa_cover);
        assert_eq!(config.acoustid_key(), None);
        config.acoustid_api_key = "   ".into();
        assert_eq!(config.acoustid_key(), None);
        config.acoustid_api_key = "  abc123 \n".into();
        assert_eq!(config.acoustid_key(), Some("abc123"));
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

#[cfg(test)]
mod persistence_tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aulos-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = scratch("roundtrip");
        let path = dir.join("nested").join(SETTINGS_FILE);
        let config = Config {
            last_view: "genres".into(),
            grid_scale: 1.25,
            volume: 0.35,
            albums_view_mode: ViewMode::List,
            repeat_mode: RepeatMode::One,
            music_dirs: vec![PathBuf::from("/music"), PathBuf::from("/more music")],
            active_provider: Some("mpd-home".into()),
            equalizer_bands: vec![1.5, -2.0, 0.0],
            mpd_servers: vec![MpdConfigEntry::default()],
            theme_mode: ThemeMode::Light,
            gapless: false,
            stop_after_current: true,
            auto_play_on_launch: true,
            watch_for_changes: false,
            tint_ui_with_cover: false,
            reduce_motion: true,
            fetch_artist_bio: true,
            fetch_artist_photos: true,
            fetch_radio_icons: false,
            ..Config::default()
        };
        config.save_to(&path).unwrap();

        assert_eq!(Config::load_from(&path), config);
        // No temporary file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, [std::ffi::OsString::from(SETTINGS_FILE)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn theme_mode_resolves_against_the_system_preference() {
        assert!(ThemeMode::System.is_dark(Some(true)));
        assert!(!ThemeMode::System.is_dark(Some(false)));
        assert!(ThemeMode::System.is_dark(None));
        assert!(ThemeMode::Dark.is_dark(Some(false)));
        assert!(!ThemeMode::Light.is_dark(Some(true)));
    }

    #[test]
    fn a_missing_file_loads_the_defaults() {
        let dir = scratch("missing");
        assert_eq!(
            Config::load_from(&dir.join(SETTINGS_FILE)),
            Config::default()
        );
    }

    #[test]
    fn visualizer_settings_default_round_trip_and_clamp() {
        let dir = scratch("visualizer");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE);
        let defaults = Config::default();
        assert!(!defaults.visualizer_locked);
        assert_eq!(defaults.visualizer_sensitivity, 1.0);
        assert_eq!(defaults.visualizer_preset, None);

        let config = Config {
            visualizer_locked: true,
            visualizer_sensitivity: 1.4,
            visualizer_preset: Some(PathBuf::from("/presets/a.milk2")),
            ..Config::default()
        };
        config.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path), config);

        std::fs::write(&path, "(visualizer_sensitivity: 9.0)").unwrap();
        assert_eq!(Config::load_from(&path).visualizer_sensitivity, 2.0);
        std::fs::write(&path, "(visualizer_sensitivity: -1.0)").unwrap();
        assert_eq!(Config::load_from(&path).visualizer_sensitivity, 0.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ui_scale_defaults_to_one_and_loads_clamped() {
        let dir = scratch("ui-scale");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE);
        assert_eq!(Config::default().ui_scale, 1.0);
        std::fs::write(&path, "(ui_scale: 5.0)").unwrap();
        assert_eq!(Config::load_from(&path).ui_scale, Config::UI_SCALE_MAX);
        std::fs::write(&path, "(ui_scale: 0.1)").unwrap();
        assert_eq!(Config::load_from(&path).ui_scale, Config::UI_SCALE_MIN);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let dir = scratch("partial");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE);
        std::fs::write(&path, "(volume: 0.25, shuffle: true)").unwrap();
        let loaded = Config::load_from(&path);
        let defaults = Config::default();
        assert_eq!(loaded.volume, 0.25);
        assert!(loaded.shuffle);
        assert_eq!(loaded.startup_page, defaults.startup_page);
        assert_eq!(loaded.grid_scale, defaults.grid_scale);
        assert_eq!(loaded.music_dirs, defaults.music_dirs);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = scratch("unknown");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE);
        std::fs::write(
            &path,
            "(volume: 0.5, from_the_future: [1, 2], other: \"x\")",
        )
        .unwrap();
        let loaded = Config::load_from(&path);
        assert_eq!(loaded.volume, 0.5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_invalid_file_is_kept_aside_and_defaults_are_used() {
        let dir = scratch("invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE);
        std::fs::write(&path, "(volume: not-a-number").unwrap();
        assert_eq!(Config::load_from(&path), Config::default());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.ron.bad")).unwrap(),
            "(volume: not-a-number"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_non_utf8_file_is_kept_aside_and_defaults_are_used() {
        let dir = scratch("non-utf8");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE);
        std::fs::write(&path, [0xff, 0xfe, 0x00, 0x28]).unwrap();
        assert_eq!(Config::load_from(&path), Config::default());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(dir.join("settings.ron.bad")).unwrap(),
            [0xff, 0xfe, 0x00, 0x28]
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
