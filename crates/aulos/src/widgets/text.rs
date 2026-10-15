use crate::theme;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align, Color32, FontId, Painter, Rect, Ui, pos2};
use std::sync::Arc;

pub(crate) fn is_icon(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| ('\u{E000}'..='\u{F8FF}').contains(&c))
}

/// Lays `text` out on one row, ending in `…` when wider than `max_w`.
pub fn layout(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    max_w: f32,
    spacing: f32,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            color,
            extra_letter_spacing: spacing,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: max_w.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

/// One row made of differently styled `runs`, ending in `…` when wider than
/// the space left in the current row.
pub fn truncated_runs(ui: &mut Ui, runs: &[(&str, FontId, Color32)]) -> egui::Response {
    let mut job = LayoutJob::default();
    for (text, font, color) in runs {
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color: *color,
                valign: Align::Center,
                ..Default::default()
            },
        );
    }
    job.wrap = TextWrapping {
        max_width: ui.available_width().max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    ui.painter().galley(rect.min, galley, Color32::WHITE);
    resp
}

/// Paints one row of text inside `rect`, vertically centred, aligned
/// horizontally by `align`, ellipsised to the rect's width. Returns the
/// painted width.
pub fn paint_text(
    painter: &Painter,
    rect: Rect,
    align: Align,
    text: &str,
    font: FontId,
    color: Color32,
) -> f32 {
    paint_text_spaced(painter, rect, align, text, font, color, 0.0)
}

pub fn paint_text_spaced(
    painter: &Painter,
    rect: Rect,
    align: Align,
    text: &str,
    font: FontId,
    color: Color32,
    spacing: f32,
) -> f32 {
    let galley = layout(painter, text, font, color, rect.width(), spacing);
    let w = galley.size().x;
    let x = match align {
        Align::Min => rect.left(),
        Align::Center => rect.center().x - w / 2.0,
        Align::Max => rect.right() - w,
    };
    painter.galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    w
}

/// An icon glyph centred on `center`. `fill` selects the filled variant (the
/// glyph must come from `egui_phosphor::fill`).
pub fn paint_icon(
    painter: &Painter,
    center: egui::Pos2,
    glyph: &str,
    size: f32,
    color: Color32,
    fill: bool,
) {
    let font = if fill {
        theme::icon_fill(size)
    } else {
        theme::icon(size)
    };
    painter.text(center, egui::Align2::CENTER_CENTER, glyph, font, color);
}

/// Width `text` takes on one row.
pub fn text_width(ui: &Ui, text: &str, font: FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, Color32::WHITE)
        .size()
        .x
}

/// Allocates and paints a single-row label (no wrapping, ellipsised to the
/// available width).
pub fn label_text(ui: &mut Ui, text: &str, font: FontId, color: Color32) -> egui::Response {
    let galley = layout(ui.painter(), text, font, color, ui.available_width(), 0.0);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    ui.painter().galley(rect.min, galley, color);
    resp
}

/// `LIBRARY`-style section label: 10.5px semibold, +0.08em tracking.
pub fn caps_label(ui: &mut Ui, text: &str) -> egui::Response {
    let t = theme::Theme::get(ui.ctx());
    let galley = layout(
        ui.painter(),
        text,
        theme::semibold(10.5),
        t.pal.text_3,
        ui.available_width(),
        0.84,
    );
    let (rect, resp) = ui.allocate_exact_size(galley.size(), egui::Sense::hover());
    ui.painter().galley(rect.min, galley, t.pal.text_3);
    resp
}

/// Single-row text that scrolls horizontally (hover or `always`) when wider
/// than `rect`, with a soft fade of `bg` on the clipped edges. Falls back to a
/// static ellipsised row when reduced motion is on and not hovered.
#[allow(clippy::too_many_arguments)]
pub fn marquee_text(
    ui: &Ui,
    rect: Rect,
    id: egui::Id,
    text: &str,
    font: FontId,
    color: Color32,
    bg: Color32,
    run: bool,
) {
    let full = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), color);
    let overflow = full.size().x - rect.width();
    let reduce = theme::motion::reduced(ui.ctx());
    if overflow <= 0.5 || !run || reduce {
        paint_text(ui.painter(), rect, Align::Min, text, font, color);
        return;
    }
    const GAP: f32 = 36.0;
    const SPEED: f32 = 32.0;
    const PAUSE: f64 = 1.2;
    let cycle = f64::from(full.size().x + GAP) / f64::from(SPEED);
    // Read the clock before `data_mut`: nesting `input` inside it deadlocks.
    let now = ui.input(|i| i.time);
    let t0 = ui
        .ctx()
        .data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
    let t = (now - t0) % (cycle + PAUSE);
    let off = if t < PAUSE {
        0.0
    } else {
        ((t - PAUSE) * f64::from(SPEED)) as f32
    };
    let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let y = rect.center().y - full.size().y / 2.0;
    p.galley(pos2(rect.left() - off, y), full.clone(), color);
    p.galley(
        pos2(rect.left() - off + full.size().x + GAP, y),
        full,
        color,
    );
    fade_edges(&p, rect, bg, 14.0);
    ui.ctx().request_repaint();
}

/// Left/right gradient fade of `bg` over `rect`'s edges.
pub fn fade_edges(painter: &Painter, rect: Rect, bg: Color32, w: f32) {
    use egui::epaint::{Mesh, Vertex};
    let clear = Color32::TRANSPARENT;
    let mut mesh = Mesh::default();
    let mut quad = |x0: f32, x1: f32, c0: Color32, c1: Color32| {
        let i = mesh.vertices.len() as u32;
        for (x, y, c) in [
            (x0, rect.top(), c0),
            (x1, rect.top(), c1),
            (x1, rect.bottom(), c1),
            (x0, rect.bottom(), c0),
        ] {
            mesh.vertices.push(Vertex {
                pos: pos2(x, y),
                uv: egui::epaint::WHITE_UV,
                color: c,
            });
        }
        mesh.indices.extend([i, i + 1, i + 2, i, i + 2, i + 3]);
    };
    quad(rect.left(), rect.left() + w, bg, clear);
    quad(rect.right() - w, rect.right(), clear, bg);
    painter.add(egui::Shape::mesh(mesh));
}

/// The text-run id helper for marquees keyed by track.
pub fn marquee_id(scope: &str, text: &str) -> egui::Id {
    egui::Id::new(("marquee", scope, text))
}
