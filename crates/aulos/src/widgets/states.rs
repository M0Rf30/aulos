use super::button::{PillKind, pill_button};
use super::text::paint_icon;
use crate::theme::{self, ColorExt, Theme};
use egui::{Align, CornerRadius, Layout, Sense, Ui, vec2};

/// Top padding that centres an empty-state block of `content_h` in `avail_h`
/// (never negative-looking: at least 8pt), so short windows still fit it.
fn top_pad(avail_h: f32, content_h: f32) -> f32 {
    ((avail_h - content_h) * 0.5 - 10.0).clamp(8.0, 140.0)
}

/// Height of `body` wrapped at `w` in `font`.
fn body_height(ui: &Ui, body: &str, font: egui::FontId, w: f32) -> f32 {
    ui.painter()
        .layout(body.to_owned(), font, egui::Color32::WHITE, w)
        .size()
        .y
}

/// Centred empty/error block: an icon in a 56px disc, a title, a body and an
/// optional pill button. Returns true when the button was clicked.
pub fn empty_state(ui: &mut Ui, icon: &str, title: &str, body: &str, action: Option<&str>) -> bool {
    empty_state_tinted(ui, icon, title, body, action, None)
}

pub fn empty_state_tinted(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    body: &str,
    action: Option<&str>,
    tint: Option<egui::Color32>,
) -> bool {
    let t = Theme::get(ui.ctx());
    let tint = tint.unwrap_or(t.pal.text_2);
    let mut clicked = false;
    let avail = ui.available_rect_before_wrap();
    let w = 360.0_f32.min(avail.width());
    let body_h = body_height(ui, body, theme::regular(13.5), w);
    let content_h = 56.0 + 14.0 + 26.0 + 2.0 + body_h + action.map_or(0.0, |_| 44.0);
    let top = top_pad(avail.height(), content_h);
    ui.allocate_ui_with_layout(
        vec2(avail.width(), avail.height().max(120.0)),
        Layout::top_down(Align::Center),
        |ui| {
            // A window too short for the block scrolls it instead of clipping.
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .id_salt("empty-state")
                .show(ui, |ui| {
                    ui.with_layout(Layout::top_down(Align::Center), |ui| {
                        ui.add_space(top);
                        let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(28), tint.alpha(0.12));
                        paint_icon(ui.painter(), rect.center(), icon, 26.0, tint, false);
                        ui.add_space(14.0);
                        ui.label(egui::RichText::new(title).font(theme::semibold(19.0)));
                        ui.add_space(2.0);
                        ui.allocate_ui(vec2(w, body_h), |ui| {
                            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                                ui.label(
                                    egui::RichText::new(body)
                                        .color(t.pal.text_2)
                                        .font(theme::regular(13.5)),
                                );
                            });
                        });
                        if let Some(a) = action {
                            ui.add_space(10.0);
                            clicked =
                                pill_button(ui, a, None, PillKind::Primary, 34.0, 0.0).clicked();
                        }
                    });
                });
        },
    );
    clicked
}

/// Design empty state: a 56px rounded-square tile (reed icon, or danger-tinted
/// when `error`), title, body and a primary + optional secondary pill.
/// Returns `(primary_clicked, secondary_clicked)`.
pub fn empty_state_actions(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    body: &str,
    primary: &str,
    secondary: Option<&str>,
    error: bool,
) -> (bool, bool) {
    let t = Theme::get(ui.ctx());
    let (bg, fg) = if error {
        (
            t.pal.danger.alpha(0.14),
            egui::Color32::from_rgb(0xEE, 0x8A, 0x7E),
        )
    } else {
        (t.pal.surface_1, t.pal.reed)
    };
    let mut clicks = (false, false);
    let avail = ui.available_rect_before_wrap();
    let w = 360.0_f32.min(avail.width());
    let body_h = body_height(ui, body, theme::regular(14.0), w);
    let content_h = 56.0 + 14.0 + 28.0 + 2.0 + body_h + 44.0;
    let top = top_pad(avail.height(), content_h);
    ui.allocate_ui_with_layout(
        vec2(avail.width(), avail.height().max(120.0)),
        Layout::top_down(Align::Center),
        |ui| {
            // A window too short for the block scrolls it instead of clipping.
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .id_salt("empty-state")
                .show(ui, |ui| {
                    ui.with_layout(Layout::top_down(Align::Center), |ui| {
                        ui.add_space(top);
                        let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
                        ui.painter().rect_filled(rect, CornerRadius::same(14), bg);
                        paint_icon(ui.painter(), rect.center(), icon, 26.0, fg, false);
                        ui.add_space(14.0);
                        ui.label(egui::RichText::new(title).font(theme::semibold(20.0)));
                        ui.add_space(2.0);
                        ui.allocate_ui(vec2(w, body_h), |ui| {
                            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                                ui.label(
                                    egui::RichText::new(body)
                                        .color(t.pal.text_2)
                                        .font(theme::regular(14.0)),
                                );
                            });
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            let pw = 140.0 + secondary.map_or(0.0, |_| 150.0);
                            ui.add_space(((ui.available_width() - pw) * 0.5).max(0.0));
                            clicks.0 = pill_button(ui, primary, None, PillKind::Primary, 34.0, 0.0)
                                .clicked();
                            if let Some(sec) = secondary {
                                clicks.1 =
                                    pill_button(ui, sec, None, PillKind::Secondary, 34.0, 0.0)
                                        .clicked();
                            }
                        });
                    });
                });
        },
    );
    clicks
}

/// A pulsing placeholder block (loading state).
pub fn skeleton(ui: &mut Ui, rect: egui::Rect, radius: u8) {
    let t = Theme::get(ui.ctx());
    let phase = (ui.input(|i| i.time) * 2.4).sin() as f32 * 0.5 + 0.5;
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius),
        t.pal.surface_hi.alpha(0.45 + 0.35 * phase),
    );
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(50));
}
