use super::button::{PillKind, pill_button};
use super::text::paint_icon;
use crate::theme::{self, ColorExt, Theme};
use egui::{Align, CornerRadius, Layout, Sense, Ui, vec2};

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
    let top = (avail.height() * 0.5 - 110.0).max(24.0);
    ui.allocate_ui_with_layout(
        vec2(avail.width(), avail.height().max(220.0)),
        Layout::top_down(Align::Center),
        |ui| {
            ui.add_space(top);
            let (rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(28), tint.alpha(0.12));
            paint_icon(ui.painter(), rect.center(), icon, 26.0, tint, false);
            ui.add_space(14.0);
            ui.label(egui::RichText::new(title).font(theme::semibold(19.0)));
            ui.add_space(2.0);
            ui.allocate_ui(vec2(w, 60.0), |ui| {
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
                clicked = pill_button(ui, a, None, PillKind::Primary, 34.0, 0.0).clicked();
            }
        },
    );
    clicked
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
