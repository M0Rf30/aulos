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
//! (`tagcore::matching::assign_files`).

use super::file::{AcoustIdHit, FileId, TagFile, TrackAssignment};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tagcore::metadata::split_position;
use tagcore::{Metadata, Release, ReleaseQuery, keys};

/// Files below this similarity to every free track stay unmatched.
const MATCH_THRESHOLD: f32 = 0.35;

/// Whether `key` is a hidden variable (`~length`, `~multiartist`, …): never
/// shown, diffed, merged into a file's tags or written.
fn is_hidden(key: &str) -> bool {
    key.starts_with('~')
}

/// `existing` with every visible field of `release_tags` replacing the same
/// field; everything `release_tags` doesn't mention (and all pictures) is kept.
#[must_use]
pub fn overlay(existing: &Metadata, release_tags: &Metadata) -> Metadata {
    let mut merged = existing.clone();
    for (key, values) in release_tags.iter().filter(|(k, _)| !is_hidden(k)) {
        merged.set_all(key, values.iter().cloned());
    }
    merged
}

/// The keys whose values differ between `before` and `after`, in display
/// order. Hidden keys are skipped.
#[must_use]
pub fn diff_keys(before: &Metadata, after: &Metadata) -> Vec<String> {
    let mut changed: Vec<String> = before
        .visible_keys()
        .chain(after.visible_keys())
        .filter(|k| before.get_all(k) != after.get_all(k))
        .map(str::to_owned)
        .collect();
    changed.sort_by_key(|k| {
        (
            keys::KNOWN_KEYS
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
    baseline: &Metadata,
    release: &Release,
    assignment: &TrackAssignment,
    acoustid: Option<&AcoustIdHit>,
) -> Metadata {
    let mut tags = match release.tracks.get(assignment.track) {
        Some(track) => overlay(baseline, &track.metadata),
        None => baseline.clone(),
    };
    if let Some(hit) = acoustid {
        tags.set(keys::ACOUSTID_ID, hit.acoustid.as_str());
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
        self.release.tracks.len().saturating_sub(self.matched())
    }

    #[must_use]
    pub fn assignment_of(&self, file: FileId) -> Option<&TrackAssignment> {
        self.entries
            .iter()
            .find(|e| e.file == file)
            .and_then(|e| e.assignment.as_ref())
    }
}

fn assignment_for(release: &Release, track: usize, score: f32) -> Option<TrackAssignment> {
    let md = &release.tracks.get(track)?.metadata;
    let position = md.get(keys::TRACKNUMBER).unwrap_or_default().trim();
    let position = match position.parse::<u32>() {
        Ok(n) => format!("{n:02}"),
        Err(_) => position.to_owned(),
    };
    let discs = md
        .get(keys::TOTALDISCS)
        .and_then(|v| split_position(v).0)
        .unwrap_or(1);
    let number = match md.get(keys::DISCNUMBER).and_then(|v| split_position(v).0) {
        Some(disc) if discs > 1 => format!("{disc}-{position}"),
        _ => position,
    };
    Some(TrackAssignment {
        track,
        score,
        number,
        title: md.get(keys::TITLE).unwrap_or_default().to_owned(),
    })
}

/// What the file says about itself for matching: its on-disk tags, its
/// length, and its file name as the title when it has none.
fn match_metadata(file: &TagFile) -> Metadata {
    let mut md = file.original.clone();
    if let Some(secs) = file.duration_secs.filter(|s| s.is_finite() && *s > 0.0) {
        md.set_length(Duration::from_secs_f64(secs));
    }
    if md.get(keys::TITLE).is_none()
        && let Some(stem) = file.path.file_stem()
    {
        md.set(keys::TITLE, stem.to_string_lossy().into_owned());
    }
    md
}

/// Matches `files` to the tracks of `release`.
///
/// `hints` maps a file to recording MBIDs it is known to be (from AcoustID).
/// A file whose recording is on the release takes that track (the first one
/// not yet taken, when a recording appears twice); the others are matched
/// by `tagcore::matching::assign_files` (title, number, length, …) and lose
/// a track a recording-matched file already holds. Each track goes to at
/// most one file.
#[must_use]
pub fn plan_release(
    files: &[&TagFile],
    release: &Arc<Release>,
    hints: &HashMap<FileId, Vec<String>>,
) -> ReleasePlan {
    let mut matches: Vec<Option<(usize, f32)>> = vec![None; files.len()];
    let mut claimed: HashSet<usize> = HashSet::new();

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
        let found = release
            .tracks
            .iter()
            .enumerate()
            .find_map(|(index, track)| {
                (!claimed.contains(&index)
                    && track
                        .metadata
                        .get_all(keys::MUSICBRAINZ_RECORDINGID)
                        .iter()
                        .any(|id| known.contains(&id.as_str())))
                .then_some(index)
            });
        if let Some(index) = found {
            claimed.insert(index);
            *slot = Some((index, 1.0));
        }
    }

    // 2. Everything else by what the files say about themselves.
    let rest: Vec<usize> = (0..files.len()).filter(|&i| matches[i].is_none()).collect();
    let free: Vec<usize> = (0..release.tracks.len())
        .filter(|t| !claimed.contains(t))
        .collect();
    if !rest.is_empty() && !free.is_empty() {
        let file_md: Vec<Metadata> = rest.iter().map(|&i| match_metadata(files[i])).collect();
        let file_refs: Vec<&Metadata> = file_md.iter().collect();
        let track_refs: Vec<&Metadata> =
            free.iter().map(|&t| &release.tracks[t].metadata).collect();
        let assigned = tagcore::matching::assign_files(&file_refs, &track_refs, MATCH_THRESHOLD);
        for ((&i, md), found) in rest.iter().zip(&file_md).zip(assigned) {
            if let Some(slot) = found {
                let track = free[slot];
                let score =
                    tagcore::matching::track_similarity(md, &release.tracks[track].metadata);
                matches[i] = Some((track, score));
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
                assignment: m.and_then(|(track, score)| assignment_for(release, track, score)),
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
    let tracks = if multi_disc {
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
        tracks,
    }
}

#[cfg(test)]
mod tests {
    use super::super::file::testing::*;
    use super::*;
    use tagcore::settings::MetadataSettings;
    use tagcore::{CoverImage, ImageType};

    fn moderat_ii() -> Arc<Release> {
        let json: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/musicbrainz/release_lookup.json"
        ))
        .unwrap();
        Arc::new(tagcore::mb::map_release(&json, &MetadataSettings::default()).unwrap())
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

    fn title_of(release: &Release, assignment: &TrackAssignment) -> String {
        release.tracks[assignment.track]
            .metadata
            .get(keys::TITLE)
            .unwrap()
            .to_owned()
    }

    #[test]
    fn overlay_replaces_provided_fields_and_keeps_the_rest() {
        let mut existing = Metadata::new();
        existing.set(keys::TITLE, "bad kingdom");
        existing.set(keys::COMMENT, "ripped by me");
        existing.set(keys::LYRICS, "la la");
        existing.set(keys::REPLAYGAIN_TRACK_GAIN, "-6.5 dB");
        existing.set(keys::ENCODEDBY, "LAME");
        existing.set("x-custom", "mine");
        existing.set_all(keys::GENRE, ["Rock".to_owned(), "Pop".to_owned()]);
        let mut release = Metadata::new();
        release.set(keys::TITLE, "Bad Kingdom");
        release.set_all(keys::GENRE, ["Electronic".to_owned()]);
        release.set(keys::ALBUM, "II");

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
    fn overlay_keeps_pictures_and_skips_hidden_keys() {
        let mut existing = Metadata::new();
        existing.images.push(CoverImage::new(
            vec![1u8],
            "image/png",
            ImageType::Front,
            "",
        ));
        let mut release = Metadata::new();
        release.set("~length", "1000");
        release.set("~multiartist", "1");
        let merged = overlay(&existing, &release);
        assert_eq!(merged.images, existing.images);
        assert!(merged.get("~length").is_none());
        assert!(merged.get("~multiartist").is_none());
    }

    #[test]
    fn diff_lists_the_changed_keys_in_display_order() {
        let mut a = Metadata::new();
        a.set(keys::TITLE, "A");
        a.set(keys::GENRE, "Rock");
        a.set("x-custom", "1");
        let mut b = a.clone();
        b.set(keys::TITLE, "B");
        b.remove(keys::GENRE);
        b.set(keys::ALBUM, "X");
        b.set("x-other", "2");
        b.set("~length", "5");
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
        assert_eq!(a.number, "1-02");
        assert_eq!(a.title, "Bad Kingdom");
        assert_eq!(plan.unused_tracks(), release.tracks.len() - 3);
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
        f.original.set(
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
        assert_eq!(title_of(&release, assignment), "Bad Kingdom");
        let mut baseline = files[1].edited.clone();
        baseline.set(keys::COMMENT, "keep me");
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
        assert!(tags.get("~length").is_none());
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
        assert_eq!(q.tracks, None);
    }

    #[test]
    fn the_query_takes_the_track_count_from_single_disc_tags_only() {
        let mut a = track(1, "A", "B", 1, "t");
        a.original.set(keys::TOTALTRACKS, "12");
        let q = lookup_query(&[&a]);
        assert_eq!(q.tracks, Some(12));
        a.original.set(keys::TOTALDISCS, "2");
        assert_eq!(lookup_query(&[&a]).tracks, None);
    }

    #[test]
    fn the_query_falls_back_to_the_track_artist() {
        let f = file(1, &[(keys::ARTIST, "Solo"), (keys::ALBUM, "Alone")]);
        let q = lookup_query(&[&f]);
        assert_eq!((q.artist.as_str(), q.album.as_str()), ("Solo", "Alone"));
    }
}
