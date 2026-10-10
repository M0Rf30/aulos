// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Pre-lowercased search keys and debounce helpers for the library search.
//!
//! Lowercasing every album/artist/track string on each keystroke allocated
//! ~3 strings per track. The keys are built once per library generation and
//! reused until the library changes.

use crate::library::{Album, Artist, Track};

/// Separator between fields inside a combined key (never typed in a query).
const SEP: char = '\u{1}';

/// Delay between the last keystroke and the library filter pass.
pub(super) const SEARCH_DEBOUNCE_MS: u64 = 150;

#[derive(Default)]
pub(super) struct SearchIndex {
    built_gen: Option<u64>,
    albums: Vec<String>,
    artists: Vec<String>,
    tracks: Vec<String>,
}

impl SearchIndex {
    /// Rebuild the keys if `generation` differs from the one they were built for.
    pub(super) fn ensure(
        &mut self,
        generation: u64,
        albums: &[Album],
        artists: &[Artist],
        tracks: &[Track],
    ) {
        if self.built_gen == Some(generation)
            && self.albums.len() == albums.len()
            && self.artists.len() == artists.len()
            && self.tracks.len() == tracks.len()
        {
            return;
        }
        self.built_gen = Some(generation);
        self.albums = albums
            .iter()
            .map(|a| format!("{}{SEP}{}", a.name.to_lowercase(), a.artist.to_lowercase()))
            .collect();
        self.artists = artists.iter().map(|a| a.name.to_lowercase()).collect();
        self.tracks = tracks
            .iter()
            .map(|t| {
                format!(
                    "{}{SEP}{}{SEP}{}",
                    t.title.to_lowercase(),
                    t.artist.to_lowercase(),
                    t.album.to_lowercase()
                )
            })
            .collect();
    }

    pub(super) fn album_matches(&self, i: usize, query: &str) -> bool {
        self.albums.get(i).is_some_and(|k| k.contains(query))
    }

    pub(super) fn artist_matches(&self, i: usize, query: &str) -> bool {
        self.artists.get(i).is_some_and(|k| k.contains(query))
    }

    pub(super) fn track_matches(&self, i: usize, query: &str) -> bool {
        self.tracks.get(i).is_some_and(|k| k.contains(query))
    }
}

/// Whether a delayed debounce message is still the latest one issued.
pub(super) fn is_latest_generation(pending: u64, latest: u64) -> bool {
    pending == latest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_only_latest_generation_fires() {
        assert!(is_latest_generation(3, 3));
        assert!(!is_latest_generation(2, 3));
    }

    #[test]
    fn empty_index_matches_nothing() {
        let idx = SearchIndex::default();
        assert!(!idx.album_matches(0, "a"));
        assert!(!idx.track_matches(0, "a"));
        assert!(!idx.artist_matches(0, "a"));
    }

    #[test]
    fn ensure_is_idempotent_for_same_generation() {
        let mut idx = SearchIndex::default();
        idx.ensure(1, &[], &[], &[]);
        assert_eq!(idx.built_gen, Some(1));
        idx.ensure(1, &[], &[], &[]);
        idx.ensure(2, &[], &[], &[]);
        assert_eq!(idx.built_gen, Some(2));
    }
}
