// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! MusicBrainz web-service JSON model (`fmt=json`, `/ws/2/`), reduced to the
//! fields the tagger needs. Every field is optional on the wire — search
//! results, release lookups and the releases nested in a recording lookup
//! carry different subsets of the same objects — so everything defaults.

use serde::{Deserialize, Deserializer};

/// Treats an explicit JSON `null` like a missing field.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
    #[serde(rename = "sort-name")]
    pub sort_name: String,
    pub disambiguation: String,
}

/// One element of an artist credit: the name as credited, the phrase that
/// joins it to the next element, and the artist it points at.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CreditName {
    pub name: String,
    pub joinphrase: String,
    pub artist: ArtistRef,
}

/// The credited text of an artist credit (`"A feat. B"`).
#[must_use]
pub fn credit_text(credit: &[CreditName]) -> String {
    credit
        .iter()
        .map(|c| {
            let name = if c.name.is_empty() {
                &c.artist.name
            } else {
                &c.name
            };
            format!("{name}{}", c.joinphrase)
        })
        .collect()
}

/// The sort form of an artist credit: each artist's sort name with the
/// same join phrases.
#[must_use]
pub fn credit_sort_text(credit: &[CreditName]) -> String {
    credit
        .iter()
        .map(|c| {
            let name = if c.artist.sort_name.is_empty() {
                &c.artist.name
            } else {
                &c.artist.sort_name
            };
            format!("{name}{}", c.joinphrase)
        })
        .collect()
}

/// The artists' own names (not the credited-as names), in order.
#[must_use]
pub fn credit_artist_names(credit: &[CreditName]) -> Vec<String> {
    credit
        .iter()
        .map(|c| {
            if c.artist.name.is_empty() {
                c.name.clone()
            } else {
                c.artist.name.clone()
            }
        })
        .collect()
}

/// The artists' MBIDs, in order.
#[must_use]
pub fn credit_artist_ids(credit: &[CreditName]) -> Vec<String> {
    credit.iter().map(|c| c.artist.id.clone()).collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ReleaseGroup {
    pub id: String,
    pub title: String,
    #[serde(rename = "primary-type")]
    pub primary_type: Option<String>,
    #[serde(rename = "secondary-types", deserialize_with = "null_default")]
    pub secondary_types: Vec<String>,
    #[serde(rename = "first-release-date")]
    pub first_release_date: Option<String>,
    #[serde(rename = "artist-credit", deserialize_with = "null_default")]
    pub artist_credit: Vec<CreditName>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Label {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LabelInfo {
    #[serde(rename = "catalog-number")]
    pub catalog_number: Option<String>,
    pub label: Option<Label>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Area {
    pub name: String,
    #[serde(rename = "iso-3166-1-codes", deserialize_with = "null_default")]
    pub iso_3166_1_codes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ReleaseEvent {
    pub date: Option<String>,
    pub area: Option<Area>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TextRepresentation {
    /// ISO 639-3 (`eng`).
    pub language: Option<String>,
    /// ISO 15924 (`Latn`).
    pub script: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CoverArtArchive {
    pub front: bool,
    pub back: bool,
    pub count: u32,
}

/// A recording, as nested in a track or returned by a recording lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Recording {
    pub id: String,
    pub title: String,
    /// Milliseconds.
    pub length: Option<u64>,
    #[serde(deserialize_with = "null_default")]
    pub isrcs: Vec<String>,
    #[serde(rename = "artist-credit", deserialize_with = "null_default")]
    pub artist_credit: Vec<CreditName>,
    #[serde(rename = "first-release-date")]
    pub first_release_date: Option<String>,
    /// Only present on recording lookups with `inc=releases`.
    #[serde(deserialize_with = "null_default")]
    pub releases: Vec<Release>,
}

/// One track of a medium.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Track {
    /// Release-track MBID (`musicbrainz_trackid`).
    pub id: String,
    /// The printed number: `"3"`, or `"A1"` on vinyl.
    pub number: Option<String>,
    /// 1-based position within the medium.
    pub position: u32,
    pub title: String,
    /// Milliseconds.
    pub length: Option<u64>,
    #[serde(rename = "artist-credit", deserialize_with = "null_default")]
    pub artist_credit: Vec<CreditName>,
    pub recording: Recording,
}

impl Track {
    /// The artist credit of this track, falling back to its recording's.
    #[must_use]
    pub fn credit(&self) -> &[CreditName] {
        if self.artist_credit.is_empty() {
            &self.recording.artist_credit
        } else {
            &self.artist_credit
        }
    }

    /// The track title, falling back to its recording's.
    #[must_use]
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            &self.recording.title
        } else {
            &self.title
        }
    }

    /// Length in milliseconds: the track's own, else its recording's.
    #[must_use]
    pub fn length_ms(&self) -> Option<u64> {
        self.length.or(self.recording.length).filter(|&l| l > 0)
    }
}

/// One medium (disc, side pair...) of a release.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Medium {
    /// 1-based position within the release (the disc number).
    pub position: u32,
    /// The disc's own title (`discsubtitle`).
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    pub format: Option<String>,
    /// Tracks on the medium. Search results carry only this count.
    #[serde(rename = "track-count")]
    pub track_count: u32,
    #[serde(deserialize_with = "null_default")]
    pub tracks: Vec<Track>,
}

impl Medium {
    /// Track count, from the tracks themselves when loaded.
    #[must_use]
    pub fn len(&self) -> usize {
        if self.tracks.is_empty() {
            self.track_count as usize
        } else {
            self.tracks.len()
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A release: full when fetched with [`super::release`], partial when it
/// comes from a search or is nested in a recording lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Release {
    pub id: String,
    pub title: String,
    pub status: Option<String>,
    pub date: Option<String>,
    pub country: Option<String>,
    pub barcode: Option<String>,
    pub asin: Option<String>,
    pub disambiguation: Option<String>,
    pub packaging: Option<String>,
    #[serde(rename = "text-representation")]
    pub text_representation: TextRepresentation,
    #[serde(rename = "artist-credit", deserialize_with = "null_default")]
    pub artist_credit: Vec<CreditName>,
    #[serde(rename = "release-group")]
    pub release_group: Option<ReleaseGroup>,
    #[serde(rename = "label-info", deserialize_with = "null_default")]
    pub label_info: Vec<LabelInfo>,
    #[serde(rename = "release-events", deserialize_with = "null_default")]
    pub release_events: Vec<ReleaseEvent>,
    #[serde(deserialize_with = "null_default")]
    pub media: Vec<Medium>,
    #[serde(rename = "cover-art-archive")]
    pub cover_art_archive: Option<CoverArtArchive>,
    /// Search relevance, 0-100 (search results only).
    pub score: Option<u32>,
    /// Total tracks over all media (search results only).
    #[serde(rename = "track-count")]
    pub search_track_count: Option<u32>,
}

impl Release {
    /// Total tracks over all media.
    #[must_use]
    pub fn track_count(&self) -> usize {
        self.search_track_count
            .map(|n| n as usize)
            .filter(|&n| n > 0)
            .unwrap_or_else(|| self.media.iter().map(Medium::len).sum())
    }

    /// The release date, falling back to the earliest release event's.
    #[must_use]
    pub fn date_text(&self) -> Option<&str> {
        non_empty(self.date.as_deref()).or_else(|| {
            self.release_events
                .iter()
                .find_map(|e| non_empty(e.date.as_deref()))
        })
    }

    /// Release countries: every release event's ISO code, else the
    /// release's `country`.
    #[must_use]
    pub fn countries(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for code in self
            .release_events
            .iter()
            .filter_map(|e| e.area.as_ref())
            .flat_map(|a| a.iso_3166_1_codes.iter())
        {
            if !code.is_empty() && !out.contains(code) {
                out.push(code.clone());
            }
        }
        if out.is_empty()
            && let Some(c) = non_empty(self.country.as_deref())
        {
            out.push(c.to_owned());
        }
        out
    }
}

/// `Some(s)` only for a non-blank string.
pub(super) fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

#[derive(Debug, Deserialize)]
pub(super) struct SearchResponse {
    #[serde(default, deserialize_with = "null_default")]
    pub releases: Vec<Release>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ErrorResponse {
    pub error: Option<String>,
}
