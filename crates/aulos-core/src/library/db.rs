// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! SQLite-backed music library database.

use super::covers::{CachedThumb, CoverThumb, ThumbStamp};
use super::smart_playlist::SmartPlaylist;
use super::{Album, Artist, Track};
use rusqlite::{Connection, params};
use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
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

/// Database files whose schema/migrations already ran in this process.
fn ready_paths() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static READY: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    READY.get_or_init(Default::default)
}

fn schema_is_ready(path: &Path) -> bool {
    ready_paths()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(path)
}

fn mark_schema_ready(path: &Path) {
    ready_paths()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(path.to_path_buf());
}

/// The music library database.
pub struct LibraryDb {
    pub(super) conn: Connection,
}

impl LibraryDb {
    /// Open or create the database at the given path.
    ///
    /// The schema script and migrations run only the first time a given
    /// database file is opened in this process; later opens (every Home
    /// load, smart-playlist switch, `record_play`, ...) just open the
    /// connection and apply the per-connection PRAGMAs. If the file has
    /// disappeared in the meantime it is initialised again.
    pub fn open(db_path: &Path) -> Result<Self, String> {
        // In the browser the database lives in memory (SQLite's default VFS
        // there), shared by name between connections, so there is no file
        // that could have disappeared.
        let backing_present = cfg!(target_arch = "wasm32") || db_path.exists();
        let already_initialised = backing_present && schema_is_ready(db_path);
        let conn = Connection::open(db_path).map_err(|e| format!("DB open error: {e}"))?;
        Self::configure_connection(&conn)?;
        if already_initialised {
            return Ok(Self { conn });
        }
        conn.execute_batch(SCHEMA_BASE)
            .map_err(|e| format!("DB init error: {e}"))?;

        let db = Self { conn };
        db.run_migration()?;
        mark_schema_ready(db_path);
        Ok(db)
    }

    /// The database file backing this connection (`None` in memory).
    pub fn file_path(&self) -> Option<PathBuf> {
        self.conn
            .path()
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
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
        super::smart_playlist::register_sql_functions(conn)
            .map_err(|e| format!("DB function registration error: {e}"))?;
        // The browser keeps the database in memory; every commit is a reason
        // to save a new snapshot of it.
        #[cfg(target_arch = "wasm32")]
        let _ = conn.commit_hook(Some(|| {
            crate::web_fs::persist::mark_dirty();
            false
        }));
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

        if version < 8 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v8 transaction error: {e}"))?;

            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS play_history (
                     id        INTEGER PRIMARY KEY AUTOINCREMENT,
                     track_id  INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                     played_at INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_play_history_played_at
                     ON play_history(played_at);
                 CREATE INDEX IF NOT EXISTS idx_play_history_track
                     ON play_history(track_id);
                 PRAGMA user_version=8;",
            )
            .map_err(|e| format!("Migration v8 error (schema): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v8 commit error: {e}"))?;
        }

        if version < 9 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v9 transaction error: {e}"))?;
            if !self.column_exists("tracks", "bit_depth")? {
                tx.execute_batch(
                    "ALTER TABLE tracks ADD COLUMN bit_depth INTEGER NOT NULL DEFAULT 0;",
                )
                .map_err(|e| format!("Migration v9 error (bit_depth): {e}"))?;
            }
            // Local files are skipped by the scanner while their mtime matches,
            // so reset it to make the next scan read the bit depth of each.
            tx.execute_batch("UPDATE tracks SET mtime = 0 WHERE provider = 'local';")
                .map_err(|e| format!("Migration v9 error (rescan): {e}"))?;
            tx.execute_batch("PRAGMA user_version=9;")
                .map_err(|e| format!("Migration v9 error (version): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v9 commit error: {e}"))?;
        }

        if version < 10 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v10 transaction error: {e}"))?;
            if !self.column_exists("radio_stations", "codec")? {
                tx.execute_batch(
                    "ALTER TABLE radio_stations ADD COLUMN codec TEXT NOT NULL DEFAULT '';",
                )
                .map_err(|e| format!("Migration v10 error (codec): {e}"))?;
            }
            if !self.column_exists("radio_stations", "bitrate_kbps")? {
                tx.execute_batch(
                    "ALTER TABLE radio_stations ADD COLUMN bitrate_kbps INTEGER NOT NULL DEFAULT 0;",
                )
                .map_err(|e| format!("Migration v10 error (bitrate): {e}"))?;
            }
            tx.execute_batch("PRAGMA user_version=10;")
                .map_err(|e| format!("Migration v10 error (version): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v10 commit error: {e}"))?;
        }

        if version < 11 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v11 transaction error: {e}"))?;
            for (column, definition) in [
                ("release_type", "TEXT NOT NULL DEFAULT ''"),
                ("added_at", "INTEGER NOT NULL DEFAULT 0"),
                ("favorited_at", "INTEGER NOT NULL DEFAULT 0"),
            ] {
                if !self.column_exists("tracks", column)? {
                    tx.execute_batch(&format!(
                        "ALTER TABLE tracks ADD COLUMN {column} {definition};"
                    ))
                    .map_err(|e| format!("Migration v11 error ({column}): {e}"))?;
                }
            }
            // The file's modification time stands in for the date a known
            // track was added; the rescan below fills in the rest.
            tx.execute_batch(
                "UPDATE tracks SET added_at = mtime WHERE added_at = 0 AND mtime > 0;
                 UPDATE tracks SET mtime = 0 WHERE provider = 'local';
                 PRAGMA user_version=11;",
            )
            .map_err(|e| format!("Migration v11 error (backfill): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v11 commit error: {e}"))?;
        }

        if version < 12 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v12 transaction error: {e}"))?;
            // Persisted grid thumbnails: `image_data` holds the encoded
            // thumbnail, the rest says what it was made from.
            for (column, definition) in [
                ("width", "INTEGER"),
                ("height", "INTEGER"),
                ("fingerprint", "INTEGER"),
                ("src_path", "TEXT"),
                ("src_mtime_ns", "INTEGER"),
                ("src_size", "INTEGER"),
                ("max_dim", "INTEGER"),
            ] {
                if !self.column_exists("cover_cache", column)? {
                    tx.execute_batch(&format!(
                        "ALTER TABLE cover_cache ADD COLUMN {column} {definition};"
                    ))
                    .map_err(|e| format!("Migration v12 error ({column}): {e}"))?;
                }
            }
            tx.execute_batch("PRAGMA user_version=12;")
                .map_err(|e| format!("Migration v12 error (version): {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v12 commit error: {e}"))?;
        }

        if version < 13 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v13 transaction error: {e}"))?;
            // A remote provider's library is fetched over the network, so
            // it is cached here (scoped by `provider_id`, unlike
            // `cover_cache`, because its album keys are not: two providers
            // can share an artist/album pair) to paint instantly on the
            // next provider switch or app restart while a fresh sync runs.
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS remote_cover_cache (
                    provider_id TEXT NOT NULL,
                    album_key   TEXT NOT NULL,
                    image_data  BLOB NOT NULL,
                    width       INTEGER NOT NULL,
                    height      INTEGER NOT NULL,
                    fingerprint INTEGER NOT NULL,
                    PRIMARY KEY (provider_id, album_key)
                );
                PRAGMA user_version=13;",
            )
            .map_err(|e| format!("Migration v13 error: {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v13 commit error: {e}"))?;
        }

        if version < 14 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v14 transaction error: {e}"))?;
            // The cascade from `tracks` looks up `playlist_tracks` by
            // `track_id`; without this index every deleted track scans it.
            tx.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_playlist_tracks_track
                     ON playlist_tracks(track_id);
                 PRAGMA user_version=14;",
            )
            .map_err(|e| format!("Migration v14 error: {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v14 commit error: {e}"))?;
        }

        if version < 15 {
            let tx = self
                .conn
                .unchecked_transaction()
                .map_err(|e| format!("Migration v15 transaction error: {e}"))?;
            // The server-side library marker (e.g. last scan time) a remote
            // provider's cached library was synced at, so an unchanged
            // server skips the full resync.
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS remote_sync_state (
                    provider_id TEXT PRIMARY KEY,
                    marker      INTEGER NOT NULL
                );
                PRAGMA user_version=15;",
            )
            .map_err(|e| format!("Migration v15 error: {e}"))?;
            tx.commit()
                .map_err(|e| format!("Migration v15 commit error: {e}"))?;
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
    /// Open an explicit transaction so a run of [`Self::upsert_track`]
    /// calls commits once instead of once per row. Pair with
    /// [`Self::commit_batch`].
    pub fn begin_batch(&self) -> Result<(), String> {
        self.conn
            .execute_batch("BEGIN")
            .map_err(|e| format!("begin batch: {e}"))
    }

    /// Commit the transaction opened by [`Self::begin_batch`].
    pub fn commit_batch(&self) -> Result<(), String> {
        self.conn
            .execute_batch("COMMIT")
            .map_err(|e| format!("commit batch: {e}"))
    }

    #[tracing::instrument(skip(self, track), level = "debug")]
    pub fn upsert_track(&self, track: &Track, mtime: i64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO tracks (path, title, artist, album_artist, album, genre,
                 track_number, disc_number, year, duration_ms, bitrate, sample_rate, mtime,
                 provider, provider_track_id, bit_depth, release_type, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18)
                 ON CONFLICT(path) DO UPDATE SET
                    release_type=excluded.release_type,
                    added_at=CASE WHEN tracks.added_at = 0 THEN excluded.mtime
                                  ELSE tracks.added_at END,
                    title=excluded.title, artist=excluded.artist,
                    album_artist=excluded.album_artist, album=excluded.album,
                    genre=excluded.genre, track_number=excluded.track_number,
                    disc_number=excluded.disc_number, year=excluded.year,
                    duration_ms=excluded.duration_ms, bitrate=excluded.bitrate,
                    sample_rate=excluded.sample_rate, mtime=excluded.mtime,
                    provider=excluded.provider, provider_track_id=excluded.provider_track_id,
                    bit_depth=excluded.bit_depth",
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
                    track.bit_depth,
                    track.release_type,
                    if track.added_at > 0 {
                        track.added_at
                    } else {
                        crate::unix_now()
                    },
                ],
            )
            .map_err(|e| format!("Upsert error: {e}"))?;
        Ok(())
    }

    /// Bulk-upserts a remote provider's tracks in one transaction, used to
    /// cache its library in the shared database so a provider switch or app
    /// restart can show it immediately instead of starting from a blank
    /// screen while the (possibly slow) network resync runs. Unlike
    /// [`Self::upsert_track`] (kept for the local scanner, where favorites
    /// and ratings are app-only state that must survive a retag) this also
    /// syncs favorite, rating and ReplayGain from the provider, which is
    /// their source of truth for a remote library.
    pub fn upsert_remote_tracks<T: Borrow<Track>>(&self, tracks: &[T]) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Upsert transaction error: {e}"))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO tracks (path, title, artist, album_artist, album, genre,
                     track_number, disc_number, year, duration_ms, bitrate, sample_rate, mtime,
                     provider, provider_track_id, bit_depth, release_type, added_at,
                     is_favorite, rating, rg_track_gain, rg_album_gain, favorited_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                             ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
                     ON CONFLICT(path) DO UPDATE SET
                        title=excluded.title, artist=excluded.artist,
                        album_artist=excluded.album_artist, album=excluded.album,
                        genre=excluded.genre, track_number=excluded.track_number,
                        disc_number=excluded.disc_number, year=excluded.year,
                        duration_ms=excluded.duration_ms, bitrate=excluded.bitrate,
                        sample_rate=excluded.sample_rate, mtime=excluded.mtime,
                        provider=excluded.provider, provider_track_id=excluded.provider_track_id,
                        bit_depth=excluded.bit_depth, release_type=excluded.release_type,
                        added_at=excluded.added_at, is_favorite=excluded.is_favorite,
                        rating=excluded.rating, rg_track_gain=excluded.rg_track_gain,
                        rg_album_gain=excluded.rg_album_gain, favorited_at=excluded.favorited_at",
                )
                .map_err(|e| format!("Upsert error: {e}"))?;
            for track in tracks.iter().map(Borrow::borrow) {
                stmt.execute(params![
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
                    0i64,
                    &*track.provider_id,
                    track.source_uri,
                    track.bit_depth,
                    track.release_type,
                    if track.added_at > 0 {
                        track.added_at
                    } else {
                        crate::unix_now()
                    },
                    track.is_favorite,
                    track.rating,
                    track.rg_track_gain,
                    track.rg_album_gain,
                    track.favorited_at,
                ])
                .map_err(|e| format!("Upsert error: {e}"))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("Upsert commit error: {e}"))?;
        Ok(())
    }

    /// Deletes `provider`'s tracks whose path is not in `keep` (removed
    /// from the remote library since the last sync).
    pub fn prune_provider_tracks(
        &self,
        provider: &str,
        keep: &HashSet<String>,
    ) -> Result<usize, String> {
        let existing: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT path FROM tracks WHERE provider = ?1")
                .map_err(|e| format!("Prune query error: {e}"))?;
            stmt.query_map(params![provider], |row| row.get(0))
                .map_err(|e| format!("Prune query error: {e}"))?
                .collect::<Result<_, _>>()
                .map_err(|e| format!("Prune row error: {e}"))?
        };
        let stale: Vec<&String> = existing.iter().filter(|p| !keep.contains(*p)).collect();
        if stale.is_empty() {
            return Ok(0);
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Prune transaction error: {e}"))?;
        let mut removed = 0;
        {
            let mut delete = tx
                .prepare("DELETE FROM tracks WHERE path = ?1")
                .map_err(|e| format!("Prune error: {e}"))?;
            for path in &stale {
                removed += delete
                    .execute(params![path])
                    .map_err(|e| format!("Prune error: {e}"))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("Prune commit error: {e}"))?;
        Ok(removed)
    }

    /// Remove missing local tracks beneath successfully scanned roots.
    #[tracing::instrument(skip(self, roots), level = "debug")]
    pub fn remove_missing_tracks(&self, roots: &[PathBuf]) -> Result<usize, String> {
        self.remove_local_tracks_beneath(roots, |path| match std::fs::metadata(path) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => {
                tracing::warn!("Skipping inaccessible track {}: {error}", path.display());
                false
            }
        })
    }

    /// Remove the local tracks beneath `roots` for which `is_missing` holds.
    /// The scanners' (native and browser) way to drop vanished files; they
    /// differ only in how a file's absence is known.
    pub fn remove_local_tracks_beneath(
        &self,
        roots: &[PathBuf],
        is_missing: impl Fn(&Path) -> bool,
    ) -> Result<usize, String> {
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
                is_missing(&path).then_some(id)
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
        self.set_sync_marker(provider_id, None)?;
        let count = self
            .conn
            .execute(
                "DELETE FROM tracks WHERE provider = ?1",
                params![provider_id],
            )
            .map_err(|e| format!("Delete error: {e}"))?;
        Ok(count)
    }

    /// The server library marker `provider_id`'s cache was last synced at.
    pub fn sync_marker(&self, provider_id: &str) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT marker FROM remote_sync_state WHERE provider_id = ?1",
                params![provider_id],
                |row| row.get(0),
            )
            .ok()
    }

    /// Record (or with `None` forget) the server library marker of a sync.
    pub fn set_sync_marker(&self, provider_id: &str, marker: Option<i64>) -> Result<(), String> {
        let result = match marker {
            Some(marker) => self.conn.execute(
                "INSERT INTO remote_sync_state (provider_id, marker) VALUES (?1, ?2)
                 ON CONFLICT(provider_id) DO UPDATE SET marker = excluded.marker",
                params![provider_id, marker],
            ),
            None => self.conn.execute(
                "DELETE FROM remote_sync_state WHERE provider_id = ?1",
                params![provider_id],
            ),
        };
        result
            .map(|_| ())
            .map_err(|e| format!("Sync marker error: {e}"))
    }

    /// Set the favorite flag of cached remote tracks, keyed by `path`.
    pub fn set_cached_favorites(
        &self,
        provider_id: &str,
        changes: &[(String, bool)],
    ) -> Result<(), String> {
        let now = crate::unix_now();
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Favorites transaction error: {e}"))?;
        {
            let mut stmt = tx
                .prepare(
                    "UPDATE tracks SET is_favorite = ?1,
                        favorited_at = CASE WHEN ?1 = 1 THEN ?2 ELSE 0 END
                     WHERE provider = ?3 AND path = ?4",
                )
                .map_err(|e| format!("Favorites error: {e}"))?;
            for (path, state) in changes {
                stmt.execute(params![state, now, provider_id, path])
                    .map_err(|e| format!("Favorites error: {e}"))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("Favorites commit error: {e}"))
    }

    /// Get all tracks, optionally filtered by provider, ordered by album then track number.
    #[tracing::instrument(skip(self), level = "debug")]
    pub fn all_tracks(&self, provider: Option<&str>) -> Result<Vec<Track>, String> {
        let (sql, param): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match provider {
            Some(p) => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, bit_depth, release_type, added_at, favorited_at
                 FROM tracks
                 WHERE provider = ?1
                 ORDER BY album_artist, album, disc_number, track_number"
                    .to_string(),
                vec![Box::new(p.to_string())],
            ),
            None => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, bit_depth, release_type, added_at, favorited_at
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

        let mut tracks: Vec<Track> = stmt
            .query_map(params_ref.as_slice(), Self::row_to_track)
            .map_err(|e| format!("Query error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();

        super::compilations::group_compilations(&mut tracks);

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
                albums[idx].tracks.push(Arc::new(track));
            } else {
                let idx = albums.len();
                index.insert(key, idx);
                let mut album = Album::new(
                    track.album.clone(),
                    track.album_artist.clone(),
                    track.year,
                    Vec::new(),
                    None,
                );
                album.tracks.push(Arc::new(track));
                albums.push(album);
            }
        }
        // Tracks were appended after construction: compute the cached
        // quality / duration / cover key once per album, not per frame.
        for album in &mut albums {
            album.refresh_derived();
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
                artists[idx].push_album(Arc::new(album));
            } else {
                let idx = artists.len();
                let name = album.artist.clone();
                index.insert(name.clone(), idx);
                artists.push(Artist::new(name, vec![Arc::new(album)]));
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

    /// Every persisted grid thumbnail, by album key. Rows without a source
    /// stamp (full-size covers cached by older code) are skipped.
    pub fn cached_thumbs(&self) -> Result<HashMap<String, CachedThumb>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT album_key, image_data, width, height, fingerprint,
                        src_path, src_mtime_ns, src_size, max_dim
                 FROM cover_cache
                 WHERE src_path IS NOT NULL
                   AND src_mtime_ns IS NOT NULL AND src_size IS NOT NULL
                   AND max_dim IS NOT NULL",
            )
            .map_err(|e| format!("Cover cache error: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                let key: String = row.get(0)?;
                // A row without image data records an album with no cover.
                let data: Option<Vec<u8>> = row.get(1)?;
                let thumb = match (
                    data,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ) {
                    (Some(data), Some(width), Some(height), Some(fingerprint)) => {
                        Some(CoverThumb {
                            width,
                            height,
                            encoded: data.into(),
                            fingerprint: fingerprint as u64,
                        })
                    }
                    _ => None,
                };
                let stamp = ThumbStamp {
                    path: row.get(5)?,
                    mtime_ns: row.get(6)?,
                    size: row.get::<_, i64>(7)? as u64,
                    max_dim: row.get(8)?,
                };
                Ok((key, CachedThumb { thumb, stamp }))
            })
            .map_err(|e| format!("Cover cache error: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("Cover cache error: {e}"))
    }

    /// Stores `entries` and drops every other persisted thumbnail whose
    /// album key is not in `keep` (albums that left the library), in one
    /// transaction.
    pub fn store_thumbs(
        &self,
        entries: &[(String, CachedThumb)],
        keep: &HashSet<String>,
    ) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Cover cache transaction error: {e}"))?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT OR REPLACE INTO cover_cache
                        (album_key, image_data, width, height, fingerprint,
                         src_path, src_mtime_ns, src_size, max_dim)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .map_err(|e| format!("Cover cache error: {e}"))?;
            for (key, cached) in entries {
                insert
                    .execute(params![
                        key,
                        cached.thumb.as_ref().map(|t| t.encoded.as_ref()),
                        cached.thumb.as_ref().map(|t| t.width),
                        cached.thumb.as_ref().map(|t| t.height),
                        cached.thumb.as_ref().map(|t| t.fingerprint as i64),
                        cached.stamp.path,
                        cached.stamp.mtime_ns,
                        cached.stamp.size as i64,
                        cached.stamp.max_dim,
                    ])
                    .map_err(|e| format!("Cover cache error: {e}"))?;
            }
            let stale: Vec<String> = tx
                .prepare("SELECT album_key FROM cover_cache WHERE src_path IS NOT NULL")
                .and_then(|mut q| q.query_map([], |row| row.get(0))?.collect())
                .map_err(|e| format!("Cover cache error: {e}"))?;
            let mut delete = tx
                .prepare("DELETE FROM cover_cache WHERE album_key = ?1")
                .map_err(|e| format!("Cover cache error: {e}"))?;
            for key in stale.iter().filter(|k| !keep.contains(*k)) {
                delete
                    .execute(params![key])
                    .map_err(|e| format!("Cover cache error: {e}"))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("Cover cache commit error: {e}"))
    }

    /// Forget the cached cover of one album, so the next scan re-reads it
    /// from the (retagged) files.
    pub fn remove_cached_cover(&self, album_key: &str) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM cover_cache WHERE album_key = ?1",
                params![album_key],
            )
            .map_err(|e| format!("Cover cache error: {e}"))?;
        Ok(())
    }

    /// Every persisted cover thumbnail of a remote provider's library, by
    /// album key. Unlike [`Self::cached_thumbs`] there is no file stamp to
    /// validate: a remote cover is only ever replaced by a fresh network
    /// fetch, never by comparing against a local file.
    pub fn cached_remote_thumbs(
        &self,
        provider_id: &str,
    ) -> Result<HashMap<String, CoverThumb>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT album_key, image_data, width, height, fingerprint
                 FROM remote_cover_cache WHERE provider_id = ?1",
            )
            .map_err(|e| format!("Remote cover cache error: {e}"))?;
        let rows = stmt
            .query_map(params![provider_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    CoverThumb {
                        encoded: row.get::<_, Vec<u8>>(1)?.into(),
                        width: row.get(2)?,
                        height: row.get(3)?,
                        fingerprint: row.get::<_, i64>(4)? as u64,
                    },
                ))
            })
            .map_err(|e| format!("Remote cover cache error: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("Remote cover cache error: {e}"))
    }

    /// Upserts (never deletes) a remote provider's cover thumbnails, one
    /// transaction per call; see [`Self::prune_remote_thumbs`] for dropping
    /// the ones that left the library.
    pub fn upsert_remote_thumbs(
        &self,
        provider_id: &str,
        entries: &[(String, CoverThumb)],
    ) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Remote cover cache transaction error: {e}"))?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT OR REPLACE INTO remote_cover_cache
                        (provider_id, album_key, image_data, width, height, fingerprint)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(|e| format!("Remote cover cache error: {e}"))?;
            for (key, thumb) in entries {
                insert
                    .execute(params![
                        provider_id,
                        key,
                        thumb.encoded.as_ref(),
                        thumb.width,
                        thumb.height,
                        thumb.fingerprint as i64,
                    ])
                    .map_err(|e| format!("Remote cover cache error: {e}"))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("Remote cover cache commit error: {e}"))
    }

    /// Deletes a remote provider's cached thumbnails whose album key is not
    /// in `keep` (left the library since the last sync, or the whole
    /// provider was removed, with `keep` empty).
    pub fn prune_remote_thumbs(
        &self,
        provider_id: &str,
        keep: &HashSet<String>,
    ) -> Result<usize, String> {
        let existing: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT album_key FROM remote_cover_cache WHERE provider_id = ?1")
                .map_err(|e| format!("Remote cover cache prune query error: {e}"))?;
            stmt.query_map(params![provider_id], |row| row.get(0))
                .map_err(|e| format!("Remote cover cache prune query error: {e}"))?
                .collect::<Result<_, _>>()
                .map_err(|e| format!("Remote cover cache prune row error: {e}"))?
        };
        let stale: Vec<&String> = existing.iter().filter(|k| !keep.contains(*k)).collect();
        if stale.is_empty() {
            return Ok(0);
        }
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Remote cover cache prune transaction error: {e}"))?;
        let mut removed = 0;
        {
            let mut delete = tx
                .prepare("DELETE FROM remote_cover_cache WHERE provider_id = ?1 AND album_key = ?2")
                .map_err(|e| format!("Remote cover cache prune error: {e}"))?;
            for key in &stale {
                removed += delete
                    .execute(params![provider_id, key])
                    .map_err(|e| format!("Remote cover cache prune error: {e}"))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("Remote cover cache prune commit error: {e}"))?;
        Ok(removed)
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

    /// Remove the local tracks beneath directory `dir` (a deleted or moved
    /// folder); returns how many were removed.
    pub fn remove_local_tracks_under(&self, dir: &Path) -> Result<usize, String> {
        let mut prefix = dir.to_string_lossy().trim_end_matches('/').to_string();
        prefix = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        self.conn
            .execute(
                "DELETE FROM tracks WHERE provider = 'local' AND path LIKE ?1 ESCAPE '\\'",
                params![format!("{prefix}/%")],
            )
            .map_err(|e| format!("Delete under dir error: {e}"))
    }

    /// The stored track at `path`, if any.
    pub fn track_by_path(&self, path: &str) -> Result<Option<Track>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, bit_depth, release_type, added_at, favorited_at
                 FROM tracks WHERE path = ?1",
            )
            .map_err(|e| format!("Query error: {e}"))?;
        let mut rows = stmt
            .query_map(params![path], Self::row_to_track)
            .map_err(|e| format!("Query error: {e}"))?;
        rows.next()
            .transpose()
            .map_err(|e| format!("Query row error: {e}"))
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
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, bit_depth, release_type, added_at, favorited_at
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
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, bit_depth, release_type, added_at, favorited_at
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
                        t.provider, t.provider_track_id, t.is_favorite, t.rating, t.rg_track_gain, t.rg_album_gain, t.bit_depth, t.release_type, t.added_at, t.favorited_at
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

    /// Removes the track at 0-based `index` of the playlist (rows after it
    /// shift up). The same track can appear several times, so entries are
    /// addressed by position, not by track.
    pub fn remove_playlist_track(&self, playlist_id: &str, index: usize) -> Result<(), String> {
        self.edit_playlist_order(playlist_id, |rows| {
            if index >= rows.len() {
                return Err(format!("No track at position {index}"));
            }
            rows.remove(index);
            Ok(())
        })
    }

    /// Moves the track at `from` so it sits at `to` (both 0-based, `to` is
    /// the final index).
    pub fn move_playlist_track(
        &self,
        playlist_id: &str,
        from: usize,
        to: usize,
    ) -> Result<(), String> {
        self.edit_playlist_order(playlist_id, |rows| {
            if from >= rows.len() || to >= rows.len() {
                return Err(format!("Cannot move {from} to {to}"));
            }
            let row = rows.remove(from);
            rows.insert(to, row);
            Ok(())
        })
    }

    /// Reads the playlist's row ids in order, lets `edit` rearrange or drop
    /// them, then rewrites positions densely (0..n) in one transaction.
    fn edit_playlist_order(
        &self,
        playlist_id: &str,
        edit: impl FnOnce(&mut Vec<i64>) -> Result<(), String>,
    ) -> Result<(), String> {
        let pid: i64 = playlist_id
            .parse()
            .map_err(|_| "Invalid playlist ID".to_string())?;
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Playlist edit transaction error: {e}"))?;
        let mut rows: Vec<i64> = {
            let mut stmt = tx
                .prepare(
                    "SELECT rowid FROM playlist_tracks WHERE playlist_id = ?1
                     ORDER BY position, rowid",
                )
                .map_err(|e| format!("Playlist edit error: {e}"))?;
            stmt.query_map(params![pid], |row| row.get(0))
                .map_err(|e| format!("Playlist edit error: {e}"))?
                .collect::<Result<_, _>>()
                .map_err(|e| format!("Playlist edit error: {e}"))?
        };
        let before = rows.clone();
        edit(&mut rows)?;
        for gone in before.iter().filter(|id| !rows.contains(id)) {
            tx.execute(
                "DELETE FROM playlist_tracks WHERE rowid = ?1",
                params![gone],
            )
            .map_err(|e| format!("Playlist edit error: {e}"))?;
        }
        for (position, rowid) in rows.iter().enumerate() {
            tx.execute(
                "UPDATE playlist_tracks SET position = ?1 WHERE rowid = ?2",
                params![position as i64, rowid],
            )
            .map_err(|e| format!("Playlist edit error: {e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("Playlist edit commit error: {e}"))
    }

    // ---- Favorites and Ratings ----

    /// Toggle the favorite status of a track. Returns the new status.
    pub fn toggle_favorite(&self, track_id: i64) -> Result<bool, String> {
        self.conn
            .execute(
                "UPDATE tracks SET
                    favorited_at = CASE WHEN is_favorite = 0 THEN ?2 ELSE 0 END,
                    is_favorite = CASE WHEN is_favorite = 0 THEN 1 ELSE 0 END
                 WHERE id = ?1",
                params![track_id, crate::unix_now()],
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
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, bit_depth, release_type, added_at, favorited_at
                 FROM tracks
                 WHERE provider = ?1 AND is_favorite = 1
                 ORDER BY title"
                    .to_string(),
                vec![Box::new(p.to_string())],
            ),
            None => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, bit_depth, release_type, added_at, favorited_at
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
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, bit_depth, release_type, added_at, favorited_at
                 FROM tracks
                 WHERE provider = ?1 AND genre = ?2
                 ORDER BY album_artist, album, disc_number, track_number"
                    .to_string(),
                vec![Box::new(p.to_string()), Box::new(genre.to_string())],
            ),
            None => (
                "SELECT id, path, title, artist, album_artist, album, genre,
                        track_number, disc_number, year, duration_ms, bitrate, sample_rate,
                        provider, provider_track_id, is_favorite, rating, rg_track_gain, rg_album_gain, bit_depth, release_type, added_at, favorited_at
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
                        .smart_playlist_match_count(&playlist)
                        .map(|n| playlist.limit.map_or(n, |l| n.min(l as usize)))
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

    /// How many tracks a smart playlist's rules match, ignoring its row
    /// limit: the "N songs match" of the editor, and the total a limited
    /// playlist is cut from.
    pub fn smart_playlist_match_count(&self, playlist: &SmartPlaylist) -> Result<usize, String> {
        let (sql, param_values) = playlist.to_count_sql();
        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|v| v.as_ref()).collect();
        let count: i64 = self
            .conn
            .query_row(&sql, params_ref.as_slice(), |row| row.get(0))
            .map_err(|e| format!("Smart playlist count error: {e}"))?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Map a database row to a Track struct.
    pub(super) fn row_to_track(row: &rusqlite::Row<'_>) -> rusqlite::Result<Track> {
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
            bit_depth: row.get::<_, u32>(19).unwrap_or(0),
            provider_id: Arc::from(row.get::<_, String>(13)?),
            source_uri: row.get(14)?,
            is_favorite: row.get::<_, i32>(15).unwrap_or(0) != 0,
            rating: row.get::<_, Option<u8>>(16).unwrap_or(None),
            rg_track_gain: row.get::<_, Option<f32>>(17).unwrap_or(None),
            rg_album_gain: row.get::<_, Option<f32>>(18).unwrap_or(None),
            release_type: row.get::<_, String>(20).unwrap_or_default(),
            added_at: row.get::<_, i64>(21).unwrap_or(0),
            favorited_at: row.get::<_, i64>(22).unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use web_time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aulos-db-{label}-{}-{}",
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

        let track_id = insert_local(&db, Path::new("/tmp/aulos-cascade.mp3"));
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
        assert!(db.column_exists("tracks", "bit_depth").unwrap());
        assert!(db.column_exists("tracks", "release_type").unwrap());
        assert!(db.column_exists("tracks", "added_at").unwrap());
        assert!(db.column_exists("tracks", "favorited_at").unwrap());
        assert!(db.column_exists("cover_cache", "src_mtime_ns").unwrap());
        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            15
        );
        assert!(
            db.conn
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name='play_history'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .is_ok()
        );
    }

    #[test]
    fn history_migration_upgrades_a_v7_database_without_data_loss() {
        let conn = Connection::open_in_memory().unwrap();
        LibraryDb::configure_connection(&conn).unwrap();
        conn.execute_batch(SCHEMA_BASE).unwrap();
        let db = LibraryDb { conn };
        db.run_migration().unwrap();
        // Rewind to a pre-history (v7) database that already holds a track.
        db.conn
            .execute_batch("DROP TABLE play_history; PRAGMA user_version=7;")
            .unwrap();
        let track_id = insert_local(&db, Path::new("/tmp/aulos-v7.mp3"));

        db.run_migration().unwrap();
        db.run_migration().unwrap();

        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            15
        );
        assert_eq!(db.track_count(None), 1);
        db.record_play(track_id, 1_700_000_000).unwrap();
        assert_eq!(db.play_count(track_id).unwrap(), 1);
    }

    fn playlist_titles(db: &LibraryDb, id: &str) -> Vec<String> {
        db.get_playlist(id)
            .unwrap()
            .tracks
            .into_iter()
            .map(|t| t.path.to_string_lossy().into_owned())
            .collect()
    }

    fn playlist_of(db: &LibraryDb, names: &[&str]) -> String {
        let ids: Vec<String> = names
            .iter()
            .map(|n| insert_local(db, Path::new(&format!("/tmp/{n}.mp3"))).to_string())
            .collect();
        let playlist = db.create_playlist("p").unwrap();
        db.add_to_playlist(&playlist.id, &ids).unwrap();
        playlist.id
    }

    #[test]
    fn playlist_track_removal_keeps_order_and_positions_dense() {
        let db = LibraryDb::open_memory().unwrap();
        let id = playlist_of(&db, &["a", "b", "c"]);
        db.remove_playlist_track(&id, 1).unwrap();
        assert_eq!(playlist_titles(&db, &id), ["/tmp/a.mp3", "/tmp/c.mp3"]);
        let positions: Vec<i64> = db
            .conn
            .prepare("SELECT position FROM playlist_tracks ORDER BY position")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(positions, [0, 1]);
        assert!(db.remove_playlist_track(&id, 5).is_err());
    }

    #[test]
    fn playlist_track_removal_targets_one_duplicate() {
        let db = LibraryDb::open_memory().unwrap();
        let id = playlist_of(&db, &["a", "b"]);
        let first: i64 = db
            .conn
            .query_row("SELECT track_id FROM playlist_tracks LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        db.add_to_playlist(&id, &[first.to_string()]).unwrap();
        db.remove_playlist_track(&id, 2).unwrap();
        assert_eq!(playlist_titles(&db, &id), ["/tmp/a.mp3", "/tmp/b.mp3"]);
    }

    #[test]
    fn playlist_track_move_reorders_in_both_directions() {
        let db = LibraryDb::open_memory().unwrap();
        let id = playlist_of(&db, &["a", "b", "c", "d"]);
        db.move_playlist_track(&id, 0, 2).unwrap();
        assert_eq!(
            playlist_titles(&db, &id),
            ["/tmp/b.mp3", "/tmp/c.mp3", "/tmp/a.mp3", "/tmp/d.mp3"]
        );
        db.move_playlist_track(&id, 3, 0).unwrap();
        assert_eq!(
            playlist_titles(&db, &id),
            ["/tmp/d.mp3", "/tmp/b.mp3", "/tmp/c.mp3", "/tmp/a.mp3"]
        );
        assert!(db.move_playlist_track(&id, 0, 4).is_err());
    }

    fn cached(path: &str, mtime_ns: i64) -> CachedThumb {
        CachedThumb {
            thumb: Some(CoverThumb {
                width: 2,
                height: 3,
                encoded: vec![1, 2, 3].into(),
                fingerprint: u64::MAX - 5,
            }),
            stamp: ThumbStamp {
                path: path.into(),
                mtime_ns,
                size: 77,
                max_dim: 256,
            },
        }
    }

    #[test]
    fn persisted_thumbs_round_trip_and_albums_that_left_are_dropped() {
        let db = LibraryDb::open_memory().unwrap();
        // A full-size cover cached by older code has no stamp: ignored.
        db.cache_cover("legacy", b"full-size").unwrap();
        db.store_thumbs(
            &[
                ("a".into(), cached("/m/a.flac", -4)),
                ("b".into(), cached("/m/b.flac", 9)),
            ],
            &HashSet::from(["a".to_string(), "b".to_string(), "legacy".to_string()]),
        )
        .unwrap();
        let loaded = db.cached_thumbs().unwrap();
        assert_eq!(loaded.len(), 2);
        let a = &loaded["a"];
        let thumb = a.thumb.as_ref().unwrap();
        assert_eq!(thumb.fingerprint, u64::MAX - 5);
        assert_eq!((thumb.width, thumb.height), (2, 3));
        assert_eq!(thumb.encoded.as_ref(), [1, 2, 3]);
        assert_eq!(a.stamp, cached("/m/a.flac", -4).stamp);

        // `b` left the library; `a` is refreshed.
        db.store_thumbs(
            &[("a".into(), cached("/m/a2.flac", 1))],
            &HashSet::from(["a".to_string()]),
        )
        .unwrap();
        let loaded = db.cached_thumbs().unwrap();
        assert_eq!(loaded.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(loaded["a"].stamp.path, "/m/a2.flac");
        // The legacy row has no stamp, so it is not touched.
        assert!(db.get_cached_cover("legacy").is_some());

        db.remove_cached_cover("a").unwrap();
        assert!(db.cached_thumbs().unwrap().is_empty());
    }

    #[test]
    fn persisted_coverless_albums_round_trip() {
        let db = LibraryDb::open_memory().unwrap();
        let mut none = cached("/m/x.flac", 5);
        none.thumb = None;
        db.store_thumbs(&[("x".into(), none)], &HashSet::from(["x".to_string()]))
            .unwrap();
        let loaded = db.cached_thumbs().unwrap();
        assert!(loaded["x"].thumb.is_none());
        assert_eq!(loaded["x"].stamp.mtime_ns, 5);
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
            bit_depth: 0,
            release_type: String::new(),
            added_at: 0,
            favorited_at: 0,
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

    #[test]
    fn removing_a_folder_drops_only_the_tracks_beneath_it() {
        let db = LibraryDb::open_memory().unwrap();
        for p in [
            "/m/a_b/1.flac",
            "/m/a_b/sub/2.flac",
            "/m/axb/3.flac",
            "/m/a_b.flac",
        ] {
            insert_local(&db, Path::new(p));
        }
        assert_eq!(
            db.remove_local_tracks_under(Path::new("/m/a_b")).unwrap(),
            2
        );
        assert_eq!(db.all_tracks(None).unwrap().len(), 2);
    }

    #[test]
    fn upsert_stores_release_type_and_stamps_added_at_once() {
        let db = LibraryDb::open_memory().unwrap();
        // A row from before added_at existed (0): the file's mtime stands in.
        insert_local(&db, Path::new("/music/old.flac"));
        let mut track = db.all_tracks(None).unwrap().remove(0);
        assert_eq!((track.added_at, track.release_type.as_str()), (0, ""));
        track.release_type = "live".into();
        db.upsert_track(&track, 777).unwrap();
        let track = db.all_tracks(None).unwrap().remove(0);
        assert_eq!((track.added_at, track.release_type.as_str()), (777, "live"));

        // Later rescans keep the date and refresh the tag.
        let mut again = track.clone();
        again.release_type = "ep".into();
        db.upsert_track(&again, 999).unwrap();
        let track = db.all_tracks(None).unwrap().remove(0);
        assert_eq!((track.added_at, track.release_type.as_str()), (777, "ep"));

        // A new file is stamped with the time it was added.
        let mut fresh = track.clone();
        fresh.path = PathBuf::from("/music/new.flac");
        fresh.source_uri = "/music/new.flac".into();
        fresh.added_at = 0;
        let before = crate::unix_now();
        db.upsert_track(&fresh, 5).unwrap();
        let added = db
            .all_tracks(None)
            .unwrap()
            .into_iter()
            .find(|t| t.path == fresh.path)
            .unwrap()
            .added_at;
        assert!(added >= before, "{added} < {before}");
    }

    fn remote_track(provider: &str, path: &str, is_favorite: bool, rating: Option<u8>) -> Track {
        Track {
            id: 0,
            path: PathBuf::from(path),
            title: path.to_string(),
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
            provider_id: Arc::from(provider),
            source_uri: path.to_string(),
            is_favorite,
            rating,
            rg_track_gain: Some(-6.0),
            rg_album_gain: Some(-5.5),
            bit_depth: 0,
            release_type: String::new(),
            added_at: 1,
            favorited_at: 0,
        }
    }

    #[test]
    fn upsert_remote_tracks_syncs_favorite_rating_and_gain_from_the_provider() {
        let db = LibraryDb::open_memory().unwrap();
        db.upsert_remote_tracks(&[remote_track("nav", "song-1", false, None)])
            .unwrap();
        // The server toggled the favorite and rated the track since the
        // last sync: unlike `upsert_track` (local scanner, which must
        // preserve app-only annotations across a retag), a later sync
        // overwrites them, because the provider is their source of truth.
        db.upsert_remote_tracks(&[remote_track("nav", "song-1", true, Some(4))])
            .unwrap();
        let track = db.all_tracks(Some("nav")).unwrap().remove(0);
        assert!(track.is_favorite);
        assert_eq!(track.rating, Some(4));
        assert_eq!(track.rg_track_gain, Some(-6.0));
    }

    #[test]
    fn prune_provider_tracks_only_drops_that_providers_missing_rows() {
        let db = LibraryDb::open_memory().unwrap();
        insert_local(&db, Path::new("/music/local.flac"));
        db.upsert_remote_tracks(&[
            remote_track("nav", "song-1", false, None),
            remote_track("nav", "song-2", false, None),
        ])
        .unwrap();

        // "song-2" left the server; "song-1" is still there.
        let keep = HashSet::from(["song-1".to_string()]);
        let removed = db.prune_provider_tracks("nav", &keep).unwrap();
        assert_eq!(removed, 1);

        let remaining: Vec<String> = db
            .all_tracks(Some("nav"))
            .unwrap()
            .into_iter()
            .map(|t| t.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining, ["song-1"]);
        // The local provider's own track is untouched.
        assert_eq!(db.all_tracks(Some("local")).unwrap().len(), 1);
    }

    #[test]
    fn remote_cover_cache_is_scoped_per_provider_and_prunes_by_key() {
        let db = LibraryDb::open_memory().unwrap();
        let thumb = |fp: u64| CoverThumb {
            width: 1,
            height: 1,
            encoded: vec![0, 0, 0, 255].into(),
            fingerprint: fp,
        };
        db.upsert_remote_thumbs(
            "nav",
            &[("a".to_string(), thumb(1)), ("b".to_string(), thumb(2))],
        )
        .unwrap();
        // A different provider can use the same album key without clashing.
        db.upsert_remote_thumbs("other", &[("a".to_string(), thumb(9))])
            .unwrap();

        let nav = db.cached_remote_thumbs("nav").unwrap();
        assert_eq!(nav.len(), 2);
        assert_eq!(nav["a"].fingerprint, 1);
        assert_eq!(
            db.cached_remote_thumbs("other").unwrap()["a"].fingerprint,
            9
        );

        // "b" left nav's library; "other"'s row is a different provider and
        // must survive pruning nav with an empty keep set.
        db.prune_remote_thumbs("nav", &HashSet::from(["a".to_string()]))
            .unwrap();
        let nav = db.cached_remote_thumbs("nav").unwrap();
        assert_eq!(nav.len(), 1);
        assert!(nav.contains_key("a"));
        assert_eq!(db.cached_remote_thumbs("other").unwrap().len(), 1);
    }

    #[test]
    fn toggling_a_favourite_stamps_and_clears_favorited_at() {
        let db = LibraryDb::open_memory().unwrap();
        let id = insert_local(&db, Path::new("/music/fav.flac"));
        let before = crate::unix_now();
        assert!(db.toggle_favorite(id).unwrap());
        let track = db.all_tracks(None).unwrap().remove(0);
        assert!(track.is_favorite && track.favorited_at >= before);
        assert!(!db.toggle_favorite(id).unwrap());
        let track = db.all_tracks(None).unwrap().remove(0);
        assert!(!track.is_favorite);
        assert_eq!(track.favorited_at, 0);
    }

    #[test]
    fn remove_cached_cover_drops_only_that_album() {
        let db = LibraryDb::open_memory().unwrap();
        db.cache_cover("a|x", b"one").unwrap();
        db.cache_cover("b|y", b"two").unwrap();
        db.remove_cached_cover("a|x").unwrap();
        assert_eq!(db.get_cached_cover("a|x"), None);
        assert_eq!(db.get_cached_cover("b|y").as_deref(), Some(&b"two"[..]));
        // Removing a key that is not cached is not an error.
        db.remove_cached_cover("zzz").unwrap();
    }

    #[test]
    fn open_runs_schema_once_per_path_and_reinitialises_missing_files() {
        let root = temp_root("open-once");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("lib.db");
        assert!(!schema_is_ready(&path));

        let db = LibraryDb::open(&path).unwrap();
        assert!(schema_is_ready(&path));
        // Drop a table: a second open must NOT re-run the schema script.
        db.conn.execute_batch("DROP TABLE cover_cache;").unwrap();
        drop(db);
        let db = LibraryDb::open(&path).unwrap();
        let has_cover_cache: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'cover_cache'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_cover_cache, 0, "schema must be skipped on reopen");
        drop(db);

        // File removed: the next open initialises it from scratch.
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
        let db = LibraryDb::open(&path).unwrap();
        let tracks: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tracks, 0);
        drop(db);
        std::fs::remove_dir_all(&root).ok();
    }
}
