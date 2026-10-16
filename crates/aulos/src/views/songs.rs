//! The songs page: every track of the library (or the search results) in one
//! virtualised list.

use crate::app::App;
use crate::theme;
use crate::views::menus::{self, MenuAction};
use crate::widgets::{self, Columns, HEADER_HEIGHT, PillKind, row_height};
use aulos_core::library::state::SortField;
use egui::{Frame, Margin, ScrollArea, Sense, Ui, vec2};
use egui_phosphor::regular;

#[derive(Default)]
pub struct State {
    /// Id of the selected (single-clicked) track.
    selected: Option<i64>,
    /// `(visible_epoch, total seconds)` of the list the title row shows.
    totals: Option<(u64, u64)>,
    /// Active sort column (`None` = the library's initial title order).
    sort: Option<SortField>,
}

/// Called when the route is entered: start loading what the page needs.
pub fn enter(_app: &mut App) {}

/// What the user asked for while the library was borrowed.
enum Intent {
    Play(usize),
    Favorite(usize),
    Rate(usize, u8),
    Menu(Box<MenuAction>),
    Select(i64),
    ShuffleAll,
    Sort(SortField),
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let mut state = std::mem::take(&mut app.views.songs);
    let intents = draw(app, ui, &mut state);
    app.views.songs = state;
    apply(app, intents);
}

/// `4812` → `4,812`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `13 d 6 h`, `2 h 44 m`, `5 m`.
fn total_label(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        aulos_core::fl!("songs-total-days-hours", days = d, hours = h)
    } else if h > 0 {
        aulos_core::fl!("songs-total-hours-minutes", hours = h, minutes = m)
    } else {
        aulos_core::fl!("songs-total-minutes", minutes = m)
    }
}

fn draw(app: &App, ui: &mut Ui, state: &mut State) -> Vec<Intent> {
    let pal = app.pal;
    let pad = app.page_pad();
    let library = &app.session.library;
    let tracks = library.visible_tracks();
    let mut intents = Vec::new();
    let mut shuffle_all = false;

    let epoch = library.visible_epoch();
    let total_secs = match state.totals {
        Some((e, s)) if e == epoch => s,
        _ => {
            let s = tracks.iter().map(|t| t.duration.as_secs()).sum();
            state.totals = Some((epoch, s));
            s
        }
    };

    let margin = Margin::symmetric(pad as i8, 0);
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.add_space(4.0);
    Frame::NONE.inner_margin(margin).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(aulos_core::fl!("songs"))
                    .font(theme::semibold(26.0))
                    .color(pal.text),
            );
            if !tracks.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {}",
                        thousands(tracks.len()),
                        total_label(total_secs)
                    ))
                    .font(theme::mono(13.0))
                    .color(pal.text_3),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = !library.tracks.is_empty();
                if widgets::pill_button_enabled(
                    ui,
                    &aulos_core::fl!("songs-shuffle-all"),
                    Some(regular::SHUFFLE),
                    PillKind::Primary,
                    30.0,
                    0.0,
                    enabled,
                )
                .clicked()
                {
                    shuffle_all = true;
                }
            });
        });
    });
    ui.add_space(14.0);

    if tracks.is_empty() {
        if library.tracks.is_empty() && library.scanning {
            loading(ui, pad);
        } else if library.tracks.is_empty() {
            widgets::empty_state(
                ui,
                regular::MUSIC_NOTES,
                &aulos_core::fl!("songs-empty-title"),
                &aulos_core::fl!("songs-empty-body"),
                None,
            );
        } else {
            let body = aulos_core::fl!(
                "songs-no-search-matches-body",
                query = library.search_query().trim()
            );
            widgets::empty_state(
                ui,
                regular::MAGNIFYING_GLASS,
                &aulos_core::fl!("songs-no-search-matches-title"),
                &body,
                None,
            );
        }
    } else {
        let cols = Columns::songs(app.class);
        let row_h = row_height(ui.ctx());
        let sorted = state.sort.unwrap_or(SortField::Title);
        Frame::NONE.inner_margin(margin).show(ui, |ui| {
            if let Some(field) = widgets::track_header_sortable(
                ui,
                cols,
                &aulos_core::fl!("songs-column-number"),
                sorted,
            ) {
                intents.push(Intent::Sort(field));
            }
        });
        let current = app.session.playback.current_track.as_ref().map(|t| t.id);
        ui.spacing_mut().item_spacing.y = 0.0;
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_h, tracks.len(), |ui, range| {
                Frame::NONE.inner_margin(margin).show(ui, |ui| {
                    for i in range {
                        let track = &tracks[i];
                        let number = (i + 1).to_string();
                        let out = widgets::track_row(
                            ui,
                            app,
                            track,
                            &number,
                            current == Some(track.id),
                            state.selected == Some(track.id),
                            cols,
                        );
                        let real = library.track_index(i);
                        if out.response.double_clicked() {
                            intents.push(Intent::Play(real));
                        } else if out.response.clicked()
                            && !out.toggle_favorite
                            && out.set_rating.is_none()
                        {
                            intents.push(Intent::Select(track.id));
                        }
                        if out.toggle_favorite {
                            intents.push(Intent::Favorite(real));
                        }
                        if let Some(n) = out.set_rating {
                            intents.push(Intent::Rate(real, n));
                        }
                        let playlists = &app.session.playlists.playlists;
                        if let Some(action) = menus::track_menu(&out.response, track, playlists)
                            .or_else(|| menus::track_menu_click(&out.more, track, playlists))
                        {
                            intents.push(Intent::Menu(Box::new(action)));
                        }
                    }
                });
            });
    }

    if shuffle_all {
        intents.push(Intent::ShuffleAll);
    }
    intents
}

/// Skeleton rows while the first scan has not produced any track yet.
fn loading(ui: &mut Ui, pad: f32) {
    let width = ui.available_width() - pad * 2.0;
    ui.add_space(HEADER_HEIGHT);
    let row_h = row_height(ui.ctx());
    let rows = ((ui.available_height() / row_h) as usize).clamp(4, 16);
    for _ in 0..rows {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
        let inner = egui::Rect::from_min_size(
            rect.min + vec2(pad + 10.0, 11.0),
            vec2((width - 20.0).max(0.0), 14.0),
        );
        let title = egui::Rect::from_min_size(inner.min, vec2(inner.width() * 0.32, 14.0));
        let album = egui::Rect::from_min_size(
            inner.min + vec2(inner.width() * 0.55, 0.0),
            vec2(inner.width() * 0.2, 14.0),
        );
        widgets::skeleton(ui, title, 4);
        widgets::skeleton(ui, album, 4);
    }
}

fn apply(app: &mut App, intents: Vec<Intent>) {
    for intent in intents {
        match intent {
            Intent::Select(id) => app.views.songs.selected = Some(id),
            Intent::ShuffleAll => shuffle_all(app),
            Intent::Sort(field) => {
                app.views.songs.sort = Some(field);
                app.session.sort_tracks(field);
            }
            Intent::Play(index) => {
                let update = app.session.play_library_track(index);
                app.run(update);
            }
            Intent::Favorite(index) => {
                if let Some(track) = app.session.library.tracks.get(index).cloned() {
                    app.toggle_favorite(&track);
                }
            }
            Intent::Rate(index, n) => {
                if let Some(id) = app
                    .session
                    .library
                    .tracks
                    .get(index)
                    .map(|t| t.id.to_string())
                {
                    let effect = app.session.set_rating(id, n);
                    app.run(effect);
                }
            }
            Intent::Menu(action) => menus::apply(app, *action),
        }
    }
}

/// Turns shuffle on and starts the whole library at a random track.
fn shuffle_all(app: &mut App) {
    if app.session.library.tracks.is_empty() {
        return;
    }
    if !app.session.config.config.shuffle {
        let update = app.session.toggle_shuffle();
        app.run(update);
    }
    let tracks = app.session.library.tracks.clone();
    let nanos = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    let start = nanos % tracks.len();
    let update = app.session.play_tracks(tracks, start);
    app.run(update);
}
