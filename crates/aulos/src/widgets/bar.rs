use crate::theme::Theme;
use egui::{CornerRadius, Rect, Response, Sense, Ui, Vec2, pos2, vec2};

/// What a [`seek_bar`] interaction produced.
pub struct SeekBar {
    /// The fraction under the pointer while pressed/dragging.
    pub dragging: Option<f32>,
    /// The fraction at release (a click counts).
    pub released: Option<f32>,
}

fn rail(ui: &Ui, rect: Rect, frac: f32, fill: egui::Color32, track: egui::Color32) {
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

/// The seek bar: a `height`px rail in a taller hit area, with a thumb on
/// hover/drag. Fills the available width (capped by `max_width`).
pub fn seek_bar(ui: &mut Ui, frac: f32, height: f32, max_width: f32, enabled: bool) -> SeekBar {
    let t = Theme::get(ui.ctx());
    let w = ui.available_width().min(max_width);
    let hit = (height + 12.0).max(16.0);
    let (rect, response) = ui.allocate_exact_size(
        vec2(w, hit),
        if enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let rail_rect = Rect::from_center_size(rect.center(), vec2(rect.width(), height));
    let pointer = response
        .interact_pointer_pos()
        .map(|p| ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let shown = pointer
        .filter(|_| response.dragged() || response.is_pointer_button_down_on())
        .unwrap_or(frac);
    rail(ui, rail_rect, shown, t.accent.fill, t.pal.track);
    if enabled && (response.hovered() || response.dragged()) {
        let x = rail_rect.left() + rail_rect.width() * shown.clamp(0.0, 1.0);
        ui.painter().circle_filled(
            pos2(x, rail_rect.center().y),
            height / 2.0 + 3.0,
            t.pal.text,
        );
    }
    let dragging = (response.dragged() || response.is_pointer_button_down_on())
        .then_some(shown)
        .filter(|_| enabled);
    let released = (response.drag_stopped() || response.clicked())
        .then_some(shown)
        .filter(|_| enabled);
    SeekBar { dragging, released }
}

/// A compact value slider (volume): returns the new value while the user
/// drags/clicks.
pub fn slider_bar(
    ui: &mut Ui,
    value: f32,
    size: Vec2,
    fill: egui::Color32,
) -> (Response, Option<f32>) {
    let t = Theme::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let rail_rect = Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
    rail(ui, rail_rect, value, fill, t.pal.track);
    if response.hovered() || response.dragged() {
        let x = rail_rect.left() + rail_rect.width() * value.clamp(0.0, 1.0);
        ui.painter()
            .circle_filled(pos2(x, rail_rect.center().y), 5.0, t.pal.text);
    }
    let new = (response.dragged() || response.clicked())
        .then(|| response.interact_pointer_pos())
        .flatten()
        .map(|p| ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    (response, new)
}
