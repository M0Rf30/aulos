//! The navigation sidebar: 224px wide, or a 64px icon rail when narrow.

use crate::app::{App, Route};
use crate::theme::{self, ColorExt, Palette, Theme};
use crate::widgets::{paint_icon, paint_text};
use egui::{Align, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use egui_phosphor::{fill, regular};

pub(super) struct Item {
    pub(super) route: Route,
    pub(super) icon: &'static str,
    pub(super) icon_fill: &'static str,
    pub(super) label: fn() -> String,
    pub(super) key: Option<&'static str>,
}

pub(super) const LIBRARY: [Item; 7] = [
    Item {
        route: Route::Home,
        icon: regular::HOUSE,
        icon_fill: fill::HOUSE,
        label: || aulos_core::fl!("sidebar-home"),
        key: Some("1"),
    },
    Item {
        route: Route::Albums,
        icon: regular::VINYL_RECORD,
        icon_fill: fill::VINYL_RECORD,
        label: || aulos_core::fl!("sidebar-albums"),
        key: Some("2"),
    },
    Item {
        route: Route::Artists,
        icon: regular::MICROPHONE,
        icon_fill: fill::MICROPHONE,
        label: || aulos_core::fl!("sidebar-artists"),
        key: Some("3"),
    },
    Item {
        route: Route::Songs,
        icon: regular::MUSIC_NOTES,
        icon_fill: fill::MUSIC_NOTES,
        label: || aulos_core::fl!("sidebar-songs"),
        key: Some("4"),
    },
    Item {
        route: Route::Genres,
        icon: regular::SHAPES,
        icon_fill: fill::SHAPES,
        label: || aulos_core::fl!("sidebar-genres"),
        key: Some("5"),
    },
    Item {
        route: Route::Folders,
        icon: regular::FOLDER_SIMPLE,
        icon_fill: fill::FOLDER_SIMPLE,
        label: || aulos_core::fl!("sidebar-folders"),
        key: Some("6"),
    },
    Item {
        route: Route::Playlists,
        icon: regular::PLAYLIST,
        icon_fill: fill::PLAYLIST,
        label: || aulos_core::fl!("sidebar-playlists"),
        key: Some("7"),
    },
];

pub(super) const LISTEN: [Item; 2] = [
    Item {
        route: Route::Podcasts,
        icon: regular::MICROPHONE_STAGE,
        icon_fill: fill::MICROPHONE_STAGE,
        label: || aulos_core::fl!("sidebar-podcasts"),
        key: Some("8"),
    },
    Item {
        route: Route::Radio,
        icon: regular::RADIO,
        icon_fill: fill::RADIO,
        label: || aulos_core::fl!("sidebar-radio"),
        key: None,
    },
];

/// A wide nav row: icon, label, optional keycap; `h` px high.
fn wide_row(
    ui: &mut Ui,
    pal: &Palette,
    it: &Item,
    selected: bool,
    h: f32,
    icon_px: f32,
    trailing: Option<&str>,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), pal.surface_sel);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), pal.text.alpha(0.05));
    }
    let fg = if selected {
        pal.text
    } else if resp.hovered() {
        pal.text_1
    } else {
        pal.text_2
    };
    let (glyph, filled) = if selected {
        (it.icon_fill, true)
    } else {
        (it.icon, false)
    };
    paint_icon(
        ui.painter(),
        pos2(rect.left() + 10.0 + icon_px / 2.0, rect.center().y),
        glyph,
        icon_px,
        fg,
        filled,
    );
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 10.0 + icon_px + 10.0, rect.top()),
        pos2(rect.right() - 10.0, rect.bottom()),
    );
    paint_text(
        ui.painter(),
        text_rect,
        Align::Min,
        &(it.label)(),
        theme::medium(13.0),
        fg,
    );
    if let Some(k) = trailing {
        paint_text(
            ui.painter(),
            rect.shrink2(vec2(10.0, 0.0)),
            Align::Max,
            k,
            theme::mono(11.0),
            pal.text_4,
        );
    }
    resp
}

fn rail_row(ui: &mut Ui, pal: &Palette, it: &Item, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 36.0), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), pal.surface_sel);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), pal.text.alpha(0.05));
    }
    let fg = if selected { pal.text } else { pal.text_2 };
    let (glyph, filled) = if selected {
        (it.icon_fill, true)
    } else {
        (it.icon, false)
    };
    paint_icon(ui.painter(), rect.center(), glyph, 18.0, fg, filled);
    resp.on_hover_text((it.label)())
}

fn logo(ui: &Ui, origin: egui::Pos2, accent: egui::Color32, ink: egui::Color32) {
    // Two bars skewed by -14deg, bottom-aligned: 4x18 accent and 4x13 ink.
    let skew = (-14.0_f32).to_radians().tan();
    let bar = |x: f32, h: f32, color: egui::Color32| {
        let (w, base) = (4.0, origin.y + 18.0);
        let top = base - h;
        // Skew around the bar's vertical centre line of the 18px box.
        let dx = |y: f32| skew * (y - (origin.y + 9.0));
        let pts = vec![
            pos2(x + dx(top), top),
            pos2(x + w + dx(top), top),
            pos2(x + w + dx(base), base),
            pos2(x + dx(base), base),
        ];
        ui.painter()
            .add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    };
    bar(origin.x, 18.0, accent);
    bar(origin.x + 7.0, 13.0, ink);
}

fn goto(app: &mut App, route: &Route) {
    if app.route != *route {
        app.navigate(route.clone());
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let t = Theme::get(ui.ctx());
    let pal = t.pal;
    let outer = ui.max_rect();
    let current = app.route.root();
    let converter = app.session.config.config.experimental_converter;

    let rail_mode: bool = ui
        .ctx()
        .data(|d| d.get_temp(egui::Id::new("sidebar_rail")))
        .unwrap_or(app.narrow() || app.sidebar_collapsed);
    if rail_mode {
        rail(app, ui, &pal);
        return;
    }

    let inner = Rect::from_min_max(outer.min + vec2(10.0, 12.0), outer.max - vec2(11.0, 12.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 1.0);

        // Logo row.
        let (row, _) = ui.allocate_exact_size(vec2(inner.width(), 40.0), Sense::hover());
        logo(
            ui,
            pos2(row.left() + 14.0, row.center().y - 9.0),
            t.accent.fill,
            pal.text,
        );
        paint_text(
            ui.painter(),
            Rect::from_min_size(pos2(row.left() + 10.0 + 22.0, row.top()), vec2(80.0, 40.0)),
            Align::Min,
            "aulos",
            theme::semibold(17.0),
            pal.text,
        );
        let collapse = ui
            .interact(
                Rect::from_center_size(pos2(row.right() - 18.0, row.center().y), vec2(28.0, 28.0)),
                ui.id().with("collapse"),
                Sense::click(),
            )
            .on_hover_text(aulos_core::fl!("sidebar-collapse"));
        paint_icon(
            ui.painter(),
            pos2(row.right() - 18.0, row.center().y),
            regular::SIDEBAR_SIMPLE,
            16.0,
            if collapse.hovered() {
                pal.text
            } else {
                pal.text_3
            },
            false,
        );
        if collapse.clicked() {
            app.sidebar_collapsed = true;
        }
        ui.add_space(4.0);

        // The navigation scrolls in the space above the pinned footer
        // (tools, separator, Settings), so Settings is always visible.
        let converter_rows = f32::from(u8::from(converter));
        let footer_h = 30.0 * (1.0 + converter_rows) + 8.0 + 1.0 + 8.0 + 34.0 + 6.0;
        let nav_h = (ui.available_height() - footer_h).max(0.0);
        let (nav_rect, _) = ui.allocate_exact_size(vec2(inner.width(), nav_h), Sense::hover());
        ui.scope_builder(egui::UiBuilder::new().max_rect(nav_rect), |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .id_salt("sidebar-nav")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 1.0);
                    section_label(ui, &aulos_core::fl!("sidebar-section-library"), 10.0, false);
                    for it in &LIBRARY {
                        if wide_row(ui, &pal, it, current == it.route, 32.0, 17.0, it.key).clicked()
                        {
                            goto(app, &it.route);
                        }
                    }
                    section_label(ui, &aulos_core::fl!("sidebar-section-listen"), 16.0, false);
                    for it in &LISTEN {
                        if wide_row(ui, &pal, it, current == it.route, 32.0, 17.0, None).clicked() {
                            goto(app, &it.route);
                        }
                    }
                    if section_label(
                        ui,
                        &aulos_core::fl!("sidebar-section-playlists"),
                        16.0,
                        true,
                    ) {
                        crate::views::playlists::begin_create(app);
                    }
                    playlist_list(app, ui, &pal);
                });
        });

        // Footer: tools and settings.
        let tool = |ui: &mut Ui,
                    route: Route,
                    icon: &'static str,
                    icon_fill: &'static str,
                    label: fn() -> String,
                    lab: bool| {
            let selected = current == route;
            let it = Item {
                route: route.clone(),
                icon,
                icon_fill,
                label,
                key: None,
            };
            // Unselected tool rows are dimmer (text_3) than the nav rows.
            let tool_pal = Palette {
                text_2: pal.text_3,
                ..pal
            };
            let resp = wide_row(ui, &tool_pal, &it, selected, 30.0, 15.0, None);
            if lab {
                let r = Rect::from_center_size(
                    pos2(resp.rect.right() - 10.0 - 14.0, resp.rect.center().y),
                    vec2(28.0, 15.0),
                );
                ui.painter().rect_stroke(
                    r,
                    CornerRadius::same(3),
                    Stroke::new(1.0, pal.stroke_strong),
                    StrokeKind::Inside,
                );
                paint_text(
                    ui.painter(),
                    r,
                    Align::Center,
                    &aulos_core::fl!("sidebar-lab-badge"),
                    theme::mono_semibold(9.5),
                    pal.text_3,
                );
            }
            resp.clicked().then_some(route)
        };
        let mut go = tool(
            ui,
            Route::Tagger,
            regular::TAG,
            fill::TAG,
            || aulos_core::fl!("sidebar-tag-editor"),
            false,
        );
        if converter {
            go = tool(
                ui,
                Route::Converter,
                regular::ARROWS_LEFT_RIGHT,
                fill::ARROWS_LEFT_RIGHT,
                || aulos_core::fl!("sidebar-converter"),
                true,
            )
            .or(go);
        }
        ui.add_space(8.0);
        let (sep, _) = ui.allocate_exact_size(vec2(inner.width(), 1.0), Sense::hover());
        ui.painter().rect_filled(
            sep.shrink2(vec2(4.0, 0.0)),
            0.0,
            if pal.dark {
                theme::hex(0x22201E)
            } else {
                pal.stroke
            },
        );
        ui.add_space(8.0);
        let settings = Item {
            route: Route::Settings,
            icon: regular::GEAR_SIX,
            icon_fill: fill::GEAR_SIX,
            label: || aulos_core::fl!("sidebar-settings"),
            key: None,
        };
        let resp = wide_row(
            ui,
            &pal,
            &settings,
            current == Route::Settings,
            34.0,
            16.0,
            None,
        );
        status_dots(app, ui, &pal, resp.rect, false);
        if resp.clicked() {
            go = Some(Route::Settings);
        }
        if let Some(route) = go {
            goto(app, &route);
        }
    });
}

fn section_label(ui: &mut Ui, text: &str, top: f32, plus: bool) -> bool {
    let t = Theme::get(ui.ctx());
    // Design: padding `top 10 7`, 10.5px line box; the text sits in the line box.
    let (full, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), top + 10.5 + 7.0), Sense::hover());
    let rect = Rect::from_min_size(full.min + vec2(0.0, top), vec2(full.width(), 10.5));
    let font = theme::semibold(10.5);
    crate::widgets::paint_text_spaced(
        ui.painter(),
        rect.shrink2(vec2(10.0, 0.0)),
        Align::Min,
        text,
        font,
        t.pal.text_3,
        0.84,
    );
    let mut clicked = false;
    if plus {
        let c = pos2(rect.right() - 17.0, rect.center().y);
        let hit = Rect::from_center_size(c, vec2(20.0, 18.0));
        let r = ui.interact(hit, ui.id().with(text), Sense::click());
        paint_icon(
            ui.painter(),
            c,
            regular::PLUS,
            14.0,
            if r.hovered() {
                t.pal.text
            } else {
                t.pal.text_3
            },
            false,
        );
        clicked = r
            .on_hover_text(aulos_core::fl!("sidebar-new-playlist"))
            .clicked();
    }
    clicked
}

fn playlist_list(app: &mut App, ui: &mut Ui, pal: &Palette) {
    let mut open: Option<(bool, String)> = None;
    let on_page = app.route == Route::Playlists;
    let pl = &app.session.playlists;
    for p in &pl.playlists {
        let selected = on_page && crate::views::playlists::is_open(app, false, &p.id);
        if list_row(ui, pal, regular::PLAYLIST, &p.name, selected).clicked() {
            open = Some((false, p.id.clone()));
        }
    }
    for p in &pl.smart_playlists {
        let selected = on_page && crate::views::playlists::is_open(app, true, &p.id.to_string());
        if list_row(ui, pal, fill::SPARKLE, &p.name, selected).clicked() {
            open = Some((true, p.id.to_string()));
        }
    }
    if let Some((smart, id)) = open {
        goto(app, &Route::Playlists);
        crate::views::playlists::open(app, smart, &id);
    }
}

fn list_row(ui: &mut Ui, pal: &Palette, icon: &str, label: &str, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), pal.surface_sel);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), pal.text.alpha(0.05));
    }
    let fg = if selected || resp.hovered() {
        pal.text
    } else {
        pal.text_2
    };
    paint_icon(
        ui.painter(),
        pos2(rect.left() + 10.0 + 7.5, rect.center().y),
        icon,
        15.0,
        fg,
        false,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_max(
            pos2(rect.left() + 35.0, rect.top()),
            pos2(rect.right() - 10.0, rect.bottom()),
        ),
        Align::Min,
        label,
        theme::regular(13.0),
        fg,
    );
    resp
}

/// Provider status dots: one per configured provider (active one green).
fn status_dots(app: &App, ui: &Ui, pal: &Palette, rect: Rect, corner: bool) {
    let mut statuses = app.session.providers.statuses();
    // Three dots at most; failing providers win a slot over healthy ones.
    statuses.sort_by_key(|s| s.error.is_none());
    statuses.truncate(3);
    let n = statuses.len();
    let mut x = if corner {
        rect.right() - 8.0 - 3.5
    } else {
        rect.right() - 10.0 - 3.5
    };
    let y = if corner {
        rect.top() + 11.0
    } else {
        rect.center().y
    };
    let right = x + 3.5;
    for status in statuses.iter().rev() {
        let color = if status.error.is_some() {
            pal.danger
        } else if status.active {
            pal.ok
        } else {
            pal.text_5
        };
        if corner {
            ui.painter().circle_filled(pos2(x, y), 5.5, pal.bg_sidebar);
        }
        ui.painter().circle_filled(pos2(x, y), 3.5, color);
        x -= 11.0;
    }
    if n > 0 {
        let strip = Rect::from_min_max(pos2(x + 11.0 - 3.5, y - 6.0), pos2(right, y + 6.0));
        let tip = statuses
            .iter()
            .map(|s| match &s.error {
                Some(e) => format!("{}: {e}", s.name),
                None => s.name.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        ui.interact(strip, ui.id().with("provider-dots"), Sense::hover())
            .on_hover_text(tip);
    }
}

fn rail(app: &mut App, ui: &mut Ui, pal: &Palette) {
    let t = Theme::get(ui.ctx());
    let outer = ui.max_rect();
    let current = app.route.root();
    let inner = Rect::from_min_max(outer.min + vec2(0.0, 12.0), outer.max - vec2(1.0, 12.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
            let (row, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
            logo(
                ui,
                pos2(row.center().x - 5.5, row.center().y - 9.0),
                t.accent.fill,
                pal.text,
            );
            if !app.narrow() {
                let resp = ui
                    .interact(row, ui.id().with("expand"), Sense::click())
                    .on_hover_text(aulos_core::fl!("sidebar-expand"));
                if resp.clicked() {
                    app.sidebar_collapsed = false;
                }
            }
            ui.add_space(6.0);
            let mut go = None;
            // Pages scroll above the pinned Settings (tools are not in the rail).
            let footer_h = 38.0 + 8.0;
            let nav_h = (ui.available_height() - footer_h).max(0.0);
            let (nav_rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), nav_h), Sense::hover());
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(nav_rect)
                    .layout(egui::Layout::top_down(Align::Center)),
                |ui| {
                    let area = egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .id_salt("rail-nav")
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
                            for it in &LIBRARY {
                                if rail_row(ui, pal, it, current == it.route).clicked() {
                                    go = Some(it.route.clone());
                                }
                            }
                            ui.add_space(8.0);
                            let (sep, _) = ui.allocate_exact_size(vec2(24.0, 1.0), Sense::hover());
                            ui.painter().rect_filled(
                                sep,
                                0.0,
                                if pal.dark {
                                    theme::hex(0x22201E)
                                } else {
                                    pal.stroke
                                },
                            );
                            ui.add_space(8.0);
                            for it in &LISTEN {
                                if rail_row(ui, pal, it, current == it.route).clicked() {
                                    go = Some(it.route.clone());
                                }
                            }
                        });
                    // Rows cut by the viewport fade out so scrolling is evident.
                    let top_hidden = area.state.offset.y > 1.0;
                    let bottom_hidden =
                        area.state.offset.y + area.inner_rect.height() < area.content_size.y - 1.0;
                    scroll_fades(
                        ui,
                        area.inner_rect,
                        pal.bg_sidebar,
                        top_hidden,
                        bottom_hidden,
                    );
                },
            );
            // Settings at the bottom.
            let settings = Item {
                route: Route::Settings,
                icon: regular::GEAR_SIX,
                icon_fill: fill::GEAR_SIX,
                label: || aulos_core::fl!("sidebar-settings"),
                key: None,
            };
            let resp = rail_row(ui, pal, &settings, current == Route::Settings);
            let active_ok = app.session.providers.active_index.is_some();
            let dot = pos2(resp.rect.right() - 8.0 - 3.5, resp.rect.top() + 7.0 + 3.5);
            ui.painter().circle_filled(dot, 5.5, pal.bg_sidebar);
            ui.painter()
                .circle_filled(dot, 3.5, if active_ok { pal.ok } else { pal.warn });
            if resp.clicked() {
                go = Some(Route::Settings);
            }
            if let Some(route) = go {
                goto(app, &route);
            }
        },
    );
}

/// Vertical `bg` fades over the top and bottom edges of `rect`.
fn scroll_fades(ui: &Ui, rect: Rect, bg: egui::Color32, top: bool, bottom: bool) {
    use egui::epaint::{Mesh, Vertex};
    const H: f32 = 18.0;
    let mut mesh = Mesh::default();
    let mut band = |y0: f32, y1: f32, c0: egui::Color32, c1: egui::Color32| {
        let i = mesh.vertices.len() as u32;
        for (x, y, c) in [
            (rect.left(), y0, c0),
            (rect.right(), y0, c0),
            (rect.right(), y1, c1),
            (rect.left(), y1, c1),
        ] {
            mesh.vertices.push(Vertex {
                pos: pos2(x, y),
                uv: egui::epaint::WHITE_UV,
                color: c,
            });
        }
        mesh.indices.extend([i, i + 1, i + 2, i, i + 2, i + 3]);
    };
    if top {
        band(rect.top(), rect.top() + H, bg, egui::Color32::TRANSPARENT);
    }
    if bottom {
        band(
            rect.bottom() - H,
            rect.bottom(),
            egui::Color32::TRANSPARENT,
            bg,
        );
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}
