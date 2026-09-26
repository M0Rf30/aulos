// SPDX-License-Identifier: GPL-3.0

//! Music library: scanning, database, metadata, and cover art.

pub mod artist_info;
pub mod artist_tags;
mod cover_art;
mod db;
mod lyrics;
pub mod palette;
pub mod quality;
mod scanner;
pub mod smart_playlist;
pub mod tags;

pub use cover_art::CoverArt;
pub use db::LibraryDb;
pub use lyrics::{LyricsProvider, parse_lrc};
pub use scanner::LibraryScanner;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Resolved audio source for the playback backend.
#[derive(Debug, Clone)]
pub enum TrackSource {
    /// A local file on the filesystem.
    LocalFile(PathBuf),
    /// An HTTP streaming URL (e.g., Subsonic `stream` endpoint).
    HttpStream(String),
    /// An internet radio / Shoutcast/Icecast live stream URL. Unlike
    /// `HttpStream`, the byte length is never known up front and seeking is
    /// never supported — see `player::engine::decoder::SymphoniaDecoder::open_stream`.
    LiveStream(String),
    /// An MPD-relative file path — sent to the MPD server, not decoded locally.
    MpdFile(String),
}

/// Resolved cover art source.
#[derive(Debug, Clone)]
pub enum CoverSource {
    /// A local file path (embedded extraction cache or directory image).
    LocalFile(PathBuf),
    /// An HTTP URL (e.g., Subsonic `getCoverArt` endpoint).
    Url(String),
    /// An MPD file path, resolved via `albumart`/`readpicture` protocol commands.
    MpdAlbumArt(String),
}

/// A single track in the music library.
#[derive(Debug, Clone)]
pub struct Track {
    pub id: i64,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub genre: String,
    pub track_number: u32,
    pub disc_number: u32,
    pub year: u32,
    pub duration: Duration,
    pub bitrate: u32,
    pub sample_rate: u32,
    /// Which provider owns this track (e.g., "local", "mpd-home", "navidrome").
    ///
    /// Stored as `Arc<str>` so all tracks from the same provider share one
    /// allocation instead of cloning a `String` per track.
    pub provider_id: Arc<str>,
    /// Provider-specific identifier (file path for local, MPD relative path, Subsonic song ID).
    pub source_uri: String,
    /// Whether this track is marked as a favorite.
    pub is_favorite: bool,
    /// User rating (1-5), or None if unrated.
    pub rating: Option<u8>,
    /// ReplayGain track gain in dB (e.g., -6.5).
    pub rg_track_gain: Option<f32>,
    /// ReplayGain album gain in dB.
    pub rg_album_gain: Option<f32>,
}

impl Track {
    /// Format duration as `H:MM:SS` when at least an hour, otherwise `M:SS`.
    pub fn duration_string(&self) -> String {
        let secs = self.duration.as_secs();
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        let seconds = secs % 60;
        if hours > 0 {
            format!("{hours}:{minutes:02}:{seconds:02}")
        } else {
            format!("{minutes}:{seconds:02}")
        }
    }

    /// Sort tracks by disc number, then track number.
    pub fn sort_by_disc_and_track(tracks: &mut [Track]) {
        tracks.sort_by(|a, b| {
            a.disc_number
                .cmp(&b.disc_number)
                .then(a.track_number.cmp(&b.track_number))
        });
    }
}

/// An album aggregated from library tracks.
#[derive(Debug, Clone)]
pub struct Album {
    pub name: String,
    pub artist: String,
    pub year: u32,
    pub tracks: Vec<Track>,
    pub cover_source: Option<CoverSource>,
}

impl Album {
    /// Construct an album from a name, sorted track list, and optional cover source.
    ///
    /// Extracts artist and year from the first track.
    pub fn from_tracks(
        name: String,
        tracks: Vec<Track>,
        cover_source: Option<CoverSource>,
    ) -> Self {
        let artist = tracks
            .first()
            .map(|t| t.album_artist.clone())
            .unwrap_or_default();
        let year = tracks.first().map(|t| t.year).unwrap_or(0);
        Self {
            name,
            artist,
            year,
            tracks,
            cover_source,
        }
    }

    /// Create a lightweight clone for cover art fetching.
    ///
    /// Contains `cover_source` and at most the first track — avoids
    /// cloning the entire track list when only cover art metadata is needed.
    pub fn cover_hint(&self) -> Self {
        Self {
            name: self.name.clone(),
            artist: self.artist.clone(),
            year: self.year,
            tracks: self.tracks.first().cloned().into_iter().collect(),
            cover_source: self.cover_source.clone(),
        }
    }

    /// Total duration of all tracks.
    pub fn total_duration(&self) -> Duration {
        self.tracks.iter().map(|t| t.duration).sum()
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }
}

/// An artist aggregated from library tracks.
#[derive(Debug, Clone)]
pub struct Artist {
    pub name: String,
    pub albums: Vec<Album>,
}

impl Artist {
    pub fn album_count(&self) -> usize {
        self.albums.len()
    }

    pub fn track_count(&self) -> usize {
        self.albums.iter().map(|a| a.tracks.len()).sum()
    }
}

/// Provider-native artist metadata (currently only meaningfully produced
/// by `SubsonicProvider::get_artist_info` — Local/MPD use
/// `crate::library::artist_info`'s own Deezer/Wikipedia lookup instead).
/// `image_bytes` is the raw encoded (JPEG/PNG) download, decoded and
/// cached by the caller, matching how album cover art bytes flow through
/// `MusicProvider::get_cover_art`.
#[derive(Debug, Clone)]
pub struct ArtistInfoResult {
    pub bio: Option<String>,
    pub image_bytes: Option<Vec<u8>>,
}

/// A user-created playlist.
#[derive(Debug, Clone)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub tracks: Vec<Track>,
    pub track_count: u32,
    pub total_duration: Duration,
}

/// A single line in synced lyrics.
#[derive(Debug, Clone)]
pub struct LyricLine {
    /// Timestamp in milliseconds from track start.
    pub timestamp_ms: u64,
    /// The lyric text for this line.
    pub text: String,
}

/// Lyrics data, either time-synchronized or plain text.
#[derive(Debug, Clone)]
pub enum Lyrics {
    /// Time-synchronized lyrics with per-line timestamps.
    Synced(Vec<LyricLine>),
    /// Plain text lyrics without timing information.
    Unsynced(String),
}
