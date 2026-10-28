//! The converter page: the job queue and the output settings.
//!
//! Everything the page decides lives in `aulos_core::session::ConvertState`
//! (queue, start, pause, cancel, retry) and in the persisted `Config` (format,
//! sample rate, channels, file name pattern, CUE/tag switches, output folder,
//! encoder options); this module draws them and routes
//! the user's clicks. Pickers are native dialogs run off the UI thread and
//! report back through a channel drained every frame.

use crate::app::App;
use crate::dialogs::{self, Filter};
use crate::theme::{self, ColorExt, Palette, lerp_color};
use crate::widgets::{
    PillKind, empty_state, paint_icon, paint_text, pill_button_enabled, tag_badge, toggle,
};
use aulos_core::config::Config;
use aulos_core::convert::encoder::{FlacBitDepth, FlacOptions, LossyOptions, Mp3Mode};
use aulos_core::convert::estimate::{self, Target};
use aulos_core::convert::options::{
    BITRATE_KBPS_OPTIONS, FLAC_BIT_DEPTH_OPTIONS, FLAC_COMPRESSION_OPTIONS, MP3_MODE_OPTIONS,
    SAMPLE_RATE_OPTIONS, VORBIS_QUALITY_OPTIONS, bitrate_kbps_label, flac_bit_depth_label,
    format_short_label, mp3_mode_label, rate_label,
};
use aulos_core::convert::source::khz;
use aulos_core::convert::{
    self, ChannelMode, ConvertJob, JobCounts, JobId, JobKind, JobState, OutputFormat, naming,
};
use aulos_core::session::ConvertState;
use egui::epaint::TextShape;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Id, Label, Layout, Margin, Popup, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, pos2, vec2,
};
use egui_phosphor::{fill, regular};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Height of one job row (10px padding, two text lines, 1px border).
const ROW_H: f32 = 54.0;
/// Formats shown in the segmented control, in the design's order.
const MAIN_FORMATS: [(OutputFormat, &str); 5] = [
    (OutputFormat::Flac, "FLAC"),
    (OutputFormat::Alac, "ALAC"),
    (OutputFormat::Opus, "Opus"),
    (OutputFormat::Mp3, "MP3"),
    (OutputFormat::Aac, "AAC"),
];
/// Every other format the core can write, offered in a dropdown.
const OTHER_FORMATS: [OutputFormat; 6] = [
    OutputFormat::Wav16,
    OutputFormat::Wav24,
    OutputFormat::Wav32Float,
    OutputFormat::Aiff16,
    OutputFormat::Aiff24,
    OutputFormat::OggVorbis,
];

/// What a picker brought back.
enum Picked {
    Files(Vec<PathBuf>),
    OutDir(PathBuf),
}

pub struct State {
    tx: Sender<Picked>,
    rx: Receiver<Picked>,
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx }
    }
}

/// What the user asked for while the page was drawn.
enum Intent {
    AddFiles,
    ChangeDir,
    OpenDir(PathBuf),
    Format(OutputFormat),
    Rate(Option<u32>),
    Channels(ChannelMode),
    Pattern(String),
    SplitCue(bool),
    CopyTags(bool),
    Pause,
    Resume,
    FlacCompression(u8),
    FlacDepth(FlacBitDepth),
    Mp3Mode(Mp3Mode),
    Bitrate(u32),
    VorbisQuality(f32),
    Start,
    CancelAll,
    ClearFinished,
    Cancel(JobId),
    Retry(JobId),
    Remove(JobId),
}

/// Called when the route is entered: probe for ffmpeg (a no-op once known).
pub fn enter(app: &mut App) {
    app.run(ConvertState::detect_ffmpeg());
}

pub fn show(app: &mut App, ui: &mut Ui) {
    drain_picks(app);
    let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .filter(|p| p.is_file())
            .collect()
    });
    if !dropped.is_empty() {
        let probes = app.session.convert.enqueue(dropped);
        app.run(probes);
    }
    let mut intents = Vec::new();
    page(app, ui, &mut intents);
    for intent in intents {
        apply(app, intent);
    }
}

// ---------------------------------------------------------------- page

fn page(app: &App, ui: &mut Ui, out: &mut Vec<Intent>) {
    let pal = app.pal;
    let pad = app.page_pad();
    let full = ui.available_rect_before_wrap();
    let inner = Rect::from_min_max(
        pos2(full.left() + pad, full.top() + 4.0),
        pos2(full.right() - pad, full.bottom() - 16.0),
    );
    let mut page_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut page_ui;
    ui.spacing_mut().item_spacing = vec2(10.0, 14.0);
    let config = &app.session.config.config;

    // Header.
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(aulos_core::fl!("converter-title"))
                .font(theme::semibold(26.0))
                .color(pal.text),
        );
        tag_badge(
            ui,
            &aulos_core::fl!("converter-lab-badge"),
            pal.text_1,
            Color32::TRANSPARENT,
            pal.text_5,
        );
        let dir = convert::out_dir(config).display().to_string();
        let experimental = format!("{} ", aulos_core::fl!("converter-experimental-note"));
        crate::widgets::truncated_runs(
            ui,
            &[
                (experimental.as_str(), theme::regular(14.0), pal.text_2),
                (&dir, theme::mono(12.0), pal.text_1),
            ],
        );
    });

    // Banners.
    if let Some(error) = &app.session.convert.dir_error {
        let text = aulos_core::fl!("converter-dir-error", error = error.to_string());
        if banner(
            ui,
            &pal,
            pal.danger,
            regular::WARNING_CIRCLE,
            &text,
            Some(aulos_core::fl!("converter-change-folder").as_str()),
        ) {
            out.push(Intent::ChangeDir);
        }
    }
    if config.convert_format.requires_ffmpeg() {
        match convert::ffmpeg::cached() {
            Some(true) => {}
            Some(false) => {
                let text = aulos_core::fl!(
                    "converter-ffmpeg-missing",
                    format = format_short_label(config.convert_format)
                );
                banner(ui, &pal, pal.warn, regular::WARNING, &text, None);
            }
            None => {
                banner(
                    ui,
                    &pal,
                    pal.text_2,
                    regular::INFO,
                    &aulos_core::fl!("converter-checking-ffmpeg"),
                    None,
                );
            }
        }
    }

    // Queue | Output card.
    let body = ui.available_rect_before_wrap();
    if app.compact() {
        // Phones: one pane at a time behind a Queue | Output switch.
        let id = Id::new("converter-pane");
        let mut pane: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0);
        ui.add_space(4.0);
        if crate::widgets::segmented(
            ui,
            &mut pane,
            &[
                aulos_core::fl!("converter-queue-title").as_str(),
                aulos_core::fl!("converter-pane-output").as_str(),
            ],
            34.0,
        ) {
            ui.data_mut(|d| d.insert_temp(id, pane));
        }
        ui.add_space(10.0);
        let body = ui.available_rect_before_wrap();
        let mut pane_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(body)
                .layout(Layout::top_down(Align::Min)),
        );
        if pane == 0 {
            queue(app, &mut pane_ui, out);
        } else {
            output_card(app, &mut pane_ui, out);
        }
        return;
    }
    // The card gives up width before the queue is squeezed under ~half.
    let right_w = (if app.narrow() { 300.0_f32 } else { 340.0 }).min(body.width() * 0.48);
    let left = Rect::from_min_max(body.min, pos2(body.right() - right_w - 20.0, body.bottom()));
    let right = Rect::from_min_max(pos2(left.right() + 20.0, body.top()), body.max);
    let mut queue_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(left)
            .layout(Layout::top_down(Align::Min)),
    );
    queue(app, &mut queue_ui, out);
    let mut card_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(right)
            .layout(Layout::top_down(Align::Min)),
    );
    output_card(app, &mut card_ui, out);
}

/// A tinted notice bar; returns true when its action link was clicked.
fn banner(
    ui: &mut Ui,
    pal: &Palette,
    tone: Color32,
    icon: &str,
    text: &str,
    action: Option<&str>,
) -> bool {
    let mut clicked = false;
    Frame::new()
        .fill(tone.alpha(0.08))
        .stroke(Stroke::new(1.0, tone.alpha(0.28)))
        .corner_radius(CornerRadius::same(theme::radius::MD))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.label(RichText::new(icon).font(theme::icon(15.0)).color(tone));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(label) = action
                        && ui
                            .add(
                                Label::new(RichText::new(label).color(pal.text_1).underline())
                                    .sense(Sense::click()),
                            )
                            .clicked()
                    {
                        clicked = true;
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(
                            Label::new(
                                RichText::new(text)
                                    .font(theme::regular(12.5))
                                    .color(pal.text),
                            )
                            .wrap(),
                        );
                    });
                });
            });
        });
    clicked
}

// ---------------------------------------------------------------- queue

fn queue(app: &App, ui: &mut Ui, out: &mut Vec<Intent>) {
    let pal = app.pal;
    let accent = app.accent;
    let jobs = &app.session.convert.jobs;
    let counts = JobCounts::compute(jobs);
    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);

    // Drag-over highlight.
    if ui.ctx().input(|i| !i.raw.hovered_files.is_empty()) {
        ui.painter().rect_stroke(
            ui.max_rect().expand(4.0),
            CornerRadius::same(theme::radius::XL),
            Stroke::new(1.5, accent.fill.alpha(0.7)),
            StrokeKind::Inside,
        );
    }

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(aulos_core::fl!("converter-queue-title"))
                .font(theme::semibold(15.0))
                .color(pal.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let active = counts.queued + counts.running > 0;
            if active {
                // While work is queued the header action is Pause/Resume;
                // adding more files stays one click away as an icon.
                let paused = app.session.convert.is_paused();
                if pause_pill(ui, &pal, paused).clicked() {
                    out.push(if paused {
                        Intent::Resume
                    } else {
                        Intent::Pause
                    });
                }
                if icon_button(
                    ui,
                    &pal,
                    regular::PLUS,
                    &aulos_core::fl!("convert-add-files"),
                )
                .clicked()
                {
                    out.push(Intent::AddFiles);
                }
            } else if pill_button_enabled(
                ui,
                &aulos_core::fl!("convert-add-files"),
                Some(regular::PLUS),
                PillKind::Secondary,
                28.0,
                0.0,
                true,
            )
            .clicked()
            {
                out.push(Intent::AddFiles);
            }
            if active
                && pill_button_enabled(
                    ui,
                    &aulos_core::fl!("convert-cancel-all"),
                    None,
                    PillKind::Danger,
                    28.0,
                    0.0,
                    true,
                )
                .clicked()
            {
                out.push(Intent::CancelAll);
            }
            if counts.done + counts.failed > 0
                && ghost_button(ui, &pal, &aulos_core::fl!("convert-clear-finished")).clicked()
            {
                out.push(Intent::ClearFinished);
            }
            let mut summary =
                aulos_core::fl!("converter-summary", jobs = jobs.len(), done = counts.done);
            if counts.failed > 0 {
                summary.push_str(" · ");
                summary.push_str(&aulos_core::fl!(
                    "converter-summary-failed",
                    failed = counts.failed
                ));
            }
            ui.add(Label::new(RichText::new(summary).color(pal.text_2)).truncate());
        });
    });

    if jobs.is_empty() {
        if empty_state(
            ui,
            regular::ARROWS_LEFT_RIGHT,
            &aulos_core::fl!("converter-empty-title"),
            &aulos_core::fl!("converter-empty-body"),
            Some(aulos_core::fl!("convert-add-files").as_str()),
        ) {
            out.push(Intent::AddFiles);
        }
        return;
    }

    if counts.running > 0 {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 3.0), Sense::hover());
        paint_bar(
            ui,
            rect,
            convert::overall_progress(jobs),
            accent.fill,
            pal.track,
        );
        // Progress is read from atomics the jobs update; keep the bars moving.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }

    ScrollArea::vertical()
        .id_salt("convert-queue")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, jobs.len(), |ui, range| {
            for job in &jobs[range] {
                job_row(ui, &pal, &app.session.config.config, job, out);
            }
        });
}

/// A text-only 28px button with a 6px radius (the design's "Clear finished").
fn ghost_button(ui: &mut Ui, pal: &Palette, label: &str) -> egui::Response {
    let font = theme::medium(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, pal.text_1);
    let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 20.0, 28.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(theme::radius::SM),
            pal.text.alpha(0.07),
        );
    }
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, pal.text_1);
    resp
}

/// The design's 28px "Pause" pill (filled pause glyph); reads "Resume" with a
/// play glyph while the queue is paused.
fn pause_pill(ui: &mut Ui, pal: &Palette, paused: bool) -> egui::Response {
    let (label, glyph) = if paused {
        (aulos_core::fl!("converter-resume"), fill::PLAY)
    } else {
        (aulos_core::fl!("pause"), fill::PAUSE)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label, theme::semibold(13.0), pal.text);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(galley.size().x + 12.0 + 6.0 + 20.0, 28.0),
        Sense::click(),
    );
    let bg = if resp.hovered() {
        pal.stroke_strong
    } else {
        pal.surface_hi
    };
    ui.painter().rect_filled(rect, CornerRadius::same(14), bg);
    paint_icon(
        ui.painter(),
        pos2(rect.left() + 10.0 + 6.0, rect.center().y),
        glyph,
        12.0,
        pal.text,
        true,
    );
    ui.painter().galley(
        pos2(
            rect.left() + 10.0 + 12.0 + 6.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        pal.text,
    );
    resp.on_hover_text(if paused {
        aulos_core::fl!("converter-resume-tooltip")
    } else {
        aulos_core::fl!("converter-pause-tooltip")
    })
}

/// A 28px square ghost button with just a glyph.
fn icon_button(ui: &mut Ui, pal: &Palette, glyph: &str, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(theme::radius::SM),
            pal.text.alpha(0.07),
        );
    }
    paint_icon(ui.painter(), rect.center(), glyph, 16.0, pal.text_1, false);
    resp.on_hover_text(tip)
}

fn paint_bar(ui: &Ui, rect: Rect, frac: f32, fill: Color32, track: Color32) {
    let r = CornerRadius::same((rect.height() / 2.0).ceil() as u8);
    ui.painter().rect_filled(rect, r, track);
    let w = rect.width() * frac.clamp(0.0, 1.0);
    if w > 0.5 {
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, vec2(w, rect.height())),
            r,
            fill,
        );
    }
}

/// The "FLAC 24/96 → Opus 160" tag of a format with its quality option.
fn format_tag(format: OutputFormat, flac: FlacOptions, lossy: LossyOptions) -> String {
    match format {
        OutputFormat::Opus => format!("Opus {}", lossy.opus_bitrate_kbps),
        OutputFormat::Aac => format!("AAC {}", lossy.aac_bitrate_kbps),
        OutputFormat::Mp3 => format!("MP3 {}", mp3_mode_label(lossy.mp3_mode)),
        OutputFormat::OggVorbis => format!("Vorbis q{}", quality_text(lossy.vorbis_quality)),
        OutputFormat::Flac => match flac.bit_depth {
            FlacBitDepth::Auto => "FLAC".to_owned(),
            depth => format!("FLAC {}", flac_bit_depth_label(depth)),
        },
        other => format_short_label(other),
    }
}

fn quality_text(q: f32) -> String {
    if q.fract() == 0.0 {
        format!("{}", q as i32)
    } else {
        format!("{q}")
    }
}

fn extension_upper(path: &Path) -> String {
    path.extension().and_then(OsStr::to_str).map_or_else(
        || aulos_core::fl!("converter-file-fallback"),
        str::to_uppercase,
    )
}

fn job_row(ui: &mut Ui, pal: &Palette, config: &Config, job: &ConvertJob, out: &mut Vec<Intent>) {
    let fail_ink = if pal.dark {
        lerp_color(pal.danger, Color32::WHITE, 0.37)
    } else {
        pal.danger
    };
    let (icon, icon_fill, icon_color, status_color, bar_fill, action) = match &job.state {
        JobState::Done => (
            fill::CHECK_CIRCLE,
            true,
            pal.ok,
            pal.text_2,
            pal.ok,
            regular::FOLDER_OPEN,
        ),
        JobState::Running if job.is_paused() => (
            regular::PAUSE_CIRCLE,
            false,
            pal.text_2,
            pal.text_2,
            pal.text_3,
            regular::X,
        ),
        JobState::Running => (
            regular::CIRCLE_NOTCH,
            false,
            pal.text_1,
            pal.text_1,
            pal.text_1,
            regular::X,
        ),
        JobState::Queued => (
            regular::CLOCK,
            false,
            pal.text_4,
            pal.text_2,
            Color32::TRANSPARENT,
            regular::X,
        ),
        JobState::Failed(_) => (
            fill::WARNING_CIRCLE,
            true,
            fail_ink,
            fail_ink,
            pal.danger,
            regular::ARROW_CLOCKWISE,
        ),
        JobState::Cancelled => (
            regular::PROHIBIT,
            false,
            pal.text_3,
            pal.text_3,
            Color32::TRANSPARENT,
            regular::ARROW_CLOCKWISE,
        ),
    };
    let (bg, border) = match &job.state {
        JobState::Running => (pal.surface_1, pal.stroke),
        JobState::Failed(_) => (Color32::TRANSPARENT, pal.danger.alpha(0.3)),
        _ => (Color32::TRANSPARENT, Color32::TRANSPARENT),
    };
    let frac = match &job.state {
        JobState::Running => job.progress_permille() as f32 / 1000.0,
        JobState::Done | JobState::Failed(_) => 1.0,
        JobState::Queued | JobState::Cancelled => 0.0,
    };
    let status = match &job.state {
        JobState::Running => running_status(job),
        JobState::Done => job.output_bytes.map_or_else(
            || aulos_core::fl!("convert-state-done"),
            |bytes| aulos_core::fl!("converter-done-size", size = estimate::format_bytes(bytes)),
        ),
        JobState::Queued => aulos_core::fl!("convert-state-queued"),
        JobState::Failed(reason) => reason.clone(),
        JobState::Cancelled => aulos_core::fl!("convert-state-cancelled"),
    };

    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(theme::radius::MD), bg);
    painter.rect_stroke(
        rect,
        CornerRadius::same(theme::radius::MD),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(11.0);
    let icon_c = pos2(inner.left() + 14.0, rect.center().y);
    if matches!(job.state, JobState::Running) && !job.is_paused() {
        let time = ui.input(|i| i.time) as f32;
        let galley = painter.layout_no_wrap(icon.to_owned(), theme::icon(17.0), icon_color);
        let pos = icon_c - galley.size() / 2.0;
        painter.add(
            TextShape::new(pos, galley, icon_color)
                .with_angle_and_anchor(time * 6.0, Align2::CENTER_CENTER),
        );
    } else {
        paint_icon(painter, icon_c, icon, 17.0, icon_color, icon_fill);
    }

    let action_rect = Rect::from_center_size(
        pos2(inner.right() - 14.0, rect.center().y),
        vec2(28.0, 28.0),
    );
    let status_left = (action_rect.left() - 12.0 - 150.0).max(inner.left() + 100.0);
    let name_left = inner.left() + 28.0 + 12.0;
    let name_right = (status_left - 12.0).max(name_left + 40.0);

    let name = job.source.file_name().map_or_else(
        || job.source.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let settings = job.settings.as_ref();
    let format = settings.map_or(config.convert_format, |s| s.format);
    let flac = settings.map_or(config.flac_options, |s| s.flac_options);
    let lossy = settings.map_or(config.lossy_options, |s| s.lossy_options);
    let rate = settings.map_or(config.convert_sample_rate, |s| s.target_rate);
    let channels = settings.map_or(config.convert_channels, |s| s.channels);
    let split_cue = settings.map_or(config.convert_split_cue, |s| s.split_cue);
    let target = format_tag(format, flac, lossy);
    let mut meta = match job.kind {
        JobKind::Convert => aulos_core::fl!(
            "converter-meta-convert",
            source = source_label(job),
            target = target.as_str()
        ),
        JobKind::CueSplit if !split_cue => {
            aulos_core::fl!("converter-meta-cue-one-file", target = target.as_str())
        }
        JobKind::CueSplit => {
            let total = job.cue_total.load(Ordering::Relaxed);
            let current = job.cue_track.load(Ordering::Relaxed);
            let known = job.source_info.as_ref().and_then(|i| i.cue_tracks);
            match (
                matches!(job.state, JobState::Running),
                total.max(known.unwrap_or(0)),
            ) {
                (true, total) if total > 0 && current > 0 => {
                    aulos_core::fl!(
                        "converter-meta-cue-track",
                        current = current,
                        total = total,
                        target = target.as_str()
                    )
                }
                (_, total) if total > 0 => aulos_core::fl!(
                    "converter-meta-cue-tracks",
                    total = total,
                    target = target.as_str()
                ),
                _ => aulos_core::fl!("converter-meta-cue-split", target = target.as_str()),
            }
        }
    };
    if let Some(hz) = rate {
        meta.push_str(" · ");
        meta.push_str(&aulos_core::fl!("converter-meta-khz", khz = khz(hz)));
    }
    match channels {
        ChannelMode::Source => {}
        ChannelMode::Mono => {
            meta.push_str(" · ");
            meta.push_str(&aulos_core::fl!("converter-meta-mono"));
        }
        ChannelMode::Stereo => {
            meta.push_str(" · ");
            meta.push_str(&aulos_core::fl!("converter-meta-stereo"));
        }
    }
    let cy = rect.center().y;
    paint_text(
        painter,
        Rect::from_min_max(pos2(name_left, cy - 17.0), pos2(name_right, cy)),
        Align::Min,
        &name,
        theme::semibold(13.0),
        pal.text,
    );
    paint_text(
        painter,
        Rect::from_min_max(pos2(name_left, cy + 1.0), pos2(name_right, cy + 16.0)),
        Align::Min,
        &meta,
        theme::mono(11.0),
        pal.text_2,
    );

    let top = cy - 11.5;
    paint_text(
        painter,
        Rect::from_min_max(
            pos2(status_left, top),
            pos2(status_left + 150.0, top + 15.0),
        ),
        Align::Min,
        &status,
        theme::mono(11.5),
        status_color,
    );
    let bar = Rect::from_min_size(pos2(status_left, top + 20.0), vec2(150.0, 3.0));
    paint_bar(ui, bar, frac, bar_fill, pal.track);

    let action_resp = ui.interact(
        action_rect,
        Id::new(("convert-job-action", job.id)),
        Sense::click(),
    );
    if action_resp.hovered() {
        ui.painter().rect_filled(
            action_rect,
            CornerRadius::same(theme::radius::SM),
            pal.text.alpha(0.07),
        );
    }
    paint_icon(
        ui.painter(),
        action_rect.center(),
        action,
        15.0,
        pal.text_2,
        false,
    );
    let job_dir = || settings.map_or_else(|| convert::out_dir(config), |s| s.out_dir.clone());
    let (tip, intent) = match &job.state {
        JobState::Done => (
            aulos_core::fl!("converter-show-in-folder"),
            Intent::OpenDir(job_dir()),
        ),
        JobState::Running => (
            aulos_core::fl!("convert-cancel-tooltip"),
            Intent::Cancel(job.id),
        ),
        JobState::Queued => (
            aulos_core::fl!("converter-remove-from-queue"),
            Intent::Remove(job.id),
        ),
        JobState::Failed(_) | JobState::Cancelled => (
            aulos_core::fl!("convert-retry-tooltip"),
            Intent::Retry(job.id),
        ),
    };
    if action_resp.on_hover_text(tip).clicked() {
        out.push(intent);
    }

    resp.context_menu(|ui| {
        if matches!(job.state, JobState::Done)
            && ui
                .button(aulos_core::fl!("converter-show-in-folder"))
                .clicked()
        {
            out.push(Intent::OpenDir(job_dir()));
        }
        if matches!(job.state, JobState::Running)
            && ui
                .button(aulos_core::fl!("convert-cancel-tooltip"))
                .clicked()
        {
            out.push(Intent::Cancel(job.id));
        }
        if matches!(job.state, JobState::Failed(_) | JobState::Cancelled)
            && ui
                .button(aulos_core::fl!("convert-retry-tooltip"))
                .clicked()
        {
            out.push(Intent::Retry(job.id));
        }
        if !matches!(job.state, JobState::Running)
            && ui
                .button(aulos_core::fl!("converter-remove-from-list"))
                .clicked()
        {
            out.push(Intent::Remove(job.id));
        }
    });
}

/// The source's `FLAC 24/96` label, or its upper-cased extension until the
/// probe has reported (or when it failed).
fn source_label(job: &ConvertJob) -> String {
    job.source_info
        .as_ref()
        .map_or_else(|| extension_upper(&job.source), |info| info.label())
}

/// `62% · 00:41 left`, `18% · 3/14` for a CUE rip, `Paused · 62%` while held.
fn running_status(job: &ConvertJob) -> String {
    let percent = job.progress_permille() / 10;
    if job.is_paused() {
        return aulos_core::fl!("converter-paused-percent", percent = percent);
    }
    let total = job.cue_total.load(Ordering::Relaxed);
    if job.kind == JobKind::CueSplit && total > 0 {
        let current = job.cue_track.load(Ordering::Relaxed).max(1);
        return aulos_core::fl!(
            "converter-progress-cue",
            percent = percent,
            current = current,
            total = total
        );
    }
    match job.eta() {
        Some(eta) => aulos_core::fl!(
            "converter-progress-eta",
            percent = percent,
            time = estimate::format_clock(eta)
        ),
        None => format!("{percent}%"),
    }
}

// ---------------------------------------------------------------- output card

fn output_card(app: &App, ui: &mut Ui, out: &mut Vec<Intent>) {
    let pal = app.pal;
    let config = &app.session.config.config;
    let rect = ui.max_rect();
    ui.painter()
        .rect_filled(rect, CornerRadius::same(theme::radius::XL), pal.surface_1);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::radius::XL),
        Stroke::new(1.0, pal.stroke),
        StrokeKind::Inside,
    );
    let inner = rect.shrink(16.0);
    let footer = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 34.0), inner.max);
    let content = Rect::from_min_max(inner.min, pos2(inner.right(), footer.top() - 14.0));

    let mut content_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(content)
            .layout(Layout::top_down(Align::Min)),
    );
    ScrollArea::vertical()
        .id_salt("convert-output")
        .auto_shrink([false, false])
        .show(&mut content_ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 14.0);
            ui.label(
                RichText::new(aulos_core::fl!("convert-output-section"))
                    .font(theme::semibold(15.0))
                    .color(pal.text),
            );
            format_section(ui, &pal, config.convert_format, out);
            settings_grid(ui, &pal, config, out);
            pattern_section(ui, &pal, &config.convert_name_pattern, out);
            let cue = cue_summary(&app.session.convert.jobs);
            switch_row(
                ui,
                &pal,
                &aulos_core::fl!("converter-split-cue"),
                &cue,
                config.convert_split_cue,
                Intent::SplitCue,
                out,
            );
            switch_row(
                ui,
                &pal,
                &aulos_core::fl!("converter-copy-tags"),
                &aulos_core::fl!("converter-copy-tags-hint"),
                config.convert_copy_tags,
                Intent::CopyTags,
                out,
            );
            folder_section(ui, &pal, config, out);
        });

    let counts = JobCounts::compute(&app.session.convert.jobs);
    let ffmpeg_blocked =
        config.convert_format.requires_ffmpeg() && convert::ffmpeg::cached() != Some(true);
    let ready = counts.queued > 0 && !ffmpeg_blocked;
    let summary = if counts.queued == 0 {
        aulos_core::fl!("converter-nothing-queued")
    } else {
        let files = aulos_core::fl!("converter-queued-files", count = counts.queued);
        let target = Target {
            format: config.convert_format,
            target_rate: config.convert_sample_rate,
            channels: config.convert_channels,
            flac: config.flac_options,
            lossy: config.lossy_options,
        };
        let queued = app
            .session
            .convert
            .jobs
            .iter()
            .filter(|j| j.state == JobState::Queued)
            .filter_map(|j| j.source_info.as_ref());
        match estimate::queue_estimate(queued, target) {
            Some(est) => aulos_core::fl!(
                "converter-queue-estimate",
                files = files.as_str(),
                from = estimate::format_bytes(est.source_bytes),
                to = estimate::format_bytes(est.output_bytes)
            ),
            None => aulos_core::fl!(
                "converter-queue-target",
                files = files.as_str(),
                target = format_tag(
                    config.convert_format,
                    config.flac_options,
                    config.lossy_options
                )
            ),
        }
    };
    let mut footer_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(footer)
            .layout(Layout::right_to_left(Align::Center)),
    );
    if pill_button_enabled(
        &mut footer_ui,
        &aulos_core::fl!("convert"),
        Some(regular::ARROWS_LEFT_RIGHT),
        PillKind::Primary,
        34.0,
        0.0,
        ready,
    )
    .clicked()
    {
        out.push(Intent::Start);
    }
    footer_ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.add(
            Label::new(
                RichText::new(summary)
                    .font(theme::regular(12.0))
                    .color(pal.text_2),
            )
            .truncate(),
        );
    });
}

fn field_label(ui: &mut Ui, pal: &Palette, text: &str, opacity: f32) {
    ui.label(
        RichText::new(text)
            .font(theme::regular(12.0))
            .color(pal.text_2.gamma_multiply(opacity)),
    );
}

fn format_section(ui: &mut Ui, pal: &Palette, current: OutputFormat, out: &mut Vec<Intent>) {
    ui.spacing_mut().item_spacing.y = 6.0;
    field_label(ui, pal, &aulos_core::fl!("convert-format"), 1.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(theme::radius::MD), pal.bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::radius::MD),
        Stroke::new(1.0, pal.stroke),
        StrokeKind::Inside,
    );
    let cells = rect.shrink(4.0);
    let n = MAIN_FORMATS.len() as f32;
    let cell_w = (cells.width() - 2.0 * (n - 1.0)) / n;
    for (i, (format, label)) in MAIN_FORMATS.iter().enumerate() {
        let cell = Rect::from_min_size(
            pos2(cells.left() + i as f32 * (cell_w + 2.0), cells.top()),
            vec2(cell_w, 26.0),
        );
        let resp = ui.interact(cell, Id::new(("convert-format", i)), Sense::click());
        let selected = *format == current;
        let r = CornerRadius::same(5);
        if selected {
            ui.painter().rect_filled(cell, r, pal.text);
        } else if resp.hovered() {
            ui.painter().rect_filled(cell, r, pal.hover);
        }
        let (font, fg) = if selected {
            (theme::semibold(13.0), pal.bg)
        } else {
            (theme::medium(13.0), pal.text_1)
        };
        paint_text(ui.painter(), cell, Align::Center, label, font, fg);
        if resp.clicked() {
            out.push(Intent::Format(*format));
        }
    }

    let other = OTHER_FORMATS.iter().position(|f| *f == current);
    let value = other.map_or_else(
        || aulos_core::fl!("converter-more-formats"),
        |i| format_short_label(OTHER_FORMATS[i]),
    );
    let dd = Dropdown {
        salt: "convert-more-formats",
        value: &value,
        count: OTHER_FORMATS.len(),
        selected: other,
        enabled: true,
    };
    if let Some(i) = dropdown(ui, pal, &dd, |i| format_short_label(OTHER_FORMATS[i])) {
        out.push(Intent::Format(OTHER_FORMATS[i]));
    }
}

fn settings_grid(ui: &mut Ui, pal: &Palette, config: &Config, out: &mut Vec<Intent>) {
    let format = config.convert_format;
    let flac = config.flac_options;
    let lossy = config.lossy_options;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.columns(2, |cols| {
            quality_cell(&mut cols[0], pal, format, flac, lossy, out);
            let ui = &mut cols[1];
            ui.spacing_mut().item_spacing.y = 6.0;
            field_label(ui, pal, &aulos_core::fl!("convert-sample-rate"), 1.0);
            let selected = SAMPLE_RATE_OPTIONS
                .iter()
                .position(|r| *r == config.convert_sample_rate);
            let value = rate_label(config.convert_sample_rate);
            let dd = Dropdown {
                salt: "convert-rate",
                value: &value,
                count: SAMPLE_RATE_OPTIONS.len(),
                selected,
                enabled: true,
            };
            if let Some(i) = dropdown(ui, pal, &dd, |i| rate_label(SAMPLE_RATE_OPTIONS[i])) {
                out.push(Intent::Rate(SAMPLE_RATE_OPTIONS[i]));
            }
        });
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            ui.spacing_mut().item_spacing.y = 6.0;
            let enabled = format == OutputFormat::Flac;
            let opacity = if enabled { 1.0 } else { 0.38 };
            field_label(ui, pal, &aulos_core::fl!("convert-flac-bitdepth"), opacity);
            let value = if enabled {
                flac_bit_depth_label(flac.bit_depth)
            } else {
                aulos_core::fl!("converter-depth-na", format = format_short_label(format))
            };
            let selected = FLAC_BIT_DEPTH_OPTIONS
                .iter()
                .position(|d| *d == flac.bit_depth);
            let dd = Dropdown {
                salt: "convert-depth",
                value: &value,
                count: FLAC_BIT_DEPTH_OPTIONS.len(),
                selected,
                enabled,
            };
            if let Some(i) = dropdown(ui, pal, &dd, |i| {
                flac_bit_depth_label(FLAC_BIT_DEPTH_OPTIONS[i])
            }) {
                out.push(Intent::FlacDepth(FLAC_BIT_DEPTH_OPTIONS[i]));
            }

            let ui = &mut cols[1];
            ui.spacing_mut().item_spacing.y = 6.0;
            field_label(ui, pal, &aulos_core::fl!("converter-channels"), 1.0);
            let selected = ChannelMode::ALL
                .iter()
                .position(|c| *c == config.convert_channels);
            let channels_value = channels_label(config.convert_channels);
            let dd = Dropdown {
                salt: "convert-channels",
                value: &channels_value,
                count: ChannelMode::ALL.len(),
                selected,
                enabled: true,
            };
            if let Some(i) = dropdown(ui, pal, &dd, |i| channels_label(ChannelMode::ALL[i])) {
                out.push(Intent::Channels(ChannelMode::ALL[i]));
            }
        });
    });
}

/// The format-specific quality control: FLAC compression, MP3 mode, AAC/Opus
/// bitrate, Vorbis quality; lossless PCM-style formats have none.
fn quality_cell(
    ui: &mut Ui,
    pal: &Palette,
    format: OutputFormat,
    flac: FlacOptions,
    lossy: LossyOptions,
    out: &mut Vec<Intent>,
) {
    ui.spacing_mut().item_spacing.y = 6.0;
    match format {
        OutputFormat::Flac => {
            field_label(ui, pal, &aulos_core::fl!("converter-compression"), 1.0);
            let selected = FLAC_COMPRESSION_OPTIONS
                .iter()
                .position(|l| *l == flac.compression_level);
            let value = aulos_core::fl!(
                "converter-compression-level",
                level = flac.compression_level
            );
            let dd = Dropdown {
                salt: "convert-quality",
                value: &value,
                count: FLAC_COMPRESSION_OPTIONS.len(),
                selected,
                enabled: true,
            };
            if let Some(i) = dropdown(ui, pal, &dd, |i| {
                aulos_core::fl!(
                    "converter-compression-level",
                    level = FLAC_COMPRESSION_OPTIONS[i]
                )
            }) {
                out.push(Intent::FlacCompression(FLAC_COMPRESSION_OPTIONS[i]));
            }
        }
        OutputFormat::Mp3 => {
            field_label(ui, pal, &aulos_core::fl!("convert-mp3-mode"), 1.0);
            let selected = MP3_MODE_OPTIONS.iter().position(|m| *m == lossy.mp3_mode);
            let value = mp3_mode_label(lossy.mp3_mode);
            let dd = Dropdown {
                salt: "convert-quality",
                value: &value,
                count: MP3_MODE_OPTIONS.len(),
                selected,
                enabled: true,
            };
            if let Some(i) = dropdown(ui, pal, &dd, |i| mp3_mode_label(MP3_MODE_OPTIONS[i])) {
                out.push(Intent::Mp3Mode(MP3_MODE_OPTIONS[i]));
            }
        }
        OutputFormat::Aac | OutputFormat::Opus => {
            field_label(ui, pal, &aulos_core::fl!("convert-bitrate"), 1.0);
            let current = if format == OutputFormat::Aac {
                lossy.aac_bitrate_kbps
            } else {
                lossy.opus_bitrate_kbps
            };
            let selected = BITRATE_KBPS_OPTIONS.iter().position(|b| *b == current);
            let value = bitrate_kbps_label(current);
            let dd = Dropdown {
                salt: "convert-quality",
                value: &value,
                count: BITRATE_KBPS_OPTIONS.len(),
                selected,
                enabled: true,
            };
            if let Some(i) = dropdown(ui, pal, &dd, |i| {
                bitrate_kbps_label(BITRATE_KBPS_OPTIONS[i])
            }) {
                out.push(Intent::Bitrate(BITRATE_KBPS_OPTIONS[i]));
            }
        }
        OutputFormat::OggVorbis => {
            field_label(ui, pal, &aulos_core::fl!("convert-vorbis-quality"), 1.0);
            let selected = VORBIS_QUALITY_OPTIONS
                .iter()
                .position(|q| (*q - lossy.vorbis_quality).abs() < 0.01);
            let value = aulos_core::fl!(
                "converter-quality-value",
                q = quality_text(lossy.vorbis_quality)
            );
            let dd = Dropdown {
                salt: "convert-quality",
                value: &value,
                count: VORBIS_QUALITY_OPTIONS.len(),
                selected,
                enabled: true,
            };
            if let Some(i) = dropdown(ui, pal, &dd, |i| {
                aulos_core::fl!(
                    "converter-quality-value",
                    q = quality_text(VORBIS_QUALITY_OPTIONS[i])
                )
            }) {
                out.push(Intent::VorbisQuality(VORBIS_QUALITY_OPTIONS[i]));
            }
        }
        OutputFormat::Wav16
        | OutputFormat::Wav24
        | OutputFormat::Wav32Float
        | OutputFormat::Aiff16
        | OutputFormat::Aiff24
        | OutputFormat::Alac => {
            field_label(ui, pal, &aulos_core::fl!("convert-mp3-mode"), 0.38);
            let lossless = aulos_core::fl!("converter-lossless");
            let dd = Dropdown {
                salt: "convert-quality",
                value: &lossless,
                count: 0,
                selected: None,
                enabled: false,
            };
            dropdown(ui, pal, &dd, |_| String::new());
        }
    }
}

fn channels_label(mode: ChannelMode) -> String {
    match mode {
        ChannelMode::Source => aulos_core::fl!("converter-channels-keep"),
        ChannelMode::Mono => aulos_core::fl!("converter-channels-mono"),
        ChannelMode::Stereo => aulos_core::fl!("converter-channels-stereo"),
    }
}

/// The file name pattern field (committed on Enter / focus loss) with a
/// token reminder.
fn pattern_section(ui: &mut Ui, pal: &Palette, pattern: &str, out: &mut Vec<Intent>) {
    ui.spacing_mut().item_spacing.y = 6.0;
    field_label(ui, pal, &aulos_core::fl!("converter-pattern-label"), 1.0);
    let id = Id::new("convert-pattern");
    let focused = ui.memory(|m| m.has_focus(id));
    // While editing, the draft lives in egui's temp storage; otherwise the
    // field mirrors the saved setting.
    let mut draft = if focused {
        ui.data(|d| d.get_temp::<String>(id))
            .unwrap_or_else(|| pattern.to_owned())
    } else {
        pattern.to_owned()
    };
    let resp = ui.add(
        TextEdit::singleline(&mut draft)
            .id(id)
            .hint_text(RichText::new(naming::DEFAULT_PATTERN).color(pal.text_4))
            .desired_width(ui.available_width())
            .margin(vec2(10.0, 7.0))
            .font(theme::mono(11.5))
            .background_color(pal.bg),
    );
    crate::widgets::reveal_focused(ui, &resp);
    ui.painter().rect_stroke(
        resp.rect,
        CornerRadius::same(theme::radius::SM),
        Stroke::new(
            1.0,
            if resp.has_focus() {
                pal.text
            } else {
                pal.input_bd
            },
        ),
        StrokeKind::Inside,
    );
    if resp.has_focus() || resp.changed() {
        ui.data_mut(|d| d.insert_temp(id, draft.clone()));
    }
    if resp.lost_focus() && draft != pattern {
        out.push(Intent::Pattern(draft));
    }
    ui.label(
        RichText::new(aulos_core::fl!(
            "converter-pattern-hint",
            tokens = "{artist} {album_artist} {album} {title} {track:02} {disc} {year} {genre} {filename}"
        ))
            .font(theme::regular(11.0))
            .color(pal.text_3),
    );
}

/// "Live at Harbour Hall.cue · 14 tracks found" for the first CUE sheet in
/// the queue, or a hint when there is none.
fn cue_summary(jobs: &[ConvertJob]) -> String {
    let Some(job) = jobs
        .iter()
        .find(|j| j.kind == JobKind::CueSplit && j.state.is_active())
    else {
        return aulos_core::fl!("converter-cue-hint");
    };
    let name = job
        .source
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    match job.source_info.as_ref().and_then(|i| i.cue_tracks) {
        Some(n) => aulos_core::fl!("converter-cue-found", name = name.as_str(), count = n),
        None => name,
    }
}

/// A title + description on the left and an on/off switch on the right.
fn switch_row(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    desc: &str,
    value: bool,
    intent: impl FnOnce(bool) -> Intent,
    out: &mut Vec<Intent>,
) {
    let mut on = value;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let switch_w = 34.0;
        ui.allocate_ui_with_layout(
            vec2((ui.available_width() - switch_w - 10.0).max(40.0), 36.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(
                    Label::new(
                        RichText::new(title)
                            .font(theme::semibold(13.0))
                            .color(pal.text),
                    )
                    .truncate(),
                );
                ui.add(
                    Label::new(
                        RichText::new(desc)
                            .font(theme::regular(12.0))
                            .color(pal.text_2),
                    )
                    .truncate(),
                );
            },
        );
        if toggle(ui, &mut on).changed() {
            out.push(intent(on));
        }
    });
}

fn folder_section(ui: &mut Ui, pal: &Palette, config: &Config, out: &mut Vec<Intent>) {
    ui.spacing_mut().item_spacing.y = 6.0;
    field_label(ui, pal, &aulos_core::fl!("converter-output-folder"), 1.0);
    let dir = convert::out_dir(config);
    let width = ui.available_width();
    let mut job = LayoutJob::single_section(
        dir.display().to_string(),
        TextFormat {
            font_id: theme::mono(11.5),
            color: pal.text,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: width - 20.0,
        break_anywhere: true,
        ..Default::default()
    };
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(
        vec2(width, (galley.size().y + 12.0).max(32.0)),
        Sense::hover(),
    );
    ui.painter()
        .rect_filled(rect, CornerRadius::same(theme::radius::SM), pal.bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::radius::SM),
        Stroke::new(1.0, pal.input_bd),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0),
        galley,
        pal.text,
    );
    ui.horizontal(|ui| {
        if pill_button_enabled(
            ui,
            &aulos_core::fl!("convert-dir-change"),
            Some(regular::PENCIL_SIMPLE),
            PillKind::Secondary,
            28.0,
            0.0,
            true,
        )
        .clicked()
        {
            out.push(Intent::ChangeDir);
        }
        if pill_button_enabled(
            ui,
            &aulos_core::fl!("converter-open"),
            Some(regular::FOLDER_OPEN),
            PillKind::Ghost,
            28.0,
            0.0,
            true,
        )
        .clicked()
        {
            out.push(Intent::OpenDir(dir.clone()));
        }
    });
}

/// A dropdown field: what it shows and which option is current.
struct Dropdown<'a> {
    salt: &'a str,
    value: &'a str,
    count: usize,
    selected: Option<usize>,
    enabled: bool,
}

/// A 32px dropdown field showing `dd.value`; `label_of(i)` names option `i`.
/// Returns the option picked this frame.
fn dropdown(
    ui: &mut Ui,
    pal: &Palette,
    dd: &Dropdown<'_>,
    label_of: impl Fn(usize) -> String,
) -> Option<usize> {
    let Dropdown {
        salt,
        value,
        count,
        selected,
        enabled,
    } = *dd;
    let opacity = if enabled { 1.0 } else { 0.38 };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    let resp = ui.interact(
        rect,
        Id::new(salt),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let radius = CornerRadius::same(theme::radius::SM);
    ui.painter().rect_filled(rect, radius, pal.bg);
    ui.painter().rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, pal.input_bd.gamma_multiply(opacity)),
        StrokeKind::Inside,
    );
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 10.0, rect.top()),
        pos2(rect.right() - 28.0, rect.bottom()),
    );
    paint_text(
        ui.painter(),
        text_rect,
        Align::Min,
        value,
        theme::regular(13.0),
        pal.text.gamma_multiply(opacity),
    );
    if count > 0 {
        paint_icon(
            ui.painter(),
            pos2(rect.right() - 16.0, rect.center().y),
            regular::CARET_DOWN,
            11.0,
            pal.text_3,
            false,
        );
    }
    if !enabled || count == 0 {
        return None;
    }
    let mut picked = None;
    Popup::menu(&resp).width(rect.width()).show(|ui| {
        for i in 0..count {
            if ui
                .selectable_label(selected == Some(i), label_of(i))
                .clicked()
            {
                picked = Some(i);
            }
        }
    });
    picked
}

// ---------------------------------------------------------------- intents

fn apply(app: &mut App, intent: Intent) {
    match intent {
        Intent::AddFiles => {
            let tx = app.views.converter.tx.clone();
            let ctx = app.wake.clone();
            app.handle.spawn(async move {
                let files = pick_files().await;
                // Cancelled: nothing to add.
                if !files.is_empty() {
                    let _ = tx.send(Picked::Files(files));
                    ctx.request_repaint();
                }
            });
        }
        Intent::ChangeDir => {
            let tx = app.views.converter.tx.clone();
            let ctx = app.wake.clone();
            app.handle.spawn(async move {
                if let Some(dir) = pick_folder().await {
                    let _ = tx.send(Picked::OutDir(dir));
                    ctx.request_repaint();
                }
            });
        }
        Intent::OpenDir(dir) => {
            if let Err(error) = aulos_core::desktop::browser::open_path(&dir) {
                app.toast_error(error);
            }
        }
        Intent::Format(format) => set_config(app, |c| c.convert_format = format),
        Intent::Rate(rate) => set_config(app, |c| c.convert_sample_rate = rate),
        Intent::Channels(mode) => set_config(app, |c| c.convert_channels = mode),
        Intent::Pattern(pattern) => {
            // An emptied field goes back to the default instead of naming
            // every file after its source.
            let pattern = pattern.trim();
            let pattern = if pattern.is_empty() {
                naming::DEFAULT_PATTERN
            } else {
                pattern
            };
            set_config(app, |c| c.convert_name_pattern = pattern.to_owned());
        }
        Intent::SplitCue(on) => set_config(app, |c| c.convert_split_cue = on),
        Intent::CopyTags(on) => set_config(app, |c| c.convert_copy_tags = on),
        Intent::Pause => app.session.convert.pause(),
        Intent::Resume => {
            let effect = app.session.convert.resume();
            app.run(effect);
        }
        Intent::FlacCompression(level) => {
            set_config(app, |c| c.flac_options.compression_level = level)
        }
        Intent::FlacDepth(depth) => set_config(app, |c| c.flac_options.bit_depth = depth),
        Intent::Mp3Mode(mode) => set_config(app, |c| c.lossy_options.mp3_mode = mode),
        Intent::Bitrate(kbps) => set_config(app, |c| match c.convert_format {
            OutputFormat::Aac => c.lossy_options.aac_bitrate_kbps = kbps,
            _ => c.lossy_options.opus_bitrate_kbps = kbps,
        }),
        Intent::VorbisQuality(q) => set_config(app, |c| c.lossy_options.vorbis_quality = q),
        Intent::Start => {
            let effect = app.session.convert.start_queue(&app.session.config.config);
            app.run(effect);
        }
        Intent::CancelAll => app.session.convert.cancel_all(),
        Intent::ClearFinished => app.session.convert.clear_finished(),
        Intent::Cancel(id) => app.session.convert.cancel(id),
        Intent::Retry(id) => app.session.convert.retry(id),
        Intent::Remove(id) => app.session.convert.remove(id),
    }
}

/// Changes the persisted converter settings.
fn set_config(app: &mut App, change: impl FnOnce(&mut Config)) {
    change(&mut app.session.config.config);
    app.session.config.save();
}

/// Applies what the pickers brought back since the last frame.
fn drain_picks(app: &mut App) {
    while let Ok(picked) = app.views.converter.rx.try_recv() {
        match picked {
            Picked::Files(files) => {
                let probes = app.session.convert.enqueue(files);
                app.run(probes);
            }
            Picked::OutDir(dir) => set_config(app, |c| c.convert_out_dir = Some(dir)),
        }
    }
}

// ---------------------------------------------------------------- pickers

/// The files the user picked; empty when the dialog was cancelled.
async fn pick_files() -> Vec<PathBuf> {
    let filter_name = aulos_core::fl!("converter-filter-name");
    let filter = Filter::new(
        &filter_name,
        aulos_core::player::engine::format_registry::SUPPORTED_EXTENSIONS
            .iter()
            .chain(["cue", "mkv", "mov", "avi"].iter())
            .copied(),
    );
    dialogs::pick_files(
        &aulos_core::fl!("converter-pick-files-title"),
        Some(&filter),
    )
    .await
}

/// The folder the user picked; `None` when the dialog was cancelled.
async fn pick_folder() -> Option<PathBuf> {
    dialogs::pick_folder(&aulos_core::fl!("converter-pick-folder-title")).await
}
