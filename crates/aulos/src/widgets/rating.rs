// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! A star that opens into a 1–5 rating: hovering it (tapping on touch
//! screens) lays five stars over what follows it, and leaving them folds it
//! back. Used where a full star column has no room (now-playing bar,
//! expanded view).

use super::paint_icon;
use crate::theme;
use egui::{Color32, CornerRadius, Id, Rect, Sense, Ui, pos2, vec2};
use egui_phosphor::{fill, regular};

/// Colours of the stars: `on` for set stars, `off`/`hover` for the others,
/// `backdrop` behind the open row (the surface the widget sits on).
pub struct RatingColors {
    pub on: Color32,
    pub off: Color32,
    pub hover: Color32,
    pub backdrop: Color32,
}

/// Draws the folded star in `rect` (rating `current`, 0 = none) and, while
/// open, the five-star row starting at `rect`'s left. Returns the new rating
/// when a star is clicked (clicking the current one clears it).
pub fn rating_stars(
    ui: &mut Ui,
    id: Id,
    rect: Rect,
    current: u8,
    icon: f32,
    colors: &RatingColors,
) -> Option<u8> {
    let open_id = id.with("open");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    let touch = theme::touch(ui.ctx());
    let step = rect.width();
    let mut row = Rect::from_min_size(rect.min, vec2(step * 5.0, rect.height()));
    // Kept inside the visible area: it opens leftwards near the right edge.
    let overflow = row.right() - ui.clip_rect().right();
    if overflow > 0.0 {
        row = row.translate(vec2(-overflow.min(step * 4.0), 0.0));
    }

    let mut picked = None;
    if open {
        let pad = vec2(4.0, 2.0);
        let area = row.expand2(pad);
        let pointer = ui.ctx().pointer_hover_pos();
        let hovered_star = pointer
            .filter(|p| row.contains(*p))
            .map(|p| (((p.x - row.left()) / step) as u8).min(4) + 1);
        // A foreground layer: the row spills over whatever is drawn after
        // the widget, which must neither cover it nor take its clicks.
        egui::Area::new(id.with("row"))
            .order(egui::Order::Foreground)
            .fixed_pos(area.min)
            .show(ui.ctx(), |ui| {
                // Sizes the area to the row.
                let _ = ui.allocate_exact_size(area.size(), Sense::hover());
                ui.painter()
                    .rect_filled(area, CornerRadius::same(6), colors.backdrop);
                for i in 0..5u8 {
                    let cell = Rect::from_min_size(
                        pos2(row.left() + step * f32::from(i), row.top()),
                        rect.size(),
                    );
                    let star = ui.interact(cell, id.with(("star", i)), Sense::click());
                    let lit = i < hovered_star.unwrap_or(current);
                    let color = match (lit, hovered_star.is_some()) {
                        (true, true) => colors.hover,
                        (true, false) => colors.on,
                        (false, _) => colors.off,
                    };
                    let glyph = if lit { fill::STAR } else { regular::STAR };
                    paint_icon(ui.painter(), cell.center(), glyph, icon, color, lit);
                    if star.clicked() {
                        picked = Some(if current == i + 1 { 0 } else { i + 1 });
                    }
                }
            });
        // Fold after a pick, or once the pointer leaves the row (touch: a
        // tap elsewhere).
        let outside = pointer.is_none_or(|p| !area.contains(p));
        let away = if touch {
            ui.input(|i| i.pointer.any_pressed()) && outside
        } else {
            outside
        };
        if picked.is_some() || away {
            open = false;
        }
    } else {
        let resp = ui
            .interact(rect, id, Sense::click())
            .on_hover_text(aulos_core::fl!("rate-track"));
        let rated = current > 0;
        let color = if rated {
            colors.on
        } else if resp.hovered() {
            colors.hover
        } else {
            colors.off
        };
        let glyph = if rated { fill::STAR } else { regular::STAR };
        paint_icon(ui.painter(), rect.center(), glyph, icon, color, rated);
        if rated {
            // The number of stars, small in the corner.
            ui.painter().text(
                pos2(rect.right() + 1.0, rect.bottom()),
                egui::Align2::RIGHT_BOTTOM,
                current.to_string(),
                theme::semibold(9.0),
                colors.on,
            );
        }
        if resp.hovered() && !touch || resp.clicked() {
            open = true;
        }
    }
    ui.data_mut(|d| d.insert_temp(open_id, open));
    if open {
        ui.ctx().request_repaint();
    }
    picked
}
