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
///
/// On hover the rail grows by 2px and, when `total_secs` is non-zero, a time
/// bubble follows the pointer.
pub fn seek_bar(
    ui: &mut Ui,
    frac: f32,
    height: f32,
    max_width: f32,
    enabled: bool,
    total_secs: f32,
) -> SeekBar {
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
    let active = enabled && (response.hovered() || response.dragged());
    let height = if active { height + 2.0 } else { height };
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
        let at = response
            .hover_pos()
            .or_else(|| response.interact_pointer_pos())
            .map(|p| ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
        if let (Some(f), true) = (at, total_secs >= 1.0) {
            let label = aulos_core::format::format_duration((f * total_secs) as u64);
            let font = crate::theme::mono(11.0);
            let galley = ui.painter().layout_no_wrap(label, font, t.pal.text);
            let size = galley.size() + vec2(12.0, 6.0);
            let cx = (rect.left() + rect.width() * f)
                .clamp(rect.left() + size.x / 2.0, rect.right() - size.x / 2.0);
            let bubble =
                Rect::from_center_size(pos2(cx, rail_rect.top() - 6.0 - size.y / 2.0), size);
            let layer = ui.painter().clone().with_layer_id(egui::LayerId::new(
                egui::Order::Tooltip,
                response.id.with("seek-bubble"),
            ));
            layer.rect_filled(bubble, CornerRadius::same(4), t.pal.surface_2);
            layer.rect_stroke(
                bubble,
                CornerRadius::same(4),
                egui::Stroke::new(1.0, t.pal.stroke_strong),
                egui::StrokeKind::Inside,
            );
            layer.galley(bubble.center() - galley.size() / 2.0, galley, t.pal.text);
        }
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
