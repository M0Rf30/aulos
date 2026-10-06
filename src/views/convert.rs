// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Local file converter/transcoder/ripper view — pick files (audio, video
//! containers, or `.cue` sheets), an output format/rate/folder plus
//! format-specific quality options, and run the queue.
//!
//! The tree shape is the same regardless of queue/job state (a fixed
//! header, an output settings card, a queue-summary card, then either the
//! empty state or the job list) — only the *content* of each slot changes,
//! so switching pages or jobs finishing never resets scroll position or
//! flashes a differently-shaped view (see the crate's iced/libcosmic UI
//! rules). The Output card's format-specific quality row (FLAC
//! compression/bit-depth, MP3 mode, AAC/Opus bitrate, Vorbis quality) is
//! the one part of that card whose *control type* varies by the currently
//! selected format — its position in the card is fixed either way.

use std::path::Path;

use cosmic::iced::{Alignment, Length};
use cosmic::widget;

use crate::convert::encoder::{FlacBitDepth, FlacOptions, LossyOptions, Mp3Mode};
use crate::convert::{ConvertJob, JobId, JobKind, JobState, OutputFormat};
use crate::fl;
use crate::views::common;

/// Sample-rate dropdown options: `None` keeps the source rate.
pub const SAMPLE_RATE_OPTIONS: &[Option<u32>] = &[
    None,
    Some(22_050),
    Some(32_000),
    Some(44_100),
    Some(48_000),
    Some(88_200),
    Some(96_000),
    Some(176_400),
    Some(192_000),
];

/// FLAC compression-level dropdown options (0 fastest/largest .. 8
/// slowest/smallest — see `encoder::apply_compression_level`).
pub const FLAC_COMPRESSION_OPTIONS: [u8; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];

/// FLAC bit-depth dropdown options.
pub const FLAC_BIT_DEPTH_OPTIONS: [FlacBitDepth; 3] = [
    FlacBitDepth::Auto,
    FlacBitDepth::Bits16,
    FlacBitDepth::Bits24,
];

/// MP3 mode/quality dropdown options: a handful of common VBR presets plus
/// common CBR bitrates, rather than exposing the raw 0-9/kbps numbers.
pub const MP3_MODE_OPTIONS: [Mp3Mode; 8] = [
    Mp3Mode::Vbr(0),
    Mp3Mode::Vbr(2),
    Mp3Mode::Vbr(4),
    Mp3Mode::Vbr(6),
    Mp3Mode::Cbr(128),
    Mp3Mode::Cbr(192),
    Mp3Mode::Cbr(256),
    Mp3Mode::Cbr(320),
];

/// Bitrate dropdown options shared by AAC and Opus.
pub const BITRATE_KBPS_OPTIONS: [u32; 6] = [96, 128, 160, 192, 256, 320];

/// Ogg Vorbis `-q:a` quality dropdown options (-1.0 lowest .. 10.0 highest).
pub const VORBIS_QUALITY_OPTIONS: [f32; 7] = [-1.0, 0.0, 2.0, 4.0, 6.0, 8.0, 10.0];

/// Messages from the convert view.
#[derive(Debug, Clone)]
pub enum ConvertMessage {
    /// Open the (multi-select) file picker to add jobs.
    AddFiles,
    /// Open the directory picker to change the output directory.
    ChangeOutputDir,
    /// Open the current output directory in the file manager.
    OpenOutputDir,
    /// User picked an entry in the format dropdown.
    FormatSelected(usize),
    /// User picked an entry in the sample-rate dropdown.
    RateSelected(usize),
    /// User picked an entry in the FLAC compression-level dropdown.
    FlacCompressionSelected(usize),
    /// User picked an entry in the FLAC bit-depth dropdown.
    FlacBitDepthSelected(usize),
    /// User picked an entry in the MP3 mode/quality dropdown.
    Mp3ModeSelected(usize),
    /// User picked an entry in the AAC bitrate dropdown.
    AacBitrateSelected(usize),
    /// User picked an entry in the Opus bitrate dropdown.
    OpusBitrateSelected(usize),
    /// User picked an entry in the Ogg Vorbis quality dropdown.
    VorbisQualitySelected(usize),
    /// Run every queued job.
    StartQueue,
    /// Cancel every queued/running job.
    CancelAll,
    /// Drop every finished (done/failed/cancelled) job from the list.
    ClearFinished,
    /// Cancel a specific job by id.
    CancelJob(JobId),
    /// Requeue a failed/cancelled job by id.
    RetryJob(JobId),
    /// Remove a single non-running job by id.
    RemoveJob(JobId),
    /// Open a finished job's output folder in the file manager.
    OpenJobFolder(JobId),
}

/// Everything [`convert_view`] needs, bundled to keep its signature
/// manageable now that the Output card has per-format quality controls
/// (same pattern as `views::radio::RadioViewProps`).
pub struct ConvertViewProps<'a, 'b> {
    pub jobs: &'a [ConvertJob],
    pub out_dir: &'b Path,
    pub format: OutputFormat,
    pub sample_rate: Option<u32>,
    pub dir_error: Option<&'a str>,
    pub flac_options: FlacOptions,
    pub lossy_options: LossyOptions,
    /// `None` while `ffmpeg`'s availability hasn't been probed yet (see
    /// `crate::app::convert_page::AppModel::detect_ffmpeg_once`) — treated
    /// the same as `Some(false)` for gating purposes until it resolves.
    pub ffmpeg_available: Option<bool>,
}

/// Counts of jobs in each lifecycle state, for the queue summary card.
#[derive(Default, Clone, Copy)]
struct JobCounts {
    queued: usize,
    running: usize,
    done: usize,
    failed: usize,
}

impl JobCounts {
    fn compute(jobs: &[ConvertJob]) -> Self {
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
fn overall_progress(jobs: &[ConvertJob]) -> f32 {
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

/// Localized label for a format dropdown entry. The five `ffmpeg`-backed
/// formats always get a "(needs ffmpeg)" suffix, regardless of whether
/// `ffmpeg` is currently available — see [`output_section`]'s separate
/// warning caption for the unavailable case.
fn format_label(format: OutputFormat) -> String {
    let base = match format {
        OutputFormat::Flac => fl!("convert-format-flac"),
        OutputFormat::Wav16 => fl!("convert-format-wav16"),
        OutputFormat::Wav24 => fl!("convert-format-wav24"),
        OutputFormat::Wav32Float => fl!("convert-format-wav32float"),
        OutputFormat::Aiff16 => fl!("convert-format-aiff16"),
        OutputFormat::Aiff24 => fl!("convert-format-aiff24"),
        OutputFormat::Mp3 => fl!("convert-format-mp3"),
        OutputFormat::Aac => fl!("convert-format-aac"),
        OutputFormat::Opus => fl!("convert-format-opus"),
        OutputFormat::OggVorbis => fl!("convert-format-vorbis"),
        OutputFormat::Alac => fl!("convert-format-alac"),
    };
    if format.requires_ffmpeg() {
        fl!("convert-format-needs-ffmpeg", format = base)
    } else {
        base
    }
}

/// Localized label for a sample-rate dropdown entry.
fn rate_label(rate: Option<u32>) -> String {
    match rate {
        None => fl!("convert-rate-source"),
        Some(hz) => fl!("convert-rate-hz", hz = hz),
    }
}

/// Localized label for a FLAC bit-depth dropdown entry.
fn flac_bit_depth_label(depth: FlacBitDepth) -> String {
    match depth {
        FlacBitDepth::Auto => fl!("convert-flac-bitdepth-auto"),
        FlacBitDepth::Bits16 => fl!("convert-flac-bitdepth-16"),
        FlacBitDepth::Bits24 => fl!("convert-flac-bitdepth-24"),
    }
}

/// Localized label for an MP3 mode/quality dropdown entry.
fn mp3_mode_label(mode: Mp3Mode) -> String {
    match mode {
        Mp3Mode::Vbr(q) => fl!("convert-mp3-vbr", q = q),
        Mp3Mode::Cbr(kbps) => fl!("convert-mp3-cbr", kbps = kbps),
    }
}

/// Localized label for a kbps bitrate dropdown entry (AAC/Opus). A named
/// function rather than an inline `fl!()` call inside `.map()`: nested
/// directly in a `.map(...).collect::<Vec<_>>()` closure, `fl!`'s
/// internal `FluentValue` conversion can't infer `*hz`'s type even though
/// it's already the concrete `u32` `BITRATE_KBPS_OPTIONS` always yields.
fn bitrate_kbps_label(kbps: u32) -> String {
    fl!("convert-rate-hz-kbps", kbps = kbps)
}

/// Localized label for a job's kind.
fn kind_label(kind: JobKind) -> String {
    match kind {
        JobKind::Convert => fl!("convert-kind-convert"),
        JobKind::CueSplit => fl!("convert-kind-cuesplit"),
    }
}

/// Localized label for a job's lifecycle state.
fn state_label(state: &JobState) -> String {
    match state {
        JobState::Queued => fl!("convert-state-queued"),
        JobState::Running => fl!("convert-state-running"),
        JobState::Done => fl!("convert-state-done"),
        JobState::Failed(_) => fl!("convert-state-failed-chip"),
        JobState::Cancelled => fl!("convert-state-cancelled"),
    }
}

/// Bare icon button with a caption tooltip. Takes an owned `String` label
/// (rather than `common::icon_button`'s borrowed `&'a str`) so it can be
/// built from `fl!()` — see `now_playing::preset_browser::panel_icon_button`
/// for the same pattern and why the borrowed signature can't take it.
fn job_icon_button<'a>(
    icon_name: &'static str,
    label: String,
    on_press: ConvertMessage,
    destructive: bool,
) -> cosmic::Element<'a, ConvertMessage> {
    let mut button =
        widget::button::icon(widget::icon::from_name(icon_name).size(16)).on_press(on_press);
    if destructive {
        button = button.class(cosmic::theme::Button::Destructive);
    }
    widget::tooltip(
        button,
        widget::text::caption(label),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// The Output card's format-specific quality row: FLAC gets a compression
/// level + bit depth dropdown, each `ffmpeg`-backed format gets its own
/// quality/bitrate dropdown, and lossless formats with no extra knob
/// (WAV/AIFF/ALAC, whose bit depth is already picked via the format
/// dropdown itself) get none. Always built at the same position in
/// `output_section`'s settings section regardless of which arm runs, so
/// only the *content* of that slot varies with the selected format.
fn quality_items(
    format: OutputFormat,
    flac_options: FlacOptions,
    lossy_options: LossyOptions,
) -> Vec<cosmic::Element<'static, ConvertMessage>> {
    match format {
        OutputFormat::Flac => {
            let compression_index = FLAC_COMPRESSION_OPTIONS
                .iter()
                .position(|&l| l == flac_options.compression_level)
                .unwrap_or(5);
            let bit_depth_index = FLAC_BIT_DEPTH_OPTIONS
                .iter()
                .position(|&d| d == flac_options.bit_depth)
                .unwrap_or(0);
            vec![
                widget::settings::item(
                    fl!("convert-flac-compression"),
                    widget::dropdown(
                        FLAC_COMPRESSION_OPTIONS
                            .iter()
                            .map(|l| l.to_string())
                            .collect::<Vec<_>>(),
                        Some(compression_index),
                        ConvertMessage::FlacCompressionSelected,
                    ),
                )
                .into(),
                widget::settings::item(
                    fl!("convert-flac-bitdepth"),
                    widget::dropdown(
                        FLAC_BIT_DEPTH_OPTIONS
                            .iter()
                            .map(|&d| flac_bit_depth_label(d))
                            .collect::<Vec<_>>(),
                        Some(bit_depth_index),
                        ConvertMessage::FlacBitDepthSelected,
                    ),
                )
                .into(),
            ]
        }
        OutputFormat::Mp3 => {
            let index = MP3_MODE_OPTIONS
                .iter()
                .position(|&m| m == lossy_options.mp3_mode)
                .unwrap_or(1);
            vec![
                widget::settings::item(
                    fl!("convert-mp3-mode"),
                    widget::dropdown(
                        MP3_MODE_OPTIONS
                            .iter()
                            .map(|&m| mp3_mode_label(m))
                            .collect::<Vec<_>>(),
                        Some(index),
                        ConvertMessage::Mp3ModeSelected,
                    ),
                )
                .into(),
            ]
        }
        OutputFormat::Aac => {
            let index = BITRATE_KBPS_OPTIONS
                .iter()
                .position(|&b| b == lossy_options.aac_bitrate_kbps)
                .unwrap_or(3);
            vec![
                widget::settings::item(
                    fl!("convert-bitrate"),
                    widget::dropdown(
                        BITRATE_KBPS_OPTIONS
                            .iter()
                            .map(|&hz| bitrate_kbps_label(hz))
                            .collect::<Vec<_>>(),
                        Some(index),
                        ConvertMessage::AacBitrateSelected,
                    ),
                )
                .into(),
            ]
        }
        OutputFormat::Opus => {
            let index = BITRATE_KBPS_OPTIONS
                .iter()
                .position(|&b| b == lossy_options.opus_bitrate_kbps)
                .unwrap_or(2);
            vec![
                widget::settings::item(
                    fl!("convert-bitrate"),
                    widget::dropdown(
                        BITRATE_KBPS_OPTIONS
                            .iter()
                            .map(|&hz| bitrate_kbps_label(hz))
                            .collect::<Vec<_>>(),
                        Some(index),
                        ConvertMessage::OpusBitrateSelected,
                    ),
                )
                .into(),
            ]
        }
        OutputFormat::OggVorbis => {
            let index = VORBIS_QUALITY_OPTIONS
                .iter()
                .position(|&q| (q - lossy_options.vorbis_quality).abs() < 0.01)
                .unwrap_or(4);
            vec![
                widget::settings::item(
                    fl!("convert-vorbis-quality"),
                    widget::dropdown(
                        VORBIS_QUALITY_OPTIONS
                            .iter()
                            .map(|q| format!("{q:.0}"))
                            .collect::<Vec<_>>(),
                        Some(index),
                        ConvertMessage::VorbisQualitySelected,
                    ),
                )
                .into(),
            ]
        }
        OutputFormat::Wav16
        | OutputFormat::Wav24
        | OutputFormat::Wav32Float
        | OutputFormat::Aiff16
        | OutputFormat::Aiff24
        | OutputFormat::Alac => Vec::new(),
    }
}

/// Output settings card: destination folder, format, sample rate, and
/// format-specific quality options applied to jobs the moment `Start` runs
/// them.
fn output_section<'a>(
    out_dir: &Path,
    format: OutputFormat,
    sample_rate: Option<u32>,
    dir_error: Option<&'a str>,
    flac_options: FlacOptions,
    lossy_options: LossyOptions,
    ffmpeg_available: Option<bool>,
) -> cosmic::Element<'a, ConvertMessage> {
    let format_index = OutputFormat::ALL
        .iter()
        .position(|f| *f == format)
        .unwrap_or(0);
    let rate_index = SAMPLE_RATE_OPTIONS
        .iter()
        .position(|r| *r == sample_rate)
        .unwrap_or(0);

    let dir_row = widget::Row::new()
        .push(common::clipped_cell(
            common::cell_text(out_dir.display().to_string()).into(),
        ))
        .push(
            widget::button::standard(fl!("convert-dir-change"))
                .on_press(ConvertMessage::ChangeOutputDir),
        )
        .push(
            widget::button::standard(fl!("convert-dir-open"))
                .on_press(ConvertMessage::OpenOutputDir),
        )
        .spacing(8)
        .align_y(Alignment::Center);

    let mut section = widget::settings::section()
        .title(fl!("convert-output-section"))
        .add(widget::settings::item(fl!("convert-output-dir"), dir_row))
        .add(widget::settings::item(
            fl!("convert-format"),
            widget::dropdown(
                OutputFormat::ALL
                    .iter()
                    .map(|&f| format_label(f))
                    .collect::<Vec<_>>(),
                Some(format_index),
                ConvertMessage::FormatSelected,
            ),
        ))
        .add(widget::settings::item(
            fl!("convert-sample-rate"),
            widget::dropdown(
                SAMPLE_RATE_OPTIONS
                    .iter()
                    .map(|&r| rate_label(r))
                    .collect::<Vec<_>>(),
                Some(rate_index),
                ConvertMessage::RateSelected,
            ),
        ));

    for item in quality_items(format, flac_options, lossy_options) {
        section = section.add(item);
    }

    if format.requires_ffmpeg() && ffmpeg_available != Some(true) {
        section = section.add(widget::text::body(fl!("convert-requires-ffmpeg")));
    }

    if let Some(reason) = dir_error {
        section = section.add(widget::text::body(fl!(
            "convert-dir-error",
            reason = reason.to_owned()
        )));
    }
    section = section.add(widget::text::caption(fl!("convert-settings-hint")));

    section.into()
}

/// Queue summary card: per-state counts, an always-present overall
/// progress bar, and Start/Cancel-all/Clear-finished actions that disable
/// (rather than disappear) when not applicable. `Start` also stays
/// disabled while the configured format needs `ffmpeg` and it isn't
/// available, since every job would just fail immediately otherwise.
fn summary_section<'a>(
    jobs: &'a [ConvertJob],
    format: OutputFormat,
    ffmpeg_available: Option<bool>,
) -> cosmic::Element<'a, ConvertMessage> {
    let counts = JobCounts::compute(jobs);
    let format_blocked = format.requires_ffmpeg() && ffmpeg_available != Some(true);

    let counts_row = widget::Row::new()
        .push(common::cell_caption(fl!(
            "convert-summary-queued",
            count = counts.queued
        )))
        .push(common::cell_caption(fl!(
            "convert-summary-running",
            count = counts.running
        )))
        .push(common::cell_caption(fl!(
            "convert-summary-done",
            count = counts.done
        )))
        .push(common::cell_caption(fl!(
            "convert-summary-failed",
            count = counts.failed
        )))
        .spacing(16);

    let progress =
        widget::progress_bar::determinate_linear(overall_progress(jobs)).width(Length::Fill);

    let buttons = widget::Row::new()
        .push(
            widget::button::suggested(fl!("convert-start")).on_press_maybe(
                (counts.queued > 0 && !format_blocked).then_some(ConvertMessage::StartQueue),
            ),
        )
        .push(
            widget::button::destructive(fl!("convert-cancel-all")).on_press_maybe(
                (counts.queued + counts.running > 0).then_some(ConvertMessage::CancelAll),
            ),
        )
        .push(
            widget::button::standard(fl!("convert-clear-finished")).on_press_maybe(
                (counts.done + counts.failed > 0).then_some(ConvertMessage::ClearFinished),
            ),
        )
        .spacing(8);

    widget::container(
        widget::Column::new()
            .push(counts_row)
            .push(progress)
            .push(buttons)
            .spacing(8)
            .padding(12),
    )
    .width(Length::Fill)
    .class(cosmic::theme::Container::Card)
    .into()
}

/// One job row: kind icon, filename, target-format/destination caption
/// (plus the failure reason when applicable), a status chip, a
/// constant-height progress slot, and state-appropriate actions.
///
/// `pending_*` are the *current* output settings, used to preview a job
/// that hasn't started yet — its own `settings` are `None` until `Start`
/// actually runs it (see `ConvertJob::start`).
fn job_row<'a>(
    job: &'a ConvertJob,
    pending_format: OutputFormat,
    pending_rate: Option<u32>,
    pending_out_dir: &Path,
) -> cosmic::Element<'a, ConvertMessage> {
    let (format, rate, out_dir): (OutputFormat, Option<u32>, &Path) = match &job.settings {
        Some(settings) => (
            settings.format,
            settings.target_rate,
            settings.out_dir.as_path(),
        ),
        None => (pending_format, pending_rate, pending_out_dir),
    };
    let destination = job.destination_preview(format, out_dir);
    let filename = job
        .source
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?");

    let caption = fl!(
        "convert-job-caption",
        kind = kind_label(job.kind),
        format = format_label(format),
        rate = rate_label(rate),
        dest = destination.display().to_string()
    );

    let mut info = widget::Column::new()
        .push(common::cell_text(filename))
        .push(common::cell_caption(caption))
        .spacing(2);

    if let JobState::Failed(reason) = &job.state {
        info = info.push(widget::tooltip(
            common::cell_caption(fl!("convert-state-failed", error = reason.clone())),
            widget::text::caption(reason.clone()),
            widget::tooltip::Position::Top,
        ));
    }

    // Constant-height progress slot regardless of state, so a row's height
    // never jumps as the job moves through the queue.
    let fraction = match &job.state {
        JobState::Running => job.progress_permille() as f32 / 1000.0,
        JobState::Done => 1.0,
        JobState::Queued | JobState::Failed(_) | JobState::Cancelled => 0.0,
    };
    info = info.push(widget::progress_bar::determinate_linear(fraction).width(Length::Fill));

    let kind_icon = match job.kind {
        JobKind::Convert => "audio-x-generic-symbolic",
        JobKind::CueSplit => "playlist-symbolic",
    };

    let status_chip = widget::container(common::cell_caption(state_label(&job.state)))
        .padding(6)
        .class(cosmic::theme::Container::Card);

    let mut actions = widget::Row::new().spacing(4).align_y(Alignment::Center);
    match job.state {
        JobState::Queued => {
            actions = actions
                .push(job_icon_button(
                    "process-stop-symbolic",
                    fl!("convert-cancel-tooltip"),
                    ConvertMessage::CancelJob(job.id),
                    true,
                ))
                .push(job_icon_button(
                    "user-trash-symbolic",
                    fl!("convert-remove-tooltip"),
                    ConvertMessage::RemoveJob(job.id),
                    false,
                ));
        }
        JobState::Running => {
            actions = actions.push(job_icon_button(
                "process-stop-symbolic",
                fl!("convert-cancel-tooltip"),
                ConvertMessage::CancelJob(job.id),
                true,
            ));
        }
        JobState::Failed(_) | JobState::Cancelled => {
            actions = actions
                .push(job_icon_button(
                    "view-refresh-symbolic",
                    fl!("convert-retry-tooltip"),
                    ConvertMessage::RetryJob(job.id),
                    false,
                ))
                .push(job_icon_button(
                    "user-trash-symbolic",
                    fl!("convert-remove-tooltip"),
                    ConvertMessage::RemoveJob(job.id),
                    false,
                ));
        }
        JobState::Done => {
            actions = actions
                .push(job_icon_button(
                    "folder-open-symbolic",
                    fl!("convert-open-folder-tooltip"),
                    ConvertMessage::OpenJobFolder(job.id),
                    false,
                ))
                .push(job_icon_button(
                    "user-trash-symbolic",
                    fl!("convert-remove-tooltip"),
                    ConvertMessage::RemoveJob(job.id),
                    false,
                ));
        }
    }

    widget::container(
        widget::Row::new()
            .push(widget::icon::from_name(kind_icon).size(32))
            .push(common::clipped_cell(info.into()))
            .push(status_chip)
            .push(actions)
            .spacing(12)
            .align_y(Alignment::Center)
            .padding(8),
    )
    .width(Length::Fill)
    .class(cosmic::theme::Container::Card)
    .into()
}

pub fn convert_view<'a, 'b>(
    props: ConvertViewProps<'a, 'b>,
) -> cosmic::Element<'a, ConvertMessage> {
    let ConvertViewProps {
        jobs,
        out_dir,
        format,
        sample_rate,
        dir_error,
        flac_options,
        lossy_options,
        ffmpeg_available,
    } = props;

    let header = widget::Row::new()
        .push(widget::text::title3(fl!("convert")))
        .push(widget::Space::new().width(Length::Fill))
        .push(
            widget::button::suggested(fl!("convert-add-files")).on_press(ConvertMessage::AddFiles),
        )
        .align_y(Alignment::Center);

    let mut col = widget::Column::new()
        .spacing(16)
        .padding(16)
        .push(header)
        .push(output_section(
            out_dir,
            format,
            sample_rate,
            dir_error,
            flac_options,
            lossy_options,
            ffmpeg_available,
        ))
        .push(summary_section(jobs, format, ffmpeg_available));

    if jobs.is_empty() {
        col = col.push(common::empty_state(
            "document-import-symbolic",
            fl!("no-convert-jobs"),
            fl!("convert-empty-hint"),
        ));
        return col.into();
    }

    let mut list = widget::Column::new().spacing(4);
    for job in jobs {
        list = list.push(job_row(job, format, sample_rate, out_dir));
    }

    col = col
        .push(widget::scrollable(widget::container(list).width(Length::Fill)).height(Length::Fill));
    col.into()
}
