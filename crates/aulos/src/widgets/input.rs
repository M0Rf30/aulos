use super::badge::keycap;
use crate::theme::{self, ColorExt, Theme};
use egui::{CornerRadius, Response, Sense, Stroke, StrokeKind, TextEdit, Ui, vec2};

/// The toolbar search field: magnifier, input, keycap hint (`/`, `Esc` when
/// focused). Returns the text edit's response; `.changed()` on edits.
pub fn search_field(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    width: f32,
    id: egui::Id,
) -> Response {
    let t = Theme::get(ui.ctx());
    let focused = ui.memory(|m| m.has_focus(id));
    let (rect, _) = ui.allocate_exact_size(vec2(width, 32.0), Sense::hover());
    let r = CornerRadius::same(8);
    ui.painter().rect_filled(rect, r, t.pal.surface_2);
    if focused {
        ui.painter().rect_stroke(
            rect.expand(1.5),
            CornerRadius::same(10),
            Stroke::new(3.0, t.pal.text.alpha(0.16)),
            StrokeKind::Outside,
        );
    }
    ui.painter().rect_stroke(
        rect,
        r,
        Stroke::new(1.0, if focused { t.pal.text } else { t.pal.stroke }),
        StrokeKind::Inside,
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(10.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 8.0;
    child.label(
        egui::RichText::new(egui_phosphor::regular::MAGNIFYING_GLASS)
            .font(theme::icon(15.0))
            .color(t.pal.text_2),
    );
    let hint_w = if focused || text.is_empty() {
        36.0
    } else {
        0.0
    };
    let edit = TextEdit::singleline(text)
        .id(id)
        .hint_text(egui::RichText::new(hint).color(t.pal.text_3))
        .frame(egui::Frame::NONE)
        .desired_width((child.available_width() - hint_w - 8.0).max(20.0))
        .font(theme::regular(13.0))
        .vertical_align(egui::Align::Center);
    let resp = child.add(edit);
    if hint_w > 0.0 {
        child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            keycap(ui, if focused { "Esc" } else { "/" });
        });
    }
    resp
}

/// A bordered single-line text field of `height` (settings, tag editor).
pub fn text_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32) -> Response {
    let t = Theme::get(ui.ctx());
    let edit = TextEdit::singleline(text)
        .hint_text(egui::RichText::new(hint).color(t.pal.text_3))
        .desired_width(width)
        .margin(vec2(10.0, 6.0))
        .font(theme::regular(13.0))
        .background_color(t.pal.surface_2);
    let resp = ui.add(edit);
    let focused = resp.has_focus();
    ui.painter().rect_stroke(
        resp.rect,
        CornerRadius::same(8),
        Stroke::new(1.0, if focused { t.pal.text } else { t.pal.input_bd }),
        StrokeKind::Inside,
    );
    resp
}

/// Records that the text field `id` is a password field. egui keeps no such
/// flag where a caller can read it, and the Android edit bar must not offer
/// Copy and Cut there (egui ignores both on a password field).
pub fn mark_password(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| d.insert_temp(password_key(id), true));
}

/// Whether [`mark_password`] was called for the field `id`.
#[cfg(target_os = "android")]
pub fn is_password(ctx: &egui::Context, id: egui::Id) -> bool {
    ctx.data(|d| d.get_temp::<bool>(password_key(id)).unwrap_or(false))
}

fn password_key(id: egui::Id) -> egui::Id {
    egui::Id::new("password_field").with(id)
}
