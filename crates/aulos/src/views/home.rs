//! The home page: a time-of-day greeting over shelves of albums and artists
//! built from the play history (`session.home`), the decade tiles and the
//! decade grid.
//!
//! Cards carry only `(artist, name)`; the library is searched for the album
//! when a card is clicked (or its context menu opened), never per frame.

use crate::app::{App, Route};
use crate::theme::{self, ColorExt, Theme};
use crate::views::menus;
use crate::widgets::{
    IconBtn, PillKind, avatar_tile, cover_image, empty_state, gradient_fill, paint_icon,
    paint_text, pill_button, seed_colors, skeleton,
};
use aulos_core::library::album_sort::AlbumSort;
use aulos_core::library::history::{AlbumRef, DecadeCount, HomeData};
use aulos_core::library::home::artist_tracks;
use aulos_core::library::home_state::{Greeting, decade_label};
use aulos_core::library::{CoverArt, Track};
use chrono::Timelike;
use egui::scroll_area::ScrollBarVisibility;
use egui::{
    Align, Color32, CornerRadius, Frame, Id, Layout, Margin, Rect, RichText, ScrollArea, Sense,
    Shape, Ui, pos2, vec2,
};
use egui_phosphor::{fill, regular};
use std::collections::{HashMap, HashSet};

const CARD: f32 = 148.0;
const CARD_GAP: f32 = 20.0;
/// 8px gap plus the two text rows under a cover.
const CARD_TEXT: f32 = 44.0;
const SECTION_GAP: f32 = 28.0;

#[derive(Default)]
pub struct State {
    /// Horizontal scroll offset of every shelf, by shelf id.
    offsets: HashMap<Id, f32>,
    /// Artists whose photo was already asked for this visit.
    asked: HashSet<String>,
}

/// Called when the route is entered: start loading what the page needs.
pub fn enter(app: &mut App) {
    app.views.home.asked.clear();
    let effect = app.session.enter_home();
    app.run(effect);
}

/// What a click on the page asked for; applied once drawing is done.
enum Act {
    OpenAlbum(String, String),
    PlayAlbum(String, String),
    OpenArtist(String),
    PlayArtist(String),
    Reroll,
    OpenDecade(u32),
    /// Play the Favourites list from this row.
    PlayFavourites(usize),
    /// Open the Albums page (ordered by date added when asked).
    SeeAllAlbums {
        recently_added: bool,
    },
    CloseDecade,
    PlayDecade,
    ShuffleAll,
    Goto(Route),
    OpenUrl(String),
}

/// Everything drawing collects for after the library borrow ends.
#[derive(Default)]
struct Out {
    act: Option<Act>,
    photos: Vec<String>,
    /// Suggested artists outside the library, which only get a public photo.
    suggested_photos: Vec<String>,
    menu: Option<menus::MenuAction>,
}

/// Drawing context shared by the card helpers.
struct Cx<'a> {
    app: &'a App,
    out: &'a mut Out,
    asked: &'a mut HashSet<String>,
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let mut st = std::mem::take(&mut app.views.home);
    let mut out = Out::default();
    if app.session.home.decade.is_some() {
        decade_page(app, ui, &mut out);
    } else {
        home_page(app, ui, &mut st, &mut out);
    }
    app.views.home = st;
    apply(app, out);
}

fn apply(app: &mut App, out: Out) {
    for name in out.photos {
        let effect = app.session.fetch_artist_info_for(&name);
        app.run(effect);
    }
    for name in out.suggested_photos {
        let effect = app.session.fetch_suggested_artist_photo(&name);
        app.run(effect);
    }
    if let Some(menu) = out.menu {
        menus::apply(app, menu);
    }
    let Some(act) = out.act else { return };
    match act {
        Act::OpenAlbum(artist, name) => {
            app.navigate(Route::Album(CoverArt::album_key(&artist, &name)))
        }
        Act::PlayAlbum(artist, name) => {
            if let Some(idx) = app.session.library.find_album_exact(&artist, &name) {
                let update = app.session.play_album(idx, 0);
                app.run(update);
            }
        }
        Act::OpenArtist(name) => app.navigate(Route::Artist(name)),
        Act::PlayArtist(name) => {
            let tracks = artist_tracks(&app.session.library.albums, &name);
            if !tracks.is_empty() {
                let update = app.session.play_tracks(tracks, 0);
                app.run(update);
            }
        }
        Act::Reroll => {
            let effect = app.session.reroll_random_picks();
            app.run(effect);
        }
        Act::OpenDecade(decade) => {
            let effect = app.session.open_decade(decade);
            app.run(effect);
        }
        Act::PlayFavourites(start) => {
            let tracks = app
                .session
                .home
                .data
                .as_ref()
                .map(|d| d.favourites.clone())
                .unwrap_or_default();
            if start < tracks.len() {
                let update = app.session.play_tracks(tracks, start);
                app.run(update);
            }
        }
        Act::SeeAllAlbums { recently_added } => {
            if recently_added {
                app.session.set_albums_sort(AlbumSort::RecentlyAdded);
            }
            app.navigate(Route::Albums);
        }
        Act::CloseDecade => app.session.home.decade = None,
        Act::PlayDecade => {
            let tracks = app.session.decade_tracks();
            play_shuffled(app, tracks);
        }
        Act::ShuffleAll => {
            let tracks = aulos_core::library::owned_tracks(&app.session.library.tracks);
            play_shuffled(app, tracks);
        }
        Act::Goto(route) => app.navigate(route),
        Act::OpenUrl(url) => {
            if url.starts_with("https://")
                && let Err(error) = aulos_core::desktop::browser::open_in_browser(&url)
            {
                app.toast_error(error);
            }
        }
    }
}

/// Turns shuffle on and starts `tracks` at a random position.
pub fn play_shuffled(app: &mut App, tracks: Vec<Track>) {
    if tracks.is_empty() {
        return;
    }
    if !app.session.config.config.shuffle {
        let update = app.session.toggle_shuffle();
        app.run(update);
    }
    let nanos = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    let start = nanos % tracks.len();
    let update = app.session.play_tracks(tracks, start);
    app.run(update);
}

// ---------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------

fn home_page(app: &App, ui: &mut Ui, st: &mut State, out: &mut Out) {
    let library_albums = app.session.library.albums.len();
    let data = app.session.home.data.as_ref();
    let nothing = library_albums == 0 && data.is_none_or(|d| d.total_albums == 0);
    if nothing {
        welcome(app, ui, out);
        return;
    }
    let State { offsets, asked } = st;
    let pad = app.page_pad() as i8;
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        Frame::NONE
            .inner_margin(Margin {
                left: pad,
                right: pad,
                top: 6,
                bottom: 32,
            })
            .show(ui, |ui| {
                greeting(app, ui, library_albums);
                ui.add_space(SECTION_GAP);

                let Some(data) = data else {
                    loading_shelves(ui);
                    return;
                };
                let mut cx = Cx { app, out, asked };

                if data.history_available && data.total_plays == 0 {
                    no_history(ui, &mut cx);
                    ui.add_space(SECTION_GAP);
                }
                if !data.recently_played.is_empty() {
                    album_shelf(
                        ui,
                        &mut cx,
                        offsets,
                        "recent",
                        &aulos_core::fl!("queue-history-label"),
                        None,
                        Some(Act::SeeAllAlbums {
                            recently_added: false,
                        }),
                        &data.recently_played,
                    );
                    ui.add_space(SECTION_GAP);
                }
                favourites_and_decades(ui, &mut cx, data);
                if !data.most_played_month.is_empty() {
                    album_shelf(
                        ui,
                        &mut cx,
                        offsets,
                        "most",
                        &aulos_core::fl!("home-most-played-month"),
                        None,
                        Some(Act::SeeAllAlbums {
                            recently_added: false,
                        }),
                        &data.most_played_month,
                    );
                    ui.add_space(SECTION_GAP);
                }
                if !data.top_artists_month.is_empty() {
                    let title = aulos_core::fl!("home-top-artists-month");
                    let head = Head {
                        title: &title,
                        hint: None,
                        action: None,
                        see_all: true,
                    };
                    let clicked = shelf(
                        ui,
                        offsets,
                        Id::new(("home-shelf", "top-artists")),
                        head,
                        |ui| {
                            for a in &data.top_artists_month {
                                let sub = aulos_core::fl!("home-plays", count = i64::from(a.plays));
                                artist_card(ui, &mut cx, &a.name, &sub, ArtistKind::Library);
                            }
                        },
                    );
                    if clicked.see_all {
                        cx.out.act = Some(Act::Goto(Route::Artists));
                    }
                    ui.add_space(SECTION_GAP);
                }
                for s in &app.session.home.similar {
                    if s.is_empty() {
                        continue;
                    }
                    let title =
                        aulos_core::fl!("home-because-you-listened", artist = s.seed.as_str());
                    let similar_hint = aulos_core::fl!("home-similar-hint");
                    let in_library = aulos_core::fl!("home-in-library");
                    let not_in_library = aulos_core::fl!("home-not-in-library");
                    let head = Head {
                        title: &title,
                        hint: Some(&similar_hint),
                        action: None,
                        see_all: true,
                    };
                    let clicked = shelf(
                        ui,
                        offsets,
                        Id::new(("home-shelf", "similar", &s.seed)),
                        head,
                        |ui| {
                            for name in &s.in_library {
                                artist_card(ui, &mut cx, name, &in_library, ArtistKind::Library);
                            }
                            for d in &s.discover {
                                let kind = ArtistKind::Discover {
                                    url: &d.url,
                                    site: d.site,
                                };
                                artist_card(ui, &mut cx, &d.name, &not_in_library, kind);
                            }
                        },
                    );
                    if clicked.see_all {
                        cx.out.act = Some(Act::Goto(Route::Artists));
                    }
                    ui.add_space(SECTION_GAP);
                }
                if !data.recently_added.is_empty() {
                    album_shelf(
                        ui,
                        &mut cx,
                        offsets,
                        "added",
                        &aulos_core::fl!("home-recently-added"),
                        None,
                        Some(Act::SeeAllAlbums {
                            recently_added: true,
                        }),
                        &data.recently_added,
                    );
                    ui.add_space(SECTION_GAP);
                }
                if !data.rediscover.is_empty() {
                    let hint = aulos_core::fl!("home-rediscover-hint");
                    album_shelf(
                        ui,
                        &mut cx,
                        offsets,
                        "rediscover",
                        &aulos_core::fl!("home-rediscover"),
                        Some(&hint),
                        Some(Act::SeeAllAlbums {
                            recently_added: false,
                        }),
                        &data.rediscover,
                    );
                    ui.add_space(SECTION_GAP);
                }
                if !data.random.is_empty() {
                    let title = aulos_core::fl!("home-random-picks");
                    let shuffle_tip = aulos_core::fl!("home-shuffle-picks");
                    let head = Head {
                        title: &title,
                        hint: None,
                        action: Some((regular::SHUFFLE, &shuffle_tip)),
                        see_all: true,
                    };
                    let clicked =
                        shelf(ui, offsets, Id::new(("home-shelf", "random")), head, |ui| {
                            for a in &data.random {
                                album_card(ui, &mut cx, a);
                            }
                        });
                    if clicked.action {
                        cx.out.act = Some(Act::Reroll);
                    } else if clicked.see_all {
                        cx.out.act = Some(Act::SeeAllAlbums {
                            recently_added: false,
                        });
                    }
                    ui.add_space(SECTION_GAP);
                }
            });
    });
}

fn greeting(app: &App, ui: &mut Ui, library_albums: usize) {
    let pal = app.pal;
    let text = Greeting::for_hour(chrono::Local::now().hour())
        .text_for(&app.session.config.config.user_name);
    ui.label(
        RichText::new(text)
            .font(theme::semibold(30.0))
            .color(pal.text),
    );
    ui.add_space(4.0);
    let resume = app
        .session
        .home
        .data
        .as_ref()
        .and_then(|data| data.resume.as_ref());
    match &app.session.playback.current_track {
        Some(track) if !track.is_live_stream() => {
            let artist = if track.artist.is_empty() {
                String::new()
            } else {
                format!(" · {}", track.artist)
            };
            let now_playing = format!("{} ", aulos_core::fl!("queue-now-playing-label"));
            crate::widgets::truncated_runs(
                ui,
                &[
                    (&now_playing, theme::regular(14.0), pal.text_2),
                    (&track.title, theme::medium(14.0), pal.text),
                    (&artist, theme::regular(14.0), pal.text_2),
                ],
            );
        }
        _ => match resume {
            Some(resume) => {
                let label = resume.label();
                // The album is emphasised in the sentence.
                let (before, album, after) = match label.find(&resume.album) {
                    Some(at) => (
                        &label[..at],
                        &label[at..at + resume.album.len()],
                        &label[at + resume.album.len()..],
                    ),
                    None => (label.as_str(), "", ""),
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let font = theme::regular(14.0);
                    ui.label(RichText::new(before).font(font.clone()).color(pal.text_2));
                    ui.label(
                        RichText::new(album)
                            .font(theme::medium(14.0))
                            .color(pal.text),
                    );
                    ui.label(RichText::new(after).font(font).color(pal.text_2));
                });
            }
            None => {
                let text = aulos_core::fl!(
                    "home-subtitle",
                    count = i64::try_from(library_albums).unwrap_or(i64::MAX)
                );
                ui.label(
                    RichText::new(text)
                        .font(theme::regular(14.0))
                        .color(pal.text_2),
                );
            }
        },
    }
}

/// The two skewed bars of the brand mark, centred on `center`.
fn brand_mark(ui: &Ui, center: egui::Pos2, pal: &theme::Palette) {
    let skew = -14.0_f32.to_radians().tan();
    let bars = [
        (10.0_f32, 52.0_f32, pal.reed, -8.5),
        (10.0, 38.0, pal.stroke_strong, 8.5),
    ];
    for (w, h, color, dx) in bars {
        let bottom = 26.0;
        let pts = [
            (-w / 2.0, bottom - h),
            (w / 2.0, bottom - h),
            (w / 2.0, bottom),
            (-w / 2.0, bottom),
        ]
        .into_iter()
        .map(|(x, y)| pos2(center.x + dx + x + skew * y, center.y + y))
        .collect();
        ui.painter()
            .add(Shape::convex_polygon(pts, color, egui::Stroke::NONE));
    }
}

/// The empty library: the welcome block (no shelves can exist yet).
fn welcome(app: &App, ui: &mut Ui, out: &mut Out) {
    let pal = app.pal;
    let scanning = app.session.library.scanning;
    let avail = ui.available_rect_before_wrap();
    ui.allocate_ui_with_layout(avail.size(), Layout::top_down(Align::Center), |ui| {
        ui.add_space((avail.height() * 0.5 - 130.0).max(24.0));
        let (mark, _) = ui.allocate_exact_size(vec2(60.0, 56.0), Sense::hover());
        brand_mark(ui, mark.center(), &pal);
        ui.add_space(14.0);
        ui.label(
            RichText::new(aulos_core::fl!("home-welcome-title"))
                .font(theme::semibold(22.0))
                .color(pal.text),
        );
        ui.add_space(8.0);
        let body = if scanning {
            aulos_core::fl!("home-empty-scanning-body")
        } else {
            aulos_core::fl!("home-empty-body")
        };
        ui.allocate_ui(vec2(avail.width().min(420.0), 60.0), |ui| {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.label(
                    RichText::new(body)
                        .font(theme::regular(14.0))
                        .color(pal.text_2),
                );
            });
        });
        ui.add_space(8.0);
        let width = 160.0 + 120.0 + 8.0;
        ui.allocate_ui_with_layout(
            vec2(width, 34.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if pill_button(
                    ui,
                    &aulos_core::fl!("home-open-settings"),
                    Some(regular::GEAR),
                    PillKind::Accent,
                    34.0,
                    0.0,
                )
                .clicked()
                {
                    out.act = Some(Act::Goto(Route::Settings));
                }
                if pill_button(
                    ui,
                    &aulos_core::fl!("home-browse-radio"),
                    Some(regular::BROADCAST),
                    PillKind::Secondary,
                    34.0,
                    0.0,
                )
                .clicked()
                {
                    out.act = Some(Act::Goto(Route::Radio));
                }
            },
        );
    });
}

/// History empty but music present: the design's centred "this page fills
/// up" block with [Shuffle everything] [Browse albums].
fn no_history(ui: &mut Ui, cx: &mut Cx) {
    let pal = cx.app.pal;
    ui.vertical_centered(|ui| {
        ui.add_space(24.0);
        let (mark, _) = ui.allocate_exact_size(vec2(60.0, 56.0), Sense::hover());
        brand_mark(ui, mark.center(), &pal);
        ui.add_space(14.0);
        ui.label(
            RichText::new(aulos_core::fl!("home-welcome-title"))
                .font(theme::semibold(22.0))
                .color(pal.text),
        );
        ui.add_space(8.0);
        ui.allocate_ui(vec2(ui.available_width().min(420.0), 44.0), |ui| {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.label(
                    RichText::new(aulos_core::fl!("home-no-history-hint"))
                        .font(theme::regular(14.0))
                        .color(pal.text_2),
                );
            });
        });
        ui.add_space(8.0);
        let width = 170.0 + 130.0 + 8.0;
        // Phones: one button per line (the labels outgrow a shared row).
        let stacked = cx.app.compact();
        let (size, layout) = if stacked {
            (
                vec2(ui.available_width(), 34.0 * 2.0 + 8.0),
                Layout::top_down(Align::Center),
            )
        } else {
            (vec2(width, 34.0), Layout::left_to_right(Align::Center))
        };
        ui.allocate_ui_with_layout(size, layout, |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            if pill_button(
                ui,
                &aulos_core::fl!("home-shuffle-everything"),
                Some(regular::SHUFFLE),
                PillKind::Accent,
                34.0,
                0.0,
            )
            .clicked()
            {
                cx.out.act = Some(Act::ShuffleAll);
            }
            if pill_button(
                ui,
                &aulos_core::fl!("home-browse-albums"),
                None,
                PillKind::Secondary,
                34.0,
                0.0,
            )
            .clicked()
            {
                cx.out.act = Some(Act::Goto(Route::Albums));
            }
        });
        ui.add_space(24.0);
    });
}

/// Pulsing placeholder shelves while the history is loading.
fn loading_shelves(ui: &mut Ui) {
    for _ in 0..2 {
        let (head, _) = ui.allocate_exact_size(vec2(160.0, 20.0), Sense::hover());
        skeleton(ui, head, 5);
        ui.add_space(12.0);
        let n = ((ui.available_width() + CARD_GAP) / (CARD + CARD_GAP))
            .floor()
            .max(1.0) as usize;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CARD_GAP;
            for _ in 0..n {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(CARD, CARD + CARD_TEXT), Sense::hover());
                skeleton(ui, Rect::from_min_size(rect.min, vec2(CARD, CARD)), 8);
                skeleton(
                    ui,
                    Rect::from_min_size(rect.min + vec2(0.0, CARD + 8.0), vec2(CARD * 0.8, 14.0)),
                    4,
                );
                skeleton(
                    ui,
                    Rect::from_min_size(rect.min + vec2(0.0, CARD + 28.0), vec2(CARD * 0.55, 12.0)),
                    4,
                );
            }
        });
        ui.add_space(SECTION_GAP);
    }
}

// ---------------------------------------------------------------------------
// Shelves
// ---------------------------------------------------------------------------

struct Head<'a> {
    title: &'a str,
    hint: Option<&'a str>,
    /// An icon button at the right of the title: glyph and tooltip.
    action: Option<(&'a str, &'a str)>,
    /// Show a "See all" link before the scroll arrows.
    see_all: bool,
}

/// What the header of a [`shelf`] reported.
#[derive(Default)]
struct Clicks {
    action: bool,
    see_all: bool,
}

/// A titled, horizontally scrolling row; `body` draws the cards. Returns
/// which header controls were clicked.
fn shelf(
    ui: &mut Ui,
    offsets: &mut HashMap<Id, f32>,
    id: Id,
    head: Head,
    body: impl FnOnce(&mut Ui),
) -> Clicks {
    let pal = Theme::get(ui.ctx()).pal;
    let mut clicks = Clicks::default();
    let mut step = 0.0_f32;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(head.title)
                .font(theme::semibold(17.0))
                .color(pal.text),
        );
        if let Some(h) = head.hint {
            ui.add_space(4.0);
            ui.label(
                RichText::new(h)
                    .font(theme::regular(12.5))
                    .color(pal.text_3),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if IconBtn::new(regular::CARET_RIGHT)
                .size(26.0)
                .icon_size(14.0)
                .show(ui)
                .clicked()
            {
                step = 1.0;
            }
            if IconBtn::new(regular::CARET_LEFT)
                .size(26.0)
                .icon_size(14.0)
                .show(ui)
                .clicked()
            {
                step = -1.0;
            }
            if let Some((glyph, tip)) = head.action
                && IconBtn::new(glyph)
                    .size(26.0)
                    .icon_size(15.0)
                    .tip(tip)
                    .show(ui)
                    .clicked()
            {
                clicks.action = true;
            }
            if head.see_all {
                ui.add_space(4.0);
                clicks.see_all = see_all_link(ui, pal.text_2);
            }
        });
    });
    ui.add_space(8.0);
    let off = offsets.entry(id).or_insert(0.0);
    *off = (*off + step * ui.available_width() * 0.8).max(0.0);
    let out = ScrollArea::horizontal()
        .id_salt(id)
        .auto_shrink([false, true])
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
        .horizontal_scroll_offset(*off)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = CARD_GAP;
                body(ui);
            });
        });
    *off = out.state.offset.x;
    clicks
}

/// A text link ("See all", "Play all"); true when clicked.
fn link(ui: &mut Ui, text: &str, color: Color32) -> bool {
    ui.add(
        egui::Label::new(RichText::new(text).font(theme::regular(13.0)).color(color))
            .sense(Sense::click()),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .clicked()
}

fn see_all_link(ui: &mut Ui, color: Color32) -> bool {
    link(ui, &aulos_core::fl!("home-see-all"), color)
}

#[allow(clippy::too_many_arguments)]
fn album_shelf(
    ui: &mut Ui,
    cx: &mut Cx,
    offsets: &mut HashMap<Id, f32>,
    id: &str,
    title: &str,
    hint: Option<&str>,
    see_all: Option<Act>,
    albums: &[AlbumRef],
) {
    let head = Head {
        title,
        hint,
        action: None,
        see_all: see_all.is_some(),
    };
    let clicks = shelf(ui, offsets, Id::new(("home-shelf", id)), head, |ui| {
        for a in albums {
            album_card(ui, cx, a);
        }
    });
    if clicks.see_all {
        cx.out.act = see_all;
    }
}

/// A shelf album card (allocates its own slot).
fn album_card(ui: &mut Ui, cx: &mut Cx, a: &AlbumRef) {
    let (rect, _) = ui.allocate_exact_size(vec2(CARD, CARD + CARD_TEXT), Sense::hover());
    album_card_at(ui, cx, rect, a);
}

/// An album card inside `rect` (width = cover size; text below).
fn album_card_at(ui: &mut Ui, cx: &mut Cx, rect: Rect, a: &AlbumRef) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let pal = cx.app.pal;
    let w = rect.width();
    let resp = ui.interact(
        rect,
        ui.unique_id().with(("album", &a.artist, &a.name)),
        Sense::click(),
    );
    let art = Rect::from_min_size(rect.min, vec2(w, w));
    let key = CoverArt::album_key(&a.artist, &a.name);
    cover_image(ui, &cx.app.covers, art, cx.app.covers.thumb(&key), &key, 8);

    let play_rect = Rect::from_center_size(
        pos2(art.right() - 30.0, art.bottom() - 30.0),
        vec2(40.0, 40.0),
    );
    let play = ui.interact(play_rect, resp.id.with("play"), Sense::click());
    if resp.hovered() || play.hovered() {
        ui.painter()
            .rect_filled(art, CornerRadius::same(8), Color32::BLACK.alpha(0.28));
        let bg = if play.hovered() {
            pal.text
        } else {
            pal.text.alpha(0.92)
        };
        ui.painter().circle_filled(play_rect.center(), 20.0, bg);
        paint_icon(
            ui.painter(),
            play_rect.center(),
            fill::PLAY,
            17.0,
            pal.bg,
            true,
        );
    }

    let more =
        crate::widgets::more_trigger(ui, resp.id, art, resp.hovered() || play.hovered(), 22.0);

    let title_rect = Rect::from_min_size(pos2(rect.left(), art.bottom() + 8.0), vec2(w, 18.0));
    if a.name.trim().is_empty() {
        paint_text(
            ui.painter(),
            title_rect,
            Align::Min,
            &aulos_core::fl!("unknown-album"),
            theme::semibold(13.5),
            pal.text_2,
        );
    } else {
        paint_text(
            ui.painter(),
            title_rect,
            Align::Min,
            &a.name,
            theme::semibold(13.5),
            pal.text,
        );
    }
    let sub_rect = Rect::from_min_size(pos2(rect.left(), art.bottom() + 26.0), vec2(w, 16.0));
    paint_text(
        ui.painter(),
        sub_rect,
        Align::Min,
        &a.artist,
        theme::regular(12.5),
        pal.text_2,
    );

    if play.clicked() {
        cx.out.act = Some(Act::PlayAlbum(a.artist.clone(), a.name.clone()));
    } else if resp.clicked() {
        cx.out.act = Some(Act::OpenAlbum(a.artist.clone(), a.name.clone()));
    }
    if resp.secondary_clicked()
        || resp.context_menu_opened()
        || more.clicked()
        || more.context_menu_opened()
    {
        let lib = &cx.app.session.library;
        let playlists = &cx.app.session.playlists.playlists;
        if let Some(idx) = lib.find_album_exact(&a.artist, &a.name)
            && let Some(menu) = menus::album_menu(&resp, &lib.albums[idx], playlists)
                .or_else(|| menus::album_menu_click(&more, &lib.albums[idx], playlists))
        {
            cx.out.menu = Some(menu);
        }
    }
}

#[derive(Clone, Copy)]
enum ArtistKind<'a> {
    /// An artist of the library: opens the artist page.
    Library,
    /// A suggestion not in the library: opens a web page.
    Discover { url: &'a str, site: &'a str },
}

/// A round artist card.
fn artist_card(ui: &mut Ui, cx: &mut Cx, name: &str, sub: &str, kind: ArtistKind) {
    let (rect, _) = ui.allocate_exact_size(vec2(CARD, CARD + CARD_TEXT), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let pal = cx.app.pal;
    let resp = ui.interact(rect, ui.id().with(("artist", name)), Sense::click());
    let art = Rect::from_min_size(rect.min, vec2(CARD, CARD));
    let library = matches!(kind, ArtistKind::Library);
    if cx.app.covers.photo(name).is_none() && !cx.asked.contains(name) {
        cx.asked.insert(name.to_owned());
        if library {
            cx.out.photos.push(name.to_owned());
        } else {
            cx.out.suggested_photos.push(name.to_owned());
        }
    }
    avatar_tile(
        ui,
        &cx.app.covers,
        art,
        name,
        cx.app.covers.photo(name),
        true,
    );

    let play_rect = Rect::from_center_size(
        pos2(art.right() - 22.0, art.bottom() - 22.0),
        vec2(40.0, 40.0),
    );
    let play = library.then(|| ui.interact(play_rect, resp.id.with("play"), Sense::click()));
    let play_hovered = play.as_ref().is_some_and(|p| p.hovered());
    let more = library.then(|| {
        crate::widgets::more_trigger(ui, resp.id, art, resp.hovered() || play_hovered, 18.0)
    });
    if library && (resp.hovered() || play_hovered) {
        let bg = if play_hovered {
            pal.text
        } else {
            pal.text.alpha(0.92)
        };
        ui.painter().circle_filled(play_rect.center(), 20.0, bg);
        paint_icon(
            ui.painter(),
            play_rect.center(),
            fill::PLAY,
            17.0,
            pal.bg,
            true,
        );
    }
    let name_rect = Rect::from_min_size(pos2(rect.left(), art.bottom() + 8.0), vec2(CARD, 18.0));
    paint_text(
        ui.painter(),
        name_rect,
        Align::Min,
        name,
        theme::semibold(13.5),
        pal.text,
    );
    let sub_rect = Rect::from_min_size(pos2(rect.left(), art.bottom() + 26.0), vec2(CARD, 16.0));
    paint_text(
        ui.painter(),
        sub_rect,
        Align::Min,
        sub,
        theme::regular(12.5),
        pal.text_2,
    );

    match kind {
        ArtistKind::Library => {
            if play.is_some_and(|p| p.clicked()) {
                cx.out.act = Some(Act::PlayArtist(name.to_owned()));
            } else if resp.clicked() {
                cx.out.act = Some(Act::OpenArtist(name.to_owned()));
            }
            if let Some(menu) = menus::artist_menu(&resp, name).or_else(|| {
                more.as_ref()
                    .and_then(|m| menus::artist_menu_click(m, name))
            }) {
                cx.out.menu = Some(menu);
            }
        }
        ArtistKind::Discover { url, site } => {
            let resp = resp.on_hover_text(aulos_core::fl!(
                "home-open-on-site",
                name = name,
                site = site
            ));
            if resp.clicked() {
                cx.out.act = Some(Act::OpenUrl(url.to_owned()));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Decades
// ---------------------------------------------------------------------------

/// Favourites of the month and the decade tiles: side by side when there is
/// room (each at least 300px wide), stacked otherwise.
fn favourites_and_decades(ui: &mut Ui, cx: &mut Cx, data: &HomeData) {
    let favourites = !data.favourites.is_empty();
    let decades = !data.decades.is_empty();
    if !favourites && !decades {
        return;
    }
    const COLUMN_GAP: f32 = 32.0;
    if favourites && decades && ui.available_width() >= 2.0 * 300.0 + COLUMN_GAP {
        ui.spacing_mut().item_spacing.x = COLUMN_GAP;
        ui.columns(2, |cols| {
            favourites_list(&mut cols[0], cx, data);
            decade_tiles(&mut cols[1], cx, &data.decades);
        });
    } else {
        if favourites {
            favourites_list(ui, cx, data);
            if decades {
                ui.add_space(SECTION_GAP);
            }
        }
        if decades {
            decade_tiles(ui, cx, &data.decades);
        }
    }
    ui.add_space(SECTION_GAP);
}

/// "Favourites of October · 12 songs · Play all" over a grid of 48px rows
/// (36px cover, title, artist), as many 200px+ columns as fit.
fn favourites_list(ui: &mut Ui, cx: &mut Cx, data: &HomeData) {
    const ROW_H: f32 = 48.0;
    const COL_GAP: f32 = 16.0;
    let pal = cx.app.pal;
    let title = if data.favourites_this_month {
        aulos_core::fl!(
            "home-favourites-of-month",
            month = month_name(chrono::Datelike::month(&chrono::Local::now()))
        )
    } else {
        aulos_core::fl!("home-favourites")
    };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .font(theme::semibold(17.0))
                .color(pal.text),
        );
        ui.add_space(2.0);
        let songs = data.favourites_total;
        ui.label(
            RichText::new(aulos_core::fl!("home-song-count", count = i64::from(songs)))
                .font(theme::regular(13.0))
                .color(pal.text_3),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if link(ui, &aulos_core::fl!("play-all"), pal.text_2) {
                cx.out.act = Some(Act::PlayFavourites(0));
            }
        });
    });
    ui.add_space(8.0);

    let width = ui.available_width();
    let cols = (((width + COL_GAP) / (200.0 + COL_GAP)).floor() as usize).max(1);
    let col_w = (width - COL_GAP * (cols as f32 - 1.0)) / cols as f32;
    let rows = data.favourites.len().div_ceil(cols);
    let (area, _) = ui.allocate_exact_size(vec2(width, rows as f32 * ROW_H), Sense::hover());
    for (i, track) in data.favourites.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let rect = Rect::from_min_size(
            pos2(
                area.left() + c as f32 * (col_w + COL_GAP),
                area.top() + r as f32 * ROW_H,
            ),
            vec2(col_w, ROW_H),
        );
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let resp = ui.interact(rect, ui.id().with(("favourite", track.id)), Sense::click());
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), pal.text.alpha(0.06));
        }
        let cover = Rect::from_min_size(
            pos2(rect.left() + 6.0, rect.center().y - 18.0),
            vec2(36.0, 36.0),
        );
        let key = track.cover_key();
        cover_image(
            ui,
            &cx.app.covers,
            cover,
            cx.app.covers.thumb(&key),
            &key,
            4,
        );
        let x = cover.right() + 10.0;
        let text_w = (rect.right() - 6.0 - x).max(0.0);
        paint_text(
            ui.painter(),
            Rect::from_min_size(pos2(x, rect.center().y - 17.0), vec2(text_w, 17.0)),
            Align::Min,
            &track.title,
            theme::semibold(13.5),
            pal.text,
        );
        paint_text(
            ui.painter(),
            Rect::from_min_size(pos2(x, rect.center().y + 1.0), vec2(text_w, 15.0)),
            Align::Min,
            &track.artist,
            theme::regular(12.0),
            pal.text_2,
        );
        if resp.clicked() {
            cx.out.act = Some(Act::PlayFavourites(i));
        }
        if (resp.secondary_clicked() || resp.context_menu_opened())
            && let Some(menu) = menus::track_menu(&resp, track, &cx.app.session.playlists.playlists)
        {
            cx.out.menu = Some(menu);
        }
    }
}

/// The "Decades" heading over a fixed three-column grid of 64px tiles.
fn decade_tiles(ui: &mut Ui, cx: &mut Cx, decades: &[DecadeCount]) {
    let pal = cx.app.pal;
    ui.label(
        RichText::new(aulos_core::fl!("home-decades"))
            .font(theme::semibold(17.0))
            .color(pal.text),
    );
    ui.add_space(10.0);
    let gap = 8.0;
    let width = ui.available_width();
    let tile_w = ((width - gap * 2.0) / 3.0).floor();
    for row in decades.chunks(3) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for d in row {
                let (rect, resp) = ui.allocate_exact_size(vec2(tile_w, 64.0), Sense::click());
                let label = decade_label(d.decade);
                let (from, to) = seed_colors(&label);
                gradient_fill(&cx.app.covers, ui, rect, 8, from, to);
                if resp.hovered() {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::same(8),
                        Color32::WHITE.alpha(0.1),
                    );
                }
                let inner = rect.shrink2(vec2(12.0, 9.0));
                let count = aulos_core::fl!("home-decade-albums", count = i64::from(d.albums));
                let top = Rect::from_min_size(
                    pos2(inner.left(), inner.bottom() - 34.0),
                    vec2(inner.width(), 19.0),
                );
                let bottom = Rect::from_min_size(
                    pos2(inner.left(), inner.bottom() - 15.0),
                    vec2(inner.width(), 15.0),
                );
                paint_text(
                    ui.painter(),
                    top,
                    Align::Min,
                    &label,
                    theme::semibold(15.0),
                    Color32::WHITE,
                );
                paint_text(
                    ui.painter(),
                    bottom,
                    Align::Min,
                    &count,
                    theme::regular(11.5),
                    Color32::WHITE.alpha(0.88),
                );
                if resp.clicked() {
                    cx.out.act = Some(Act::OpenDecade(d.decade));
                }
            }
        });
        ui.add_space(gap);
    }
}

/// The album grid of the open decade, with Back and Shuffle play.
fn decade_page(app: &App, ui: &mut Ui, out: &mut Out) {
    let pal = app.pal;
    let Some(view) = &app.session.home.decade else {
        return;
    };
    let pad = app.page_pad();
    let ipad = pad as i8;
    Frame::NONE
        .inner_margin(Margin {
            left: ipad,
            right: ipad,
            top: 4,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if IconBtn::new(regular::ARROW_LEFT)
                    .size(30.0)
                    .icon_size(16.0)
                    .tip(aulos_core::fl!("home-back"))
                    .show(ui)
                    .clicked()
                {
                    out.act = Some(Act::CloseDecade);
                }
                ui.label(
                    RichText::new(aulos_core::fl!("home-decade-title", decade = view.decade))
                        .font(theme::semibold(26.0))
                        .color(pal.text),
                );
                if let Some(albums) = &view.albums {
                    ui.label(
                        RichText::new(albums.len().to_string())
                            .font(theme::mono(13.0))
                            .color(pal.text_3),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let enabled = view.albums.as_ref().is_some_and(|a| !a.is_empty());
                    let r = crate::widgets::pill_button_enabled(
                        ui,
                        &aulos_core::fl!("home-decade-play"),
                        Some(regular::SHUFFLE),
                        PillKind::Accent,
                        32.0,
                        0.0,
                        enabled,
                    );
                    if r.clicked() {
                        out.act = Some(Act::PlayDecade);
                    }
                });
            });
            ui.add_space(12.0);
        });

    let mut asked = HashSet::new();
    let mut cx = Cx {
        app,
        out,
        asked: &mut asked,
    };
    match &view.albums {
        None => {
            Frame::NONE
                .inner_margin(Margin {
                    left: ipad,
                    right: ipad,
                    top: 0,
                    bottom: 0,
                })
                .show(ui, |ui| {
                    let cols = (((ui.available_width() + CARD_GAP) / (CARD + CARD_GAP)).floor()
                        as usize)
                        .max(1);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = CARD_GAP;
                        for _ in 0..cols {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(CARD, CARD), Sense::hover());
                            skeleton(ui, rect, 8);
                        }
                    });
                });
        }
        Some(albums) if albums.is_empty() => {
            empty_state(
                ui,
                regular::VINYL_RECORD,
                &aulos_core::fl!("home-decade-empty-title"),
                &aulos_core::fl!("home-decade-empty-hint"),
                None,
            );
        }
        Some(albums) => {
            ScrollArea::vertical()
                .auto_shrink(false)
                .show_viewport(ui, |ui, viewport| {
                    let width = ui.available_width() - 2.0 * pad;
                    let cols = (((width + CARD_GAP) / (CARD + CARD_GAP)).floor() as usize).max(1);
                    let cw = ((width - CARD_GAP * (cols as f32 - 1.0)) / cols as f32)
                        .floor()
                        .max(80.0);
                    let row_h = cw + CARD_TEXT + 24.0;
                    let rows = albums.len().div_ceil(cols);
                    let (full, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), row_h * rows as f32 + 32.0),
                        Sense::hover(),
                    );
                    let first = (viewport.min.y / row_h).floor().max(0.0) as usize;
                    let last = ((viewport.max.y / row_h).ceil() as usize + 1).min(rows);
                    for row in first..last {
                        let top = full.top() + row as f32 * row_h;
                        for (i, a) in albums.iter().skip(row * cols).take(cols).enumerate() {
                            let x = full.left() + pad + i as f32 * (cw + CARD_GAP);
                            let rect = Rect::from_min_size(pos2(x, top), vec2(cw, cw + CARD_TEXT));
                            album_card_at(ui, &mut cx, rect, a);
                        }
                    }
                });
        }
    }
}

/// The localized name of calendar month `month` (1–12).
fn month_name(month: u32) -> String {
    match month {
        1 => aulos_core::fl!("home-month-1"),
        2 => aulos_core::fl!("home-month-2"),
        3 => aulos_core::fl!("home-month-3"),
        4 => aulos_core::fl!("home-month-4"),
        5 => aulos_core::fl!("home-month-5"),
        6 => aulos_core::fl!("home-month-6"),
        7 => aulos_core::fl!("home-month-7"),
        8 => aulos_core::fl!("home-month-8"),
        9 => aulos_core::fl!("home-month-9"),
        10 => aulos_core::fl!("home-month-10"),
        11 => aulos_core::fl!("home-month-11"),
        _ => aulos_core::fl!("home-month-12"),
    }
}
