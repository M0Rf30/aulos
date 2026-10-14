//! Transient toasts: an inverse pill at the bottom centre.

use crate::app::{App, ToastKind};
use crate::theme::{self, ColorExt, size};
use egui::{Align2, Area, Frame, Id, Margin, Order, RichText, Stroke};

pub fn show(app: &mut App, ctx: &egui::Context) {
    if app.toasts.is_empty() {
        return;
    }
    let now = ctx.input(|i| i.time);
    let pal = app.pal;
    let mut y = size::NOW_BAR + 16.0;
    let mut dismissed = None;
    for (i, toast) in app.toasts.iter().enumerate().rev() {
        let age = now - toast.born;
        let fade = ((toast.life - age) / 0.25)
            .clamp(0.0, 1.0)
            .min((age / 0.12).clamp(0.0, 1.0)) as f32;
        let (bg, fg) = match toast.kind {
            ToastKind::Info => (pal.text, pal.bg),
            ToastKind::Error => (pal.danger, egui::Color32::WHITE),
        };
        let resp = Area::new(Id::new(("toast", i)))
            .order(Order::Tooltip)
            .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -y))
            .show(ctx, |ui| {
                ui.set_opacity(fade);
                Frame::new()
                    .fill(bg)
                    .stroke(Stroke::new(1.0, bg.alpha(0.2)))
                    .corner_radius(18)
                    .shadow(theme::shadow([0, 8], 24, 90))
                    .inner_margin(Margin::symmetric(16, 8))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(&toast.text)
                                .font(theme::medium(13.0))
                                .color(fg),
                        );
                    });
            })
            .response;
        if resp.clicked() {
            dismissed = Some(i);
        }
        y += resp.rect.height() + 8.0;
    }
    if let Some(i) = dismissed {
        app.toasts.remove(i);
    }
}
