// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! SQLite-backed music library database.

use super::smart_playlist::SmartPlaylist;
use super::{Album, Artist, Track};
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Base schema for the tracks table.
const SCHEMA_BASE: &str = "
    CREATE TABLE IF NOT EXISTS tracks (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        path         TEXT NOT NULL UNIQUE,
        title        TEXT NOT NULL DEFAULT '',
        artist       TEXT NOT NULL DEFAULT '',
        album_artist TEXT NOT NULL DEFAULT '',
        album        TEXT NOT NULL DEFAULT '',
        genre        TEXT NOT NULL DEFAULT '',
        track_number INTEGER NOT NULL DEFAULT 0,
        disc_number  INTEGER NOT NULL DEFAULT 0,
        year         INTEGER NOT NULL DEFAULT 0,
        duration_ms  INTEGER NOT NULL DEFAULT 0,
        bitrate      INTEGER NOT NULL DEFAULT 0,
        sample_rate  INTEGER NOT NULL DEFAULT 0,
        mtime        INTEGER NOT NULL DEFAULT 0
    );

    CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album);
    CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
    CREATE INDEX IF NOT EXISTS idx_tracks_album_artist ON tracks(album_artist);
    CREATE INDEX IF NOT EXISTS idx_tracks_album_grouping ON tracks(album_artist, album, disc_number, track_number);

    CREATE TABLE IF NOT EXISTS cover_cache (
        album_key TEXT PRIMARY KEY,
        image_data BLOB
    );
";

/// The music library database.
pub struct LibraryDb {
    conn: Connection,
}

impl LibraryDb {
    /// Open or create the database at the given path.
    pub fn open(db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|e| format!("DB open error: {e}"))?;
        Self::configure_connection(&conn)?;
        conn.execute_batch(SCHEMA_BASE)
            .map_err(|e| format!("DB init error: {e}"))?;

        let db = Self { conn };
        db.run_migration()?;
        Ok(db)
    }

    /// Open an in-memory database (useful for tests).
    pub fn open_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("DB error: {e}"))?;
        Self::configure_connection(&conn)?;
        conn.execute_batch(SCHEMA_BASE)
            .map_err(|e| format!("DB init error: {e}"))?;

        let db = Self { conn };
        db.run_migration()?;
        Ok(db)
    }

    fn configure_connection(conn: &Connection) -> Result<(), String> {
        // WAL enables concurrent reads during background scans; synchronous=NORMAL
        // reduces fsync overhead; cache_size=-8000 gives an 8 MB page cache.
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-8000;",
        )
        .map_err(|e| format!("DB PRAGMA error: {e}"))?;

        let foreign_keys: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .map_err(|e| format!("DB foreign key verification error: {e}"))?;
        if foreign_keys != 1 {
            return Err("DB foreign keys could not be enabled".to_string());
        }
        Ok(())
    }

    /// Apply versioned migrations atomically.
    fn run_migration(&self) -> Result<(), String> {
        let version: u32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|e| format!("Migration version error: {e}"))?;

        if version < 1 {
            let has_provider = self.column_exists("tracks", "provider")?;
            let has_provider_track_id = self.column_exists("tracks", "provider_track_id")?;
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v1 transaction error: {e}"))?;

            if !has_provider {
                tx.execute_batch(
                    "ALTER TABLE tracks ADD COLUMN provider TEXT NOT NULL DEFAULT 'local';",
                )
                .map_err(|e| format!("Migration v1 error (provider): {e}"))?;
            }
            if !has_provider_track_id {
                tx.execute_batch(
                    "ALTER TABLE tracks ADD COLUMN provider_track_id TEXT NOT NULL DEFAULT '';",
                )
                .map_err(|e| format!("Migration v1 error (provider ID): {e}"))?;
            }
            tx.execute(
                "UPDATE tracks SET provider_track_id = path WHERE provider_track_id = ''",
                [],
            )
            .map_err(|e| format!("Migration v1 error (backfill): {e}"))?;
            tx.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_provider_id
                     ON tracks(provider, provider_track_id);
                 CREATE INDEX IF NOT EXISTS idx_tracks_provider ON tracks(provider);
                 PRAGMA user_version=1;",
            )
            .map_err(|e| format!("Migration v1 error (indexes): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v1 commit error: {e}"))?;
        }

        if version < 2 {
            let has_favorite = self.column_exists("tracks", "is_favorite")?;
            let has_rating = self.column_exists("tracks", "rating")?;
            let has_track_gain = self.column_exists("tracks", "rg_track_gain")?;
            let has_album_gain = self.column_exists("tracks", "rg_album_gain")?;
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v2 transaction error: {e}"))?;

            if !has_favorite {
                tx.execute_batch(
                    "ALTER TABLE tracks ADD COLUMN is_favorite INTEGER NOT NULL DEFAULT 0;",
                )
                .map_err(|e| format!("Migration v2 error (favorite): {e}"))?;
            }
            if !has_rating {
                tx.execute_batch("ALTER TABLE tracks ADD COLUMN rating INTEGER;")
                    .map_err(|e| format!("Migration v2 error (rating): {e}"))?;
            }
            if !has_track_gain {
                tx.execute_batch("ALTER TABLE tracks ADD COLUMN rg_track_gain REAL;")
                    .map_err(|e| format!("Migration v2 error (track gain): {e}"))?;
            }
            if !has_album_gain {
                tx.execute_batch("ALTER TABLE tracks ADD COLUMN rg_album_gain REAL;")
                    .map_err(|e| format!("Migration v2 error (album gain): {e}"))?;
            }
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS playlists (
                     id         INTEGER PRIMARY KEY AUTOINCREMENT,
                     name       TEXT NOT NULL,
                     created_at TEXT NOT NULL DEFAULT (datetime('now'))
                 );
                 CREATE TABLE IF NOT EXISTS playlist_tracks (
                     playlist_id INTEGER NOT NULL,
                     track_id    INTEGER NOT NULL,
                     position    INTEGER NOT NULL,
                     FOREIGN KEY (playlist_id) REFERENCES playlists(id) ON DELETE CASCADE,
                     FOREIGN KEY (track_id) REFERENCES tracks(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_playlist_tracks_playlist
                     ON playlist_tracks(playlist_id, position);
                 PRAGMA user_version=2;",
            )
            .map_err(|e| format!("Migration v2 error (schema): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v2 commit error: {e}"))?;
        }

        if version < 3 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v3 transaction error: {e}"))?;

            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS podcasts (
                     id             INTEGER PRIMARY KEY AUTOINCREMENT,
                     feed_url       TEXT NOT NULL UNIQUE,
                     title          TEXT NOT NULL DEFAULT '',
                     description    TEXT NOT NULL DEFAULT '',
                     image_url      TEXT NOT NULL DEFAULT '',
                     last_refreshed INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS podcast_episodes (
                     id             INTEGER PRIMARY KEY AUTOINCREMENT,
                     podcast_id     INTEGER NOT NULL REFERENCES podcasts(id) ON DELETE CASCADE,
                     guid           TEXT NOT NULL,
                     title          TEXT NOT NULL DEFAULT '',
                     enclosure_url  TEXT NOT NULL,
                     mime           TEXT NOT NULL DEFAULT '',
                     duration_secs  INTEGER NOT NULL DEFAULT 0,
                     pub_date       INTEGER NOT NULL DEFAULT 0,
                     description    TEXT NOT NULL DEFAULT '',
                     position_ms    INTEGER NOT NULL DEFAULT 0,
                     played         INTEGER NOT NULL DEFAULT 0,
                     UNIQUE(podcast_id, guid)
                 );
                 CREATE INDEX IF NOT EXISTS idx_podcast_episodes_podcast_pubdate
                     ON podcast_episodes(podcast_id, pub_date DESC);
                 CREATE TABLE IF NOT EXISTS radio_stations (
                     id           INTEGER PRIMARY KEY AUTOINCREMENT,
                     name         TEXT NOT NULL,
                     stream_url   TEXT NOT NULL UNIQUE,
                     homepage     TEXT NOT NULL DEFAULT '',
                     favicon_url  TEXT NOT NULL DEFAULT '',
                     tags         TEXT NOT NULL DEFAULT ''
                 );
                 PRAGMA user_version=3;",
            )
            .map_err(|e| format!("Migration v3 error (schema): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v3 commit error: {e}"))?;
        }

        if version < 4 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v4 transaction error: {e}"))?;

            tx.execute_batch(
                "ALTER TABLE podcast_episodes ADD COLUMN downloaded_path TEXT NOT NULL DEFAULT '';
                 PRAGMA user_version=4;",
            )
            .map_err(|e| format!("Migration v4 error (schema): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v4 commit error: {e}"))?;
        }

        if version < 5 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v5 transaction error: {e}"))?;

            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS smart_playlists (
                     id         INTEGER PRIMARY KEY AUTOINCREMENT,
                     name       TEXT NOT NULL,
                     definition TEXT NOT NULL,
                     created_at TEXT NOT NULL DEFAULT (datetime('now'))
                 );
                 PRAGMA user_version=5;",
            )
            .map_err(|e| format!("Migration v5 error (schema): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v5 commit error: {e}"))?;
        }

        if version < 6 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v6 transaction error: {e}"))?;

            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_tracks_genre ON tracks(genre);
                 PRAGMA user_version=6;",
            )
            .map_err(|e| format!("Migration v6 error (schema): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v6 commit error: {e}"))?;
        }

        if version < 7 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v7 transaction error: {e}"))?;

            tx.execute_batch(
                "ALTER TABLE podcasts ADD COLUMN author TEXT NOT NULL DEFAULT '';
                 PRAGMA user_version=7;",
            )
            .map_err(|e| format!("Migration v7 error (schema): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v7 commit error: {e}"))?;
        }

        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Orphan repair transaction error: {e}"))?;
        tx.execute(
            "DELETE FROM playlist_tracks
             WHERE playlist_id NOT IN (SELECT id FROM playlists)
                OR track_id NOT IN (SELECT id FROM tracks)",
            [],
        )
        .map_err(|e| format!("Orphan repair error: {e}"))?;
        tx.commit()
            .map_err(|e| format!("Orphan repair commit error: {e}"))?;
        Ok(())
    }

    /// Check if a column exists in a table using PRAGMA table_info.
    fn column_exists(&self, table: &str, column: &str) -> Result<bool, String> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| format!("PRAGMA error: {e}"))?;

        let exists = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("PRAGMA query error: {e}"))?
            .filter_map(|r| r.ok())
            .any(|name| name == column);

        Ok(exists)
    }

    /// Insert or update a track.
    #[tracing::instrument(skip(self, track), level = "debug")]
    pub fn upsert_track(&self, track: &Track, mtime: i64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO tracks (path, title, artist, album_artist, album, genre,
                 track_number, disc_number, year, duration_ms, bitrate, sample_rate, mtime,
                 provider, provider_track_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(path) DO UPDATE SET
                    title=excluded.title, artist=excluded.artist,
                    album_artist=excluded.album_artist, album=excluded.album,
                    genre=excluded.genre, track_number=excluded.track_number,
                    disc_number=excluded.disc_number, year=excluded.year,
                    duration_ms=excluded.duration_ms, bitrate=excluded.bitrate,
                    sample_rate=excluded.sample_rate, mtime=excluded.mtime,
                    provider=excluded.provider, provider_track_id=excluded.provider_track_id",
                params![
                    track.path.to_string_lossy().as_ref(),
                    track.title,
                    track.artist,
                    track.album_artist,
                    track.album,
                    track.genre,
                    track.track_number,
                    track.disc_number,
                    track.year,
                    track.duration.as_millis() as i64,
                    track.bitrate,
                    track.sample_rate,
                    mtime,
                    &*track.provider_id,
                    track.source_uri,
                ],
            )
            .map_err(|e| format!("Upsert error: {e}"))?;
        Ok(())
    }

    /// Remove missing local tracks beneath successfully scanned roots.
    #[tracing::instrument(skip(self, roots), level = "debug")]
    pub fn remove_missing_tracks(&self, roots: &[PathBuf]) -> Result<usize, String> {
        if roots.is_empty() {
            return Ok(0);
        }

        let tracks: Vec<(i64, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, path FROM tracks WHERE provider = 'local'")
                .map_err(|e| format!("Query error: {e}"))?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| format!("Query error: {e}"))?
                .collect::<Result<_, _>>()
                .map_err(|e| format!("Query row error: {e}"))?
        };

        let missing: Vec<i64> = tracks
            .into_iter()
            .filter_map(|(id, path)| {
                let path = PathBuf::from(path);
                if !roots.iter().any(|root| path.starts_with(root)) {
                    return None;
                }
                match std::fs::metadata(&path) {
                    Ok(_) => None,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(id),
                    Err(error) => {
                        tracing::warn!("Skipping inaccessible track {}: {error}", path.display());
                        None
                    }
                }
            })
            .collect();

        if missing.is_empty() {
            return Ok(0);
        }

        let count = missing.len();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Transaction begin error: {e}"))?;
        for chunk in missing.chunks(999) {
            let placeholders: String = (1..=chunk.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("DELETE FROM tracks WHERE id IN ({placeholders})");
            let params: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|id| id as &dyn rusqlite::types::ToSql)
                .collect();
            tx.execute(&sql, params.as_slice())
                .map_err(|e| format!("Batch delete error: {e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("Transaction commit error: {e}"))?;
        Ok(count)
    }

    /// Remove all tracks for a given provider (used when a provider is removed from config).
    pub fn remove_provider_tracks(&self, provider_id: &str) -> Result<usize, String> {
        let count = self
            .conn
            .execute(
                "DELETE FROM tracks WHERE provider = ?1",
                params![provider_id],
            )
            .map_err(|e| format!("Delete error: {e}"))?;
        Ok(count)
    }

    /// Get all tracks, optionally filtered by provider, ordered by album then track number.
    #[tracing::instrument(skip(self), level = "debug")]
    pub fn all_tracks(&self, provider: Option<&str>) -> Result<Vec<Track>, String> {
        let (sql, param): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match provider {
            Some(p) => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain
                 FROM tracks
                 WHERE provider = ?1
                 ORDER BY album_artist, album, disc_number, track_number"
                    .to_string(),
                vec![Box::new(p.to_string())],
            ),
            None => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain
                 FROM tracks
                 ORDER BY album_artist, album, disc_number, track_number"
                    .to_string(),
                vec![],
            ),
        };

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("Query error: {e}"))?;

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            param.iter().map(|p| p.as_ref()).collect();

        let tracks = stmt
            .query_map(params_ref.as_slice(), Self::row_to_track)
            .map_err(|e| format!("Query error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }

    /// Get all albums (grouped from tracks), optionally filtered by provider.
    ///
    /// Uses a HashMap index for O(1) grouping instead of linear search.
    #[tracing::instrument(skip(self), level = "debug")]
    pub fn all_albums(&self, provider: Option<&str>) -> Result<Vec<Album>, String> {
        let tracks = self.all_tracks(provider)?;

        if tracks.is_empty() {
            return Ok(Vec::new());
        }

        let mut albums: Vec<Album> = Vec::with_capacity(tracks.len() / 10);
        let mut index: HashMap<(String, String), usize> = HashMap::with_capacity(tracks.len() / 10);

        for track in tracks {
            let key = (track.album.clone(), track.album_artist.clone());
            if let Some(&idx) = index.get(&key) {
                albums[idx].tracks.push(track);
            } else {
                let idx = albums.len();
                index.insert(key, idx);
                albums.push(Album {
                    name: track.album.clone(),
                    artist: track.album_artist.clone(),
                    year: track.year,
                    cover_source: None,
                    tracks: vec![track],
                });
            }
        }

        albums.sort_unstable_by(|a, b| a.artist.cmp(&b.artist).then(a.year.cmp(&b.year)));
        Ok(albums)
    }

    /// Get all artists (grouped from albums), optionally filtered by provider.
    ///
    /// Uses a HashMap index for O(1) grouping instead of linear search.
    #[tracing::instrument(skip(self), level = "debug")]
    pub fn all_artists(&self, provider: Option<&str>) -> Result<Vec<Artist>, String> {
        let albums = self.all_albums(provider)?;

        if albums.is_empty() {
            return Ok(Vec::new());
        }

        let mut artists: Vec<Artist> = Vec::with_capacity(albums.len() / 3);
        let mut index: HashMap<String, usize> = HashMap::with_capacity(albums.len() / 3);

        for album in albums {
            if let Some(&idx) = index.get(&album.artist) {
                artists[idx].albums.push(album);
            } else {
                let idx = artists.len();
                let name = album.artist.clone();
                index.insert(name.clone(), idx);
                artists.push(Artist {
                    name,
                    albums: vec![album],
                });
            }
        }

        artists.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(artists)
    }

    /// Cache cover art for an album. Key includes provider to avoid collisions.
    pub fn cache_cover(&self, album_key: &str, data: &[u8]) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO cover_cache (album_key, image_data) VALUES (?1, ?2)",
                params![album_key, data],
            )
            .map_err(|e| format!("Cover cache error: {e}"))?;
        Ok(())
    }

    /// Retrieve cached cover art.
    pub fn get_cached_cover(&self, album_key: &str) -> Option<Vec<u8>> {
        self.conn
            .query_row(
                "SELECT image_data FROM cover_cache WHERE album_key = ?1",
                params![album_key],
                |row| row.get(0),
            )
            .ok()
    }

    /// Remove a single track by its file path.
    ///
    /// Used by the incremental filesystem-watcher scan to remove deleted files.
    pub fn remove_track_by_path(&self, path: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM tracks WHERE path = ?1", params![path])
            .map_err(|e| format!("Delete by path error: {e}"))?;
        Ok(())
    }

    /// Get track mtime to determine if rescan is needed.
    pub fn get_track_mtime(&self, path: &str) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT mtime FROM tracks WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .ok()
    }

    /// Get every known path and stored mtime for one provider in a single
    /// query — used by a full directory scan to skip unchanged files
    /// without issuing a point query per file on disk.
    pub fn track_mtimes(&self, provider: &str) -> Result<HashMap<String, i64>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, mtime FROM tracks WHERE provider = ?1")
            .map_err(|e| format!("Query error: {e}"))?;
        let mtimes = stmt
            .query_map(params![provider], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(|e| format!("Query error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(mtimes)
    }

    /// Count total tracks, optionally filtered by provider.
    pub fn track_count(&self, provider: Option<&str>) -> usize {
        match provider {
            Some(p) => self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM tracks WHERE provider = ?1",
                    params![p],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0) as usize,
            None => self
                .conn
                .query_row("SELECT COUNT(*) FROM tracks", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap_or(0) as usize,
        }
    }

    // ---- Search ----

    /// Search tracks by matching query against title, artist, album, or genre.
    pub fn search_tracks(&self, query: &str, provider: Option<&str>) -> Result<Vec<Track>, String> {
        let pattern = format!("%{}%", super::smart_playlist::escape_like(query));
        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match provider {
            Some(p) => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain
                 FROM tracks
                 WHERE provider = ?1
                   AND (title LIKE ?2 ESCAPE '\\' OR artist LIKE ?2 ESCAPE '\\'
                        OR album LIKE ?2 ESCAPE '\\' OR genre LIKE ?2 ESCAPE '\\')
                 ORDER BY
                   CASE WHEN title LIKE ?2 ESCAPE '\\' THEN 0 ELSE 1 END,
                   title"
                    .to_string(),
                vec![
                    Box::new(p.to_string()),
                    Box::new(pattern),
                ],
            ),
            None => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain
                 FROM tracks
                 WHERE title LIKE ?1 ESCAPE '\\' OR artist LIKE ?1 ESCAPE '\\'
                    OR album LIKE ?1 ESCAPE '\\' OR genre LIKE ?1 ESCAPE '\\'
                 ORDER BY
                   CASE WHEN title LIKE ?1 ESCAPE '\\' THEN 0 ELSE 1 END,
                   title"
                    .to_string(),
                vec![Box::new(pattern)],
            ),
        };

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("Search query error: {e}"))?;

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let tracks = stmt
            .query_map(params_ref.as_slice(), Self::row_to_track)
            .map_err(|e| format!("Search query error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }

    // ---- Playlists ----

    /// List all playlists.
    pub fn list_playlists(&self) -> Result<Vec<super::Playlist>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT p.id, p.name,
                        COUNT(pt.track_id) as track_count,
                        COALESCE(SUM(t.duration_ms), 0) as total_duration_ms
                 FROM playlists p
                 LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
                 LEFT JOIN tracks t ON t.id = pt.track_id
                 GROUP BY p.id
                 ORDER BY p.name",
            )
            .map_err(|e| format!("List playlists error: {e}"))?;

        let playlists = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let track_count: u32 = row.get(2)?;
                let total_ms: i64 = row.get(3)?;
                Ok(super::Playlist {
                    id: id.to_string(),
                    name,
                    tracks: Vec::new(),
                    track_count,
                    total_duration: Duration::from_millis(total_ms as u64),
                })
            })
            .map_err(|e| format!("List playlists error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(playlists)
    }

    /// Get a playlist with its tracks.
    pub fn get_playlist(&self, playlist_id: &str) -> Result<super::Playlist, String> {
        let id: i64 = playlist_id
            .parse()
            .map_err(|_| "Invalid playlist ID".to_string())?;

        let (name, created_at): (String, String) = self
            .conn
            .query_row(
                "SELECT name, created_at FROM playlists WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| format!("Get playlist error: {e}"))?;

        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.id, t.path, t.title, t.artist, t.album_artist, t.album, t.genre,
                        t.track_number, t.disc_number, t.year, t.duration_ms, t.bitrate, t.sample_rate,
                        t.provider, t.provider_track_id, t.is_favorite, t.rating, t.rg_track_gain, t.rg_album_gain
                 FROM playlist_tracks pt
                 JOIN tracks t ON t.id = pt.track_id
                 WHERE pt.playlist_id = ?1
                 ORDER BY pt.position",
            )
            .map_err(|e| format!("Get playlist tracks error: {e}"))?;

        let tracks: Vec<Track> = stmt
            .query_map(params![id], Self::row_to_track)
            .map_err(|e| format!("Get playlist tracks error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        let total_duration = tracks.iter().map(|t| t.duration).sum();
        let track_count = tracks.len() as u32;
        let _ = created_at; // available if needed later

        Ok(super::Playlist {
            id: playlist_id.to_string(),
            name,
            tracks,
            track_count,
            total_duration,
        })
    }

    /// Create a new playlist.
    pub fn create_playlist(&self, name: &str) -> Result<super::Playlist, String> {
        self.conn
            .execute("INSERT INTO playlists (name) VALUES (?1)", params![name])
            .map_err(|e| format!("Create playlist error: {e}"))?;

        let id = self.conn.last_insert_rowid();
        Ok(super::Playlist {
            id: id.to_string(),
            name: name.to_string(),
            tracks: Vec::new(),
            track_count: 0,
            total_duration: Duration::ZERO,
        })
    }

    /// Delete a playlist.
    pub fn delete_playlist(&self, playlist_id: &str) -> Result<(), String> {
        let id: i64 = playlist_id
            .parse()
            .map_err(|_| "Invalid playlist ID".to_string())?;

        // CASCADE handles playlist_tracks deletion.
        self.conn
            .execute("DELETE FROM playlists WHERE id = ?1", params![id])
            .map_err(|e| format!("Delete playlist error: {e}"))?;
        Ok(())
    }

    /// Rename a playlist.
    pub fn rename_playlist(&self, playlist_id: &str, new_name: &str) -> Result<(), String> {
        let id: i64 = playlist_id
            .parse()
            .map_err(|_| "Invalid playlist ID".to_string())?;

        self.conn
            .execute(
                "UPDATE playlists SET name = ?1 WHERE id = ?2",
                params![new_name, id],
            )
            .map_err(|e| format!("Rename playlist error: {e}"))?;
        Ok(())
    }

    /// Add tracks to a playlist by their IDs.
    pub fn add_to_playlist(&self, playlist_id: &str, track_ids: &[String]) -> Result<(), String> {
        let pid: i64 = playlist_id
            .parse()
            .map_err(|_| "Invalid playlist ID".to_string())?;

        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Add to playlist transaction error: {e}"))?;

        // Get current max position.
        let max_pos: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(position), -1) FROM playlist_tracks WHERE playlist_id = ?1",
                params![pid],
                |row| row.get(0),
            )
            .unwrap_or(-1);

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (?1, ?2, ?3)",
                )
                .map_err(|e| format!("Add to playlist prepare error: {e}"))?;

            for (i, tid) in track_ids.iter().enumerate() {
                let track_id: i64 = tid
                    .parse()
                    .map_err(|_| format!("Invalid track ID: {tid}"))?;
                stmt.execute(params![pid, track_id, max_pos + 1 + i as i64])
                    .map_err(|e| format!("Add to playlist error: {e}"))?;
            }
        }

        tx.commit()
            .map_err(|e| format!("Add to playlist commit error: {e}"))?;
        Ok(())
    }

    // ---- Favorites and Ratings ----

    /// Toggle the favorite status of a track. Returns the new status.
    pub fn toggle_favorite(&self, track_id: i64) -> Result<bool, String> {
        self.conn
            .execute(
                "UPDATE tracks SET is_favorite = CASE WHEN is_favorite = 0 THEN 1 ELSE 0 END
                 WHERE id = ?1",
                params![track_id],
            )
            .map_err(|e| format!("Toggle favorite error: {e}"))?;

        let new_val: bool = self
            .conn
            .query_row(
                "SELECT is_favorite FROM tracks WHERE id = ?1",
                params![track_id],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .map_err(|e| format!("Toggle favorite read error: {e}"))?;

        Ok(new_val)
    }

    /// Check if a track is a favorite.
    pub fn is_favorite(&self, track_id: i64) -> Result<bool, String> {
        self.conn
            .query_row(
                "SELECT is_favorite FROM tracks WHERE id = ?1",
                params![track_id],
                |row| row.get::<_, i32>(0).map(|v| v != 0),
            )
            .map_err(|e| format!("Is favorite error: {e}"))
    }

    /// Set a rating (1-5) for a track. Pass 0 to clear.
    pub fn set_rating(&self, track_id: i64, rating: u8) -> Result<(), String> {
        let val: Option<u8> = if rating == 0 { None } else { Some(rating) };
        self.conn
            .execute(
                "UPDATE tracks SET rating = ?1 WHERE id = ?2",
                params![val, track_id],
            )
            .map_err(|e| format!("Set rating error: {e}"))?;
        Ok(())
    }

    /// Get the rating for a track.
    pub fn get_rating(&self, track_id: i64) -> Result<Option<u8>, String> {
        self.conn
            .query_row(
                "SELECT rating FROM tracks WHERE id = ?1",
                params![track_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("Get rating error: {e}"))
    }

    /// List all favorite tracks.
    pub fn list_favorites(&self, provider: Option<&str>) -> Result<Vec<Track>, String> {
        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match provider {
            Some(p) => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain
                 FROM tracks
                 WHERE provider = ?1 AND is_favorite = 1
                 ORDER BY title"
                    .to_string(),
                vec![Box::new(p.to_string())],
            ),
            None => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain
                 FROM tracks
                 WHERE is_favorite = 1
                 ORDER BY title"
                    .to_string(),
                vec![],
            ),
        };

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("List favorites error: {e}"))?;

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let tracks = stmt
            .query_map(params_ref.as_slice(), Self::row_to_track)
            .map_err(|e| format!("List favorites error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }

    // ---- Genres ----

    /// List all distinct genres.
    pub fn list_genres(&self, provider: Option<&str>) -> Result<Vec<String>, String> {
        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match provider {
            Some(p) => (
                "SELECT DISTINCT genre FROM tracks
                 WHERE provider = ?1 AND genre != ''
                 ORDER BY genre"
                    .to_string(),
                vec![Box::new(p.to_string())],
            ),
            None => (
                "SELECT DISTINCT genre FROM tracks
                 WHERE genre != ''
                 ORDER BY genre"
                    .to_string(),
                vec![],
            ),
        };

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("List genres error: {e}"))?;

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let genres = stmt
            .query_map(params_ref.as_slice(), |row| row.get(0))
            .map_err(|e| format!("List genres error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(genres)
    }

    /// Get all tracks matching a genre.
    pub fn tracks_by_genre(
        &self,
        genre: &str,
        provider: Option<&str>,
    ) -> Result<Vec<Track>, String> {
        let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match provider {
            Some(p) => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain
                 FROM tracks
                 WHERE provider = ?1 AND genre = ?2
                 ORDER BY album_artist, album, disc_number, track_number"
                    .to_string(),
                vec![Box::new(p.to_string()), Box::new(genre.to_string())],
            ),
            None => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain
                 FROM tracks
                 WHERE genre = ?1
                 ORDER BY album_artist, album, disc_number, track_number"
                    .to_string(),
                vec![Box::new(genre.to_string())],
            ),
        };

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("Tracks by genre error: {e}"))?;

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let tracks = stmt
            .query_map(params_ref.as_slice(), Self::row_to_track)
            .map_err(|e| format!("Tracks by genre error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }

    // ---- Smart Playlists ----

    /// List all smart playlists, each with a live-resolved track count.
    ///
    /// The count re-runs the playlist's query across every provider (it
    /// is not scoped, since the list view isn't tied to one provider) —
    /// with realistically few saved smart playlists, re-querying to size
    /// the result costs nothing worth caching.
    pub fn list_smart_playlists(&self) -> Result<Vec<SmartPlaylist>, String> {
        let rows: Vec<(i64, String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, name, definition FROM smart_playlists ORDER BY name")
                .map_err(|e| format!("List smart playlists error: {e}"))?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(|e| format!("List smart playlists error: {e}"))?
                .filter_map(|r| r.ok())
                .collect()
        };

        let mut playlists = Vec::with_capacity(rows.len());
        for (id, name, definition) in rows {
            match serde_json::from_str::<SmartPlaylist>(&definition) {
                Ok(mut playlist) => {
                    playlist.id = id;
                    playlist.name = name;
                    playlist.track_count = self
                        .smart_playlist_tracks(&playlist, None)
                        .map(|t| t.len())
                        .unwrap_or(0);
                    playlists.push(playlist);
                }
                Err(e) => tracing::warn!("Corrupt smart playlist definition (id {id}): {e}"),
            }
        }

        Ok(playlists)
    }

    /// Create a new smart playlist. Returns the assigned row id.
    pub fn create_smart_playlist(&self, playlist: &SmartPlaylist) -> Result<i64, String> {
        let definition = serde_json::to_string(playlist)
            .map_err(|e| format!("Serialize smart playlist error: {e}"))?;
        self.conn
            .execute(
                "INSERT INTO smart_playlists (name, definition) VALUES (?1, ?2)",
                params![playlist.name, definition],
            )
            .map_err(|e| format!("Create smart playlist error: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update an existing smart playlist's name and rule definition.
    pub fn update_smart_playlist(&self, playlist: &SmartPlaylist) -> Result<(), String> {
        let definition = serde_json::to_string(playlist)
            .map_err(|e| format!("Serialize smart playlist error: {e}"))?;
        self.conn
            .execute(
                "UPDATE smart_playlists SET name = ?1, definition = ?2 WHERE id = ?3",
                params![playlist.name, definition, playlist.id],
            )
            .map_err(|e| format!("Update smart playlist error: {e}"))?;
        Ok(())
    }

    /// Delete a smart playlist by id.
    pub fn delete_smart_playlist(&self, id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM smart_playlists WHERE id = ?1", params![id])
            .map_err(|e| format!("Delete smart playlist error: {e}"))?;
        Ok(())
    }

    /// Resolve a smart playlist's rules into matching tracks, optionally
    /// scoped to one provider (mirrors `all_tracks`/`search_tracks`).
    ///
    /// Splices a `provider = ?N` clause onto whatever `to_sql` produced.
    /// The extra parameter is appended (never inserted into the middle of
    /// the vector) and given the next sequential placeholder number, since
    /// SQLite resolves a numbered `?N` parameter by its number, not by
    /// where it appears in the query text.
    pub fn smart_playlist_tracks(
        &self,
        playlist: &SmartPlaylist,
        provider: Option<&str>,
    ) -> Result<Vec<Track>, String> {
        let (mut sql, mut param_values) = playlist.to_sql();

        if let Some(p) = provider {
            let order_pos = sql
                .find(" ORDER BY")
                .ok_or_else(|| "Smart playlist query malformed: no ORDER BY".to_string())?;
            let next_n = param_values.len() + 1;
            let clause = if playlist.rules.is_empty() {
                format!(" WHERE provider = ?{next_n}")
            } else {
                format!(" AND provider = ?{next_n}")
            };
            sql.insert_str(order_pos, &clause);
            param_values.push(Box::new(p.to_string()));
        }

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("Smart playlist query error: {e}"))?;

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|v| v.as_ref()).collect();

        let tracks = stmt
            .query_map(params_ref.as_slice(), Self::row_to_track)
            .map_err(|e| format!("Smart playlist query error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }

    /// Map a database row to a Track struct.
    fn row_to_track(row: &rusqlite::Row<'_>) -> rusqlite::Result<Track> {
        let path_str: String = row.get(1)?;
        Ok(Track {
            id: row.get(0)?,
            path: PathBuf::from(&path_str),
            title: row.get(2)?,
            artist: row.get(3)?,
            album_artist: row.get(4)?,
            album: row.get(5)?,
            genre: row.get(6)?,
            track_number: row.get(7)?,
            disc_number: row.get(8)?,
            year: row.get(9)?,
            duration: Duration::from_millis(row.get::<_, i64>(10)? as u64),
            bitrate: row.get(11)?,
            sample_rate: row.get(12)?,
            provider_id: Arc::from(row.get::<_, String>(13)?),
            source_uri: row.get(14)?,
            is_favorite: row.get::<_, i32>(15).unwrap_or(0) != 0,
            rating: row.get::<_, Option<u8>>(16).unwrap_or(None),
            rg_track_gain: row.get::<_, Option<f32>>(17).unwrap_or(None),
            rg_album_gain: row.get::<_, Option<f32>>(18).unwrap_or(None),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lyra-db-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn insert_local(db: &LibraryDb, path: &Path) -> i64 {
        let path = path.to_string_lossy();
        db.conn
            .execute(
                "INSERT INTO tracks (path, provider, provider_track_id) VALUES (?1, 'local', ?1)",
                params![path.as_ref()],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    #[test]
    fn foreign_keys_reject_orphans_and_cascade_track_deletes() {
        let db = LibraryDb::open_memory().unwrap();
        assert_eq!(
            db.conn
                .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );

        let track_id = insert_local(&db, Path::new("/tmp/lyra-cascade.mp3"));
        let playlist = db.create_playlist("test").unwrap();
        db.add_to_playlist(&playlist.id, &[track_id.to_string()])
            .unwrap();
        assert!(
            db.add_to_playlist(&playlist.id, &["999999".to_string()])
                .is_err()
        );

        db.conn
            .execute("DELETE FROM tracks WHERE id = ?1", params![track_id])
            .unwrap();
        assert_eq!(
            db.conn
                .query_row("SELECT COUNT(*) FROM playlist_tracks", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn migrations_roll_back_failures_and_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        LibraryDb::configure_connection(&conn).unwrap();
        conn.execute_batch(SCHEMA_BASE).unwrap();
        conn.execute_batch("CREATE VIEW idx_tracks_provider AS SELECT 1;")
            .unwrap();
        let db = LibraryDb { conn };

        assert!(db.run_migration().is_err());
        assert!(!db.column_exists("tracks", "provider").unwrap());
        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            0
        );

        db.conn
            .execute_batch("DROP VIEW idx_tracks_provider;")
            .unwrap();
        db.run_migration().unwrap();
        db.run_migration().unwrap();
        assert!(db.column_exists("tracks", "rg_album_gain").unwrap());
        // v5 added the smart_playlists table, v6 added the genre index, v7
        // added podcasts.author; assert all so this test keeps pinning the
        // migration ladder's head.
        assert!(
            db.conn
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name='smart_playlists'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .is_ok()
        );
        assert!(
            db.conn
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='index' AND name='idx_tracks_genre'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .is_ok()
        );
        assert!(db.column_exists("podcasts", "author").unwrap());
        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            7
        );
    }

    #[test]
    fn migrations_remove_legacy_playlist_orphans() {
        let db = LibraryDb::open_memory().unwrap();
        db.conn
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 INSERT INTO playlist_tracks (playlist_id, track_id, position) VALUES (1, 1, 0);
                 PRAGMA foreign_keys=ON;",
            )
            .unwrap();

        db.run_migration().unwrap();
        assert_eq!(
            db.conn
                .query_row("SELECT COUNT(*) FROM playlist_tracks", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn failed_batched_cleanup_rolls_back_every_chunk() {
        let db = LibraryDb::open_memory().unwrap();
        let root = temp_root("cleanup-rollback");
        let prefix = root.join("missing-").to_string_lossy().into_owned();
        db.conn
            .execute(
                "INSERT INTO tracks (path, provider, provider_track_id)
                 WITH RECURSIVE n(value) AS (
                     VALUES(1) UNION ALL SELECT value + 1 FROM n WHERE value < 1000
                 )
                 SELECT ?1 || value, 'local', ?1 || value FROM n",
                params![prefix],
            )
            .unwrap();
        db.conn
            .execute_batch(
                "CREATE TRIGGER fail_last_cleanup BEFORE DELETE ON tracks
                 WHEN OLD.id = 1000
                 BEGIN SELECT RAISE(ABORT, 'forced cleanup failure'); END;",
            )
            .unwrap();

        assert!(
            db.remove_missing_tracks(std::slice::from_ref(&root))
                .is_err()
        );
        assert_eq!(
            db.conn
                .query_row("SELECT COUNT(*) FROM tracks", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1000
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn search_tracks_treats_underscore_as_literal_not_wildcard() {
        let db = LibraryDb::open_memory().unwrap();
        let mut literal = Track {
            id: 0,
            path: PathBuf::from("/music/literal.flac"),
            title: "track_1".to_string(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 0,
            duration: Duration::ZERO,
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("local"),
            source_uri: "/music/literal.flac".to_string(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        };
        db.upsert_track(&literal, 0).unwrap();
        literal.path = PathBuf::from("/music/wildcard-match.flac");
        literal.source_uri = "/music/wildcard-match.flac".to_string();
        literal.title = "trackX1".to_string();
        db.upsert_track(&literal, 0).unwrap();

        // "_" in the query is user text, not a single-character LIKE
        // wildcard: only the literal "track_1" title should come back.
        let results = db.search_tracks("track_1", None).unwrap();
        assert_eq!(results.len(), 1, "unexpected matches: {results:?}");
        assert_eq!(results[0].title, "track_1");
    }
}
