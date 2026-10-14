// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Local file conversion / transcoding / CUE-sheet ripping.
//!
//! Decoding goes through symphonia (already a dependency); encoding is
//! pure Rust for every lossless format (`flacenc` for FLAC, `hound` for
//! WAV, a hand-written writer for AIFF — see `encoder`'s module docs) and
//! falls back to shelling out to the system `ffmpeg` binary for the five
//! formats with no usable pure-Rust encoder (MP3, AAC, Opus, Ogg Vorbis,
//! ALAC — see `ffmpeg`'s module docs for why). CD-drive ripping needs C
//! bindings and stays out of scope either way. `pipeline` decodes any
//! symphonia-supported input (audio files *and* video containers such as
//! mp4/mkv, since symphonia's probe is content-based and picks the
//! default audio track regardless of container), `encoder`/`ffmpeg`
//! together write the chosen output format, and `cue` splits a single
//! ripped file into per-track outputs from a CUE sheet.

pub mod cue;
pub mod encoder;
pub mod ffmpeg;
pub mod options;
pub mod pipeline;

use std::path::{Path, PathBuf};
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

/// Output format/sample-rate/directory a job runs (or ran) with.
///
/// Captured onto a [`ConvertJob`] only once it actually starts (see
/// [`ConvertJob::start`]), not when the job is added to the queue — a
/// queued job previously froze these at add time, so changing the
/// dropdowns afterwards silently did nothing. Now a still-queued job has
/// no `JobSettings` at all and the UI previews it against whatever the
/// dropdowns currently say; only a running/finished job's row reflects
/// what it actually used.
#[derive(Debug, Clone, PartialEq)]
pub struct JobSettings {
    pub format: OutputFormat,
    pub target_rate: Option<u32>,
    pub out_dir: PathBuf,
    pub flac_options: encoder::FlacOptions,
    pub lossy_options: encoder::LossyOptions,
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
    /// `None` while `Queued` — see [`JobSettings`]'s docs.
    pub settings: Option<JobSettings>,
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
    pub state: JobState,
}

impl ConvertJob {
    pub fn new(id: JobId, source: PathBuf, kind: JobKind) -> Self {
        Self {
            id,
            source,
            kind,
            settings: None,
            progress: Arc::new(AtomicU32::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            state: JobState::Queued,
        }
    }

    /// Current progress as permille (0-1000) of the job's decode work.
    pub fn progress_permille(&self) -> u32 {
        self.progress.load(Ordering::Relaxed)
    }

    /// Transitions `Queued` -> `Running`, capturing `settings` as what
    /// this run actually uses. No-op if the job isn't currently queued
    /// (e.g. a stale message for an already-started job).
    pub fn start(&mut self, settings: JobSettings) {
        if self.state != JobState::Queued {
            return;
        }
        self.settings = Some(settings);
        self.state = JobState::Running;
    }

    /// Cancels or dequeues the job, whichever applies to its current
    /// state. A still-queued job has no async task to signal, so it's
    /// simply marked `Cancelled` directly; a running job gets the
    /// cooperative flag `pipeline` checks between packets. No-op once the
    /// job has already reached a terminal state.
    pub fn cancel(&mut self) {
        match self.state {
            JobState::Queued => self.state = JobState::Cancelled,
            JobState::Running => self.cancel.store(true, Ordering::Relaxed),
            JobState::Done | JobState::Failed(_) | JobState::Cancelled => {}
        }
    }

    /// Requeues a failed/cancelled job so `Start` picks it up again with
    /// whatever settings are current at that point. No-op for a job
    /// that's still queued or running.
    pub fn retry(&mut self) {
        if !matches!(self.state, JobState::Failed(_) | JobState::Cancelled) {
            return;
        }
        self.settings = None;
        self.progress.store(0, Ordering::Relaxed);
        self.cancel.store(false, Ordering::Relaxed);
        self.state = JobState::Queued;
    }

    /// Best-effort preview of what this job will produce with
    /// `format`/`out_dir` — cheap, filesystem-free, so it's safe to call
    /// from `view()`. For [`JobKind::Convert`] this may not exactly match
    /// the final unique-ified filename `pipeline`'s collision-avoidance
    /// picks at run time (that check needs a `stat` call); for
    /// [`JobKind::CueSplit`] the per-track filenames come from the CUE
    /// sheet itself, which would need parsing (real file I/O) to preview,
    /// so this just names the destination folder.
    pub fn destination_preview(&self, format: OutputFormat, out_dir: &Path) -> PathBuf {
        match self.kind {
            JobKind::Convert => {
                let stem = self
                    .source
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("track");
                out_dir.join(format!("{stem}.{}", format.extension()))
            }
            JobKind::CueSplit => out_dir.to_path_buf(),
        }
    }
}

/// Counts of jobs in each lifecycle state, for the queue summary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JobCounts {
    pub queued: usize,
    pub running: usize,
    pub done: usize,
    /// Failed or cancelled: finished without producing output.
    pub failed: usize,
}

impl JobCounts {
    #[must_use]
    pub fn compute(jobs: &[ConvertJob]) -> Self {
        let mut counts = Self::default();
        for job in jobs {
            match job.state {
                JobState::Queued => counts.queued += 1,
                JobState::Running => counts.running += 1,
                JobState::Done => counts.done += 1,
                JobState::Failed(_) | JobState::Cancelled => counts.failed += 1,
            }
        }
        counts
    }
}

/// Overall queue progress (0.0-1.0): each queued job counts as 0, each
/// finished (done/failed/cancelled) job as 1, and a running job as its own
/// live fraction — so the bar climbs smoothly instead of jumping in
/// per-job steps.
#[must_use]
pub fn overall_progress(jobs: &[ConvertJob]) -> f32 {
    if jobs.is_empty() {
        return 0.0;
    }
    let sum: f32 = jobs
        .iter()
        .map(|job| match job.state {
            JobState::Queued => 0.0,
            JobState::Running => job.progress_permille() as f32 / 1000.0,
            JobState::Done | JobState::Failed(_) | JobState::Cancelled => 1.0,
        })
        .sum();
    sum / jobs.len() as f32
}

impl JobState {
    /// Still waiting for or doing work (`Queued` or `Running`).
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

impl JobKind {
    /// How a source file is processed: a `.cue` sheet is split, anything
    /// else is transcoded whole.
    #[must_use]
    pub fn for_path(path: &Path) -> Self {
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
        {
            Self::CueSplit
        } else {
            Self::Convert
        }
    }
}

/// Default output directory when `Config::convert_out_dir` is unset.
fn default_out_dir() -> PathBuf {
    dirs::audio_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("Converted")
}

/// Resolved output directory: `config.convert_out_dir`, or the default under
/// the user's music/home directory when unset.
#[must_use]
pub fn out_dir(config: &crate::config::Config) -> PathBuf {
    config
        .convert_out_dir
        .clone()
        .unwrap_or_else(default_out_dir)
}

impl JobSettings {
    /// Snapshot of the persisted converter settings (format, sample rate,
    /// resolved output directory and encoder options).
    #[must_use]
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            format: config.convert_format,
            target_rate: config.convert_sample_rate,
            out_dir: out_dir(config),
            flac_options: config.flac_options,
            lossy_options: config.lossy_options,
        }
    }
}

/// Makes sure `dir` exists (creating it if needed) and is a directory.
/// Blocking filesystem work: run it off the UI thread.
pub fn ensure_out_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    if dir.is_dir() {
        Ok(())
    } else {
        Err(format!("{} is not a directory", dir.display()))
    }
}

/// Starts every still-queued job with `settings` and returns a copy of each
/// job that was started, ready to be handed to [`run_job`].
pub fn start_queued(jobs: &mut [ConvertJob], settings: &JobSettings) -> Vec<ConvertJob> {
    let mut started = Vec::new();
    for job in jobs {
        if job.state != JobState::Queued {
            continue;
        }
        job.start(settings.clone());
        started.push(job.clone());
    }
    started
}

/// `(done, failed)` once the queue has drained (no queued or running job
/// left) after finishing at least one job; `None` while work remains or
/// when nothing ever finished, so clearing an empty queue stays silent.
/// `failed` includes cancelled jobs.
#[must_use]
pub fn finished_batch(jobs: &[ConvertJob]) -> Option<(usize, usize)> {
    let counts = JobCounts::compute(jobs);
    if counts.queued + counts.running > 0 || counts.done + counts.failed == 0 {
        return None;
    }
    Some((counts.done, counts.failed))
}

/// Number of conversion jobs allowed to run concurrently: the system's
/// available parallelism, clamped to a sane range so a single-core box
/// still gets one and a many-core one doesn't oversubscribe the disk/CPU
/// for what's ultimately background batch work.
pub fn concurrency() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(2)
        .clamp(1, 4)
}

/// Runs a single job to completion on a blocking thread, capped at
/// [`concurrency`] concurrently-running jobs via `semaphore` (shared
/// across all in-flight job futures). Returns the job id and the terminal
/// state to report back to the UI through a `Message`, mirroring how
/// library scans report completion. `job` must already be `Running` with
/// `settings` set (see [`ConvertJob::start`]) — `pipeline::run` panics
/// otherwise.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(out_dir: &str) -> JobSettings {
        JobSettings {
            format: OutputFormat::Flac,
            target_rate: None,
            out_dir: PathBuf::from(out_dir),
            flac_options: encoder::FlacOptions::default(),
            lossy_options: encoder::LossyOptions::default(),
        }
    }

    #[test]
    fn new_job_is_queued_with_no_settings() {
        let job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        assert_eq!(job.state, JobState::Queued);
        assert!(job.settings.is_none());
        assert_eq!(job.progress_permille(), 0);
    }

    #[test]
    fn start_captures_settings_and_transitions_to_running() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.start(settings("/out"));
        assert_eq!(job.state, JobState::Running);
        assert_eq!(
            job.settings.as_ref().unwrap().out_dir,
            PathBuf::from("/out")
        );
    }

    #[test]
    fn start_is_a_no_op_once_already_running() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.start(settings("/first"));
        // A second `start` (e.g. a stale re-dispatch) must not clobber the
        // settings the job actually started with.
        job.start(settings("/second"));
        assert_eq!(
            job.settings.as_ref().unwrap().out_dir,
            PathBuf::from("/first")
        );
    }

    #[test]
    fn cancel_dequeues_a_queued_job_directly() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.cancel();
        assert_eq!(job.state, JobState::Cancelled);
        // No async task is running yet, so the cooperative flag is never set.
        assert!(!job.cancel.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn cancel_sets_the_cooperative_flag_on_a_running_job() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.start(settings("/out"));
        job.cancel();
        // State stays `Running` until the async task reports back; only the
        // flag `pipeline` checks between packets is set here.
        assert_eq!(job.state, JobState::Running);
        assert!(job.cancel.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn cancel_is_a_no_op_on_a_terminal_job() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.state = JobState::Done;
        job.cancel();
        assert_eq!(job.state, JobState::Done);
    }

    #[test]
    fn retry_requeues_a_failed_job_and_clears_its_settings_and_progress() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.start(settings("/out"));
        job.progress
            .store(500, std::sync::atomic::Ordering::Relaxed);
        job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        job.state = JobState::Failed("boom".to_owned());

        job.retry();

        assert_eq!(job.state, JobState::Queued);
        assert!(job.settings.is_none());
        assert_eq!(job.progress_permille(), 0);
        assert!(!job.cancel.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn retry_requeues_a_cancelled_job() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.state = JobState::Cancelled;
        job.retry();
        assert_eq!(job.state, JobState::Queued);
    }

    #[test]
    fn retry_is_a_no_op_on_a_queued_or_running_job() {
        let mut queued = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        queued.retry();
        assert_eq!(queued.state, JobState::Queued);

        let mut running = ConvertJob::new(2, PathBuf::from("b.wav"), JobKind::Convert);
        running.start(settings("/out"));
        running.retry();
        assert_eq!(running.state, JobState::Running);
        assert!(running.settings.is_some());
    }

    #[test]
    fn destination_preview_names_a_sibling_file_for_convert_jobs() {
        let job = ConvertJob::new(1, PathBuf::from("/music/track.mp3"), JobKind::Convert);
        let dest = job.destination_preview(OutputFormat::Flac, Path::new("/out"));
        assert_eq!(dest, PathBuf::from("/out/track.flac"));
    }

    #[test]
    fn destination_preview_names_the_folder_for_cue_split_jobs() {
        let job = ConvertJob::new(1, PathBuf::from("/music/album.cue"), JobKind::CueSplit);
        let dest = job.destination_preview(OutputFormat::Wav16, Path::new("/out"));
        assert_eq!(dest, PathBuf::from("/out"));
    }

    #[test]
    fn job_counts_bucket_every_state() {
        let mut jobs: Vec<ConvertJob> = (0..5)
            .map(|i| ConvertJob::new(i, PathBuf::from("a.wav"), JobKind::Convert))
            .collect();
        jobs[1].state = JobState::Running;
        jobs[2].state = JobState::Done;
        jobs[3].state = JobState::Failed("boom".to_owned());
        jobs[4].state = JobState::Cancelled;
        assert_eq!(
            JobCounts::compute(&jobs),
            JobCounts {
                queued: 1,
                running: 1,
                done: 1,
                failed: 2,
            }
        );
    }

    #[test]
    fn overall_progress_blends_finished_and_running_jobs() {
        assert_eq!(overall_progress(&[]), 0.0);
        let mut jobs: Vec<ConvertJob> = (0..4)
            .map(|i| ConvertJob::new(i, PathBuf::from("a.wav"), JobKind::Convert))
            .collect();
        jobs[0].state = JobState::Done;
        jobs[1].state = JobState::Failed("boom".to_owned());
        jobs[2].state = JobState::Running;
        jobs[2]
            .progress
            .store(500, std::sync::atomic::Ordering::Relaxed);
        // 1 + 1 + 0.5 + 0 queued, over 4 jobs.
        assert!((overall_progress(&jobs) - 0.625).abs() < f32::EPSILON);
    }

    #[test]
    fn concurrency_is_always_in_range() {
        let n = concurrency();
        assert!((1..=4).contains(&n), "expected 1..=4, got {n}");
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    fn job(id: JobId, state: JobState) -> ConvertJob {
        let mut job = ConvertJob::new(id, PathBuf::from("a.wav"), JobKind::Convert);
        job.state = state;
        job
    }

    #[test]
    fn cue_sources_are_split_case_insensitively() {
        assert_eq!(JobKind::for_path(Path::new("album.CUE")), JobKind::CueSplit);
        assert_eq!(JobKind::for_path(Path::new("track.flac")), JobKind::Convert);
        assert_eq!(JobKind::for_path(Path::new("cue")), JobKind::Convert);
    }

    #[test]
    fn finished_batch_reports_only_a_drained_non_empty_queue() {
        assert_eq!(finished_batch(&[]), None);
        assert_eq!(finished_batch(&[job(1, JobState::Queued)]), None);
        assert_eq!(
            finished_batch(&[job(1, JobState::Done), job(2, JobState::Running)]),
            None
        );
        assert_eq!(
            finished_batch(&[
                job(1, JobState::Done),
                job(2, JobState::Cancelled),
                job(3, JobState::Failed("x".into())),
            ]),
            Some((1, 2))
        );
    }

    #[test]
    fn start_queued_only_starts_queued_jobs() {
        let mut jobs = vec![job(1, JobState::Queued), job(2, JobState::Done)];
        let settings = JobSettings::from_config(&crate::config::Config::default());
        let started = start_queued(&mut jobs, &settings);
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].id, 1);
        assert_eq!(jobs[0].state, JobState::Running);
        assert_eq!(jobs[1].state, JobState::Done);
    }

    #[test]
    fn ensure_out_dir_creates_missing_directories() {
        let dir = std::env::temp_dir()
            .join(format!("aulos-convert-out-{}", std::process::id()))
            .join("nested");
        assert!(ensure_out_dir(&dir).is_ok());
        assert!(dir.is_dir());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
