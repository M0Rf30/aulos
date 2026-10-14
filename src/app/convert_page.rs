// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Update logic for the local file converter/transcoder/ripper page.
//!
//! Everything the Convert view can trigger lives here, split into two
//! message families dispatched from `update.rs`:
//! - [`crate::views::convert::ConvertMessage`] — direct UI actions (button
//!   presses, dropdown selections), wrapped in `Message::Convert`.
//! - [`ConvertEvent`] — results of async work (file/dir pickers, output-dir
//!   validation, job completion) and the progress ticker, wrapped in
//!   `Message::ConvertEvent`.
//!
//! Output format/sample-rate/directory live in [`aulos_core::config::Config`]
//! (persisted across restarts) rather than duplicated on `AppModel`; a
//! still-`Queued` job has no [`aulos_core::convert::JobSettings`] of its own and
//! is only bound to *current* config values once `Start` actually runs it
//! (see [`aulos_core::convert::ConvertJob::start`]), so changing the dropdowns
//! after adding files does affect not-yet-started jobs — unlike before.

use super::{AppModel, Message};
use crate::views::convert::ConvertMessage;
use aulos_core::convert::options::{
    BITRATE_KBPS_OPTIONS, FLAC_BIT_DEPTH_OPTIONS, FLAC_COMPRESSION_OPTIONS, MP3_MODE_OPTIONS,
    SAMPLE_RATE_OPTIONS, VORBIS_QUALITY_OPTIONS,
};
use aulos_core::convert::{
    self, ConvertJob, JobId, JobKind, JobSettings, JobState, OutputFormat, run_job,
};
use aulos_core::fl;
use cosmic::prelude::*;
use cosmic::widget;
use std::path::PathBuf;
use std::sync::Arc;

/// Async results and ticks for the convert page — see the module docs.
#[derive(Debug, Clone)]
pub enum ConvertEvent {
    /// The "Add files" portal picker finished.
    FilesPicked(Result<Vec<PathBuf>, String>),
    /// The "Change output folder" portal picker finished.
    OutputDirPicked(Result<PathBuf, String>),
    /// The output directory was validated (exists or was created) before
    /// starting the queue; `Ok` carries the settings snapshot to actually
    /// start every currently-queued job with.
    ReadyToStart(Result<JobSettings, String>),
    /// A running job reached a terminal state.
    JobFinished(JobId, JobState),
    /// The off-thread `ffmpeg -version` availability probe finished (see
    /// `AppModel::detect_ffmpeg_once`). The result is already cached
    /// globally by `aulos_core::convert::ffmpeg::detect` itself — this only
    /// exists to trigger a re-render once it's known.
    FfmpegDetected(bool),
    /// Progress ticker while any job is running (see
    /// `subscriptions::convert_tick_stream`) — just triggers a redraw so
    /// the view picks up the latest atomic progress values.
    Tick,
}

impl AppModel {
    /// Resolved output directory: `config.convert_out_dir`, or the default
    /// under the user's music/home directory when unset.
    pub(super) fn convert_out_dir(&self) -> PathBuf {
        convert::out_dir(&self.config)
    }

    /// Handles direct UI actions from the convert view.
    pub(super) fn update_convert(&mut self, msg: ConvertMessage) -> Task<cosmic::Action<Message>> {
        match msg {
            ConvertMessage::AddFiles => cosmic::task::future(async {
                let result = async {
                    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

                    let mut filter = FileFilter::new("Audio, Video & CUE Files");
                    for ext in aulos_core::player::engine::decoder::SUPPORTED_EXTENSIONS
                        .iter()
                        .chain(["cue", "mkv", "mov", "avi"].iter())
                    {
                        filter = filter.glob(&format!("*.{ext}"));
                    }

                    let selected = SelectedFiles::open_file()
                        .title("Select Audio/Video Files or a CUE Sheet")
                        .multiple(true)
                        .modal(true)
                        .filter(filter)
                        .send()
                        .await
                        .map_err(|e| format!("Portal request failed: {e}"))?
                        .response()
                        .map_err(|e| format!("Portal response failed: {e}"))?;

                    let mut paths = Vec::new();
                    for uri in selected.uris() {
                        let uri_str = uri.as_str();
                        let path = uri_str
                            .strip_prefix("file://")
                            .ok_or_else(|| format!("Not a local file URI: {uri_str}"))
                            .and_then(|encoded| {
                                urlencoding::decode(encoded)
                                    .map(|d| PathBuf::from(d.as_ref()))
                                    .map_err(|e| format!("Could not decode URI path: {e}"))
                            })?;
                        paths.push(path);
                    }
                    if paths.is_empty() {
                        Err("No files selected".to_string())
                    } else {
                        Ok(paths)
                    }
                }
                .await;
                cosmic::Action::App(Message::ConvertEvent(ConvertEvent::FilesPicked(result)))
            }),

            ConvertMessage::ChangeOutputDir => cosmic::task::future(async {
                let result = async {
                    use ashpd::desktop::file_chooser::SelectedFiles;

                    let selected = SelectedFiles::open_file()
                        .title("Select Output Directory")
                        .directory(true)
                        .modal(true)
                        .send()
                        .await
                        .map_err(|e| format!("Portal request failed: {e}"))?
                        .response()
                        .map_err(|e| format!("Portal response failed: {e}"))?;

                    let uris = selected.uris();
                    if let Some(uri) = uris.first() {
                        let uri_str = uri.as_str();
                        uri_str
                            .strip_prefix("file://")
                            .ok_or_else(|| format!("Not a local file URI: {uri_str}"))
                            .and_then(|encoded| {
                                urlencoding::decode(encoded)
                                    .map(|d| PathBuf::from(d.as_ref()))
                                    .map_err(|e| format!("Could not decode URI path: {e}"))
                            })
                    } else {
                        Err("No directory selected".to_string())
                    }
                }
                .await;
                cosmic::Action::App(Message::ConvertEvent(ConvertEvent::OutputDirPicked(result)))
            }),

            ConvertMessage::OpenOutputDir => {
                open::that_detached(self.convert_out_dir()).ok();
                Task::none()
            }

            ConvertMessage::FormatSelected(index) => {
                if let Some(format) = OutputFormat::ALL.get(index).copied()
                    && let Some(ctx) = &self.config_context
                    && let Err(e) = self.config.set_convert_format(ctx, format)
                {
                    tracing::error!("Failed to persist convert format: {e}");
                }
                Task::none()
            }

            ConvertMessage::RateSelected(index) => {
                if let Some(&rate) = SAMPLE_RATE_OPTIONS.get(index)
                    && let Some(ctx) = &self.config_context
                    && let Err(e) = self.config.set_convert_sample_rate(ctx, rate)
                {
                    tracing::error!("Failed to persist convert sample rate: {e}");
                }
                Task::none()
            }

            ConvertMessage::FlacCompressionSelected(index) => {
                if let Some(&level) = FLAC_COMPRESSION_OPTIONS.get(index)
                    && let Some(ctx) = &self.config_context
                {
                    let mut opts = self.config.flac_options;
                    opts.compression_level = level;
                    if let Err(e) = self.config.set_flac_options(ctx, opts) {
                        tracing::error!("Failed to persist FLAC compression level: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::FlacBitDepthSelected(index) => {
                if let Some(&depth) = FLAC_BIT_DEPTH_OPTIONS.get(index)
                    && let Some(ctx) = &self.config_context
                {
                    let mut opts = self.config.flac_options;
                    opts.bit_depth = depth;
                    if let Err(e) = self.config.set_flac_options(ctx, opts) {
                        tracing::error!("Failed to persist FLAC bit depth: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::Mp3ModeSelected(index) => {
                if let Some(&mode) = MP3_MODE_OPTIONS.get(index)
                    && let Some(ctx) = &self.config_context
                {
                    let mut opts = self.config.lossy_options;
                    opts.mp3_mode = mode;
                    if let Err(e) = self.config.set_lossy_options(ctx, opts) {
                        tracing::error!("Failed to persist MP3 mode: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::AacBitrateSelected(index) => {
                if let Some(&kbps) = BITRATE_KBPS_OPTIONS.get(index)
                    && let Some(ctx) = &self.config_context
                {
                    let mut opts = self.config.lossy_options;
                    opts.aac_bitrate_kbps = kbps;
                    if let Err(e) = self.config.set_lossy_options(ctx, opts) {
                        tracing::error!("Failed to persist AAC bitrate: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::OpusBitrateSelected(index) => {
                if let Some(&kbps) = BITRATE_KBPS_OPTIONS.get(index)
                    && let Some(ctx) = &self.config_context
                {
                    let mut opts = self.config.lossy_options;
                    opts.opus_bitrate_kbps = kbps;
                    if let Err(e) = self.config.set_lossy_options(ctx, opts) {
                        tracing::error!("Failed to persist Opus bitrate: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::VorbisQualitySelected(index) => {
                if let Some(&quality) = VORBIS_QUALITY_OPTIONS.get(index)
                    && let Some(ctx) = &self.config_context
                {
                    let mut opts = self.config.lossy_options;
                    opts.vorbis_quality = quality;
                    if let Err(e) = self.config.set_lossy_options(ctx, opts) {
                        tracing::error!("Failed to persist Vorbis quality: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::StartQueue => self.start_convert_queue(),

            ConvertMessage::CancelAll => {
                for job in &mut self.convert_jobs {
                    job.cancel();
                }
                Task::none()
            }

            ConvertMessage::ClearFinished => {
                self.convert_jobs.retain(|j| j.state.is_active());
                Task::none()
            }

            ConvertMessage::CancelJob(id) => {
                if let Some(job) = self.convert_jobs.iter_mut().find(|j| j.id == id) {
                    job.cancel();
                }
                Task::none()
            }

            ConvertMessage::RetryJob(id) => {
                if let Some(job) = self.convert_jobs.iter_mut().find(|j| j.id == id) {
                    job.retry();
                }
                Task::none()
            }

            ConvertMessage::RemoveJob(id) => {
                // Keep running jobs even if their id matches — cancel first.
                self.convert_jobs
                    .retain(|j| j.id != id || j.state == JobState::Running);
                Task::none()
            }

            ConvertMessage::OpenJobFolder(id) => {
                if let Some(job) = self.convert_jobs.iter().find(|j| j.id == id)
                    && let Some(settings) = &job.settings
                {
                    open::that_detached(&settings.out_dir).ok();
                }
                Task::none()
            }
        }
    }

    /// Handles async results and ticks for the convert page.
    pub(super) fn update_convert_event(
        &mut self,
        event: ConvertEvent,
    ) -> Task<cosmic::Action<Message>> {
        match event {
            ConvertEvent::FilesPicked(result) => {
                match result {
                    Ok(paths) => {
                        for path in paths {
                            let kind = JobKind::for_path(&path);
                            let id = self.convert_next_id;
                            self.convert_next_id += 1;
                            self.convert_jobs.push(ConvertJob::new(id, path, kind));
                        }
                    }
                    Err(e) => tracing::warn!("convert: file picker failed: {e}"),
                }
                Task::none()
            }

            ConvertEvent::OutputDirPicked(result) => {
                match result {
                    Ok(path) => {
                        self.convert_dir_error = None;
                        if let Some(ctx) = &self.config_context
                            && let Err(e) = self.config.set_convert_out_dir(ctx, Some(path))
                        {
                            tracing::error!("Failed to persist convert output dir: {e}");
                        }
                    }
                    Err(e) => tracing::warn!("convert: output directory picker failed: {e}"),
                }
                Task::none()
            }

            ConvertEvent::ReadyToStart(result) => match result {
                Ok(settings) => {
                    let tasks: Vec<_> = convert::start_queued(&mut self.convert_jobs, &settings)
                        .into_iter()
                        .map(|job| {
                            let semaphore = Arc::clone(&self.convert_semaphore);
                            cosmic::task::future(async move {
                                let (id, state) = run_job(job, semaphore).await;
                                cosmic::Action::App(Message::ConvertEvent(
                                    ConvertEvent::JobFinished(id, state),
                                ))
                            })
                        })
                        .collect();
                    Task::batch(tasks)
                }
                Err(reason) => {
                    self.convert_dir_error = Some(reason);
                    Task::none()
                }
            },

            ConvertEvent::JobFinished(id, state) => {
                if let Some(job) = self.convert_jobs.iter_mut().find(|j| j.id == id) {
                    job.state = state;
                }

                // Queue just drained (no more queued/running jobs): summarize
                // the batch in a toast. Guarded on "was something finished at
                // all" so clearing an already-empty queue stays silent.
                let Some((done, failed)) = convert::finished_batch(&self.convert_jobs) else {
                    return Task::none();
                };
                self.push_toast(widget::toaster::Toast::new(if failed > 0 {
                    fl!(
                        "toast-convert-queue-done-with-failures",
                        done = done,
                        failed = failed
                    )
                } else {
                    fl!("toast-convert-queue-done", done = done)
                }))
            }

            ConvertEvent::FfmpegDetected(_) => Task::none(),

            ConvertEvent::Tick => Task::none(),
        }
    }

    /// Fires the off-thread `ffmpeg` availability probe if it hasn't run
    /// yet this session (see `aulos_core::convert::ffmpeg`) — safe to call
    /// every time the Convert page is opened, since it's a no-op once the
    /// result is cached.
    pub(super) fn detect_ffmpeg_once(&self) -> Task<cosmic::Action<Message>> {
        if aulos_core::convert::ffmpeg::cached().is_some() {
            return Task::none();
        }
        cosmic::task::future(async {
            let available = tokio::task::spawn_blocking(aulos_core::convert::ffmpeg::detect)
                .await
                .unwrap_or(false);
            cosmic::Action::App(Message::ConvertEvent(ConvertEvent::FfmpegDetected(
                available,
            )))
        })
    }

    /// Validates (creating if needed) the current output directory off the
    /// UI thread, then — once confirmed usable — starts every currently
    /// queued job with a settings snapshot taken now (format/rate/out_dir),
    /// via [`ConvertEvent::ReadyToStart`].
    fn start_convert_queue(&mut self) -> Task<cosmic::Action<Message>> {
        self.convert_dir_error = None;
        if !self
            .convert_jobs
            .iter()
            .any(|j| j.state == JobState::Queued)
        {
            return Task::none();
        }

        let settings = JobSettings::from_config(&self.config);

        cosmic::task::future(async move {
            let out_dir = settings.out_dir.clone();
            let validation = tokio::task::spawn_blocking(move || convert::ensure_out_dir(&out_dir))
                .await
                .unwrap_or_else(|e| Err(format!("validation task panicked: {e}")));

            cosmic::Action::App(Message::ConvertEvent(ConvertEvent::ReadyToStart(
                validation.map(|()| settings),
            )))
        })
    }
}
