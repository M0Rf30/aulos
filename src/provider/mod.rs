// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Music provider abstraction layer.
//!
//! Defines the [`MusicProvider`] trait that all music sources implement,
//! and the [`ProviderRegistry`] that manages active providers.

pub mod local;
pub mod mpd;
pub mod subsonic;

use crate::library::{Album, Artist, Lyrics, Track};
use std::collections::HashMap;
use std::fmt;

/// The type of a music provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderType {
    /// Local filesystem (scanned directories, cpal-based playback).
    Local,
    /// MPD server (remote library, server-side playback).
    Mpd,
    /// OpenSubsonic-compatible server (HTTP API, client-side streaming).
    Subsonic,
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "Local"),
            Self::Mpd => write!(f, "MPD"),
            Self::Subsonic => write!(f, "Subsonic"),
        }
    }
}

/// Errors from provider operations.
#[derive(Debug)]
pub enum ProviderError {
    /// The provider is not connected or not available.
    NotConnected(String),
    /// A network or I/O error occurred.
    Io(String),
    /// The operation is not supported by this provider.
    NotSupported(String),
    /// A database error occurred.
    Database(String),
    /// Any other error.
    Other(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConnected(msg) => write!(f, "not connected: {msg}"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
            Self::NotSupported(msg) => write!(f, "not supported: {msg}"),
            Self::Database(msg) => write!(f, "database error: {msg}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ProviderError {}

impl From<String> for ProviderError {
    fn from(s: String) -> Self {
        Self::Other(s)
    }
}

/// Wrap a network/protocol error into `ProviderError::Io`, labeled with the
/// backend name and the failing operation. Shared by `mpd.rs` and
/// `subsonic.rs`, whose per-backend `*_err` helpers differed only in prefix.
pub(crate) fn wrap_err<E: fmt::Display>(
    backend: &str,
    op: &str,
) -> impl FnOnce(E) -> ProviderError {
    let backend = backend.to_string();
    let op = op.to_string();
    move |e| ProviderError::Io(format!("{backend} {op}: {e}"))
}

/// Common interface for all music providers.
///
/// Each provider manages a library of tracks, albums, and artists, and can
/// resolve audio sources for playback. Implementations must be `Send + Sync`
/// to work with async runtimes and the COSMIC task system.
pub trait MusicProvider: Send + Sync {
    /// Unique identifier for this provider instance (e.g., "local", "mpd-home").
    fn id(&self) -> &str;

    /// Human-readable name (e.g., "Local Music", "Home MPD Server").
    fn name(&self) -> &str;

    /// The type of this provider.
    fn provider_type(&self) -> ProviderType;

    /// Browse all albums from this provider's library.
    fn browse_albums(&self) -> Result<Vec<Album>, ProviderError>;

    /// Browse all artists from this provider's library.
    fn browse_artists(&self) -> Result<Vec<Artist>, ProviderError>;

    /// Browse all tracks from this provider's library.
    fn browse_tracks(&self) -> Result<Vec<Track>, ProviderError>;

    /// Search tracks matching the given query string.
    fn search(&self, query: &str) -> Result<Vec<Track>, ProviderError>;

    /// Get cover art bytes for an album, if available.
    fn get_cover_art(&self, album: &Album) -> Result<Option<Vec<u8>>, ProviderError>;

    /// Get lyrics for a track, if available.
    fn get_lyrics(&self, track: &Track) -> Result<Option<Lyrics>, ProviderError>;

    /// Synchronize / refresh the provider's library.
    /// Returns the number of tracks added or updated.
    fn sync_library(&self) -> Result<usize, ProviderError>;

    // --- Playlist methods (optional, default: NotSupported) ---

    /// List all playlists from this provider.
    fn list_playlists(&self) -> Result<Vec<crate::library::Playlist>, ProviderError> {
        Err(ProviderError::NotSupported("playlists".into()))
    }

    /// Get a playlist with its tracks.
    fn get_playlist(&self, id: &str) -> Result<crate::library::Playlist, ProviderError> {
        let _ = id;
        Err(ProviderError::NotSupported("playlists".into()))
    }

    /// Create a new playlist with the given name.
    fn create_playlist(&self, name: &str) -> Result<crate::library::Playlist, ProviderError> {
        let _ = name;
        Err(ProviderError::NotSupported("playlists".into()))
    }

    /// Delete a playlist by ID.
    fn delete_playlist(&self, id: &str) -> Result<(), ProviderError> {
        let _ = id;
        Err(ProviderError::NotSupported("playlists".into()))
    }

    /// Rename a playlist.
    fn rename_playlist(&self, id: &str, new_name: &str) -> Result<(), ProviderError> {
        let _ = (id, new_name);
        Err(ProviderError::NotSupported("playlists".into()))
    }

    /// Add tracks to a playlist.
    fn add_to_playlist(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> Result<(), ProviderError> {
        let _ = (playlist_id, track_ids);
        Err(ProviderError::NotSupported("playlists".into()))
    }

    // --- Favorites and ratings methods (optional, default: not supported) ---

    /// Toggle favorite status for a track (by source_uri or provider-specific ID).
    fn toggle_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let _ = track_id;
        Err(ProviderError::NotSupported("favorites".into()))
    }

    /// Check if a track is a favorite.
    fn is_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let _ = track_id;
        Ok(false)
    }

    /// Set a rating (1-5) for a track. Pass 0 to clear.
    fn set_rating(&self, track_id: &str, rating: u8) -> Result<(), ProviderError> {
        let _ = (track_id, rating);
        Err(ProviderError::NotSupported("ratings".into()))
    }

    /// Get the rating for a track.
    fn get_rating(&self, track_id: &str) -> Result<Option<u8>, ProviderError> {
        let _ = track_id;
        Ok(None)
    }

    /// List all favorite tracks.
    fn list_favorites(&self) -> Result<Vec<Track>, ProviderError> {
        Err(ProviderError::NotSupported("favorites".into()))
    }

    // --- Genre methods (optional, default: empty) ---

    /// List all distinct genres from this provider.
    fn list_genres(&self) -> Result<Vec<String>, ProviderError> {
        Ok(Vec::new())
    }

    /// Get all tracks matching the given genre.
    fn get_tracks_by_genre(&self, genre: &str) -> Result<Vec<Track>, ProviderError> {
        let _ = genre;
        Ok(Vec::new())
    }

    // --- Artist metadata (optional, default: not supported) ---

    /// Fetch provider-native artist metadata (bio + image), if this
    /// provider serves its own (currently only `SubsonicProvider`, from
    /// the server's `getArtistInfo2`/cover art). The default
    /// `NotSupported` tells `crate::library::artist_info` callers to fall
    /// back to Lyra's own Deezer/Wikipedia lookup instead, gated by
    /// `Config::fetch_artist_info`.
    fn get_artist_info(
        &self,
        name: &str,
    ) -> Result<Option<crate::library::ArtistInfoResult>, ProviderError> {
        let _ = name;
        Err(ProviderError::NotSupported("artist_info".into()))
    }
}

/// Manages all registered music providers.
///
/// Wraps providers in `Arc` so they can be shared with async tasks for
/// background library loading, scanning, and cover art fetching.
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn MusicProvider>>,
    active_provider_id: String,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            active_provider_id: String::new(),
        }
    }

    /// Register a provider. If this is the first provider, it becomes active.
    pub fn register(&mut self, provider: Arc<dyn MusicProvider>) {
        let id = provider.id().to_string();
        if self.providers.is_empty() {
            self.active_provider_id = id.clone();
        }
        self.providers.insert(id, provider);
    }

    /// Get a provider by its ID.
    pub fn get(&self, id: &str) -> Option<&dyn MusicProvider> {
        self.providers.get(id).map(|p| p.as_ref())
    }

    /// Get a shared reference to a provider for use in async tasks.
    pub fn get_shared(&self, id: &str) -> Option<Arc<dyn MusicProvider>> {
        self.providers.get(id).cloned()
    }

    /// Get a shared reference to the currently active provider.
    pub fn active_shared(&self) -> Option<Arc<dyn MusicProvider>> {
        self.get_shared(&self.active_provider_id)
    }

    /// Get the currently active provider (used for library browsing).
    pub fn active(&self) -> Option<&dyn MusicProvider> {
        self.get(&self.active_provider_id)
    }

    /// Set the active provider by ID. Returns false if the ID is not registered.
    pub fn set_active(&mut self, id: &str) -> bool {
        if self.providers.contains_key(id) {
            self.active_provider_id = id.to_string();
            true
        } else {
            false
        }
    }

    /// Get the active provider ID.
    pub fn active_id(&self) -> &str {
        &self.active_provider_id
    }

    /// Remove all providers of a given type.
    pub fn remove_by_type(&mut self, ptype: ProviderType) {
        self.providers.retain(|_, p| p.provider_type() != ptype);
        // If the active provider was removed, reset to the first remaining
        if !self.providers.contains_key(&self.active_provider_id) {
            self.active_provider_id = self.providers.keys().next().cloned().unwrap_or_default();
        }
    }

    /// List all registered provider IDs and names.
    pub fn list(&self) -> Vec<(String, String, ProviderType)> {
        self.providers
            .values()
            .map(|p| (p.id().to_string(), p.name().to_string(), p.provider_type()))
            .collect()
    }
}

use std::sync::Arc;
