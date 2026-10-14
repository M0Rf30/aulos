// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! MusicBrainz release data to tags, following MusicBrainz Picard's
//! standard mapping
//! (<https://picard-docs.musicbrainz.org/en/appendices/tag_mapping.html>).

use super::model::{
    Recording, Release, credit_artist_ids, credit_artist_names, credit_sort_text, credit_text,
    non_empty,
};
use crate::library::tagio::{TagSet, keys};

/// Appends each value not yet present in `into`.
fn push_unique<'a>(into: &mut Vec<String>, values: impl IntoIterator<Item = &'a str>) {
    for v in values {
        if !v.is_empty() && !into.iter().any(|e| e == v) {
            into.push(v.to_owned());
        }
    }
}

impl Release {
    /// The tags Picard would write for track `track_idx` of medium
    /// `medium_idx` (both 0-based indices into [`Release::media`] and
    /// [`Medium::tracks`](super::Medium)); empty when either is out of
    /// range.
    ///
    /// `tracknumber` is the 1-based position on the medium; a printed
    /// number that isn't numeric (`A1` on vinyl) is not written because
    /// the library and every player parse the field as a number.
    #[must_use]
    pub fn track_tags(&self, medium_idx: usize, track_idx: usize) -> TagSet {
        let mut tags = TagSet::new();
        let Some(medium) = self.media.get(medium_idx) else {
            return tags;
        };
        let Some(track) = medium.tracks.get(track_idx) else {
            return tags;
        };

        // Track.
        tags.set_one(keys::TITLE, track.display_title());
        let credit = track.credit();
        tags.set_one(keys::ARTIST, credit_text(credit));
        tags.set(keys::ARTISTS, credit_artist_names(credit));
        tags.set_one(keys::ARTISTSORT, credit_sort_text(credit));
        tags.set(keys::MUSICBRAINZ_ARTISTID, credit_artist_ids(credit));
        tags.set_one(keys::MUSICBRAINZ_TRACKID, track.id.as_str());
        tags.set_one(keys::MUSICBRAINZ_RECORDINGID, track.recording.id.as_str());
        tags.set(keys::ISRC, track.recording.isrcs.iter().cloned());

        // Position.
        let number = track.position.max(1).to_string();
        tags.set_one(keys::TRACKNUMBER, number);
        tags.set_one(keys::TOTALTRACKS, medium.len().to_string());
        tags.set_one(keys::DISCNUMBER, medium.position.max(1).to_string());
        tags.set_one(keys::TOTALDISCS, self.media.len().to_string());
        tags.set_one(keys::DISCSUBTITLE, medium.title.trim());
        tags.set_one(
            keys::MEDIA,
            non_empty(medium.format.as_deref()).unwrap_or_default(),
        );

        // Release.
        tags.set_one(keys::ALBUM, self.title.as_str());
        tags.set_one(keys::ALBUMARTIST, credit_text(&self.artist_credit));
        tags.set_one(keys::ALBUMARTISTSORT, credit_sort_text(&self.artist_credit));
        tags.set(
            keys::MUSICBRAINZ_ALBUMARTISTID,
            credit_artist_ids(&self.artist_credit),
        );
        tags.set_one(keys::MUSICBRAINZ_ALBUMID, self.id.as_str());
        tags.set_one(keys::DATE, self.date_text().unwrap_or_default());
        tags.set_one(
            keys::BARCODE,
            non_empty(self.barcode.as_deref()).unwrap_or_default(),
        );
        tags.set_one(
            keys::ASIN,
            non_empty(self.asin.as_deref()).unwrap_or_default(),
        );
        tags.set(keys::RELEASECOUNTRY, self.countries());
        tags.set_one(
            keys::RELEASESTATUS,
            non_empty(self.status.as_deref())
                .map(str::to_lowercase)
                .unwrap_or_default(),
        );
        tags.set_one(
            keys::SCRIPT,
            non_empty(self.text_representation.script.as_deref()).unwrap_or_default(),
        );
        tags.set_one(
            keys::LANGUAGE,
            non_empty(self.text_representation.language.as_deref()).unwrap_or_default(),
        );

        // Labels and catalog numbers: unique, in release order.
        let mut labels = Vec::new();
        let mut label_ids = Vec::new();
        let mut catnos = Vec::new();
        for info in &self.label_info {
            if let Some(label) = &info.label {
                push_unique(&mut labels, [label.name.as_str()]);
                push_unique(&mut label_ids, [label.id.as_str()]);
            }
            push_unique(&mut catnos, non_empty(info.catalog_number.as_deref()));
        }
        tags.set(keys::LABEL, labels);
        tags.set(keys::MUSICBRAINZ_LABELID, label_ids);
        tags.set(keys::CATALOGNUMBER, catnos);

        // Release group.
        if let Some(group) = &self.release_group {
            tags.set_one(keys::MUSICBRAINZ_RELEASEGROUPID, group.id.as_str());
            let original = non_empty(group.first_release_date.as_deref()).unwrap_or_default();
            tags.set_one(keys::ORIGINALDATE, original);
            let year = original
                .get(..4)
                .filter(|y| y.bytes().all(|b| b.is_ascii_digit()));
            tags.set_one(keys::ORIGINALYEAR, year.unwrap_or_default());
            let types: Vec<String> = group
                .primary_type
                .iter()
                .chain(&group.secondary_types)
                .map(|t| t.to_lowercase())
                .collect();
            if types.iter().any(|t| t == "compilation") {
                tags.set_one(keys::COMPILATION, "1");
            }
            tags.set(keys::RELEASETYPE, types);
        }
        tags
    }
}

impl Recording {
    /// The recording-level tags (what an AcoustID hit identifies without
    /// choosing a release): title, artist credit, ISRCs and MBIDs.
    #[must_use]
    pub fn tags(&self) -> TagSet {
        let mut tags = TagSet::new();
        tags.set_one(keys::TITLE, self.title.as_str());
        tags.set_one(keys::ARTIST, credit_text(&self.artist_credit));
        tags.set(keys::ARTISTS, credit_artist_names(&self.artist_credit));
        tags.set_one(keys::ARTISTSORT, credit_sort_text(&self.artist_credit));
        tags.set(
            keys::MUSICBRAINZ_ARTISTID,
            credit_artist_ids(&self.artist_credit),
        );
        tags.set_one(keys::MUSICBRAINZ_RECORDINGID, self.id.as_str());
        tags.set(keys::ISRC, self.isrcs.iter().cloned());
        tags
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::fixture_release;
    use super::*;

    #[test]
    fn maps_a_vinyl_track_like_picard() {
        let r = fixture_release();
        // Second medium, first track (C1).
        let t = r.track_tags(1, 0);
        let m = &r.media[1].tracks[0];
        assert_eq!(t.get(keys::TITLE), Some(m.title.as_str()));
        assert_eq!(t.get(keys::ARTIST), Some("Moderat"));
        assert_eq!(t.get(keys::ARTISTS), Some("Moderat"));
        assert_eq!(t.get(keys::ARTISTSORT), Some("Moderat"));
        assert_eq!(t.get(keys::ALBUM), Some("II"));
        assert_eq!(t.get(keys::ALBUMARTIST), Some("Moderat"));
        assert_eq!(t.get(keys::ALBUMARTISTSORT), Some("Moderat"));
        assert_eq!(t.get(keys::DATE), Some("2013-08-02"));
        assert_eq!(t.get(keys::ORIGINALDATE), Some("2013-07"));
        assert_eq!(t.get(keys::ORIGINALYEAR), Some("2013"));
        assert_eq!(t.get(keys::TRACKNUMBER), Some("1"));
        assert_eq!(t.get(keys::TOTALTRACKS), Some("6"));
        assert_eq!(t.get(keys::DISCNUMBER), Some("2"));
        assert_eq!(t.get(keys::TOTALDISCS), Some("2"));
        assert_eq!(t.get(keys::DISCSUBTITLE), None);
        assert_eq!(t.get(keys::MEDIA), Some("12\" Vinyl"));
        // The two catalog numbers of the same label collapse to one label.
        assert_eq!(t.get_all(keys::LABEL), ["Monkeytown Records"]);
        assert_eq!(
            t.get_all(keys::MUSICBRAINZ_LABELID),
            ["dd5e9af0-a9bb-4e11-9c57-5921c8838945"]
        );
        assert_eq!(
            t.get_all(keys::CATALOGNUMBER),
            ["MONKEYTOWN036LP", "MTR036LP"]
        );
        assert_eq!(t.get(keys::BARCODE), Some("817231011237"));
        assert_eq!(t.get(keys::ASIN), Some("B00BFJHEL8"));
        assert_eq!(t.get(keys::RELEASECOUNTRY), Some("DE"));
        assert_eq!(t.get(keys::RELEASESTATUS), Some("official"));
        assert_eq!(t.get(keys::RELEASETYPE), Some("album"));
        assert_eq!(t.get(keys::SCRIPT), Some("Latn"));
        assert_eq!(t.get(keys::LANGUAGE), Some("eng"));
        assert_eq!(t.get(keys::COMPILATION), None);
        assert_eq!(
            t.get(keys::MUSICBRAINZ_ALBUMID),
            Some("00398749-1816-4c98-8a39-377627740365")
        );
        assert_eq!(
            t.get(keys::MUSICBRAINZ_RELEASEGROUPID),
            Some("885181a4-9133-485f-bfce-fddc1168a3c1")
        );
        assert_eq!(
            t.get(keys::MUSICBRAINZ_ALBUMARTISTID),
            Some("7754905b-8bf7-48e2-935a-03d566fec464")
        );
        assert_eq!(
            t.get(keys::MUSICBRAINZ_ARTISTID),
            Some("7754905b-8bf7-48e2-935a-03d566fec464")
        );
        assert_eq!(t.get(keys::MUSICBRAINZ_TRACKID), Some(m.id.as_str()));
        assert_eq!(
            t.get(keys::MUSICBRAINZ_RECORDINGID),
            Some(m.recording.id.as_str())
        );
    }

    #[test]
    fn maps_isrcs_and_numbers_for_the_first_track() {
        let t = fixture_release().track_tags(0, 0);
        assert_eq!(t.get(keys::TITLE), Some("The Mark (interlude)"));
        assert_eq!(t.get(keys::ISRC), Some("DEOE81310017"));
        assert_eq!(t.get(keys::TRACKNUMBER), Some("1"));
        assert_eq!(t.get(keys::TOTALTRACKS), Some("5"));
        assert_eq!(t.get(keys::DISCNUMBER), Some("1"));
    }

    #[test]
    fn out_of_range_indices_yield_no_tags() {
        let r = fixture_release();
        assert!(r.track_tags(5, 0).is_empty());
        assert!(r.track_tags(0, 99).is_empty());
    }

    #[test]
    fn multi_artist_credit_compilation_release() {
        let json = r#"{
            "id":"11111111-1111-1111-1111-111111111111","title":"Mixed","status":"Promotion",
            "date":"","release-events":[{"date":"2001-05","area":{"name":"Europe","iso-3166-1-codes":["XE"]}}],
            "artist-credit":[{"name":"Various Artists","joinphrase":"","artist":{"id":"89ad4ac3-39f7-470e-963a-56509c546377","name":"Various Artists","sort-name":"Various Artists"}}],
            "release-group":{"id":"22222222-2222-2222-2222-222222222222","title":"Mixed","primary-type":"Album","secondary-types":["Compilation","Live"],"first-release-date":"2001"},
            "media":[{"position":1,"title":"Side one","format":null,"track-count":1,"tracks":[{
                "id":"33333333-3333-3333-3333-333333333333","number":"1","position":1,"title":"Duet","length":null,
                "artist-credit":[
                  {"name":"Alice","joinphrase":" feat. ","artist":{"id":"a0000000-0000-0000-0000-000000000001","name":"Alice Smith","sort-name":"Smith, Alice"}},
                  {"name":"Bob","joinphrase":"","artist":{"id":"b0000000-0000-0000-0000-000000000002","name":"Bob Jones","sort-name":"Jones, Bob"}}],
                "recording":{"id":"44444444-4444-4444-4444-444444444444","title":"Duet (recording)","length":181000,"isrcs":["AA1","BB2"]}}]}]
        }"#;
        let r: Release = serde_json::from_str(json).unwrap();
        let t = r.track_tags(0, 0);
        assert_eq!(t.get(keys::TITLE), Some("Duet"));
        assert_eq!(t.get(keys::ARTIST), Some("Alice feat. Bob"));
        assert_eq!(t.get_all(keys::ARTISTS), ["Alice Smith", "Bob Jones"]);
        assert_eq!(
            t.get(keys::ARTISTSORT),
            Some("Smith, Alice feat. Jones, Bob")
        );
        assert_eq!(
            t.get_all(keys::MUSICBRAINZ_ARTISTID),
            [
                "a0000000-0000-0000-0000-000000000001",
                "b0000000-0000-0000-0000-000000000002"
            ]
        );
        assert_eq!(t.get_all(keys::ISRC), ["AA1", "BB2"]);
        assert_eq!(
            t.get(keys::DATE),
            Some("2001-05"),
            "falls back to the first release event"
        );
        assert_eq!(t.get(keys::RELEASECOUNTRY), Some("XE"));
        assert_eq!(t.get(keys::RELEASESTATUS), Some("promotion"));
        assert_eq!(
            t.get_all(keys::RELEASETYPE),
            ["album", "compilation", "live"]
        );
        assert_eq!(t.get(keys::COMPILATION), Some("1"));
        assert_eq!(t.get(keys::ORIGINALDATE), Some("2001"));
        assert_eq!(t.get(keys::ORIGINALYEAR), Some("2001"));
        assert_eq!(t.get(keys::DISCSUBTITLE), Some("Side one"));
        assert_eq!(t.get(keys::MEDIA), None, "null format writes nothing");
        assert_eq!(t.get(keys::BARCODE), None);
    }
}
