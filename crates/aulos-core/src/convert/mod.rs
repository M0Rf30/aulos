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

pub mod channels;
pub mod cue;
pub mod encoder;
pub mod estimate;
pub mod ffmpeg;
pub mod naming;
pub mod options;
pub mod pipeline;
pub mod source;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use web_time::Instant;

pub use channels::ChannelMode;
pub use encoder::OutputFormat;
pub use source::SourceInfo;

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
    /// Output channel layout (downmix/upmix applied before encoding).
    pub channels: ChannelMode,
    /// File name pattern, see [`naming`].
    pub name_pattern: String,
    /// Split a CUE source into one file per track. When off, the audio file
    /// the sheet refers to is converted as a single file instead.
    pub split_cue: bool,
    /// Carry the source's tags and front cover over to the output. The
    /// title/artist/number a CUE sheet itself provides are always written.
    pub copy_tags: bool,
    /// Trim the encoder delay/padding the source flags (MP3/AAC priming), as in
    /// playback, so the output carries only the real audio. Taken from
    /// `Config::gapless`.
    pub gapless: bool,
}

/// A single conversion/rip job tracked by the UI and run by [`run_job`].
///
/// `progress`, `cancel`, `pause` and the CUE track counters are shared
/// (`Arc`) with whichever async task is running the job, so the UI can poll
/// progress and request cancellation/pausing without message round-trips.
#[derive(Debug, Clone)]
pub struct ConvertJob {
    pub id: JobId,
    pub source: PathBuf,
    pub kind: JobKind,
    /// `None` while `Queued` — see [`JobSettings`]'s docs.
    pub settings: Option<JobSettings>,
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
    /// While set, the running pipeline holds between packets (and a queued
    /// job isn't dispatched). Shared by every job of a queue — see
    /// `ConvertState::pause`.
    pub pause: Arc<AtomicBool>,
    /// 1-based CUE track being converted (0 before the first starts) and the
    /// sheet's track count (0 until parsed); only used by CUE jobs.
    pub cue_track: Arc<AtomicU32>,
    pub cue_total: Arc<AtomicU32>,
    pub state: JobState,
    /// Probed facts about the source; `None` until the probe finished (or if
    /// it failed).
    pub source_info: Option<SourceInfo>,
    /// Start was requested and the job waits for a free slot (or a resume).
    pub scheduled: bool,
    /// When the job started running; `None` while queued.
    pub started_at: Option<Instant>,
    /// Total time spent paused so far, excluding the current pause.
    paused_for: Duration,
    /// Start of the current pause, if the job is paused right now.
    paused_since: Option<Instant>,
    /// Total size of the produced file(s), once `Done`.
    pub output_bytes: Option<u64>,
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
            pause: Arc::new(AtomicBool::new(false)),
            cue_track: Arc::new(AtomicU32::new(0)),
            cue_total: Arc::new(AtomicU32::new(0)),
            state: JobState::Queued,
            source_info: None,
            scheduled: false,
            started_at: None,
            paused_for: Duration::ZERO,
            paused_since: None,
            output_bytes: None,
        }
    }

    /// Makes the job follow `flag` as its pause switch (the queue-wide one).
    #[must_use]
    pub fn with_pause_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.pause = flag;
        self
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
        self.started_at = Some(Instant::now());
        self.scheduled = false;
    }

    /// Records that the queue was paused at `now`. No-op unless running.
    pub fn mark_paused(&mut self, now: Instant) {
        if self.state == JobState::Running && self.paused_since.is_none() {
            self.paused_since = Some(now);
        }
    }

    /// Records that the queue resumed at `now`.
    pub fn mark_resumed(&mut self, now: Instant) {
        if let Some(since) = self.paused_since.take() {
            self.paused_for += now.saturating_duration_since(since);
        }
    }

    /// Time the job has been actively running at `now` (pauses excluded);
    /// `None` while queued.
    #[must_use]
    pub fn elapsed_at(&self, now: Instant) -> Option<Duration> {
        let started = self.started_at?;
        let total = now.saturating_duration_since(started);
        let paused = self.paused_for
            + self
                .paused_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        Some(total.saturating_sub(paused))
    }

    /// Estimated time left at `now`, extrapolated from the progress made
    /// over the active running time. `None` unless running, started, and
    /// past the first permille (nothing to extrapolate from).
    #[must_use]
    pub fn eta_at(&self, now: Instant) -> Option<Duration> {
        if self.state != JobState::Running {
            return None;
        }
        let permille = self.progress_permille();
        if permille == 0 {
            return None;
        }
        let elapsed = self.elapsed_at(now)?.as_secs_f64();
        let remaining = elapsed * f64::from(1000u32.saturating_sub(permille)) / f64::from(permille);
        Some(Duration::from_secs_f64(remaining))
    }

    /// [`Self::eta_at`] for the current instant.
    #[must_use]
    pub fn eta(&self) -> Option<Duration> {
        self.eta_at(Instant::now())
    }

    /// Whether the job is running but held by the queue pause.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.state == JobState::Running && self.pause.load(Ordering::Relaxed)
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
        self.cue_track.store(0, Ordering::Relaxed);
        self.state = JobState::Queued;
        self.scheduled = false;
        self.started_at = None;
        self.paused_for = Duration::ZERO;
        self.paused_since = None;
        self.output_bytes = None;
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
            channels: config.convert_channels,
            name_pattern: config.convert_name_pattern.clone(),
            split_cue: config.convert_split_cue,
            copy_tags: config.convert_copy_tags,
            gapless: config.gapless,
        }
    }
}

#[cfg(test)]
impl JobSettings {
    /// Settings for tests: default encoder options, source channels, the
    /// source file's own name (`{filename}`), CUE splitting and tag copying
    /// on.
    pub(crate) fn for_test(
        format: OutputFormat,
        target_rate: Option<u32>,
        out_dir: PathBuf,
    ) -> Self {
        Self {
            format,
            target_rate,
            out_dir,
            flac_options: encoder::FlacOptions::default(),
            lossy_options: encoder::LossyOptions::default(),
            channels: ChannelMode::Source,
            name_pattern: "{filename}".to_owned(),
            split_cue: true,
            copy_tags: true,
            gapless: true,
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

/// Starts up to `limit` jobs that are queued *and* scheduled (their start
/// was requested) with `settings`, oldest first, and returns a copy of each
/// started job, ready to be handed to [`run_job`].
pub fn start_next(
    jobs: &mut [ConvertJob],
    settings: &JobSettings,
    limit: usize,
) -> Vec<ConvertJob> {
    let mut started = Vec::new();
    for job in jobs {
        if started.len() >= limit {
            break;
        }
        if job.state != JobState::Queued || !job.scheduled {
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

/// How a job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobReport {
    pub id: JobId,
    pub state: JobState,
    /// Total size of the produced file(s); 0 unless the job is `Done`.
    pub output_bytes: u64,
}

/// Runs a single job to completion on a blocking thread and reports how it
/// ended, to be sent back through a `ConvertEvent`, mirroring how library
/// scans report completion. How many jobs run at once is the caller's
/// business (see [`concurrency`]). `job` must already be `Running` with
/// `settings` set (see [`ConvertJob::start`]) — `pipeline::run` panics
/// otherwise.
pub async fn run_job(job: ConvertJob) -> JobReport {
    let id = job.id;
    let (state, output_bytes) = crate::task::spawn_blocking(move || match pipeline::run(&job) {
        Ok(bytes) => (JobState::Done, bytes),
        Err(ConvertError::Cancelled) => (JobState::Cancelled, 0),
        Err(e) => (JobState::Failed(e.to_string()), 0),
    })
    .await
    .unwrap_or_else(|e| (JobState::Failed(format!("job panicked: {e}")), 0));

    JobReport {
        id,
        state,
        output_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(out_dir: &str) -> JobSettings {
        JobSettings::for_test(OutputFormat::Flac, None, PathBuf::from(out_dir))
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
    fn eta_extrapolates_from_progress_and_elapsed_time() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.start(settings("/out"));
        let t0 = job.started_at.unwrap();
        // Nothing to extrapolate from yet.
        assert_eq!(job.eta_at(t0 + Duration::from_secs(5)), None);

        // 25% in 10 s -> 30 s left.
        job.progress.store(250, Ordering::Relaxed);
        assert_eq!(
            job.eta_at(t0 + Duration::from_secs(10)),
            Some(Duration::from_secs(30))
        );
        // Done: nothing left.
        job.progress.store(1000, Ordering::Relaxed);
        assert_eq!(
            job.eta_at(t0 + Duration::from_secs(10)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn eta_is_only_reported_while_running() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.progress.store(500, Ordering::Relaxed);
        assert_eq!(job.eta_at(Instant::now()), None, "queued");
        job.start(settings("/out"));
        job.state = JobState::Done;
        assert_eq!(job.eta_at(Instant::now()), None, "finished");
    }

    #[test]
    fn paused_time_is_excluded_from_elapsed_and_eta() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.start(settings("/out"));
        let t0 = job.started_at.unwrap();
        job.progress.store(500, Ordering::Relaxed);

        job.mark_paused(t0 + Duration::from_secs(10));
        // Still paused 100 s later: only the first 10 s count.
        assert_eq!(
            job.elapsed_at(t0 + Duration::from_secs(110)),
            Some(Duration::from_secs(10))
        );
        job.mark_resumed(t0 + Duration::from_secs(110));
        // 10 s active at 50% -> 10 s left, 20 s after resuming at +120.
        assert_eq!(
            job.eta_at(t0 + Duration::from_secs(110)),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            job.elapsed_at(t0 + Duration::from_secs(120)),
            Some(Duration::from_secs(20))
        );
    }

    #[test]
    fn mark_paused_ignores_jobs_that_are_not_running() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.mark_paused(Instant::now());
        job.start(settings("/out"));
        let t0 = job.started_at.unwrap();
        assert_eq!(
            job.elapsed_at(t0 + Duration::from_secs(4)),
            Some(Duration::from_secs(4))
        );
    }

    #[test]
    fn is_paused_follows_the_shared_flag_for_running_jobs_only() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert)
            .with_pause_flag(Arc::clone(&flag));
        flag.store(true, Ordering::Relaxed);
        assert!(!job.is_paused(), "queued jobs aren't running");
        job.start(settings("/out"));
        assert!(job.is_paused());
        flag.store(false, Ordering::Relaxed);
        assert!(!job.is_paused());
    }

    #[test]
    fn retry_resets_the_run_clock_and_output_size() {
        let mut job = ConvertJob::new(1, PathBuf::from("a.wav"), JobKind::Convert);
        job.start(settings("/out"));
        job.output_bytes = Some(10);
        job.state = JobState::Failed("x".into());
        job.retry();
        assert_eq!(job.started_at, None);
        assert_eq!(job.output_bytes, None);
        assert!(!job.scheduled);
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
    fn start_next_only_starts_scheduled_queued_jobs_up_to_the_limit() {
        let mut jobs = vec![
            job(1, JobState::Queued),
            job(2, JobState::Queued),
            job(3, JobState::Queued),
            job(4, JobState::Done),
        ];
        jobs[0].scheduled = true;
        jobs[2].scheduled = true;
        jobs[3].scheduled = true;
        let settings = JobSettings::from_config(&crate::config::Config::default());

        let started = start_next(&mut jobs, &settings, 1);
        assert_eq!(started.iter().map(|j| j.id).collect::<Vec<_>>(), [1]);
        assert_eq!(jobs[0].state, JobState::Running);
        assert!(jobs[0].started_at.is_some());
        assert!(!jobs[0].scheduled, "a started job is no longer waiting");
        assert_eq!(jobs[1].state, JobState::Queued, "never scheduled");
        assert_eq!(jobs[2].state, JobState::Queued, "over the limit");
        assert_eq!(jobs[3].state, JobState::Done);

        let started = start_next(&mut jobs, &settings, 5);
        assert_eq!(started.iter().map(|j| j.id).collect::<Vec<_>>(), [3]);
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
