// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Assigns local files to the tracks of a MusicBrainz release.
//!
//! Every (file, track) pair is scored from whatever the file offers — disc
//! and track number, title, duration — and the files are then assigned to
//! tracks one-to-one so that the *total* score is maximal (Hungarian
//! algorithm), rather than greedily: two files that both resemble track 3
//! cannot both end up on it.

use super::model::Release;
use crate::library::tagio::{TagSet, keys, model::split_position};

/// Pairs scoring below this are reported as unmatched.
pub const MIN_MATCH_SCORE: f32 = 0.35;

const W_TITLE: f64 = 0.5;
const W_NUMBER: f64 = 0.3;
const W_DISC: f64 = 0.2;
const W_DURATION: f64 = 0.2;
/// Duration deltas up to this many seconds count as identical…
const DURATION_EXACT_SECS: f64 = 2.0;
/// …and ones past this as unrelated; linear in between.
const DURATION_FAR_SECS: f64 = 15.0;

/// What is known about one local file.
#[derive(Debug, Clone, Copy)]
pub struct FileForMatch<'a> {
    pub tags: &'a TagSet,
    pub duration_secs: Option<f64>,
    /// File name without extension. Used for the title and track number
    /// when the tags have none (`"03 - Some Song"`).
    pub file_stem: Option<&'a str>,
}

/// The release track a file was assigned to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackMatch {
    /// Index into [`Release::media`].
    pub medium: usize,
    /// Index into that medium's tracks.
    pub track: usize,
    /// Similarity in `0.0..=1.0`.
    pub score: f32,
}

/// Normalised title for comparison: lowercase letters and digits only,
/// words separated by single spaces.
#[must_use]
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// Removes `(...)` / `[...]` groups: "Song (Remastered 2011)" → "Song".
fn strip_brackets(s: &str) -> String {
    let mut depth = 0u32;
    s.chars()
        .filter(|&c| match c {
            '(' | '[' => {
                depth += 1;
                false
            }
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                false
            }
            _ => depth == 0,
        })
        .collect()
}

/// `1 - edit_distance / longer_length` over characters.
fn ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[b.len()] as f64 / longest as f64
}

/// Similarity of two titles in `0.0..=1.0`, ignoring case, punctuation and
/// bracketed qualifiers such as "(Remastered)" when that helps.
#[must_use]
pub fn title_similarity(a: &str, b: &str) -> f32 {
    let (na, nb) = (normalize(a), normalize(b));
    let mut best = ratio(&na, &nb);
    let (sa, sb) = (normalize(&strip_brackets(a)), normalize(&strip_brackets(b)));
    if !sa.is_empty() && !sb.is_empty() && (sa != na || sb != nb) {
        best = best.max(ratio(&sa, &sb));
    }
    best as f32
}

/// The title and (disc, track) number a file claims, from its tags or,
/// failing that, its file name.
struct FileClaim {
    title: Option<String>,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    duration_secs: Option<f64>,
}

fn claim(file: &FileForMatch<'_>) -> FileClaim {
    let tags = file.tags;
    let number = |key| {
        tags.get(key)
            .and_then(|v| split_position(v).0)
            .filter(|&n| n > 0)
    };
    let mut title = tags
        .get(keys::TITLE)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned);
    let mut track_number = number(keys::TRACKNUMBER);
    if let Some(stem) = file.file_stem {
        // "03 - Name", "03. Name", "03_Name": up to three leading digits
        // followed by a separator.
        const SEPARATORS: [char; 5] = [' ', '.', '-', '_', ')'];
        let digits = stem.chars().take_while(char::is_ascii_digit).count();
        let (num, after) = stem.split_at(digits);
        let has_number = (1..=3).contains(&digits) && after.starts_with(SEPARATORS);
        let rest = if has_number {
            after.trim_start_matches(SEPARATORS)
        } else {
            stem
        };
        if track_number.is_none() && has_number {
            track_number = num.parse().ok().filter(|&n| n > 0);
        }
        if title.is_none() {
            let name = rest.trim();
            if !name.is_empty() {
                title = Some(name.to_owned());
            }
        }
    }
    FileClaim {
        title,
        track_number,
        disc_number: number(keys::DISCNUMBER),
        duration_secs: file.duration_secs.filter(|d| d.is_finite() && *d > 0.0),
    }
}

fn duration_similarity(file_secs: f64, track_ms: u64) -> f64 {
    let delta = (file_secs - track_ms as f64 / 1000.0).abs();
    if delta <= DURATION_EXACT_SECS {
        1.0
    } else if delta >= DURATION_FAR_SECS {
        0.0
    } else {
        1.0 - (delta - DURATION_EXACT_SECS) / (DURATION_FAR_SECS - DURATION_EXACT_SECS)
    }
}

/// Weighted mean over the evidence the file provides; a file with no
/// usable data scores 0 against everything.
fn pair_score(
    claim: &FileClaim,
    multi_disc: bool,
    medium_position: u32,
    global_position: u32,
    track: &super::Track,
) -> f64 {
    let mut sum = 0.0;
    let mut weight = 0.0;
    if let Some(title) = &claim.title {
        sum += W_TITLE * f64::from(title_similarity(title, track.display_title()));
        weight += W_TITLE;
    }
    if let Some(n) = claim.track_number {
        // Files without a disc number often number tracks across the whole
        // release (6..11 on disc 2), so that also counts as a hit.
        let global_hit = multi_disc && claim.disc_number.is_none() && n == global_position;
        sum += W_NUMBER * f64::from(n == track.position || global_hit);
        weight += W_NUMBER;
    }
    if multi_disc && let Some(d) = claim.disc_number {
        sum += W_DISC * f64::from(d == medium_position);
        weight += W_DISC;
    }
    if let (Some(secs), Some(ms)) = (claim.duration_secs, track.length_ms()) {
        sum += W_DURATION * duration_similarity(secs, ms);
        weight += W_DURATION;
    }
    if weight == 0.0 { 0.0 } else { sum / weight }
}

/// Matches `files` to the tracks of `release`: entry *i* of the result is
/// the track file *i* was assigned to, or `None` when no track scored at
/// least [`MIN_MATCH_SCORE`] (or the release has fewer tracks than files).
/// No two files share a track.
#[must_use]
pub fn match_files(files: &[FileForMatch<'_>], release: &Release) -> Vec<Option<TrackMatch>> {
    let offsets: Vec<usize> = release
        .media
        .iter()
        .scan(0, |acc, m| {
            let at = *acc;
            *acc += m.tracks.len();
            Some(at)
        })
        .collect();
    let slots: Vec<(usize, usize)> = release
        .media
        .iter()
        .enumerate()
        .flat_map(|(m, medium)| (0..medium.tracks.len()).map(move |t| (m, t)))
        .collect();
    if files.is_empty() || slots.is_empty() {
        return vec![None; files.len()];
    }
    let multi_disc = release.media.len() > 1;
    let claims: Vec<FileClaim> = files.iter().map(claim).collect();
    let scores: Vec<Vec<f64>> = claims
        .iter()
        .map(|c| {
            slots
                .iter()
                .map(|&(m, t)| {
                    let medium = &release.media[m];
                    let global = (offsets[m] + t + 1) as u32;
                    pair_score(c, multi_disc, medium.position, global, &medium.tracks[t])
                })
                .collect()
        })
        .collect();
    assign_max(&scores)
        .into_iter()
        .enumerate()
        .map(|(file, slot)| {
            let slot = slot?;
            let score = scores[file][slot] as f32;
            (score >= MIN_MATCH_SCORE).then(|| TrackMatch {
                medium: slots[slot].0,
                track: slots[slot].1,
                score,
            })
        })
        .collect()
}

/// One-to-one assignment of rows to columns maximising the summed score
/// (Hungarian algorithm, O(n²·m) with n ≤ m). Rows that get no column
/// (more rows than columns) are `None`.
fn assign_max(scores: &[Vec<f64>]) -> Vec<Option<usize>> {
    let rows = scores.len();
    let cols = scores.first().map_or(0, Vec::len);
    if rows == 0 || cols == 0 {
        return vec![None; rows];
    }
    if rows <= cols {
        return hungarian(rows, cols, |r, c| 1.0 - scores[r][c]);
    }
    // Transpose so the smaller side is the one being assigned.
    let by_col = hungarian(cols, rows, |c, r| 1.0 - scores[r][c]);
    let mut out = vec![None; rows];
    for (col, row) in by_col.into_iter().enumerate() {
        if let Some(row) = row {
            out[row] = Some(col);
        }
    }
    out
}

/// Minimum-cost assignment of `n` rows to `m ≥ n` columns.
fn hungarian(n: usize, m: usize, cost: impl Fn(usize, usize) -> f64) -> Vec<Option<usize>> {
    // Potentials and the matching are 1-indexed; index 0 is a sentinel.
    let mut u = vec![0.0f64; n + 1];
    let mut v = vec![0.0f64; m + 1];
    let mut p = vec![0usize; m + 1];
    let mut way = vec![0usize; m + 1];
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0;
        let mut minv = vec![f64::INFINITY; m + 1];
        let mut used = vec![false; m + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = f64::INFINITY;
            let mut j1 = 0;
            for j in 1..=m {
                if used[j] {
                    continue;
                }
                let cur = cost(i0 - 1, j - 1) - u[i0] - v[j];
                if cur < minv[j] {
                    minv[j] = cur;
                    way[j] = j0;
                }
                if minv[j] < delta {
                    delta = minv[j];
                    j1 = j;
                }
            }
            for j in 0..=m {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut out = vec![None; n];
    for j in 1..=m {
        if p[j] != 0 {
            out[p[j] - 1] = Some(j - 1);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::fixture_release;
    use super::*;

    fn tagged(title: &str, number: &str, disc: Option<&str>) -> TagSet {
        let mut t = TagSet::new();
        t.set_one(keys::TITLE, title);
        t.set_one(keys::TRACKNUMBER, number);
        if let Some(d) = disc {
            t.set_one(keys::DISCNUMBER, d);
        }
        t
    }

    fn file<'a>(tags: &'a TagSet, secs: Option<f64>) -> FileForMatch<'a> {
        FileForMatch {
            tags,
            duration_secs: secs,
            file_stem: None,
        }
    }

    #[test]
    fn title_similarity_ignores_case_punctuation_and_qualifiers() {
        assert!(title_similarity("The Mark (Interlude)", "the mark interlude") > 0.99);
        assert!(title_similarity("Song (Remastered 2011)", "Song") > 0.99);
        assert!(title_similarity("Bad Kingdom", "Bad  Kingdom!") > 0.99);
        assert!(title_similarity("Bad Kingdom", "Finalise") < 0.4);
        assert_eq!(title_similarity("", ""), 1.0);
        assert!(title_similarity("日本語の曲", "日本語の曲") > 0.99);
    }

    #[test]
    fn assignment_is_globally_optimal_not_greedy() {
        // Greedy takes (0,0)=0.9 first and leaves row 1 with 0.0 (total 0.9);
        // the optimum is (0,1)+(1,0) = 0.8 + 0.85.
        let scores = vec![vec![0.9, 0.8], vec![0.85, 0.0]];
        assert_eq!(assign_max(&scores), vec![Some(1), Some(0)]);
        // More rows than columns: the weakest row stays unassigned.
        let scores = vec![vec![0.9], vec![0.1], vec![0.5]];
        assert_eq!(assign_max(&scores), vec![Some(0), None, None]);
        assert_eq!(assign_max(&[]), Vec::<Option<usize>>::new());
    }

    #[test]
    fn matches_shuffled_files_by_title_and_number() {
        let r = fixture_release();
        // Pick tracks: B2 (medium 0, idx 4) and D3 (medium 1, idx 5), C1 (1, 0).
        let titles: Vec<String> = [(0, 4), (1, 5), (1, 0)]
            .iter()
            .map(|&(m, t)| r.media[m].tracks[t].title.clone())
            .collect();
        // Titles only (no numbers), in an order unrelated to the release.
        let t0 = {
            let mut t = TagSet::new();
            t.set_one(keys::TITLE, titles[1].to_lowercase());
            t
        };
        let t1 = {
            let mut t = TagSet::new();
            t.set_one(keys::TITLE, titles[2].as_str());
            t
        };
        let t2 = {
            let mut t = TagSet::new();
            t.set_one(keys::TITLE, titles[0].as_str());
            t
        };
        let m = match_files(&[file(&t0, None), file(&t1, None), file(&t2, None)], &r);
        let at = |i: usize| m[i].map(|x| (x.medium, x.track));
        assert_eq!(at(0), Some((1, 5)));
        assert_eq!(at(1), Some((1, 0)));
        assert_eq!(at(2), Some((0, 4)));
        assert!(m.iter().flatten().all(|x| x.score > 0.9));
    }

    #[test]
    fn disc_and_track_numbers_disambiguate_duplicate_titles() {
        let r = fixture_release();
        // Same junk title on both files; disc number + track number decide.
        let a = tagged("Untitled", "2", Some("2"));
        let b = tagged("Untitled", "2", Some("1"));
        let m = match_files(&[file(&a, None), file(&b, None)], &r);
        // The numbers alone only score 0.5 (title is junk); both still pass the floor.
        assert_eq!(m[0].map(|x| (x.medium, x.track)), Some((1, 1)));
        assert_eq!(m[1].map(|x| (x.medium, x.track)), Some((0, 1)));
    }

    #[test]
    fn release_wide_track_numbers_match_later_discs() {
        let r = fixture_release();
        // Track "7" with no disc number: the 2nd track of disc 2 (5 + 2).
        let t = {
            let mut t = TagSet::new();
            t.set_one(keys::TRACKNUMBER, "7");
            t
        };
        let m = match_files(&[file(&t, None)], &r);
        assert_eq!(m[0].map(|x| (x.medium, x.track)), Some((1, 1)));
        // With an explicit disc number only the in-disc position counts.
        let t = tagged("x", "7", Some("2"));
        assert_eq!(match_files(&[file(&t, None)], &r), vec![None]);
    }

    #[test]
    fn duration_breaks_ties_between_identical_titles() {
        let r = fixture_release();
        let len = |m: usize, t: usize| r.media[m].tracks[t].length_ms().unwrap() as f64 / 1000.0;
        let title = r.media[0].tracks[1].title.clone();
        let tags = {
            let mut t = TagSet::new();
            t.set_one(keys::TITLE, title);
            t
        };
        // Only the duration says which track it is.
        let m = match_files(&[file(&tags, Some(len(0, 1) + 0.4))], &r);
        assert_eq!(m[0].map(|x| (x.medium, x.track)), Some((0, 1)));
        assert!(m[0].unwrap().score > 0.95);
    }

    #[test]
    fn falls_back_to_the_file_name() {
        let r = fixture_release();
        let title = r.media[0].tracks[2].title.clone();
        let tags = TagSet::new();
        let stem = format!("03 - {title}");
        let m = match_files(
            &[FileForMatch {
                tags: &tags,
                duration_secs: None,
                file_stem: Some(&stem),
            }],
            &r,
        );
        // Medium 0 has track position 3 and the matching title; the same
        // position on medium 1 has a different title.
        assert_eq!(m[0].map(|x| (x.medium, x.track)), Some((0, 2)));
    }

    #[test]
    fn unrelated_files_stay_unmatched() {
        let r = fixture_release();
        let tags = tagged("Completely Different Song Name Here", "99", None);
        let m = match_files(&[file(&tags, Some(1000.0))], &r);
        assert_eq!(m, vec![None]);
        // No usable data at all.
        let empty = TagSet::new();
        assert_eq!(match_files(&[file(&empty, None)], &r), vec![None]);
    }

    #[test]
    fn surplus_files_never_share_a_track() {
        let r = fixture_release();
        // Every track once, plus a duplicate of the very first one.
        let mut tags: Vec<TagSet> = r
            .media
            .iter()
            .flat_map(|medium| {
                medium.tracks.iter().map(|t| {
                    tagged(
                        &t.title,
                        &t.position.to_string(),
                        Some(&medium.position.to_string()),
                    )
                })
            })
            .collect();
        tags.push(tags[0].clone());
        let files: Vec<FileForMatch<'_>> = tags.iter().map(|t| file(t, None)).collect();
        let m = match_files(&files, &r);
        assert_eq!(m.len(), 12);
        let mut seen = std::collections::HashSet::new();
        for hit in m.iter().flatten() {
            assert!(seen.insert((hit.medium, hit.track)), "track used twice");
        }
        // Eleven tracks, twelve files: exactly one of the two duplicates loses.
        assert_eq!(seen.len(), 11);
        assert_eq!(m.iter().filter(|x| x.is_none()).count(), 1);
        assert!(m[0].is_none() ^ m[11].is_none());
    }
}
