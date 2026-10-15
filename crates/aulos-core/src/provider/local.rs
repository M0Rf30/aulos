// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Local filesystem music provider.
//!
//! Wraps the existing library scanning, database, cover art, and lyrics
//! functionality behind the [`MusicProvider`] trait.

use super::{MusicProvider, ProviderError, ProviderType};
use crate::library::covers::CachedThumb;
use crate::library::scan_progress::ScanProgress;
use crate::library::{
    Album, Artist, CoverArt, LibraryDb, LibraryScanner, LyricsProvider, Playlist, Track,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A music provider backed by local filesystem scanning and SQLite storage.
///
/// Wraps `LibraryDb` in a `Mutex` because `rusqlite::Connection` is not `Sync`,
/// but the `MusicProvider` trait requires `Send + Sync`. All database and
/// file-system work runs on [`crate::task::spawn_blocking`] (inline in the
/// browser), so a scan holding the lock never stalls an async worker.
pub struct LocalProvider {
    shared: Arc<Shared>,
}

/// What the blocking closures of [`LocalProvider`] own a handle to.
struct Shared {
    db: Mutex<LibraryDb>,
    music_dirs: Vec<PathBuf>,
}

impl Shared {
    /// Lock the database, returning a ProviderError on poisoned mutex.
    fn lock_db(&self) -> Result<std::sync::MutexGuard<'_, LibraryDb>, ProviderError> {
        self.db
            .lock()
            .map_err(|e| ProviderError::Database(format!("DB lock poisoned: {e}")))
    }
}

/// Parses the numeric row ID the local database uses for tracks.
fn parse_track_id(track_id: &str) -> Result<i64, ProviderError> {
    track_id
        .parse()
        .map_err(|_| ProviderError::Other(format!("Invalid track ID: {track_id}")))
}

impl LocalProvider {
    /// Create a new local provider.
    pub fn new(db: LibraryDb, music_dirs: Vec<PathBuf>) -> Self {
        Self {
            shared: Arc::new(Shared {
                db: Mutex::new(db),
                music_dirs,
            }),
        }
    }

    /// Runs `f` on the blocking pool with the shared state.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Shared) -> Result<T, ProviderError> + Send + 'static,
    ) -> Result<T, ProviderError> {
        let shared = Arc::clone(&self.shared);
        crate::task::spawn_blocking(move || f(&shared))
            .await
            .map_err(|e| ProviderError::Other(e.to_string()))?
    }

    /// Runs `f` on the locked database, on the blocking pool.
    async fn with_db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&LibraryDb) -> Result<T, String> + Send + 'static,
    ) -> Result<T, ProviderError> {
        self.blocking(move |shared| f(&*shared.lock_db()?).map_err(ProviderError::Database))
            .await
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
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

    async fn browse_albums(&self) -> Result<Vec<Album>, ProviderError> {
        self.with_db(|db| db.all_albums(Some("local"))).await
    }

    async fn browse_artists(&self) -> Result<Vec<Artist>, ProviderError> {
        self.with_db(|db| db.all_artists(Some("local"))).await
    }

    async fn browse_tracks(&self) -> Result<Vec<Track>, ProviderError> {
        self.with_db(|db| db.all_tracks(Some("local"))).await
    }

    async fn search(&self, query: &str) -> Result<Vec<Track>, ProviderError> {
        if query.is_empty() {
            return self.browse_tracks().await;
        }
        let query = query.to_string();
        self.with_db(move |db| db.search_tracks(&query, Some("local")))
            .await
    }

    async fn get_cover_art(&self, album: &Album) -> Result<Option<Vec<u8>>, ProviderError> {
        let path = album.tracks.first().map(|t| t.path.clone());
        self.blocking(move |_| Ok(path.and_then(|p| CoverArt::get_cover_art(&p))))
            .await
    }

    async fn get_lyrics(
        &self,
        track: &Track,
    ) -> Result<Option<crate::library::Lyrics>, ProviderError> {
        // Try embedded tags first, then .lrc sidecar file.
        // Online LRCLIB fetch is async and handled separately by the app.
        let path = track.path.clone();
        self.blocking(move |_| {
            Ok(LyricsProvider::from_tags(&path).or_else(|| LyricsProvider::from_lrc_file(&path)))
        })
        .await
    }

    async fn sync_library(&self, progress: Arc<ScanProgress>) -> Result<usize, ProviderError> {
        self.blocking(move |shared| {
            let db = shared.lock_db()?;
            LibraryScanner::scan(&db, &shared.music_dirs, &progress)
                .map_err(ProviderError::Database)
        })
        .await
    }

    // --- Playlists ---

    async fn list_playlists(&self) -> Result<Vec<Playlist>, ProviderError> {
        self.with_db(|db| db.list_playlists()).await
    }

    async fn get_playlist(&self, id: &str) -> Result<Playlist, ProviderError> {
        let id = id.to_string();
        self.with_db(move |db| db.get_playlist(&id)).await
    }

    async fn create_playlist(&self, name: &str) -> Result<Playlist, ProviderError> {
        let name = name.to_string();
        self.with_db(move |db| db.create_playlist(&name)).await
    }

    async fn delete_playlist(&self, id: &str) -> Result<(), ProviderError> {
        let id = id.to_string();
        self.with_db(move |db| db.delete_playlist(&id)).await
    }

    async fn rename_playlist(&self, id: &str, new_name: &str) -> Result<(), ProviderError> {
        let (id, new_name) = (id.to_string(), new_name.to_string());
        self.with_db(move |db| db.rename_playlist(&id, &new_name))
            .await
    }

    async fn add_to_playlist(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> Result<(), ProviderError> {
        let (playlist_id, track_ids) = (playlist_id.to_string(), track_ids.to_vec());
        self.with_db(move |db| db.add_to_playlist(&playlist_id, &track_ids))
            .await
    }

    fn cached_thumbs(&self) -> std::collections::HashMap<String, CachedThumb> {
        let loaded = self
            .shared
            .lock_db()
            .map_err(|e| e.to_string())
            .and_then(|db| db.cached_thumbs());
        loaded.unwrap_or_else(|e| {
            tracing::warn!("cover cache unreadable: {e}");
            Default::default()
        })
    }

    fn store_thumbs(
        &self,
        entries: &[(String, CachedThumb)],
        keep: &std::collections::HashSet<String>,
    ) {
        let stored = self
            .shared
            .lock_db()
            .map_err(|e| e.to_string())
            .and_then(|db| db.store_thumbs(entries, keep));
        if let Err(e) = stored {
            tracing::warn!("cover cache not saved: {e}");
        }
    }

    async fn remove_from_playlist(
        &self,
        playlist_id: &str,
        index: usize,
    ) -> Result<(), ProviderError> {
        let playlist_id = playlist_id.to_string();
        self.with_db(move |db| db.remove_playlist_track(&playlist_id, index))
            .await
    }

    async fn move_in_playlist(
        &self,
        playlist_id: &str,
        from: usize,
        to: usize,
    ) -> Result<(), ProviderError> {
        let playlist_id = playlist_id.to_string();
        self.with_db(move |db| db.move_playlist_track(&playlist_id, from, to))
            .await
    }

    // --- Favorites and Ratings ---

    async fn toggle_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let id = parse_track_id(track_id)?;
        self.with_db(move |db| db.toggle_favorite(id)).await
    }

    async fn is_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let id = parse_track_id(track_id)?;
        self.with_db(move |db| db.is_favorite(id)).await
    }

    async fn set_rating(&self, track_id: &str, rating: u8) -> Result<(), ProviderError> {
        let id = parse_track_id(track_id)?;
        self.with_db(move |db| db.set_rating(id, rating)).await
    }

    async fn get_rating(&self, track_id: &str) -> Result<Option<u8>, ProviderError> {
        let id = parse_track_id(track_id)?;
        self.with_db(move |db| db.get_rating(id)).await
    }

    async fn list_favorites(&self) -> Result<Vec<Track>, ProviderError> {
        self.with_db(|db| db.list_favorites(Some("local"))).await
    }

    // --- Genres ---

    async fn list_genres(&self) -> Result<Vec<String>, ProviderError> {
        self.with_db(|db| db.list_genres(Some("local"))).await
    }

    async fn get_tracks_by_genre(&self, genre: &str) -> Result<Vec<Track>, ProviderError> {
        let genre = genre.to_string();
        self.with_db(move |db| db.tracks_by_genre(&genre, Some("local")))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::track;

    fn song(title: &str) -> Track {
        Track {
            path: PathBuf::from(format!("/m/{title}.flac")),
            title: title.into(),
            artist: "artist".into(),
            album: "album".into(),
            provider_id: "local".into(),
            source_uri: format!("/m/{title}.flac"),
            ..track(0)
        }
    }

    fn provider(titles: &[&str]) -> Arc<dyn MusicProvider> {
        let db = LibraryDb::open_memory().expect("db");
        for title in titles {
            db.upsert_track(&song(title), 0).expect("track");
        }
        Arc::new(LocalProvider::new(db, Vec::new()))
    }

    #[tokio::test]
    async fn browse_and_search_run_through_the_trait_object() {
        let provider = provider(&["alpha", "beta"]);
        assert_eq!(provider.browse_tracks().await.expect("tracks").len(), 2);
        assert_eq!(provider.browse_albums().await.expect("albums").len(), 1);
        let found = provider.search("alp").await.expect("search");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "alpha");
    }

    #[tokio::test]
    async fn playlists_round_trip_through_the_database() {
        let provider = provider(&["alpha", "beta"]);
        let ids: Vec<String> = provider
            .browse_tracks()
            .await
            .expect("tracks")
            .iter()
            .map(|t| t.id.to_string())
            .collect();
        let created = provider.create_playlist("mix").await.expect("create");
        provider
            .add_to_playlist(&created.id, &ids)
            .await
            .expect("add");
        let stored = provider.get_playlist(&created.id).await.expect("get");
        assert_eq!(stored.tracks.len(), 2);
        provider.delete_playlist(&created.id).await.expect("delete");
        assert!(provider.list_playlists().await.expect("list").is_empty());
    }
}
