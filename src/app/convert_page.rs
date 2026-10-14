// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Update logic for the local file converter/transcoder/ripper page.
//!
//! Everything the Convert view can trigger lives here, split into two
//! message families dispatched from `update.rs`:
//! - [`crate::views::convert::ConvertMessage`] — direct UI actions (button
//!   presses, dropdown selections), wrapped in `Message::Convert`.
//! - [`ConvertPicked`] — results of the file/directory pickers, wrapped in
//!   `Message::ConvertPicked`.
//!
//! The queue's own results (output-dir validation, job completion) and the
//! progress ticker are session events (`Message::Session`).
//!
//! Output format/sample-rate/directory live in [`aulos_core::config::Config`]
//! (persisted across restarts) rather than duplicated on `AppModel`; a
//! still-`Queued` job has no [`aulos_core::convert::JobSettings`] of its own and
//! is only bound to *current* config values once `Start` actually runs it
//! (see [`aulos_core::convert::ConvertJob::start`]), so changing the dropdowns
//! after adding files does affect not-yet-started jobs — unlike before.

use super::tasks::session_effect;
use super::{AppModel, Message};
use crate::views::convert::ConvertMessage;
use aulos_core::convert::options::{
    BITRATE_KBPS_OPTIONS, FLAC_BIT_DEPTH_OPTIONS, FLAC_COMPRESSION_OPTIONS, MP3_MODE_OPTIONS,
    SAMPLE_RATE_OPTIONS, VORBIS_QUALITY_OPTIONS,
};
use aulos_core::convert::{self, OutputFormat};
use aulos_core::session::ConvertState;
use cosmic::prelude::*;
use std::path::PathBuf;

/// Results of the convert page's portal pickers — see the module docs.
#[derive(Debug, Clone)]
pub enum ConvertPicked {
    /// The "Add files" portal picker finished.
    FilesPicked(Result<Vec<PathBuf>, String>),
    /// The "Change output folder" portal picker finished.
    OutputDirPicked(Result<PathBuf, String>),
}

impl AppModel {
    /// Resolved output directory: `config.convert_out_dir`, or the default
    /// under the user's music/home directory when unset.
    pub(super) fn convert_out_dir(&self) -> PathBuf {
        convert::out_dir(&self.session.config.config)
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
                cosmic::Action::App(Message::ConvertPicked(ConvertPicked::FilesPicked(result)))
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
                cosmic::Action::App(Message::ConvertPicked(ConvertPicked::OutputDirPicked(
                    result,
                )))
            }),

            ConvertMessage::OpenOutputDir => {
                open::that_detached(self.convert_out_dir()).ok();
                Task::none()
            }

            ConvertMessage::FormatSelected(index) => {
                if let Some(format) = OutputFormat::ALL.get(index).copied()
                    && let Some(ctx) = &self.session.config.context
                    && let Err(e) = self.session.config.config.set_convert_format(ctx, format)
                {
                    tracing::error!("Failed to persist convert format: {e}");
                }
                Task::none()
            }

            ConvertMessage::RateSelected(index) => {
                if let Some(&rate) = SAMPLE_RATE_OPTIONS.get(index)
                    && let Some(ctx) = &self.session.config.context
                    && let Err(e) = self
                        .session
                        .config
                        .config
                        .set_convert_sample_rate(ctx, rate)
                {
                    tracing::error!("Failed to persist convert sample rate: {e}");
                }
                Task::none()
            }

            ConvertMessage::FlacCompressionSelected(index) => {
                if let Some(&level) = FLAC_COMPRESSION_OPTIONS.get(index)
                    && let Some(ctx) = &self.session.config.context
                {
                    let mut opts = self.session.config.config.flac_options;
                    opts.compression_level = level;
                    if let Err(e) = self.session.config.config.set_flac_options(ctx, opts) {
                        tracing::error!("Failed to persist FLAC compression level: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::FlacBitDepthSelected(index) => {
                if let Some(&depth) = FLAC_BIT_DEPTH_OPTIONS.get(index)
                    && let Some(ctx) = &self.session.config.context
                {
                    let mut opts = self.session.config.config.flac_options;
                    opts.bit_depth = depth;
                    if let Err(e) = self.session.config.config.set_flac_options(ctx, opts) {
                        tracing::error!("Failed to persist FLAC bit depth: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::Mp3ModeSelected(index) => {
                if let Some(&mode) = MP3_MODE_OPTIONS.get(index)
                    && let Some(ctx) = &self.session.config.context
                {
                    let mut opts = self.session.config.config.lossy_options;
                    opts.mp3_mode = mode;
                    if let Err(e) = self.session.config.config.set_lossy_options(ctx, opts) {
                        tracing::error!("Failed to persist MP3 mode: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::AacBitrateSelected(index) => {
                if let Some(&kbps) = BITRATE_KBPS_OPTIONS.get(index)
                    && let Some(ctx) = &self.session.config.context
                {
                    let mut opts = self.session.config.config.lossy_options;
                    opts.aac_bitrate_kbps = kbps;
                    if let Err(e) = self.session.config.config.set_lossy_options(ctx, opts) {
                        tracing::error!("Failed to persist AAC bitrate: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::OpusBitrateSelected(index) => {
                if let Some(&kbps) = BITRATE_KBPS_OPTIONS.get(index)
                    && let Some(ctx) = &self.session.config.context
                {
                    let mut opts = self.session.config.config.lossy_options;
                    opts.opus_bitrate_kbps = kbps;
                    if let Err(e) = self.session.config.config.set_lossy_options(ctx, opts) {
                        tracing::error!("Failed to persist Opus bitrate: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::VorbisQualitySelected(index) => {
                if let Some(&quality) = VORBIS_QUALITY_OPTIONS.get(index)
                    && let Some(ctx) = &self.session.config.context
                {
                    let mut opts = self.session.config.config.lossy_options;
                    opts.vorbis_quality = quality;
                    if let Err(e) = self.session.config.config.set_lossy_options(ctx, opts) {
                        tracing::error!("Failed to persist Vorbis quality: {e}");
                    }
                }
                Task::none()
            }

            ConvertMessage::StartQueue => session_effect(
                self.session
                    .convert
                    .start_queue(&self.session.config.config),
            ),

            ConvertMessage::CancelAll => {
                self.session.convert.cancel_all();
                Task::none()
            }

            ConvertMessage::ClearFinished => {
                self.session.convert.clear_finished();
                Task::none()
            }

            ConvertMessage::CancelJob(id) => {
                self.session.convert.cancel(id);
                Task::none()
            }

            ConvertMessage::RetryJob(id) => {
                self.session.convert.retry(id);
                Task::none()
            }

            ConvertMessage::RemoveJob(id) => {
                self.session.convert.remove(id);
                Task::none()
            }

            ConvertMessage::OpenJobFolder(id) => {
                if let Some(job) = self.session.convert.jobs.iter().find(|j| j.id == id)
                    && let Some(settings) = &job.settings
                {
                    open::that_detached(&settings.out_dir).ok();
                }
                Task::none()
            }
        }
    }

    /// Handles the results of the convert page's pickers.
    pub(super) fn update_convert_picked(
        &mut self,
        picked: ConvertPicked,
    ) -> Task<cosmic::Action<Message>> {
        match picked {
            ConvertPicked::FilesPicked(result) => match result {
                Ok(paths) => self.session.convert.enqueue(paths),
                Err(e) => tracing::warn!("convert: file picker failed: {e}"),
            },

            ConvertPicked::OutputDirPicked(result) => match result {
                Ok(path) => {
                    self.session.convert.dir_error = None;
                    if let Some(ctx) = &self.session.config.context
                        && let Err(e) = self
                            .session
                            .config
                            .config
                            .set_convert_out_dir(ctx, Some(path))
                    {
                        tracing::error!("Failed to persist convert output dir: {e}");
                    }
                }
                Err(e) => tracing::warn!("convert: output directory picker failed: {e}"),
            },
        }
        Task::none()
    }

    /// Fires the off-thread `ffmpeg` availability probe if it hasn't run
    /// yet this session — safe to call every time the Convert page is
    /// opened, since it's a no-op once the result is cached.
    pub(super) fn detect_ffmpeg_once(&self) -> Task<cosmic::Action<Message>> {
        session_effect(ConvertState::detect_ffmpeg())
    }
}
