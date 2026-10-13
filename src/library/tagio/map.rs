// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The single table relating Picard's tag names ([`super::keys`]) to
//! Symphonia's [`StandardTag`]s. Reading uses it from `StandardTag` to key,
//! writing from key to `StandardTag`; the Symphonia fork's own readers and
//! writers then translate the `StandardTag` to and from each container's
//! native field, so nothing here is format specific.
//!
//! Some keys have extra read-only rows (`canonical: false`): standard tags
//! that different containers produce for the same Picard field (an MP4
//! `©day` reads as `ReleaseDate`, an ID3v1 year as `RecordingYear`, a Vorbis
//! `MUSICBRAINZ_TRACKID` as `MusicBrainzTrackId`, which Picard calls the
//! recording id). Writing always uses the canonical row.

use super::keys;
use std::sync::Arc;
use symphonia::core::meta::{StandardTag, StandardVisualKey};

pub(super) struct Field {
    pub key: &'static str,
    /// Written by [`std_for`]; non-canonical rows only feed [`key_for`].
    pub canonical: bool,
    /// The field's value when `tag` is this row's standard tag.
    pub get: fn(&StandardTag) -> Option<String>,
    /// The row's standard tag for `value`; `None` when `value` doesn't fit
    /// the tag's type (a non-numeric track number, ...).
    pub make: fn(&str) -> Option<StandardTag>,
}

macro_rules! text {
    ($key:expr, $variant:ident) => {
        text!($key, $variant, true)
    };
    ($key:expr, $variant:ident, $canonical:expr) => {
        Field {
            key: $key,
            canonical: $canonical,
            get: |t| match t {
                StandardTag::$variant(v) => Some(v.to_string()),
                _ => None,
            },
            make: |s| Some(StandardTag::$variant(Arc::new(s.to_owned()))),
        }
    };
}

macro_rules! number {
    ($key:expr, $variant:ident, $ty:ty) => {
        number!($key, $variant, $ty, true)
    };
    ($key:expr, $variant:ident, $ty:ty, $canonical:expr) => {
        Field {
            key: $key,
            canonical: $canonical,
            get: |t| match t {
                StandardTag::$variant(v) => Some(v.to_string()),
                _ => None,
            },
            make: |s| s.trim().parse::<$ty>().ok().map(StandardTag::$variant),
        }
    };
}

pub(super) const FIELDS: &[Field] = &[
    text!(keys::TITLE, TrackTitle),
    text!(keys::ARTIST, Artist),
    text!(keys::ALBUM, Album),
    text!(keys::ALBUMARTIST, AlbumArtist),
    text!(keys::DATE, RecordingDate),
    text!(keys::DATE, ReleaseDate, false),
    number!(keys::DATE, RecordingYear, u16, false),
    number!(keys::DATE, ReleaseYear, u16, false),
    text!(keys::ORIGINALDATE, OriginalReleaseDate),
    number!(keys::ORIGINALYEAR, OriginalReleaseYear, u16),
    text!(keys::GENRE, Genre),
    number!(keys::TRACKNUMBER, TrackNumber, u64),
    number!(keys::TOTALTRACKS, TrackTotal, u64),
    number!(keys::DISCNUMBER, DiscNumber, u64),
    number!(keys::TOTALDISCS, DiscTotal, u64),
    text!(keys::DISCSUBTITLE, DiscSubtitle),
    text!(keys::COMPOSER, Composer),
    text!(keys::LYRICIST, Lyricist),
    text!(keys::CONDUCTOR, Conductor),
    text!(keys::ARRANGER, Arranger),
    text!(keys::PRODUCER, Producer),
    text!(keys::ENGINEER, Engineer),
    text!(keys::MIXER, MixEngineer),
    text!(keys::REMIXER, Remixer),
    text!(keys::LABEL, Label),
    text!(keys::CATALOGNUMBER, IdentCatalogNumber),
    text!(keys::BARCODE, IdentBarcode),
    text!(keys::ISRC, IdentIsrc),
    text!(keys::ASIN, IdentAsin),
    text!(keys::MEDIA, MediaFormat),
    text!(keys::RELEASECOUNTRY, ReleaseCountry),
    text!(keys::RELEASESTATUS, MusicBrainzReleaseStatus),
    text!(keys::RELEASETYPE, MusicBrainzReleaseType),
    text!(keys::SCRIPT, Script),
    text!(keys::LANGUAGE, Language),
    number!(keys::BPM, Bpm, u64),
    text!(keys::KEY, InitialKey),
    text!(keys::MOOD, Mood),
    text!(keys::GROUPING, Grouping),
    Field {
        key: keys::COMPILATION,
        canonical: true,
        get: |t| match t {
            StandardTag::CompilationFlag(true) => Some("1".to_owned()),
            _ => None,
        },
        make: |s| match s.trim() {
            "1" => Some(StandardTag::CompilationFlag(true)),
            _ => None,
        },
    },
    text!(keys::COMMENT, Comment),
    text!(keys::LYRICS, Lyrics),
    text!(keys::COPYRIGHT, Copyright),
    text!(keys::LICENSE, License),
    text!(keys::ENCODEDBY, EncodedBy),
    text!(keys::ENCODERSETTINGS, EncoderSettings),
    text!(keys::WEBSITE, Url),
    text!(keys::WEBSITE, UrlOfficial, false),
    text!(keys::WEBSITE, UrlArtist, false),
    text!(keys::TITLESORT, SortTrackTitle),
    text!(keys::ARTISTSORT, SortArtist),
    text!(keys::ALBUMSORT, SortAlbum),
    text!(keys::ALBUMARTISTSORT, SortAlbumArtist),
    text!(keys::COMPOSERSORT, SortComposer),
    // Picard's recording id is ID3 `UFID:http://musicbrainz.org` and Vorbis/APE/MP4
    // `MUSICBRAINZ_TRACKID`, which the fork's readers produce as
    // `MusicBrainzRecordingId` and `MusicBrainzTrackId` respectively.
    text!(keys::MUSICBRAINZ_RECORDINGID, MusicBrainzRecordingId),
    text!(keys::MUSICBRAINZ_RECORDINGID, MusicBrainzTrackId, false),
    // Picard's track id is the MusicBrainz *release track* id.
    text!(keys::MUSICBRAINZ_TRACKID, MusicBrainzReleaseTrackId),
    text!(keys::MUSICBRAINZ_ALBUMID, MusicBrainzAlbumId),
    text!(keys::MUSICBRAINZ_ARTISTID, MusicBrainzArtistId),
    text!(keys::MUSICBRAINZ_ALBUMARTISTID, MusicBrainzAlbumArtistId),
    text!(keys::MUSICBRAINZ_RELEASEGROUPID, MusicBrainzReleaseGroupId),
    text!(keys::MUSICBRAINZ_WORKID, MusicBrainzWorkId),
    text!(keys::MUSICBRAINZ_DISCID, MusicBrainzDiscId),
    text!(keys::MUSICBRAINZ_LABELID, MusicBrainzLabelId),
    text!(keys::ACOUSTID_ID, AcoustIdId),
    text!(keys::ACOUSTID_FINGERPRINT, AcoustIdFingerprint),
    text!(keys::REPLAYGAIN_TRACK_GAIN, ReplayGainTrackGain),
    text!(keys::REPLAYGAIN_TRACK_PEAK, ReplayGainTrackPeak),
    text!(keys::REPLAYGAIN_ALBUM_GAIN, ReplayGainAlbumGain),
    text!(keys::REPLAYGAIN_ALBUM_PEAK, ReplayGainAlbumPeak),
];

/// ID3v2 `APIC` / FLAC `PICTURE` picture type (see [`super::picture_type`])
/// for a picture's usage. Types the model doesn't name read as `Other`.
pub(super) fn usage_for_type(kind: u8) -> StandardVisualKey {
    use StandardVisualKey as K;
    match kind {
        1 => K::FileIcon,
        2 => K::OtherIcon,
        3 => K::FrontCover,
        4 => K::BackCover,
        5 => K::Leaflet,
        6 => K::Media,
        7 => K::LeadArtistPerformerSoloist,
        8 => K::ArtistPerformer,
        9 => K::Conductor,
        10 => K::BandOrchestra,
        11 => K::Composer,
        12 => K::Lyricist,
        13 => K::RecordingLocation,
        14 => K::RecordingSession,
        15 => K::Performance,
        16 => K::ScreenCapture,
        18 => K::Illustration,
        19 => K::BandArtistLogo,
        20 => K::PublisherStudioLogo,
        _ => K::Other,
    }
}

/// Inverse of [`usage_for_type`]; a picture without a usage is `Other`.
pub(super) fn type_for_usage(usage: Option<StandardVisualKey>) -> u8 {
    use StandardVisualKey as K;
    match usage {
        Some(K::FileIcon) => 1,
        Some(K::OtherIcon) => 2,
        Some(K::FrontCover) => 3,
        Some(K::BackCover) => 4,
        Some(K::Leaflet) => 5,
        Some(K::Media) => 6,
        Some(K::LeadArtistPerformerSoloist) => 7,
        Some(K::ArtistPerformer) => 8,
        Some(K::Conductor) => 9,
        Some(K::BandOrchestra) => 10,
        Some(K::Composer) => 11,
        Some(K::Lyricist) => 12,
        Some(K::RecordingLocation) => 13,
        Some(K::RecordingSession) => 14,
        Some(K::Performance) => 15,
        Some(K::ScreenCapture) => 16,
        Some(K::Illustration) => 18,
        Some(K::BandArtistLogo) => 19,
        Some(K::PublisherStudioLogo) => 20,
        Some(K::Other) | None => 0,
        // `StandardVisualKey` is `#[non_exhaustive]`.
        Some(_) => 0,
    }
}

/// The Picard key and value `tag` stands for, if it has one.
pub(super) fn key_for(tag: &StandardTag) -> Option<(&'static str, String)> {
    FIELDS.iter().find_map(|f| (f.get)(tag).map(|v| (f.key, v)))
}

/// The canonical standard tag for `key`/`value`; `None` when the key has no
/// standard tag or the value doesn't fit its type (the caller then writes a
/// free-form field).
pub(super) fn std_for(key: &str, value: &str) -> Option<StandardTag> {
    FIELDS
        .iter()
        .find(|f| f.canonical && f.key == key)
        .and_then(|f| (f.make)(value))
}

/// Whether `key` has a canonical standard tag (used by the table's own tests).
#[cfg(test)]
pub(super) fn has_std(key: &str) -> bool {
    FIELDS.iter().any(|f| f.canonical && f.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tagio::model::KNOWN_KEYS;
    use std::collections::HashSet;

    /// Picard keys with no `StandardTag`: always free-form fields.
    const FREE_FORM: &[&str] = &[keys::ARTISTS];

    #[test]
    fn canonical_rows_round_trip() {
        let mut seen = HashSet::new();
        for f in FIELDS.iter().filter(|f| f.canonical) {
            assert!(seen.insert(f.key), "duplicate canonical row {}", f.key);
            let std = (f.make)("1").unwrap_or_else(|| panic!("{} rejects 7", f.key));
            let (key, value) = key_for(&std).unwrap_or_else(|| panic!("{} unmapped", f.key));
            assert_eq!((key, value.as_str()), (f.key, "1"));
            assert_eq!(std_for(f.key, "1"), Some(std));
        }
    }

    #[test]
    fn every_known_key_maps_or_is_free_form() {
        for k in KNOWN_KEYS {
            assert_eq!(
                has_std(k),
                !FREE_FORM.contains(k),
                "{k}: table and FREE_FORM disagree"
            );
        }
    }

    #[test]
    fn every_canonical_row_is_a_known_key() {
        for f in FIELDS.iter().filter(|f| f.canonical) {
            assert!(KNOWN_KEYS.contains(&f.key), "{} not in KNOWN_KEYS", f.key);
        }
    }

    #[test]
    fn std_tags_are_claimed_once() {
        // The same standard tag kind must never resolve to two keys.
        for (i, a) in FIELDS.iter().enumerate() {
            let probe = (a.make)("1").expect("row accepts 7");
            let owners: Vec<_> = FIELDS
                .iter()
                .filter(|b| (b.get)(&probe).is_some())
                .collect();
            assert_eq!(owners.len(), 1, "row {i} ({}) claimed twice", a.key);
        }
    }

    #[test]
    fn aliases_read_as_the_canonical_key() {
        let (k, v) = key_for(&StandardTag::MusicBrainzTrackId(Arc::new("x".into()))).unwrap();
        assert_eq!((k, v.as_str()), (keys::MUSICBRAINZ_RECORDINGID, "x"));
        let (k, v) = key_for(&StandardTag::ReleaseYear(1999)).unwrap();
        assert_eq!((k, v.as_str()), (keys::DATE, "1999"));
        let (k, _) = key_for(&StandardTag::MusicBrainzReleaseTrackId(Arc::new(
            "y".into(),
        )))
        .unwrap();
        assert_eq!(k, keys::MUSICBRAINZ_TRACKID);
    }

    #[test]
    fn picture_types_round_trip() {
        for kind in (0..=20u8).filter(|k| *k != 17) {
            assert_eq!(type_for_usage(Some(usage_for_type(kind))), kind);
        }
    }

    #[test]
    fn mismatched_values_have_no_std() {
        assert_eq!(std_for(keys::TRACKNUMBER, "A1"), None);
        assert_eq!(std_for(keys::COMPILATION, "0"), None);
        assert_eq!(std_for(keys::ARTISTS, "x"), None);
    }
}
