//! The mini now-playing view (replaces the whole window).

use super::expanded::{TransportStyle, paint_backdrop, progress, seek_row, transport_row};
use crate::app::App;
use crate::theme::{self, ColorExt, Theme, hex};
use crate::widgets::{IconBtn, cover_image, paint_text, paint_text_spaced, text_width};
use egui::{
    Align, Color32, CornerRadius, Rect, Shadow, Ui, UiBuilder, ViewportCommand, WindowLevel, pos2,
    vec2,
};
use egui_phosphor::{fill, regular};

const SUB: Color32 = hex(0xC9D2D6);

#[derive(Default)]
pub struct State {
    /// The window is kept above others.
    pinned: bool,
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let t = Theme::get(ui.ctx());
    let rect = ui.max_rect();
    let veil = Color32::from_rgba_unmultiplied(8, 16, 20, 189);
    paint_backdrop(ui, app, rect, veil, veil);

    // Drag the undecorated window from anywhere that is not a control.
    let drag = ui.interact(
        rect,
        egui::Id::new("mini_drag"),
        egui::Sense::click_and_drag(),
    );
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }

    let Some(track) = app.session.playback.current_track.clone() else {
        let mid = Rect::from_center_size(rect.center(), vec2(rect.width(), 24.0));
        paint_text(
            ui.painter(),
            mid,
            Align::Center,
            &aulos_core::fl!("mini-nothing-playing"),
            theme::semibold(14.0),
            SUB,
        );
        expand_button(
            app,
            ui,
            Rect::from_min_size(
                pos2(rect.right() - 14.0 - 28.0, rect.top() + 14.0),
                vec2(28.0, 28.0),
            ),
        );
        return;
    };

    let left = rect.left() + 14.0;
    let right = rect.right() - 14.0;
    let top = rect.top() + 14.0;

    // Cover.
    let cover = Rect::from_min_size(pos2(left, top), vec2(72.0, 72.0));
    ui.painter().add(
        Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(115),
        }
        .as_shape(cover, CornerRadius::same(6)),
    );
    let key = track.cover_key();
    let tex = app.cover_large.as_ref().or_else(|| app.covers.thumb(&key));
    cover_image(ui, &app.covers, cover, tex, &key, 6);

    // Window buttons, stacked on the right.
    let pin = Rect::from_min_size(pos2(right - 28.0, top), vec2(28.0, 28.0));
    let expand = Rect::from_min_size(pos2(right - 28.0, top + 30.0), vec2(28.0, 28.0));
    let pinned = app.views.mini.pinned;
    if pinned {
        ui.painter()
            .rect_filled(pin, CornerRadius::same(6), Color32::WHITE.alpha(0.08));
    }
    let pin_resp = ui.scope_builder(UiBuilder::new().max_rect(pin), |ui| {
        IconBtn::new(if pinned {
            fill::PUSH_PIN
        } else {
            regular::PUSH_PIN
        })
        .filled(pinned)
        .size(28.0)
        .icon_size(14.0)
        .color(if pinned { t.accent.fill } else { SUB })
        .tip(if pinned {
            aulos_core::fl!("mini-unpin")
        } else {
            aulos_core::fl!("mini-keep-on-top")
        })
        .show(ui)
    });
    if pin_resp.inner.clicked() {
        app.views.mini.pinned = !pinned;
        let level = if pinned {
            WindowLevel::Normal
        } else {
            WindowLevel::AlwaysOnTop
        };
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::WindowLevel(level));
    }
    expand_button(app, ui, expand);

    // Title, artist, quality badge.
    let text_x = cover.right() + 12.0;
    let text_w = (right - 28.0 - 12.0 - text_x).max(40.0);
    let title = Rect::from_min_size(pos2(text_x, top + 6.0), vec2(text_w, 20.0));
    crate::widgets::marquee_text(
        ui,
        title,
        crate::widgets::marquee_id("mini", &track.title),
        &track.title,
        theme::semibold(14.0),
        Color32::WHITE,
        hex(0x0A1418).alpha(0.9),
        true,
    );
    let artist = Rect::from_min_size(pos2(text_x, top + 28.0), vec2(text_w, 18.0));
    paint_text(
        ui.painter(),
        artist,
        Align::Min,
        &track.artist,
        theme::regular(12.5),
        SUB,
    );
    if let Some(q) = App::quality_of(&track) {
        let dark = t.pal.dark;
        let font = theme::mono_semibold(10.0);
        let label = match track.depth_rate_label() {
            Some(rate) => format!("{} {rate}", q.label()),
            None => q.label().to_owned(),
        };
        let w = text_width(ui, &label, font.clone()) + 0.4 * label.chars().count() as f32 + 10.0;
        let badge = Rect::from_min_size(pos2(text_x, top + 52.0), vec2(w.min(text_w), 16.0));
        ui.painter()
            .rect_filled(badge, CornerRadius::same(4), q.bg(dark));
        ui.painter().rect_stroke(
            badge,
            CornerRadius::same(4),
            egui::Stroke::new(1.0, q.border(dark)),
            egui::StrokeKind::Inside,
        );
        paint_text_spaced(
            ui.painter(),
            badge.shrink2(vec2(5.0, 0.0)),
            Align::Min,
            &label,
            font,
            q.fg(dark),
            0.4,
        );
    }

    // Times + seek.
    let prog = progress(app);
    let line_y = top + 72.0 + 10.0 + 8.0;
    let mono = theme::mono(11.0);
    let left_w = text_width(ui, &prog.elapsed, mono.clone()).max(28.0);
    let right_w = text_width(ui, &prog.remaining, mono.clone()).max(34.0);
    let line = Rect::from_min_max(pos2(left, line_y - 8.0), pos2(right, line_y + 8.0));
    paint_text(
        ui.painter(),
        line,
        Align::Min,
        &prog.elapsed,
        mono.clone(),
        SUB,
    );
    paint_text(ui.painter(), line, Align::Max, &prog.remaining, mono, SUB);
    let rail = Rect::from_min_max(
        pos2(left + left_w + 8.0, line.top()),
        pos2(right - right_w - 8.0, line.bottom()),
    );
    seek_row(app, ui, rail, 4.0, prog.enabled, prog.frac, false);

    // Transport.
    let row = Rect::from_min_max(
        pos2(left, line.bottom() + 6.0),
        pos2(right, rect.bottom() - 12.0),
    );
    let style = TransportStyle {
        side: 30.0,
        side_icon: 16.0,
        skip: 30.0,
        skip_icon: 18.0,
        disc: 38.0,
        disc_icon: 18.0,
        gap: Some(8.0),
        halo: false,
    };
    transport_row(app, ui, row, &style);
}

fn expand_button(app: &mut App, ui: &mut Ui, rect: Rect) {
    let resp = ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        IconBtn::new(regular::ARROWS_OUT_SIMPLE)
            .size(28.0)
            .icon_size(15.0)
            .color(SUB)
            .tip(aulos_core::fl!("mini-expand"))
            .show(ui)
    });
    if resp.inner.clicked() {
        let ctx = ui.ctx().clone();
        app.set_mini(&ctx, false);
    }
}
