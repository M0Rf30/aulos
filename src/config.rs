// SPDX-License-Identifier: GPL-3.0

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
        }
    }
}
