// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Music library: scanning, database, metadata, and cover art.

pub mod artist_info;
pub mod artist_tags;
pub mod compilations;
mod cover_art;
mod db;
pub mod history;
mod lyrics;
pub mod m3u;
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
///
/// `cover_key`, `quality` and `total_secs` are derived from the other
/// fields and cached so list/grid views never recompute them per frame;
/// call [`Album::refresh_derived`] after mutating `name`, `artist` or
/// `tracks` of an already-built album.
#[derive(Debug, Clone)]
pub struct Album {
    pub name: String,
    pub artist: String,
    pub year: u32,
    pub tracks: Vec<Track>,
    pub cover_source: Option<CoverSource>,
    /// `CoverArt::album_key(artist, name)`: key into the cover image maps.
    pub cover_key: String,
    /// Best audio quality tier among the tracks.
    pub quality: quality::AudioQuality,
    /// Total duration of all tracks in whole seconds.
    pub total_secs: u64,
}

impl Album {
    /// Build an album, computing the cached derived fields once.
    pub fn new(
        name: String,
        artist: String,
        year: u32,
        tracks: Vec<Track>,
        cover_source: Option<CoverSource>,
    ) -> Self {
        let mut album = Self {
            name,
            artist,
            year,
            tracks,
            cover_source,
            cover_key: String::new(),
            quality: quality::AudioQuality::Unknown,
            total_secs: 0,
        };
        album.refresh_derived();
        album
    }

    /// Recompute `cover_key`, `quality` and `total_secs` from the current
    /// name/artist/tracks.
    pub fn refresh_derived(&mut self) {
        self.cover_key = CoverArt::album_key(&self.artist, &self.name);
        self.quality = quality::album_quality(&self.tracks);
        self.total_secs = self.total_duration().as_secs();
    }

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
        Self::new(name, artist, year, tracks, cover_source)
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
            cover_key: self.cover_key.clone(),
            quality: self.quality,
            total_secs: self.total_secs,
        }
    }

    /// Total duration of all tracks.
    pub fn total_duration(&self) -> Duration {
        self.tracks.iter().map(|t| t.duration).sum()
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Whether this is a compilation ("Various Artists" album artist; see
    /// [`compilations`]).
    pub fn is_compilation(&self) -> bool {
        compilations::is_various_artists(&self.artist)
    }
}

/// An artist aggregated from library tracks.
///
/// `track_total` and `total_secs` are cached sums over `albums`; build
/// with [`Artist::new`] and grow with [`Artist::push_album`] so they stay
/// in sync.
#[derive(Debug, Clone)]
pub struct Artist {
    pub name: String,
    pub albums: Vec<Album>,
    /// Number of tracks across all albums.
    pub track_total: usize,
    /// Total duration of all albums in whole seconds.
    pub total_secs: u64,
}

impl Artist {
    /// Build an artist from its albums, computing the cached sums once.
    pub fn new(name: String, albums: Vec<Album>) -> Self {
        let track_total = albums.iter().map(|a| a.tracks.len()).sum();
        let total_secs = albums.iter().map(|a| a.total_secs).sum();
        Self {
            name,
            albums,
            track_total,
            total_secs,
        }
    }

    /// Append an album, keeping the cached sums current.
    pub fn push_album(&mut self, album: Album) {
        self.track_total += album.tracks.len();
        self.total_secs += album.total_secs;
        self.albums.push(album);
    }

    pub fn album_count(&self) -> usize {
        self.albums.len()
    }

    pub fn track_count(&self) -> usize {
        self.track_total
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

#[cfg(test)]
mod derived_tests {
    use super::*;

    fn track(path: &str, millis: u64, sample_rate: u32) -> Track {
        Track {
            id: 0,
            path: PathBuf::from(path),
            title: String::new(),
            artist: String::new(),
            album_artist: "Band".into(),
            album: "Record".into(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 1999,
            duration: Duration::from_millis(millis),
            bitrate: 900,
            sample_rate,
            provider_id: Arc::from("local"),
            source_uri: path.to_string(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    #[test]
    fn album_caches_key_quality_and_duration() {
        let album = Album::from_tracks(
            "Record".into(),
            vec![track("a.mp3", 1500, 44_100), track("b.flac", 1500, 96_000)],
            None,
        );
        assert_eq!(album.cover_key, CoverArt::album_key("Band", "Record"));
        assert_eq!(album.quality, quality::AudioQuality::HiRes);
        // Summed before truncating, exactly like `total_duration()`.
        assert_eq!(album.total_secs, 3);
        assert_eq!(album.total_secs, album.total_duration().as_secs());
    }

    #[test]
    fn refresh_derived_follows_artist_override_and_new_tracks() {
        let mut album = Album::from_tracks("Record".into(), vec![track("a.mp3", 1000, 0)], None);
        album.artist = "Other".into();
        album.tracks.push(track("b.flac", 2000, 44_100));
        album.refresh_derived();
        assert_eq!(album.cover_key, CoverArt::album_key("Other", "Record"));
        assert_eq!(album.quality, quality::AudioQuality::CdLossless);
        assert_eq!(album.total_secs, 3);
        // The hint keeps the cached fields of the full album.
        assert_eq!(album.cover_hint().cover_key, album.cover_key);
    }

    #[test]
    fn artist_sums_follow_construction_and_push() {
        let a1 = Album::from_tracks("A".into(), vec![track("a.mp3", 1000, 0)], None);
        let a2 = Album::from_tracks(
            "B".into(),
            vec![track("b.mp3", 2000, 0), track("c.mp3", 3000, 0)],
            None,
        );
        let mut artist = Artist::new("Band".into(), vec![a1]);
        assert_eq!((artist.album_count(), artist.track_count()), (1, 1));
        assert_eq!(artist.total_secs, 1);
        artist.push_album(a2);
        assert_eq!((artist.album_count(), artist.track_count()), (2, 3));
        assert_eq!(artist.total_secs, 6);
    }
}
