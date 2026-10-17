// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Music library: scanning, database, metadata, and cover art.

pub mod album_filter;
pub mod album_sort;
pub mod artist_info;
pub mod artist_tags;
pub mod compilations;
mod cover_art;
pub mod covers;
mod db;
pub mod display;
pub mod fingerprint;
pub mod folder_tree;
pub mod genre;
pub mod history;
pub mod home;
pub mod home_state;
mod lyrics;
pub mod m3u;
pub mod palette;
pub mod playlist_io;
pub mod properties;
pub mod quality;
pub mod queue_source;
pub mod release_kind;
pub mod scan_progress;
mod scanner;
pub mod search;
pub mod smart_playlist;
pub mod sparse;
pub mod state;
pub mod tagio;
pub mod tags;
pub mod tech_info;
pub mod top_tracks;
pub mod watcher;
#[cfg(target_arch = "wasm32")]
pub mod web_covers;
#[cfg(target_arch = "wasm32")]
pub mod web_scanner;

pub use cover_art::{CoverArt, pick_cover_file};
pub use db::LibraryDb;
pub use lyrics::{LyricsProvider, find_current_line_index, parse_lrc};
pub use scanner::LibraryScanner;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Whether the configured music folder `dir` exists. In the browser a folder
/// is a virtual root (see [`crate::virtual_path`]) that exists by definition;
/// whether it can be read now is [`crate::web_fs::status`].
#[must_use]
pub fn music_dir_exists(dir: &std::path::Path) -> bool {
    cfg!(target_arch = "wasm32") || dir.is_dir()
}

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
    /// Bits per sample of a lossless file (16, 24, …); `0` when lossy,
    /// streamed or unknown.
    pub bit_depth: u32,
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
    /// Raw release-type tag (MusicBrainz album type, e.g. `"album; live"`);
    /// empty when untagged. See [`release_kind::ReleaseKind`].
    pub release_type: String,
    /// Unix seconds the track entered the library; `0` when unknown
    /// (tracks that are not stored in the library database).
    pub added_at: i64,
    /// Unix seconds the track was last marked as a favorite; `0` when it
    /// is not one, or was one before favourites were timestamped.
    pub favorited_at: i64,
}

impl Track {
    /// The identifier the active provider addresses this track by in
    /// favorite/rating calls: the database row id for local tracks, and
    /// the provider's own id (Subsonic song id, MPD path; stored in
    /// `path`) for remote ones, whose `id` is `0` while live.
    #[must_use]
    pub fn key(&self) -> String {
        if &*self.provider_id == "local" {
            self.id.to_string()
        } else {
            self.path.to_string_lossy().into_owned()
        }
    }

    /// Whether [`Self::key`] equals `key`, without allocating.
    #[must_use]
    pub fn has_key(&self, key: &str) -> bool {
        if &*self.provider_id == "local" {
            key.parse::<i64>() == Ok(self.id)
        } else {
            self.path.to_str() == Some(key)
        }
    }

    /// Format duration as `H:MM:SS` when at least an hour, otherwise `M:SS`.
    pub fn duration_string(&self) -> String {
        crate::format::format_duration(self.duration.as_secs())
    }

    /// `24/96` (or `44.1 kHz` without a bit depth); `None` when the sample
    /// rate is unknown.
    #[must_use]
    pub fn depth_rate_label(&self) -> Option<String> {
        crate::format::format_depth_rate(self.bit_depth, self.sample_rate)
    }

    /// The expanded player's format chip: `FLAC · 24/96 · 2,304 kbps`, each
    /// part left out when unknown.
    #[must_use]
    pub fn format_chip(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ext) = self
            .path
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| !e.is_empty() && e.len() <= 5)
        {
            parts.push(ext.to_uppercase());
        }
        parts.extend(self.depth_rate_label());
        if self.bitrate > 0 {
            parts.push(format!(
                "{} kbps",
                crate::format::format_thousands(self.bitrate)
            ));
        }
        parts.join(" \u{00B7} ")
    }

    /// The artist albums are grouped under: the album artist when present,
    /// else the track artist.
    #[must_use]
    pub fn grouping_artist(&self) -> &str {
        if self.album_artist.is_empty() {
            &self.artist
        } else {
            &self.album_artist
        }
    }

    /// Cover-art cache key of this track's album, matching how albums key
    /// their art ([`CoverArt::album_key`] over [`Track::grouping_artist`]).
    #[must_use]
    pub fn cover_key(&self) -> String {
        CoverArt::album_key(self.grouping_artist(), &self.album)
    }

    /// Whether this track should be presented as a live stream rather than
    /// a normal, seekable recording: radio (`provider_id == "radio"`) or
    /// anything else reporting zero duration. UIs hide/disable the same
    /// controls (seek slider, shuffle, prev/next, repeat) for the same
    /// tracks.
    #[must_use]
    pub fn is_live_stream(&self) -> bool {
        &*self.provider_id == "radio" || self.duration.is_zero()
    }

    /// Sort tracks by disc number, then track number.
    pub fn sort_by_disc_and_track(tracks: &mut [Track]) {
        tracks.sort_by(Self::disc_track_order);
    }

    /// Disc number, then track number.
    pub fn disc_track_order(a: &Track, b: &Track) -> std::cmp::Ordering {
        a.disc_number
            .cmp(&b.disc_number)
            .then(a.track_number.cmp(&b.track_number))
    }
}

/// Owned copies of shared library tracks, for queues and players that take
/// `Track` by value.
#[must_use]
pub fn owned_tracks(tracks: &[Arc<Track>]) -> Vec<Track> {
    tracks.iter().map(|t| Track::clone(t)).collect()
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
    pub tracks: Vec<Arc<Track>>,
    pub cover_source: Option<CoverSource>,
    /// `CoverArt::album_key(artist, name)`: key into the cover image maps.
    pub cover_key: String,
    /// Best audio quality tier among the tracks.
    pub quality: quality::AudioQuality,
    /// Total duration of all tracks in whole seconds.
    pub total_secs: u64,
    /// Album, EP, single, live album or compilation.
    pub kind: release_kind::ReleaseKind,
    /// Unix seconds the album's newest track entered the library; `0` when
    /// unknown (providers other than the local one).
    pub added_at: i64,
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
            tracks: tracks.into_iter().map(Arc::new).collect(),
            cover_source,
            cover_key: String::new(),
            quality: quality::AudioQuality::Unknown,
            total_secs: 0,
            kind: release_kind::ReleaseKind::Album,
            added_at: 0,
        };
        album.refresh_derived();
        album
    }

    /// Recompute `cover_key`, `quality`, `total_secs`, `kind` and
    /// `added_at` from the current name/artist/tracks.
    pub fn refresh_derived(&mut self) {
        self.kind = release_kind::ReleaseKind::of_album(&self.name, &self.artist, &self.tracks);
        self.added_at = self.tracks.iter().map(|t| t.added_at).max().unwrap_or(0);
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
            kind: self.kind,
            added_at: self.added_at,
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
    pub albums: Vec<Arc<Album>>,
    /// Number of tracks across all albums.
    pub track_total: usize,
    /// Total duration of all albums in whole seconds.
    pub total_secs: u64,
}

impl Artist {
    /// Build an artist from its albums, computing the cached sums once.
    pub fn new(name: String, albums: Vec<Arc<Album>>) -> Self {
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
    pub fn push_album(&mut self, album: Arc<Album>) {
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

impl Playlist {
    /// Recomputes `track_count` and `total_duration` from `tracks`.
    pub fn refresh_totals(&mut self) {
        self.track_count = self.tracks.len() as u32;
        self.total_duration = self.tracks.iter().map(|t| t.duration).sum();
    }
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
            bit_depth: 0,
            release_type: String::new(),
            added_at: 0,
            favorited_at: 0,
        }
    }

    #[test]
    fn format_chip_shows_depth_rate_and_grouped_bitrate() {
        let mut t = track("a.flac", 1000, 96_000);
        t.bitrate = 2_304;
        t.bit_depth = 24;
        assert_eq!(t.format_chip(), "FLAC \u{00B7} 24/96 \u{00B7} 2,304 kbps");
        t.bit_depth = 0;
        assert_eq!(t.format_chip(), "FLAC \u{00B7} 96 kHz \u{00B7} 2,304 kbps");
    }

    #[test]
    fn radio_provider_is_live_even_with_a_nonzero_duration() {
        let mut t = track("a.mp3", 180_000, 0);
        t.provider_id = Arc::from("radio");
        assert!(t.is_live_stream());
    }

    #[test]
    fn zero_duration_track_is_live_regardless_of_provider() {
        assert!(track("a.mp3", 0, 0).is_live_stream());
    }

    #[test]
    fn normal_local_track_is_not_live() {
        assert!(!track("a.mp3", 180_000, 0).is_live_stream());
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
        album.tracks.push(Arc::new(track("b.flac", 2000, 44_100)));
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
        let mut artist = Artist::new("Band".into(), vec![Arc::new(a1)]);
        assert_eq!((artist.album_count(), artist.track_count()), (1, 1));
        assert_eq!(artist.total_secs, 1);
        artist.push_album(Arc::new(a2));
        assert_eq!((artist.album_count(), artist.track_count()), (2, 3));
        assert_eq!(artist.total_secs, 6);
    }
}

#[cfg(test)]
mod track_key_tests {
    use crate::session::test_support::track;
    use std::path::PathBuf;

    #[test]
    fn local_tracks_are_addressed_by_row_id() {
        let mut t = track(42);
        t.provider_id = "local".into();
        assert_eq!(t.key(), "42");
        assert!(t.has_key("42"));
        assert!(!t.has_key("0"));
    }

    #[test]
    fn remote_tracks_are_addressed_by_provider_id_not_the_zero_row_id() {
        let mut a = track(0);
        a.provider_id = "navidrome".into();
        a.path = PathBuf::from("song-a");
        let mut b = a.clone();
        b.path = PathBuf::from("song-b");
        assert_eq!(a.key(), "song-a");
        assert!(a.has_key("song-a"));
        assert!(!a.has_key("0"));
        assert_ne!(a.key(), b.key());
    }
}
