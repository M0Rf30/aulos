//! Transient toasts: an inverse pill at the bottom centre.

use crate::app::ToastAction;
use crate::app::{App, ToastKind};
use crate::theme::{self, ColorExt, size};
use crate::widgets::{paint_icon, paint_text, text_width};
use egui::{
    Align, Align2, Area, CornerRadius, Frame, Id, Margin, Order, RichText, Sense, Stroke, vec2,
};
use egui_phosphor::regular;

pub fn show(app: &mut App, ctx: &egui::Context) {
    if app.toasts.is_empty() {
        return;
    }
    let now = ctx.input(|i| i.time);
    let pal = app.pal;
    let mut y = size::NOW_BAR + 16.0;
    let mut dismissed = None;
    let mut actions: Vec<usize> = Vec::new();
    for (i, toast) in app.toasts.iter().enumerate().rev() {
        let age = now - toast.born;
        let fade = ((toast.life - age) / 0.25)
            .clamp(0.0, 1.0)
            .min((age / 0.12).clamp(0.0, 1.0)) as f32;
        let (bg, fg) = match toast.kind {
            ToastKind::Info => (pal.text, pal.bg),
            ToastKind::Error => (pal.danger, egui::Color32::WHITE),
        };
        let mut close = false;
        let mut act = false;
        let resp = Area::new(Id::new(("toast", i)))
            .order(Order::Tooltip)
            .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -y))
            .show(ctx, |ui| {
                ui.set_opacity(fade);
                Frame::new()
                    .fill(bg)
                    .stroke(Stroke::new(1.0, bg.alpha(0.2)))
                    .corner_radius(10)
                    .shadow(theme::shadow([0, 12], 30, 115))
                    .inner_margin(Margin {
                        left: 14,
                        right: 6,
                        top: 0,
                        bottom: 0,
                    })
                    .show(ui, |ui| {
                        ui.set_height(42.0);
                        ui.spacing_mut().item_spacing.x = 12.0;
                        ui.horizontal_centered(|ui| {
                            let icon = toast.icon.unwrap_or(match toast.kind {
                                ToastKind::Info => regular::INFO,
                                ToastKind::Error => regular::WARNING_CIRCLE,
                            });
                            ui.label(RichText::new(icon).font(theme::icon(15.0)).color(fg));
                            let text = RichText::new(&toast.text)
                                .font(theme::medium(13.0))
                                .color(fg);
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.label(text);
                            if let Some(subject) = &toast.subject {
                                ui.label(RichText::new(subject).font(theme::bold(13.0)).color(fg));
                                ui.label(
                                    RichText::new(&toast.suffix)
                                        .font(theme::medium(13.0))
                                        .color(fg),
                                );
                            }
                            ui.spacing_mut().item_spacing.x = 12.0;
                            if let Some(action) = &toast.action {
                                let label = action.label();
                                let label = label.as_str();
                                let w = text_width(ui, label, theme::semibold(13.0)) + 24.0;
                                let (rect, r) =
                                    ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
                                let fill = fg.alpha(if r.hovered() { 0.16 } else { 0.10 });
                                ui.painter().rect_filled(rect, CornerRadius::same(7), fill);
                                paint_text(
                                    ui.painter(),
                                    rect,
                                    Align::Center,
                                    label,
                                    theme::semibold(13.0),
                                    fg,
                                );
                                if r.clicked() {
                                    act = true;
                                }
                            }
                            let (rect, r) =
                                ui.allocate_exact_size(vec2(26.0, 30.0), Sense::click());
                            let color = if r.hovered() { fg } else { fg.alpha(0.6) };
                            paint_icon(ui.painter(), rect.center(), regular::X, 13.0, color, false);
                            if r.clicked() {
                                close = true;
                            }
                        });
                    });
            })
            .response;
        if act {
            actions.push(i);
        }
        if close || act {
            dismissed = Some(i);
        }
        y += resp.rect.height() + 8.0;
    }
    if let Some(i) = dismissed {
        let toast = app.toasts.remove(i);
        if actions.contains(&i) {
            match toast.action {
                Some(ToastAction::RestoreStation(station)) => {
                    let effect = app.session.radio.restore(station);
                    app.run(effect);
                }
                Some(ToastAction::AllowMusicFolders) => {
                    super::settings::allow_pending_folders(app);
                }
                None => {}
            }
        }
    }
}
