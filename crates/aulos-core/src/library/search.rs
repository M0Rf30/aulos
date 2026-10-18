// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Normalised search keys and debounce helpers for the library search.
//!
//! Normalising (case, diacritics, punctuation) every album/artist/track
//! string on each keystroke allocated ~3 strings per track. The keys are
//! built once per library generation and reused until the library changes.
//! A query is split into words ([`query_tokens`]) that must all occur in a
//! key, in any order and across its fields: `"jackson thriller"` finds the
//! album Thriller by Michael Jackson, and `"beyonce"` finds Beyoncé.

use super::{Album, Artist, Track};
use crate::online::scrobble::import::normalize_text;
use std::sync::Arc;

/// Delay between the last keystroke and the library filter pass.
pub const SEARCH_DEBOUNCE_MS: u64 = 150;

#[derive(Default)]
pub struct SearchIndex {
    built_gen: Option<u64>,
    albums: Vec<String>,
    artists: Vec<String>,
    tracks: Vec<String>,
}

impl SearchIndex {
    /// Rebuild the keys if `generation` differs from the one they were built for.
    pub fn ensure(
        &mut self,
        generation: u64,
        albums: &[Arc<Album>],
        artists: &[Artist],
        tracks: &[Arc<Track>],
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
            .map(|a| normalize_text(&format!("{} {}", a.name, a.artist)))
            .collect();
        self.artists = artists.iter().map(|a| normalize_text(&a.name)).collect();
        self.tracks = tracks
            .iter()
            .map(|t| normalize_text(&format!("{} {} {}", t.title, t.artist, t.album)))
            .collect();
    }

    #[must_use]
    pub fn album_matches(&self, i: usize, tokens: &[String]) -> bool {
        self.albums.get(i).is_some_and(|k| contains_all(k, tokens))
    }

    #[must_use]
    pub fn artist_matches(&self, i: usize, tokens: &[String]) -> bool {
        self.artists.get(i).is_some_and(|k| contains_all(k, tokens))
    }

    #[must_use]
    pub fn track_matches(&self, i: usize, tokens: &[String]) -> bool {
        self.tracks.get(i).is_some_and(|k| contains_all(k, tokens))
    }
}

/// The words of a typed query, normalised like the keys.
#[must_use]
pub fn query_tokens(query: &str) -> Vec<String> {
    normalize_text(query)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Whether the normalised `text` contains every one of `tokens`.
#[must_use]
pub fn contains_all(text: &str, tokens: &[String]) -> bool {
    tokens.iter().all(|t| text.contains(t.as_str()))
}

/// Whether a delayed debounce message is still the latest one issued.
#[must_use]
pub fn is_latest_generation(pending: u64, latest: u64) -> bool {
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
        let q = query_tokens("a");
        assert!(!idx.album_matches(0, &q));
        assert!(!idx.track_matches(0, &q));
        assert!(!idx.artist_matches(0, &q));
    }

    #[test]
    fn words_match_in_any_order_across_fields() {
        let tokens = |q: &str| query_tokens(q);
        let key = normalize_text("Thriller Michael Jackson");
        assert!(contains_all(&key, &tokens("jackson thriller")));
        assert!(contains_all(&key, &tokens("  mich  thr ")));
        assert!(!contains_all(&key, &tokens("thriller queen")));
    }

    #[test]
    fn case_diacritics_and_punctuation_are_ignored() {
        let key = normalize_text("Halo Beyoncé");
        assert!(contains_all(&key, &query_tokens("BEYONCE")));
        assert!(contains_all(
            &normalize_text("AC/DC"),
            &query_tokens("ac/dc")
        ));
        assert!(contains_all(
            &normalize_text("Sigur Rós"),
            &query_tokens("ros")
        ));
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
