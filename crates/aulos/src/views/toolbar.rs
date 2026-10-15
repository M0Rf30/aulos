//! The 56px toolbar: history, search, and the view controls of the library
//! pages.

use crate::app::{App, Route};
use crate::theme::{self, ColorExt, Theme};
use crate::widgets::{IconBtn, paint_icon, paint_text, search_field, segmented};
use aulos_core::config::ViewMode;
use aulos_core::library::album_sort::AlbumSort;
use egui::{Align, CornerRadius, Layout, Rect, Sense, Ui, pos2, vec2};
use egui_phosphor::regular;

pub fn show(app: &mut App, ui: &mut Ui) {
    let t = Theme::get(ui.ctx());
    let rect = ui.max_rect();
    let pad_right = app.page_pad();
    let inner = Rect::from_min_max(
        pos2(rect.left() + 16.0, rect.top()),
        pos2(rect.right() - pad_right, rect.bottom()),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 0.0);

            // History.
            ui.spacing_mut().item_spacing.x = 2.0;
            let back = IconBtn::new(regular::CARET_LEFT)
                .size(28.0)
                .icon_size(15.0)
                .enabled(app.can_go_back())
                .color(t.pal.text_1)
                .tip("Back")
                .show(ui);
            if back.clicked() {
                app.go_back();
            }
            let fwd = IconBtn::new(regular::CARET_RIGHT)
                .size(28.0)
                .icon_size(15.0)
                .enabled(app.can_go_forward())
                .color(t.pal.text_1)
                .tip("Forward")
                .show(ui);
            if fwd.clicked() {
                app.go_forward();
            }
            ui.spacing_mut().item_spacing.x = 10.0;

            // Search: min(320, 42%).
            let width = 320.0_f32.min(inner.width() * 0.42);
            let hint = if app.narrow {
                "Search"
            } else {
                "Search albums, artists, songs"
            };
            let id = app.search_id;
            let resp = search_field(ui, &mut app.search, hint, width, id);
            if resp.changed() {
                let effect = app.session.set_search_query(app.search.clone());
                app.run(effect);
            }

            // View controls on the browsing pages.
            let root = app.route.root();
            let browse = matches!(root, Route::Albums | Route::Artists | Route::Genres);
            let populated = match root {
                Route::Albums => !app.session.library.albums.is_empty(),
                Route::Artists => !app.session.library.artists.is_empty(),
                _ => !app.session.library.genres.is_empty(),
            };
            let detail = matches!(
                app.route,
                Route::Album(_) | Route::Artist(_) | Route::Genre(_)
            );
            if browse && populated && !detail {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let cfg = &mut app.session.config.config;
                    let mode = match root {
                        Route::Albums => &mut cfg.albums_view_mode,
                        Route::Artists => &mut cfg.artists_view_mode,
                        _ => &mut cfg.genres_view_mode,
                    };
                    let mut idx = usize::from(*mode == ViewMode::List);
                    if segmented(ui, &mut idx, &[regular::SQUARES_FOUR, regular::LIST], 26.0) {
                        *mode = if idx == 1 {
                            ViewMode::List
                        } else {
                            ViewMode::Grid
                        };
                        app.session.config.save();
                    }
                    sources_menu(app, ui);
                    sort_menu(app, ui, &root);
                });
            } else if browse && !detail && app.session.providers.provider_list.len() > 1 {
                // An empty library has no view controls, but the way to
                // another source (a server, say) must stay reachable.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    sources_menu(app, ui);
                });
            }
        },
    );
}

/// A toolbar dropdown trigger: icon, label and caret in a 30px hover pill.
fn dropdown_button(ui: &mut Ui, icon: &str, label: &str) -> egui::Response {
    let t = Theme::get(ui.ctx());
    let font = theme::regular(13.0);
    let w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), t.pal.text)
        .size()
        .x
        + 10.0
        + 14.0
        + 6.0
        + 6.0
        + 11.0
        + 10.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), t.pal.text.alpha(0.06));
    }
    paint_icon(
        ui.painter(),
        pos2(rect.left() + 17.0, rect.center().y),
        icon,
        14.0,
        t.pal.text_2,
        false,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_max(
            pos2(rect.left() + 30.0, rect.top()),
            pos2(rect.right() - 24.0, rect.bottom()),
        ),
        Align::Min,
        label,
        font,
        t.pal.text_1,
    );
    paint_icon(
        ui.painter(),
        pos2(rect.right() - 15.0, rect.center().y),
        regular::CARET_DOWN,
        11.0,
        t.pal.text_3,
        false,
    );
    resp
}

/// The sort dropdown of the Albums / Artists / Genres pages.
fn sort_menu(app: &mut App, ui: &mut Ui, root: &Route) {
    let cfg = &app.session.config.config;
    let current = match root {
        Route::Albums => cfg.albums_sort,
        Route::Artists => cfg.artists_sort,
        _ => cfg.genres_sort,
    };
    let label = if app.narrow {
        current.short_label()
    } else {
        current.label()
    };
    let resp = dropdown_button(ui, regular::ARROWS_DOWN_UP, &label);
    let mut chosen = None;
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(150.0);
        for sort in AlbumSort::ALL {
            // Artists and genres have no separate "artist" key: Title is it.
            if sort == AlbumSort::Artist && !matches!(root, Route::Albums) {
                continue;
            }
            if ui.selectable_label(sort == current, sort.label()).clicked() {
                chosen = Some(sort);
                ui.close();
            }
        }
    });
    if let Some(sort) = chosen {
        match root {
            Route::Albums => app.session.set_albums_sort(sort),
            Route::Artists => app.session.set_artists_sort(sort),
            _ => app.session.set_genres_sort(sort),
        }
    }
}

/// The "All sources" dropdown: switches the active provider.
fn sources_menu(app: &mut App, ui: &mut Ui) {
    let providers = &app.session.providers;
    // A single provider is just "all sources"; name the active one only when
    // there is a choice.
    let label = providers
        .active_index
        .filter(|_| providers.provider_list.len() > 1)
        .and_then(|i| providers.provider_list.get(i))
        .map_or("All sources", |(_, name)| name.as_str())
        .to_owned();
    let resp = dropdown_button(ui, regular::HARD_DRIVES, &label);

    let entries: Vec<(usize, String, bool)> = {
        let p = &app.session.providers;
        p.provider_list
            .iter()
            .enumerate()
            .map(|(i, (_, name))| (i, name.clone(), p.active_index == Some(i)))
            .collect()
    };
    let mut chosen = None;
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(180.0);
        for (i, name, active) in &entries {
            let mark = if *active { regular::CHECK } else { "" };
            if ui.button(format!("{name}  {mark}")).clicked() {
                chosen = Some(*i);
                ui.close();
            }
        }
    });
    if let Some(i) = chosen {
        let update = app.session.switch_provider(i);
        app.run(update);
    }
}
