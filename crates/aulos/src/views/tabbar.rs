// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The phone layout's bottom tab bar: four destinations and a "More" menu
//! that holds every other page.

use super::sidebar::{Item, LIBRARY, LISTEN};
use crate::app::{App, Route};
use crate::theme::{self, Theme};
use crate::widgets::{paint_icon, paint_text};
use egui::{Align, CornerRadius, Rect, Sense, Ui, pos2, vec2};
use egui_phosphor::{fill, regular};

/// Indexes into [`LIBRARY`] of the pinned tabs: Home, Albums, Artists, Songs.
const TABS: [usize; 4] = [0, 1, 2, 3];

pub fn show(app: &mut App, ui: &mut Ui) {
    let t = Theme::get(ui.ctx());
    let pal = t.pal;
    let bar = ui.max_rect();
    let current = app.route.root();
    let cell_w = bar.width() / (TABS.len() + 1) as f32;
    let cell = |i: usize| {
        Rect::from_min_size(
            pos2(bar.left() + cell_w * i as f32, bar.top()),
            vec2(cell_w, bar.height()),
        )
    };

    let mut go = None;
    for (i, &idx) in TABS.iter().enumerate() {
        let it = &LIBRARY[idx];
        let rect = cell(i);
        let resp = ui.interact(rect, ui.id().with(("tab", i)), Sense::click());
        paint_tab(ui, rect, it, current == it.route, &t);
        if resp.clicked() {
            go = Some(it.route.clone());
        }
    }

    // "More": everything not pinned, highlighted when the page is one of them.
    let more_rect = cell(TABS.len());
    let more = ui.interact(more_rect, ui.id().with("tab-more"), Sense::click());
    let in_more = !TABS.iter().any(|&i| LIBRARY[i].route == current);
    let fg = if in_more { t.accent.text } else { pal.text_3 };
    paint_icon(
        ui.painter(),
        pos2(more_rect.center().x, more_rect.top() + 20.0),
        regular::LIST,
        22.0,
        fg,
        false,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_size(
            pos2(more_rect.left(), more_rect.top() + 33.0),
            vec2(more_rect.width(), 14.0),
        ),
        Align::Center,
        &aulos_core::fl!("tabbar-more"),
        theme::medium(10.5),
        fg,
    );

    let converter = app.session.config.config.experimental_converter;
    let mut entries: Vec<Item> = vec![
        sheet_item(LIBRARY_GENRES),
        sheet_item(LIBRARY_FOLDERS),
        sheet_item(LIBRARY_PLAYLISTS),
    ];
    entries.extend(LISTEN.iter().map(|i| Item {
        route: i.route.clone(),
        icon: i.icon,
        icon_fill: i.icon_fill,
        label: i.label,
        key: None,
    }));
    entries.push(Item {
        route: Route::Tagger,
        icon: regular::TAG,
        icon_fill: fill::TAG,
        label: || aulos_core::fl!("sidebar-tag-editor"),
        key: None,
    });
    if converter {
        entries.push(Item {
            route: Route::Converter,
            icon: regular::ARROWS_LEFT_RIGHT,
            icon_fill: fill::ARROWS_LEFT_RIGHT,
            label: || aulos_core::fl!("sidebar-converter"),
            key: None,
        });
    }
    entries.push(Item {
        route: Route::Settings,
        icon: regular::GEAR_SIX,
        icon_fill: fill::GEAR_SIX,
        label: || aulos_core::fl!("sidebar-settings"),
        key: None,
    });

    egui::Popup::menu(&more).show(|ui| {
        ui.set_min_width(220.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        for it in &entries {
            let (rect, resp) = ui.allocate_exact_size(
                vec2(ui.available_width().max(220.0), theme::size::TOUCH),
                Sense::click(),
            );
            let selected = current == it.route;
            if selected {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(8), pal.surface_sel);
            } else if resp.hovered() {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(8), pal.hover);
            }
            let color = if selected { pal.text } else { pal.text_1 };
            paint_icon(
                ui.painter(),
                pos2(rect.left() + 22.0, rect.center().y),
                if selected { it.icon_fill } else { it.icon },
                19.0,
                color,
                selected,
            );
            paint_text(
                ui.painter(),
                Rect::from_min_max(pos2(rect.left() + 46.0, rect.top()), rect.right_bottom()),
                Align::Min,
                &(it.label)(),
                theme::medium(14.0),
                color,
            );
            if resp.clicked() {
                go = Some(it.route.clone());
                ui.close();
            }
        }
    });

    if let Some(route) = go
        && app.route != route
    {
        app.navigate(route);
    }
}

const LIBRARY_GENRES: usize = 4;
const LIBRARY_FOLDERS: usize = 5;
const LIBRARY_PLAYLISTS: usize = 6;

fn sheet_item(idx: usize) -> Item {
    let i = &LIBRARY[idx];
    Item {
        route: i.route.clone(),
        icon: i.icon,
        icon_fill: i.icon_fill,
        label: i.label,
        key: None,
    }
}

fn paint_tab(ui: &Ui, rect: Rect, it: &Item, selected: bool, t: &Theme) {
    let fg = if selected {
        t.accent.text
    } else {
        t.pal.text_3
    };
    paint_icon(
        ui.painter(),
        pos2(rect.center().x, rect.top() + 20.0),
        if selected { it.icon_fill } else { it.icon },
        22.0,
        fg,
        selected,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_size(
            pos2(rect.left(), rect.top() + 33.0),
            vec2(rect.width(), 14.0),
        ),
        Align::Center,
        &(it.label)(),
        theme::medium(10.5),
        fg,
    );
}
