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

use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::core::Background;
use cosmic::iced::{Alignment, Border, Color, Length};
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
    let base = format_short_label(format);
    if format.requires_ffmpeg() {
        fl!("convert-format-needs-ffmpeg", format = base)
    } else {
        base
    }
}

/// Localized format name without the "(needs ffmpeg)" suffix — used on the
/// job rows' target-format chips, where the suffix would just be noise.
fn format_short_label(format: OutputFormat) -> String {
    match format {
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

/// Semantic colour of a chip/banner, resolved against the active theme so
/// it follows light/dark and the user's accent.
#[derive(Clone, Copy)]
enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

fn tone_color(theme: &cosmic::Theme, tone: Tone) -> Color {
    let cosmic = theme.cosmic();
    match tone {
        Tone::Neutral => cosmic.palette.neutral_7.into(),
        Tone::Accent => cosmic.accent_color().into(),
        Tone::Success => cosmic.success_color().into(),
        Tone::Warning => cosmic.warning_color().into(),
        Tone::Danger => cosmic.destructive_color().into(),
    }
}

/// Soft tinted background with tone-coloured text/icons (pill or rounded rect).
fn tint_class(tone: Tone, pill: bool) -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::custom(move |theme| {
        let color = tone_color(theme, tone);
        let radii = theme.cosmic().corner_radii;
        let radius = if pill {
            radii.radius_xl
        } else {
            radii.radius_m
        };
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(Color { a: 0.14, ..color })),
            text_color: Some(color),
            icon_color: Some(color),
            border: Border {
                radius: radius.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
}

/// Tinted banner surface with a thin tone-coloured outline; only icons take
/// the tone colour so multi-line text stays readable.
fn banner_class(tone: Tone) -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::custom(move |theme| {
        let color = tone_color(theme, tone);
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(Color { a: 0.10, ..color })),
            icon_color: Some(color),
            border: Border {
                radius: theme.cosmic().corner_radii.radius_m.into(),
                width: 1.0,
                color: Color { a: 0.45, ..color },
            },
            ..Default::default()
        }
    })
}

/// Caption text dimmed to the theme's secondary colour.
fn secondary_caption<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Custom(|theme| {
        cosmic::iced::widget::text::Style {
            color: Some(theme.cosmic().palette.neutral_7.into()),
            ..Default::default()
        }
    }))
}

/// Caption text in the theme's destructive colour (failure reasons).
fn danger_caption<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Custom(|theme| {
        cosmic::iced::widget::text::Style {
            color: Some(theme.cosmic().destructive_color().into()),
            ..Default::default()
        }
    }))
}

/// Small tinted pill with an optional leading icon.
fn chip<'a>(
    icon_name: Option<&'static str>,
    label: String,
    tone: Tone,
) -> cosmic::Element<'a, ConvertMessage> {
    let mut row = widget::Row::new().spacing(4).align_y(Alignment::Center);
    if let Some(name) = icon_name {
        row = row.push(widget::icon::from_name(name).size(12));
    }
    row = row.push(common::cell_caption(label));
    widget::container(row)
        .padding([2, 8])
        .class(tint_class(tone, true))
        .into()
}

/// Full-width banner: tone icon, bold title, optional body lines.
fn banner<'a>(
    icon_name: &'static str,
    title: String,
    body: Vec<cosmic::Element<'a, ConvertMessage>>,
    tone: Tone,
) -> cosmic::Element<'a, ConvertMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut text = widget::Column::new()
        .push(widget::text::heading(title))
        .spacing(2);
    for line in body {
        text = text.push(line);
    }
    widget::container(
        widget::Row::new()
            .push(widget::icon::from_name(icon_name).size(24))
            .push(text.width(Length::Fill))
            .spacing(spacing.space_s)
            .align_y(Alignment::Center),
    )
    .padding(spacing.space_s)
    .width(Length::Fill)
    .class(banner_class(tone))
    .into()
}

/// Output destination row: folder glyph, the path (clipped) and its actions.
fn destination_row<'a>(out_dir: &Path) -> cosmic::Element<'a, ConvertMessage> {
    widget::settings::item_row(vec![
        widget::icon::from_name("folder-symbolic").size(20).into(),
        common::clipped_cell(common::cell_text(out_dir.display().to_string()).into()),
        widget::button::standard(fl!("convert-dir-change"))
            .on_press(ConvertMessage::ChangeOutputDir)
            .into(),
        job_icon_button(
            "folder-open-symbolic",
            fl!("convert-dir-open"),
            ConvertMessage::OpenOutputDir,
            false,
        ),
    ])
    .into()
}

/// Settings pane: destination card, then format/rate/quality card plus a
/// hint. The quality slot's content varies with the selected format; its
/// position never does.
fn settings_pane<'a>(
    out_dir: &Path,
    format: OutputFormat,
    sample_rate: Option<u32>,
    flac_options: FlacOptions,
    lossy_options: LossyOptions,
) -> cosmic::Element<'a, ConvertMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let format_index = OutputFormat::ALL
        .iter()
        .position(|f| *f == format)
        .unwrap_or(0);
    let rate_index = SAMPLE_RATE_OPTIONS
        .iter()
        .position(|r| *r == sample_rate)
        .unwrap_or(0);

    let destination = widget::settings::section()
        .title(fl!("convert-destination-section"))
        .add(destination_row(out_dir));

    let mut output = widget::settings::section()
        .title(fl!("convert-output-section"))
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
        output = output.add(item);
    }

    widget::Column::new()
        .push(destination)
        .push(output)
        .push(
            secondary_caption(fl!("convert-settings-hint"))
                .wrapping(cosmic::iced::widget::text::Wrapping::Word),
        )
        .spacing(spacing.space_m)
        .into()
}

/// Queue summary card: title, per-state chips (only the non-zero ones),
/// primary Start action plus secondary actions that only appear when
/// applicable, and a slim overall progress bar. `Start` stays disabled while
/// the configured format needs `ffmpeg` and it isn't available.
fn summary_card<'a>(
    jobs: &'a [ConvertJob],
    format: OutputFormat,
    ffmpeg_available: Option<bool>,
) -> cosmic::Element<'a, ConvertMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let counts = JobCounts::compute(jobs);
    let format_blocked = format.requires_ffmpeg() && ffmpeg_available != Some(true);

    let mut chips = widget::Row::new()
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center);
    if counts.running > 0 {
        chips = chips.push(chip(
            Some("emblem-synchronizing-symbolic"),
            fl!("convert-summary-running", count = counts.running),
            Tone::Accent,
        ));
    }
    if counts.queued > 0 {
        chips = chips.push(chip(
            Some("document-open-recent-symbolic"),
            fl!("convert-summary-queued", count = counts.queued),
            Tone::Neutral,
        ));
    }
    if counts.done > 0 {
        chips = chips.push(chip(
            Some("object-select-symbolic"),
            fl!("convert-summary-done", count = counts.done),
            Tone::Success,
        ));
    }
    if counts.failed > 0 {
        chips = chips.push(chip(
            Some("dialog-error-symbolic"),
            fl!("convert-summary-failed", count = counts.failed),
            Tone::Danger,
        ));
    }

    let mut actions = widget::Row::new()
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center);
    if counts.done + counts.failed > 0 {
        actions = actions.push(
            widget::button::standard(fl!("convert-clear-finished"))
                .on_press(ConvertMessage::ClearFinished),
        );
    }
    if counts.queued + counts.running > 0 {
        actions = actions.push(
            widget::button::destructive(fl!("convert-cancel-all"))
                .on_press(ConvertMessage::CancelAll),
        );
    }
    actions = actions.push(
        widget::button::suggested(fl!("convert-start"))
            .leading_icon(widget::icon::from_name("media-playback-start-symbolic").handle())
            .on_press_maybe(
                (counts.queued > 0 && !format_blocked).then_some(ConvertMessage::StartQueue),
            ),
    );

    let top = widget::Row::new()
        .push(widget::text::title4(fl!("convert-queue-title")))
        .push(widget::Space::new().width(Length::Fill))
        .push(actions)
        .spacing(spacing.space_s)
        .align_y(Alignment::Center);

    widget::container(
        widget::Column::new()
            .push(top)
            .push(chips)
            .push(
                widget::progress_bar::determinate_linear(overall_progress(jobs))
                    .width(Length::Fill),
            )
            .spacing(spacing.space_s)
            .padding(spacing.space_s),
    )
    .width(Length::Fill)
    .class(cosmic::theme::Container::Card)
    .into()
}

/// Icon + tone for a job's status chip.
fn state_visual(state: &JobState) -> (&'static str, Tone) {
    match state {
        JobState::Queued => ("document-open-recent-symbolic", Tone::Neutral),
        JobState::Running => ("emblem-synchronizing-symbolic", Tone::Accent),
        JobState::Done => ("object-select-symbolic", Tone::Success),
        JobState::Failed(_) => ("dialog-error-symbolic", Tone::Danger),
        JobState::Cancelled => ("process-stop-symbolic", Tone::Warning),
    }
}

/// One job row: kind tile, filename, source → target format chips,
/// destination, failure reason (when failed), a constant-height progress
/// slot, a status chip and state-appropriate actions.
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
    let spacing = cosmic::theme::active().cosmic().spacing;
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

    let source_label = match job.kind {
        JobKind::CueSplit => "CUE".to_owned(),
        JobKind::Convert => job
            .source
            .extension()
            .and_then(|e| e.to_str())
            .map_or_else(|| "?".to_owned(), str::to_uppercase),
    };

    let mut chips = widget::Row::new()
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center)
        .push(chip(None, source_label, Tone::Neutral))
        .push(widget::icon::from_name("go-next-symbolic").size(12))
        .push(chip(None, format_short_label(format), Tone::Accent));
    if let Some(hz) = rate {
        chips = chips.push(chip(None, rate_label(Some(hz)), Tone::Neutral));
    }
    if job.kind == JobKind::CueSplit {
        chips = chips.push(chip(None, kind_label(job.kind), Tone::Neutral));
    }

    let dest_line = widget::Row::new()
        .push(widget::icon::from_name("folder-symbolic").size(12))
        .push(common::clipped_cell(
            secondary_caption(destination.display().to_string()).into(),
        ))
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center);

    let mut info = widget::Column::new()
        .push(common::cell_text(filename))
        .push(chips)
        .push(dest_line)
        .spacing(spacing.space_xxs);

    if let JobState::Failed(reason) = &job.state {
        info = info.push(widget::tooltip(
            danger_caption(fl!("convert-state-failed", error = reason.clone())),
            widget::text::caption(reason.clone()),
            widget::tooltip::Position::Top,
        ));
    }

    // Constant-height progress slot regardless of state, so a row's height
    // never jumps as the job moves through the queue.
    let progress_slot: cosmic::Element<'a, ConvertMessage> = if job.state == JobState::Running {
        widget::progress_bar::determinate_linear(job.progress_permille() as f32 / 1000.0)
            .width(Length::Fill)
            .into()
    } else {
        widget::Space::new().height(Length::Fixed(4.0)).into()
    };
    info = info.push(progress_slot);

    let kind_icon = match job.kind {
        JobKind::Convert => "audio-x-generic-symbolic",
        JobKind::CueSplit => "playlist-symbolic",
    };
    let kind_tile = widget::container(widget::icon::from_name(kind_icon).size(22))
        .padding(spacing.space_xs)
        .class(tint_class(Tone::Accent, false));

    let (state_icon, state_tone) = state_visual(&job.state);
    let mut state_text = state_label(&job.state);
    if job.state == JobState::Running {
        state_text = format!("{state_text} {}%", job.progress_permille() / 10);
    }
    let status_chip = chip(Some(state_icon), state_text, state_tone);

    let mut actions = widget::Row::new()
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center);
    match job.state {
        JobState::Queued => {
            actions = actions
                .push(job_icon_button(
                    "process-stop-symbolic",
                    fl!("convert-cancel-tooltip"),
                    ConvertMessage::CancelJob(job.id),
                    false,
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
            .push(kind_tile)
            .push(common::clipped_cell(info.into()))
            .push(status_chip)
            .push(actions)
            .spacing(spacing.space_s)
            .align_y(Alignment::Center)
            .padding(spacing.space_s),
    )
    .width(Length::Fill)
    .class(cosmic::theme::Container::Card)
    .into()
}

/// Empty queue: a drop-zone-style tile inviting the user to add files. The
/// whole tile is one big "Add Files" target (the inner pill is only a
/// visual affordance, not a nested button).
fn drop_zone<'a>(fixed_height: Option<f32>) -> cosmic::Element<'a, ConvertMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let glyph = widget::container(widget::icon::from_name("document-import-symbolic").size(40))
        .padding(spacing.space_m)
        .class(tint_class(Tone::Accent, true));

    let add_pill = widget::container(
        widget::Row::new()
            .push(widget::icon::from_name("list-add-symbolic").size(16))
            .push(widget::text::body(fl!("convert-add-files")))
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center),
    )
    .padding([spacing.space_xs, spacing.space_m])
    .class(tint_class(Tone::Accent, true));

    let content = widget::Column::new()
        .push(glyph)
        .push(widget::text::title3(fl!("no-convert-jobs")))
        .push(
            secondary_caption(fl!("convert-empty-hint"))
                .wrapping(cosmic::iced::widget::text::Wrapping::Word)
                .align_x(cosmic::iced::alignment::Horizontal::Center),
        )
        .push(add_pill)
        .push(secondary_caption(fl!("convert-dropzone-formats")))
        .spacing(spacing.space_s)
        .align_x(Alignment::Center);

    let tile = widget::container(content)
        .padding(spacing.space_l)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(cosmic::iced::alignment::Horizontal::Center)
        .align_y(cosmic::iced::alignment::Vertical::Center)
        .class(cosmic::theme::Container::custom(|theme| {
            let cosmic = theme.cosmic();
            let outline: Color = cosmic.palette.neutral_7.with_alpha(0.35).into();
            cosmic::iced::widget::container::Style {
                border: Border {
                    radius: cosmic.corner_radii.radius_m.into(),
                    width: 2.0,
                    color: outline,
                },
                ..Default::default()
            }
        }));

    widget::button::custom(tile)
        .on_press(ConvertMessage::AddFiles)
        .width(Length::Fill)
        .height(fixed_height.map_or(Length::Fill, Length::Fixed))
        .padding(0)
        .class(crate::views::card_button_class())
        .into()
}

/// Queue pane: the drop zone while empty, otherwise the summary card above
/// the job list. `scroll` wraps the list in its own scrollable (wide
/// layout); the narrow layout lets the whole page scroll instead.
fn queue_pane<'a>(
    jobs: &'a [ConvertJob],
    format: OutputFormat,
    sample_rate: Option<u32>,
    out_dir: &Path,
    ffmpeg_available: Option<bool>,
    scroll: bool,
) -> cosmic::Element<'a, ConvertMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    if jobs.is_empty() {
        return drop_zone((!scroll).then_some(280.0));
    }

    let mut list = widget::Column::new().spacing(spacing.space_xs);
    for job in jobs {
        list = list.push(job_row(job, format, sample_rate, out_dir));
    }

    let mut col = widget::Column::new()
        .spacing(spacing.space_s)
        .push(summary_card(jobs, format, ffmpeg_available));
    if scroll {
        col = col.push(
            widget::scrollable(widget::container(list).width(Length::Fill)).height(Length::Fill),
        );
    } else {
        col = col.push(list);
    }
    col.into()
}

/// Below this width the settings and queue stack in one scrolling column.
const WIDE_BREAKPOINT: f32 = 860.0;
/// Width of the settings pane in the two-pane layout.
const SETTINGS_PANE_WIDTH: f32 = 400.0;

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
    let spacing = cosmic::theme::active().cosmic().spacing;

    let header = widget::Row::new()
        .push(
            widget::Column::new()
                .push(widget::text::title3(fl!("convert")))
                .push(secondary_caption(fl!("convert-subtitle")))
                .spacing(2),
        )
        .push(widget::Space::new().width(Length::Fill))
        .push(
            widget::button::suggested(fl!("convert-add-files"))
                .leading_icon(widget::icon::from_name("list-add-symbolic").handle())
                .on_press(ConvertMessage::AddFiles),
        )
        .align_y(Alignment::Center);

    // Always-present banner slot (zero-height when empty) so the page tree
    // keeps its shape whether or not a warning is showing.
    let mut banners = widget::Column::new().spacing(spacing.space_xs);
    if format.requires_ffmpeg() && ffmpeg_available != Some(true) {
        banners = banners.push(banner(
            "dialog-warning-symbolic",
            fl!("convert-ffmpeg-missing-title"),
            vec![
                widget::text::body(fl!("convert-requires-ffmpeg")).into(),
                secondary_caption(fl!("convert-ffmpeg-missing-hint")).into(),
            ],
            Tone::Warning,
        ));
    }
    if let Some(reason) = dir_error {
        banners = banners.push(banner(
            "dialog-error-symbolic",
            fl!("convert-dir-error", reason = reason.to_owned()),
            Vec::new(),
            Tone::Danger,
        ));
    }

    let out_dir = out_dir.to_path_buf();
    let body = widget::responsive(move |size| {
        let spacing = cosmic::theme::active().cosmic().spacing;
        if size.width >= WIDE_BREAKPOINT {
            let settings = widget::scrollable(
                widget::container(settings_pane(
                    &out_dir,
                    format,
                    sample_rate,
                    flac_options,
                    lossy_options,
                ))
                .padding([0, spacing.space_s, 0, 0]),
            )
            .width(Length::Fixed(SETTINGS_PANE_WIDTH))
            .height(Length::Fill);
            widget::Row::new()
                .push(settings)
                .push(
                    widget::container(queue_pane(
                        jobs,
                        format,
                        sample_rate,
                        &out_dir,
                        ffmpeg_available,
                        true,
                    ))
                    .width(Length::Fill)
                    .height(Length::Fill),
                )
                .spacing(spacing.space_l)
                .into()
        } else {
            widget::scrollable(
                widget::Column::new()
                    .push(settings_pane(
                        &out_dir,
                        format,
                        sample_rate,
                        flac_options,
                        lossy_options,
                    ))
                    .push(queue_pane(
                        jobs,
                        format,
                        sample_rate,
                        &out_dir,
                        ffmpeg_available,
                        false,
                    ))
                    .spacing(spacing.space_m)
                    .padding([0, spacing.space_s, 0, 0]),
            )
            .height(Length::Fill)
            .into()
        }
    });

    widget::Column::new()
        .push(header)
        .push(banners)
        .push(
            widget::container(body)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .spacing(spacing.space_m)
        .padding(spacing.space_m)
        .into()
}
