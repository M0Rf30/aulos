// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use crate::config::Config;
use crate::convert::{
    self, ConvertJob, JobId, JobKind, JobSettings, JobState, concurrency, run_job,
};
use crate::task::Effect;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Async results of the conversion workflows; feed them to
/// [`ConvertState::apply_event`].
#[derive(Debug, Clone)]
pub enum ConvertEvent {
    /// The output directory was validated (exists or was created) before
    /// starting the queue; `Ok` carries the settings snapshot to actually
    /// start every currently-queued job with.
    ReadyToStart(Result<JobSettings, String>),
    /// A running job reached a terminal state.
    JobFinished(JobId, JobState),
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
}

/// Local file conversion / transcoding / CUE-ripping jobs.
pub struct ConvertState {
    /// Queued/running/finished conversion jobs, oldest first.
    pub jobs: Vec<ConvertJob>,
    /// Monotonic id source for new jobs.
    pub next_id: u64,
    /// Caps concurrently-running conversion jobs, shared across every
    /// in-flight job future — see [`crate::convert::concurrency`].
    pub semaphore: Arc<Semaphore>,
    /// Error from the last failed output-directory validation; cleared on
    /// every new start.
    pub dir_error: Option<String>,
}

impl ConvertState {
    pub(super) fn new() -> Self {
        Self {
            jobs: Vec::new(),
            next_id: 0,
            semaphore: Arc::new(Semaphore::new(concurrency())),
            dir_error: None,
        }
    }

    /// Queues one job per path (kind inferred from the file).
    pub fn enqueue(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            let kind = JobKind::for_path(&path);
            let id = self.next_id;
            self.next_id += 1;
            self.jobs.push(ConvertJob::new(id, path, kind));
        }
    }

    /// Validates (creating if needed) the output directory off the UI
    /// thread, then — once confirmed usable — starts every currently queued
    /// job with a settings snapshot of `config` taken now (format/rate/
    /// out_dir), via [`ConvertEvent::ReadyToStart`]. A no-op when nothing is
    /// queued.
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

    /// Cancels every job.
    pub fn cancel_all(&mut self) {
        for job in &mut self.jobs {
            job.cancel();
        }
    }

    /// Drops every finished job, keeping the queued and running ones.
    pub fn clear_finished(&mut self) {
        self.jobs.retain(|j| j.state.is_active());
    }

    pub fn cancel(&mut self, id: JobId) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.cancel();
        }
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
                let started = convert::start_queued(&mut self.jobs, &settings);
                let effect = Effect::batch(started.into_iter().map(|job| {
                    let semaphore = Arc::clone(&self.semaphore);
                    Effect::future(async move {
                        let (id, state) = run_job(job, semaphore).await;
                        ConvertEvent::JobFinished(id, state)
                    })
                }));
                ConvertUpdate {
                    effect,
                    notice: None,
                }
            }
            ConvertEvent::ReadyToStart(Err(reason)) => {
                self.dir_error = Some(reason);
                ConvertUpdate::none()
            }
            ConvertEvent::JobFinished(id, state) => {
                if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
                    job.state = state;
                }
                // Queue just drained (no more queued/running jobs): summarize
                // the batch. Guarded on "was something finished at all" so
                // clearing an already-empty queue stays silent.
                ConvertUpdate {
                    effect: Effect::none(),
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

    #[test]
    fn queue_drained_reports_the_batch_only_once_nothing_is_active() {
        let mut state = ConvertState::new();
        state.enqueue(vec![PathBuf::from("a.flac"), PathBuf::from("b.flac")]);
        assert_eq!(state.next_id, 2);

        let update = state.apply_event(ConvertEvent::JobFinished(0, JobState::Done));
        assert_eq!(update.notice, None, "job 1 is still queued");

        let update = state.apply_event(ConvertEvent::JobFinished(
            1,
            JobState::Failed("boom".into()),
        ));
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
}
