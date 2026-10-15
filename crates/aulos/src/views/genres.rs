//! The genres page: a grid / list of genre cards, and the genre detail route.

use crate::app::{App, Route};
use crate::theme::{self, ColorExt};
use crate::views::artists::{
    COVER_ROW_HEIGHT, CoverRow, cover_track_row, page_title, scanning_banner, vertical_gradient,
    visible_rows,
};
use crate::views::menus::{self, MenuAction};
use crate::widgets::{
    PillKind, empty_state, gradient_fill, label_text, paint_icon, paint_text, pill_button_enabled,
    skeleton, text_width,
};
use aulos_core::config::ViewMode;
use aulos_core::format::format_duration_coarse;
use aulos_core::library::genre;
use egui::{
    Align, Color32, CornerRadius, Frame, Margin, Mesh, Rect, Sense, Shape, TextureHandle, Ui, Vec2,
    pos2, vec2,
};
use egui_phosphor::regular;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How long an in-flight load shows the loading state before the page
/// concludes there is nothing to show.
const LOAD_GRACE: Duration = Duration::from_secs(3);

#[derive(Default)]
pub struct State {
    /// When the genre list was last requested.
    requested: Option<Instant>,
    /// When the tracks of the open genre were last requested.
    tracks_requested: Option<Instant>,
    /// Counts and cover keys per genre, for the cards.
    meta: Option<GenreMeta>,
    /// Summary of the open genre.
    stats: Option<GenreStats>,
    /// Genre the detail state belongs to.
    detail: String,
    /// Selected track row (track id).
    selected: Option<i64>,
}

/// Per-genre song counts and a few album covers, built from the library.
struct GenreMeta {
    generation: u64,
    /// Lower-cased genre -> song count.
    counts: HashMap<String, u32>,
    /// Lower-cased genre -> cover keys of some of its albums.
    covers: HashMap<String, Vec<String>>,
}

/// Summary of the tracks loaded for the open genre.
struct GenreStats {
    /// `(genre row, track count, library generation)` this was built for.
    key: (usize, usize, u64),
    albums: usize,
    secs: u64,
    /// Most represented artists, best first.
    artists: Vec<String>,
}

/// Called when the route is entered: start loading what the page needs.
pub fn enter(app: &mut App) {
    app.views.genres.requested = Some(Instant::now());
    if app.route == Route::Genres {
        app.session.library.selected_genre = None;
        app.session.library.genre_tracks.clear();
    }
    let effect = app.session.load_genres();
    app.run(effect);
}

pub fn show(app: &mut App, ui: &mut Ui) {
    match app.route.clone() {
        Route::Genre(name) => detail(app, ui, &name),
        _ => browse(app, ui),
    }
}

fn recent(at: Option<Instant>) -> bool {
    at.is_some_and(|t| t.elapsed() < LOAD_GRACE)
}

fn colors(genre: &str) -> (Color32, Color32) {
    let (a, b) = genre::gradient_rgb8(genre);
    (
        Color32::from_rgb(a[0], a[1], a[2]),
        Color32::from_rgb(b[0], b[1], b[2]),
    )
}

fn songs_label(count: u32) -> String {
    format!("{count} song{}", if count == 1 { "" } else { "s" })
}

// -- browse -----------------------------------------------------------------------

#[derive(Default)]
struct BrowseOut {
    open: Option<String>,
}

fn refresh_meta(app: &mut App) {
    let lib = &app.session.library;
    let generation = lib.generation();
    if app
        .views
        .genres
        .meta
        .as_ref()
        .is_some_and(|m| m.generation == generation)
    {
        return;
    }
    let mut counts: HashMap<String, u32> = HashMap::new();
    for track in &lib.tracks {
        if !track.genre.is_empty() {
            *counts.entry(track.genre.to_lowercase()).or_default() += 1;
        }
    }
    let mut covers: HashMap<String, Vec<String>> = HashMap::new();
    for album in &lib.albums {
        let Some(track) = album.tracks.first() else {
            continue;
        };
        if track.genre.is_empty() {
            continue;
        }
        let keys = covers.entry(track.genre.to_lowercase()).or_default();
        if keys.len() < 4 {
            keys.push(album.cover_key.clone());
        }
    }
    app.views.genres.meta = Some(GenreMeta {
        generation,
        counts,
        covers,
    });
}

fn browse(app: &mut App, ui: &mut Ui) {
    refresh_meta(app);
    let pad = app.page_pad();
    let list = app.session.config.config.genres_view_mode == ViewMode::List;
    let mut out = BrowseOut::default();
    {
        let a: &App = app;
        let pal = a.pal;
        let lib = &a.session.library;
        let genres = lib.visible_genres();
        let loading = genres.is_empty()
            && lib.genres.is_empty()
            && (lib.scanning || recent(a.views.genres.requested));
        egui::ScrollArea::vertical()
            .id_salt("genres_scroll")
            .auto_shrink([false, false])
            .content_margin(Margin::ZERO)
            .show(ui, |ui| {
                ui.add_space(4.0);
                Frame::NONE
                    .inner_margin(Margin::symmetric(pad as i8, 0))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing = Vec2::ZERO;
                        page_title(ui, &pal, "Genres", &genres.len().to_string());
                        ui.add_space(18.0);
                        if loading {
                            ui.ctx().request_repaint_after(Duration::from_millis(100));
                            if lib.scanning {
                                scanning_banner(
                                    ui,
                                    &pal,
                                    &lib.scan_progress.snapshot(),
                                    "Scanning your library…",
                                );
                                ui.add_space(14.0);
                            }
                            skeleton_grid(ui);
                        } else if genres.is_empty() {
                            empty(ui, a);
                        } else if list {
                            list_body(ui, a, genres, &mut out);
                        } else {
                            grid_body(ui, a, genres, &mut out);
                        }
                        ui.add_space(32.0);
                    });
            });
    }
    if let Some(name) = out.open {
        app.navigate(Route::Genre(name));
    }
}

fn empty(ui: &mut Ui, a: &App) {
    let query = a.session.library.search_query().trim().to_owned();
    if query.is_empty() {
        empty_state(
            ui,
            regular::SHAPES,
            "No genres yet",
            "Genres come from the tags of your music. Songs without a genre tag are not listed here.",
            None,
        );
    } else {
        empty_state(
            ui,
            regular::SHAPES,
            "No matching genres",
            &format!("No genre matches “{query}”."),
            None,
        );
    }
}

fn skeleton_grid(ui: &mut Ui) {
    let w = ui.available_width();
    let cols = (((w + 14.0) / (188.0 + 14.0)).floor() as usize).max(1);
    let cw = (w - 14.0 * (cols as f32 - 1.0)) / cols as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(w, 108.0 * 3.0 + 14.0 * 2.0), Sense::hover());
    for r in 0..3 {
        for c in 0..cols {
            let min = pos2(
                rect.left() + c as f32 * (cw + 14.0),
                rect.top() + r as f32 * (108.0 + 14.0),
            );
            skeleton(ui, Rect::from_min_size(min, vec2(cw, 108.0)), 10);
        }
    }
}

/// Paints `tex` centre-cropped to a square, tilted by 14 degrees, into `rect`.
fn paint_tilted(painter: &egui::Painter, tex: &TextureHandle, rect: Rect) {
    let [w, h] = tex.size();
    let (w, h) = (w as f32, h as f32);
    let uv = if w > h {
        let m = (1.0 - h / w) / 2.0;
        Rect::from_min_max(pos2(m, 0.0), pos2(1.0 - m, 1.0))
    } else {
        let m = (1.0 - w / h) / 2.0;
        Rect::from_min_max(pos2(0.0, m), pos2(1.0, 1.0 - m))
    };
    let mut mesh = Mesh::with_texture(tex.id());
    mesh.add_rect_with_uv(rect, uv, Color32::WHITE);
    mesh.rotate(
        egui::emath::Rot2::from_angle(14f32.to_radians()),
        rect.center(),
    );
    painter.add(Shape::mesh(mesh));
}

fn grid_body(ui: &mut Ui, a: &App, genres: &[String], out: &mut BrowseOut) {
    let (min_w, gap, card_h) = (188.0, 14.0, 108.0);
    let w = ui.available_width();
    let cols = (((w + gap) / (min_w + gap)).floor() as usize).max(1);
    let cw = (w - gap * (cols as f32 - 1.0)) / cols as f32;
    let stride = card_h + gap;
    let n = genres.len();
    let grid_rows = n.div_ceil(cols);
    let (rect, _) = ui.allocate_exact_size(
        vec2(w, (grid_rows as f32 * stride - gap).max(0.0)),
        Sense::hover(),
    );
    let base_id = ui.id();
    let painter = ui.painter().clone();
    let meta = a.views.genres.meta.as_ref();

    for r in visible_rows(ui, rect.top(), stride, grid_rows) {
        for c in 0..cols {
            let i = r * cols + c;
            if i >= n {
                break;
            }
            let name = genres[i].as_str();
            let card = Rect::from_min_size(
                pos2(
                    rect.left() + c as f32 * (cw + gap),
                    rect.top() + r as f32 * stride,
                ),
                vec2(cw, card_h),
            );
            let resp = ui.interact(card, base_id.with(("genre", i)), Sense::click());
            let (from, to) = colors(name);
            gradient_fill(&a.covers, ui, card, 10, from, to);

            let lower = name.to_lowercase();
            let clipped = painter.with_clip_rect(card.intersect(ui.clip_rect()));
            let thumb = meta
                .and_then(|m| m.covers.get(&lower))
                .and_then(|keys| keys.iter().find_map(|k| a.covers.thumb(k)));
            if let Some(tex) = thumb {
                let tile = Rect::from_min_size(
                    pos2(card.right() + 14.0 - 64.0, card.bottom() + 14.0 - 64.0),
                    vec2(64.0, 64.0),
                );
                paint_tilted(&clipped, tex, tile);
            }
            if resp.hovered() {
                painter.rect_filled(card, CornerRadius::same(10), Color32::WHITE.alpha(0.08));
            }
            let text_w = (cw - 28.0).max(0.0);
            let title = Rect::from_min_size(
                pos2(card.left() + 14.0, card.top() + 12.0),
                vec2(text_w, 22.0),
            );
            paint_text(
                &painter,
                title,
                Align::Min,
                name,
                theme::semibold(16.0),
                Color32::WHITE,
            );
            if let Some(count) = meta.and_then(|m| m.counts.get(&lower)) {
                let sub = Rect::from_min_size(
                    pos2(card.left() + 14.0, card.bottom() - 12.0 - 16.0),
                    vec2(text_w, 16.0),
                );
                paint_text(
                    &painter,
                    sub,
                    Align::Min,
                    &songs_label(*count),
                    theme::regular(12.0),
                    Color32::WHITE.alpha(0.9),
                );
            }
            if resp.clicked() {
                out.open = Some(name.to_owned());
            }
        }
    }
}

fn list_body(ui: &mut Ui, a: &App, genres: &[String], out: &mut BrowseOut) {
    const ROW_H: f32 = 56.0;
    let pal = a.pal;
    let w = ui.available_width();
    let n = genres.len();
    let (rect, _) = ui.allocate_exact_size(vec2(w, n as f32 * ROW_H), Sense::hover());
    let base_id = ui.id();
    let painter = ui.painter().clone();
    let meta = a.views.genres.meta.as_ref();
    for i in visible_rows(ui, rect.top(), ROW_H, n) {
        let name = genres[i].as_str();
        let row = Rect::from_min_size(
            pos2(rect.left(), rect.top() + i as f32 * ROW_H),
            vec2(w, ROW_H),
        );
        let resp = ui.interact(row, base_id.with(("genre_row", i)), Sense::click());
        if resp.hovered() {
            painter.rect_filled(row, CornerRadius::same(6), pal.hover);
        }
        let swatch = Rect::from_min_size(
            pos2(row.left() + 10.0, row.center().y - 20.0),
            vec2(40.0, 40.0),
        );
        let (from, to) = colors(name);
        gradient_fill(&a.covers, ui, swatch, 8, from, to);
        let right = row.right() - 10.0;
        let count_rect = Rect::from_min_size(
            pos2(right - 16.0 - 14.0 - 96.0, row.top()),
            vec2(96.0, ROW_H),
        );
        let name_rect = Rect::from_min_max(
            pos2(swatch.right() + 14.0, row.top()),
            pos2(count_rect.left() - 14.0, row.bottom()),
        );
        paint_text(
            &painter,
            name_rect,
            Align::Min,
            name,
            theme::semibold(13.0),
            pal.text,
        );
        if let Some(count) = meta.and_then(|m| m.counts.get(&name.to_lowercase())) {
            paint_text(
                &painter,
                count_rect,
                Align::Max,
                &songs_label(*count),
                theme::mono(12.0),
                pal.text_2,
            );
        }
        paint_icon(
            &painter,
            pos2(right - 8.0, row.center().y),
            regular::CARET_RIGHT,
            13.0,
            pal.text_4,
            false,
        );
        if resp.clicked() {
            out.open = Some(name.to_owned());
        }
    }
}

// -- detail -----------------------------------------------------------------------

#[derive(Default)]
struct DetailOut {
    shuffle: bool,
    play: Option<usize>,
    select: Option<i64>,
    open_artist: Option<String>,
    action: Option<MenuAction>,
}

fn refresh_stats(app: &mut App, idx: usize) {
    let lib = &app.session.library;
    let key = (idx, lib.genre_tracks.len(), lib.generation());
    if app
        .views
        .genres
        .stats
        .as_ref()
        .is_some_and(|s| s.key == key)
    {
        return;
    }
    let tracks = &lib.genre_tracks;
    let mut albums: HashSet<(&str, &str)> = HashSet::new();
    let mut by_artist: HashMap<&str, usize> = HashMap::new();
    let mut secs = 0;
    for track in tracks {
        albums.insert((track.grouping_artist(), track.album.as_str()));
        *by_artist.entry(track.grouping_artist()).or_default() += 1;
        secs += track.duration.as_secs();
    }
    let mut ranked: Vec<(&str, usize)> = by_artist
        .into_iter()
        .filter(|(name, _)| !name.is_empty())
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let artists = ranked
        .into_iter()
        .take(5)
        .map(|(name, _)| name.to_owned())
        .collect();
    app.views.genres.stats = Some(GenreStats {
        key,
        albums: albums.len(),
        secs,
        artists,
    });
}

fn detail(app: &mut App, ui: &mut Ui, name: &str) {
    if app.views.genres.detail != name {
        let state = &mut app.views.genres;
        state.detail = name.to_owned();
        state.selected = None;
        state.stats = None;
        state.tracks_requested = None;
    }
    let pad = app.page_pad();
    let pal = app.pal;
    let idx = app
        .session
        .library
        .genres
        .iter()
        .position(|g| g.eq_ignore_ascii_case(name));
    let Some(idx) = idx else {
        let lib = &app.session.library;
        if lib.scanning || recent(app.views.genres.requested) {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            ui.add_space(4.0);
            Frame::NONE
                .inner_margin(Margin::symmetric(pad as i8, 0))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    skeleton_grid(ui);
                });
        } else if empty_state(
            ui,
            regular::SHAPES,
            "Genre not found",
            &format!("There are no songs tagged “{name}” in this library."),
            Some("All genres"),
        ) {
            app.navigate(Route::Genres);
        }
        return;
    };
    if app.session.library.selected_genre != Some(idx) {
        let effect = app.session.load_genre_tracks(idx);
        app.run(effect);
        app.views.genres.tracks_requested = Some(Instant::now());
    }
    refresh_stats(app, idx);

    let mut out = DetailOut::default();
    {
        let a: &App = app;
        let lib = &a.session.library;
        let genre_name = lib.genres[idx].as_str();
        let tracks = lib.genre_tracks.as_slice();
        let loading = tracks.is_empty() && recent(a.views.genres.tracks_requested);
        egui::ScrollArea::vertical()
            .id_salt("genre_detail")
            .auto_shrink([false, false])
            .content_margin(Margin::ZERO)
            .show(ui, |ui| {
                let origin = ui.cursor().min;
                let (from, _) = colors(genre_name);
                let tint = theme::lerp_color(pal.bg, from, 0.9);
                let hero_rect = Rect::from_min_size(origin, vec2(ui.available_width(), 260.0));
                vertical_gradient(ui.painter(), hero_rect, tint, pal.bg);
                super::artists::announce_hero_tint(ui, origin, tint);
                ui.add_space(8.0);
                Frame::NONE.inner_margin(Margin::symmetric(pad as i8, 0)).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    hero(ui, a, genre_name, tracks.len(), &mut out);
                    ui.add_space(22.0);
                    top_artists(ui, a, &mut out);
                    ui.add_space(22.0);
                    if loading {
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                        skeleton_rows(ui);
                    } else if tracks.is_empty() {
                        empty_state(
                            ui,
                            regular::MUSIC_NOTES,
                            "No songs in this genre",
                            "Nothing tagged with this genre could be loaded from the active source.",
                            None,
                        );
                    } else {
                        track_list(ui, a, tracks, &mut out);
                    }
                    ui.add_space(24.0);
                });
            });
    }
    if out.select.is_some() {
        app.views.genres.selected = out.select;
    }
    if out.shuffle {
        let update = app.session.shuffle_genre();
        app.run(update);
    }
    if let Some(i) = out.play {
        let update = app.session.play_genre_track(i);
        app.run(update);
    }
    if let Some(name) = out.open_artist {
        app.navigate(Route::Artist(name));
    }
    if let Some(action) = out.action {
        menus::apply(app, action);
    }
}

fn hero(ui: &mut Ui, a: &App, genre_name: &str, track_count: usize, out: &mut DetailOut) {
    let white = Color32::WHITE;
    ui.add_space(20.0);
    ui.label(
        egui::RichText::new("GENRE")
            .font(theme::semibold(11.0))
            .color(white.alpha(0.82))
            .extra_letter_spacing(1.1),
    );
    ui.add_space(8.0);
    label_text(ui, genre_name, theme::bold(48.0), white);
    ui.add_space(8.0);
    if let Some(stats) = a.views.genres.stats.as_ref().filter(|_| track_count > 0) {
        let albums = format!(
            "{} album{}",
            stats.albums,
            if stats.albums == 1 { "" } else { "s" }
        );
        let summary = format!(
            "{} · {albums} · {}",
            songs_label(track_count as u32),
            format_duration_coarse(stats.secs)
        );
        label_text(ui, &summary, theme::regular(13.0), white.alpha(0.86));
    } else {
        label_text(
            ui,
            &songs_label(track_count as u32),
            theme::regular(13.0),
            white.alpha(0.86),
        );
    }
    ui.add_space(16.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let has = track_count > 0;
        if pill_button_enabled(
            ui,
            "Shuffle",
            Some(regular::SHUFFLE),
            PillKind::Primary,
            34.0,
            0.0,
            has,
        )
        .clicked()
        {
            out.shuffle = true;
        }
        if pill_button_enabled(
            ui,
            "Play in order",
            Some(regular::PLAY),
            PillKind::Secondary,
            34.0,
            0.0,
            has,
        )
        .clicked()
        {
            out.play = Some(0);
        }
    });
}

fn top_artists(ui: &mut Ui, a: &App, out: &mut DetailOut) {
    let pal = a.pal;
    let Some(stats) = a
        .views
        .genres
        .stats
        .as_ref()
        .filter(|s| !s.artists.is_empty())
    else {
        return;
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        let label_w = text_width(ui, "Top artists", theme::regular(13.0)) + 4.0;
        let (rect, _) = ui.allocate_exact_size(vec2(label_w, 30.0), Sense::hover());
        paint_text(
            ui.painter(),
            rect,
            Align::Min,
            "Top artists",
            theme::regular(13.0),
            pal.text_2,
        );
        for name in &stats.artists {
            let font = theme::regular(13.0);
            let text_w = text_width(ui, name, font.clone());
            let (rect, resp) = ui
                .allocate_exact_size(vec2(4.0 + 22.0 + 8.0 + text_w + 12.0, 30.0), Sense::click());
            let bg = if resp.hovered() {
                pal.surface_hi
            } else {
                pal.surface_2
            };
            ui.painter().rect_filled(rect, CornerRadius::same(15), bg);
            let avatar = Rect::from_min_size(
                pos2(rect.left() + 4.0, rect.center().y - 11.0),
                vec2(22.0, 22.0),
            );
            match a.covers.photo(name) {
                Some(tex) => {
                    crate::widgets::cover_image(ui, &a.covers, avatar, Some(tex), name, 255)
                }
                None => {
                    let (from, to) = crate::widgets::seed_colors(name);
                    gradient_fill(&a.covers, ui, avatar, 255, from, to);
                }
            }
            let label = Rect::from_min_max(
                pos2(avatar.right() + 8.0, rect.top()),
                pos2(rect.right() - 12.0, rect.bottom()),
            );
            paint_text(ui.painter(), label, Align::Min, name, font, pal.text);
            if resp.clicked() {
                out.open_artist = Some(name.clone());
            }
            if let Some(action) = menus::artist_menu(&resp, name) {
                out.action = Some(action);
            }
        }
    });
}

fn skeleton_rows(ui: &mut Ui) {
    let w = ui.available_width();
    for _ in 0..8 {
        let (row, _) = ui.allocate_exact_size(vec2(w, COVER_ROW_HEIGHT), Sense::hover());
        skeleton(
            ui,
            Rect::from_min_size(
                pos2(row.left() + 8.0 + 24.0 + 12.0, row.center().y - 18.0),
                vec2(36.0, 36.0),
            ),
            4,
        );
        let x = row.left() + 8.0 + 24.0 + 12.0 + 36.0 + 12.0;
        skeleton(
            ui,
            Rect::from_min_size(pos2(x, row.center().y - 11.0), vec2(w * 0.25, 11.0)),
            4,
        );
        skeleton(
            ui,
            Rect::from_min_size(pos2(x, row.center().y + 4.0), vec2(w * 0.15, 9.0)),
            4,
        );
    }
}

fn track_list(ui: &mut Ui, a: &App, tracks: &[aulos_core::library::Track], out: &mut DetailOut) {
    let n = tracks.len();
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, n as f32 * COVER_ROW_HEIGHT), Sense::hover());
    let current = a.session.playback.current_track.as_ref().map(|t| t.id);
    let selected = a.views.genres.selected;
    for i in visible_rows(ui, rect.top(), COVER_ROW_HEIGHT, n) {
        let track = &tracks[i];
        let row = Rect::from_min_size(
            pos2(rect.left(), rect.top() + i as f32 * COVER_ROW_HEIGHT),
            vec2(w, COVER_ROW_HEIGHT),
        );
        let number = (i + 1).to_string();
        let (resp, action) = cover_track_row(
            ui,
            a,
            row,
            CoverRow {
                track,
                number: &number,
                subtitle: &track.artist,
                album_col: true,
                current: current == Some(track.id),
                selected: selected == Some(track.id),
            },
        );
        if resp.double_clicked() {
            out.play = Some(i);
        } else if resp.clicked() {
            out.select = Some(track.id);
        }
        if let Some(action) = action {
            out.action = Some(action);
        }
    }
}
