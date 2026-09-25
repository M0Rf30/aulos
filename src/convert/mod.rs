// SPDX-License-Identifier: GPL-3.0

//! Local file conversion / transcoding / CUE-sheet ripping.
//!
//! Pure-Rust by design: decoding goes through symphonia (already a
//! dependency), encoding through `flacenc` (FLAC) and `hound` (WAV) — no
//! lossy encoders exist in pure Rust, and CD-drive ripping needs C bindings,
//! so neither is in scope here. `pipeline` decodes any symphonia-supported
//! input (audio files *and* video containers such as mp4/mkv, since
//! symphonia's probe is content-based and picks the default audio track
//! regardless of container), `encoder` writes the chosen output format, and
//! `cue` splits a single ripped file into per-track outputs from a CUE sheet.

pub mod cue;
pub mod encoder;
pub mod pipeline;
pub mod tag_writer;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub use encoder::OutputFormat;

/// Unique id for a queued/running/finished conversion job.
pub type JobId = u64;

/// What a job does with its source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// Straight format/sample-rate transcode of the whole source file.
    Convert,
    /// Split a single source file into one output per track of a CUE sheet.
    /// `source` is the `.cue` file; the referenced audio file is resolved
    /// relative to it.
    CueSplit,
}

/// Lifecycle state of a [`ConvertJob`], as reported back to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

/// Errors from decoding, encoding, or CUE-parsing a conversion job.
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to decode source: {0}")]
    Decode(String),
    #[error("failed to encode output: {0}")]
    Encode(String),
    #[error("no audio track found in source")]
    NoAudioTrack,
    #[error("invalid CUE sheet: {0}")]
    Cue(String),
    #[error("cancelled")]
    Cancelled,
}

/// A single conversion/rip job tracked by the UI and run by [`run_job`].
///
/// `progress` and `cancel` are shared (`Arc`) with whichever async task is
/// running the job, so the UI can poll progress and request cancellation
/// without message round-trips.
#[derive(Debug, Clone)]
pub struct ConvertJob {
    pub id: JobId,
    pub source: PathBuf,
    pub kind: JobKind,
    pub format: OutputFormat,
    pub target_rate: Option<u32>,
    pub out_dir: PathBuf,
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
    pub state: JobState,
}

impl ConvertJob {
    pub fn new(
        id: JobId,
        source: PathBuf,
        kind: JobKind,
        format: OutputFormat,
        target_rate: Option<u32>,
        out_dir: PathBuf,
    ) -> Self {
        Self {
            id,
            source,
            kind,
            format,
            target_rate,
            out_dir,
            progress: Arc::new(AtomicU32::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            state: JobState::Queued,
        }
    }

    /// Current progress as permille (0-1000) of the job's decode work.
    pub fn progress_permille(&self) -> u32 {
        self.progress.load(Ordering::Relaxed)
    }

    /// Request cancellation; the running job checks this cooperatively and
    /// stops at the next packet boundary.
    pub fn request_cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Runs a single job to completion on a blocking thread, capped at `N=2`
/// concurrently-running jobs via `semaphore` (shared across all in-flight
/// job futures). Returns the job id and the terminal state to report back
/// to the UI through a `Message`, mirroring how library scans report
/// completion.
pub async fn run_job(job: ConvertJob, semaphore: Arc<tokio::sync::Semaphore>) -> (JobId, JobState) {
    let id = job.id;
    let permit = match semaphore.acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return (id, JobState::Failed("job queue closed".to_owned())),
    };

    let state = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match pipeline::run(&job) {
            Ok(()) => JobState::Done,
            Err(ConvertError::Cancelled) => JobState::Cancelled,
            Err(e) => JobState::Failed(e.to_string()),
        }
    })
    .await
    .unwrap_or_else(|e| JobState::Failed(format!("job panicked: {e}")));

    (id, state)
}
