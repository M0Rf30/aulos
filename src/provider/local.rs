// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Local filesystem music provider.
//!
//! Wraps the existing library scanning, database, cover art, and lyrics
//! functionality behind the [`MusicProvider`] trait.

use super::{MusicProvider, ProviderError, ProviderType};
use crate::library::{
    Album, Artist, CoverArt, LibraryDb, LibraryScanner, LyricsProvider, Playlist, Track,
};
use std::path::PathBuf;
use std::sync::Mutex;

/// A music provider backed by local filesystem scanning and SQLite storage.
///
/// Wraps `LibraryDb` in a `Mutex` because `rusqlite::Connection` is not `Sync`,
/// but the `MusicProvider` trait requires `Send + Sync`.
pub struct LocalProvider {
    db: Mutex<LibraryDb>,
    music_dirs: Vec<PathBuf>,
}

impl LocalProvider {
    /// Create a new local provider.
    pub fn new(db: LibraryDb, music_dirs: Vec<PathBuf>) -> Self {
        Self {
            db: Mutex::new(db),
            music_dirs,
        }
    }

    /// Lock the database, returning a ProviderError on poisoned mutex.
    fn lock_db(&self) -> Result<std::sync::MutexGuard<'_, LibraryDb>, ProviderError> {
        self.db
            .lock()
            .map_err(|e| ProviderError::Database(format!("DB lock poisoned: {e}")))
    }
}

impl MusicProvider for LocalProvider {
    fn id(&self) -> &str {
        "local"
    }

    fn name(&self) -> &str {
        "Local Music"
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Local
    }

    fn browse_albums(&self) -> Result<Vec<Album>, ProviderError> {
        let db = self.lock_db()?;
        db.all_albums(Some("local"))
            .map_err(ProviderError::Database)
    }

    fn browse_artists(&self) -> Result<Vec<Artist>, ProviderError> {
        let db = self.lock_db()?;
        db.all_artists(Some("local"))
            .map_err(ProviderError::Database)
    }

    fn browse_tracks(&self) -> Result<Vec<Track>, ProviderError> {
        let db = self.lock_db()?;
        db.all_tracks(Some("local"))
            .map_err(ProviderError::Database)
    }

    fn search(&self, query: &str) -> Result<Vec<Track>, ProviderError> {
        if query.is_empty() {
            return self.browse_tracks();
        }
        let db = self.lock_db()?;
        db.search_tracks(query, Some("local"))
            .map_err(ProviderError::Database)
    }

    fn get_cover_art(&self, album: &Album) -> Result<Option<Vec<u8>>, ProviderError> {
        let bytes = album
            .tracks
            .first()
            .and_then(|t| CoverArt::get_cover_art(&t.path));
        Ok(bytes)
    }

    fn get_lyrics(&self, track: &Track) -> Result<Option<crate::library::Lyrics>, ProviderError> {
        // Try embedded tags first, then .lrc sidecar file.
        // Online LRCLIB fetch is async and handled separately by the app.
        let lyrics = LyricsProvider::from_tags(&track.path)
            .or_else(|| LyricsProvider::from_lrc_file(&track.path));
        Ok(lyrics)
    }

    fn sync_library(&self) -> Result<usize, ProviderError> {
        let db = self.lock_db()?;
        LibraryScanner::scan(&db, &self.music_dirs).map_err(ProviderError::Database)
    }

    // --- Playlists ---

    fn list_playlists(&self) -> Result<Vec<Playlist>, ProviderError> {
        let db = self.lock_db()?;
        db.list_playlists().map_err(ProviderError::Database)
    }

    fn get_playlist(&self, id: &str) -> Result<Playlist, ProviderError> {
        let db = self.lock_db()?;
        db.get_playlist(id).map_err(ProviderError::Database)
    }

    fn create_playlist(&self, name: &str) -> Result<Playlist, ProviderError> {
        let db = self.lock_db()?;
        db.create_playlist(name).map_err(ProviderError::Database)
    }

    fn delete_playlist(&self, id: &str) -> Result<(), ProviderError> {
        let db = self.lock_db()?;
        db.delete_playlist(id).map_err(ProviderError::Database)
    }

    fn rename_playlist(&self, id: &str, new_name: &str) -> Result<(), ProviderError> {
        let db = self.lock_db()?;
        db.rename_playlist(id, new_name)
            .map_err(ProviderError::Database)
    }

    fn add_to_playlist(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> Result<(), ProviderError> {
        let db = self.lock_db()?;
        db.add_to_playlist(playlist_id, track_ids)
            .map_err(ProviderError::Database)
    }

    // --- Favorites and Ratings ---

    fn toggle_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| ProviderError::Other(format!("Invalid track ID: {track_id}")))?;
        let db = self.lock_db()?;
        db.toggle_favorite(id).map_err(ProviderError::Database)
    }

    fn is_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| ProviderError::Other(format!("Invalid track ID: {track_id}")))?;
        let db = self.lock_db()?;
        db.is_favorite(id).map_err(ProviderError::Database)
    }

    fn set_rating(&self, track_id: &str, rating: u8) -> Result<(), ProviderError> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| ProviderError::Other(format!("Invalid track ID: {track_id}")))?;
        let db = self.lock_db()?;
        db.set_rating(id, rating).map_err(ProviderError::Database)
    }

    fn get_rating(&self, track_id: &str) -> Result<Option<u8>, ProviderError> {
        let id: i64 = track_id
            .parse()
            .map_err(|_| ProviderError::Other(format!("Invalid track ID: {track_id}")))?;
        let db = self.lock_db()?;
        db.get_rating(id).map_err(ProviderError::Database)
    }

    fn list_favorites(&self) -> Result<Vec<Track>, ProviderError> {
        let db = self.lock_db()?;
        db.list_favorites(Some("local"))
            .map_err(ProviderError::Database)
    }

    // --- Genres ---

    fn list_genres(&self) -> Result<Vec<String>, ProviderError> {
        let db = self.lock_db()?;
        db.list_genres(Some("local"))
            .map_err(ProviderError::Database)
    }

    fn get_tracks_by_genre(&self, genre: &str) -> Result<Vec<Track>, ProviderError> {
        let db = self.lock_db()?;
        db.tracks_by_genre(genre, Some("local"))
            .map_err(ProviderError::Database)
    }
}
