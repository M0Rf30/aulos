// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Standalone database access for podcast subscriptions and internet radio
//! stations.
//!
//! Opens its own [`rusqlite::Connection`] to the same `library.db` file used
//! by [`crate::library::db::LibraryDb`]. Schema ownership — including the
//! `podcasts`/`podcast_episodes`/`radio_stations` tables and the
//! `PRAGMA user_version` migration that creates them — stays with
//! `LibraryDb`; this type never runs migrations itself, so it must only be
//! opened after `LibraryDb::open` has run at least once.

use rusqlite::{Connection, params};
use std::path::Path;

use super::podcast::{EpisodeMeta, PodcastMeta};

/// A subscribed podcast feed.
#[derive(Debug, Clone)]
pub struct Podcast {
    pub id: i64,
    pub feed_url: String,
    pub title: String,
    pub description: String,
    pub image_url: String,
    pub author: String,
    pub last_refreshed: i64,
    /// Count of episodes with `played = 0`, computed by `list_podcasts`'s
    /// query rather than stored — always in sync, never a separate write
    /// path to keep consistent with `podcast_episodes.played`.
    pub unplayed_count: i64,
}

/// A single episode of a subscribed podcast.
#[derive(Debug, Clone)]
pub struct Episode {
    pub id: i64,
    pub podcast_id: i64,
    pub guid: String,
    pub title: String,
    pub enclosure_url: String,
    pub mime: String,
    pub duration_secs: i64,
    pub pub_date: i64,
    pub description: String,
    pub position_ms: i64,
    pub played: bool,
    pub downloaded_path: String,
}

/// A saved internet radio station.
#[derive(Debug, Clone)]
pub struct RadioStation {
    pub id: i64,
    pub name: String,
    pub stream_url: String,
    pub homepage: String,
    pub favicon_url: String,
    pub tags: String,
}

/// Database access for podcasts and radio stations.
pub struct OnlineStore {
    conn: Connection,
}

impl OnlineStore {
    /// Open a connection to the shared library database. Never runs
    /// migrations — assumes [`crate::library::db::LibraryDb::open`] already
    /// created/migrated the schema.
    pub fn open(db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|e| format!("DB open error: {e}"))?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;",
        )
        .map_err(|e| format!("DB PRAGMA error: {e}"))?;
        Ok(Self { conn })
    }

    // ---- Podcasts ----

    /// Subscribe to a podcast feed, or refresh its metadata if already
    pub fn add_podcast(&self, feed_url: &str, meta: &PodcastMeta) -> Result<i64, String> {
        self.conn
            .execute(
                "INSERT INTO podcasts (feed_url, title, description, image_url, author)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(feed_url) DO UPDATE SET
                    title=excluded.title, description=excluded.description,
                    image_url=excluded.image_url, author=excluded.author",
                params![feed_url, meta.title, meta.description, meta.image_url, meta.author],
            )
            .map_err(|e| format!("Add podcast error: {e}"))?;
        self.conn
            .query_row(
                "SELECT id FROM podcasts WHERE feed_url = ?1",
                params![feed_url],
                |row| row.get(0),
            )
            .map_err(|e| format!("Add podcast lookup error: {e}"))
    }

    pub fn remove_podcast(&self, id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM podcasts WHERE id = ?1", params![id])
            .map_err(|e| format!("Remove podcast error: {e}"))?;
        Ok(())
    }

    pub fn list_podcasts(&self) -> Result<Vec<Podcast>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT p.id, p.feed_url, p.title, p.description, p.image_url, p.author,
                        p.last_refreshed,
                        (SELECT COUNT(*) FROM podcast_episodes e
                         WHERE e.podcast_id = p.id AND e.played = 0) AS unplayed_count
                 FROM podcasts p ORDER BY p.title",
            )
            .map_err(|e| format!("List podcasts error: {e}"))?;
        let podcasts = stmt
            .query_map([], |row| {
                Ok(Podcast {
                    id: row.get(0)?,
                    feed_url: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    image_url: row.get(4)?,
                    author: row.get(5)?,
                    last_refreshed: row.get(6)?,
                    unplayed_count: row.get(7)?,
                })
            })
            .map_err(|e| format!("List podcasts error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(podcasts)
    }

    /// Update a podcast's metadata and last-refreshed timestamp after a
    /// successful feed fetch.
    pub fn touch_podcast_refresh(
        &self,
        id: i64,
        meta: &PodcastMeta,
        refreshed_at: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE podcasts SET title=?2, description=?3, image_url=?4, author=?5, last_refreshed=?6
                 WHERE id=?1",
                params![id, meta.title, meta.description, meta.image_url, meta.author, refreshed_at],
            )
            .map_err(|e| format!("Update podcast error: {e}"))?;
        Ok(())
    }

    /// Insert new episodes and refresh metadata for existing ones (matched
    /// by `(podcast_id, guid)`), without disturbing `position_ms`/`played`
    /// for episodes the user has already started.
    pub fn upsert_episodes(&self, podcast_id: i64, episodes: &[EpisodeMeta]) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Upsert episodes transaction error: {e}"))?;
        {
            let mut insert_stmt = tx
                .prepare(
                    "INSERT OR IGNORE INTO podcast_episodes
                     (podcast_id, guid, title, enclosure_url, mime, duration_secs, pub_date, description)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(|e| format!("Upsert episodes prepare error: {e}"))?;
            let mut update_stmt = tx
                .prepare(
                    "UPDATE podcast_episodes SET
                        title=?3, enclosure_url=?4, mime=?5, duration_secs=?6, pub_date=?7, description=?8
                     WHERE podcast_id=?1 AND guid=?2",
                )
                .map_err(|e| format!("Upsert episodes prepare error: {e}"))?;

            for ep in episodes {
                let args = params![
                    podcast_id,
                    ep.guid,
                    ep.title,
                    ep.enclosure_url,
                    ep.mime,
                    ep.duration_secs,
                    ep.pub_date,
                    ep.description
                ];
                insert_stmt
                    .execute(args)
                    .map_err(|e| format!("Upsert episode insert error: {e}"))?;
                update_stmt
                    .execute(args)
                    .map_err(|e| format!("Upsert episode update error: {e}"))?;
            }
        }
        tx.commit()
            .map_err(|e| format!("Upsert episodes commit error: {e}"))?;
        Ok(())
    }

    /// List a podcast's episodes, newest first.
    pub fn list_episodes(&self, podcast_id: i64) -> Result<Vec<Episode>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, podcast_id, guid, title, enclosure_url, mime, duration_secs, pub_date,
                        description, position_ms, played, downloaded_path
                 FROM podcast_episodes WHERE podcast_id = ?1 ORDER BY pub_date DESC",
            )
            .map_err(|e| format!("List episodes error: {e}"))?;
        let episodes = stmt
            .query_map(params![podcast_id], Self::row_to_episode)
            .map_err(|e| format!("List episodes error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(episodes)
    }

    fn row_to_episode(row: &rusqlite::Row<'_>) -> rusqlite::Result<Episode> {
        Ok(Episode {
            id: row.get(0)?,
            podcast_id: row.get(1)?,
            guid: row.get(2)?,
            title: row.get(3)?,
            enclosure_url: row.get(4)?,
            mime: row.get(5)?,
            duration_secs: row.get(6)?,
            pub_date: row.get(7)?,
            description: row.get(8)?,
            position_ms: row.get(9)?,
            played: row.get::<_, i64>(10)? != 0,
            downloaded_path: row.get(11)?,
        })
    }

    /// Save playback progress for an episode.
    pub fn save_episode_position(
        &self,
        episode_id: i64,
        position_ms: i64,
        played: bool,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE podcast_episodes SET position_ms = ?1, played = ?2 WHERE id = ?3",
                params![position_ms, played as i64, episode_id],
            )
            .map_err(|e| format!("Save episode position error: {e}"))?;
        Ok(())
    }

    /// Set (or, passing `""`, clear) an episode's locally downloaded file
    /// path. One method serves both set and clear — there's no separate
    /// `clear_*` variant.
    pub fn set_episode_downloaded_path(&self, episode_id: i64, path: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE podcast_episodes SET downloaded_path = ?1 WHERE id = ?2",
                params![path, episode_id],
            )
            .map_err(|e| format!("Set episode downloaded path error: {e}"))?;
        Ok(())
    }

    // ---- Radio stations ----

    /// Save a radio station, or update its metadata if the stream URL is
    /// already saved (the upsert key). Returns the station's row id and
    /// whether it already existed, so callers can tell a fresh save from
    /// a duplicate save-of-an-already-saved station apart (e.g. to show
    /// "Saved" vs "Already saved").
    pub fn add_radio_station(
        &self,
        name: &str,
        stream_url: &str,
        homepage: &str,
        favicon_url: &str,
        tags: &str,
    ) -> Result<(i64, bool), String> {
        use rusqlite::OptionalExtension;
        let existing_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM radio_stations WHERE stream_url = ?1",
                params![stream_url],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("Add radio station lookup error: {e}"))?;
        self.conn
            .execute(
                "INSERT INTO radio_stations (name, stream_url, homepage, favicon_url, tags)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(stream_url) DO UPDATE SET
                    name=excluded.name, homepage=excluded.homepage,
                    favicon_url=excluded.favicon_url, tags=excluded.tags",
                params![name, stream_url, homepage, favicon_url, tags],
            )
            .map_err(|e| format!("Add radio station error: {e}"))?;
        let id = match existing_id {
            Some(id) => id,
            None => self.conn.last_insert_rowid(),
        };
        Ok((id, existing_id.is_some()))
    }

    /// Rename a saved station in place, leaving every other field untouched.
    pub fn rename_radio_station(&self, id: i64, name: &str) -> Result<(), String> {
        self.conn
            .execute("UPDATE radio_stations SET name = ?1 WHERE id = ?2", params![name, id])
            .map_err(|e| format!("Rename radio station error: {e}"))?;
        Ok(())
    }

    pub fn remove_radio_station(&self, id: i64) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM radio_stations WHERE id = ?1", params![id])
            .map_err(|e| format!("Remove radio station error: {e}"))?;
        Ok(())
    }

    pub fn list_radio_stations(&self) -> Result<Vec<RadioStation>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, stream_url, homepage, favicon_url, tags
                 FROM radio_stations ORDER BY name",
            )
            .map_err(|e| format!("List radio stations error: {e}"))?;
        let stations = stmt
            .query_map([], |row| {
                Ok(RadioStation {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    stream_url: row.get(2)?,
                    homepage: row.get(3)?,
                    favicon_url: row.get(4)?,
                    tags: row.get(5)?,
                })
            })
            .map_err(|e| format!("List radio stations error: {e}"))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(stations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_migrated_memory() -> OnlineStore {
        // Mirrors `LibraryDb::open_memory` schema creation for this module's
        // tables, since `OnlineStore` itself never migrates.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE podcasts (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, feed_url TEXT NOT NULL UNIQUE,
                 title TEXT NOT NULL DEFAULT '', description TEXT NOT NULL DEFAULT '',
                 image_url TEXT NOT NULL DEFAULT '', author TEXT NOT NULL DEFAULT '',
                 last_refreshed INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE podcast_episodes (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 podcast_id INTEGER NOT NULL REFERENCES podcasts(id) ON DELETE CASCADE,
                 guid TEXT NOT NULL, title TEXT NOT NULL DEFAULT '',
                 enclosure_url TEXT NOT NULL, mime TEXT NOT NULL DEFAULT '',
                 duration_secs INTEGER NOT NULL DEFAULT 0, pub_date INTEGER NOT NULL DEFAULT 0,
                 description TEXT NOT NULL DEFAULT '', position_ms INTEGER NOT NULL DEFAULT 0,
                 played INTEGER NOT NULL DEFAULT 0, downloaded_path TEXT NOT NULL DEFAULT '',
                 UNIQUE(podcast_id, guid)
             );
             CREATE TABLE radio_stations (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                 stream_url TEXT NOT NULL UNIQUE, homepage TEXT NOT NULL DEFAULT '',
                 favicon_url TEXT NOT NULL DEFAULT '', tags TEXT NOT NULL DEFAULT ''
             );",
        )
        .unwrap();
        OnlineStore { conn }
    }

    fn meta(title: &str) -> PodcastMeta {
        PodcastMeta {
            title: title.to_string(),
            description: "desc".to_string(),
            image_url: "https://example.com/art.png".to_string(),
            author: "Example Author".to_string(),
        }
    }

    fn episode(guid: &str, title: &str) -> EpisodeMeta {
        EpisodeMeta {
            guid: guid.to_string(),
            title: title.to_string(),
            enclosure_url: format!("https://example.com/{guid}.mp3"),
            mime: "audio/mpeg".to_string(),
            duration_secs: 120,
            pub_date: 1000,
            description: String::new(),
        }
    }

    #[test]
    fn add_podcast_is_idempotent_by_feed_url() {
        let store = open_migrated_memory();
        let id1 = store.add_podcast("https://feed.example/rss", &meta("Show")).unwrap();
        let id2 = store
            .add_podcast("https://feed.example/rss", &meta("Show Renamed"))
            .unwrap();
        assert_eq!(id1, id2);
        let podcasts = store.list_podcasts().unwrap();
        assert_eq!(podcasts.len(), 1);
        assert_eq!(podcasts[0].title, "Show Renamed");
    }

    #[test]
    fn add_podcast_persists_author() {
        let store = open_migrated_memory();
        store.add_podcast("https://feed.example/rss", &meta("Show")).unwrap();
        let podcasts = store.list_podcasts().unwrap();
        assert_eq!(podcasts[0].author, "Example Author");
    }

    #[test]
    fn list_podcasts_computes_unplayed_count() {
        let store = open_migrated_memory();
        let id = store.add_podcast("https://feed.example/rss", &meta("Show")).unwrap();
        store
            .upsert_episodes(id, &[episode("guid-1", "Ep 1"), episode("guid-2", "Ep 2")])
            .unwrap();

        let podcasts = store.list_podcasts().unwrap();
        assert_eq!(podcasts[0].unplayed_count, 2);

        let episodes = store.list_episodes(id).unwrap();
        store
            .save_episode_position(episodes[0].id, 1_000, true)
            .unwrap();

        let podcasts = store.list_podcasts().unwrap();
        assert_eq!(podcasts[0].unplayed_count, 1);
    }

    #[test]
    fn upsert_episodes_preserves_position_on_refresh() {
        let store = open_migrated_memory();
        let id = store.add_podcast("https://feed.example/rss", &meta("Show")).unwrap();
        store.upsert_episodes(id, &[episode("guid-1", "Ep 1")]).unwrap();

        let episodes = store.list_episodes(id).unwrap();
        assert_eq!(episodes.len(), 1);
        store
            .save_episode_position(episodes[0].id, 5_000, false)
            .unwrap();

        // Re-fetching the same feed (title updated) must not reset progress.
        store
            .upsert_episodes(id, &[episode("guid-1", "Ep 1 - Updated Title")])
            .unwrap();
        let episodes = store.list_episodes(id).unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].title, "Ep 1 - Updated Title");
        assert_eq!(episodes[0].position_ms, 5_000);
        assert!(!episodes[0].played);
    }

    #[test]
    fn radio_station_crud_roundtrip() {
        let store = open_migrated_memory();
        let (id, existed) = store
            .add_radio_station("Test FM", "https://stream.example/live", "https://example.com", "", "jazz")
            .unwrap();
        assert!(!existed);
        assert_eq!(store.list_radio_stations().unwrap().len(), 1);

        store.remove_radio_station(id).unwrap();
        assert!(store.list_radio_stations().unwrap().is_empty());
    }

    #[test]
    fn add_radio_station_reports_already_existed_and_updates_metadata() {
        let store = open_migrated_memory();
        let (id1, existed1) = store
            .add_radio_station("Test FM", "https://stream.example/live", "https://example.com", "", "jazz")
            .unwrap();
        assert!(!existed1);

        let (id2, existed2) = store
            .add_radio_station(
                "Test FM Renamed",
                "https://stream.example/live",
                "https://example.com",
                "https://example.com/icon.png",
                "jazz,chill",
            )
            .unwrap();
        assert!(existed2);
        assert_eq!(id1, id2);

        let stations = store.list_radio_stations().unwrap();
        assert_eq!(stations.len(), 1);
        assert_eq!(stations[0].name, "Test FM Renamed");
        assert_eq!(stations[0].tags, "jazz,chill");
    }

    #[test]
    fn rename_radio_station_updates_only_the_name() {
        let store = open_migrated_memory();
        let (id, _) = store
            .add_radio_station("Test FM", "https://stream.example/live", "https://example.com", "", "jazz")
            .unwrap();
        store.rename_radio_station(id, "Renamed FM").unwrap();
        let stations = store.list_radio_stations().unwrap();
        assert_eq!(stations.len(), 1);
        assert_eq!(stations[0].name, "Renamed FM");
        assert_eq!(stations[0].tags, "jazz");
        assert_eq!(stations[0].stream_url, "https://stream.example/live");
    }
}
