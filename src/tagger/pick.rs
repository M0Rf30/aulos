// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Choosing one release for a set of files from their AcoustID hits.
//!
//! A fingerprint resolves to recordings, and a recording appears on many
//! releases (the album, singles, compilations, regional editions…). For a
//! cluster the right release is the one most of its files appear on; ties
//! go to the release whose size fits the cluster (the same number of tracks
//! as files, or a medium of that size), then to the one AcoustID is surest
//! about, then to the earliest release.

use super::file::FileId;
use crate::online::acoustid::{AcoustIdRelease, AcoustIdResult};
use std::cmp::Ordering;
use std::collections::HashSet;

/// Results below this score are guesses and ignored.
pub const MIN_SCORE: f64 = 0.8;

/// The result to use for a fingerprint: the highest-scoring one that is
/// good enough and maps to at least one recording.
#[must_use]
pub fn best_result(results: &[AcoustIdResult]) -> Option<&AcoustIdResult> {
    results
        .iter()
        .filter(|r| r.score >= MIN_SCORE && !r.recordings.is_empty())
        .max_by(|a, b| a.score.total_cmp(&b.score))
}

/// The release chosen for a cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseChoice {
    /// MusicBrainz release MBID.
    pub release_id: String,
    pub title: String,
    pub date: Option<String>,
    /// Tracks on the release, as AcoustID reports them (0 when unknown).
    pub track_count: u32,
    /// The files whose fingerprint appears on the release.
    pub covered: Vec<FileId>,
}

struct Candidate {
    release: AcoustIdRelease,
    covered: Vec<FileId>,
    score_sum: f64,
}

impl Candidate {
    /// The release has as many tracks as there are files, or a medium that
    /// does.
    fn fits(&self, file_count: usize) -> bool {
        let count = file_count as u32;
        self.release.track_count == count
            || self.release.mediums.iter().any(|m| m.track_count == count)
    }
}

/// Earlier dates first; unknown dates last. `"2013"` < `"2013-08"` < `"2014"`.
fn date_order(a: Option<&str>, b: Option<&str>) -> Ordering {
    fn known(d: Option<&str>) -> Option<&str> {
        d.filter(|d| !d.is_empty())
    }
    match (known(a), known(b)) {
        (Some(a), Some(b)) => a.cmp(b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The release that covers most of the files whose best hit is in `hits`;
/// `file_count` is the number of files that were scanned (hit or not).
/// `None` when no file has a hit with a release.
#[must_use]
pub fn choose_release(
    hits: &[(FileId, AcoustIdResult)],
    file_count: usize,
) -> Option<ReleaseChoice> {
    let mut candidates: Vec<Candidate> = Vec::new();
    for (file, result) in hits {
        let mut seen: HashSet<&str> = HashSet::new();
        for recording in &result.recordings {
            for (_, release) in recording.all_releases() {
                if release.id.is_empty() || !seen.insert(release.id.as_str()) {
                    continue;
                }
                match candidates.iter_mut().find(|c| c.release.id == release.id) {
                    Some(c) => {
                        c.covered.push(*file);
                        c.score_sum += result.score;
                        // Releases nested under a release group tend to carry
                        // the medium details; keep whichever has them.
                        if c.release.mediums.is_empty() && !release.mediums.is_empty() {
                            c.release = release.clone();
                        }
                    }
                    None => candidates.push(Candidate {
                        release: release.clone(),
                        covered: vec![*file],
                        score_sum: result.score,
                    }),
                }
            }
        }
    }

    candidates
        .into_iter()
        .max_by(|a, b| {
            a.covered
                .len()
                .cmp(&b.covered.len())
                .then(a.fits(file_count).cmp(&b.fits(file_count)))
                .then(a.score_sum.total_cmp(&b.score_sum))
                // `max_by`: the *better* candidate must compare greater, so
                // an earlier date / smaller id compares greater.
                .then_with(|| date_order(b.release.date.as_deref(), a.release.date.as_deref()))
                .then_with(|| b.release.id.cmp(&a.release.id))
        })
        .map(|c| ReleaseChoice {
            release_id: c.release.id,
            title: c.release.title,
            date: c.release.date,
            track_count: c.release.track_count,
            covered: c.covered,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::online::acoustid::{AcoustIdMedium, AcoustIdRecording, AcoustIdReleaseGroup};

    fn release(id: &str, tracks: u32, date: Option<&str>) -> AcoustIdRelease {
        AcoustIdRelease {
            id: id.into(),
            title: format!("Release {id}"),
            date: date.map(Into::into),
            track_count: tracks,
            medium_count: 1,
            mediums: vec![AcoustIdMedium {
                position: 1,
                track_count: tracks,
                ..AcoustIdMedium::default()
            }],
            ..AcoustIdRelease::default()
        }
    }

    fn result(score: f64, releases: Vec<AcoustIdRelease>) -> AcoustIdResult {
        AcoustIdResult {
            id: "acoustid".into(),
            score,
            recordings: vec![AcoustIdRecording {
                id: "rec".into(),
                release_groups: vec![AcoustIdReleaseGroup {
                    releases,
                    ..AcoustIdReleaseGroup::default()
                }],
                ..AcoustIdRecording::default()
            }],
        }
    }

    #[test]
    fn the_release_covering_most_files_wins() {
        let album = release("album", 12, Some("2013"));
        let single = release("single", 2, Some("2012"));
        let hits = vec![
            (1, result(0.97, vec![album.clone(), single.clone()])),
            (2, result(0.95, vec![album.clone()])),
            (3, result(0.99, vec![album.clone(), single])),
        ];
        let choice = choose_release(&hits, 12).unwrap();
        assert_eq!(choice.release_id, "album");
        assert_eq!(choice.covered, vec![1, 2, 3]);
    }

    #[test]
    fn a_tie_goes_to_the_release_that_fits_the_file_count() {
        // Both releases cover both files; the deluxe edition has 20 tracks,
        // the standard one 10, and 10 files were scanned.
        let standard = release("standard", 10, Some("2015"));
        let deluxe = release("deluxe", 20, Some("2014"));
        let hits = vec![
            (1, result(0.9, vec![deluxe.clone(), standard.clone()])),
            (2, result(0.9, vec![standard, deluxe])),
        ];
        assert_eq!(choose_release(&hits, 10).unwrap().release_id, "standard");
        assert_eq!(
            choose_release(&hits, 20).unwrap().release_id,
            "deluxe",
            "fit is relative to the number of files"
        );
    }

    #[test]
    fn a_medium_of_the_right_size_counts_as_a_fit() {
        let mut two_discs = release("box", 24, None);
        two_discs.medium_count = 2;
        two_discs.mediums = vec![
            AcoustIdMedium {
                position: 1,
                track_count: 12,
                ..AcoustIdMedium::default()
            },
            AcoustIdMedium {
                position: 2,
                track_count: 12,
                ..AcoustIdMedium::default()
            },
        ];
        let other = release("other", 30, None);
        let hits = vec![(1, result(0.9, vec![other, two_discs]))];
        assert_eq!(choose_release(&hits, 12).unwrap().release_id, "box");
    }

    #[test]
    fn then_the_surer_score_then_the_earlier_release() {
        let early = release("early", 10, Some("2001"));
        let late = release("late", 10, Some("2005"));
        let hits = vec![(1, result(0.9, vec![late.clone(), early.clone()]))];
        assert_eq!(choose_release(&hits, 10).unwrap().release_id, "early");
        let undated = release("undated", 10, None);
        let hits = vec![(1, result(0.9, vec![undated, late]))];
        assert_eq!(choose_release(&hits, 10).unwrap().release_id, "late");
    }

    #[test]
    fn a_release_listed_twice_for_one_file_counts_once() {
        let album = release("album", 10, None);
        let mut r = result(0.9, vec![album.clone()]);
        r.recordings[0].releases = vec![album];
        let choice = choose_release(&[(1, r)], 10).unwrap();
        assert_eq!(choice.covered, vec![1]);
    }

    #[test]
    fn no_hits_no_release() {
        assert!(choose_release(&[], 5).is_none());
        let no_releases = AcoustIdResult {
            id: "x".into(),
            score: 0.99,
            recordings: vec![AcoustIdRecording::default()],
        };
        assert!(choose_release(&[(1, no_releases)], 5).is_none());
    }

    #[test]
    fn best_result_ignores_weak_matches_and_empty_ones() {
        let strong = result(0.93, vec![release("a", 1, None)]);
        let weak = result(0.4, vec![release("b", 1, None)]);
        let empty = AcoustIdResult {
            id: "e".into(),
            score: 0.99,
            recordings: vec![],
        };
        let results = vec![weak.clone(), empty, strong.clone()];
        assert_eq!(best_result(&results), Some(&strong));
        assert_eq!(best_result(&[weak]), None);
        assert_eq!(best_result(&[]), None);
    }
}
