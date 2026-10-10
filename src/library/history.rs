// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Play history and the queries behind the Home page.
//!
//! A *play* is recorded in the `play_history` table (one row per play: track
//! id + Unix timestamp) once a track has actually been listened to for half
//! its length or four minutes, whichever comes first — see [`PlayTracker`].
//! The rest of this module is read-side: album/artist aggregates over that
//! table (recently played, most played, rediscover, …) plus a few
//! history-independent shelves (recently added, random, decades) so the Home
//! page can be assembled with a single [`LibraryDb::home_data`] call.
//!
//! Only tracks stored in the library database (provider `"local"`) can have
//! history: remote providers (MPD, Subsonic) hand out transient track ids.

use super::LibraryDb;
use super::Track;
use rusqlite::{ToSql, params};
use std::time::Duration;

/// Provider whose tracks live in the library database.
pub const LOCAL_PROVIDER: &str = "local";

/// Listened time after which a play is counted, at most.
const MAX_LISTEN: Duration = Duration::from_secs(240);

/// Largest forward position step still counted as continuous listening;
/// anything bigger is treated as a seek. Playback ticks arrive every 500 ms.
const MAX_CONTINUOUS_STEP: Duration = Duration::from_secs(3);

/// How far into a track a backwards jump must land to count as a restart
/// (repeat-one, or the user seeking back to the beginning).
const RESTART_WINDOW: Duration = Duration::from_secs(5);

/// Items per Home shelf.
pub const SHELF_LEN: u32 = 20;

/// Albums not played for this long count as "forgotten" for Rediscover.
pub const REDISCOVER_AFTER_SECS: i64 = 90 * 24 * 3600;

/// Length of the "this month" window for most-played shelves.
pub const MONTH_SECS: i64 = 30 * 24 * 3600;

/// Listened time needed for a track of `duration` to count as played:
/// half of it, capped at four minutes. `None` when the length is unknown.
pub fn play_threshold(duration: Duration) -> Option<Duration> {
    if duration.is_zero() {
        return None;
    }
    Some((duration / 2).min(MAX_LISTEN))
}

/// Accumulates *actual* listening time for the current track and reports,
/// exactly once, when it crosses [`play_threshold`]. Seeking forward does
/// not count as listening, pausing stops the clock, and jumping back to the
/// start re-arms the tracker so a repeated track is counted again.
#[derive(Debug, Clone, Default)]
pub struct PlayTracker {
    last_position: Option<Duration>,
    listened: Duration,
    recorded: bool,
}

impl PlayTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything — call whenever the current track changes.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Whether the current track has already been counted.
    pub fn is_recorded(&self) -> bool {
        self.recorded
    }

    /// Feed one playback observation. Returns `true` exactly once per play.
    pub fn observe(&mut self, position: Duration, duration: Duration, playing: bool) -> bool {
        let previous = self.last_position.replace(position);

        if !playing {
            return false;
        }

        if let Some(previous) = previous {
            if position >= previous {
                let step = position - previous;
                if step <= MAX_CONTINUOUS_STEP {
                    self.listened += step;
                }
            } else if position < RESTART_WINDOW && previous > position + RESTART_WINDOW {
                // Jumped back to the beginning: a fresh listen.
                self.listened = Duration::ZERO;
                self.recorded = false;
            }
        }

        if self.recorded {
            return false;
        }
        match play_threshold(duration) {
            Some(threshold) if self.listened >= threshold => {
                self.recorded = true;
                true
            }
            _ => false,
        }
    }
}

/// Lightweight album reference returned by the history/Home queries.
/// `(artist, name)` is the album's identity everywhere in the app
/// (`album_artist` + `album`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumRef {
    pub artist: String,
    pub name: String,
    pub year: u32,
    /// Number of recorded plays of the album's tracks (0 when the query
    /// doesn't involve history).
    pub plays: u32,
    /// Query-specific Unix timestamp: last play, or file modification time
    /// for "recently added"; 0 when not applicable.
    pub timestamp: i64,
}

/// An artist with its recorded play count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistRef {
    pub name: String,
    pub plays: u32,
}

/// A decade present in the library and how many albums fall in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecadeCount {
    /// First year of the decade, e.g. `1980`.
    pub decade: u32,
    pub albums: u32,
}

/// Everything the Home page shows, loaded in one go.
#[derive(Debug, Clone, Default)]
pub struct HomeData {
    pub recently_played: Vec<AlbumRef>,
    pub most_played_month: Vec<AlbumRef>,
    pub top_artists_month: Vec<ArtistRef>,
    pub recently_added: Vec<AlbumRef>,
    pub rediscover: Vec<AlbumRef>,
    pub random: Vec<AlbumRef>,
    pub decades: Vec<DecadeCount>,
    pub total_albums: u32,
    pub total_plays: u64,
    /// Whether plays are recorded for this library at all (false for
    /// remote providers, whose tracks have no stable database id).
    pub history_available: bool,
}

/// Drop from `picks` every album also present in `exclude`, then cap it at
/// `limit` entries.
pub fn exclude_albums(picks: &mut Vec<AlbumRef>, exclude: &[AlbumRef], limit: usize) {
    picks.retain(|a| {
        !exclude
            .iter()
            .any(|e| e.artist == a.artist && e.name == a.name)
    });
    picks.truncate(limit);
}

/// Leading columns every album query selects (the album identity and its
/// year); the caller appends a play count and a timestamp column.
const ALBUM_COLUMNS: &str = "t.album_artist, t.album, MAX(t.year)";

impl LibraryDb {
    /// Record one play of `track_id` at `played_at` (Unix seconds).
    pub fn record_play(&self, track_id: i64, played_at: i64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO play_history (track_id, played_at) VALUES (?1, ?2)",
                params![track_id, played_at],
            )
            .map_err(|e| format!("Record play error: {e}"))?;
        Ok(())
    }

    /// Number of recorded plays of a track.
    pub fn play_count(&self, track_id: i64) -> Result<u32, String> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM play_history WHERE track_id = ?1",
                params![track_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n.max(0) as u32)
            .map_err(|e| format!("Play count error: {e}"))
    }

    /// Total number of recorded plays.
    pub fn total_plays(&self) -> Result<u64, String> {
        self.conn
            .query_row("SELECT COUNT(*) FROM play_history", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|n| n.max(0) as u64)
            .map_err(|e| format!("Total plays error: {e}"))
    }

    /// Delete the whole play history.
    pub fn clear_play_history(&self) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM play_history", [])
            .map_err(|e| format!("Clear history error: {e}"))?;
        Ok(())
    }

    fn query_albums(&self, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<AlbumRef>, String> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| format!("Album query error: {e}"))?;
        let rows = stmt
            .query_map(params, |row| {
                Ok(AlbumRef {
                    artist: row.get(0)?,
                    name: row.get(1)?,
                    year: row.get::<_, i64>(2)?.max(0) as u32,
                    plays: row.get::<_, i64>(3)?.max(0) as u32,
                    timestamp: row.get(4)?,
                })
            })
            .map_err(|e| format!("Album query error: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Album row error: {e}"))
    }

    /// Distinct tracks, most recently played first.
    pub fn recently_played_tracks(&self, provider: &str, limit: u32) -> Result<Vec<Track>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.id, t.path, t.title, t.artist, t.album_artist, t.album, t.genre,
                        t.track_number, t.disc_number, t.year, t.duration_ms, t.bitrate,
                        t.sample_rate, t.provider, t.provider_track_id, t.is_favorite,
                        t.rating, t.rg_track_gain, t.rg_album_gain
                 FROM tracks t
                 JOIN (SELECT track_id, MAX(played_at) AS lp, MAX(id) AS mid
                       FROM play_history GROUP BY track_id) h ON h.track_id = t.id
                 WHERE t.provider = ?1
                 ORDER BY h.lp DESC, h.mid DESC
                 LIMIT ?2",
            )
            .map_err(|e| format!("Recent tracks query error: {e}"))?;
        let rows = stmt
            .query_map(params![provider, limit], Self::row_to_track)
            .map_err(|e| format!("Recent tracks query error: {e}"))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Albums ordered by their most recent play.
    pub fn recently_played_albums(
        &self,
        provider: &str,
        limit: u32,
    ) -> Result<Vec<AlbumRef>, String> {
        let sql = format!(
            "SELECT {ALBUM_COLUMNS}, COUNT(h.id), MAX(h.played_at)
             FROM play_history h JOIN tracks t ON t.id = h.track_id
             WHERE t.provider = ?1 AND t.album != ''
             GROUP BY t.album_artist, t.album
             ORDER BY MAX(h.played_at) DESC, MAX(h.id) DESC
             LIMIT ?2"
        );
        self.query_albums(&sql, &[&provider, &limit])
    }

    /// Albums ranked by how often they were played since `since` (Unix
    /// seconds; `0` for all time). Plays are normalised by track count so a
    /// 20-track album isn't favoured over an EP that was listened to just as
    /// often.
    pub fn most_played_albums(
        &self,
        provider: &str,
        since: i64,
        limit: u32,
    ) -> Result<Vec<AlbumRef>, String> {
        let sql = format!(
            "SELECT {ALBUM_COLUMNS}, COUNT(h.id), MAX(h.played_at)
             FROM play_history h JOIN tracks t ON t.id = h.track_id
             WHERE t.provider = ?1 AND t.album != '' AND h.played_at >= ?2
             GROUP BY t.album_artist, t.album
             ORDER BY CAST(COUNT(h.id) AS REAL) /
                      (SELECT COUNT(*) FROM tracks t2
                        WHERE t2.provider = t.provider
                          AND t2.album_artist = t.album_artist AND t2.album = t.album) DESC,
                      COUNT(h.id) DESC, MAX(h.played_at) DESC
             LIMIT ?3"
        );
        self.query_albums(&sql, &[&provider, &since, &limit])
    }

    /// Artists ranked by play count since `since` (`0` for all time).
    pub fn most_played_artists(
        &self,
        provider: &str,
        since: i64,
        limit: u32,
    ) -> Result<Vec<ArtistRef>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT COALESCE(NULLIF(t.album_artist, ''), t.artist) AS name, COUNT(h.id)
                 FROM play_history h JOIN tracks t ON t.id = h.track_id
                 WHERE t.provider = ?1 AND h.played_at >= ?2 AND name != ''
                 GROUP BY name
                 ORDER BY COUNT(h.id) DESC, MAX(h.played_at) DESC
                 LIMIT ?3",
            )
            .map_err(|e| format!("Top artists query error: {e}"))?;
        let rows = stmt
            .query_map(params![provider, since, limit], |row| {
                Ok(ArtistRef {
                    name: row.get(0)?,
                    plays: row.get::<_, i64>(1)?.max(0) as u32,
                })
            })
            .map_err(|e| format!("Top artists query error: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Top artists row error: {e}"))
    }

    /// Albums by newest file modification time (the library keeps no
    /// separate "date added", so the scan-time file mtime stands in).
    pub fn recently_added_albums(
        &self,
        provider: &str,
        limit: u32,
    ) -> Result<Vec<AlbumRef>, String> {
        let sql = format!(
            "SELECT {ALBUM_COLUMNS}, 0, MAX(t.mtime)
             FROM tracks t
             WHERE t.provider = ?1 AND t.album != ''
             GROUP BY t.album_artist, t.album
             ORDER BY MAX(t.mtime) DESC, t.album_artist, t.album
             LIMIT ?2"
        );
        self.query_albums(&sql, &[&provider, &limit])
    }

    /// Albums never played, or not played since `now - REDISCOVER_AFTER_SECS`
    /// — never-played ones first, each group in random order.
    pub fn rediscover_albums(
        &self,
        provider: &str,
        now: i64,
        limit: u32,
    ) -> Result<Vec<AlbumRef>, String> {
        let cutoff = now - REDISCOVER_AFTER_SECS;
        let sql = format!(
            "SELECT {ALBUM_COLUMNS}, COUNT(h.id), COALESCE(MAX(h.played_at), 0)
             FROM tracks t LEFT JOIN play_history h ON h.track_id = t.id
             WHERE t.provider = ?1 AND t.album != ''
             GROUP BY t.album_artist, t.album
             HAVING COALESCE(MAX(h.played_at), 0) < ?2
             ORDER BY CASE WHEN COUNT(h.id) = 0 THEN 0 ELSE 1 END, RANDOM()
             LIMIT ?3"
        );
        self.query_albums(&sql, &[&provider, &cutoff, &limit])
    }

    /// Random albums.
    pub fn random_albums(&self, provider: &str, limit: u32) -> Result<Vec<AlbumRef>, String> {
        let sql = format!(
            "SELECT {ALBUM_COLUMNS}, 0, 0
             FROM tracks t
             WHERE t.provider = ?1 AND t.album != ''
             GROUP BY t.album_artist, t.album
             ORDER BY RANDOM()
             LIMIT ?2"
        );
        self.query_albums(&sql, &[&provider, &limit])
    }

    /// Decades (`1970`, `1980`, …) that have at least one dated album,
    /// oldest first. An album belongs to the decade of its newest track.
    pub fn album_decades(&self, provider: &str) -> Result<Vec<DecadeCount>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT (y / 10) * 10 AS decade, COUNT(*)
                 FROM (SELECT MAX(year) AS y FROM tracks
                       WHERE provider = ?1 AND album != ''
                       GROUP BY album_artist, album
                       HAVING MAX(year) >= 1900)
                 GROUP BY decade
                 ORDER BY decade",
            )
            .map_err(|e| format!("Decades query error: {e}"))?;
        let rows = stmt
            .query_map(params![provider], |row| {
                Ok(DecadeCount {
                    decade: row.get::<_, i64>(0)?.max(0) as u32,
                    albums: row.get::<_, i64>(1)?.max(0) as u32,
                })
            })
            .map_err(|e| format!("Decades query error: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Decades row error: {e}"))
    }

    /// Every album whose year falls in the decade starting at `decade`,
    /// ordered by year, then artist and title.
    pub fn albums_by_decade(&self, provider: &str, decade: u32) -> Result<Vec<AlbumRef>, String> {
        let from = i64::from(decade);
        let to = from + 9;
        let sql = format!(
            "SELECT {ALBUM_COLUMNS}, 0, 0
             FROM tracks t
             WHERE t.provider = ?1 AND t.album != ''
             GROUP BY t.album_artist, t.album
             HAVING MAX(t.year) BETWEEN ?2 AND ?3
             ORDER BY MAX(t.year), t.album_artist, t.album"
        );
        self.query_albums(&sql, &[&provider, &from, &to])
    }

    /// Number of distinct albums for a provider.
    pub fn album_count(&self, provider: &str) -> Result<u32, String> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM (SELECT 1 FROM tracks
                   WHERE provider = ?1 AND album != ''
                   GROUP BY album_artist, album)",
                params![provider],
                |row| row.get::<_, i64>(0),
            )
            .map(|n| n.max(0) as u32)
            .map_err(|e| format!("Album count error: {e}"))
    }

    /// Assemble every Home shelf. `now` is Unix seconds.
    ///
    /// Shelves are deduplicated against each other where it matters: the
    /// random picks never repeat Rediscover entries, and Rediscover is empty
    /// until there is any history to rediscover *from*.
    pub fn home_data(&self, provider: &str, now: i64) -> Result<HomeData, String> {
        let total_plays = self.total_plays()?;
        let month_start = now - MONTH_SECS;

        let recently_played = self.recently_played_albums(provider, SHELF_LEN)?;
        let most_played_month = self.most_played_albums(provider, month_start, SHELF_LEN)?;
        let top_artists_month = self.most_played_artists(provider, month_start, SHELF_LEN)?;
        let recently_added = self.recently_added_albums(provider, SHELF_LEN)?;
        let rediscover = if total_plays > 0 {
            self.rediscover_albums(provider, now, SHELF_LEN)?
        } else {
            Vec::new()
        };
        let mut random = self.random_albums(provider, SHELF_LEN * 2)?;
        exclude_albums(&mut random, &rediscover, SHELF_LEN as usize);

        Ok(HomeData {
            recently_played,
            most_played_month,
            top_artists_month,
            recently_added,
            rediscover,
            random,
            decades: self.album_decades(provider)?,
            total_albums: self.album_count(provider)?,
            total_plays,
            history_available: true,
        })
    }
}

/// A library track as the listening-history importer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportTrack {
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    /// Track length in seconds (0 = unknown).
    pub duration_secs: u32,
}

/// One play to be merged into the history by [`LibraryDb::import_plays`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportPlay {
    pub track_id: i64,
    /// Original listen time, Unix seconds.
    pub played_at: i64,
    /// Plays of the same track already in the history within this many
    /// seconds count as the same listen (see [`LibraryDb::import_plays`]).
    pub window_secs: i64,
}

/// An artist's taste profile, used to derive "similar artists" locally when
/// no online source is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistProfile {
    pub name: String,
    /// Lower-cased genre labels found on the artist's tracks.
    pub genres: Vec<String>,
    /// Mean release year of the dated tracks, `0` when none is dated.
    pub year: u32,
}

/// Library artist name expression shared by the artist queries: the album
/// artist, else the track artist (same identity as `most_played_artists`).
const ARTIST_NAME_EXPR: &str = "COALESCE(NULLIF(t.album_artist, ''), t.artist)";

/// Split a genre tag (`"Rock; Alt/Indie"`) into lower-cased labels.
pub fn genre_labels(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split([';', '/', ',']) {
        let label = part.trim().to_lowercase();
        if !label.is_empty() && !out.contains(&label) {
            out.push(label);
        }
    }
    out
}

impl LibraryDb {
    /// Every track of `provider` with the fields the importer matches on.
    pub fn import_tracks(&self, provider: &str) -> Result<Vec<ImportTrack>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, artist, album_artist, album, duration_ms
                 FROM tracks WHERE provider = ?1",
            )
            .map_err(|e| format!("Import tracks query error: {e}"))?;
        let rows = stmt
            .query_map(params![provider], |row| {
                Ok(ImportTrack {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    artist: row.get(2)?,
                    album_artist: row.get(3)?,
                    album: row.get(4)?,
                    duration_secs: (row.get::<_, i64>(5)?.max(0) / 1000) as u32,
                })
            })
            .map_err(|e| format!("Import tracks query error: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Import tracks row error: {e}"))
    }

    /// Highest `play_history` row id (0 when empty). Take it *before* an
    /// import and hand it to [`Self::import_plays`].
    pub fn max_play_id(&self) -> Result<i64, String> {
        self.conn
            .query_row("SELECT COALESCE(MAX(id), 0) FROM play_history", [], |r| {
                r.get(0)
            })
            .map_err(|e| format!("Max play id error: {e}"))
    }

    /// Merge `plays` into the history; returns how many rows were added.
    ///
    /// Idempotent: a play is skipped when the track already has a play at
    /// exactly that timestamp (so re-importing adds nothing), or — for rows
    /// with an id `<= preexisting_max_id`, i.e. recorded before this import
    /// started — one within `window_secs` of it. The latter keeps a listen
    /// Aulos recorded itself (stamped when the half-way mark was crossed)
    /// from being counted a second time when the service reports it (stamped
    /// with the track's start), and a second service's copy of the same
    /// listen likewise. Genuine repeats *inside* one import are kept.
    pub fn import_plays(
        &self,
        plays: &[ImportPlay],
        preexisting_max_id: i64,
    ) -> Result<u32, String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Import transaction error: {e}"))?;
        let mut added = 0u32;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO play_history (track_id, played_at)
                     SELECT ?1, ?2
                     WHERE NOT EXISTS (
                         SELECT 1 FROM play_history
                         WHERE track_id = ?1
                           AND (played_at = ?2
                                OR (id <= ?4 AND played_at BETWEEN ?2 - ?3 AND ?2 + ?3)))",
                )
                .map_err(|e| format!("Import prepare error: {e}"))?;
            for p in plays {
                let n = stmt
                    .execute(params![
                        p.track_id,
                        p.played_at,
                        p.window_secs.max(0),
                        preexisting_max_id
                    ])
                    .map_err(|e| format!("Import insert error: {e}"))?;
                added += n as u32;
            }
        }
        tx.commit()
            .map_err(|e| format!("Import commit error: {e}"))?;
        Ok(added)
    }

    /// Artists with favorited tracks, most favorites first.
    pub fn favorite_artists(&self, provider: &str, limit: u32) -> Result<Vec<String>, String> {
        let sql = format!(
            "SELECT {ARTIST_NAME_EXPR} AS name FROM tracks t
             WHERE t.provider = ?1 AND t.is_favorite = 1 AND name != ''
             GROUP BY name ORDER BY COUNT(*) DESC, name LIMIT ?2"
        );
        self.query_names(&sql, params![provider, limit])
    }

    /// Random library artists.
    pub fn random_artists(&self, provider: &str, limit: u32) -> Result<Vec<String>, String> {
        let sql = format!(
            "SELECT {ARTIST_NAME_EXPR} AS name FROM tracks t
             WHERE t.provider = ?1 AND name != ''
             GROUP BY name ORDER BY RANDOM() LIMIT ?2"
        );
        self.query_names(&sql, params![provider, limit])
    }

    /// Every distinct library artist name.
    pub fn library_artists(&self, provider: &str) -> Result<Vec<String>, String> {
        let sql = format!(
            "SELECT {ARTIST_NAME_EXPR} AS name FROM tracks t
             WHERE t.provider = ?1 AND name != '' GROUP BY name ORDER BY name"
        );
        self.query_names(&sql, params![provider])
    }

    fn query_names(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| format!("Artist names query error: {e}"))?;
        let rows = stmt
            .query_map(params, |row| row.get::<_, String>(0))
            .map_err(|e| format!("Artist names query error: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Artist names row error: {e}"))
    }

    /// Genre/decade profile of every library artist.
    pub fn artist_profiles(&self, provider: &str) -> Result<Vec<ArtistProfile>, String> {
        let sql = format!(
            "SELECT {ARTIST_NAME_EXPR} AS name, t.genre, t.year FROM tracks t
             WHERE t.provider = ?1 AND name != ''"
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| format!("Artist profiles query error: {e}"))?;
        let rows = stmt
            .query_map(params![provider], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| format!("Artist profiles query error: {e}"))?;

        // name -> (genres, year sum, dated tracks)
        let mut acc: std::collections::BTreeMap<String, (Vec<String>, i64, i64)> =
            std::collections::BTreeMap::new();
        for row in rows {
            let (name, genre, year) = row.map_err(|e| format!("Artist profiles row error: {e}"))?;
            let entry = acc.entry(name).or_default();
            for g in genre_labels(&genre) {
                if !entry.0.contains(&g) {
                    entry.0.push(g);
                }
            }
            if year >= 1900 {
                entry.1 += year;
                entry.2 += 1;
            }
        }
        Ok(acc
            .into_iter()
            .map(|(name, (genres, sum, n))| ArtistProfile {
                name,
                genres,
                year: if n > 0 { (sum / n) as u32 } else { 0 },
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 24 * 3600;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    // ---- PlayTracker -------------------------------------------------

    #[test]
    fn threshold_is_half_the_track_capped_at_four_minutes() {
        assert_eq!(play_threshold(secs(180)), Some(secs(90)));
        assert_eq!(play_threshold(secs(600)), Some(secs(240)));
        assert_eq!(play_threshold(secs(480)), Some(secs(240)));
        assert_eq!(play_threshold(Duration::ZERO), None);
    }

    /// Drive the tracker with 0.5 s ticks from `from` to `to` seconds.
    fn play_through(
        tracker: &mut PlayTracker,
        from: u64,
        to: u64,
        duration: Duration,
    ) -> Option<Duration> {
        let mut ms = from * 1000;
        while ms <= to * 1000 {
            if tracker.observe(Duration::from_millis(ms), duration, true) {
                return Some(Duration::from_millis(ms));
            }
            ms += 500;
        }
        None
    }

    #[test]
    fn records_once_at_half_of_a_normal_track() {
        let mut t = PlayTracker::new();
        let fired = play_through(&mut t, 0, 180, secs(180)).expect("play recorded");
        assert!(fired >= secs(89) && fired <= secs(91), "fired at {fired:?}");
        // Never fires a second time for the same listen.
        assert_eq!(play_through(&mut t, 91, 180, secs(180)), None);
        assert!(t.is_recorded());
    }

    #[test]
    fn long_tracks_record_at_four_minutes() {
        let mut t = PlayTracker::new();
        let fired = play_through(&mut t, 0, 600, secs(600)).expect("play recorded");
        assert!(
            fired >= secs(239) && fired <= secs(241),
            "fired at {fired:?}"
        );
    }

    #[test]
    fn seeking_forward_does_not_count_as_listening() {
        let mut t = PlayTracker::new();
        assert!(!t.observe(secs(0), secs(200), true));
        // Skip straight to 150 s: not "listened".
        assert!(!t.observe(secs(150), secs(200), true));
        assert!(!t.observe(secs(151), secs(200), true));
        assert!(!t.is_recorded());
    }

    #[test]
    fn paused_time_does_not_count() {
        let mut t = PlayTracker::new();
        for i in 0..100 {
            assert!(!t.observe(secs(10), secs(60), false), "tick {i}");
        }
        assert!(!t.is_recorded());
        // Resuming from the same spot counts only the time actually played.
        assert_eq!(play_through(&mut t, 10, 20, secs(60)), None);
        assert!(play_through(&mut t, 20, 40, secs(60)).is_some());
    }

    #[test]
    fn restart_after_record_counts_again() {
        let mut t = PlayTracker::new();
        assert!(play_through(&mut t, 0, 100, secs(100)).is_some());
        // Repeat-one: position falls back to the start.
        assert!(!t.observe(secs(0), secs(100), true));
        assert!(!t.is_recorded());
        assert!(play_through(&mut t, 0, 100, secs(100)).is_some());
    }

    #[test]
    fn reset_clears_progress() {
        let mut t = PlayTracker::new();
        play_through(&mut t, 0, 40, secs(100));
        t.reset();
        assert!(play_through(&mut t, 0, 40, secs(100)).is_none());
    }

    #[test]
    fn unknown_duration_never_records() {
        let mut t = PlayTracker::new();
        assert_eq!(play_through(&mut t, 0, 600, Duration::ZERO), None);
    }

    // ---- Queries -----------------------------------------------------

    fn make_track(path: &str, artist: &str, album: &str, year: u32) -> Track {
        Track {
            id: 0,
            path: PathBuf::from(path),
            title: path.to_string(),
            artist: artist.to_string(),
            album_artist: artist.to_string(),
            album: album.to_string(),
            genre: String::new(),
            track_number: 1,
            disc_number: 1,
            year,
            duration: Duration::from_secs(200),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("local"),
            source_uri: path.to_string(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    fn add(db: &LibraryDb, path: &str, artist: &str, album: &str, year: u32, mtime: i64) -> i64 {
        db.upsert_track(&make_track(path, artist, album, year), mtime)
            .unwrap();
        db.conn
            .query_row(
                "SELECT id FROM tracks WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn play_at(db: &LibraryDb, id: i64, at: i64) {
        db.record_play(id, at).unwrap();
    }

    fn names(albums: &[AlbumRef]) -> Vec<&str> {
        albums.iter().map(|a| a.name.as_str()).collect()
    }

    /// Three albums: A (1975, 2 tracks), B (1984, 2 tracks), C (1991, 1 track).
    fn library() -> (LibraryDb, [i64; 5]) {
        let db = LibraryDb::open_memory().unwrap();
        let a1 = add(&db, "/m/a1.flac", "Alpha", "A", 1975, 100);
        let a2 = add(&db, "/m/a2.flac", "Alpha", "A", 1975, 100);
        let b1 = add(&db, "/m/b1.flac", "Beta", "B", 1984, 300);
        let b2 = add(&db, "/m/b2.flac", "Beta", "B", 1984, 300);
        let c1 = add(&db, "/m/c1.flac", "Gamma", "C", 1991, 200);
        (db, [a1, a2, b1, b2, c1])
    }

    #[test]
    fn empty_history_gives_empty_play_shelves() {
        let (db, _) = library();
        assert!(db.recently_played_albums("local", 10).unwrap().is_empty());
        assert!(db.recently_played_tracks("local", 10).unwrap().is_empty());
        assert!(db.most_played_albums("local", 0, 10).unwrap().is_empty());
        assert!(db.most_played_artists("local", 0, 10).unwrap().is_empty());
        let home = db.home_data("local", NOW).unwrap();
        assert!(
            home.rediscover.is_empty(),
            "no history → nothing to rediscover"
        );
        assert_eq!(home.total_plays, 0);
        assert_eq!(home.total_albums, 3);
        assert_eq!(home.random.len(), 3);
    }

    #[test]
    fn record_play_and_play_count() {
        let (db, ids) = library();
        play_at(&db, ids[0], NOW);
        play_at(&db, ids[0], NOW + 10);
        play_at(&db, ids[2], NOW);
        assert_eq!(db.play_count(ids[0]).unwrap(), 2);
        assert_eq!(db.play_count(ids[1]).unwrap(), 0);
        assert_eq!(db.total_plays().unwrap(), 3);
        assert!(
            db.record_play(999_999, NOW).is_err(),
            "FK rejects unknown track"
        );
        db.clear_play_history().unwrap();
        assert_eq!(db.total_plays().unwrap(), 0);
    }

    #[test]
    fn recently_played_orders_by_latest_play_and_dedupes() {
        let (db, ids) = library();
        play_at(&db, ids[0], NOW - 300); // A
        play_at(&db, ids[2], NOW - 200); // B
        play_at(&db, ids[1], NOW - 100); // A again (other track)

        let albums = db.recently_played_albums("local", 10).unwrap();
        assert_eq!(names(&albums), ["A", "B"]);
        assert_eq!(albums[0].plays, 2);
        assert_eq!(albums[0].timestamp, NOW - 100);
        assert_eq!(albums[0].artist, "Alpha");
        assert_eq!(albums[0].year, 1975);

        let tracks = db.recently_played_tracks("local", 10).unwrap();
        let paths: Vec<_> = tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(paths, ["/m/a2.flac", "/m/b1.flac", "/m/a1.flac"]);

        assert_eq!(db.recently_played_albums("local", 1).unwrap().len(), 1);
    }

    #[test]
    fn most_played_respects_window_and_normalises_by_track_count() {
        let (db, ids) = library();
        // A (2 tracks): 2 recent plays -> 1.0 per track.
        play_at(&db, ids[0], NOW - DAY);
        play_at(&db, ids[1], NOW - DAY);
        // C (1 track): 2 recent plays -> 2.0 per track, should win.
        play_at(&db, ids[4], NOW - DAY);
        play_at(&db, ids[4], NOW - 2 * DAY);
        // B: 5 plays but all older than a month.
        for _ in 0..5 {
            play_at(&db, ids[2], NOW - 60 * DAY);
        }

        let month = db
            .most_played_albums("local", NOW - MONTH_SECS, 10)
            .unwrap();
        assert_eq!(names(&month), ["C", "A"]);

        let all_time = db.most_played_albums("local", 0, 10).unwrap();
        assert_eq!(all_time.len(), 3);
        // B: 5 plays / 2 tracks = 2.5 beats C's 2.0 once old plays count.
        assert_eq!(names(&all_time), ["B", "C", "A"]);

        let artists = db
            .most_played_artists("local", NOW - MONTH_SECS, 10)
            .unwrap();
        assert_eq!(artists.len(), 2);
        assert_eq!(
            artists[0],
            ArtistRef {
                name: "Alpha".into(),
                plays: 2
            }
        );
        let all_artists = db.most_played_artists("local", 0, 1).unwrap();
        assert_eq!(all_artists[0].name, "Beta");
        assert_eq!(all_artists[0].plays, 5);
    }

    #[test]
    fn recently_added_orders_by_mtime() {
        let (db, _) = library();
        let albums = db.recently_added_albums("local", 10).unwrap();
        assert_eq!(names(&albums), ["B", "C", "A"]);
        assert_eq!(albums[0].timestamp, 300);
        assert_eq!(db.recently_added_albums("local", 2).unwrap().len(), 2);
    }

    #[test]
    fn rediscover_returns_unplayed_and_stale_but_not_recent() {
        let (db, ids) = library();
        play_at(&db, ids[0], NOW - DAY); // A: played yesterday -> excluded
        play_at(&db, ids[2], NOW - 200 * DAY); // B: played long ago -> stale
        // C: never played.

        let albums = db.rediscover_albums("local", NOW, 10).unwrap();
        assert_eq!(names(&albums), ["C", "B"], "never-played first, A excluded");
        assert_eq!(albums[0].plays, 0);
        assert_eq!(albums[1].plays, 1);

        let home = db.home_data("local", NOW).unwrap();
        assert_eq!(home.rediscover.len(), 2);
        assert!(
            home.random.iter().all(|r| r.name == "A"),
            "random picks skip rediscover entries: {:?}",
            names(&home.random)
        );
    }

    #[test]
    fn random_albums_respects_limit_and_untitled_albums() {
        let (db, _) = library();
        add(&db, "/m/loose.flac", "Delta", "", 2000, 1);
        assert_eq!(db.random_albums("local", 2).unwrap().len(), 2);
        let all = db.random_albums("local", 50).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|a| !a.name.is_empty()));
    }

    #[test]
    fn decades_and_albums_by_decade() {
        let (db, _) = library();
        add(&db, "/m/u.flac", "Undated", "U", 0, 1);
        let decades = db.album_decades("local").unwrap();
        assert_eq!(
            decades,
            [
                DecadeCount {
                    decade: 1970,
                    albums: 1
                },
                DecadeCount {
                    decade: 1980,
                    albums: 1
                },
                DecadeCount {
                    decade: 1990,
                    albums: 1
                },
            ]
        );
        let eighties = db.albums_by_decade("local", 1980).unwrap();
        assert_eq!(names(&eighties), ["B"]);
        assert!(db.albums_by_decade("local", 2000).unwrap().is_empty());
    }

    #[test]
    fn queries_are_scoped_to_the_provider() {
        let (db, ids) = library();
        play_at(&db, ids[0], NOW);
        assert!(
            db.recently_played_albums("subsonic", 10)
                .unwrap()
                .is_empty()
        );
        assert!(db.recently_added_albums("subsonic", 10).unwrap().is_empty());
        assert_eq!(db.album_count("subsonic").unwrap(), 0);
        assert_eq!(db.album_count("local").unwrap(), 3);
    }

    #[test]
    fn deleting_a_track_removes_its_history() {
        let (db, ids) = library();
        play_at(&db, ids[4], NOW);
        assert_eq!(db.total_plays().unwrap(), 1);
        db.remove_track_by_path("/m/c1.flac").unwrap();
        assert_eq!(db.total_plays().unwrap(), 0);
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn add(db: &LibraryDb, path: &str, artist: &str, genre: &str, year: u32, fav: bool) -> i64 {
        let track = Track {
            id: 0,
            path: PathBuf::from(path),
            title: path.to_string(),
            artist: artist.to_string(),
            album_artist: artist.to_string(),
            album: "Album".to_string(),
            genre: genre.to_string(),
            track_number: 1,
            disc_number: 1,
            year,
            duration: Duration::from_secs(200),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("local"),
            source_uri: path.to_string(),
            is_favorite: fav,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        };
        db.upsert_track(&track, 1).unwrap();
        let id: i64 = db
            .conn
            .query_row(
                "SELECT id FROM tracks WHERE path = ?1",
                params![path],
                |r| r.get(0),
            )
            .unwrap();
        if fav {
            db.toggle_favorite(id).unwrap();
        }
        id
    }

    fn play(track_id: i64, played_at: i64) -> ImportPlay {
        ImportPlay {
            track_id,
            played_at,
            window_secs: 200,
        }
    }

    #[test]
    fn import_is_idempotent() {
        let db = LibraryDb::open_memory().unwrap();
        let t = add(&db, "/m/a.flac", "Alpha", "Rock", 1990, false);
        let plays = [play(t, 1_000), play(t, 5_000), play(t, 9_000)];
        let snapshot = db.max_play_id().unwrap();
        assert_eq!(db.import_plays(&plays, snapshot).unwrap(), 3);
        assert_eq!(db.play_count(t).unwrap(), 3);

        // Second run (fresh snapshot, as a real re-import would take).
        let snapshot = db.max_play_id().unwrap();
        assert_eq!(db.import_plays(&plays, snapshot).unwrap(), 0);
        assert_eq!(db.play_count(t).unwrap(), 3);
    }

    #[test]
    fn repeats_inside_one_import_are_kept() {
        let db = LibraryDb::open_memory().unwrap();
        let t = add(&db, "/m/a.flac", "Alpha", "", 0, false);
        // Repeat-one: two listens 100 s apart, well inside the window.
        let snapshot = db.max_play_id().unwrap();
        let added = db
            .import_plays(&[play(t, 1_000), play(t, 1_100)], snapshot)
            .unwrap();
        assert_eq!(added, 2);
    }

    #[test]
    fn listens_already_recorded_locally_are_not_doubled() {
        let db = LibraryDb::open_memory().unwrap();
        let t = add(&db, "/m/a.flac", "Alpha", "", 0, false);
        // Aulos recorded the play at the half-way mark...
        db.record_play(t, 1_100).unwrap();
        // ...the service reports the same listen stamped with its start.
        let snapshot = db.max_play_id().unwrap();
        let added = db
            .import_plays(&[play(t, 1_000), play(t, 9_000)], snapshot)
            .unwrap();
        assert_eq!(added, 1, "only the unrelated later listen is new");
        assert_eq!(db.play_count(t).unwrap(), 2);
    }

    #[test]
    fn import_rejects_unknown_tracks() {
        let db = LibraryDb::open_memory().unwrap();
        assert!(db.import_plays(&[play(424_242, 1)], 0).is_err());
        assert_eq!(db.total_plays().unwrap(), 0, "transaction rolled back");
    }

    #[test]
    fn import_tracks_and_artist_queries() {
        let db = LibraryDb::open_memory().unwrap();
        add(&db, "/m/a1.flac", "Alpha", "Rock; Indie", 1990, true);
        add(&db, "/m/a2.flac", "Alpha", "rock", 1994, false);
        add(&db, "/m/b1.flac", "Beta", "Jazz", 0, false);
        let tracks = db.import_tracks("local").unwrap();
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].duration_secs, 200);
        assert_eq!(db.favorite_artists("local", 10).unwrap(), ["Alpha"]);
        assert_eq!(db.library_artists("local").unwrap(), ["Alpha", "Beta"]);
        assert_eq!(db.random_artists("local", 10).unwrap().len(), 2);

        let profiles = db.artist_profiles("local").unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].name, "Alpha");
        assert_eq!(profiles[0].genres, ["rock", "indie"]);
        assert_eq!(profiles[0].year, 1992);
        assert_eq!(profiles[1].year, 0, "undated tracks leave the year unknown");
    }

    #[test]
    fn genre_labels_split_and_dedupe() {
        assert_eq!(
            genre_labels("Rock; Alt/Indie, rock"),
            ["rock", "alt", "indie"]
        );
        assert!(genre_labels("  ").is_empty());
    }
}
