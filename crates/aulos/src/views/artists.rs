//! The artists page: a grid / list of artists, and the artist detail route.

use crate::app::{App, Route};
use crate::theme::{self, ColorExt, Palette, lerp_color};
use crate::views::menus::{self, MenuAction};
use crate::widgets::{
    IconBtn, PillKind, avatar_tile, chip, cover_image, empty_state, label_text, paint_icon,
    paint_text, pill_button, quality_badge, seed_colors, skeleton,
};
use aulos_core::config::ViewMode;
use aulos_core::format::{format_duration, format_duration_coarse};
use aulos_core::library::display::bio_preview;
use aulos_core::library::{Artist, Track};
use aulos_core::provider::ProviderType;
use egui::{
    Align, Color32, CornerRadius, Frame, Layout, Margin, Painter, Rect, Response, RichText, Sense,
    Stroke, Ui, UiBuilder, Vec2, pos2, vec2,
};
use egui_phosphor::{fill, regular};
use std::ops::Range;

/// Height of the tinted hero gradient on detail pages.
pub(crate) const HERO_HEIGHT: f32 = 274.0;
/// Height of a cover track row (genre and artist detail).
pub(crate) const COVER_ROW_HEIGHT: f32 = 44.0;

#[derive(Default)]
pub struct State {
    /// Library generation the photo/bio fetch was last requested for.
    info_epoch: Option<u64>,
    /// The "Favourites" chip is on.
    favourites_only: bool,
    /// `(library generation, search epoch)` the favourite rows were built for.
    fav_key: Option<u64>,
    /// Positions in `visible_artists()` of artists with a favourite track.
    fav_rows: Vec<usize>,
    /// The biography is shown in full.
    bio_expanded: bool,
    /// Artist the detail state belongs to.
    detail: String,
    /// Derived data of the artist shown in the detail route.
    top: Option<TopTracks>,
    /// Selected top-track row (track id).
    selected: Option<i64>,
}

/// The cached "Top tracks" block and summary of the open artist.
struct TopTracks {
    name: String,
    epoch: u64,
    title: &'static str,
    tracks: Vec<Track>,
    genres: String,
}

/// Called when the route is entered: start loading what the page needs.
pub fn enter(app: &mut App) {
    match app.route.clone() {
        Route::Artist(name) => {
            let effect = app.session.fetch_artist_info_for(&name);
            app.run(effect);
        }
        _ => {
            app.views.artists.info_epoch = Some(app.session.library.generation());
            let effect = app.session.fetch_artist_info_for_visible();
            app.run(effect);
        }
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    match app.route.clone() {
        Route::Artist(name) => detail(app, ui, &name),
        _ => browse(app, ui),
    }
}

// -- shared helpers (also used by the genres page) -----------------------------

/// The page title (26px semibold) with a 13px mono count next to it.
pub(crate) fn page_title(ui: &mut Ui, pal: &Palette, title: &str, count: &str) {
    let title_galley =
        ui.painter()
            .layout_no_wrap(title.to_owned(), theme::semibold(26.0), pal.text);
    let count_galley = ui
        .painter()
        .layout_no_wrap(count.to_owned(), theme::mono(13.0), pal.text_3);
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), title_galley.size().y),
        Sense::hover(),
    );
    let title_w = title_galley.size().x;
    let count_pos = pos2(
        rect.left() + title_w + 10.0,
        rect.bottom() - count_galley.size().y - 5.0,
    );
    ui.painter().galley(rect.min, title_galley, pal.text);
    ui.painter().galley(count_pos, count_galley, pal.text_3);
}

/// Rows of a `stride`-high list starting at screen `top` that intersect the
/// visible clip rect (one row of slack on each side is not needed: rows are
/// clipped by the painter).
pub(crate) fn visible_rows(ui: &Ui, top: f32, stride: f32, rows: usize) -> Range<usize> {
    let clip = ui.clip_rect();
    let first = ((clip.top() - top) / stride).floor().max(0.0) as usize;
    let last = (((clip.bottom() - top) / stride).ceil().max(0.0) as usize).min(rows);
    first.min(rows)..last
}

/// Fills `rect` with a vertical gradient.
/// Tells the toolbar to tint itself like the hero gradient behind it, while the
/// page is scrolled to the top.
pub(crate) fn announce_hero_tint(ui: &Ui, origin: egui::Pos2, tint: Color32) {
    if origin.y >= ui.clip_rect().top() - 0.5 {
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::new("hero_tint_next"), tint));
    }
}

pub(crate) fn vertical_gradient(painter: &Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// The "Scanning…" strip of the loading state (indeterminate: the scanner
/// reports no totals).
pub(crate) fn scanning_banner(ui: &mut Ui, pal: &Palette, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(8), pal.surface_1);
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, pal.stroke),
        egui::StrokeKind::Inside,
    );
    paint_icon(
        &painter,
        pos2(rect.left() + 12.0 + 8.0, rect.center().y),
        regular::ARROWS_CLOCKWISE,
        15.0,
        pal.reed,
        false,
    );
    let text_w = paint_text(
        &painter,
        Rect::from_min_max(
            pos2(rect.left() + 40.0, rect.top()),
            pos2(rect.right() - 12.0, rect.bottom()),
        ),
        Align::Min,
        text,
        theme::regular(13.0),
        pal.text_1,
    );
    let bar_left = rect.left() + 40.0 + text_w + 12.0;
    let bar_right = rect.right() - 12.0;
    if bar_right - bar_left > 24.0 {
        let bar = Rect::from_min_max(
            pos2(bar_left, rect.center().y - 2.0),
            pos2(bar_right, rect.center().y + 2.0),
        );
        painter.rect_filled(bar, CornerRadius::same(2), pal.track);
        let phase = (ui.input(|i| i.time) * 0.8).fract() as f32;
        let seg = bar.width() * 0.25;
        let x0 = bar.left() + (bar.width() + seg) * phase - seg;
        let seg_rect = Rect::from_min_max(
            pos2(x0.max(bar.left()), bar.top()),
            pos2((x0 + seg).min(bar.right()), bar.bottom()),
        );
        if seg_rect.width() > 0.0 {
            painter.rect_filled(seg_rect, CornerRadius::same(2), pal.reed);
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// What a cover track row shows.
pub(crate) struct CoverRow<'a> {
    pub track: &'a Track,
    /// Text of the first column (position).
    pub number: &'a str,
    /// Second line under the title.
    pub subtitle: &'a str,
    /// Show the album as its own column.
    pub album_col: bool,
    pub current: bool,
    pub selected: bool,
}

/// One 44px row with the cover (genre and artist detail): number, cover,
/// title + subtitle, album, quality badge, duration, painted into `rect`.
pub(crate) fn cover_track_row(
    ui: &mut Ui,
    app: &App,
    rect: Rect,
    row: CoverRow<'_>,
) -> (Response, Option<MenuAction>) {
    let pal = app.pal;
    let accent = app.accent;
    let track = row.track;
    let resp = ui.interact(
        rect,
        ui.id().with(("cover_row", track.id, row.number)),
        Sense::click(),
    );
    let painter = ui.painter().clone();
    let radius = CornerRadius::same(6);
    if row.current {
        painter.rect_filled(rect, radius, accent.row_tint);
    } else if row.selected {
        painter.rect_filled(rect, radius, pal.row_sel);
    } else if resp.hovered() {
        painter.rect_filled(rect, radius, pal.hover);
    }

    let gap = if row.album_col { 12.0 } else { 10.0 };
    let inner = rect.shrink2(vec2(8.0, 0.0));
    let cy = inner.center().y;
    let mut x = inner.left();

    let num = Rect::from_min_size(pos2(x, inner.top()), vec2(24.0, inner.height()));
    x += 24.0 + gap;
    if row.current {
        paint_icon(
            &painter,
            num.right_center() - vec2(6.0, 0.0),
            fill::MUSIC_NOTES_SIMPLE,
            13.0,
            accent.fill,
            true,
        );
    } else if resp.hovered() {
        paint_icon(
            &painter,
            num.right_center() - vec2(6.0, 0.0),
            fill::PLAY,
            12.0,
            pal.text,
            true,
        );
    } else {
        paint_text(
            &painter,
            num,
            Align::Max,
            row.number,
            theme::mono(12.0),
            pal.text_3,
        );
    }

    let cover = Rect::from_min_size(pos2(x, cy - 18.0), vec2(36.0, 36.0));
    x += 36.0 + gap;
    let key = track.cover_key();
    cover_image(ui, &app.covers, cover, app.covers.thumb(&key), &key, 4);

    let items = if row.album_col { 4.0 } else { 3.0 };
    let avail = (inner.right() - x - 54.0 - 44.0 - gap * (items - 1.0)).max(0.0);
    let title_w = if row.album_col {
        avail * 1.4 / 2.4
    } else {
        avail
    };
    let title_rect = Rect::from_min_size(pos2(x, cy - 17.0), vec2(title_w, 17.0));
    let sub_rect = Rect::from_min_size(pos2(x, cy + 1.0), vec2(title_w, 15.0));
    let title_color = if row.current { accent.text } else { pal.text };
    paint_text(
        &painter,
        title_rect,
        Align::Min,
        &track.title,
        theme::semibold(13.0),
        title_color,
    );
    paint_text(
        &painter,
        sub_rect,
        Align::Min,
        row.subtitle,
        theme::regular(12.0),
        pal.text_2,
    );
    x += title_w + gap;
    if row.album_col {
        let album_w = avail / 2.4;
        let album = Rect::from_min_size(pos2(x, inner.top()), vec2(album_w, inner.height()));
        paint_text(
            &painter,
            album,
            Align::Min,
            &track.album,
            theme::regular(13.0),
            pal.text_2,
        );
        x += album_w + gap;
    }
    if let Some(q) = App::quality_of(track) {
        let badge = Rect::from_min_size(pos2(x, cy - 7.5), vec2(54.0, 15.0));
        ui.scope_builder(UiBuilder::new().max_rect(badge), |ui| {
            quality_badge(ui, q);
        });
    }
    let dur = Rect::from_min_size(
        pos2(inner.right() - 44.0, inner.top()),
        vec2(44.0, inner.height()),
    );
    paint_text(
        &painter,
        dur,
        Align::Max,
        &format_duration(track.duration.as_secs()),
        theme::mono(12.0),
        pal.text_2,
    );

    let action = menus::track_menu(&resp, track, &app.session.playlists.playlists);
    (resp, action)
}

/// Whether artist photos / biographies can be fetched for the active source.
fn online_info_enabled(app: &App) -> bool {
    app.session.config.config.fetch_artist_info
        || app
            .session
            .providers
            .registry
            .active_shared()
            .is_some_and(|p| p.provider_type() == ProviderType::Subsonic)
}

// -- browse (grid / list) ---------------------------------------------------------

#[derive(Default)]
struct BrowseOut {
    open: Option<String>,
    action: Option<MenuAction>,
    split: Option<bool>,
    toggle_favourites: bool,
    settings: bool,
}

/// Rebuilds the favourite-artist rows when the library or filter changed.
fn refresh_favourites(app: &mut App) {
    let state = &mut app.views.artists;
    if !state.favourites_only {
        return;
    }
    let lib = &app.session.library;
    let key = lib.visible_epoch();
    if state.fav_key == Some(key) {
        return;
    }
    state.fav_key = Some(key);
    state.fav_rows.clear();
    for (i, artist) in lib.visible_artists().iter().enumerate() {
        if artist
            .albums
            .iter()
            .any(|a| a.tracks.iter().any(|t| t.is_favorite))
        {
            state.fav_rows.push(i);
        }
    }
}

fn nth<'a>(artists: &'a [Artist], rows: Option<&[usize]>, i: usize) -> &'a Artist {
    match rows {
        Some(rows) => &artists[rows[i]],
        None => &artists[i],
    }
}

fn browse(app: &mut App, ui: &mut Ui) {
    let epoch = app.session.library.generation();
    if app.views.artists.info_epoch != Some(epoch) {
        app.views.artists.info_epoch = Some(epoch);
        let effect = app.session.fetch_artist_info_for_visible();
        app.run(effect);
    }
    refresh_favourites(app);

    let pad = app.page_pad();
    let list = app.session.config.config.artists_view_mode == ViewMode::List;
    let mut out = BrowseOut::default();
    {
        let a: &App = app;
        let pal = a.pal;
        let lib = &a.session.library;
        let artists = lib.visible_artists();
        let rows = a
            .views
            .artists
            .favourites_only
            .then_some(a.views.artists.fav_rows.as_slice());
        let count = rows.map_or(artists.len(), <[usize]>::len);
        let split = a.session.config.config.split_artist_tags;
        let favourites_only = a.views.artists.favourites_only;
        let online = online_info_enabled(a);

        egui::ScrollArea::vertical()
            .id_salt("artists_scroll")
            .auto_shrink([false, false])
            .content_margin(Margin::ZERO)
            .show(ui, |ui| {
                ui.add_space(4.0);
                Frame::NONE.inner_margin(Margin::symmetric(pad as i8, 0)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    page_title(ui, &pal, "Artists", &count.to_string());
                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if chip(ui, "Album artists", !split, None).clicked() && split {
                            out.split = Some(false);
                        }
                        if chip(ui, "All credited", split, None).clicked() && !split {
                            out.split = Some(true);
                        }
                        if chip(ui, "Favourites", favourites_only, None).clicked() {
                            out.toggle_favourites = true;
                        }
                    });
                    ui.add_space(16.0);

                    if lib.scanning && lib.artists.is_empty() {
                        scanning_banner(ui, &pal, "Scanning your library…");
                        ui.add_space(16.0);
                        skeleton_grid(ui);
                    } else if count == 0 {
                        empty(ui, a, favourites_only, &mut out);
                    } else if list {
                        list_body(ui, a, artists, rows, count, &mut out);
                    } else {
                        grid_body(ui, a, artists, rows, count, &mut out);
                    }

                    if count > 0 && !online {
                        ui.add_space(24.0);
                        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
                        paint_text(
                            ui.painter(),
                            rect,
                            Align::Center,
                            "Photos appear when Online data › Artist photos is on. Until then artists get initials on a tint.",
                            theme::regular(12.0),
                            pal.text_3,
                        );
                    }
                    ui.add_space(32.0);
                });
            });
    }

    if let Some(split) = out.split {
        app.session.set_split_artist_tags(split);
        app.views.artists.fav_key = None;
    }
    if out.toggle_favourites {
        let state = &mut app.views.artists;
        state.favourites_only = !state.favourites_only;
        state.fav_key = None;
    }
    if let Some(name) = out.open {
        app.navigate(Route::Artist(name));
    }
    if let Some(action) = out.action {
        menus::apply(app, action);
    }
    if out.settings {
        app.navigate(Route::Settings);
    }
}

/// Empty / no-result state of the artists page.
fn empty(ui: &mut Ui, a: &App, favourites_only: bool, out: &mut BrowseOut) {
    let query = a.session.library.search_query().trim().to_owned();
    if !query.is_empty() {
        empty_state(
            ui,
            regular::MICROPHONE,
            "No matching artists",
            &format!("Nothing in your artists matches “{query}”."),
            None,
        );
    } else if favourites_only {
        empty_state(
            ui,
            regular::HEART,
            "No favourite artists",
            "Heart a few songs and the artists behind them show up here.",
            None,
        );
    } else if empty_state(
        ui,
        regular::MICROPHONE,
        "No artists yet",
        "Add a music folder or connect an MPD or Subsonic server. Artists appear as soon as the first files are scanned.",
        Some("Open Settings"),
    ) {
        out.settings = true;
    }
}

/// Loading placeholder: circles with two text bars, fading down the page.
fn skeleton_grid(ui: &mut Ui) {
    let w = ui.available_width();
    let cols = (((w + 20.0) / (132.0 + 20.0)).floor() as usize).max(1);
    let cw = (w - 20.0 * (cols as f32 - 1.0)) / cols as f32;
    let stride = cw + 44.0 + 26.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, stride * 2.0 - 26.0), Sense::hover());
    for r in 0..2 {
        for c in 0..cols {
            let x = rect.left() + c as f32 * (cw + 20.0);
            let y = rect.top() + r as f32 * stride;
            skeleton(ui, Rect::from_min_size(pos2(x, y), vec2(cw, cw)), 255);
            skeleton(
                ui,
                Rect::from_center_size(pos2(x + cw / 2.0, y + cw + 19.0), vec2(cw * 0.7, 11.0)),
                4,
            );
            skeleton(
                ui,
                Rect::from_center_size(pos2(x + cw / 2.0, y + cw + 36.0), vec2(cw * 0.45, 10.0)),
                4,
            );
        }
    }
}

fn grid_body(
    ui: &mut Ui,
    a: &App,
    artists: &[Artist],
    rows: Option<&[usize]>,
    count: usize,
    out: &mut BrowseOut,
) {
    let pal = a.pal;
    let accent = a.accent;
    let (min_w, gap_x, gap_y) = (132.0, 20.0, 26.0);
    let w = ui.available_width();
    let cols = (((w + gap_x) / (min_w + gap_x)).floor() as usize).max(1);
    let cw = (w - gap_x * (cols as f32 - 1.0)) / cols as f32;
    let cell_h = cw + 10.0 + 18.0 + 16.0;
    let stride = cell_h + gap_y;
    let grid_rows = count.div_ceil(cols);
    let (rect, _) = ui.allocate_exact_size(
        vec2(w, (grid_rows as f32 * stride - gap_y).max(0.0)),
        Sense::hover(),
    );
    let base_id = ui.id();
    let painter = ui.painter().clone();

    for r in visible_rows(ui, rect.top(), stride, grid_rows) {
        for c in 0..cols {
            let i = r * cols + c;
            if i >= count {
                break;
            }
            let artist = nth(artists, rows, i);
            let x = rect.left() + c as f32 * (cw + gap_x);
            let y = rect.top() + r as f32 * stride;
            let cell = Rect::from_min_size(pos2(x, y), vec2(cw, cell_h));
            let resp = ui.interact(cell, base_id.with(("artist", i)), Sense::click());
            let circle = Rect::from_min_size(cell.min, vec2(cw, cw));
            avatar_tile(
                ui,
                &a.covers,
                circle,
                &artist.name,
                a.covers.photo(&artist.name),
                true,
            );
            if resp.hovered() {
                painter.circle_stroke(
                    circle.center(),
                    cw / 2.0 + 3.0,
                    Stroke::new(2.0, accent.fill),
                );
            }
            let name_rect = Rect::from_min_size(pos2(x, y + cw + 10.0), vec2(cw, 18.0));
            let meta_rect = Rect::from_min_size(pos2(x, y + cw + 28.0), vec2(cw, 16.0));
            paint_text(
                &painter,
                name_rect,
                Align::Center,
                &artist.name,
                theme::semibold(13.0),
                pal.text,
            );
            paint_text(
                &painter,
                meta_rect,
                Align::Center,
                &artist.albums_label(),
                theme::regular(12.0),
                pal.text_2,
            );
            if resp.clicked() {
                out.open = Some(artist.name.clone());
            }
            if let Some(action) = menus::artist_menu(&resp, &artist.name) {
                out.action = Some(action);
            }
        }
    }
}

fn list_body(
    ui: &mut Ui,
    a: &App,
    artists: &[Artist],
    rows: Option<&[usize]>,
    count: usize,
    out: &mut BrowseOut,
) {
    const ROW_H: f32 = 56.0;
    const GAP: f32 = 14.0;
    const ALBUMS_W: f32 = 64.0;
    const SONGS_W: f32 = 64.0;
    const LENGTH_W: f32 = 64.0;
    const CARET_W: f32 = 16.0;
    let pal = a.pal;
    let w = ui.available_width();

    // Column header.
    let (head, _) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
    let painter = ui.painter().clone();
    let cols_right = |inner_right: f32| {
        let caret_l = inner_right - CARET_W;
        let length_l = caret_l - GAP - LENGTH_W;
        let songs_l = length_l - GAP - SONGS_W;
        let albums_l = songs_l - GAP - ALBUMS_W;
        (albums_l, songs_l, length_l)
    };
    let (albums_l, songs_l, length_l) = cols_right(head.right() - 10.0);
    let label = |left: f32, width: f32, align: Align, text: &str| {
        let r = Rect::from_min_size(pos2(left, head.top()), vec2(width, head.height()));
        crate::widgets::paint_text_spaced(
            &painter,
            r,
            align,
            text,
            theme::semibold(11.0),
            pal.text_3,
            0.66,
        );
    };
    label(
        head.left() + 10.0 + 40.0 + GAP,
        (albums_l - GAP - head.left() - 64.0).max(0.0),
        Align::Min,
        "ARTIST",
    );
    label(albums_l, ALBUMS_W, Align::Max, "ALBUMS");
    label(songs_l, SONGS_W, Align::Max, "SONGS");
    label(length_l, LENGTH_W, Align::Max, "LENGTH");
    painter.hline(
        head.x_range(),
        head.bottom() - 0.5,
        Stroke::new(1.0, pal.stroke),
    );

    let (rect, _) = ui.allocate_exact_size(vec2(w, count as f32 * ROW_H), Sense::hover());
    let base_id = ui.id();
    for i in visible_rows(ui, rect.top(), ROW_H, count) {
        let artist = nth(artists, rows, i);
        let row = Rect::from_min_size(
            pos2(rect.left(), rect.top() + i as f32 * ROW_H),
            vec2(w, ROW_H),
        );
        let resp = ui.interact(row, base_id.with(("artist_row", i)), Sense::click());
        if resp.hovered() {
            painter.rect_filled(row, CornerRadius::same(6), pal.hover);
        }
        let avatar = Rect::from_min_size(
            pos2(row.left() + 10.0, row.center().y - 20.0),
            vec2(40.0, 40.0),
        );
        avatar_tile(
            ui,
            &a.covers,
            avatar,
            &artist.name,
            a.covers.photo(&artist.name),
            true,
        );
        let (albums_l, songs_l, length_l) = cols_right(row.right() - 10.0);
        let name_rect = Rect::from_min_max(
            pos2(avatar.right() + GAP, row.top()),
            pos2(albums_l - GAP, row.bottom()),
        );
        paint_text(
            &painter,
            name_rect,
            Align::Min,
            &artist.name,
            theme::semibold(13.0),
            pal.text,
        );
        let cell = |left: f32, width: f32, text: &str| {
            let r = Rect::from_min_size(pos2(left, row.top()), vec2(width, ROW_H));
            paint_text(&painter, r, Align::Max, text, theme::mono(12.0), pal.text_2);
        };
        cell(albums_l, ALBUMS_W, &artist.album_count().to_string());
        cell(songs_l, SONGS_W, &artist.track_count().to_string());
        cell(
            length_l,
            LENGTH_W,
            &format_duration_coarse(artist.total_secs),
        );
        paint_icon(
            &painter,
            pos2(row.right() - 10.0 - CARET_W / 2.0, row.center().y),
            regular::CARET_RIGHT,
            13.0,
            pal.text_4,
            false,
        );
        if resp.clicked() {
            out.open = Some(artist.name.clone());
        }
        if let Some(action) = menus::artist_menu(&resp, &artist.name) {
            out.action = Some(action);
        }
    }
}

// -- detail ----------------------------------------------------------------------------

#[derive(Default)]
struct DetailOut {
    play: bool,
    shuffle: bool,
    play_track: Option<i64>,
    select: Option<i64>,
    toggle_bio: bool,
    open_album: Option<String>,
    play_album: Option<usize>,
    action: Option<MenuAction>,
}

/// (Re)builds the top-tracks block when the artist or the library changed.
fn refresh_top(app: &mut App, idx: usize) {
    let epoch = app.session.library.generation();
    let artist = &app.session.library.artists[idx];
    if app
        .views
        .artists
        .top
        .as_ref()
        .is_some_and(|t| t.name == artist.name && t.epoch == epoch)
    {
        return;
    }
    let all = || artist.albums.iter().flat_map(|album| album.tracks.iter());
    let mut ranked: Vec<&Track> = all()
        .filter(|t| t.rating.is_some() || t.is_favorite)
        .collect();
    let (title, tracks): (&'static str, Vec<Track>) = if ranked.is_empty() {
        ("Songs", all().take(5).cloned().collect())
    } else {
        ranked.sort_by(|a, b| {
            b.rating
                .unwrap_or(0)
                .cmp(&a.rating.unwrap_or(0))
                .then(b.is_favorite.cmp(&a.is_favorite))
        });
        ("Top tracks", ranked.into_iter().take(5).cloned().collect())
    };
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for track in all() {
        if track.genre.is_empty() {
            continue;
        }
        match counts
            .iter_mut()
            .find(|(g, _)| g.eq_ignore_ascii_case(&track.genre))
        {
            Some(entry) => entry.1 += 1,
            None => counts.push((&track.genre, 1)),
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    let genres = counts
        .iter()
        .take(3)
        .map(|(g, _)| *g)
        .collect::<Vec<_>>()
        .join(", ");
    app.views.artists.top = Some(TopTracks {
        name: artist.name.clone(),
        epoch,
        title,
        tracks,
        genres,
    });
}

fn artist_tracks(app: &App, idx: usize) -> Vec<Track> {
    app.session
        .library
        .artists
        .get(idx)
        .map(|artist| {
            artist
                .albums
                .iter()
                .flat_map(|album| album.tracks.iter().cloned())
                .collect()
        })
        .unwrap_or_default()
}

fn detail(app: &mut App, ui: &mut Ui, name: &str) {
    if app.views.artists.detail != name {
        let state = &mut app.views.artists;
        state.detail = name.to_owned();
        state.bio_expanded = false;
        state.selected = None;
        state.top = None;
    }
    let Some(idx) = app.session.library.find_artist(name) else {
        let pal = app.pal;
        let pad = app.page_pad();
        let lib = &app.session.library;
        if lib.scanning || (lib.artists.is_empty() && lib.albums.is_empty()) {
            ui.add_space(4.0);
            Frame::NONE
                .inner_margin(Margin::symmetric(pad as i8, 0))
                .show(ui, |ui| {
                    scanning_banner(ui, &pal, "Loading your library…");
                });
        } else if empty_state(
            ui,
            regular::MICROPHONE,
            "Artist not found",
            &format!("“{name}” is not in your library any more."),
            Some("All artists"),
        ) {
            app.navigate(Route::Artists);
        }
        return;
    };
    refresh_top(app, idx);

    let pad = app.page_pad();
    let mut out = DetailOut::default();
    {
        let a: &App = app;
        let pal = a.pal;
        let artist = &a.session.library.artists[idx];
        egui::ScrollArea::vertical()
            .id_salt("artist_detail")
            .auto_shrink([false, false])
            .content_margin(Margin::ZERO)
            .show(ui, |ui| {
                let origin = ui.cursor().min;
                let tint = lerp_color(pal.bg, seed_colors(&artist.name).0, 0.45);
                vertical_gradient(
                    ui.painter(),
                    Rect::from_min_size(origin, vec2(ui.available_width(), HERO_HEIGHT)),
                    tint,
                    pal.bg,
                );
                announce_hero_tint(ui, origin, tint);
                ui.add_space(4.0);
                Frame::NONE
                    .inner_margin(Margin::symmetric(pad as i8, 0))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        hero(ui, a, artist, &mut out);
                        ui.add_space(26.0);
                        let wide = ui.available_width() >= 320.0 * 2.0 + 36.0;
                        if wide {
                            ui.spacing_mut().item_spacing.x = 36.0;
                            ui.columns(2, |cols| {
                                top_tracks(&mut cols[0], a, &mut out);
                                about(&mut cols[1], a, &artist.name, &mut out);
                            });
                            ui.spacing_mut().item_spacing.x = 0.0;
                        } else {
                            top_tracks(ui, a, &mut out);
                            ui.add_space(28.0);
                            about(ui, a, &artist.name, &mut out);
                        }
                        if !artist.albums.is_empty() {
                            ui.add_space(26.0);
                            discography(ui, a, artist, &mut out);
                        }
                        ui.add_space(32.0);
                    });
            });
    }
    if out.toggle_bio {
        let state = &mut app.views.artists;
        state.bio_expanded = !state.bio_expanded;
    }
    if out.select.is_some() {
        app.views.artists.selected = out.select;
    }
    if out.play {
        let tracks = artist_tracks(app, idx);
        let update = app.session.play_tracks(tracks, 0);
        app.run(update);
    }
    if out.shuffle {
        let tracks = artist_tracks(app, idx);
        if !tracks.is_empty() {
            if !app.session.config.config.shuffle {
                let update = app.session.toggle_shuffle();
                app.run(update);
            }
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as usize)
                .unwrap_or(0);
            let start = nanos % tracks.len();
            let update = app.session.play_tracks(tracks, start);
            app.run(update);
        }
    }
    if let Some(id) = out.play_track {
        let tracks = artist_tracks(app, idx);
        let start = tracks.iter().position(|t| t.id == id).unwrap_or(0);
        let update = app.session.play_tracks(tracks, start);
        app.run(update);
    }
    if let Some(album) = out.play_album {
        let update = app.session.play_artist_album(idx, album, 0);
        app.run(update);
    }
    if let Some(key) = out.open_album {
        app.navigate(Route::Album(key));
    }
    if let Some(action) = out.action {
        menus::apply(app, action);
    }
}

/// The photo, name, summary and Play / Shuffle buttons.
fn hero(ui: &mut Ui, a: &App, artist: &Artist, out: &mut DetailOut) {
    let pal = a.pal;
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 144.0), Sense::hover());
    let photo_rect = Rect::from_min_size(rect.min, vec2(144.0, 144.0));
    ui.painter().add(
        egui::Shadow {
            offset: [0, 14],
            blur: 40,
            spread: 0,
            color: Color32::BLACK.alpha(0.45),
        }
        .as_shape(photo_rect, CornerRadius::same(72)),
    );
    avatar_tile(
        ui,
        &a.covers,
        photo_rect,
        &artist.name,
        a.covers.photo(&artist.name),
        true,
    );

    let genres = a
        .views
        .artists
        .top
        .as_ref()
        .map(|t| t.genres.as_str())
        .unwrap_or("");
    let mut summary = format!(
        "{} · {} · {}",
        plural(artist.album_count(), "album"),
        plural(artist.track_count(), "song"),
        format_duration_coarse(artist.total_secs)
    );
    if !genres.is_empty() {
        summary.push_str(" · ");
        summary.push_str(genres);
    }

    let block = Rect::from_min_max(pos2(rect.left() + 168.0, rect.top()), rect.right_bottom());
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(block)
            .layout(Layout::bottom_up(Align::Min)),
        |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.add_space(4.0);
            ui.horizontal(|ui| {
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
                    out.play = true;
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
                    out.shuffle = true;
                }
                let more = IconBtn::new(regular::DOTS_THREE)
                    .size(34.0)
                    .icon_size(17.0)
                    .round(true)
                    .tip("More")
                    .show(ui);
                if let Some(action) = menus::artist_menu(&more, &artist.name) {
                    out.action = Some(action);
                }
            });
            ui.add_space(14.0);
            label_text(ui, &summary, theme::regular(13.0), pal.text_15);
            ui.add_space(8.0);
            label_text(ui, &artist.name, theme::bold(42.0), pal.text);
        },
    );
}

fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

fn top_tracks(ui: &mut Ui, a: &App, out: &mut DetailOut) {
    ui.spacing_mut().item_spacing = Vec2::ZERO;
    let Some(top) = a.views.artists.top.as_ref() else {
        return;
    };
    if top.tracks.is_empty() {
        return;
    }
    label_text(ui, top.title, theme::semibold(17.0), a.pal.text);
    ui.add_space(10.0);
    let current = a.session.playback.current_track.as_ref().map(|t| t.id);
    for (i, track) in top.tracks.iter().enumerate() {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), COVER_ROW_HEIGHT), Sense::hover());
        let number = (i + 1).to_string();
        let (resp, action) = cover_track_row(
            ui,
            a,
            rect,
            CoverRow {
                track,
                number: &number,
                subtitle: &track.album,
                album_col: false,
                current: current == Some(track.id),
                selected: a.views.artists.selected == Some(track.id),
            },
        );
        if resp.double_clicked() {
            out.play_track = Some(track.id);
        } else if resp.clicked() {
            out.select = Some(track.id);
        }
        if let Some(action) = action {
            out.action = Some(action);
        }
        ui.add_space(6.0);
    }
}

fn about(ui: &mut Ui, a: &App, name: &str, out: &mut DetailOut) {
    ui.spacing_mut().item_spacing = Vec2::ZERO;
    let pal = a.pal;
    label_text(ui, "About", theme::semibold(17.0), pal.text);
    ui.add_space(10.0);
    let online = online_info_enabled(a);
    let bio = a
        .session
        .artist_info
        .bios
        .get(name)
        .map(String::as_str)
        .filter(|b| !b.trim().is_empty());
    if let Some(bio) = bio {
        let expanded = a.views.artists.bio_expanded;
        let preview = bio_preview(bio, expanded);
        ui.label(
            RichText::new(preview.text)
                .font(theme::regular(13.5))
                .color(pal.text_15)
                .line_height(Some(21.6)),
        );
        if preview.is_long {
            ui.add_space(6.0);
            let label = if expanded { "Show less" } else { "Show more" };
            let link = ui.add(
                egui::Label::new(
                    RichText::new(label)
                        .font(theme::medium(12.5))
                        .color(a.accent.text),
                )
                .sense(Sense::click()),
            );
            if link.clicked() {
                out.toggle_bio = true;
            }
        }
    } else if a.session.artist_info.pending.contains(name) {
        let w = ui.available_width();
        for frac in [1.0, 0.92, 0.6] {
            let (rect, _) = ui.allocate_exact_size(vec2(w * frac, 11.0), Sense::hover());
            skeleton(ui, rect, 4);
            ui.add_space(8.0);
        }
    } else if online {
        ui.label(
            RichText::new("No biography found for this artist.")
                .font(theme::regular(13.0))
                .color(pal.text_2),
        );
    } else {
        ui.label(
            RichText::new("Online data is opt-in · Settings › Online data")
                .font(theme::regular(12.0))
                .color(pal.text_3),
        );
    }
}

fn discography(ui: &mut Ui, a: &App, artist: &Artist, out: &mut DetailOut) {
    let pal = a.pal;
    label_text(ui, "Albums", theme::semibold(17.0), pal.text);
    ui.add_space(12.0);
    let (card_w, gap_x, gap_y) = (148.0, 20.0, 24.0);
    let cell_h = card_w + 8.0 + 18.0 + 16.0;
    let stride = cell_h + gap_y;
    let w = ui.available_width();
    let cols = (((w + gap_x) / (card_w + gap_x)).floor() as usize).max(1);
    let n = artist.albums.len();
    let grid_rows = n.div_ceil(cols);
    let (rect, _) = ui.allocate_exact_size(
        vec2(w, (grid_rows as f32 * stride - gap_y).max(0.0)),
        Sense::hover(),
    );
    let base_id = ui.id();
    let painter = ui.painter().clone();
    for r in visible_rows(ui, rect.top(), stride, grid_rows) {
        for c in 0..cols {
            let i = r * cols + c;
            if i >= n {
                break;
            }
            let album = &artist.albums[i];
            let x = rect.left() + c as f32 * (card_w + gap_x);
            let y = rect.top() + r as f32 * stride;
            let cell = Rect::from_min_size(pos2(x, y), vec2(card_w, cell_h));
            let cover = Rect::from_min_size(cell.min, vec2(card_w, card_w));
            let resp = ui.interact(cell, base_id.with(("album", i)), Sense::click());
            let play_rect = Rect::from_center_size(
                pos2(cover.right() - 30.0, cover.bottom() - 30.0),
                vec2(40.0, 40.0),
            );
            let play = ui.interact(play_rect, base_id.with(("album_play", i)), Sense::click());
            cover_image(
                ui,
                &a.covers,
                cover,
                a.covers.thumb(&album.cover_key),
                &album.cover_key,
                8,
            );
            if resp.hovered() || play.hovered() {
                painter.rect_filled(cover, CornerRadius::same(8), Color32::BLACK.alpha(0.25));
                painter.circle_filled(
                    play_rect.center(),
                    20.0,
                    if play.hovered() { pal.text } else { pal.text_1 },
                );
                paint_icon(&painter, play_rect.center(), fill::PLAY, 17.0, pal.bg, true);
            }
            let title = Rect::from_min_size(pos2(x, y + card_w + 8.0), vec2(card_w, 18.0));
            let meta = Rect::from_min_size(pos2(x, y + card_w + 26.0), vec2(card_w, 16.0));
            paint_text(
                &painter,
                title,
                Align::Min,
                &album.name,
                theme::semibold(13.5),
                pal.text,
            );
            let tracks = plural(album.tracks.len(), "track");
            let meta_text = if album.year > 0 {
                format!("{} · {tracks}", album.year)
            } else {
                tracks
            };
            paint_text(
                &painter,
                meta,
                Align::Min,
                &meta_text,
                theme::regular(12.5),
                pal.text_2,
            );
            if play.clicked() {
                out.play_album = Some(i);
            } else if resp.clicked() {
                out.open_album = Some(album.cover_key.clone());
            }
            if let Some(action) = menus::album_menu(&resp, album, &a.session.playlists.playlists) {
                out.action = Some(action);
            }
        }
    }
}
