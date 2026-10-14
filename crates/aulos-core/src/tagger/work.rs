// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The tag editor's background I/O: reading and writing tags, fingerprinting,
//! MusicBrainz and AcoustID, and refreshing the library afterwards.
//!
//! Every job is a plain future that resolves to the [`TaggerEvent`] the UI
//! feeds back to [`super::TaggerState`]; the UI only decides how to run it.
//! All blocking work runs inside `spawn_blocking`. Every job carries the
//! generation / request id the state handed out, so a result that arrives
//! after its files were removed or its lookup replaced is dropped by the
//! state instead of landing on the wrong files.

use super::jobs::{self, LOAD_CHUNK, LOOKUP_MIN_GAP, SUBMIT_BATCH};
use super::state::{CoverRequest, ReleaseRequest, SearchRequest, SeedFile};
use super::{
    FileId, LoadRequest, LoadedFile, SaveJob, ScanFileResult, ScanStart, SubmitJob, SubmitResult,
    SubmitStart, TaggerEvent, edit, thumbs,
};
use crate::fl;
use crate::library::tagio::{self, EditPicture};
use crate::library::{LibraryDb, LibraryScanner, fingerprint, tags};
use crate::online::acoustid::{self, Submission};
use crate::online::musicbrainz::{self, FileForMatch};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::{Mutex, Semaphore};

/// Files being read at once (in chunks of [`LOAD_CHUNK`]).
static LOAD_PERMITS: Semaphore = Semaphore::const_new(4);
/// Files being written at once.
static SAVE_PERMITS: Semaphore = Semaphore::const_new(4);
/// Files being fingerprinted at once.
static FINGERPRINT_PERMITS: Semaphore = Semaphore::const_new(3);
/// When the last AcoustID lookup started, shared by every scan.
static LAST_LOOKUP: Mutex<Option<Instant>> = Mutex::const_new(None);
/// The scan whose files may still be fingerprinted; `0` when none (a
/// cancelled scan's queued files stop at their next checkpoint).
static ACTIVE_SCAN: AtomicU64 = AtomicU64::new(0);

/// Runs blocking work off the UI thread.
async fn blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| format!("background task failed: {e}"))?
}

/// Waits until an AcoustID lookup may start, and claims the slot.
async fn wait_for_lookup_slot() {
    let mut last = LAST_LOOKUP.lock().await;
    let wait = jobs::throttle_wait(*last, Instant::now(), LOOKUP_MIN_GAP);
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    *last = Some(Instant::now());
}

// --- Pickers' follow-ups --------------------------------------------------------

/// Every audio file under a picked folder, as the files to add; an error
/// when there are none.
pub async fn folder_files(dir: PathBuf) -> TaggerEvent {
    let files = blocking(move || Ok(jobs::audio_files_under(&dir))).await;
    TaggerEvent::FilesPicked(match files {
        Ok(files) if files.is_empty() => Err(fl!("tagger-notice-no-audio")),
        other => other,
    })
}

/// Reads a picked image file as a picture of type `kind`.
pub async fn read_image(path: PathBuf, kind: u8) -> TaggerEvent {
    let result = blocking(move || {
        let data =
            std::fs::read(&path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        edit::picture_from_bytes(data, kind)
    })
    .await;
    TaggerEvent::ImagePicked(result)
}

/// Decodes the thumbnail of the picture with `hash`.
pub async fn decode_thumb(hash: u64, bytes: Vec<u8>) -> TaggerEvent {
    let pixels = tokio::task::spawn_blocking(move || thumbs::decode(&bytes))
        .await
        .ok()
        .flatten();
    TaggerEvent::ThumbDecoded { hash, pixels }
}

// --- Loading --------------------------------------------------------------------

/// The jobs reading the files of one load request, in chunks of
/// [`LOAD_CHUNK`], a few chunks at a time.
pub fn load(request: LoadRequest) -> impl Iterator<Item = impl Future<Output = TaggerEvent>> {
    let LoadRequest { generation, paths } = request;
    jobs::split_batches(paths, LOAD_CHUNK)
        .into_iter()
        .map(move |chunk| load_chunk(generation, chunk))
}

async fn load_chunk(generation: u64, chunk: Vec<PathBuf>) -> TaggerEvent {
    let permit = LOAD_PERMITS.acquire().await;
    let paths = chunk.clone();
    let files = match tokio::task::spawn_blocking(move || {
        chunk.into_iter().map(read_file).collect::<Vec<_>>()
    })
    .await
    {
        Ok(files) => files,
        // Every requested file must be answered, or the editor would wait
        // for it forever.
        Err(e) => paths
            .into_iter()
            .map(|path| LoadedFile {
                path,
                read: Err(format!("background task failed: {e}")),
                duration_secs: None,
            })
            .collect(),
    };
    drop(permit);
    TaggerEvent::FilesLoaded { generation, files }
}

fn read_file(path: PathBuf) -> LoadedFile {
    let read = tagio::read(&path).map_err(|e| e.to_string());
    let duration_secs = if read.is_ok() {
        tags::probe(&path, false)
            .map(|p| p.properties.duration.as_secs_f64())
            .filter(|d| *d > 0.0)
    } else {
        None
    };
    LoadedFile {
        path,
        read,
        duration_secs,
    }
}

// --- MusicBrainz ----------------------------------------------------------------

/// Searches releases for a lookup.
pub async fn search(request: SearchRequest) -> TaggerEvent {
    let SearchRequest { request, query } = request;
    let result =
        blocking(move || jobs::search_with_retry(&query, musicbrainz::search_releases)).await;
    TaggerEvent::SearchFinished { request, result }
}

/// Fetches the release the user chose.
pub async fn release(request: ReleaseRequest) -> TaggerEvent {
    let ReleaseRequest {
        request,
        release_id,
    } = request;
    let result = blocking(move || musicbrainz::release(&release_id)).await;
    TaggerEvent::ReleaseFetched {
        request,
        result: result.map(Box::new),
    }
}

/// Fetches a release's Cover Art Archive front cover.
pub async fn cover(request: CoverRequest) -> TaggerEvent {
    let CoverRequest {
        request,
        release_id,
    } = request;
    let result: Result<Option<EditPicture>, String> =
        blocking(move || musicbrainz::fetch_front_cover(&release_id)).await;
    TaggerEvent::CoverFetched { request, result }
}

/// Opens MusicBrainz's "add release" page, seeded with `files`.
pub async fn open_seed(files: Vec<SeedFile>) -> TaggerEvent {
    let result = blocking(move || {
        let for_match: Vec<FileForMatch<'_>> = files
            .iter()
            .map(|file| FileForMatch {
                tags: &file.tags,
                duration_secs: file.duration_secs,
                file_stem: file.file_stem.as_deref(),
            })
            .collect();
        let page = musicbrainz::seed_release_add(&for_match)?;
        open::that_detached(&page).map_err(|e| format!("Could not open the browser: {e}"))
    })
    .await;
    TaggerEvent::SeedOpened(result)
}

// --- Saving ---------------------------------------------------------------------

/// Writes one file's tags.
pub async fn save(job: SaveJob) -> TaggerEvent {
    let SaveJob { file, path, tags } = job;
    let permit = SAVE_PERMITS.acquire().await;
    let result = blocking(move || tagio::write(&path, &tags).map_err(|e| e.to_string())).await;
    drop(permit);
    TaggerEvent::FileSaved { file, result }
}

/// Re-reads the written files into the library database, after forgetting
/// the cached covers of the albums they belonged to. Returns the number of
/// tracks updated.
pub async fn refresh_library(written: Vec<PathBuf>, album_keys: Vec<String>) -> usize {
    tokio::task::spawn_blocking(move || refresh_library_blocking(&written, &album_keys))
        .await
        .unwrap_or(0)
}

fn refresh_library_blocking(written: &[PathBuf], album_keys: &[String]) -> usize {
    let db_path = crate::paths::library_db_path();
    let db = match LibraryDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            tracing::error!("Failed to open DB after saving tags: {e}");
            return 0;
        }
    };
    for key in album_keys {
        if let Err(e) = db.remove_cached_cover(key) {
            tracing::warn!("could not drop cached cover {key}: {e}");
        }
    }
    LibraryScanner::scan_paths(&db, written).unwrap_or_else(|e| {
        tracing::error!("scan_paths after saving tags failed: {e}");
        0
    })
}

// --- AcoustID -------------------------------------------------------------------

/// Makes the scan of `start` the active one and returns one job per file:
/// fingerprint it and look it up with the application key `key`.
pub fn scan(
    start: ScanStart,
    key: String,
) -> impl Iterator<Item = impl Future<Output = TaggerEvent>> {
    ACTIVE_SCAN.store(start.generation, Ordering::Relaxed);
    let generation = start.generation;
    start.files.into_iter().map(move |(file, path)| {
        let key = key.clone();
        async move {
            let result = scan_file(generation, path, key).await;
            TaggerEvent::FileScanned {
                generation,
                file,
                result,
            }
        }
    })
}

/// Stops the running scan: its queued files give up at their next
/// checkpoint.
pub fn cancel_scan() {
    ACTIVE_SCAN.store(0, Ordering::Relaxed);
}

/// Fingerprints one file and looks it up. A scan that was cancelled (or
/// replaced) while the file waited stops without doing the work.
async fn scan_file(generation: u64, path: PathBuf, key: String) -> ScanFileResult {
    let cancelled = || ScanFileResult {
        fingerprint: Err("Scan cancelled".to_owned()),
        lookup: None,
    };
    let permit = FINGERPRINT_PERMITS.acquire().await;
    if ACTIVE_SCAN.load(Ordering::Relaxed) != generation {
        return cancelled();
    }
    let fingerprint = blocking(move || fingerprint::compute(&path)).await;
    drop(permit);
    let fingerprint = match fingerprint {
        Ok(fp) => fp,
        Err(error) => {
            return ScanFileResult {
                fingerprint: Err(error),
                lookup: None,
            };
        }
    };
    wait_for_lookup_slot().await;
    if ACTIVE_SCAN.load(Ordering::Relaxed) != generation {
        return cancelled();
    }
    let sent = fingerprint.clone();
    let lookup = blocking(move || acoustid::lookup(&key, &sent)).await;
    ScanFileResult {
        fingerprint: Ok(fingerprint),
        lookup: Some(lookup),
    }
}

/// A file ready to submit, or why it can't be.
struct Prepared {
    /// Computed here (to keep for later), as opposed to taken from the file.
    computed: Option<fingerprint::Fingerprint>,
    submission: Result<Submission, String>,
}

async fn prepare_submission(job: SubmitJob) -> Prepared {
    let (fingerprint, computed) = match &job.fingerprint {
        Some(fp) => (Ok(fingerprint::Fingerprint::clone(fp)), None),
        None => {
            let permit = FINGERPRINT_PERMITS.acquire().await;
            let path = job.path.clone();
            let result = blocking(move || fingerprint::compute(&path)).await;
            drop(permit);
            match result {
                Ok(fp) => (Ok(fp.clone()), Some(fp)),
                Err(e) => (Err(e), None),
            }
        }
    };
    let submission = fingerprint.map(|fp| {
        let mut submission = Submission::from_tags(fp, &job.tags, Some(job.recording_id.clone()));
        submission.file_format = jobs::file_format(&job.path);
        submission.bitrate_kbps = None;
        submission
    });
    Prepared {
        computed,
        submission,
    }
}

/// Fingerprints what needs it, then sends everything to AcoustID in batches.
pub async fn submit(start: SubmitStart, app_key: String) -> TaggerEvent {
    let SubmitStart { generation, jobs } = start;
    let aborted = |error: String| TaggerEvent::SubmitAborted { generation, error };

    let user_key =
        match blocking(|| crate::credentials::retrieve_password(super::ACOUSTID_USER_KEY_ENTRY))
            .await
        {
            Ok(Some(key)) if !key.trim().is_empty() => key,
            Ok(_) => return aborted(fl!("tagger-notice-no-user-key")),
            Err(e) => return aborted(fl!("tagger-notice-keyring-failed", error = e)),
        };

    let files: Vec<FileId> = jobs.iter().map(|j| j.file).collect();
    let mut set = tokio::task::JoinSet::new();
    for (index, job) in jobs.into_iter().enumerate() {
        set.spawn(async move { (index, prepare_submission(job).await) });
    }
    let mut prepared: Vec<Option<Prepared>> = files.iter().map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        if let Ok((index, item)) = joined {
            prepared[index] = Some(item);
        }
    }

    let mut results: Vec<SubmitResult> = Vec::with_capacity(files.len());
    let mut ready: Vec<(FileId, Option<fingerprint::Fingerprint>, Submission)> = Vec::new();
    for (file, item) in files.into_iter().zip(prepared) {
        match item {
            Some(Prepared {
                computed,
                submission: Ok(submission),
            }) => ready.push((file, computed, submission)),
            Some(Prepared {
                submission: Err(error),
                ..
            }) => results.push(SubmitResult {
                file,
                fingerprint: None,
                outcome: Err(error),
            }),
            None => results.push(SubmitResult {
                file,
                fingerprint: None,
                outcome: Err("The fingerprinting task failed".to_owned()),
            }),
        }
    }

    for batch in jobs::split_batches(ready, SUBMIT_BATCH) {
        let (meta, submissions): (Vec<_>, Vec<_>) = batch
            .into_iter()
            .map(|(file, computed, submission)| ((file, computed), submission))
            .unzip();
        let (app_key, user_key) = (app_key.clone(), user_key.clone());
        let response = blocking(move || acoustid::submit(&app_key, &user_key, &submissions)).await;
        results.extend(jobs::map_submit_response(meta, response));
    }
    TaggerEvent::Submitted {
        generation,
        results,
    }
}
