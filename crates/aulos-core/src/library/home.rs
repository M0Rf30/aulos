// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! UI-independent Home page logic: the shelf-reuse cache and the builders
//! that derive Home content from the albums held in memory (used for
//! providers whose library does not live in the library database).

use super::history::{
    AlbumRef, DecadeCount, FAVOURITES_SHELF_LEN, HomeData, SHELF_LEN, month_start,
};
use super::state::names_match;
use super::{Album, Track};
use std::time::Duration;
use web_time::Instant;

/// How long loaded Home shelves are reused before a Home visit reloads them
/// anyway (the "this month" windows are relative to the wall clock).
pub const HOME_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

/// What the currently loaded `HomeData` was computed for. A Home visit only
/// reloads when this no longer matches (a play was recorded, the provider or
/// the suggestions toggle changed) or the data aged out; library changes and
/// explicit refreshes always reload directly.
#[derive(Debug, Default)]
pub struct HomeCache {
    /// Bumped whenever a play is written to the history.
    play_gen: u64,
    loaded: Option<HomeCacheKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomeCacheKey {
    provider: String,
    suggestions: bool,
    play_gen: u64,
    loaded_at: Instant,
}

impl HomeCache {
    /// A play was recorded: loaded shelves are out of date.
    pub fn invalidate(&mut self) {
        self.play_gen += 1;
    }

    /// Record that shelves for `provider` were just loaded.
    pub fn mark_loaded(&mut self, provider: &str, suggestions: bool, now: Instant) {
        self.loaded = Some(HomeCacheKey {
            provider: provider.to_string(),
            suggestions,
            play_gen: self.play_gen,
            loaded_at: now,
        });
    }

    /// Whether the loaded shelves can be shown again as they are.
    #[must_use]
    pub fn is_fresh(&self, provider: &str, suggestions: bool, now: Instant) -> bool {
        self.loaded.as_ref().is_some_and(|k| {
            k.provider == provider
                && k.suggestions == suggestions
                && k.play_gen == self.play_gen
                && now.saturating_duration_since(k.loaded_at) < HOME_CACHE_TTL
        })
    }
}

/// Every track of every album credited to `artist`, album by album.
#[must_use]
pub fn artist_tracks(albums: &[Album], artist: &str) -> Vec<Track> {
    albums
        .iter()
        .filter(|a| names_match(&a.artist, artist))
        .flat_map(|a| a.tracks.iter().cloned())
        .collect()
}

/// The first year of `year`'s decade, if it is a plausible release year.
fn decade_of(year: u32) -> Option<u32> {
    (year >= 1900).then_some(year / 10 * 10)
}

fn album_ref(album: &Album) -> AlbumRef {
    AlbumRef {
        artist: album.artist.clone(),
        name: album.name.clone(),
        year: album.year,
        plays: 0,
        timestamp: 0,
    }
}

/// Fisher–Yates shuffle driven by a time-seeded xorshift: good enough for
/// "random picks", and avoids a dependency.
fn shuffled<T>(mut items: Vec<T>) -> Vec<T> {
    let mut state = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    for i in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(i, (state % (i as u64 + 1)) as usize);
    }
    items
}

/// Up to `limit` randomly chosen titled albums.
#[must_use]
pub fn random_album_refs(albums: &[Album], limit: usize) -> Vec<AlbumRef> {
    let candidates: Vec<&Album> = albums.iter().filter(|a| !a.name.is_empty()).collect();
    shuffled(candidates)
        .into_iter()
        .take(limit)
        .map(album_ref)
        .collect()
}

/// Albums released in the decade starting at `decade`, by year then artist.
#[must_use]
pub fn decade_albums(albums: &[Album], decade: u32) -> Vec<AlbumRef> {
    let mut found: Vec<AlbumRef> = albums
        .iter()
        .filter(|a| !a.name.is_empty() && decade_of(a.year) == Some(decade))
        .map(album_ref)
        .collect();
    found.sort_by(|a, b| {
        a.year
            .cmp(&b.year)
            .then_with(|| a.artist.cmp(&b.artist))
            .then_with(|| a.name.cmp(&b.name))
    });
    found
}

/// A history-less Home for providers that don't live in the library
/// database: random picks, decades, recently added albums and favourites
/// (as of `now`, Unix seconds) from the albums in memory.
#[must_use]
pub fn home_from_albums(albums: &[Album], now: i64) -> HomeData {
    let mut decades: Vec<DecadeCount> = Vec::new();
    for album in albums.iter().filter(|a| !a.name.is_empty()) {
        if let Some(decade) = decade_of(album.year) {
            match decades.iter_mut().find(|d| d.decade == decade) {
                Some(entry) => entry.albums += 1,
                None => decades.push(DecadeCount { decade, albums: 1 }),
            }
        }
    }
    decades.sort_by_key(|d| d.decade);

    let mut added: Vec<&Album> = albums
        .iter()
        .filter(|a| !a.name.is_empty() && a.added_at > 0)
        .collect();
    added.sort_by_key(|a| std::cmp::Reverse(a.added_at));
    let recently_added = added
        .into_iter()
        .take(SHELF_LEN as usize)
        .map(|a| AlbumRef {
            timestamp: a.added_at,
            ..album_ref(a)
        })
        .collect();

    let mut favourites: Vec<Track> = albums
        .iter()
        .flat_map(|a| a.tracks.iter())
        .filter(|t| t.is_favorite)
        .cloned()
        .collect();
    favourites.sort_by_key(|t| std::cmp::Reverse(t.favorited_at));
    let favourites_total = favourites.len() as u32;
    let month = month_start(now);
    let favourites_this_month = favourites.iter().any(|t| t.favorited_at >= month);
    if favourites_this_month {
        favourites.retain(|t| t.favorited_at >= month);
    }
    favourites.truncate(FAVOURITES_SHELF_LEN as usize);

    HomeData {
        recently_added,
        favourites,
        favourites_this_month,
        favourites_total,
        random: random_album_refs(albums, SHELF_LEN as usize),
        decades,
        total_albums: albums.len() as u32,
        history_available: false,
        ..HomeData::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn track(album: &str, artist: &str, n: u32) -> Track {
        Track {
            id: 0,
            path: format!("/m/{artist}/{album}/{n}").into(),
            title: format!("t{n}"),
            artist: artist.into(),
            album_artist: artist.into(),
            album: album.into(),
            genre: String::new(),
            track_number: n,
            disc_number: 1,
            year: 0,
            duration: Duration::from_secs(100),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("subsonic"),
            source_uri: String::new(),
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

    fn album(name: &str, artist: &str, year: u32, tracks: u32) -> Album {
        Album::new(
            name.into(),
            artist.into(),
            year,
            (1..=tracks).map(|n| track(name, artist, n)).collect(),
            None,
        )
    }

    fn library() -> Vec<Album> {
        vec![
            album("A", "Alpha", 1975, 2),
            album("B", "Beta", 1984, 3),
            album("C", "Beta", 1989, 1),
            album("D", "Delta", 0, 1),
            album("", "Loose", 1990, 1),
        ]
    }

    #[test]
    fn decade_of_rejects_implausible_years() {
        assert_eq!(decade_of(1984), Some(1980));
        assert_eq!(decade_of(2000), Some(2000));
        assert_eq!(decade_of(0), None);
        assert_eq!(decade_of(1899), None);
    }

    #[test]
    fn artist_tracks_collects_every_album_in_order() {
        let tracks = artist_tracks(&library(), "Beta");
        assert_eq!(tracks.len(), 4);
        assert_eq!(tracks[0].album, "B");
        assert_eq!(tracks[3].album, "C");
        assert!(artist_tracks(&library(), "Nobody").is_empty());
    }

    #[test]
    fn decade_albums_filters_and_sorts() {
        let eighties = decade_albums(&library(), 1980);
        let names: Vec<_> = eighties.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["B", "C"]);
        assert!(decade_albums(&library(), 2000).is_empty());
        // Untitled albums never appear.
        assert!(decade_albums(&library(), 1990).is_empty());
    }

    #[test]
    fn shuffled_is_a_permutation() {
        let mut out = shuffled((0..50).collect::<Vec<u32>>());
        out.sort_unstable();
        assert_eq!(out, (0..50).collect::<Vec<u32>>());
        assert!(shuffled(Vec::<u32>::new()).is_empty());
    }

    #[test]
    fn fallback_home_has_random_picks_and_decades_but_no_history() {
        let home = home_from_albums(&library(), 1_000_000);
        assert!(!home.history_available);
        assert_eq!(home.total_albums, 5);
        assert_eq!(home.random.len(), 4, "untitled album is skipped");
        assert_eq!(
            home.decades,
            [
                DecadeCount {
                    decade: 1970,
                    albums: 1
                },
                DecadeCount {
                    decade: 1980,
                    albums: 2
                },
            ]
        );
        assert!(home.recently_played.is_empty() && home.recently_added.is_empty());
    }

    #[test]
    fn home_cache_is_fresh_until_play_provider_toggle_or_ttl() {
        let t0 = Instant::now();
        let mut cache = HomeCache::default();
        assert!(!cache.is_fresh("local", true, t0), "nothing loaded yet");

        cache.mark_loaded("local", true, t0);
        assert!(cache.is_fresh("local", true, t0 + Duration::from_secs(60)));
        assert!(!cache.is_fresh("mpd", true, t0), "other provider");
        assert!(!cache.is_fresh("local", false, t0), "suggestions toggled");
        assert!(
            !cache.is_fresh("local", true, t0 + HOME_CACHE_TTL + Duration::from_secs(1)),
            "aged out"
        );

        cache.invalidate();
        assert!(!cache.is_fresh("local", true, t0), "play recorded");
        cache.mark_loaded("local", true, t0);
        assert!(cache.is_fresh("local", true, t0));
    }
}
