// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Settings search: the query matcher and the searchable text of every
//! settings section, so any UI filters its sections the same way.

use crate::online::scrobble::import::normalize_text;

/// A settings search query: whitespace-separated tokens that must all occur
/// (case, diacritics and punctuation ignored) somewhere in a section's text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    tokens: Vec<String>,
}

impl SearchQuery {
    #[must_use]
    pub fn new(raw: &str) -> Self {
        let tokens = normalize_text(raw)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        Self { tokens }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Whether a section whose searchable strings are `haystack` matches.
    /// An empty query matches everything.
    #[must_use]
    pub fn matches<S: AsRef<str>>(&self, haystack: &[S]) -> bool {
        if self.tokens.is_empty() {
            return true;
        }
        let joined: Vec<&str> = haystack.iter().map(AsRef::as_ref).collect();
        let text = normalize_text(&joined.join(" "));
        self.tokens.iter().all(|t| text.contains(t.as_str()))
    }
}

/// Searchable strings (titles and descriptions) of each settings section.
pub mod keywords {
    use crate::fl;

    pub fn library() -> Vec<String> {
        vec![
            fl!("settings-library"),
            fl!("settings-library-description"),
            fl!("add-music-folder"),
        ]
    }

    pub fn artist_tags() -> Vec<String> {
        vec![
            fl!("settings-artist-tags"),
            fl!("settings-artist-tags-description"),
            fl!("split-artist-tags"),
            fl!("split-artist-tags-description"),
            fl!("artist-tag-delimiters"),
            fl!("artist-tag-delimiters-description"),
        ]
    }

    pub fn compilations() -> Vec<String> {
        vec![
            fl!("settings-compilations"),
            fl!("settings-compilations-description"),
            fl!("show-compilations-in-artists"),
            fl!("show-compilations-in-artists-description"),
        ]
    }

    pub fn playlists() -> Vec<String> {
        vec![
            fl!("settings-playlists"),
            fl!("settings-playlists-description"),
            fl!("m3u-relative-paths"),
            fl!("m3u-relative-paths-description"),
        ]
    }

    pub fn playback() -> Vec<String> {
        vec![
            fl!("settings-playback"),
            fl!("settings-playback-description"),
            fl!("crossfade-duration"),
            fl!("crossfade-description"),
            fl!("replay-gain"),
            fl!("replay-gain-description"),
            fl!("volume"),
        ]
    }

    pub fn playback_extras() -> Vec<String> {
        vec![
            fl!("settings-playback-extras"),
            fl!("settings-playback-extras-description"),
            fl!("fade-duration"),
            fl!("fade-description"),
            fl!("auto-play"),
            fl!("auto-play-description"),
            fl!("party-mode"),
            fl!("party-mode-description"),
            fl!("party-genres"),
            fl!("settings-desktop"),
            fl!("settings-desktop-description"),
            fl!("notify-track-change"),
            fl!("notify-track-change-description"),
            fl!("inhibit-suspend"),
            fl!("inhibit-suspend-description"),
            fl!("background-playback"),
            fl!("background-playback-description"),
        ]
    }

    pub fn startup() -> Vec<String> {
        vec![
            fl!("settings-startup"),
            fl!("startup-page"),
            fl!("startup-page-description"),
            fl!("startup-provider"),
            fl!("startup-provider-description"),
        ]
    }

    pub fn appearance() -> Vec<String> {
        vec![
            fl!("settings-appearance"),
            fl!("settings-appearance-description"),
            fl!("grid-size"),
            fl!("grid-size-description"),
        ]
    }

    pub fn artist_info() -> Vec<String> {
        vec![
            fl!("settings-artist-info"),
            fl!("fetch-artist-info"),
            fl!("fetch-artist-info-description"),
        ]
    }

    pub fn experimental() -> Vec<String> {
        vec![
            fl!("settings-experimental"),
            fl!("experimental-converter"),
            fl!("experimental-converter-description"),
        ]
    }

    pub fn tagger() -> Vec<String> {
        vec![
            fl!("settings-tagger"),
            fl!("settings-tagger-description"),
            fl!("settings-tagger-app-key"),
            fl!("settings-tagger-user-key"),
            fl!("settings-tagger-caa-cover"),
        ]
    }

    pub fn about() -> Vec<String> {
        vec![
            fl!("settings-about"),
            fl!("about"),
            fl!("settings-about-description"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        let q = SearchQuery::new("   ");
        assert!(q.is_empty());
        assert!(q.matches(&["anything"]));
        assert!(q.matches::<&str>(&[]));
    }

    #[test]
    fn query_words_must_all_match_ignoring_case_and_diacritics() {
        let hay = ["Playback", "Crossfade duration", "Réplay gain"];
        assert!(SearchQuery::new("cross").matches(&hay));
        assert!(SearchQuery::new("CROSSFADE gain").matches(&hay));
        assert!(SearchQuery::new("replay").matches(&hay));
        assert!(!SearchQuery::new("crossfade scrobble").matches(&hay));
        assert!(!SearchQuery::new("zzz").matches(&hay));
    }

    #[test]
    fn punctuation_in_names_is_ignored() {
        let hay = ["Last.fm", "Scrobble to Last.fm"];
        assert!(SearchQuery::new("last.fm").matches(&hay));
        assert!(SearchQuery::new("fm").matches(&hay));
    }
}
