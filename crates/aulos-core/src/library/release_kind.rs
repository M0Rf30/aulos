// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! What kind of release an album is: album, EP, single, live recording or
//! compilation.
//!
//! The kind comes from the files' release-type tags (MusicBrainz "album
//! type": Vorbis `RELEASETYPE`, ID3 `TXXX:MusicBrainz Album Type`, MP4
//! `----:com.apple.iTunes:MusicBrainz Album Type`) when present. Untagged
//! albums fall back to hints in the album title ("(Live)", "EP", "- Single")
//! and finally to the track count and total duration, the same thresholds
//! streaming catalogues use.

use std::sync::Arc;
use std::time::Duration;

use super::Track;
use crate::fl;

/// A release of at most this many tracks and less than [`SINGLE_MAX_SECS`]
/// is a single.
const SINGLE_MAX_TRACKS: usize = 3;
const SINGLE_MAX_SECS: u64 = 20 * 60;
/// Up to this many tracks and less than [`EP_MAX_SECS`] makes an EP.
const EP_MAX_TRACKS: usize = 6;
const EP_MAX_SECS: u64 = 30 * 60;

/// The kind of release an album is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ReleaseKind {
    /// A regular studio album (and anything the tags classify as none of
    /// the other kinds, e.g. soundtracks).
    #[default]
    Album,
    Ep,
    Single,
    Live,
    /// "Various Artists" compilation.
    Compilation,
}

impl ReleaseKind {
    /// Every kind, in the order chips and summaries list them.
    pub const ALL: [Self; 5] = [
        Self::Album,
        Self::Ep,
        Self::Single,
        Self::Compilation,
        Self::Live,
    ];

    /// The kind a raw release-type tag value names, if it names one.
    ///
    /// The value may carry several types ("album; live", Picard's
    /// multi-valued `RELEASETYPE`): a secondary type beats the primary one,
    /// since a "live album" is first of all live. Other known types
    /// ("remix", "soundtrack", "broadcast", …) count as plain albums; an
    /// empty or unrecognisable value names nothing.
    #[must_use]
    pub fn from_tag(raw: &str) -> Option<Self> {
        // Precedence: live > compilation > ep > single > album.
        fn rank(kind: ReleaseKind) -> u8 {
            match kind {
                ReleaseKind::Live => 4,
                ReleaseKind::Compilation => 3,
                ReleaseKind::Ep => 2,
                ReleaseKind::Single => 1,
                ReleaseKind::Album => 0,
            }
        }
        let lower = raw.to_lowercase();
        let mut found: Option<Self> = None;
        for word in lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            let kind = match word {
                "live" => Self::Live,
                "compilation" => Self::Compilation,
                "ep" => Self::Ep,
                "single" => Self::Single,
                "album" | "soundtrack" | "remix" | "broadcast" | "other" | "audiobook"
                | "spokenword" | "interview" | "demo" | "mixtape" | "dj" => Self::Album,
                _ => continue,
            };
            if found.is_none_or(|f| rank(kind) > rank(f)) {
                found = Some(kind);
            }
        }
        found
    }

    /// The kind an album title hints at: "Live at …", "(Live)", "… EP",
    /// "- Single".
    #[must_use]
    pub fn from_title(title: &str) -> Option<Self> {
        let t = title.trim().to_lowercase();
        let ends = |suffixes: &[&str]| suffixes.iter().any(|s| t.ends_with(s));
        let starts = |prefixes: &[&str]| prefixes.iter().any(|p| t.starts_with(p));
        if starts(&["live at ", "live in ", "live from ", "live @ "])
            || t.contains("(live")
            || t.contains("[live")
            || t.contains(" - live")
            || t.contains("unplugged")
            || ends(&[" live"])
        {
            Some(Self::Live)
        } else if ends(&[" ep", "(ep)", "[ep]", " - ep"]) {
            Some(Self::Ep)
        } else if ends(&[" single", "(single)", "[single]", " - single"]) {
            Some(Self::Single)
        } else {
            None
        }
    }

    /// The kind implied by how big the release is: up to three short tracks
    /// make a single, up to six tracks within half an hour an EP, anything
    /// else an album. `track_count` should be the best estimate of the
    /// release's full size (see [`ReleaseKind::of_album`]).
    #[must_use]
    pub fn from_size(track_count: usize, total: Duration) -> Self {
        let secs = total.as_secs();
        if track_count <= SINGLE_MAX_TRACKS && secs < SINGLE_MAX_SECS {
            Self::Single
        } else if track_count <= EP_MAX_TRACKS && secs < EP_MAX_SECS {
            Self::Ep
        } else {
            Self::Album
        }
    }

    /// Classify an album from its tracks.
    ///
    /// Order of evidence: a "Various Artists" album artist (or a
    /// `compilation` tag) → [`Self::Compilation`]; the first track with a
    /// release-type tag; hints in the album title; then the size. The size
    /// uses the highest track number as the album's size when it exceeds
    /// the number of tracks present, so a partly ripped 12-track album is
    /// not mistaken for a single.
    #[must_use]
    pub fn of_album(title: &str, artist: &str, tracks: &[Arc<Track>]) -> Self {
        let tagged = tracks.iter().find_map(|t| Self::from_tag(&t.release_type));
        if super::compilations::is_various_artists(artist) || tagged == Some(Self::Compilation) {
            return Self::Compilation;
        }
        if let Some(kind) = tagged {
            return kind;
        }
        // A remote album whose tracks are not fetched yet says nothing
        // about its size.
        if tracks.is_empty() {
            return Self::from_title(title).unwrap_or(Self::Album);
        }
        if let Some(kind) = Self::from_title(title) {
            return kind;
        }
        let highest = tracks.iter().map(|t| t.track_number).max().unwrap_or(0) as usize;
        let total = tracks.iter().map(|t| t.duration).sum();
        Self::from_size(tracks.len().max(highest), total)
    }

    /// Localized singular name: "Album", "EP", "Single", "Live",
    /// "Compilation".
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Album => fl!("release-kind-album"),
            Self::Ep => fl!("release-kind-ep"),
            Self::Single => fl!("release-kind-single"),
            Self::Live => fl!("release-kind-live"),
            Self::Compilation => fl!("release-kind-compilation"),
        }
    }

    /// Localized plural name for filter chips: "Albums", "EPs", "Singles",
    /// "Live", "Compilations".
    #[must_use]
    pub fn plural_label(self) -> String {
        match self {
            Self::Album => fl!("filter-albums"),
            Self::Ep => fl!("filter-eps"),
            Self::Single => fl!("filter-singles"),
            Self::Live => fl!("filter-live"),
            Self::Compilation => fl!("filter-compilations"),
        }
    }

    /// Localized "N albums" / "N EPs" / … count of this kind.
    #[must_use]
    pub fn count_label(self, count: usize) -> String {
        let count = count.to_string();
        match self {
            Self::Album => fl!("release-count-album", count = count),
            Self::Ep => fl!("release-count-ep", count = count),
            Self::Single => fl!("release-count-single", count = count),
            Self::Live => fl!("release-count-live", count = count),
            Self::Compilation => fl!("release-count-compilation", count = count),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn track(release_type: &str, number: u32, secs: u64) -> Track {
        Track {
            id: 0,
            path: PathBuf::new(),
            title: String::new(),
            artist: String::new(),
            album_artist: "Band".into(),
            album: "Record".into(),
            genre: String::new(),
            track_number: number,
            disc_number: 1,
            year: 2000,
            duration: Duration::from_secs(secs),
            bitrate: 0,
            sample_rate: 44_100,
            bit_depth: 0,
            provider_id: Arc::from("local"),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
            release_type: release_type.into(),
            added_at: 0,
            favorited_at: 0,
        }
    }

    fn album_of(title: &str, artist: &str, tags: &str, n: u32, secs: u64) -> ReleaseKind {
        let tracks: Vec<Arc<Track>> = (1..=n).map(|i| Arc::new(track(tags, i, secs))).collect();
        ReleaseKind::of_album(title, artist, &tracks)
    }

    #[test]
    fn tag_values_map_to_kinds() {
        assert_eq!(ReleaseKind::from_tag("album"), Some(ReleaseKind::Album));
        assert_eq!(ReleaseKind::from_tag("EP"), Some(ReleaseKind::Ep));
        assert_eq!(ReleaseKind::from_tag("Single"), Some(ReleaseKind::Single));
        assert_eq!(ReleaseKind::from_tag("live"), Some(ReleaseKind::Live));
        assert_eq!(
            ReleaseKind::from_tag("compilation"),
            Some(ReleaseKind::Compilation)
        );
        assert_eq!(
            ReleaseKind::from_tag("soundtrack"),
            Some(ReleaseKind::Album)
        );
        assert_eq!(ReleaseKind::from_tag(""), None);
        assert_eq!(ReleaseKind::from_tag("whatever"), None);
    }

    #[test]
    fn secondary_release_types_win_over_the_primary_one() {
        assert_eq!(
            ReleaseKind::from_tag("album; live"),
            Some(ReleaseKind::Live)
        );
        assert_eq!(
            ReleaseKind::from_tag("album/compilation"),
            Some(ReleaseKind::Compilation)
        );
        assert_eq!(ReleaseKind::from_tag("single ep"), Some(ReleaseKind::Ep));
    }

    #[test]
    fn title_hints() {
        for (title, kind) in [
            ("Live at Wembley", Some(ReleaseKind::Live)),
            ("Songs (Live)", Some(ReleaseKind::Live)),
            ("MTV Unplugged in New York", Some(ReleaseKind::Live)),
            ("Night Moves - Live", Some(ReleaseKind::Live)),
            ("Low Tide EP", Some(ReleaseKind::Ep)),
            ("Low Tide (EP)", Some(ReleaseKind::Ep)),
            ("Hit Song - Single", Some(ReleaseKind::Single)),
            ("Hit Song [Single]", Some(ReleaseKind::Single)),
            ("Alive", None),
            ("Deep", None),
            ("Live and Let Die", None),
            ("Abbey Road", None),
        ] {
            assert_eq!(ReleaseKind::from_title(title), kind, "{title}");
        }
    }

    #[test]
    fn size_thresholds() {
        let mins = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(ReleaseKind::from_size(1, mins(4)), ReleaseKind::Single);
        assert_eq!(ReleaseKind::from_size(3, mins(12)), ReleaseKind::Single);
        // Three long tracks are an album, not a single.
        assert_eq!(ReleaseKind::from_size(3, mins(45)), ReleaseKind::Album);
        assert_eq!(ReleaseKind::from_size(4, mins(18)), ReleaseKind::Ep);
        assert_eq!(ReleaseKind::from_size(6, mins(29)), ReleaseKind::Ep);
        assert_eq!(ReleaseKind::from_size(6, mins(40)), ReleaseKind::Album);
        assert_eq!(ReleaseKind::from_size(7, mins(25)), ReleaseKind::Album);
        assert_eq!(ReleaseKind::from_size(12, mins(45)), ReleaseKind::Album);
    }

    #[test]
    fn album_kind_prefers_tags_then_title_then_size() {
        // Tag beats a 12-track size and a misleading title.
        assert_eq!(
            album_of("Greatest EP", "Band", "single", 12, 240),
            ReleaseKind::Single
        );
        // Title beats size.
        assert_eq!(album_of("Songs EP", "Band", "", 10, 240), ReleaseKind::Ep);
        // Size alone.
        assert_eq!(album_of("Songs", "Band", "", 5, 240), ReleaseKind::Ep);
        assert_eq!(album_of("Songs", "Band", "", 10, 240), ReleaseKind::Album);
        assert_eq!(album_of("Songs", "Band", "", 2, 200), ReleaseKind::Single);
    }

    #[test]
    fn compilations_come_from_the_artist_or_the_tag() {
        assert_eq!(
            album_of("Now 99", "Various Artists", "", 20, 200),
            ReleaseKind::Compilation
        );
        assert_eq!(
            album_of("Best of", "Band", "album; compilation", 20, 200),
            ReleaseKind::Compilation
        );
    }

    #[test]
    fn an_album_without_tracks_is_not_sized() {
        assert_eq!(
            ReleaseKind::of_album("Songs", "Band", &[]),
            ReleaseKind::Album
        );
        assert_eq!(
            ReleaseKind::of_album("Songs EP", "Band", &[]),
            ReleaseKind::Ep
        );
    }

    #[test]
    fn a_partly_present_album_is_not_a_single() {
        // Two tracks numbered 3 and 11 of a longer album.
        let tracks = vec![Arc::new(track("", 3, 200)), Arc::new(track("", 11, 200))];
        assert_eq!(
            ReleaseKind::of_album("Songs", "Band", &tracks),
            ReleaseKind::Album
        );
    }
}
