//! The main window layout: sidebar | toolbar + page, now-playing bar below,
//! side sheets floating on top.

use super::{now_bar, properties, queue, sidebar, tabbar, toolbar};
use crate::app::App;
use crate::theme::{self, size};
use egui::{Frame, Margin, Panel};

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let pal = app.pal;
    let line = egui::Stroke::new(
        1.0,
        if pal.dark {
            theme::hex(0x22201E)
        } else {
            pal.stroke
        },
    );

    let compact = app.compact();
    if compact {
        // Outermost: the tab bar; the player strip sits right above it.
        Panel::bottom("tab_bar")
            .exact_size(size::TAB_BAR)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(pal.bg_sidebar).inner_margin(Margin::ZERO))
            .show(ui, |ui| {
                let r = ui.max_rect();
                ui.painter().hline(r.x_range(), r.top() + 0.5, line);
                tabbar::show(app, ui);
            });
    }

    Panel::bottom("now_bar")
        .exact_size(if compact {
            size::MINI_BAR
        } else {
            size::NOW_BAR
        })
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new().fill(pal.bg_bar).inner_margin(Margin::ZERO))
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().hline(r.x_range(), r.top() + 0.5, line);
            now_bar::show(app, ui);
        });

    // 180ms collapse; the rail layout takes over at the midpoint.
    let collapse = theme::motion::animate(
        ui.ctx(),
        egui::Id::new("sidebar_collapse"),
        app.narrow() || app.sidebar_collapsed,
        theme::motion::PANEL,
    );
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new("sidebar_rail"), collapse > 0.5));
    if !compact {
        Panel::left("sidebar")
            .exact_size(size::SIDEBAR + (size::RAIL - size::SIDEBAR) * collapse)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(pal.bg_sidebar).inner_margin(Margin::ZERO))
            .show(ui, |ui| {
                let r = ui.max_rect();
                ui.painter().vline(r.right() - 0.5, r.y_range(), line);
                sidebar::show(app, ui);
            });
    }

    let hero_tint: Option<egui::Color32> =
        ui.ctx().data(|d| d.get_temp(egui::Id::new("hero_tint")));
    Panel::top("toolbar")
        .exact_size(size::TOOLBAR)
        .resizable(false)
        .show_separator_line(false)
        .frame(
            Frame::new()
                .fill(hero_tint.unwrap_or(pal.bg))
                .inner_margin(Margin::ZERO),
        )
        .show(ui, |ui| toolbar::show(app, ui));

    egui::CentralPanel::default()
        .frame(Frame::new().fill(pal.bg).inner_margin(Margin::ZERO))
        .show(ui, |ui| super::show_page(app, ui));

    let ctx = ui.ctx().clone();
    queue::show(app, &ctx);
    properties::show(app, &ctx);
}
