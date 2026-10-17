// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Text and presentation rules for library entities that every UI needs but
//! none should re-derive: which artist line an album card shows, track-count
//! captions, the genres of an album, the biography preview. Kept here, on the
//! types themselves, so the Albums/Artists/Playlists views (and any future
//! front-end) stay thin widget code.

use super::Playlist;
use super::history::AlbumRef;
use super::release_kind::ReleaseKind;
use super::{Album, Artist, Track};
use crate::fl;
use crate::format::format_duration_coarse;

/// Max characters shown before a biography is clipped with a "show more"
/// link — long enough for a couple of sentences, short enough that the
/// detail view's header stays above the fold.
pub const BIO_PREVIEW_CHARS: usize = 320;

/// Whether an album's artist is worth showing next to its title: not empty
/// and not just an echo of the title (self-titled albums).
#[must_use]
pub fn has_distinct_artist(artist: &str, album_name: &str) -> bool {
    !artist.trim().is_empty() && !artist.trim().eq_ignore_ascii_case(album_name.trim())
}

/// English `"N noun"` with a trailing `s` unless `count` is 1.
fn counted(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// A biography clipped for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BioPreview {
    /// The text to show: the full biography, or a character-capped preview
    /// with a trailing ellipsis.
    pub text: String,
    /// Whether the biography is long enough to need a show-more/show-less
    /// toggle.
    pub is_long: bool,
}

/// Clips `bio` to [`BIO_PREVIEW_CHARS`] unless `expanded` (or it is short
/// enough already).
#[must_use]
pub fn bio_preview(bio: &str, expanded: bool) -> BioPreview {
    let is_long = bio.chars().count() > BIO_PREVIEW_CHARS;
    let text = if expanded || !is_long {
        bio.to_string()
    } else {
        let mut preview: String = bio.chars().take(BIO_PREVIEW_CHARS).collect();
        preview.push('…');
        preview
    };
    BioPreview { text, is_long }
}

impl Track {
    /// Number shown in an album's track list: the tag's track number, or the
    /// 1-based `position` in the list when the track has none.
    #[must_use]
    pub fn display_number(&self, position: usize) -> String {
        if self.track_number > 0 {
            self.track_number.to_string()
        } else {
            (position + 1).to_string()
        }
    }
}

impl Album {
    /// See [`has_distinct_artist`].
    #[must_use]
    pub fn has_distinct_artist(&self) -> bool {
        has_distinct_artist(&self.artist, &self.name)
    }

    /// Distinct genres of the album's tracks, sorted. Tags often pack several
    /// genres into one field ("Trance; Electronic"), so each field is split
    /// into single genres.
    #[must_use]
    pub fn genres(&self) -> Vec<String> {
        let mut genres: Vec<String> = self
            .tracks
            .iter()
            .flat_map(|t| t.genre.split([';', ',', '/']))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        genres.sort();
        genres.dedup();
        genres
    }

    /// English "N tracks" label for list rows and the detail header.
    #[must_use]
    pub fn tracks_label(&self) -> String {
        counted(self.track_count(), "track")
    }

    /// Coarse total duration, e.g. "1h 20m".
    #[must_use]
    pub fn duration_label(&self) -> String {
        format_duration_coarse(self.total_secs)
    }

    /// Localized "{year} · N tracks" (or just "N tracks") summary, used for
    /// an album listed under its artist.
    #[must_use]
    pub fn track_summary(&self) -> String {
        let n = self.track_count();
        let track_str = if n == 1 {
            fl!("artist-track-count-one", count = n.to_string())
        } else {
            fl!("artist-track-count-other", count = n.to_string())
        };
        if self.year > 0 {
            format!("{} · {}", self.year, track_str)
        } else {
            track_str
        }
    }

    /// Localized "{year} · {kind}" line ("2019 · EP", or just the kind when
    /// the year is unknown), shown under an album listed on an artist page.
    #[must_use]
    pub fn year_kind_label(&self) -> String {
        if self.year > 0 {
            format!("{} · {}", self.year, self.kind.label())
        } else {
            self.kind.label()
        }
    }

    /// Whether any track's artist differs from the album artist (compilations
    /// and split albums), i.e. whether a per-track artist column is useful.
    #[must_use]
    pub fn has_varied_artists(&self) -> bool {
        self.tracks.iter().any(|t| t.artist != self.artist)
    }

    /// Whether the album's files can be edited in the tag editor (judged by
    /// its first track; an album lives in one provider).
    #[must_use]
    pub fn is_taggable(&self) -> bool {
        self.tracks
            .first()
            .is_some_and(|t| crate::tagger::is_taggable(t))
    }
}

impl AlbumRef {
    /// See [`has_distinct_artist`].
    #[must_use]
    pub fn has_distinct_artist(&self) -> bool {
        has_distinct_artist(&self.artist, &self.name)
    }
}

impl Artist {
    /// How many releases of each kind the artist has, in
    /// [`ReleaseKind::ALL`] order, skipping kinds with none.
    #[must_use]
    pub fn release_counts(&self) -> Vec<(ReleaseKind, usize)> {
        ReleaseKind::ALL
            .into_iter()
            .map(|kind| (kind, self.albums.iter().filter(|a| a.kind == kind).count()))
            .filter(|&(_, n)| n > 0)
            .collect()
    }

    /// Localized releases breakdown for list rows: "2 albums · 1 EP".
    #[must_use]
    pub fn albums_label(&self) -> String {
        self.release_counts()
            .into_iter()
            .map(|(kind, n)| kind.count_label(n))
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// English "N tracks" label for list rows.
    #[must_use]
    pub fn tracks_label(&self) -> String {
        counted(self.track_count(), "track")
    }

    /// Coarse total duration, e.g. "1h 20m".
    #[must_use]
    pub fn duration_label(&self) -> String {
        format_duration_coarse(self.total_secs)
    }

    /// Localized "N albums, M tracks" summary line.
    #[must_use]
    pub fn summary(&self) -> String {
        let albums = self.album_count();
        let tracks = self.track_count();
        let album_str = if albums == 1 {
            fl!("artist-album-count-one", count = albums.to_string())
        } else {
            fl!("artist-album-count-other", count = albums.to_string())
        };
        let track_str = if tracks == 1 {
            fl!("artist-track-count-one", count = tracks.to_string())
        } else {
            fl!("artist-track-count-other", count = tracks.to_string())
        };
        format!("{album_str}, {track_str}")
    }
}

impl Playlist {
    /// Localized "N tracks" label.
    #[must_use]
    pub fn track_count_label(&self) -> String {
        if self.track_count == 1 {
            fl!(
                "playlist-track-count-one",
                count = self.track_count.to_string()
            )
        } else {
            fl!(
                "playlist-track-count-other",
                count = self.track_count.to_string()
            )
        }
    }

    /// Coarse total duration, e.g. "1h 20m".
    #[must_use]
    pub fn duration_label(&self) -> String {
        format_duration_coarse(self.total_duration.as_secs())
    }

    /// Caption "N tracks  -  1h 20m" for list rows and the detail header.
    #[must_use]
    pub fn summary(&self) -> String {
        format!("{}  -  {}", self.track_count_label(), self.duration_label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(artist: &str, genre: &str, number: u32) -> Track {
        Track {
            id: 0,
            path: std::path::PathBuf::new(),
            title: String::new(),
            artist: artist.into(),
            album_artist: String::new(),
            album: String::new(),
            genre: genre.into(),
            track_number: number,
            disc_number: 0,
            year: 0,
            duration: std::time::Duration::ZERO,
            bitrate: 0,
            sample_rate: 0,
            provider_id: std::sync::Arc::from("test"),
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

    #[test]
    fn distinct_artist_ignores_empty_and_title_echo() {
        assert!(has_distinct_artist("Band", "Record"));
        assert!(!has_distinct_artist("  ", "Record"));
        assert!(!has_distinct_artist("Weezer", " weezer "));
    }

    #[test]
    fn album_genres_are_split_sorted_and_deduped() {
        let album = Album::new(
            "A".into(),
            "B".into(),
            0,
            vec![
                track("B", "Trance; Electronic", 0),
                track("B", "electronic, Trance", 0),
                track("B", "", 0),
            ],
            None,
        );
        assert_eq!(album.genres(), vec!["Electronic", "Trance", "electronic"]);
    }

    #[test]
    fn display_number_falls_back_to_position() {
        assert_eq!(track("a", "", 7).display_number(0), "7");
        assert_eq!(track("a", "", 0).display_number(2), "3");
    }

    #[test]
    fn varied_artists_detects_guest_tracks() {
        let same = Album::new("A".into(), "B".into(), 0, vec![track("B", "", 1)], None);
        assert!(!same.has_varied_artists());
        let mixed = Album::new(
            "A".into(),
            "B".into(),
            0,
            vec![track("B", "", 1), track("C", "", 2)],
            None,
        );
        assert!(mixed.has_varied_artists());
    }

    #[test]
    fn english_plurals() {
        assert_eq!(counted(1, "track"), "1 track");
        assert_eq!(counted(0, "track"), "0 tracks");
        assert_eq!(counted(12, "album"), "12 albums");
    }

    #[test]
    fn bio_is_clipped_only_when_long_and_collapsed() {
        let short = bio_preview("short", false);
        assert_eq!(
            short,
            BioPreview {
                text: "short".into(),
                is_long: false
            }
        );

        let long = "x".repeat(BIO_PREVIEW_CHARS + 10);
        let clipped = bio_preview(&long, false);
        assert!(clipped.is_long);
        assert_eq!(clipped.text.chars().count(), BIO_PREVIEW_CHARS + 1);
        assert!(clipped.text.ends_with('…'));

        let full = bio_preview(&long, true);
        assert!(full.is_long);
        assert_eq!(full.text, long);
    }
}
