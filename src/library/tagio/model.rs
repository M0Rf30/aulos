// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Format-independent tag model shared by the tag reader, every format
//! writer, the MusicBrainz/AcoustID lookup and the tag editor UI.
//!
//! Field names are MusicBrainz Picard's internal tag names (lowercase), so a
//! release looked up on MusicBrainz maps 1:1 onto the same keys Picard's tag
//! mapping table documents for every container
//! (<https://picard-docs.musicbrainz.org/en/appendices/tag_mapping.html>).
//! Writers translate these keys to the native frame/atom/comment names;
//! keys they don't know are written as free-form fields (ID3 `TXXX:<key>`,
//! MP4 `----:com.apple.iTunes:<key>`, Vorbis/APE `<KEY>`), and the reader
//! maps such free-form fields back to the lowercase key.

use std::collections::BTreeMap;

/// Canonical field keys (Picard internal names).
pub mod keys {
    pub const TITLE: &str = "title";
    pub const ARTIST: &str = "artist";
    /// Multi-valued list of the individual track artists.
    pub const ARTISTS: &str = "artists";
    pub const ALBUM: &str = "album";
    pub const ALBUMARTIST: &str = "albumartist";
    pub const DATE: &str = "date";
    pub const ORIGINALDATE: &str = "originaldate";
    pub const ORIGINALYEAR: &str = "originalyear";
    pub const GENRE: &str = "genre";
    pub const TRACKNUMBER: &str = "tracknumber";
    pub const TOTALTRACKS: &str = "totaltracks";
    pub const DISCNUMBER: &str = "discnumber";
    pub const TOTALDISCS: &str = "totaldiscs";
    pub const DISCSUBTITLE: &str = "discsubtitle";
    pub const COMPOSER: &str = "composer";
    pub const LYRICIST: &str = "lyricist";
    pub const CONDUCTOR: &str = "conductor";
    pub const ARRANGER: &str = "arranger";
    pub const PRODUCER: &str = "producer";
    pub const ENGINEER: &str = "engineer";
    pub const MIXER: &str = "mixer";
    pub const REMIXER: &str = "remixer";
    pub const LABEL: &str = "label";
    pub const CATALOGNUMBER: &str = "catalognumber";
    pub const BARCODE: &str = "barcode";
    pub const ISRC: &str = "isrc";
    pub const ASIN: &str = "asin";
    pub const MEDIA: &str = "media";
    pub const RELEASECOUNTRY: &str = "releasecountry";
    pub const RELEASESTATUS: &str = "releasestatus";
    pub const RELEASETYPE: &str = "releasetype";
    pub const SCRIPT: &str = "script";
    pub const LANGUAGE: &str = "language";
    pub const BPM: &str = "bpm";
    pub const KEY: &str = "key";
    pub const MOOD: &str = "mood";
    pub const GROUPING: &str = "grouping";
    /// `"1"` when set; absent otherwise.
    pub const COMPILATION: &str = "compilation";
    pub const COMMENT: &str = "comment";
    pub const LYRICS: &str = "lyrics";
    pub const COPYRIGHT: &str = "copyright";
    pub const LICENSE: &str = "license";
    pub const ENCODEDBY: &str = "encodedby";
    pub const ENCODERSETTINGS: &str = "encodersettings";
    pub const WEBSITE: &str = "website";
    pub const TITLESORT: &str = "titlesort";
    pub const ARTISTSORT: &str = "artistsort";
    pub const ALBUMSORT: &str = "albumsort";
    pub const ALBUMARTISTSORT: &str = "albumartistsort";
    pub const COMPOSERSORT: &str = "composersort";
    /// MusicBrainz recording MBID (Vorbis `MUSICBRAINZ_TRACKID`, ID3 `UFID:http://musicbrainz.org`).
    pub const MUSICBRAINZ_RECORDINGID: &str = "musicbrainz_recordingid";
    /// MusicBrainz release-track MBID (Vorbis `MUSICBRAINZ_RELEASETRACKID`).
    pub const MUSICBRAINZ_TRACKID: &str = "musicbrainz_trackid";
    pub const MUSICBRAINZ_ALBUMID: &str = "musicbrainz_albumid";
    pub const MUSICBRAINZ_ARTISTID: &str = "musicbrainz_artistid";
    pub const MUSICBRAINZ_ALBUMARTISTID: &str = "musicbrainz_albumartistid";
    pub const MUSICBRAINZ_RELEASEGROUPID: &str = "musicbrainz_releasegroupid";
    pub const MUSICBRAINZ_WORKID: &str = "musicbrainz_workid";
    pub const MUSICBRAINZ_DISCID: &str = "musicbrainz_discid";
    pub const MUSICBRAINZ_LABELID: &str = "musicbrainz_labelid";
    pub const ACOUSTID_ID: &str = "acoustid_id";
    pub const ACOUSTID_FINGERPRINT: &str = "acoustid_fingerprint";
    pub const REPLAYGAIN_TRACK_GAIN: &str = "replaygain_track_gain";
    pub const REPLAYGAIN_TRACK_PEAK: &str = "replaygain_track_peak";
    pub const REPLAYGAIN_ALBUM_GAIN: &str = "replaygain_album_gain";
    pub const REPLAYGAIN_ALBUM_PEAK: &str = "replaygain_album_peak";
}

/// Display order of the well-known fields in the editor: the common block
/// first, then credits, release details, sort names and identifiers.
/// Fields not listed here are shown after these, alphabetically.
pub const KNOWN_KEYS: &[&str] = &[
    keys::TITLE,
    keys::ARTIST,
    keys::ALBUM,
    keys::ALBUMARTIST,
    keys::DATE,
    keys::GENRE,
    keys::TRACKNUMBER,
    keys::TOTALTRACKS,
    keys::DISCNUMBER,
    keys::TOTALDISCS,
    keys::DISCSUBTITLE,
    keys::COMPILATION,
    keys::COMMENT,
    keys::LYRICS,
    keys::ARTISTS,
    keys::COMPOSER,
    keys::LYRICIST,
    keys::CONDUCTOR,
    keys::ARRANGER,
    keys::PRODUCER,
    keys::ENGINEER,
    keys::MIXER,
    keys::REMIXER,
    keys::ORIGINALDATE,
    keys::ORIGINALYEAR,
    keys::LABEL,
    keys::CATALOGNUMBER,
    keys::BARCODE,
    keys::ISRC,
    keys::ASIN,
    keys::MEDIA,
    keys::RELEASECOUNTRY,
    keys::RELEASESTATUS,
    keys::RELEASETYPE,
    keys::SCRIPT,
    keys::LANGUAGE,
    keys::BPM,
    keys::KEY,
    keys::MOOD,
    keys::GROUPING,
    keys::COPYRIGHT,
    keys::LICENSE,
    keys::ENCODEDBY,
    keys::ENCODERSETTINGS,
    keys::WEBSITE,
    keys::TITLESORT,
    keys::ARTISTSORT,
    keys::ALBUMSORT,
    keys::ALBUMARTISTSORT,
    keys::COMPOSERSORT,
    keys::MUSICBRAINZ_RECORDINGID,
    keys::MUSICBRAINZ_TRACKID,
    keys::MUSICBRAINZ_ALBUMID,
    keys::MUSICBRAINZ_ARTISTID,
    keys::MUSICBRAINZ_ALBUMARTISTID,
    keys::MUSICBRAINZ_RELEASEGROUPID,
    keys::MUSICBRAINZ_WORKID,
    keys::MUSICBRAINZ_DISCID,
    keys::MUSICBRAINZ_LABELID,
    keys::ACOUSTID_ID,
    keys::ACOUSTID_FINGERPRINT,
    keys::REPLAYGAIN_TRACK_GAIN,
    keys::REPLAYGAIN_TRACK_PEAK,
    keys::REPLAYGAIN_ALBUM_GAIN,
    keys::REPLAYGAIN_ALBUM_PEAK,
];

/// ID3v2 `APIC` / FLAC `PICTURE` picture type (the same 0..=20 table in
/// both specs).
pub mod picture_type {
    pub const OTHER: u8 = 0;
    pub const FILE_ICON: u8 = 1;
    pub const FRONT_COVER: u8 = 3;
    pub const BACK_COVER: u8 = 4;
    pub const LEAFLET: u8 = 5;
    pub const MEDIA: u8 = 6;
    pub const ARTIST: u8 = 8;
}

/// One embedded picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditPicture {
    /// [`picture_type`] value. Containers without a type (MP4 `covr`) read
    /// as [`picture_type::FRONT_COVER`].
    pub kind: u8,
    /// MIME type (`image/jpeg`, `image/png`, ...). Sniffed with
    /// [`crate::library::tags::infer_mime`] when the container didn't store one.
    pub mime: String,
    pub description: String,
    pub data: Vec<u8>,
}

/// A file's tags: an ordered multi-map of canonical keys to values, plus
/// pictures. Keys are always lowercase; a key with no values is never
/// stored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagSet {
    fields: BTreeMap<String, Vec<String>>,
    pub pictures: Vec<EditPicture>,
}

impl TagSet {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// First value of `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .get(&key.to_ascii_lowercase())
            .and_then(|v| v.first())
            .map(String::as_str)
    }

    /// Every value of `key` (empty when absent).
    #[must_use]
    pub fn get_all(&self, key: &str) -> &[String] {
        self.fields
            .get(&key.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }

    /// Replaces `key`'s values. Empty strings are dropped; no remaining
    /// value removes the key.
    pub fn set(&mut self, key: &str, values: impl IntoIterator<Item = String>) {
        let values: Vec<String> = values.into_iter().filter(|v| !v.is_empty()).collect();
        let key = key.to_ascii_lowercase();
        if values.is_empty() {
            self.fields.remove(&key);
        } else {
            self.fields.insert(key, values);
        }
    }

    /// Replaces `key` with a single value (empty removes it).
    pub fn set_one(&mut self, key: &str, value: impl Into<String>) {
        self.set(key, [value.into()]);
    }

    /// Appends one value to `key`.
    pub fn push(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if value.is_empty() {
            return;
        }
        self.fields
            .entry(key.to_ascii_lowercase())
            .or_default()
            .push(value);
    }

    pub fn remove(&mut self, key: &str) {
        self.fields.remove(&key.to_ascii_lowercase());
    }

    /// `(key, values)` in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.fields.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.pictures.is_empty()
    }

    /// `(key, values)` in editor display order: [`KNOWN_KEYS`] first, then
    /// the remaining keys alphabetically.
    #[must_use]
    pub fn display_order(&self) -> Vec<(&str, &[String])> {
        let mut out: Vec<(&str, &[String])> = KNOWN_KEYS
            .iter()
            .filter_map(|k| self.fields.get_key_value(*k))
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect();
        out.extend(
            self.fields
                .iter()
                .filter(|(k, _)| !KNOWN_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.as_str(), v.as_slice())),
        );
        out
    }
}

/// Splits a `"3/12"` style position into `(number, total)`. Either half may
/// be missing; non-numeric text yields `None` for that half.
#[must_use]
pub fn split_position(s: &str) -> (Option<u32>, Option<u32>) {
    let mut parts = s.splitn(2, '/');
    let n = parts.next().and_then(|p| p.trim().parse().ok());
    let t = parts.next().and_then(|p| p.trim().parse().ok());
    (n, t)
}

/// Joins `(number, total)` into `"3/12"`, `"3"` or `""`.
#[must_use]
pub fn join_position(number: Option<&str>, total: Option<&str>) -> String {
    match (
        number.filter(|s| !s.is_empty()),
        total.filter(|s| !s.is_empty()),
    ) {
        (Some(n), Some(t)) => format!("{n}/{t}"),
        (Some(n), None) => n.to_string(),
        (None, Some(t)) => format!("/{t}"),
        (None, None) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_drops_empty_values_and_removes_empty_keys() {
        let mut t = TagSet::new();
        t.set("Title", ["".into(), "A".into()]);
        assert_eq!(t.get_all("title"), ["A".to_string()]);
        t.set("title", [String::new()]);
        assert_eq!(t.get("title"), None);
        assert!(t.is_empty());
    }

    #[test]
    fn display_order_puts_known_keys_first_then_alphabetical() {
        let mut t = TagSet::new();
        t.set_one("zzz", "1");
        t.set_one(keys::ALBUM, "B");
        t.set_one("aaa", "2");
        t.set_one(keys::TITLE, "A");
        let order: Vec<&str> = t.display_order().into_iter().map(|(k, _)| k).collect();
        assert_eq!(order, ["title", "album", "aaa", "zzz"]);
    }

    #[test]
    fn positions_round_trip() {
        assert_eq!(split_position("3/12"), (Some(3), Some(12)));
        assert_eq!(split_position(" 7 "), (Some(7), None));
        assert_eq!(split_position("/9"), (None, Some(9)));
        assert_eq!(join_position(Some("3"), Some("12")), "3/12");
        assert_eq!(join_position(Some("3"), Some("")), "3");
        assert_eq!(join_position(None, None), "");
    }
}
