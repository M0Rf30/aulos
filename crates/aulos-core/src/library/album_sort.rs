// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Sort order of the Albums, Artists and Genres pages (the toolbar's sort
//! control): recently added, title, artist or year.
//!
//! Each page keeps its own [`AlbumSort`] in the config; the library state
//! re-sorts its lists with these functions whenever the data or the
//! selection changes.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Album, Artist, Track};
use crate::fl;

/// How a browse page orders its items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AlbumSort {
    /// Newest additions to the library first.
    #[default]
    RecentlyAdded,
    /// By title (artists and genres: by name), A to Z.
    Title,
    /// By artist, then title. Genres have no artist and sort by name.
    Artist,
    /// Newest release year first; items without a year last.
    Year,
}

impl AlbumSort {
    /// Every order, in the order the toolbar menu lists them.
    pub const ALL: [Self; 4] = [Self::RecentlyAdded, Self::Title, Self::Artist, Self::Year];

    /// Localized name: "Recently added", "Title", "Artist", "Year".
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::RecentlyAdded => fl!("sort-recently-added"),
            Self::Title => fl!("sort-title"),
            Self::Artist => fl!("sort-artist"),
            Self::Year => fl!("sort-year"),
        }
    }

    /// Localized short name for a narrow toolbar ("Added" for recently
    /// added; the others are short already).
    #[must_use]
    pub fn short_label(self) -> String {
        match self {
            Self::RecentlyAdded => fl!("sort-added"),
            other => other.label(),
        }
    }
}

/// Case-insensitive ordering key of a name.
fn name_key(name: &str) -> String {
    name.to_lowercase()
}

/// Newest year first, `0` (unknown) last.
fn year_key(year: u32) -> Reverse<u32> {
    Reverse(year)
}

/// Sort `albums` by `sort`. Ties fall back to the title, so the order is
/// stable across reloads.
pub fn sort_albums(albums: &mut [Arc<Album>], sort: AlbumSort) {
    match sort {
        AlbumSort::RecentlyAdded => {
            albums.sort_by_cached_key(|a| (Reverse(a.added_at), name_key(&a.name)));
        }
        AlbumSort::Title => albums.sort_by_cached_key(|a| name_key(&a.name)),
        AlbumSort::Artist => {
            albums.sort_by_cached_key(|a| (name_key(&a.artist), name_key(&a.name)));
        }
        AlbumSort::Year => albums.sort_by_cached_key(|a| (year_key(a.year), name_key(&a.name))),
    }
}

/// Sort `artists` by `sort`: by name (title and artist orders), by their
/// newest album's addition date, or by their newest album's year.
pub fn sort_artists(artists: &mut [Artist], sort: AlbumSort) {
    let newest_added = |a: &Artist| a.albums.iter().map(|al| al.added_at).max().unwrap_or(0);
    let newest_year = |a: &Artist| a.albums.iter().map(|al| al.year).max().unwrap_or(0);
    match sort {
        AlbumSort::RecentlyAdded => {
            artists.sort_by_cached_key(|a| (Reverse(newest_added(a)), name_key(&a.name)));
        }
        AlbumSort::Title | AlbumSort::Artist => {
            artists.sort_by_cached_key(|a| name_key(&a.name));
        }
        AlbumSort::Year => {
            artists.sort_by_cached_key(|a| (year_key(newest_year(a)), name_key(&a.name)));
        }
    }
}

/// Sort `genres` by `sort`, judging recency and year by the `tracks` that
/// carry each genre.
pub fn sort_genres(genres: &mut [String], tracks: &[Arc<Track>], sort: AlbumSort) {
    if matches!(sort, AlbumSort::Title | AlbumSort::Artist) {
        genres.sort_by_cached_key(|g| name_key(g));
        return;
    }
    // genre -> (newest added_at, newest year)
    let mut newest: HashMap<&str, (i64, u32)> = HashMap::new();
    for t in tracks.iter().filter(|t| !t.genre.is_empty()) {
        let e = newest.entry(t.genre.as_str()).or_default();
        e.0 = e.0.max(t.added_at);
        e.1 = e.1.max(t.year);
    }
    let of = |g: &str| newest.get(g).copied().unwrap_or_default();
    match sort {
        AlbumSort::RecentlyAdded => {
            genres.sort_by_cached_key(|g| (Reverse(of(g).0), name_key(g)));
        }
        _ => genres.sort_by_cached_key(|g| (year_key(of(g).1), name_key(g))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(name: &str, artist: &str, year: u32, added_at: i64) -> Arc<Album> {
        let mut a = Album::new(name.into(), artist.into(), year, Vec::new(), None);
        a.added_at = added_at;
        Arc::new(a)
    }

    fn names(albums: &[Arc<Album>]) -> Vec<&str> {
        albums.iter().map(|a| a.name.as_str()).collect()
    }

    fn sample() -> Vec<Arc<Album>> {
        vec![
            album("beta", "Zed", 1999, 10),
            album("Alpha", "Yan", 2020, 30),
            album("Gamma", "Xi", 0, 20),
            album("Delta", "Yan", 2020, 0),
        ]
    }

    #[test]
    fn albums_sort_by_each_order() {
        let mut a = sample();
        sort_albums(&mut a, AlbumSort::RecentlyAdded);
        assert_eq!(names(&a), ["Alpha", "Gamma", "beta", "Delta"]);
        sort_albums(&mut a, AlbumSort::Title);
        assert_eq!(names(&a), ["Alpha", "beta", "Delta", "Gamma"]);
        sort_albums(&mut a, AlbumSort::Artist);
        assert_eq!(names(&a), ["Gamma", "Alpha", "Delta", "beta"]);
        sort_albums(&mut a, AlbumSort::Year);
        assert_eq!(names(&a), ["Alpha", "Delta", "beta", "Gamma"]);
    }

    #[test]
    fn artists_sort_by_name_recency_or_newest_year() {
        let mut artists = vec![
            Artist::new("Old".into(), vec![album("a", "Old", 1980, 5)]),
            Artist::new(
                "New".into(),
                vec![album("b", "New", 1970, 1), album("c", "New", 2023, 50)],
            ),
            Artist::new("Mid".into(), vec![album("d", "Mid", 2000, 20)]),
        ];
        let order = |a: &[Artist]| a.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
        sort_artists(&mut artists, AlbumSort::Title);
        assert_eq!(order(&artists), ["Mid", "New", "Old"]);
        sort_artists(&mut artists, AlbumSort::RecentlyAdded);
        assert_eq!(order(&artists), ["New", "Mid", "Old"]);
        sort_artists(&mut artists, AlbumSort::Year);
        assert_eq!(order(&artists), ["New", "Mid", "Old"]);
    }

    #[test]
    fn genres_sort_by_the_tracks_that_carry_them() {
        let track = |genre: &str, year: u32, added_at: i64| Track {
            id: 0,
            path: std::path::PathBuf::new(),
            title: String::new(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            genre: genre.into(),
            track_number: 0,
            disc_number: 0,
            year,
            duration: std::time::Duration::ZERO,
            bitrate: 0,
            sample_rate: 0,
            bit_depth: 0,
            provider_id: std::sync::Arc::from("local"),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
            release_type: String::new(),
            added_at,
            favorited_at: 0,
        };
        let tracks = vec![
            Arc::new(track("Rock", 1975, 5)),
            Arc::new(track("Jazz", 1960, 9)),
            Arc::new(track("Pop", 2010, 1)),
        ];
        let mut genres: Vec<String> = ["Rock", "Jazz", "Pop"].map(String::from).to_vec();
        sort_genres(&mut genres, &tracks, AlbumSort::Title);
        assert_eq!(genres, ["Jazz", "Pop", "Rock"]);
        sort_genres(&mut genres, &tracks, AlbumSort::RecentlyAdded);
        assert_eq!(genres, ["Jazz", "Rock", "Pop"]);
        sort_genres(&mut genres, &tracks, AlbumSort::Year);
        assert_eq!(genres, ["Pop", "Rock", "Jazz"]);
    }
}
