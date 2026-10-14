// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The words and numbers the tag editor shows: field names, picture types,
//! statuses and the small formatting rules behind them.

use super::{AddFieldError, SubmissionState};
use crate::fl;
use crate::library::tagio::{TagSet, keys, model::split_position, picture_type};
use crate::online::musicbrainz::ReleaseCandidate;

/// Semantic colour of a chip, banner or icon; the UI maps it to its theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

/// The readable name of a tag field; custom fields show their raw key.
/// `fl!` needs literal ids, hence the explicit table (one arm per entry of
/// `KNOWN_KEYS`, checked by a test).
pub fn field_label(key: &str) -> String {
    match key {
        keys::TITLE => fl!("tagger-field-title"),
        keys::ARTIST => fl!("tagger-field-artist"),
        keys::ALBUM => fl!("tagger-field-album"),
        keys::ALBUMARTIST => fl!("tagger-field-albumartist"),
        keys::DATE => fl!("tagger-field-date"),
        keys::GENRE => fl!("tagger-field-genre"),
        keys::TRACKNUMBER => fl!("tagger-field-tracknumber"),
        keys::TOTALTRACKS => fl!("tagger-field-totaltracks"),
        keys::DISCNUMBER => fl!("tagger-field-discnumber"),
        keys::TOTALDISCS => fl!("tagger-field-totaldiscs"),
        keys::DISCSUBTITLE => fl!("tagger-field-discsubtitle"),
        keys::COMPILATION => fl!("tagger-field-compilation"),
        keys::COMMENT => fl!("tagger-field-comment"),
        keys::LYRICS => fl!("tagger-field-lyrics"),
        keys::ARTISTS => fl!("tagger-field-artists"),
        keys::COMPOSER => fl!("tagger-field-composer"),
        keys::LYRICIST => fl!("tagger-field-lyricist"),
        keys::CONDUCTOR => fl!("tagger-field-conductor"),
        keys::ARRANGER => fl!("tagger-field-arranger"),
        keys::PRODUCER => fl!("tagger-field-producer"),
        keys::ENGINEER => fl!("tagger-field-engineer"),
        keys::MIXER => fl!("tagger-field-mixer"),
        keys::REMIXER => fl!("tagger-field-remixer"),
        keys::ORIGINALDATE => fl!("tagger-field-originaldate"),
        keys::ORIGINALYEAR => fl!("tagger-field-originalyear"),
        keys::LABEL => fl!("tagger-field-label"),
        keys::CATALOGNUMBER => fl!("tagger-field-catalognumber"),
        keys::BARCODE => fl!("tagger-field-barcode"),
        keys::ISRC => fl!("tagger-field-isrc"),
        keys::ASIN => fl!("tagger-field-asin"),
        keys::MEDIA => fl!("tagger-field-media"),
        keys::RELEASECOUNTRY => fl!("tagger-field-releasecountry"),
        keys::RELEASESTATUS => fl!("tagger-field-releasestatus"),
        keys::RELEASETYPE => fl!("tagger-field-releasetype"),
        keys::SCRIPT => fl!("tagger-field-script"),
        keys::LANGUAGE => fl!("tagger-field-language"),
        keys::BPM => fl!("tagger-field-bpm"),
        keys::KEY => fl!("tagger-field-key"),
        keys::MOOD => fl!("tagger-field-mood"),
        keys::GROUPING => fl!("tagger-field-grouping"),
        keys::COPYRIGHT => fl!("tagger-field-copyright"),
        keys::LICENSE => fl!("tagger-field-license"),
        keys::ENCODEDBY => fl!("tagger-field-encodedby"),
        keys::ENCODERSETTINGS => fl!("tagger-field-encodersettings"),
        keys::WEBSITE => fl!("tagger-field-website"),
        keys::TITLESORT => fl!("tagger-field-titlesort"),
        keys::ARTISTSORT => fl!("tagger-field-artistsort"),
        keys::ALBUMSORT => fl!("tagger-field-albumsort"),
        keys::ALBUMARTISTSORT => fl!("tagger-field-albumartistsort"),
        keys::COMPOSERSORT => fl!("tagger-field-composersort"),
        keys::MUSICBRAINZ_RECORDINGID => fl!("tagger-field-musicbrainz-recordingid"),
        keys::MUSICBRAINZ_TRACKID => fl!("tagger-field-musicbrainz-trackid"),
        keys::MUSICBRAINZ_ALBUMID => fl!("tagger-field-musicbrainz-albumid"),
        keys::MUSICBRAINZ_ARTISTID => fl!("tagger-field-musicbrainz-artistid"),
        keys::MUSICBRAINZ_ALBUMARTISTID => fl!("tagger-field-musicbrainz-albumartistid"),
        keys::MUSICBRAINZ_RELEASEGROUPID => fl!("tagger-field-musicbrainz-releasegroupid"),
        keys::MUSICBRAINZ_WORKID => fl!("tagger-field-musicbrainz-workid"),
        keys::MUSICBRAINZ_DISCID => fl!("tagger-field-musicbrainz-discid"),
        keys::MUSICBRAINZ_LABELID => fl!("tagger-field-musicbrainz-labelid"),
        keys::ACOUSTID_ID => fl!("tagger-field-acoustid-id"),
        keys::ACOUSTID_FINGERPRINT => fl!("tagger-field-acoustid-fingerprint"),
        keys::REPLAYGAIN_TRACK_GAIN => fl!("tagger-field-replaygain-track-gain"),
        keys::REPLAYGAIN_TRACK_PEAK => fl!("tagger-field-replaygain-track-peak"),
        keys::REPLAYGAIN_ALBUM_GAIN => fl!("tagger-field-replaygain-album-gain"),
        keys::REPLAYGAIN_ALBUM_PEAK => fl!("tagger-field-replaygain-album-peak"),
        other => other.to_owned(),
    }
}

/// The name of a picture type (`type N` for the ones the editor doesn't
/// offer).
pub fn picture_kind_label(kind: u8) -> String {
    match kind {
        picture_type::FRONT_COVER => fl!("tagger-picture-front"),
        picture_type::BACK_COVER => fl!("tagger-picture-back"),
        picture_type::LEAFLET => fl!("tagger-picture-leaflet"),
        picture_type::MEDIA => fl!("tagger-picture-media"),
        picture_type::ARTIST => fl!("tagger-picture-artist"),
        picture_type::OTHER => fl!("tagger-picture-other"),
        other => fl!("tagger-picture-type", kind = other),
    }
}

/// The type choices for a picture of `kind`: the six the editor offers,
/// plus the picture's own when it is none of them. Returns the labels and
/// the index of `kind` among them.
#[must_use]
pub fn picture_kind_choices(kind: u8) -> (Vec<String>, Option<usize>) {
    let mut names: Vec<String> = super::edit::PICTURE_KINDS
        .iter()
        .map(|k| picture_kind_label(*k))
        .collect();
    let mut selected = super::edit::PICTURE_KINDS.iter().position(|k| *k == kind);
    if selected.is_none() {
        names.push(picture_kind_label(kind));
        selected = Some(names.len() - 1);
    }
    (names, selected)
}

/// Why the "add field" form refused, in words.
pub fn add_field_error_text(error: AddFieldError) -> String {
    match error {
        AddFieldError::NoSelection => fl!("tagger-add-field-error-no-selection"),
        AddFieldError::NoKey => fl!("tagger-add-field-error-no-key"),
        AddFieldError::InvalidKey => fl!("tagger-add-field-error-invalid-key"),
        AddFieldError::NoValue => fl!("tagger-add-field-error-no-value"),
    }
}

/// A match similarity (`0.0..=1.0`) as a whole percentage.
#[must_use]
pub fn percent(score: f32) -> u32 {
    (score.clamp(0.0, 1.0) * 100.0).round() as u32
}

/// Unit and number text of a picture size: whole kilobytes (at least 1)
/// below a megabyte, megabytes with one decimal above.
#[derive(Debug, PartialEq, Eq)]
pub enum SizeText {
    Kilobytes(String),
    Megabytes(String),
}

pub fn size_text(bytes: usize) -> SizeText {
    const MIB: usize = 1024 * 1024;
    if bytes < MIB {
        SizeText::Kilobytes(bytes.div_ceil(1024).max(1).to_string())
    } else {
        SizeText::Megabytes(format!("{:.1}", bytes as f64 / MIB as f64))
    }
}

/// A picture size for display.
pub fn format_size(bytes: usize) -> String {
    match size_text(bytes) {
        SizeText::Kilobytes(value) => fl!("tagger-size-kb", value = value),
        SizeText::Megabytes(value) => fl!("tagger-size-mb", value = value),
    }
}

/// The track number of a file's tags (the part before `/`), empty when
/// there is none.
pub fn track_label(tags: &TagSet) -> String {
    tags.get(keys::TRACKNUMBER)
        .and_then(|v| split_position(v).0)
        .map(|n| n.to_string())
        .unwrap_or_default()
}

/// The words and tone of a file's submission state; `None` when it was
/// never submitted.
pub fn submission_text(state: &SubmissionState) -> Option<(String, Tone)> {
    match state {
        SubmissionState::NotSubmitted => None,
        SubmissionState::Pending => Some((fl!("tagger-submission-pending"), Tone::Accent)),
        SubmissionState::Submitted { status } => Some((
            match status.as_str() {
                "pending" => fl!("tagger-submission-pending-status"),
                "imported" => fl!("tagger-submission-imported-status"),
                other => fl!("tagger-submission-submitted", status = other.to_owned()),
            },
            Tone::Success,
        )),
        SubmissionState::Failed(error) => Some((
            fl!("tagger-submission-failed", error = error.clone()),
            Tone::Danger,
        )),
    }
}

/// The non-empty `parts` joined as one `a · b · c` line.
pub fn dotted(parts: impl IntoIterator<Item = String>) -> String {
    parts
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The details line of a release candidate: everything but title and score.
pub fn candidate_details(candidate: &ReleaseCandidate) -> String {
    dotted([
        candidate.artist.clone(),
        candidate.date.clone(),
        candidate.country.clone(),
        candidate.label.clone(),
        candidate.formats.clone(),
        if candidate.track_count > 0 {
            fl!("tagger-lookup-tracks", count = candidate.track_count)
        } else {
            String::new()
        },
    ])
}

/// The first `limit` of `names` joined with commas, and how many are left
/// out.
pub fn name_list(names: &[String], limit: usize) -> (String, usize) {
    let shown = names
        .iter()
        .take(limit)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    (shown, names.len().saturating_sub(limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tagio::model::KNOWN_KEYS;

    #[test]
    fn picture_choices_select_an_offered_kind_in_place() {
        let (names, selected) = picture_kind_choices(picture_type::FRONT_COVER);
        assert_eq!(names.len(), super::super::edit::PICTURE_KINDS.len());
        assert_eq!(selected, Some(0));
    }

    #[test]
    fn picture_choices_append_a_kind_the_editor_does_not_offer() {
        let (names, selected) = picture_kind_choices(200);
        assert_eq!(names.len(), super::super::edit::PICTURE_KINDS.len() + 1);
        assert_eq!(selected, Some(names.len() - 1));
    }

    #[test]
    fn every_known_field_has_a_label_of_its_own() {
        for key in KNOWN_KEYS {
            assert_ne!(field_label(key), *key, "no label for {key}");
        }
    }

    #[test]
    fn custom_fields_keep_their_raw_key() {
        assert_eq!(field_label("my_custom_field"), "my_custom_field");
    }

    #[test]
    fn scores_round_to_whole_percentages_in_range() {
        assert_eq!(percent(0.924), 92);
        assert_eq!(percent(0.5), 50);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(1.7), 100);
        assert_eq!(percent(-0.2), 0);
    }

    #[test]
    fn sizes_round_up_to_kilobytes_then_switch_to_megabytes() {
        assert_eq!(size_text(1), SizeText::Kilobytes("1".into()));
        assert_eq!(size_text(1024), SizeText::Kilobytes("1".into()));
        assert_eq!(size_text(1025), SizeText::Kilobytes("2".into()));
        assert_eq!(size_text(300 * 1024), SizeText::Kilobytes("300".into()));
        assert_eq!(size_text(1024 * 1024), SizeText::Megabytes("1.0".into()));
        assert_eq!(
            size_text(5 * 1024 * 1024 / 2),
            SizeText::Megabytes("2.5".into())
        );
    }

    #[test]
    fn track_label_takes_the_number_before_the_slash() {
        let mut tags = TagSet::new();
        assert_eq!(track_label(&tags), "");
        tags.set_one(keys::TRACKNUMBER, "3/12");
        assert_eq!(track_label(&tags), "3");
        tags.set_one(keys::TRACKNUMBER, "A1");
        assert_eq!(track_label(&tags), "");
    }

    #[test]
    fn dotted_skips_empty_parts() {
        let line = dotted(["A".into(), String::new(), " ".into(), "B".into()]);
        assert_eq!(line, "A · B");
    }

    #[test]
    fn name_lists_report_what_they_cut() {
        let names: Vec<String> = ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(name_list(&names, 2), ("a, b".to_owned(), 1));
        assert_eq!(name_list(&names, 5), ("a, b, c".to_owned(), 0));
    }
}
