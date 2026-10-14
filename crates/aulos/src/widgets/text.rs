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
