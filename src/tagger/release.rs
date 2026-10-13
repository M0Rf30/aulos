// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Applying a MusicBrainz release to a set of files: which file is which
//! track, and what each file's tags become.
//!
//! A file's new tags are its *current* tags overlaid with the tags the
//! release track maps to ([`overlay`]): every field MusicBrainz provides is
//! replaced, every other field — comment, lyrics, ReplayGain, encoder, custom
//! fields — is kept. Files are matched to tracks first by recording MBID
//! (a hit from an AcoustID scan, or the `musicbrainz_recordingid` the file
//! already carries), then by title, number and duration
//! ([`crate::online::musicbrainz::match_files`]).

use super::file::{AcoustIdHit, FileId, TagFile, TrackAssignment};
use crate::library::tagio::{TagSet, keys, model::split_position};
use crate::online::musicbrainz::{FileForMatch, Release, ReleaseQuery, TrackMatch, match_files};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// `existing` with every field of `release_tags` replacing the same field;
/// everything `release_tags` doesn't mention (and all pictures) is kept.
#[must_use]
pub fn overlay(existing: &TagSet, release_tags: &TagSet) -> TagSet {
    let mut merged = existing.clone();
    for (key, values) in release_tags.iter() {
        merged.set(key, values.iter().cloned());
    }
    merged
}

/// The keys whose values differ between `before` and `after`, in display
/// order.
#[must_use]
pub fn diff_keys(before: &TagSet, after: &TagSet) -> Vec<String> {
    let mut changed: Vec<String> = before
        .iter()
        .map(|(k, _)| k)
        .chain(after.iter().map(|(k, _)| k))
        .filter(|k| before.get_all(k) != after.get_all(k))
        .map(str::to_owned)
        .collect();
    changed.sort_by_key(|k| {
        (
            crate::library::tagio::model::KNOWN_KEYS
                .iter()
                .position(|known| known == k)
                .unwrap_or(usize::MAX),
            k.clone(),
        )
    });
    changed.dedup();
    changed
}

/// A file's tags after taking track `assignment` of `release`, starting
/// from `baseline`. An AcoustID hit adds its `acoustid_id`.
#[must_use]
pub fn tags_for(
    baseline: &TagSet,
    release: &Release,
    assignment: &TrackAssignment,
    acoustid: Option<&AcoustIdHit>,
) -> TagSet {
    let mut tags = overlay(
        baseline,
        &release.track_tags(assignment.medium, assignment.track),
    );
    if let Some(hit) = acoustid {
        tags.set_one(keys::ACOUSTID_ID, hit.acoustid.as_str());
    }
    tags
}

/// How the files of a lookup line up with a release.
#[derive(Debug, Clone)]
pub struct ReleasePlan {
    pub release: Arc<Release>,
    /// One entry per target file, in the order they were given.
    pub entries: Vec<PlanEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanEntry {
    pub file: FileId,
    /// `None` when no track matched well enough.
    pub assignment: Option<TrackAssignment>,
}

impl ReleasePlan {
    /// Files that got a track.
    #[must_use]
    pub fn matched(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.assignment.is_some())
            .count()
    }

    /// Files no track matched.
    #[must_use]
    pub fn unmatched(&self) -> Vec<FileId> {
        self.entries
            .iter()
            .filter(|e| e.assignment.is_none())
            .map(|e| e.file)
            .collect()
    }

    /// Tracks of the release no file was matched to.
    #[must_use]
    pub fn unused_tracks(&self) -> usize {
        self.release.track_count().saturating_sub(self.matched())
    }

    #[must_use]
    pub fn assignment_of(&self, file: FileId) -> Option<&TrackAssignment> {
        self.entries
            .iter()
            .find(|e| e.file == file)
            .and_then(|e| e.assignment.as_ref())
    }
}

fn assignment_for(release: &Release, m: TrackMatch) -> Option<TrackAssignment> {
    let medium = release.media.get(m.medium)?;
    let track = medium.tracks.get(m.track)?;
    let number = if release.media.len() > 1 {
        format!("{}-{:02}", medium.position.max(1), track.position)
    } else {
        format!("{:02}", track.position)
    };
    Some(TrackAssignment {
        medium: m.medium,
        track: m.track,
        score: m.score,
        number,
        title: track.display_title().to_owned(),
    })
}

/// Matches `files` to the tracks of `release`.
///
/// `hints` maps a file to recording MBIDs it is known to be (from AcoustID).
/// A file whose recording is on the release takes that track (the first one
/// not yet taken, when a recording appears twice); the others are matched
/// by title, number and duration, and lose a track a recording-matched file
/// already holds. Each track goes to at most one file.
#[must_use]
pub fn plan_release(
    files: &[&TagFile],
    release: &Arc<Release>,
    hints: &HashMap<FileId, Vec<String>>,
) -> ReleasePlan {
    let mut matches: Vec<Option<TrackMatch>> = vec![None; files.len()];
    let mut claimed: HashSet<(usize, usize)> = HashSet::new();

    // 1. Recordings we know.
    for (slot, file) in matches.iter_mut().zip(files) {
        let own = file.original.get_all(keys::MUSICBRAINZ_RECORDINGID);
        let known: Vec<&str> = hints
            .get(&file.id)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .chain(own)
            .map(String::as_str)
            .collect();
        if known.is_empty() {
            continue;
        }
        let found = release.media.iter().enumerate().find_map(|(m, medium)| {
            medium.tracks.iter().enumerate().find_map(|(t, track)| {
                (known.contains(&track.recording.id.as_str()) && !claimed.contains(&(m, t)))
                    .then_some((m, t))
            })
        });
        if let Some((medium, track)) = found {
            claimed.insert((medium, track));
            *slot = Some(TrackMatch {
                medium,
                track,
                score: 1.0,
            });
        }
    }

    // 2. Everything else by what the files say about themselves.
    let rest: Vec<usize> = (0..files.len()).filter(|&i| matches[i].is_none()).collect();
    if !rest.is_empty() {
        let stems: Vec<Option<String>> = rest
            .iter()
            .map(|&i| {
                files[i]
                    .path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .collect();
        let claims: Vec<FileForMatch<'_>> = rest
            .iter()
            .zip(&stems)
            .map(|(&i, stem)| FileForMatch {
                tags: &files[i].original,
                duration_secs: files[i].duration_secs,
                file_stem: stem.as_deref(),
            })
            .collect();
        for (&i, found) in rest.iter().zip(match_files(&claims, release)) {
            if let Some(m) = found
                && claimed.insert((m.medium, m.track))
            {
                matches[i] = Some(m);
            }
        }
    }

    ReleasePlan {
        release: Arc::clone(release),
        entries: files
            .iter()
            .zip(matches)
            .map(|(file, m)| PlanEntry {
                file: file.id,
                assignment: m.and_then(|m| assignment_for(release, m)),
            })
            .collect(),
    }
}

// --- The release search for a set of files ---------------------------------

/// The most common non-empty value, comparing case-insensitively; the first
/// spelling met wins.
fn most_common(values: impl Iterator<Item = String>) -> Option<String> {
    let mut counts: Vec<(String, String, usize)> = Vec::new();
    for value in values {
        let value = value.trim().to_owned();
        if value.is_empty() {
            continue;
        }
        let lower = value.to_lowercase();
        match counts.iter_mut().find(|(l, _, _)| *l == lower) {
            Some((_, _, n)) => *n += 1,
            None => counts.push((lower, value, 1)),
        }
    }
    // `max_by_key` keeps the last maximum; reverse so the first one wins.
    counts
        .into_iter()
        .rev()
        .max_by_key(|(_, _, n)| *n)
        .map(|(_, value, _)| value)
}

/// What to search MusicBrainz for to find the release of `files`: the most
/// common album artist (or artist) and album of their on-disk tags, and the
/// track count when they say how many tracks the release has (single-disc
/// only: `totaltracks` counts one disc).
#[must_use]
pub fn lookup_query(files: &[&TagFile]) -> ReleaseQuery {
    let artist = most_common(files.iter().filter_map(|f| {
        f.original
            .get(keys::ALBUMARTIST)
            .or_else(|| f.original.get(keys::ARTIST))
            .map(str::to_owned)
    }))
    .unwrap_or_default();
    let album = most_common(
        files
            .iter()
            .filter_map(|f| f.original.get(keys::ALBUM).map(str::to_owned)),
    )
    .unwrap_or_default();
    let multi_disc = files.iter().any(|f| {
        f.original
            .get(keys::TOTALDISCS)
            .and_then(|v| split_position(v).0)
            .is_some_and(|n| n > 1)
    });
    let track_count = if multi_disc {
        None
    } else {
        files
            .iter()
            .filter_map(|f| {
                f.original
                    .get(keys::TOTALTRACKS)
                    .and_then(|v| split_position(v).0)
            })
            .max()
            .filter(|&n| n > 0)
    };
    ReleaseQuery {
        artist,
        album,
        track_count,
        barcode: None,
        catalog_number: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::file::testing::*;
    use super::*;

    fn moderat_ii() -> Arc<Release> {
        Arc::new(
            serde_json::from_str(include_str!(
                "../../tests/fixtures/musicbrainz/release_lookup.json"
            ))
            .unwrap(),
        )
    }

    fn files_of_disc_one() -> Vec<TagFile> {
        [
            (1, "The Mark (interlude)", 96.0),
            (2, "Bad Kingdom", 262.0),
            (3, "Versions", 309.0),
        ]
        .iter()
        .map(|&(n, title, secs)| {
            let mut f = track(u64::from(n), "Moderat", "II", n, title);
            f.duration_secs = Some(secs);
            f
        })
        .collect()
    }

    #[test]
    fn overlay_replaces_provided_fields_and_keeps_the_rest() {
        let mut existing = TagSet::new();
        existing.set_one(keys::TITLE, "bad kingdom");
        existing.set_one(keys::COMMENT, "ripped by me");
        existing.set_one(keys::LYRICS, "la la");
        existing.set_one(keys::REPLAYGAIN_TRACK_GAIN, "-6.5 dB");
        existing.set_one(keys::ENCODEDBY, "LAME");
        existing.set_one("x-custom", "mine");
        existing.set(keys::GENRE, ["Rock".to_owned(), "Pop".to_owned()]);
        let mut release = TagSet::new();
        release.set_one(keys::TITLE, "Bad Kingdom");
        release.set(keys::GENRE, ["Electronic".to_owned()]);
        release.set_one(keys::ALBUM, "II");

        let merged = overlay(&existing, &release);
        assert_eq!(merged.get(keys::TITLE), Some("Bad Kingdom"));
        assert_eq!(merged.get(keys::ALBUM), Some("II"));
        assert_eq!(
            merged.get_all(keys::GENRE),
            ["Electronic"],
            "lists are replaced, not appended"
        );
        for kept in [
            keys::COMMENT,
            keys::LYRICS,
            keys::REPLAYGAIN_TRACK_GAIN,
            keys::ENCODEDBY,
            "x-custom",
        ] {
            assert_eq!(merged.get(kept), existing.get(kept), "{kept} must survive");
        }
    }

    #[test]
    fn overlay_keeps_pictures() {
        let mut existing = TagSet::new();
        existing.pictures.push(crate::library::tagio::EditPicture {
            kind: 3,
            mime: "image/png".into(),
            description: String::new(),
            data: vec![1],
        });
        let merged = overlay(&existing, &TagSet::new());
        assert_eq!(merged.pictures, existing.pictures);
    }

    #[test]
    fn diff_lists_the_changed_keys_in_display_order() {
        let mut a = TagSet::new();
        a.set_one(keys::TITLE, "A");
        a.set_one(keys::GENRE, "Rock");
        a.set_one("x-custom", "1");
        let mut b = a.clone();
        b.set_one(keys::TITLE, "B");
        b.remove(keys::GENRE);
        b.set_one(keys::ALBUM, "X");
        b.set_one("x-other", "2");
        assert_eq!(diff_keys(&a, &b), ["title", "album", "genre", "x-other"]);
        assert!(diff_keys(&a, &a).is_empty());
    }

    #[test]
    fn matches_files_by_title_and_number_and_numbers_multi_disc_positions() {
        let release = moderat_ii();
        let files = files_of_disc_one();
        let refs: Vec<&TagFile> = files.iter().collect();
        let plan = plan_release(&refs, &release, &HashMap::new());
        assert_eq!(plan.matched(), 3);
        let a = plan.assignment_of(2).unwrap();
        assert_eq!((a.medium, a.track), (0, 1));
        assert_eq!(a.number, "1-02");
        assert_eq!(a.title, "Bad Kingdom");
        assert_eq!(plan.unused_tracks(), 11 - 3);
        assert!(plan.unmatched().is_empty());
    }

    #[test]
    fn a_recording_hint_beats_wrong_tags() {
        let release = moderat_ii();
        // Tagged as track 1 / "Wrong" but AcoustID says it is "Gita".
        let wrong = track(1, "Moderat", "II", 1, "Wrong");
        let mut hints = HashMap::new();
        hints.insert(1, vec!["4a46bcc8-efb5-4995-9301-bc5ee92f3805".to_owned()]);
        let plan = plan_release(&[&wrong], &release, &hints);
        let a = plan.assignment_of(1).unwrap();
        assert_eq!(a.title, "Gita");
        assert_eq!(a.number, "2-02");
        assert!((a.score - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_files_own_recording_id_matches_it() {
        let release = moderat_ii();
        let mut f = file(1, &[(keys::TITLE, "???")]);
        f.original.set_one(
            keys::MUSICBRAINZ_RECORDINGID,
            "b0762d25-6a79-4801-8aec-bba8cae4f361",
        );
        let plan = plan_release(&[&f], &release, &HashMap::new());
        assert_eq!(plan.assignment_of(1).unwrap().title, "Milk");
    }

    #[test]
    fn a_track_goes_to_one_file_even_when_two_claim_it() {
        let release = moderat_ii();
        // Both files are identified as the same recording.
        let a = track(1, "Moderat", "II", 1, "x");
        let b = track(2, "Moderat", "II", 1, "y");
        let id = "ca5449c0-c875-4f96-8ed6-f7c8021133c4".to_owned();
        let hints = HashMap::from([(1, vec![id.clone()]), (2, vec![id])]);
        let plan = plan_release(&[&a, &b], &release, &hints);
        let taken: Vec<_> = plan
            .entries
            .iter()
            .filter_map(|e| e.assignment.as_ref())
            .filter(|a| a.title == "Therapy")
            .collect();
        assert_eq!(
            taken.len(),
            1,
            "the recording's single track is held by one file only"
        );
    }

    #[test]
    fn unmatchable_files_are_flagged() {
        let release = moderat_ii();
        let junk = file(
            9,
            &[
                (keys::TITLE, "Completely Unrelated Song"),
                (keys::TRACKNUMBER, "99"),
            ],
        );
        let plan = plan_release(&[&junk], &release, &HashMap::new());
        assert_eq!(plan.unmatched(), vec![9]);
    }

    #[test]
    fn tags_for_overlays_the_track_and_records_the_acoustid() {
        let release = moderat_ii();
        let files = files_of_disc_one();
        let refs: Vec<&TagFile> = files.iter().collect();
        let plan = plan_release(&refs, &release, &HashMap::new());
        let assignment = plan.assignment_of(2).unwrap();
        let mut baseline = files[1].edited.clone();
        baseline.set_one(keys::COMMENT, "keep me");
        let hit = AcoustIdHit {
            acoustid: "11111111-2222-3333-4444-555555555555".into(),
            recording_ids: vec![],
        };
        let tags = tags_for(&baseline, &release, assignment, Some(&hit));
        assert_eq!(tags.get(keys::TITLE), Some("Bad Kingdom"));
        assert_eq!(tags.get(keys::DISCNUMBER), Some("1"));
        assert_eq!(tags.get(keys::TOTALDISCS), Some("2"));
        assert_eq!(
            tags.get(keys::MUSICBRAINZ_ALBUMID),
            Some("00398749-1816-4c98-8a39-377627740365")
        );
        assert_eq!(tags.get(keys::COMMENT), Some("keep me"));
        assert_eq!(
            tags.get(keys::ACOUSTID_ID),
            Some("11111111-2222-3333-4444-555555555555")
        );
    }

    #[test]
    fn the_query_uses_the_most_common_artist_and_album() {
        let files = [
            track(1, "Moderat", "II", 1, "a"),
            track(2, "moderat", "II", 2, "b"),
            track(3, "Someone", "Other", 3, "c"),
        ];
        let refs: Vec<&TagFile> = files.iter().collect();
        let q = lookup_query(&refs);
        assert_eq!(q.artist, "Moderat");
        assert_eq!(q.album, "II");
        assert_eq!(q.track_count, None);
    }

    #[test]
    fn the_query_takes_the_track_count_from_single_disc_tags_only() {
        let mut a = track(1, "A", "B", 1, "t");
        a.original.set_one(keys::TOTALTRACKS, "12");
        let q = lookup_query(&[&a]);
        assert_eq!(q.track_count, Some(12));
        a.original.set_one(keys::TOTALDISCS, "2");
        assert_eq!(lookup_query(&[&a]).track_count, None);
    }

    #[test]
    fn the_query_falls_back_to_the_track_artist() {
        let f = file(1, &[(keys::ARTIST, "Solo"), (keys::ALBUM, "Alone")]);
        let q = lookup_query(&[&f]);
        assert_eq!((q.artist.as_str(), q.album.as_str()), ("Solo", "Alone"));
    }
}
