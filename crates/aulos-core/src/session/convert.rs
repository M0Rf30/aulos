// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use crate::config::Config;
use crate::convert::{
    self, ConvertJob, JobId, JobKind, JobReport, JobSettings, JobState, SourceInfo, concurrency,
    run_job,
};
use crate::task::Effect;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Async results of the conversion workflows; feed them to
/// [`ConvertState::apply_event`].
#[derive(Debug, Clone)]
pub enum ConvertEvent {
    /// The output directory was validated (exists or was created) before
    /// starting the queue; `Ok` carries the settings snapshot to actually
    /// start every currently-queued job with.
    ReadyToStart(Result<JobSettings, String>),
    /// A queued job's source was probed off-thread (`None`: unreadable).
    SourceProbed(JobId, Option<SourceInfo>),
    /// A running job reached a terminal state.
    JobFinished(JobReport),
    /// The off-thread `ffmpeg -version` availability probe finished. The
    /// result is already cached globally by
    /// [`crate::convert::ffmpeg::detect`] itself — this only exists to let a
    /// UI re-render once it's known.
    FfmpegDetected(bool),
}

/// Something the user should be told about after applying a
/// [`ConvertEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertNotice {
    /// The queue just drained: `done` jobs succeeded, `failed` did not.
    QueueFinished { done: usize, failed: usize },
}

/// What a UI must do after [`ConvertState::apply_event`].
#[must_use]
pub struct ConvertUpdate {
    /// Follow-up work (the started jobs); its events go back into
    /// `apply_event`.
    pub effect: Effect<ConvertEvent>,
    pub notice: Option<ConvertNotice>,
}

impl ConvertUpdate {
    fn none() -> Self {
        Self {
            effect: Effect::none(),
            notice: None,
        }
    }

    fn effect(effect: Effect<ConvertEvent>) -> Self {
        Self {
            effect,
            notice: None,
        }
    }
}

/// Local file conversion / transcoding / CUE-ripping jobs.
///
/// Jobs run through a small scheduler: pressing Start *schedules* every
/// queued job, and at most [`concurrency`] of them run at once — the rest
/// stay `Queued` until a slot frees. Pausing the queue holds the running jobs
/// (see [`crate::convert::pipeline`]) and keeps scheduled jobs from starting
/// until it is resumed.
pub struct ConvertState {
    /// Queued/running/finished conversion jobs, oldest first.
    pub jobs: Vec<ConvertJob>,
    /// Monotonic id source for new jobs.
    pub next_id: u64,
    /// Error from the last failed output-directory validation; cleared on
    /// every new start.
    pub dir_error: Option<String>,
    /// Settings snapshot of the last Start: what every job it scheduled runs
    /// with, even when it only gets a slot later.
    run_settings: Option<JobSettings>,
    /// Queue-wide pause switch, shared with every job.
    pause: Arc<AtomicBool>,
}

impl ConvertState {
    pub(super) fn new() -> Self {
        Self {
            jobs: Vec::new(),
            next_id: 0,
            dir_error: None,
            run_settings: None,
            pause: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Queues one job per path (kind inferred from the file) and probes each
    /// source off-thread; the results come back as
    /// [`ConvertEvent::SourceProbed`].
    pub fn enqueue(&mut self, paths: Vec<PathBuf>) -> Effect<ConvertEvent> {
        let mut probes = Vec::with_capacity(paths.len());
        for path in paths {
            let kind = JobKind::for_path(&path);
            let id = self.next_id;
            self.next_id += 1;
            self.jobs.push(
                ConvertJob::new(id, path.clone(), kind).with_pause_flag(Arc::clone(&self.pause)),
            );
            probes.push(Effect::future(async move {
                let info = tokio::task::spawn_blocking(move || SourceInfo::probe(&path, kind))
                    .await
                    .unwrap_or(None);
                ConvertEvent::SourceProbed(id, info)
            }));
        }
        Effect::batch(probes)
    }

    /// Validates (creating if needed) the output directory off the UI
    /// thread, then — once confirmed usable — schedules every currently
    /// queued job with a settings snapshot of `config` taken now, via
    /// [`ConvertEvent::ReadyToStart`]. A no-op when nothing is queued.
    pub fn start_queue(&mut self, config: &Config) -> Effect<ConvertEvent> {
        self.dir_error = None;
        if !self.jobs.iter().any(|j| j.state == JobState::Queued) {
            return Effect::none();
        }

        let settings = JobSettings::from_config(config);
        Effect::future(async move {
            let out_dir = settings.out_dir.clone();
            let validation = tokio::task::spawn_blocking(move || convert::ensure_out_dir(&out_dir))
                .await
                .unwrap_or_else(|e| Err(format!("validation task panicked: {e}")));
            ConvertEvent::ReadyToStart(validation.map(|()| settings))
        })
    }

    /// Whether the queue is paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.pause.load(Ordering::Relaxed)
    }

    /// Pauses the queue: running jobs hold between packets and scheduled
    /// jobs don't start until [`Self::resume`]. No-op when already paused or
    /// when nothing is queued or running.
    pub fn pause(&mut self) {
        if self.is_paused() || !self.has_active_jobs() {
            return;
        }
        self.pause.store(true, Ordering::Relaxed);
        let now = Instant::now();
        for job in &mut self.jobs {
            job.mark_paused(now);
        }
    }

    /// Resumes a paused queue: running jobs continue and scheduled jobs
    /// start as slots allow. The returned effect runs the newly started
    /// jobs.
    pub fn resume(&mut self) -> Effect<ConvertEvent> {
        if !self.is_paused() {
            return Effect::none();
        }
        self.pause.store(false, Ordering::Relaxed);
        let now = Instant::now();
        for job in &mut self.jobs {
            job.mark_resumed(now);
        }
        self.dispatch()
    }

    /// Cancels every job.
    pub fn cancel_all(&mut self) {
        for job in &mut self.jobs {
            job.cancel();
        }
        self.release_idle_pause();
    }

    /// Drops every finished job, keeping the queued and running ones.
    pub fn clear_finished(&mut self) {
        self.jobs.retain(|j| j.state.is_active());
    }

    pub fn cancel(&mut self, id: JobId) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.cancel();
        }
        self.release_idle_pause();
    }

    pub fn retry(&mut self, id: JobId) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.retry();
        }
    }

    /// Removes a job; a running one is kept (cancel it first).
    pub fn remove(&mut self, id: JobId) {
        self.jobs
            .retain(|j| j.id != id || j.state == JobState::Running);
        self.release_idle_pause();
    }

    fn has_active_jobs(&self) -> bool {
        self.jobs.iter().any(|j| j.state.is_active())
    }

    /// A pause only makes sense while there is something to hold: clears it
    /// once nothing is queued or running (e.g. everything was cancelled), so
    /// the next batch doesn't start out stuck.
    fn release_idle_pause(&mut self) {
        if !self.has_active_jobs() {
            self.pause.store(false, Ordering::Relaxed);
        }
    }

    /// Starts scheduled jobs while slots are free and the queue isn't
    /// paused.
    fn dispatch(&mut self) -> Effect<ConvertEvent> {
        if self.is_paused() {
            return Effect::none();
        }
        let Some(settings) = self.run_settings.as_ref() else {
            return Effect::none();
        };
        let running = self
            .jobs
            .iter()
            .filter(|j| j.state == JobState::Running)
            .count();
        let free = concurrency().saturating_sub(running);
        let started = convert::start_next(&mut self.jobs, settings, free);
        Effect::batch(started.into_iter().map(|job| {
            Effect::future(async move { ConvertEvent::JobFinished(run_job(job).await) })
        }))
    }

    /// Fires the off-thread `ffmpeg` availability probe if it hasn't run yet
    /// this session (see [`crate::convert::ffmpeg`]) — safe to call every
    /// time the Convert page is opened, since it's a no-op once the result
    /// is cached.
    pub fn detect_ffmpeg() -> Effect<ConvertEvent> {
        if convert::ffmpeg::cached().is_some() {
            return Effect::none();
        }
        Effect::future(async {
            let available = tokio::task::spawn_blocking(convert::ffmpeg::detect)
                .await
                .unwrap_or(false);
            ConvertEvent::FfmpegDetected(available)
        })
    }

    /// Applies an async result.
    pub fn apply_event(&mut self, event: ConvertEvent) -> ConvertUpdate {
        match event {
            ConvertEvent::ReadyToStart(Ok(settings)) => {
                for job in &mut self.jobs {
                    if job.state == JobState::Queued {
                        job.scheduled = true;
                    }
                }
                self.run_settings = Some(settings);
                ConvertUpdate::effect(self.dispatch())
            }
            ConvertEvent::ReadyToStart(Err(reason)) => {
                self.dir_error = Some(reason);
                ConvertUpdate::none()
            }
            ConvertEvent::SourceProbed(id, info) => {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                    job.source_info = info;
                }
                ConvertUpdate::none()
            }
            ConvertEvent::JobFinished(report) => {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == report.id) {
                    job.output_bytes =
                        (report.state == JobState::Done).then_some(report.output_bytes);
                    job.state = report.state;
                }
                self.release_idle_pause();
                // Queue just drained (no more queued/running jobs): summarize
                // the batch. Guarded on "was something finished at all" so
                // clearing an already-empty queue stays silent.
                ConvertUpdate {
                    effect: self.dispatch(),
                    notice: convert::finished_batch(&self.jobs)
                        .map(|(done, failed)| ConvertNotice::QueueFinished { done, failed }),
                }
            }
            ConvertEvent::FfmpegDetected(_) => ConvertUpdate::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::OutputFormat;

    fn report(id: JobId, state: JobState) -> ConvertEvent {
        ConvertEvent::JobFinished(JobReport {
            id,
            state,
            output_bytes: 0,
        })
    }

    fn settings() -> JobSettings {
        JobSettings::for_test(OutputFormat::Flac, None, PathBuf::from("/out"))
    }

    /// A state with `n` queued, scheduled jobs and a run snapshot, as after
    /// Start. Dispatch itself isn't exercised here (it needs a runtime for
    /// the effects), only the bookkeeping around it.
    fn started_state(n: usize) -> ConvertState {
        let mut state = ConvertState::new();
        for i in 0..n {
            let id = state.next_id;
            state.next_id += 1;
            state.jobs.push(
                ConvertJob::new(id, PathBuf::from(format!("{i}.flac")), JobKind::Convert)
                    .with_pause_flag(Arc::clone(&state.pause)),
            );
        }
        state
    }

    #[test]
    fn queue_drained_reports_the_batch_only_once_nothing_is_active() {
        let mut state = started_state(2);

        let update = state.apply_event(report(0, JobState::Done));
        assert_eq!(update.notice, None, "job 1 is still queued");

        let update = state.apply_event(report(1, JobState::Failed("boom".into())));
        assert_eq!(
            update.notice,
            Some(ConvertNotice::QueueFinished { done: 1, failed: 1 })
        );
    }

    #[test]
    fn failed_directory_validation_is_recorded() {
        let mut state = ConvertState::new();
        let update = state.apply_event(ConvertEvent::ReadyToStart(Err("nope".into())));
        assert!(update.effect.is_none());
        assert_eq!(state.dir_error.as_deref(), Some("nope"));
    }

    #[test]
    fn finishing_records_the_output_size_only_for_done_jobs() {
        let mut state = started_state(2);
        let _ = state.apply_event(ConvertEvent::JobFinished(JobReport {
            id: 0,
            state: JobState::Done,
            output_bytes: 3_600_000,
        }));
        let _ = state.apply_event(ConvertEvent::JobFinished(JobReport {
            id: 1,
            state: JobState::Cancelled,
            output_bytes: 7,
        }));
        assert_eq!(state.jobs[0].output_bytes, Some(3_600_000));
        assert_eq!(state.jobs[1].output_bytes, None);
    }

    #[test]
    fn probe_results_land_on_their_job() {
        let mut state = started_state(2);
        let info = SourceInfo {
            codec: "FLAC".into(),
            bit_depth: Some(24),
            sample_rate: Some(96_000),
            channels: Some(2),
            duration: None,
            bytes: 1,
            lossless: true,
            cue_tracks: None,
        };
        let _ = state.apply_event(ConvertEvent::SourceProbed(1, Some(info.clone())));
        assert_eq!(state.jobs[0].source_info, None);
        assert_eq!(state.jobs[1].source_info, Some(info));
    }

    #[test]
    fn ready_to_start_schedules_queued_jobs_without_dispatching_while_paused() {
        let mut state = started_state(2);
        state.pause();
        assert!(state.is_paused());

        let update = state.apply_event(ConvertEvent::ReadyToStart(Ok(settings())));
        assert!(update.effect.is_none(), "paused: nothing may start");
        assert!(state.jobs.iter().all(|j| j.state == JobState::Queued));
        assert!(state.jobs.iter().all(|j| j.scheduled));
    }

    #[test]
    fn pause_needs_something_to_hold() {
        let mut state = ConvertState::new();
        state.pause();
        assert!(!state.is_paused());

        let mut state = started_state(1);
        state.pause();
        assert!(state.is_paused());
        state.pause(); // idempotent
        assert!(state.is_paused());
    }

    #[test]
    fn resume_clears_the_flag_for_every_job() {
        let mut state = started_state(2);
        state.jobs[0].start(settings());
        state.pause();
        assert!(
            state.jobs[0].is_paused(),
            "running jobs see the shared flag"
        );

        let effect = state.resume();
        assert!(!state.is_paused());
        assert!(!state.jobs[0].is_paused());
        assert!(effect.is_none(), "no run snapshot, so nothing to dispatch");
        // Resuming a queue that isn't paused is a no-op.
        assert!(state.resume().is_none());
    }

    #[test]
    fn pausing_stops_the_running_jobs_clock() {
        let mut state = started_state(1);
        state.jobs[0].start(settings());
        let t0 = state.jobs[0].started_at.unwrap();
        state.pause();
        let later = t0 + std::time::Duration::from_secs(60);
        assert!(
            state.jobs[0].elapsed_at(later).unwrap() < std::time::Duration::from_secs(60),
            "paused time must not count"
        );
    }

    #[test]
    fn cancelling_everything_releases_the_pause() {
        let mut state = started_state(2);
        state.pause();
        state.cancel_all();
        assert!(state.jobs.iter().all(|j| j.state == JobState::Cancelled));
        assert!(!state.is_paused(), "nothing left to hold");
    }

    #[test]
    fn draining_the_queue_releases_the_pause() {
        let mut state = started_state(1);
        state.jobs[0].start(settings());
        state.pause();
        let _ = state.apply_event(report(0, JobState::Done));
        assert!(!state.is_paused());
    }

    #[test]
    fn retried_jobs_need_a_new_start() {
        let mut state = started_state(1);
        state.jobs[0].scheduled = true;
        state.jobs[0].state = JobState::Failed("x".into());
        state.retry(0);
        assert_eq!(state.jobs[0].state, JobState::Queued);
        assert!(!state.jobs[0].scheduled);
    }

    #[test]
    fn enqueue_infers_the_kind_and_shares_the_pause_flag() {
        let mut state = ConvertState::new();
        let effect = state.enqueue(vec![PathBuf::from("a.flac"), PathBuf::from("b.CUE")]);
        assert!(!effect.is_none());
        assert_eq!(state.next_id, 2);
        assert_eq!(state.jobs[0].kind, JobKind::Convert);
        assert_eq!(state.jobs[1].kind, JobKind::CueSplit);
        assert_eq!(state.jobs[1].source, PathBuf::from("b.CUE"));

        state.pause();
        assert!(state.jobs.iter().all(|j| j.pause.load(Ordering::Relaxed)));
    }
}
