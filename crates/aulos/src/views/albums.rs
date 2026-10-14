//! The albums page (grid / list) and the album detail route.

use crate::app::{App, Route};
use crate::theme::{self, ColorExt, Quality, Theme};
use crate::views::artists::scanning_banner;
use crate::views::menus::{self, MenuAction};
use crate::widgets::{
    Columns, PillKind, chip, cover_image, empty_state, keycap, paint_icon, paint_text,
    paint_text_spaced, pill_button, quality_badge, skeleton, text_width, track_header, track_row,
};
use aulos_core::config::ViewMode;
use aulos_core::format::format_duration;
use aulos_core::library::Album;
use aulos_core::library::album_filter::{AlbumFilter, AlbumFilterCache};
use aulos_core::library::quality::AudioQuality;
use aulos_core::library::queue_source::QueueSource;
use aulos_core::library::release_kind::ReleaseKind;
use egui::{
    Align, CornerRadius, Layout, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use egui_phosphor::regular;

const MIN_CARD: f32 = 148.0;
const GRID_GAP_X: f32 = 20.0;
const GRID_GAP_Y: f32 = 24.0;
const CARD_TEXT_H: f32 = 9.0 + 18.0 + 17.0;
const LIST_ROW_H: f32 = 52.0;
const LIST_HEADER_H: f32 = 32.0;

#[derive(Default)]
pub struct State {
    filter: AlbumFilter,
    cache: AlbumFilterCache,
    /// Cached `(cover_key, index in library.albums)` of the open album.
    detail: Option<(String, usize)>,
    /// Selected track id in the album detail.
    selected: Option<i64>,
}

/// Called when the route is entered: start loading what the page needs.
/// The library is loaded at start-up; nothing else to fetch.
pub fn enter(_app: &mut App) {}

/// What the user asked for while the page was drawn; applied afterwards.
enum Action {
    Open(String),
    PlayAlbum { album: usize, start: usize },
    ShuffleAlbum(usize),
    QueueAlbum { album: usize, next: bool },
    Menu(MenuAction),
    Favorite(aulos_core::library::Track),
    Rate { id: String, stars: u8 },
    Select(i64),
    Go(Route),
    ClearSearch,
}

fn apply(app: &mut App, action: Action) {
    match action {
        Action::Open(key) => app.navigate(Route::Album(key)),
        Action::PlayAlbum { album, start } => {
            let u = app.session.play_album(album, start);
            app.run(u);
        }
        Action::ShuffleAlbum(album) => {
            if !app.session.config.config.shuffle {
                let u = app.session.toggle_shuffle();
                app.run(u);
            }
            let u = app.session.play_album(album, 0);
            app.run(u);
        }
        Action::QueueAlbum { album, next } => {
            let u = app
                .session
                .queue_from(QueueSource::Album { album, track: None }, next);
            app.run(u);
            app.toast(if next {
                "Playing next"
            } else {
                "Added to queue"
            });
        }
        Action::Menu(a) => menus::apply(app, a),
        Action::Favorite(track) => app.toggle_favorite(&track),
        Action::Rate { id, stars } => {
            let eff = app.session.set_rating(id, stars);
            app.run(eff);
        }
        Action::Select(id) => app.views.albums.selected = Some(id),
        Action::Go(route) => app.navigate(route),
        Action::ClearSearch => app.search.clear(),
    }
}

/// The name shown for an album; untagged files have none.
fn album_title(name: &str) -> &str {
    if name.trim().is_empty() {
        "Unknown album"
    } else {
        name
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let key = match &app.route {
        Route::Album(key) => Some(key.clone()),
        _ => None,
    };
    let mut actions = Vec::new();
    match key {
        Some(key) => detail(app, ui, &key, &mut actions),
        None => list_page(app, ui, &mut actions),
    }
    for a in actions {
        apply(app, a);
    }
}

fn quality_of(q: AudioQuality) -> Option<Quality> {
    match q {
        AudioQuality::Unknown => None,
        AudioQuality::Lossy => Some(Quality::Lossy),
        AudioQuality::CdLossless => Some(Quality::Cd),
        AudioQuality::HiRes => Some(Quality::HiRes),
        AudioQuality::Dsd => Some(Quality::Dsd),
    }
}

/// Runs `f` in the page's horizontally padded column.
fn padded<R>(ui: &mut Ui, pad: f32, f: impl FnOnce(&mut Ui) -> R) -> R {
    let r = ui.available_rect_before_wrap();
    let inner = Rect::from_min_max(
        pos2(r.left() + pad, r.top()),
        pos2((r.right() - pad).max(r.left() + pad), r.bottom()),
    );
    ui.scope_builder(UiBuilder::new().max_rect(inner), f).inner
}

fn title_row(ui: &mut Ui, title: &str, count: &str) {
    let t = Theme::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(
            egui::RichText::new(title)
                .font(theme::semibold(26.0))
                .color(t.pal.text),
        );
        ui.label(
            egui::RichText::new(count)
                .font(theme::mono(13.0))
                .color(t.pal.text_3),
        );
    });
}

// ---------------------------------------------------------------------------
// Albums page
// ---------------------------------------------------------------------------

fn list_page(app: &mut App, ui: &mut Ui, actions: &mut Vec<Action>) {
    let pad = app.page_pad();
    let total = app.session.library.albums.len();
    let scanning = app.session.library.scanning;
    let searching = !app.search.trim().is_empty();

    ui.add_space(4.0);
    if total == 0 {
        padded(ui, pad, |ui| title_row(ui, "Albums", "0"));
        ui.add_space(16.0);
        if scanning {
            loading_state(app, ui, pad);
        } else {
            padded(ui, pad, |ui| {
                if library_empty_state(ui) {
                    actions.push(Action::Go(Route::Settings));
                }
            });
        }
        return;
    }

    // Chips need the decades present; the cache memoises the derived data.
    let (visible, decades, genres) = {
        let lib = &app.session.library;
        app.views.albums.cache.get(
            lib.visible_epoch(),
            &app.views.albums.filter,
            lib.visible_albums(),
        )
    };
    padded(ui, pad, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        title_row(ui, "Albums", &visible.len().to_string());
        ui.add_space(17.0);
        filter_chips(app, ui, &decades, &genres);
        if searching {
            ui.add_space(10.0);
            search_banner(app, ui, visible.len(), total, actions);
        }
        if scanning {
            ui.add_space(12.0);
            scan_banner(app, ui);
        }
        ui.add_space(9.0);
    });

    if visible.is_empty() {
        padded(ui, pad, |ui| {
            let (title, body, button) = if searching {
                (
                    "No albums match",
                    "Nothing in your library matches this search.",
                    "Clear search",
                )
            } else {
                (
                    "No albums match these filters",
                    "Try a different kind, decade or genre.",
                    "Clear filters",
                )
            };
            if empty_state(ui, regular::VINYL_RECORD, title, body, Some(button)) {
                if searching {
                    actions.push(Action::ClearSearch);
                } else {
                    app.views.albums.filter = AlbumFilter::default();
                }
            }
        });
        return;
    }

    match app.session.config.config.albums_view_mode {
        ViewMode::Grid => grid(app, ui, pad, &visible, actions),
        ViewMode::List => list(app, ui, pad, &visible, actions),
    }
}

fn filter_chips(app: &mut App, ui: &mut Ui, decades: &[u32], genres: &[String]) {
    let pal = app.pal;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        let kinds = [
            ("All", None),
            ("Albums", Some(ReleaseKind::Album)),
            ("EPs", Some(ReleaseKind::Ep)),
            ("Singles", Some(ReleaseKind::Single)),
            ("Compilations", Some(ReleaseKind::Compilation)),
            ("Live", Some(ReleaseKind::Live)),
        ];
        for (label, kind) in kinds {
            if chip(ui, label, app.views.albums.filter.kind == kind, None).clicked() {
                app.views.albums.filter.set_kind(kind);
            }
        }
        let separator = |ui: &mut Ui| {
            let (sep, _) = ui.allocate_exact_size(vec2(9.0, 18.0), Sense::hover());
            ui.painter()
                .vline(sep.center().x, sep.y_range(), Stroke::new(1.0, pal.stroke));
        };
        if !decades.is_empty() {
            separator(ui);
            let current = app.views.albums.filter.decade;
            let label = match current {
                Some(d) => format!("{d}s"),
                None => "Decade".to_owned(),
            };
            let resp = chip(ui, &label, current.is_some(), Some(regular::CARET_DOWN));
            let mut chosen: Option<Option<u32>> = None;
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(120.0);
                if ui
                    .selectable_label(current.is_none(), "Any decade")
                    .clicked()
                {
                    chosen = Some(None);
                    ui.close();
                }
                for &d in decades.iter().rev() {
                    if ui
                        .selectable_label(current == Some(d), format!("{d}s"))
                        .clicked()
                    {
                        chosen = Some(Some(d));
                        ui.close();
                    }
                }
            });
            if let Some(d) = chosen {
                app.views.albums.filter.set_decade(d);
            }
        }
        if !genres.is_empty() {
            let current = app.views.albums.filter.genre.clone();
            let label = current.clone().unwrap_or_else(|| "Genre".to_owned());
            let resp = chip(ui, &label, current.is_some(), Some(regular::CARET_DOWN));
            let mut chosen: Option<Option<String>> = None;
            egui::Popup::menu(&resp).show(|ui| {
                ui.set_min_width(160.0);
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        if ui
                            .selectable_label(current.is_none(), "Any genre")
                            .clicked()
                        {
                            chosen = Some(None);
                            ui.close();
                        }
                        for genre in genres {
                            let on = current
                                .as_deref()
                                .is_some_and(|c| c.eq_ignore_ascii_case(genre));
                            if ui.selectable_label(on, genre).clicked() {
                                chosen = Some(Some(genre.clone()));
                                ui.close();
                            }
                        }
                    });
            });
            if let Some(g) = chosen {
                app.views.albums.filter.set_genre(g);
            }
        }
        let favourites = app.views.albums.filter.favourites;
        if chip(ui, "Favourites", favourites, None).clicked() {
            app.views.albums.filter.set_favourites(!favourites);
        }
    });
}

fn search_banner(app: &App, ui: &mut Ui, matches: usize, total: usize, actions: &mut Vec<Action>) {
    let pal = app.pal;
    let artists = app.session.library.visible_artists().len();
    let songs = app.session.library.visible_tracks().len();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(
            egui::RichText::new(matches.to_string())
                .font(theme::semibold(13.0))
                .color(pal.text),
        );
        ui.label(
            egui::RichText::new(format!("of {total} albums match “{}”", app.search.trim()))
                .font(theme::regular(13.0))
                .color(pal.text_2),
        );
        ui.label(egui::RichText::new("·").color(pal.text_5));
        for (n, noun, route) in [
            (artists, "artist", Route::Artists),
            (songs, "song", Route::Songs),
        ] {
            let text = format!("{n} {noun}{}", if n == 1 { "" } else { "s" });
            if ui
                .add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .font(theme::regular(13.0))
                            .color(pal.text_1)
                            .underline(),
                    )
                    .sense(Sense::click()),
                )
                .clicked()
            {
                actions.push(Action::Go(route));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                egui::RichText::new("clears")
                    .font(theme::regular(13.0))
                    .color(pal.text_2),
            );
            keycap(ui, "Esc");
        });
    });
}

/// The slim "library is being scanned" banner: folder, progress bar and
/// `done / total` counter.
fn scan_banner(app: &App, ui: &mut Ui) {
    let lib = &app.session.library;
    scanning_banner(
        ui,
        &app.pal,
        &lib.scan_progress.snapshot(),
        "Scanning your library…",
    );
}

fn loading_state(app: &App, ui: &mut Ui, pad: f32) {
    padded(ui, pad, |ui| {
        scan_banner(app, ui);
        ui.add_space(16.0);
        let w = ui.available_width();
        let cols = columns_for(w);
        let card_w = card_width(w, cols);
        let origin = ui.available_rect_before_wrap().min;
        let rows = ((ui.available_height() / (card_w + CARD_TEXT_H + GRID_GAP_Y)).ceil() as usize)
            .clamp(2, 6);
        for i in 0..rows * cols {
            let (r, c) = (i / cols, i % cols);
            let x = origin.x + c as f32 * (card_w + GRID_GAP_X);
            let y = origin.y + r as f32 * (card_w + CARD_TEXT_H + GRID_GAP_Y);
            skeleton(ui, Rect::from_min_size(pos2(x, y), vec2(card_w, card_w)), 8);
            skeleton(
                ui,
                Rect::from_min_size(pos2(x, y + card_w + 11.0), vec2(card_w * 0.78, 11.0)),
                4,
            );
            skeleton(
                ui,
                Rect::from_min_size(pos2(x, y + card_w + 30.0), vec2(card_w * 0.52, 10.0)),
                4,
            );
        }
    });
}

/// Design empty state with the accent bars; true when an action was picked.
fn library_empty_state(ui: &mut Ui) -> bool {
    let t = Theme::get(ui.ctx());
    let mut go = false;
    let avail = ui.available_rect_before_wrap();
    ui.allocate_ui_with_layout(vec2(avail.width(), avail.height().max(320.0)), Layout::top_down(Align::Center), |ui| {
        ui.add_space(((avail.height() - 260.0) * 0.4).max(32.0));
        let (rect, _) = ui.allocate_exact_size(vec2(27.0, 52.0), Sense::hover());
        let skew = |r: Rect, color| {
            let s = (-14.0_f32).to_radians().tan();
            let dx = |y: f32| s * (y - rect.center().y);
            let pts = vec![pos2(r.left() + dx(r.top()), r.top()), pos2(r.right() + dx(r.top()), r.top()), pos2(r.right() + dx(r.bottom()), r.bottom()), pos2(r.left() + dx(r.bottom()), r.bottom())];
            ui.painter().add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        };
        skew(Rect::from_min_size(rect.min, vec2(10.0, 52.0)), t.pal.reed);
        skew(Rect::from_min_size(pos2(rect.min.x + 17.0, rect.max.y - 38.0), vec2(10.0, 38.0)), t.pal.surface_hi);
        ui.add_space(24.0);
        ui.label(egui::RichText::new("Nothing on the shelves yet").font(theme::semibold(21.0)).color(t.pal.text));
        ui.add_space(8.0);
        ui.allocate_ui(vec2(440.0_f32.min(avail.width()), 48.0), |ui| {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.label(
                    egui::RichText::new("Add a music folder or connect an MPD or Subsonic server. Scanning runs in the background — play the first album as soon as it lands.")
                        .font(theme::regular(14.0))
                        .color(t.pal.text_2),
                );
            });
        });
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            let w = 170.0 + 190.0 + 8.0;
            ui.add_space(((ui.available_width() - w) * 0.5).max(0.0));
            ui.spacing_mut().item_spacing.x = 8.0;
            go |= pill_button(ui, "Add music folder", Some(regular::FOLDER_PLUS), PillKind::Accent, 34.0, 170.0).clicked();
            go |= pill_button(ui, "Connect a server", Some(regular::PLUGS_CONNECTED), PillKind::Secondary, 34.0, 190.0).clicked();
        });
    });
    go
}

fn columns_for(w: f32) -> usize {
    (((w + GRID_GAP_X) / (MIN_CARD + GRID_GAP_X)).floor() as usize).max(1)
}

fn card_width(w: f32, cols: usize) -> f32 {
    ((w - GRID_GAP_X * (cols as f32 - 1.0)) / cols as f32).max(40.0)
}

fn year_suffix(year: u32) -> String {
    if year > 0 {
        format!(" · {year}")
    } else {
        String::new()
    }
}

fn grid(app: &App, ui: &mut Ui, pad: f32, visible: &[usize], actions: &mut Vec<Action>) {
    let pal = app.pal;
    let accent = app.accent;
    let lib = &app.session.library;
    let albums = lib.visible_albums();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("albums-grid")
        .show_viewport(ui, |ui, viewport| {
            let full_w = ui.available_width();
            let w = (full_w - 2.0 * pad).max(MIN_CARD);
            let cols = columns_for(w);
            let card_w = card_width(w, cols);
            let card_h = card_w + CARD_TEXT_H;
            let stride = card_h + GRID_GAP_Y;
            let rows = visible.len().div_ceil(cols);
            let total_h = rows as f32 * stride - GRID_GAP_Y + 32.0;
            let (area, _) = ui.allocate_exact_size(vec2(full_w, total_h), Sense::hover());
            let first = (viewport.min.y / stride).floor().max(0.0) as usize;
            let last = ((viewport.max.y / stride).ceil() as usize).min(rows);
            for r in first..last {
                for c in 0..cols {
                    let Some(&vi) = visible.get(r * cols + c) else {
                        break;
                    };
                    let Some(album) = albums.get(vi) else {
                        continue;
                    };
                    let x = area.left() + pad + c as f32 * (card_w + GRID_GAP_X);
                    let y = area.top() + r as f32 * stride;
                    let card = Rect::from_min_size(pos2(x, y), vec2(card_w, card_h));
                    let cover = Rect::from_min_size(card.min, vec2(card_w, card_w));
                    let playing = app.now_key.as_deref() == Some(album.cover_key.as_str());
                    let id = ui.id().with(("album-card", &album.cover_key, vi));
                    let resp = ui.interact(card, id, Sense::click());
                    let hovered = resp.contains_pointer();

                    cover_image(
                        ui,
                        &app.covers,
                        cover,
                        app.covers.thumb(&album.cover_key),
                        &album.cover_key,
                        8,
                    );
                    if playing {
                        ui.painter().rect_stroke(
                            cover,
                            CornerRadius::same(8),
                            Stroke::new(2.0, accent.fill),
                            StrokeKind::Inside,
                        );
                    }
                    let play_rect = Rect::from_center_size(
                        pos2(cover.right() - 30.0, cover.bottom() - 30.0),
                        vec2(40.0, 40.0),
                    );
                    let mut play_clicked = false;
                    if hovered {
                        hover_gradient(ui.painter(), cover, 8.0);
                        let pr = ui.interact(play_rect, id.with("play"), Sense::click());
                        let bg = if pr.hovered() {
                            pal.text
                        } else {
                            pal.text.alpha(0.94)
                        };
                        ui.painter().circle_filled(play_rect.center(), 20.0, bg);
                        paint_icon(
                            ui.painter(),
                            play_rect.center(),
                            regular::PLAY,
                            17.0,
                            pal.bg,
                            true,
                        );
                        play_clicked = pr.clicked();
                    }
                    // Hover-only "…" circle (top right) opening the album menu.
                    let more_rect = Rect::from_center_size(
                        pos2(cover.right() - 22.0, cover.top() + 22.0),
                        vec2(28.0, 28.0),
                    );
                    let more = ui.interact(more_rect, id.with("more"), Sense::click());
                    if hovered || more.hovered() {
                        ui.painter().circle_filled(
                            more_rect.center(),
                            14.0,
                            egui::Color32::from_black_alpha(if more.hovered() { 170 } else { 120 }),
                        );
                        paint_icon(
                            ui.painter(),
                            more_rect.center(),
                            regular::DOTS_THREE,
                            17.0,
                            egui::Color32::WHITE,
                            false,
                        );
                    }
                    if playing {
                        let badge = Rect::from_min_size(
                            pos2(cover.left() + 8.0, cover.bottom() - 32.0),
                            vec2(82.0, 24.0),
                        );
                        ui.painter().rect_filled(
                            badge,
                            CornerRadius::same(12),
                            egui::Color32::from_rgba_unmultiplied(15, 14, 13, 204),
                        );
                        paint_icon(
                            ui.painter(),
                            pos2(badge.left() + 15.0, badge.center().y),
                            regular::SPEAKER_HIGH,
                            12.0,
                            accent.fill,
                            false,
                        );
                        paint_text(
                            ui.painter(),
                            Rect::from_min_max(pos2(badge.left() + 27.0, badge.top()), badge.max),
                            Align::Min,
                            "Playing",
                            theme::semibold(11.5),
                            accent.fill,
                        );
                    }

                    let title_rect = Rect::from_min_size(
                        pos2(card.left(), cover.bottom() + 9.0),
                        vec2(card_w, 18.0),
                    );
                    paint_text(
                        ui.painter(),
                        title_rect,
                        Align::Min,
                        album_title(&album.name),
                        theme::semibold(13.5),
                        if playing {
                            accent.text
                        } else if album.name.trim().is_empty() {
                            pal.text_3
                        } else {
                            pal.text
                        },
                    );
                    let quality = quality_of(album.quality);
                    let line = Rect::from_min_size(
                        pos2(card.left(), title_rect.bottom()),
                        vec2(card_w, 17.0),
                    );
                    let text_w = if quality.is_some() {
                        card_w - 46.0
                    } else {
                        card_w
                    };
                    paint_text(
                        ui.painter(),
                        Rect::from_min_size(line.min, vec2(text_w, line.height())),
                        Align::Min,
                        &format!("{}{}", album.artist, year_suffix(album.year)),
                        theme::regular(12.5),
                        pal.text_2,
                    );
                    if let Some(q) = quality {
                        ui.scope_builder(
                            UiBuilder::new()
                                .max_rect(line)
                                .layout(Layout::right_to_left(Align::Center)),
                            |ui| {
                                quality_badge(ui, q);
                            },
                        );
                    }

                    let playlists = &app.session.playlists.playlists;
                    if let Some(m) = menus::album_menu(&resp, album, playlists)
                        .or_else(|| menus::album_menu_click(&more, album, playlists))
                    {
                        actions.push(Action::Menu(m));
                    }
                    if play_clicked {
                        actions.push(Action::PlayAlbum {
                            album: lib.album_index(vi),
                            start: 0,
                        });
                    } else if resp.clicked() {
                        actions.push(Action::Open(album.cover_key.clone()));
                    }
                }
            }
        });
}

struct ListCols {
    cover: Rect,
    name: Rect,
    year: Rect,
    kind: Option<Rect>,
    tracks: Rect,
    length: Rect,
    quality: Rect,
}

fn list_cols(row: Rect, narrow: bool) -> ListCols {
    const GAP: f32 = 14.0;
    let inner = row.shrink2(vec2(10.0, 0.0));
    let kind_w = if narrow { 0.0 } else { 90.0 };
    let fixed = 44.0 + 56.0 + kind_w + 56.0 + 64.0 + 60.0;
    let n = if narrow { 6.0 } else { 7.0 };
    let name_w = (inner.width() - fixed - GAP * (n - 1.0)).max(40.0);
    let mut x = inner.left();
    let mut take = |w: f32| {
        let r = Rect::from_min_size(pos2(x, inner.top()), vec2(w, inner.height()));
        x += w + GAP;
        r
    };
    let cover_slot = take(44.0);
    let cover = Rect::from_center_size(
        pos2(cover_slot.left() + 20.0, cover_slot.center().y),
        vec2(40.0, 40.0),
    );
    let name = take(name_w);
    let year = take(56.0);
    let kind = (!narrow).then(|| take(kind_w));
    let tracks = take(56.0);
    let length = take(64.0);
    let quality = take(60.0);
    ListCols {
        cover,
        name,
        year,
        kind,
        tracks,
        length,
        quality,
    }
}

fn list(app: &App, ui: &mut Ui, pad: f32, visible: &[usize], actions: &mut Vec<Action>) {
    let pal = app.pal;
    let accent = app.accent;
    let narrow = app.narrow;
    let lib = &app.session.library;
    let albums = lib.visible_albums();

    // Column header.
    let (hrect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), LIST_HEADER_H), Sense::hover());
    let hinner = hrect.shrink2(vec2(pad, 0.0));
    let hc = list_cols(hinner, narrow);
    let font = theme::semibold(11.0);
    let cap = |r: Rect, align: Align, text: &str| {
        paint_text_spaced(ui.painter(), r, align, text, font.clone(), pal.text_3, 0.66);
    };
    cap(hc.name, Align::Min, "ALBUM");
    cap(hc.year, Align::Min, "YEAR");
    if let Some(k) = hc.kind {
        cap(k, Align::Min, "KIND");
    }
    cap(hc.tracks, Align::Max, "TRACKS");
    cap(hc.length, Align::Max, "LENGTH");
    cap(hc.quality, Align::Min, "QUALITY");
    ui.painter().hline(
        hinner.x_range(),
        hrect.bottom() - 0.5,
        Stroke::new(1.0, pal.stroke),
    );

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("albums-list")
        .show_viewport(ui, |ui, viewport| {
            let full_w = ui.available_width();
            let total_h = visible.len() as f32 * LIST_ROW_H + 32.0;
            let (area, _) = ui.allocate_exact_size(vec2(full_w, total_h), Sense::hover());
            let first = (viewport.min.y / LIST_ROW_H).floor().max(0.0) as usize;
            let last = ((viewport.max.y / LIST_ROW_H).ceil() as usize + 1).min(visible.len());
            for i in first..last {
                let Some(&vi) = visible.get(i) else { break };
                let Some(album) = albums.get(vi) else {
                    continue;
                };
                let row = Rect::from_min_size(
                    pos2(area.left() + pad, area.top() + i as f32 * LIST_ROW_H),
                    vec2(full_w - 2.0 * pad, LIST_ROW_H),
                );
                let id = ui.id().with(("album-row", &album.cover_key, vi));
                let resp = ui.interact(row, id, Sense::click());
                let playing = app.now_key.as_deref() == Some(album.cover_key.as_str());
                let hovered = resp.contains_pointer();
                let c = list_cols(row, narrow);
                if playing {
                    ui.painter()
                        .rect_filled(row, CornerRadius::same(6), accent.row_tint);
                } else if hovered {
                    ui.painter()
                        .rect_filled(row, CornerRadius::same(6), pal.hover);
                }
                cover_image(
                    ui,
                    &app.covers,
                    c.cover,
                    app.covers.thumb(&album.cover_key),
                    &album.cover_key,
                    4,
                );
                let mut play_clicked = false;
                if hovered {
                    ui.painter().rect_filled(
                        c.cover,
                        CornerRadius::same(4),
                        egui::Color32::BLACK.alpha(0.45),
                    );
                    let pr = ui.interact(c.cover, id.with("play"), Sense::click());
                    paint_icon(
                        ui.painter(),
                        c.cover.center(),
                        regular::PLAY,
                        16.0,
                        egui::Color32::WHITE,
                        true,
                    );
                    play_clicked = pr.clicked();
                }
                let name_top =
                    Rect::from_min_size(c.name.min + vec2(0.0, 9.0), vec2(c.name.width(), 18.0));
                paint_text(
                    ui.painter(),
                    name_top,
                    Align::Min,
                    album_title(&album.name),
                    theme::semibold(13.0),
                    if playing { accent.text } else { pal.text },
                );
                let name_sub =
                    Rect::from_min_size(name_top.left_bottom(), vec2(c.name.width(), 16.0));
                paint_text(
                    ui.painter(),
                    name_sub,
                    Align::Min,
                    &album.artist,
                    theme::regular(12.0),
                    pal.text_2,
                );
                let year = if album.year > 0 {
                    album.year.to_string()
                } else {
                    "—".to_owned()
                };
                paint_text(
                    ui.painter(),
                    c.year,
                    Align::Min,
                    &year,
                    theme::mono(12.0),
                    pal.text_2,
                );
                if let Some(k) = c.kind {
                    paint_text(
                        ui.painter(),
                        k,
                        Align::Min,
                        if album.is_compilation() {
                            "Compilation"
                        } else {
                            "Album"
                        },
                        theme::regular(13.0),
                        pal.text_2,
                    );
                }
                paint_text(
                    ui.painter(),
                    c.tracks,
                    Align::Max,
                    &album.tracks.len().to_string(),
                    theme::mono(12.0),
                    pal.text_2,
                );
                paint_text(
                    ui.painter(),
                    c.length,
                    Align::Max,
                    &format_duration(album.total_secs),
                    theme::mono(12.0),
                    pal.text_2,
                );
                if let Some(q) = quality_of(album.quality) {
                    ui.scope_builder(
                        UiBuilder::new()
                            .max_rect(c.quality)
                            .layout(Layout::left_to_right(Align::Center)),
                        |ui| {
                            quality_badge(ui, q);
                        },
                    );
                }
                if let Some(m) = menus::album_menu(&resp, album, &app.session.playlists.playlists) {
                    actions.push(Action::Menu(m));
                }
                if play_clicked {
                    actions.push(Action::PlayAlbum {
                        album: lib.album_index(vi),
                        start: 0,
                    });
                } else if resp.clicked() {
                    actions.push(Action::Open(album.cover_key.clone()));
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Album detail
// ---------------------------------------------------------------------------

/// Index in `library.albums` of the album with `key` (memoised in the state).
fn find_album(app: &mut App, key: &str) -> Option<usize> {
    let albums = &app.session.library.albums;
    if let Some((k, i)) = &app.views.albums.detail
        && k == key
        && albums.get(*i).is_some_and(|a| a.cover_key == key)
    {
        return Some(*i);
    }
    let idx = albums.iter().position(|a| a.cover_key == key)?;
    app.views.albums.detail = Some((key.to_owned(), idx));
    Some(idx)
}

fn duration_text(secs: u64) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    if h > 0 {
        format!("{h} h {m} min")
    } else {
        format!("{} min", (secs as f64 / 60.0).round() as u64)
    }
}

/// The hover scrim of a grid card: transparent from the top down to 45 % of
/// the cover, then black fading in to 45 % alpha at the bottom, with the
/// bottom corners following the cover's rounding.
fn hover_gradient(painter: &egui::Painter, cover: Rect, radius: f32) {
    use egui::epaint::{Mesh, Vertex, WHITE_UV};
    let top = cover.top() + cover.height() * 0.45;
    let span = cover.bottom() - top;
    let steps = (span / 2.0).ceil().max(1.0) as usize;
    let inset = |y: f32| {
        let dy = y - (cover.bottom() - radius);
        if dy <= 0.0 {
            0.0
        } else {
            radius - (radius * radius - dy * dy).max(0.0).sqrt()
        }
    };
    let color = |y: f32| egui::Color32::BLACK.alpha(0.45 * ((y - top) / span).clamp(0.0, 1.0));
    let mut mesh = Mesh::default();
    for i in 0..=steps {
        let y = (top + span * i as f32 / steps as f32).min(cover.bottom());
        let d = inset(y);
        let c = color(y);
        for x in [cover.left() + d, cover.right() - d] {
            mesh.vertices.push(Vertex {
                pos: pos2(x, y),
                uv: WHITE_UV,
                color: c,
            });
        }
    }
    for i in 0..steps as u32 {
        let k = i * 2;
        mesh.indices
            .extend_from_slice(&[k, k + 1, k + 2, k + 1, k + 3, k + 2]);
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn detail(app: &mut App, ui: &mut Ui, key: &str, actions: &mut Vec<Action>) {
    let pad = app.page_pad();
    ui.add_space(4.0);
    let Some(idx) = find_album(app, key) else {
        padded(ui, pad, |ui| {
            let scanning = app.session.library.scanning;
            let (title, body) = if scanning {
                ("Loading album…", "The library is still being scanned.")
            } else {
                ("Album not found", "It is no longer in your library.")
            };
            if empty_state(
                ui,
                regular::VINYL_RECORD,
                title,
                body,
                Some("Back to albums"),
            ) {
                actions.push(Action::Go(Route::Albums));
            }
        });
        return;
    };

    let pal = app.pal;
    let narrow = app.narrow;
    let lib = &app.session.library;
    let Some(album) = lib.albums.get(idx) else {
        return;
    };
    let current_id = app.session.playback.current_track.as_ref().map(|t| t.id);
    let selected = app.views.albums.selected;
    let playlists = &app.session.playlists.playlists;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("album-detail")
        .show(ui, |ui| {
            padded(ui, pad, |ui| {
                hero(app, ui, album, idx, actions);
                ui.add_space(24.0);

                let cols = Columns::album_detail(narrow);
                track_header(ui, cols, "#");
                let multi_disc = album.tracks.iter().any(|t| t.disc_number > 1);
                let mut last_disc = None;
                for (i, track) in album.tracks.iter().enumerate() {
                    if multi_disc && last_disc != Some(track.disc_number) {
                        last_disc = Some(track.disc_number);
                        let (r, _) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
                        paint_icon(
                            ui.painter(),
                            pos2(r.left() + 20.0, r.center().y),
                            regular::VINYL_RECORD,
                            14.0,
                            pal.text_3,
                            false,
                        );
                        let label = format!("Disc {}", track.disc_number.max(1));
                        paint_text(
                            ui.painter(),
                            Rect::from_min_max(pos2(r.left() + 36.0, r.top()), r.max),
                            Align::Min,
                            &label,
                            theme::semibold(12.5),
                            pal.text_2,
                        );
                    }
                    let number = if track.track_number > 0 {
                        track.track_number.to_string()
                    } else {
                        (i + 1).to_string()
                    };
                    let current = current_id == Some(track.id);
                    let out = track_row(
                        ui,
                        app,
                        track,
                        &number,
                        current,
                        selected == Some(track.id),
                        cols,
                    );
                    if out.response.double_clicked() {
                        actions.push(Action::PlayAlbum {
                            album: idx,
                            start: i,
                        });
                    } else if out.response.clicked() {
                        actions.push(Action::Select(track.id));
                    }
                    if out.toggle_favorite {
                        actions.push(Action::Favorite(track.clone()));
                    }
                    if let Some(stars) = out.set_rating {
                        actions.push(Action::Rate {
                            id: track.id.to_string(),
                            stars,
                        });
                    }
                    if let Some(m) = menus::track_menu(&out.response, track, playlists)
                        .or_else(|| menus::track_menu_click(&out.more, track, playlists))
                    {
                        actions.push(Action::Menu(m));
                    }
                }
                ui.add_space(32.0);
            });
        });
}

fn hero(app: &App, ui: &mut Ui, album: &Album, idx: usize, actions: &mut Vec<Action>) {
    let pal = app.pal;
    let accent = app.accent;
    let size = if app.narrow { 150.0 } else { 184.0 };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size), Sense::hover());
    let cover = Rect::from_min_size(rect.min, vec2(size, size));
    ui.painter().rect_filled(
        cover.translate(vec2(0.0, 10.0)).shrink(6.0),
        CornerRadius::same(12),
        egui::Color32::BLACK.alpha(0.28),
    );
    cover_image(
        ui,
        &app.covers,
        cover,
        app.covers.thumb(&album.cover_key),
        &album.cover_key,
        8,
    );
    let cover_resp = ui.interact(cover, ui.id().with("album-hero-cover"), Sense::click());
    if let Some(m) = menus::album_menu(&cover_resp, album, &app.session.playlists.playlists) {
        actions.push(Action::Menu(m));
    }

    let x = cover.right() + 24.0;
    let w = (rect.right() - x).max(60.0);
    let b = rect.bottom();
    let btn_top = b - 34.0;
    let meta = Rect::from_min_size(pos2(x, btn_top - 12.0 - 18.0), vec2(w, 18.0));
    let artist = Rect::from_min_size(pos2(x, meta.top() - 2.0 - 20.0), vec2(w, 20.0));
    let title = Rect::from_min_size(pos2(x, artist.top() - 2.0 - 44.0), vec2(w, 44.0));
    let kind = Rect::from_min_size(pos2(x, title.top() - 18.0), vec2(w, 16.0));

    paint_text_spaced(
        ui.painter(),
        kind,
        Align::Min,
        if album.is_compilation() {
            "COMPILATION"
        } else {
            "ALBUM"
        },
        theme::semibold(11.0),
        pal.text_3,
        0.66,
    );
    paint_text(
        ui.painter(),
        title,
        Align::Min,
        album_title(&album.name),
        theme::bold(if app.narrow { 28.0 } else { 34.0 }),
        pal.text,
    );

    // Artist (opens the artist page when it is a real artist).
    let aw = text_width(ui, &album.artist, theme::medium(15.0)).min(w);
    let artist_hit = Rect::from_min_size(artist.min, vec2(aw, artist.height()));
    let ar = ui.interact(
        artist_hit,
        ui.id().with("album-hero-artist"),
        Sense::click(),
    );
    paint_text(
        ui.painter(),
        artist,
        Align::Min,
        &album.artist,
        theme::medium(15.0),
        if ar.hovered() {
            accent.text
        } else {
            pal.text_15
        },
    );
    if ar.clicked() && !album.artist.is_empty() {
        actions.push(Action::Go(Route::Artist(album.artist.clone())));
    }

    // Meta: year · tracks · duration (+ quality).
    let n = album.tracks.len();
    let mut parts = Vec::new();
    if album.year > 0 {
        parts.push(album.year.to_string());
    }
    parts.push(format!("{n} song{}", if n == 1 { "" } else { "s" }));
    parts.push(duration_text(album.total_secs));
    let meta_text = parts.join(" · ");
    let mw = paint_text(
        ui.painter(),
        meta,
        Align::Min,
        &meta_text,
        theme::regular(13.0),
        pal.text_2,
    );
    if let Some(q) = quality_of(album.quality) {
        let r = Rect::from_min_max(
            pos2(meta.left() + mw + 10.0, meta.top()),
            pos2(rect.right(), meta.bottom()),
        );
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(r)
                .layout(Layout::left_to_right(Align::Center)),
            |ui| {
                quality_badge(ui, q);
            },
        );
    }

    // Actions.
    let brect = Rect::from_min_max(pos2(x, btn_top), pos2(rect.right(), b));
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(brect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if pill_button(
                ui,
                "Play",
                Some(regular::PLAY),
                PillKind::Primary,
                34.0,
                0.0,
            )
            .clicked()
            {
                actions.push(Action::PlayAlbum {
                    album: idx,
                    start: 0,
                });
            }
            if pill_button(
                ui,
                "Shuffle",
                Some(regular::SHUFFLE),
                PillKind::Secondary,
                34.0,
                0.0,
            )
            .clicked()
            {
                actions.push(Action::ShuffleAlbum(idx));
            }
            if pill_button(
                ui,
                "Play next",
                Some(regular::QUEUE),
                PillKind::Secondary,
                34.0,
                0.0,
            )
            .clicked()
            {
                actions.push(Action::QueueAlbum {
                    album: idx,
                    next: true,
                });
            }
            if pill_button(
                ui,
                "Add to queue",
                Some(regular::LIST_PLUS),
                PillKind::Secondary,
                34.0,
                0.0,
            )
            .clicked()
            {
                actions.push(Action::QueueAlbum {
                    album: idx,
                    next: false,
                });
            }
        },
    );
}
