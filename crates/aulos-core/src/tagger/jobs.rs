// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Pure helpers of the tag editor's background work: what the pickers
//! return, how work is split up, how AcoustID calls are spaced, and how a
//! submission response maps back onto files.

use super::{FileId, SubmitResult};
use crate::library::fingerprint::Fingerprint;
use crate::online::acoustid::SubmissionStatus;
use crate::online::musicbrainz::{ReleaseCandidate, ReleaseQuery};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Files read per background task.
pub const LOAD_CHUNK: usize = 16;
/// Fingerprints sent to AcoustID per `submit` call.
pub const SUBMIT_BATCH: usize = 50;
/// Least time between two AcoustID lookups (the service allows 3 per second).
pub const LOOKUP_MIN_GAP: Duration = Duration::from_millis(350);

/// Splits `items` into consecutive groups of at most `size` (at least 1).
pub fn split_batches<T>(items: Vec<T>, size: usize) -> Vec<Vec<T>> {
    let size = size.max(1);
    let mut batches = Vec::with_capacity(items.len().div_ceil(size));
    let mut iter = items.into_iter().peekable();
    while iter.peek().is_some() {
        batches.push(iter.by_ref().take(size).collect());
    }
    batches
}

/// Whether `path` has an extension Aulos can decode.
pub fn is_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            crate::player::engine::decoder::SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| supported.eq_ignore_ascii_case(ext))
        })
}

/// Every audio file under `root` (recursively), sorted by path.
pub fn audio_files_under(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| is_audio_path(path))
        .collect();
    files.sort();
    files
}

/// The local path of a `file://` URI returned by the file chooser portal.
pub fn uri_to_path(uri: &str) -> Result<PathBuf, String> {
    crate::file_uri_to_path(uri).ok_or_else(|| format!("Not a local file URI: {uri}"))
}

/// How long to wait before the next AcoustID lookup so lookups start at
/// least `min_gap` apart.
pub fn throttle_wait(last: Option<Instant>, now: Instant, min_gap: Duration) -> Duration {
    last.map_or(Duration::ZERO, |last| {
        min_gap.saturating_sub(now.saturating_duration_since(last))
    })
}

/// Searches with `search`; when a query that carried a track count finds
/// nothing, tries once more without the count (releases whose track count
/// MusicBrainz records differently).
pub fn search_with_retry(
    query: &ReleaseQuery,
    search: impl Fn(&ReleaseQuery) -> Result<Vec<ReleaseCandidate>, String>,
) -> Result<Vec<ReleaseCandidate>, String> {
    let hits = search(query)?;
    if hits.is_empty() && query.track_count.is_some() {
        let relaxed = ReleaseQuery {
            track_count: None,
            ..query.clone()
        };
        return search(&relaxed);
    }
    Ok(hits)
}

/// The file extension, lowercase, for AcoustID's `file_format`.
pub fn file_format(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// Maps the response of one `submit` call onto the files it carried, in
/// order. `batch` is each file with the fingerprint computed for it (kept
/// for later). A failed call, or a response with fewer statuses than files,
/// fails the files it does not cover.
pub fn map_submit_response(
    batch: Vec<(FileId, Option<Fingerprint>)>,
    response: Result<Vec<SubmissionStatus>, String>,
) -> Vec<SubmitResult> {
    match response {
        Err(error) => batch
            .into_iter()
            .map(|(file, fingerprint)| SubmitResult {
                file,
                fingerprint,
                outcome: Err(error.clone()),
            })
            .collect(),
        Ok(statuses) => {
            let mut statuses = statuses.into_iter();
            batch
                .into_iter()
                .map(|(file, fingerprint)| SubmitResult {
                    file,
                    fingerprint,
                    outcome: statuses
                        .next()
                        .map(|s| s.status)
                        .ok_or_else(|| "AcoustID returned no status for this file".to_owned()),
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn batches_keep_order_and_bound_their_size() {
        let batches = split_batches((1..=7).collect::<Vec<_>>(), 3);
        assert_eq!(batches, vec![vec![1, 2, 3], vec![4, 5, 6], vec![7]]);
        assert!(split_batches(Vec::<u8>::new(), 3).is_empty());
        // A zero size still makes progress.
        assert_eq!(split_batches(vec![1, 2], 0), vec![vec![1], vec![2]]);
    }

    #[test]
    fn audio_paths_are_recognised_by_extension_ignoring_case() {
        assert!(is_audio_path(Path::new("/m/a.flac")));
        assert!(is_audio_path(Path::new("/m/a.MP3")));
        assert!(!is_audio_path(Path::new("/m/cover.jpg")));
        assert!(!is_audio_path(Path::new("/m/noext")));
    }

    #[test]
    fn folder_walk_keeps_audio_files_sorted_and_recurses() {
        let root = std::env::temp_dir().join(format!("aulos-tagger-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("b/deep")).unwrap();
        std::fs::create_dir_all(root.join("a")).unwrap();
        for name in [
            "b/deep/3.ogg",
            "b/2.mp3",
            "a/1.flac",
            "a/cover.jpg",
            "z.txt",
            "0.opus",
        ] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        let found = audio_files_under(&root);
        let names: Vec<_> = found
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["0.opus", "a/1.flac", "b/2.mp3", "b/deep/3.ogg"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn portal_uris_decode_to_paths() {
        assert_eq!(
            uri_to_path("file:///home/me/My%20Music/a.flac").unwrap(),
            PathBuf::from("/home/me/My Music/a.flac")
        );
        assert!(uri_to_path("https://x/y").is_err());
    }

    #[test]
    fn lookups_are_spaced_by_the_minimum_gap() {
        let t0 = Instant::now();
        let gap = Duration::from_millis(350);
        assert_eq!(throttle_wait(None, t0, gap), Duration::ZERO);
        assert_eq!(throttle_wait(Some(t0), t0, gap), gap);
        assert_eq!(
            throttle_wait(Some(t0), t0 + Duration::from_millis(100), gap),
            Duration::from_millis(250)
        );
        assert_eq!(
            throttle_wait(Some(t0), t0 + Duration::from_millis(400), gap),
            Duration::ZERO
        );
    }

    fn candidate(id: &str) -> ReleaseCandidate {
        ReleaseCandidate {
            id: id.to_owned(),
            ..ReleaseCandidate::default()
        }
    }

    #[test]
    fn an_empty_search_with_a_track_count_is_retried_without_it() {
        let seen = RefCell::new(Vec::new());
        let query = ReleaseQuery {
            artist: "A".into(),
            album: "B".into(),
            track_count: Some(9),
            ..ReleaseQuery::default()
        };
        let result = search_with_retry(&query, |q| {
            seen.borrow_mut().push(q.track_count);
            Ok(if q.track_count.is_some() {
                Vec::new()
            } else {
                vec![candidate("x")]
            })
        });
        assert_eq!(result.unwrap().len(), 1);
        assert_eq!(*seen.borrow(), [Some(9), None]);
    }

    #[test]
    fn searches_that_hit_or_fail_are_not_retried() {
        let calls = RefCell::new(0);
        let query = ReleaseQuery {
            artist: "A".into(),
            track_count: Some(9),
            ..ReleaseQuery::default()
        };
        let hit = search_with_retry(&query, |_| {
            *calls.borrow_mut() += 1;
            Ok(vec![candidate("x")])
        });
        assert!(hit.is_ok());
        let failed = search_with_retry(&query, |_| {
            *calls.borrow_mut() += 1;
            Err("offline".to_owned())
        });
        assert_eq!(failed.unwrap_err(), "offline");
        assert_eq!(*calls.borrow(), 2);

        // Without a track count there is nothing to relax.
        let plain = ReleaseQuery {
            artist: "A".into(),
            ..ReleaseQuery::default()
        };
        let none = search_with_retry(&plain, |_| {
            *calls.borrow_mut() += 1;
            Ok(Vec::new())
        });
        assert!(none.unwrap().is_empty());
        assert_eq!(*calls.borrow(), 3);
    }

    fn status(text: &str) -> SubmissionStatus {
        SubmissionStatus {
            id: 1,
            status: text.to_owned(),
            acoustid: None,
        }
    }

    #[test]
    fn submission_statuses_map_onto_files_in_order() {
        let results = map_submit_response(
            vec![(1, None), (2, None), (3, None)],
            Ok(vec![status("pending"), status("imported")]),
        );
        assert_eq!(results[0].outcome.as_deref(), Ok("pending"));
        assert_eq!(results[1].outcome.as_deref(), Ok("imported"));
        assert!(results[2].outcome.is_err(), "no status for the third file");
        assert_eq!(
            results.iter().map(|r| r.file).collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn a_failed_submission_call_fails_every_file_but_keeps_fingerprints() {
        let fp = Fingerprint::from_raw(10, vec![1, 2, 3]);
        let results = map_submit_response(
            vec![(1, Some(fp.clone())), (2, None)],
            Err("HTTP 503".to_owned()),
        );
        assert!(
            results
                .iter()
                .all(|r| r.outcome == Err("HTTP 503".to_owned()))
        );
        assert_eq!(results[0].fingerprint, Some(fp));
        assert!(results[1].fingerprint.is_none());
    }
}
