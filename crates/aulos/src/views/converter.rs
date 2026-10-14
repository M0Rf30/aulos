//! The converter page: the job queue and the output settings.
//!
//! Everything the page decides lives in `aulos_core::session::ConvertState`
//! (queue, start, cancel, retry) and in the persisted `Config` (format, sample
//! rate, output folder, encoder options); this module draws them and routes
//! the user's clicks. Pickers are native dialogs run off the UI thread and
//! report back through a channel drained every frame.

use crate::app::App;
use crate::dialogs::{self, Filter};
use crate::theme::{self, ColorExt, Palette, lerp_color};
use crate::widgets::{
    PillKind, empty_state, paint_icon, paint_text, pill_button_enabled, tag_badge,
};
use aulos_core::config::Config;
use aulos_core::convert::encoder::{FlacBitDepth, FlacOptions, LossyOptions, Mp3Mode};
use aulos_core::convert::options::{
    BITRATE_KBPS_OPTIONS, FLAC_BIT_DEPTH_OPTIONS, FLAC_COMPRESSION_OPTIONS, MP3_MODE_OPTIONS,
    SAMPLE_RATE_OPTIONS, VORBIS_QUALITY_OPTIONS, bitrate_kbps_label, flac_bit_depth_label,
    format_short_label, mp3_mode_label, rate_label,
};
use aulos_core::convert::{self, ConvertJob, JobCounts, JobId, JobKind, JobState, OutputFormat};
use aulos_core::session::ConvertState;
use egui::epaint::TextShape;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Id, Label, Layout, Margin, Popup, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use egui_phosphor::{fill, regular};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
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
        app.session.convert.enqueue(dropped);
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
            RichText::new("Converter")
                .font(theme::semibold(26.0))
                .color(pal.text),
        );
        tag_badge(ui, "LAB", pal.text_1, Color32::TRANSPARENT, pal.text_5);
        ui.label(
            RichText::new("Experimental. Originals are never modified; output goes to")
                .color(pal.text_2),
        );
        ui.add(
            Label::new(
                RichText::new(convert::out_dir(config).display().to_string())
                    .font(theme::mono(12.0))
                    .color(pal.text_1),
            )
            .truncate(),
        );
    });

    // Banners.
    if let Some(error) = &app.session.convert.dir_error {
        let text = format!("Could not use the output folder: {error}");
        if banner(
            ui,
            &pal,
            pal.danger,
            regular::WARNING_CIRCLE,
            &text,
            Some("Change folder"),
        ) {
            out.push(Intent::ChangeDir);
        }
    }
    if config.convert_format.requires_ffmpeg() {
        match convert::ffmpeg::cached() {
            Some(true) => {}
            Some(false) => {
                let text = format!(
                    "{} needs ffmpeg, which was not found on this system. Install it to convert to this format.",
                    format_short_label(config.convert_format)
                );
                banner(ui, &pal, pal.warn, regular::WARNING, &text, None);
            }
            None => {
                banner(
                    ui,
                    &pal,
                    pal.text_2,
                    regular::INFO,
                    "Checking for ffmpeg…",
                    None,
                );
            }
        }
    }

    // Queue | Output card.
    let body = ui.available_rect_before_wrap();
    let right_w = if app.narrow { 300.0 } else { 340.0 };
    let left = Rect::from_min_max(
        body.min,
        pos2(
            (body.right() - right_w - 20.0).max(body.left() + 160.0),
            body.bottom(),
        ),
    );
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
            RichText::new("Queue")
                .font(theme::semibold(15.0))
                .color(pal.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if pill_button_enabled(
                ui,
                "Add files",
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
            if counts.queued + counts.running > 0
                && pill_button_enabled(ui, "Cancel all", None, PillKind::Danger, 28.0, 0.0, true)
                    .clicked()
            {
                out.push(Intent::CancelAll);
            }
            if counts.done + counts.failed > 0 && ghost_button(ui, &pal, "Clear finished").clicked()
            {
                out.push(Intent::ClearFinished);
            }
            let mut summary = format!("{} jobs · {} done", jobs.len(), counts.done);
            if counts.failed > 0 {
                summary.push_str(&format!(" · {} failed", counts.failed));
            }
            ui.label(RichText::new(summary).color(pal.text_2));
        });
    });

    if jobs.is_empty() {
        if empty_state(
            ui,
            regular::ARROWS_LEFT_RIGHT,
            "Nothing to convert",
            "Add audio files, video files or a CUE sheet — or drop them here. Originals are never modified.",
            Some("Add files"),
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
    path.extension()
        .and_then(OsStr::to_str)
        .map_or_else(|| "FILE".to_owned(), str::to_uppercase)
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
        JobState::Running => format!("{}%", job.progress_permille() / 10),
        JobState::Done => "Done".to_owned(),
        JobState::Queued => "Queued".to_owned(),
        JobState::Failed(reason) => reason.clone(),
        JobState::Cancelled => "Cancelled".to_owned(),
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
    if matches!(job.state, JobState::Running) {
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
    let mut meta = match job.kind {
        JobKind::Convert => format!(
            "{} → {}",
            extension_upper(&job.source),
            format_tag(format, flac, lossy)
        ),
        JobKind::CueSplit => format!("CUE split → {}", format_tag(format, flac, lossy)),
    };
    if let Some(hz) = rate {
        meta.push_str(&format!(" · {} kHz", khz_text(hz)));
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
        JobState::Done => ("Show in folder", Intent::OpenDir(job_dir())),
        JobState::Running => ("Cancel", Intent::Cancel(job.id)),
        JobState::Queued => ("Remove from queue", Intent::Remove(job.id)),
        JobState::Failed(_) | JobState::Cancelled => ("Retry", Intent::Retry(job.id)),
    };
    if action_resp.on_hover_text(tip).clicked() {
        out.push(intent);
    }

    resp.context_menu(|ui| {
        if matches!(job.state, JobState::Done) && ui.button("Show in folder").clicked() {
            out.push(Intent::OpenDir(job_dir()));
        }
        if matches!(job.state, JobState::Running) && ui.button("Cancel").clicked() {
            out.push(Intent::Cancel(job.id));
        }
        if matches!(job.state, JobState::Failed(_) | JobState::Cancelled)
            && ui.button("Retry").clicked()
        {
            out.push(Intent::Retry(job.id));
        }
        if !matches!(job.state, JobState::Running) && ui.button("Remove from list").clicked() {
            out.push(Intent::Remove(job.id));
        }
    });
}

fn khz_text(hz: u32) -> String {
    let khz = hz as f32 / 1000.0;
    if khz.fract() == 0.0 {
        format!("{}", khz as u32)
    } else {
        format!("{khz}")
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
                RichText::new("Output")
                    .font(theme::semibold(15.0))
                    .color(pal.text),
            );
            format_section(ui, &pal, config.convert_format, out);
            settings_grid(ui, &pal, config, out);
            folder_section(ui, &pal, config, out);
        });

    let counts = JobCounts::compute(&app.session.convert.jobs);
    let ffmpeg_blocked =
        config.convert_format.requires_ffmpeg() && convert::ffmpeg::cached() != Some(true);
    let ready = counts.queued > 0 && !ffmpeg_blocked;
    let summary = if counts.queued == 0 {
        "Nothing queued".to_owned()
    } else {
        format!(
            "{} {} → {}",
            counts.queued,
            if counts.queued == 1 { "file" } else { "files" },
            format_tag(
                config.convert_format,
                config.flac_options,
                config.lossy_options
            )
        )
    };
    let mut footer_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(footer)
            .layout(Layout::right_to_left(Align::Center)),
    );
    if pill_button_enabled(
        &mut footer_ui,
        "Convert",
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
    field_label(ui, pal, "Format", 1.0);
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
        || "More formats…".to_owned(),
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
            field_label(ui, pal, "Sample rate", 1.0);
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
            field_label(ui, pal, "Bit depth", opacity);
            let value = if enabled {
                flac_bit_depth_label(flac.bit_depth)
            } else {
                format!("n/a for {}", format_short_label(format))
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
            field_label(ui, pal, "Compression", 1.0);
            let selected = FLAC_COMPRESSION_OPTIONS
                .iter()
                .position(|l| *l == flac.compression_level);
            let value = format!("Level {}", flac.compression_level);
            let dd = Dropdown {
                salt: "convert-quality",
                value: &value,
                count: FLAC_COMPRESSION_OPTIONS.len(),
                selected,
                enabled: true,
            };
            if let Some(i) = dropdown(ui, pal, &dd, |i| {
                format!("Level {}", FLAC_COMPRESSION_OPTIONS[i])
            }) {
                out.push(Intent::FlacCompression(FLAC_COMPRESSION_OPTIONS[i]));
            }
        }
        OutputFormat::Mp3 => {
            field_label(ui, pal, "Quality", 1.0);
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
            field_label(ui, pal, "Bitrate", 1.0);
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
            field_label(ui, pal, "Quality", 1.0);
            let selected = VORBIS_QUALITY_OPTIONS
                .iter()
                .position(|q| (*q - lossy.vorbis_quality).abs() < 0.01);
            let value = format!("Quality {}", quality_text(lossy.vorbis_quality));
            let dd = Dropdown {
                salt: "convert-quality",
                value: &value,
                count: VORBIS_QUALITY_OPTIONS.len(),
                selected,
                enabled: true,
            };
            if let Some(i) = dropdown(ui, pal, &dd, |i| {
                format!("Quality {}", quality_text(VORBIS_QUALITY_OPTIONS[i]))
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
            field_label(ui, pal, "Quality", 0.38);
            let dd = Dropdown {
                salt: "convert-quality",
                value: "Lossless",
                count: 0,
                selected: None,
                enabled: false,
            };
            dropdown(ui, pal, &dd, |_| String::new());
        }
    }
}

fn folder_section(ui: &mut Ui, pal: &Palette, config: &Config, out: &mut Vec<Intent>) {
    ui.spacing_mut().item_spacing.y = 6.0;
    field_label(ui, pal, "Output folder", 1.0);
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
            "Change…",
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
            "Open",
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
            let ctx = app.ctx.clone();
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
            let ctx = app.ctx.clone();
            app.handle.spawn(async move {
                if let Some(dir) = pick_folder().await {
                    let _ = tx.send(Picked::OutDir(dir));
                    ctx.request_repaint();
                }
            });
        }
        Intent::OpenDir(dir) => {
            if let Err(error) = open::that_detached(&dir) {
                app.toast_error(format!("Could not open the folder: {error}"));
            }
        }
        Intent::Format(format) => set_config(app, |c| c.convert_format = format),
        Intent::Rate(rate) => set_config(app, |c| c.convert_sample_rate = rate),
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
            Picked::Files(files) => app.session.convert.enqueue(files),
            Picked::OutDir(dir) => set_config(app, |c| c.convert_out_dir = Some(dir)),
        }
    }
}

// ---------------------------------------------------------------- pickers

/// The files the user picked; empty when the dialog was cancelled.
async fn pick_files() -> Vec<PathBuf> {
    let filter = Filter::new(
        "Audio, video and CUE files",
        aulos_core::player::engine::decoder::SUPPORTED_EXTENSIONS
            .iter()
            .chain(["cue", "mkv", "mov", "avi"].iter())
            .copied(),
    );
    dialogs::pick_files("Select audio or video files, or a CUE sheet", Some(&filter)).await
}

/// The folder the user picked; `None` when the dialog was cancelled.
async fn pick_folder() -> Option<PathBuf> {
    dialogs::pick_folder("Select the output folder").await
}
