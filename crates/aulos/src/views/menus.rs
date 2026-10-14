//! Right-click menus for tracks, albums and artists, and the actions they
//! produce. Menus draw as a popup: the main panel, and beside it the
//! "Add to playlist" submenu while that row is hovered.

use super::overlays;
use crate::app::{App, Route, Sheet};
use crate::theme::{self, Palette, Theme};
use crate::widgets::paint_icon;
use aulos_core::library::queue_source::QueueSource;
use aulos_core::library::{Album, Playlist, Track};
use aulos_core::session::QueuePlacement;
use aulos_core::tagger::{TagAction, is_taggable};
use egui::{
    Align2, Context, CornerRadius, Frame, Id, Margin, Popup, Rect, Response, ScrollArea, Sense,
    Stroke, Ui, pos2, vec2,
};
use egui_phosphor::regular;

const MENU_W: f32 = 224.0;
const SUB_W: f32 = 200.0;
const ITEM_H: f32 = 30.0;

#[derive(Default)]
pub struct State {
    pending: Option<PendingPlaylist>,
}

/// A playlist being created from `save_as_playlist`. Its tracks are added once
/// the playlist shows up in the session's playlist list.
struct PendingPlaylist {
    name: String,
    uris: Vec<String>,
}

pub enum MenuAction {
    PlayTracks(Vec<Track>),
    QueueTracks {
        tracks: Vec<Track>,
        next: bool,
    },
    PlayAlbum {
        artist: String,
        album: String,
    },
    QueueAlbum {
        artist: String,
        album: String,
        next: bool,
    },
    PlayArtist(String),
    QueueArtist {
        name: String,
        next: bool,
    },
    AddToPlaylist {
        playlist: String,
        uris: Vec<String>,
    },
    NewPlaylist(Vec<String>),
    Open(Route),
    TrackProperties(Track),
    AlbumProperties(Album),
    Tag {
        tracks: Vec<Track>,
        action: TagAction,
    },
}

/// Trailing content of a menu row.
pub enum Trailing<'a> {
    None,
    Key(&'a str),
    Caret,
}

#[derive(Clone, Copy)]
enum Play {
    Now,
    Next,
    Queue,
}

/// What a menu shows: the play group, an optional playlist submenu, and any
/// rows below the separator. Closures run only while the menu is open.
struct Spec<'a> {
    playlists: Option<&'a [Playlist]>,
    uris: &'a dyn Fn() -> Vec<String>,
    play: &'a dyn Fn(Play) -> MenuAction,
    tail: &'a dyn Fn(&mut Ui, Palette) -> Option<MenuAction>,
}

pub fn track_menu(
    response: &Response,
    track: &Track,
    playlists: &[Playlist],
) -> Option<MenuAction> {
    show_menu(
        response,
        Spec {
            playlists: Some(playlists),
            uris: &|| vec![track.source_uri.clone()],
            play: &|play: Play| match play {
                Play::Now => MenuAction::PlayTracks(vec![track.clone()]),
                Play::Next => MenuAction::QueueTracks {
                    tracks: vec![track.clone()],
                    next: true,
                },
                Play::Queue => MenuAction::QueueTracks {
                    tracks: vec![track.clone()],
                    next: false,
                },
            },
            tail: &|ui: &mut Ui, pal: Palette| {
                separator(ui, pal);
                let mut out = None;
                if menu_item(ui, regular::MICROPHONE, "Go to artist", Trailing::None).clicked() {
                    out = Some(MenuAction::Open(Route::Artist(
                        track.grouping_artist().to_owned(),
                    )));
                }
                if menu_item(ui, regular::INFO, "Properties", Trailing::Key("Alt ↵")).clicked() {
                    out = Some(MenuAction::TrackProperties(track.clone()));
                }
                if is_taggable(track) {
                    out = tag_items(ui, std::slice::from_ref(track)).or(out);
                }
                out
            },
        },
    )
}

/// The tag-editor rows shared by the track and album menus.
fn tag_items(ui: &mut Ui, tracks: &[Track]) -> Option<MenuAction> {
    let mut out = None;
    for (icon, label, action) in [
        (regular::TAG, "Edit tags…", TagAction::Edit),
        (
            regular::MAGNIFYING_GLASS,
            "Look up on MusicBrainz",
            TagAction::LookUp,
        ),
        (
            regular::FINGERPRINT,
            "Scan with AcoustID",
            TagAction::AcoustId,
        ),
    ] {
        if menu_item(ui, icon, label, Trailing::None).clicked() {
            out = Some(MenuAction::Tag {
                tracks: tracks.to_vec(),
                action,
            });
        }
    }
    out
}

pub fn album_menu(
    response: &Response,
    album: &Album,
    playlists: &[Playlist],
) -> Option<MenuAction> {
    show_menu(
        response,
        Spec {
            playlists: Some(playlists),
            uris: &|| album.tracks.iter().map(|t| t.source_uri.clone()).collect(),
            play: &|play: Play| match play {
                Play::Now => MenuAction::PlayAlbum {
                    artist: album.artist.clone(),
                    album: album.name.clone(),
                },
                Play::Next => MenuAction::QueueAlbum {
                    artist: album.artist.clone(),
                    album: album.name.clone(),
                    next: true,
                },
                Play::Queue => MenuAction::QueueAlbum {
                    artist: album.artist.clone(),
                    album: album.name.clone(),
                    next: false,
                },
            },
            tail: &|ui: &mut Ui, pal: Palette| {
                separator(ui, pal);
                let mut out = None;
                if menu_item(ui, regular::INFO, "Album properties", Trailing::None).clicked() {
                    out = Some(MenuAction::AlbumProperties(album.clone()));
                }
                if menu_item(ui, regular::MICROPHONE, "Go to artist", Trailing::None).clicked() {
                    out = Some(MenuAction::Open(Route::Artist(album.artist.clone())));
                }
                if album.tracks.iter().any(is_taggable) {
                    out = tag_items(ui, &album.tracks).or(out);
                }
                out
            },
        },
    )
}

pub fn artist_menu(response: &Response, name: &str) -> Option<MenuAction> {
    show_menu(
        response,
        Spec {
            playlists: None,
            uris: &|| Vec::new(),
            play: &|play: Play| match play {
                Play::Now => MenuAction::PlayArtist(name.to_owned()),
                Play::Next => MenuAction::QueueArtist {
                    name: name.to_owned(),
                    next: true,
                },
                Play::Queue => MenuAction::QueueArtist {
                    name: name.to_owned(),
                    next: false,
                },
            },
            tail: &|ui: &mut Ui, pal: Palette| {
                separator(ui, pal);
                let mut out = None;
                if menu_item(ui, regular::MICROPHONE, "Go to artist", Trailing::None).clicked() {
                    out = Some(MenuAction::Open(Route::Artist(name.to_owned())));
                }
                out
            },
        },
    )
}

/// One row of a context menu: 30px, icon, label, optional trailing content.
/// The caller decides what a click means, and closes the menu.
pub fn menu_item(ui: &mut Ui, icon: &str, label: &str, trailing: Trailing) -> Response {
    let pal = Theme::get(ui.ctx()).pal;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, ITEM_H), Sense::click());
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, CornerRadius::same(6), pal.seg_sel);
    }
    let cy = rect.center().y;
    painter.text(
        pos2(rect.left() + 15.5, cy),
        Align2::CENTER_CENTER,
        icon,
        theme::icon(15.0),
        pal.text_2,
    );
    painter.text(
        pos2(rect.left() + 31.0, cy),
        Align2::LEFT_CENTER,
        label,
        theme::regular(13.0),
        pal.text,
    );
    match trailing {
        Trailing::None => {}
        Trailing::Key(key) => {
            painter.text(
                pos2(rect.right() - 8.0, cy),
                Align2::RIGHT_CENTER,
                key,
                theme::mono(11.0),
                pal.text_3,
            );
        }
        Trailing::Caret => paint_icon(
            painter,
            pos2(rect.right() - 13.0, cy),
            regular::CARET_RIGHT,
            12.0,
            pal.text_3,
            false,
        ),
    }
    response
}

/// Pops the menu for `response` and returns the row the user picked.
fn show_menu(response: &Response, spec: Spec) -> Option<MenuAction> {
    let ctx = response.ctx.clone();
    let pal = Theme::get(&ctx).pal;
    let sub_key = Id::new(("aulos_menu_sub", response.id));
    // Submenu rect from the previous frame, so the pointer can cross into it.
    let last_sub: Option<Rect> = ctx.data(|d| d.get_temp::<Option<Rect>>(sub_key)).flatten();

    Popup::context_menu(response)
        .frame(Frame::NONE)
        .show(|ui| {
            let mut out = None;
            let mut sub_rect = None;
            ui.horizontal_top(|ui| {
                let (main_rect, (main_out, row)) =
                    panel(ui, pal, MENU_W, |ui| main_column(ui, pal, &spec));
                out = main_out;
                let out = &mut out;
                if let Some((row_rect, row_hovered)) = row {
                    let open = row_hovered || last_sub.is_some_and(|r| pointer_in(&ctx, r));
                    if open {
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.add_space(row_rect.top() - main_rect.top());
                            let (rect, sub_out) =
                                panel(ui, pal, SUB_W, |ui| submenu(ui, pal, &spec));
                            sub_rect = Some(rect);
                            if out.is_none() {
                                *out = sub_out;
                            }
                        });
                    }
                }
            });
            ctx.data_mut(|d| d.insert_temp(sub_key, sub_rect));
            if out.is_some() {
                ui.close();
            }
            out
        })
        .and_then(|r| r.inner)
}

fn main_column(
    ui: &mut Ui,
    pal: Palette,
    spec: &Spec,
) -> (Option<MenuAction>, Option<(Rect, bool)>) {
    let mut out = None;
    let mut row = None;
    if menu_item(ui, regular::PLAY, "Play", Trailing::Key("Enter")).clicked() {
        out = Some((spec.play)(Play::Now));
    }
    if menu_item(
        ui,
        regular::ARROW_BEND_DOWN_RIGHT,
        "Play next",
        Trailing::None,
    )
    .clicked()
    {
        out = Some((spec.play)(Play::Next));
    }
    if menu_item(ui, regular::LIST_PLUS, "Add to queue", Trailing::None).clicked() {
        out = Some((spec.play)(Play::Queue));
    }
    if spec.playlists.is_some() {
        let item = menu_item(ui, regular::PLAYLIST, "Add to playlist", Trailing::Caret);
        row = Some((item.rect, item.hovered()));
    }
    let tail = (spec.tail)(ui, pal);
    out = out.or(tail);
    (out, row)
}

fn submenu(ui: &mut Ui, pal: Palette, spec: &Spec) -> Option<MenuAction> {
    let uris = (spec.uris)();
    let mut out = None;
    if menu_item(ui, regular::PLUS, "New playlist…", Trailing::None).clicked() {
        out = Some(MenuAction::NewPlaylist(uris.clone()));
    }
    separator(ui, pal);
    let playlists = spec.playlists.unwrap_or(&[]);
    if playlists.is_empty() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("No playlists yet")
                .font(theme::regular(12.5))
                .color(pal.text_3),
        );
        ui.add_space(4.0);
    } else {
        ScrollArea::vertical()
            .max_height(280.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for playlist in playlists {
                    if menu_item(ui, regular::PLAYLIST, &playlist.name, Trailing::None).clicked() {
                        out = Some(MenuAction::AddToPlaylist {
                            playlist: playlist.id.clone(),
                            uris: uris.clone(),
                        });
                    }
                }
            });
    }
    out
}

/// A menu panel: 1px border, 10px radius, 5px padding, drop shadow.
fn panel<R>(ui: &mut Ui, pal: Palette, width: f32, body: impl FnOnce(&mut Ui) -> R) -> (Rect, R) {
    let frame = Frame::new()
        .fill(pal.surface_3)
        .stroke(Stroke::new(1.0, pal.stroke_strong))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(5))
        .shadow(theme::shadow([0, 16], 40, 128))
        .show(ui, |ui| {
            // Content width = outer width minus padding (2 × 5) and border (2 × 1).
            ui.set_width(width - 12.0);
            body(ui)
        });
    (frame.response.rect, frame.inner)
}

fn separator(ui: &mut Ui, pal: Palette) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 9.0), Sense::hover());
    ui.painter().hline(
        rect.left() + 6.0..=rect.right() - 6.0,
        rect.center().y,
        Stroke::new(1.0, pal.stroke_strong),
    );
}

fn pointer_in(ctx: &Context, rect: Rect) -> bool {
    ctx.input(|i| i.pointer.hover_pos())
        .is_some_and(|p| rect.contains(p))
}

/// Runs a menu action against the app. Browse actions navigate, playback
/// actions go through the session, and dialogs open as overlays.
pub fn apply(app: &mut App, action: MenuAction) {
    match action {
        MenuAction::PlayTracks(tracks) => {
            let update = app.session.play_tracks(tracks, 0);
            app.run(update);
        }
        MenuAction::QueueTracks { tracks, next } => {
            let placement = if next {
                QueuePlacement::Next
            } else {
                QueuePlacement::End
            };
            let update = app.session.queue_tracks(tracks, placement);
            app.run(update);
        }
        MenuAction::PlayAlbum { artist, album } => {
            match app.session.library.find_album(&artist, &album) {
                Some(index) => {
                    let update = app.session.play_album(index, 0);
                    app.run(update);
                }
                None => app.toast_error(format!("{album} is not in the library")),
            }
        }
        MenuAction::QueueAlbum {
            artist,
            album,
            next,
        } => match app.session.library.find_album(&artist, &album) {
            Some(index) => {
                let source = QueueSource::Album {
                    album: index,
                    track: None,
                };
                let update = app.session.queue_from(source, next);
                app.run(update);
            }
            None => app.toast_error(format!("{album} is not in the library")),
        },
        MenuAction::PlayArtist(name) => {
            let tracks = artist_tracks(app, &name);
            if tracks.is_empty() {
                app.toast_error(format!("No tracks for {name}"));
            } else {
                let update = app.session.play_tracks(tracks, 0);
                app.run(update);
            }
        }
        MenuAction::QueueArtist { name, next } => {
            let tracks = artist_tracks(app, &name);
            if tracks.is_empty() {
                app.toast_error(format!("No tracks for {name}"));
            } else {
                let placement = if next {
                    QueuePlacement::Next
                } else {
                    QueuePlacement::End
                };
                let update = app.session.queue_tracks(tracks, placement);
                app.run(update);
            }
        }
        MenuAction::AddToPlaylist { playlist, uris } => {
            for uri in uris {
                let effect = app.session.add_to_playlist(playlist.clone(), uri);
                app.run(effect);
            }
        }
        MenuAction::NewPlaylist(uris) => {
            overlays::prompt(
                app,
                "New playlist",
                "Name",
                "Playlist name",
                "Create",
                move |app, name| {
                    save_as_playlist(app, name, uris);
                },
            );
        }
        MenuAction::Open(route) => app.navigate(route),
        MenuAction::TrackProperties(track) => {
            let effect = app.session.open_track_properties(track);
            app.run(effect);
            app.sheet = Some(Sheet::Properties);
        }
        MenuAction::AlbumProperties(album) => {
            let effect = app.session.open_album_properties(album);
            app.run(effect);
            app.sheet = Some(Sheet::Properties);
        }
        MenuAction::Tag { tracks, action } => super::tagger::open_tracks(app, &tracks, action),
    }
}

/// Tracks of every album by `name`, in library order.
fn artist_tracks(app: &App, name: &str) -> Vec<Track> {
    app.session
        .library
        .artists
        .iter()
        .find(|artist| artist.name == name)
        .map(|artist| {
            artist
                .albums
                .iter()
                .flat_map(|album| album.tracks.iter().cloned())
                .collect()
        })
        .unwrap_or_default()
}

/// Creates a playlist and queues `uris` to be added to it once the playlist
/// list shows the new playlist. See `resolve_pending`.
pub fn save_as_playlist(app: &mut App, name: String, uris: Vec<String>) {
    let effect = app.session.create_playlist(name.clone());
    app.views.menus.pending = Some(PendingPlaylist { name, uris });
    app.run(effect);
}

/// Adds the pending playlist's tracks once the playlist exists in the session.
pub fn resolve_pending(app: &mut App) {
    let Some(pending) = app.views.menus.pending.as_ref() else {
        return;
    };
    let Some(id) = app
        .session
        .playlists
        .playlists
        .iter()
        .find(|playlist| playlist.name == pending.name)
        .map(|playlist| playlist.id.clone())
    else {
        return;
    };
    let Some(pending) = app.views.menus.pending.take() else {
        return;
    };
    for uri in pending.uris {
        let effect = app.session.add_to_playlist(id.clone(), uri);
        app.run(effect);
    }
}
