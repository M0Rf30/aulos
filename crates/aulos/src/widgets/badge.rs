use super::text::{layout, paint_text};
use crate::theme::{self, Quality, Theme};
use egui::{Align, Color32, CornerRadius, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};

/// A bordered pill-ish mono tag (9.5px, semibold, +0.04em): used for quality
/// tiers, `LAB`, `NEW`, `ON AIR`…
pub fn tag_badge(
    ui: &mut Ui,
    text: &str,
    fg: Color32,
    bg: Color32,
    border: Color32,
) -> egui::Response {
    let font = theme::mono_semibold(9.5);
    let galley = layout(ui.painter(), text, font, fg, 200.0, 0.38);
    let size = galley.size() + vec2(8.0, 6.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(size.x, size.y.max(15.0)), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(theme::radius::XS), bg);
    p.rect_stroke(
        rect,
        CornerRadius::same(theme::radius::XS),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
    p.galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

/// The quality tier badge (`LOSSY`, `CD`, `HI-RES`, `DSD`).
pub fn quality_badge(ui: &mut Ui, q: Quality) -> egui::Response {
    let dark = Theme::get(ui.ctx()).pal.dark;
    tag_badge(ui, q.label(), q.fg(dark), q.bg(dark), q.border(dark))
}

/// A keyboard hint in a bordered box (`/`, `Esc`).
pub fn keycap(ui: &mut Ui, text: &str) -> egui::Response {
    let t = Theme::get(ui.ctx());
    let font = theme::mono(11.0);
    let galley = layout(ui.painter(), text, font, t.pal.text_3, 80.0, 0.0);
    let size = vec2(galley.size().x + 12.0, 20.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, t.pal.input_bd),
        StrokeKind::Inside,
    );
    paint_text(
        ui.painter(),
        rect,
        Align::Center,
        text,
        theme::mono(11.0),
        t.pal.text_3,
    );
    resp
}
