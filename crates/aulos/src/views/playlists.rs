//! The playlists page: the list of saved and smart playlists on the left, the
//! selected playlist (header, rules editor for smart playlists, tracks) on
//! the right, plus the create / rename / delete dialogs and playlist file
//! import / export.

use crate::app::{App, Route};
use crate::theme::{self, ColorExt, Palette};
use crate::views::{home, menus};
use crate::widgets::{
    IconBtn, PillKind, caps_label, cover_image, empty_state, gradient_fill, label_text, paint_icon,
    paint_text, pill_button, pill_button_enabled, quality_badge, seed_colors, segmented, skeleton,
    text_field, toggle,
};
use aulos_core::format::{format_duration, format_duration_coarse};
use aulos_core::library::playlist_io::{ExportOutcome, ExportSource};
use aulos_core::library::smart_playlist::{
    EditorState, MatchMode, OrderField, RuleField, RuleOp, SmartPlaylist,
};
use aulos_core::library::{Playlist, Track};
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Color32, CornerRadius, Frame, Id, Image, Layout, Margin, Modal, Rect, Response,
    RichText, ScrollArea, Sense, Stroke, TextureHandle, Ui, UiBuilder, pos2, vec2,
};
use egui_phosphor::{fill, regular};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc;

const ROW_H: f32 = 40.0;
const ITEM_H: f32 = 48.0;

/// Which playlist the right pane shows.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Sel {
    /// A provider playlist, by `Playlist::id`.
    Plain(String),
    /// A smart playlist, by `SmartPlaylist::id`.
    Smart(i64),
}

enum Dialog {
    Create { name: String, smart: bool },
    Rename { id: String, name: String },
    Delete { sel: Sel, name: String },
}

/// Results of the portal file dialogs (they run off the UI thread).
enum IoMsg {
    Picked(Vec<PathBuf>),
    Failed(String),
    Exported(Result<Option<ExportOutcome>, String>),
}

struct Io {
    tx: mpsc::Sender<IoMsg>,
    rx: mpsc::Receiver<IoMsg>,
}

#[derive(Default)]
pub struct State {
    sel: Option<Sel>,
    dialog: Option<Dialog>,
    dialog_fresh: bool,
    /// The selected row of the track list.
    row_sel: Option<usize>,
    /// Address of the smart-playlist list last seen: the session replaces the
    /// vector on every reload, so a change means "reloaded".
    smart_sig: usize,
    /// `rule_summary` of every smart playlist (localised, so cached).
    smart_meta: Vec<String>,
    /// Select the playlist of this name once it shows up: `(smart, name, born)`.
    select_new: Option<(bool, String, f64)>,
    /// Cover keys of the header mosaic and what they were computed for.
    mosaic: Vec<String>,
    mosaic_sig: u64,
    io: Option<Io>,
}

/// Called when the route is entered: start loading what the page needs.
pub fn enter(app: &mut App) {
    app.views.playlists.select_new = None;
    let effect = app.session.load_playlists();
    app.run(effect);
    let effect = app.session.playlists.load_smart_playlists();
    app.run(effect);
}

/// Selects a playlist (`smart`: a smart playlist, by id) in the page.
pub fn open(app: &mut App, smart: bool, id: &str) {
    let sel = if smart {
        match id.parse::<i64>() {
            Ok(id) => Sel::Smart(id),
            Err(_) => return,
        }
    } else {
        Sel::Plain(id.to_owned())
    };
    let mut st = std::mem::take(&mut app.views.playlists);
    select_with(app, &mut st, sel);
    app.views.playlists = st;
}

/// Starts creating a new playlist (the sidebar `+`).
pub fn begin_create(app: &mut App) {
    app.navigate(Route::Playlists);
    app.views.playlists.dialog = Some(Dialog::Create {
        name: String::new(),
        smart: false,
    });
    app.views.playlists.dialog_fresh = true;
}

/// Selects `sel`, discarding an open rules editor and (for a smart playlist)
/// resolving its tracks.
fn select_with(app: &mut App, st: &mut State, sel: Sel) {
    app.session.playlists.cancel_smart_editor();
    if let Sel::Smart(id) = &sel
        && let Some(idx) = app
            .session
            .playlists
            .smart_playlists
            .iter()
            .position(|p| p.id == *id)
    {
        let effect = app.session.playlists.select_smart_playlist(idx);
        app.run(effect);
    }
    st.sel = Some(sel);
    st.row_sel = None;
}

// ---------------------------------------------------------------------------
// What a frame asks for
// ---------------------------------------------------------------------------

enum Act {
    Select(Sel),
    BeginCreate,
    Import,
    /// Play `Sel` (shuffled, or from a track index).
    Play {
        sel: Sel,
        shuffle: bool,
        from: Option<usize>,
    },
    EditRules(i64),
    SaveRules,
    CancelRules,
    Export(Sel),
    OpenRename(Sel),
    OpenDelete(Sel),
    CreatePlain(String),
    CreateSmart(String),
    Rename(String, String),
    Delete(Sel),
}

#[derive(Default)]
struct Out {
    acts: Vec<Act>,
    menu: Option<menus::MenuAction>,
}

/// The playlist the right pane shows.
enum Cur<'a> {
    Plain(&'a Playlist),
    Smart(&'a SmartPlaylist),
}

impl Cur<'_> {
    fn name(&self) -> &str {
        match self {
            Cur::Plain(p) => &p.name,
            Cur::Smart(p) => &p.name,
        }
    }

    fn sel(&self) -> Sel {
        match self {
            Cur::Plain(p) => Sel::Plain(p.id.clone()),
            Cur::Smart(p) => Sel::Smart(p.id),
        }
    }
}

fn resolve<'a>(app: &'a App, sel: &Sel) -> Option<Cur<'a>> {
    match sel {
        Sel::Plain(id) => app
            .session
            .playlists
            .playlists
            .iter()
            .find(|p| p.id == *id)
            .map(Cur::Plain),
        Sel::Smart(id) => app
            .session
            .playlists
            .smart_playlists
            .iter()
            .find(|p| p.id == *id)
            .map(Cur::Smart),
    }
}

fn tracks_of<'a>(app: &'a App, cur: &Cur<'a>) -> &'a [Track] {
    match cur {
        Cur::Plain(p) => &p.tracks,
        Cur::Smart(_) => &app.session.playlists.smart_playlist_tracks,
    }
}

fn songs(n: usize) -> String {
    if n == 1 {
        "1 song".to_owned()
    } else {
        format!("{n} songs")
    }
}

fn plain_secs(p: &Playlist) -> u64 {
    if p.total_duration.is_zero() {
        p.tracks.iter().map(|t| t.duration.as_secs()).sum()
    } else {
        p.total_duration.as_secs()
    }
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

pub fn show(app: &mut App, ui: &mut Ui) {
    let mut st = std::mem::take(&mut app.views.playlists);
    poll_io(app, &mut st);
    sync(app, &mut st);

    // The editor is taken out so the page can edit it while reading the rest
    // of the session.
    let mut editor = app.session.playlists.smart_playlist_editor.take();
    let mut out = Out::default();
    {
        let app_ref: &App = app;
        refresh_mosaic(app_ref, &mut st);
        page(app_ref, ui, &mut st, editor.as_mut(), &mut out);
        dialogs(ui.ctx(), app_ref.pal, &mut st, &mut out);
    }
    app.session.playlists.smart_playlist_editor = editor;
    app.views.playlists = st;
    apply(app, out);
}

/// Reacts to what changed under the page: reloaded smart playlists, a freshly
/// created playlist, a vanished or missing selection.
fn sync(app: &mut App, st: &mut State) {
    let ptr = app.session.playlists.smart_playlists.as_ptr() as usize;
    if ptr != st.smart_sig {
        st.smart_sig = ptr;
        st.smart_meta = app
            .session
            .playlists
            .smart_playlists
            .iter()
            .map(SmartPlaylist::rule_summary)
            .collect();
        if let Some(Sel::Smart(id)) = &st.sel
            && let Some(idx) = app
                .session
                .playlists
                .smart_playlists
                .iter()
                .position(|p| p.id == *id)
        {
            let effect = app.session.playlists.select_smart_playlist(idx);
            app.run(effect);
        }
    }

    if let Some((smart, name, born)) = &st.select_new {
        let playlists = &app.session.playlists;
        let found = if *smart {
            playlists
                .smart_playlists
                .iter()
                .find(|p| p.name == *name)
                .map(|p| Sel::Smart(p.id))
        } else {
            playlists
                .playlists
                .iter()
                .find(|p| p.name == *name)
                .map(|p| Sel::Plain(p.id.clone()))
        };
        let expired = app.ctx.input(|i| i.time) - born > 10.0;
        if let Some(sel) = found {
            st.select_new = None;
            select_with(app, st, sel);
        } else if expired {
            st.select_new = None;
        }
    }

    // Nothing (valid) selected and no editor open: show the first playlist.
    let valid = st.sel.as_ref().is_some_and(|s| resolve(app, s).is_some());
    if !valid && app.session.playlists.smart_playlist_editor.is_none() {
        let playlists = &app.session.playlists;
        let first = playlists
            .playlists
            .first()
            .map(|p| Sel::Plain(p.id.clone()))
            .or_else(|| playlists.smart_playlists.first().map(|p| Sel::Smart(p.id)));
        match first {
            Some(sel) => select_with(app, st, sel),
            None => st.sel = None,
        }
    }
}

/// Recomputes the header mosaic's cover keys when the shown tracks changed.
fn refresh_mosaic(app: &App, st: &mut State) {
    let Some(cur) = st.sel.as_ref().and_then(|s| resolve(app, s)) else {
        st.mosaic.clear();
        st.mosaic_sig = 0;
        return;
    };
    let tracks = tracks_of(app, &cur);
    let mut h = DefaultHasher::new();
    cur.sel().hash(&mut h);
    tracks.len().hash(&mut h);
    (tracks.as_ptr() as usize).hash(&mut h);
    let sig = h.finish();
    if sig == st.mosaic_sig {
        return;
    }
    st.mosaic_sig = sig;
    st.mosaic.clear();
    for t in tracks {
        let key = t.cover_key();
        if !st.mosaic.contains(&key) {
            st.mosaic.push(key);
            if st.mosaic.len() == 4 {
                break;
            }
        }
    }
}

fn page(app: &App, ui: &mut Ui, st: &mut State, ed: Option<&mut EditorState>, out: &mut Out) {
    let pad = app.page_pad();
    let rect = ui.available_rect_before_wrap();
    let left_w = if app.narrow { 220.0 } else { 260.0 };
    let left = Rect::from_min_max(
        pos2(rect.left() + pad, rect.top() + 4.0),
        pos2(rect.left() + pad + left_w, rect.bottom() - 24.0),
    );
    let right = Rect::from_min_max(pos2(left.right() + 24.0, rect.top() + 4.0), rect.max);
    let new_editor = ed.as_ref().is_some_and(|e| e.playlist.id == 0);
    ui.scope_builder(UiBuilder::new().max_rect(left), |ui| {
        left_column(app, ui, st, new_editor, out)
    });
    ui.scope_builder(UiBuilder::new().max_rect(right), |ui| {
        right_column(app, ui, st, ed, out)
    });
}

// ---------------------------------------------------------------------------
// Left: the list
// ---------------------------------------------------------------------------

fn square_button(ui: &mut Ui, pal: &Palette, glyph: &str, tip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
    let bg = if resp.hovered() {
        pal.stroke_strong
    } else {
        pal.surface_hi
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    paint_icon(ui.painter(), rect.center(), glyph, 15.0, pal.text, false);
    resp.on_hover_text(tip)
}

fn left_column(app: &App, ui: &mut Ui, st: &State, new_editor: bool, out: &mut Out) {
    let pal = app.pal;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Playlists")
                .font(theme::semibold(26.0))
                .color(pal.text),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if square_button(ui, &pal, regular::PLUS, "New playlist").clicked() {
                out.acts.push(Act::BeginCreate);
            }
            if square_button(ui, &pal, regular::DOWNLOAD_SIMPLE, "Import playlist file").clicked() {
                out.acts.push(Act::Import);
            }
        });
    });
    ui.add_space(8.0);

    let playlists = &app.session.playlists;
    ScrollArea::vertical()
        .id_salt("playlists-left")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            let selected = if new_editor { None } else { st.sel.as_ref() };
            if !playlists.playlists.is_empty() {
                section_label(ui, "PLAYLISTS");
                for p in &playlists.playlists {
                    let sel = Sel::Plain(p.id.clone());
                    let n = p.tracks.len().max(p.track_count as usize);
                    let secs = plain_secs(p);
                    let meta = if secs > 0 {
                        format!("{} · {}", songs(n), format_duration_coarse(secs))
                    } else {
                        songs(n)
                    };
                    let resp = list_item(
                        ui,
                        app,
                        &p.name,
                        &meta,
                        regular::PLAYLIST,
                        selected == Some(&sel),
                    );
                    item_menu(&resp, &sel, true, out);
                    if resp.clicked() {
                        out.acts.push(Act::Select(sel));
                    }
                }
            }
            if !playlists.smart_playlists.is_empty() {
                section_label(ui, "SMART");
                for (i, p) in playlists.smart_playlists.iter().enumerate() {
                    let sel = Sel::Smart(p.id);
                    let rules = st.smart_meta.get(i).map(|s| s.trim()).unwrap_or("");
                    let meta = if rules.is_empty() {
                        songs(p.track_count)
                    } else {
                        format!("{rules} · {}", songs(p.track_count))
                    };
                    let resp = list_item(
                        ui,
                        app,
                        &p.name,
                        &meta,
                        fill::SPARKLE,
                        selected == Some(&sel),
                    );
                    item_menu(&resp, &sel, false, out);
                    if resp.clicked() {
                        out.acts.push(Act::Select(sel));
                    }
                }
            }
        });
}

fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        caps_label(ui, text);
    });
    ui.add_space(2.0);
}

fn list_item(
    ui: &mut Ui,
    app: &App,
    title: &str,
    meta: &str,
    icon: &str,
    selected: bool,
) -> Response {
    let pal = app.pal;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ITEM_H), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), pal.surface_sel);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), pal.hover);
    }
    let cover = Rect::from_min_size(
        pos2(rect.left() + 8.0, rect.center().y - 18.0),
        vec2(36.0, 36.0),
    );
    let (a, b) = seed_colors(title);
    gradient_fill(&app.covers, ui, cover, 5, a, b);
    paint_icon(
        ui.painter(),
        cover.center(),
        icon,
        16.0,
        Color32::WHITE.alpha(0.92),
        false,
    );
    let x = cover.right() + 10.0;
    let w = (rect.right() - 8.0 - x).max(10.0);
    paint_text(
        ui.painter(),
        Rect::from_min_size(pos2(x, rect.top() + 8.0), vec2(w, 18.0)),
        Align::Min,
        title,
        theme::semibold(13.0),
        pal.text,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_size(pos2(x, rect.top() + 26.0), vec2(w, 15.0)),
        Align::Min,
        meta,
        theme::regular(12.0),
        pal.text_2,
    );
    resp
}

/// The right-click menu of a list row.
fn item_menu(resp: &Response, sel: &Sel, renamable: bool, out: &mut Out) {
    resp.context_menu(|ui| {
        if ui.button("Play").clicked() {
            out.acts.push(Act::Play {
                sel: sel.clone(),
                shuffle: false,
                from: None,
            });
            ui.close();
        }
        if ui.button("Shuffle").clicked() {
            out.acts.push(Act::Play {
                sel: sel.clone(),
                shuffle: true,
                from: None,
            });
            ui.close();
        }
        if renamable && ui.button("Rename…").clicked() {
            out.acts.push(Act::OpenRename(sel.clone()));
            ui.close();
        }
        if ui.button("Export…").clicked() {
            out.acts.push(Act::Export(sel.clone()));
            ui.close();
        }
        if ui.button("Delete…").clicked() {
            out.acts.push(Act::OpenDelete(sel.clone()));
            ui.close();
        }
    });
}

// ---------------------------------------------------------------------------
// Right: the selected playlist
// ---------------------------------------------------------------------------

fn right_column(
    app: &App,
    ui: &mut Ui,
    st: &mut State,
    mut ed: Option<&mut EditorState>,
    out: &mut Out,
) {
    let pal = app.pal;
    let pad = app.page_pad() as i8;
    let new_editor = ed.as_ref().is_some_and(|e| e.playlist.id == 0);
    let cur = if new_editor {
        None
    } else {
        st.sel.as_ref().and_then(|s| resolve(app, s))
    };
    if cur.is_none() && !new_editor {
        let none = app.session.playlists.playlists.is_empty()
            && app.session.playlists.smart_playlists.is_empty();
        let (title, body) = if none {
            (
                "No playlists yet",
                "Create a playlist, build a smart playlist from rules, or import an M3U / PLS file.",
            )
        } else {
            (
                "Pick a playlist",
                "Select a playlist on the left to see its songs.",
            )
        };
        if empty_state(
            ui,
            regular::PLAYLIST,
            title,
            body,
            none.then_some("New playlist"),
        ) {
            out.acts.push(Act::BeginCreate);
        }
        return;
    }

    ScrollArea::vertical()
        .id_salt("playlists-right")
        .auto_shrink(false)
        .show_viewport(ui, |ui, viewport| {
            let origin = ui.cursor().top();
            Frame::NONE
                .inner_margin(Margin {
                    left: 0,
                    right: pad,
                    top: 0,
                    bottom: 24,
                })
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    match &cur {
                        None => {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(
                                    RichText::new(fill::SPARKLE)
                                        .font(theme::icon_fill(12.0))
                                        .color(pal.text_3),
                                );
                                ui.label(
                                    RichText::new("NEW SMART PLAYLIST")
                                        .font(theme::semibold(11.0))
                                        .color(pal.text_3),
                                );
                            });
                            ui.add_space(10.0);
                            if let Some(e) = ed.as_mut() {
                                editor_card(ui, &pal, e, out);
                            }
                        }
                        Some(cur) => {
                            let tracks = tracks_of(app, cur);
                            detail_header(ui, app, st, cur, tracks, out);
                            ui.add_space(18.0);
                            if let Cur::Smart(p) = cur
                                && let Some(e) = ed.as_mut()
                                && e.playlist.id == p.id
                            {
                                editor_card(ui, &pal, e, out);
                                ui.add_space(18.0);
                            }
                            track_rows(ui, app, st, cur, tracks, (viewport, origin), out);
                        }
                    }
                });
        });
}

fn detail_header(ui: &mut Ui, app: &App, st: &State, cur: &Cur, tracks: &[Track], out: &mut Out) {
    let pal = app.pal;
    let sel = cur.sel();
    let (kind, glyph, meta) = match cur {
        Cur::Plain(p) => {
            let secs = plain_secs(p);
            let n = p.tracks.len().max(p.track_count as usize);
            let meta = if secs > 0 {
                format!("{} · {}", songs(n), format_duration_coarse(secs))
            } else {
                songs(n)
            };
            ("PLAYLIST", regular::PLAYLIST, meta)
        }
        Cur::Smart(p) => {
            let secs: u64 = tracks.iter().map(|t| t.duration.as_secs()).sum();
            let n = if tracks.is_empty() {
                p.track_count
            } else {
                tracks.len()
            };
            let meta = if secs > 0 {
                format!(
                    "{} · {} · updates live",
                    songs(n),
                    format_duration_coarse(secs)
                )
            } else {
                format!("{} · updates live", songs(n))
            };
            ("SMART PLAYLIST", fill::SPARKLE, meta)
        }
    };
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        vec2(width, 120.0),
        Layout::left_to_right(Align::Max),
        |ui| {
            ui.spacing_mut().item_spacing.x = 20.0;
            mosaic(ui, app, &st.mosaic, cur.name());
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let font = if matches!(cur, Cur::Smart(_)) {
                        theme::icon_fill(12.0)
                    } else {
                        theme::icon(12.0)
                    };
                    ui.label(RichText::new(glyph).font(font).color(pal.text_3));
                    ui.label(
                        RichText::new(kind)
                            .font(theme::semibold(11.0))
                            .color(pal.text_3),
                    );
                });
                label_text(ui, cur.name(), theme::bold(30.0), pal.text);
                label_text(ui, &meta, theme::regular(13.0), pal.text_2);
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                    let has =
                        !tracks.is_empty() || matches!(cur, Cur::Smart(p) if p.track_count > 0);
                    if pill_button_enabled(
                        ui,
                        "Play",
                        Some(regular::PLAY),
                        PillKind::Primary,
                        32.0,
                        0.0,
                        has,
                    )
                    .clicked()
                    {
                        out.acts.push(Act::Play {
                            sel: sel.clone(),
                            shuffle: false,
                            from: None,
                        });
                    }
                    if pill_button_enabled(
                        ui,
                        "Shuffle",
                        Some(regular::SHUFFLE),
                        PillKind::Secondary,
                        32.0,
                        0.0,
                        has,
                    )
                    .clicked()
                    {
                        out.acts.push(Act::Play {
                            sel: sel.clone(),
                            shuffle: true,
                            from: None,
                        });
                    }
                    match cur {
                        Cur::Smart(p) => {
                            if pill_button(
                                ui,
                                "Edit rules",
                                Some(regular::SLIDERS_HORIZONTAL),
                                PillKind::Secondary,
                                32.0,
                                0.0,
                            )
                            .clicked()
                            {
                                out.acts.push(Act::EditRules(p.id));
                            }
                        }
                        Cur::Plain(_) => {
                            if pill_button(
                                ui,
                                "Rename",
                                Some(regular::PENCIL_SIMPLE),
                                PillKind::Secondary,
                                32.0,
                                0.0,
                            )
                            .clicked()
                            {
                                out.acts.push(Act::OpenRename(sel.clone()));
                            }
                        }
                    }
                    if pill_button(
                        ui,
                        "Export",
                        Some(regular::UPLOAD_SIMPLE),
                        PillKind::Secondary,
                        32.0,
                        0.0,
                    )
                    .clicked()
                    {
                        out.acts.push(Act::Export(sel.clone()));
                    }
                    if pill_button(
                        ui,
                        "Delete",
                        Some(regular::TRASH),
                        PillKind::Danger,
                        32.0,
                        0.0,
                    )
                    .clicked()
                    {
                        out.acts.push(Act::OpenDelete(sel.clone()));
                    }
                });
            });
        },
    );
}

/// One tile of the mosaic: `tex` (or the gradient of `seed`) with per-corner
/// rounding.
fn tile(
    ui: &Ui,
    app: &App,
    rect: Rect,
    tex: Option<&TextureHandle>,
    seed: &str,
    radius: CornerRadius,
) {
    match tex {
        Some(tex) => {
            let [w, h] = tex.size();
            let (w, h) = (w as f32, h as f32);
            let uv = if w > h {
                let m = (1.0 - h / w) / 2.0;
                Rect::from_min_max(pos2(m, 0.0), pos2(1.0 - m, 1.0))
            } else {
                let m = (1.0 - w / h) / 2.0;
                Rect::from_min_max(pos2(0.0, m), pos2(1.0, 1.0 - m))
            };
            Image::new((tex.id(), tex.size_vec2()))
                .corner_radius(radius)
                .uv(uv)
                .paint_at(ui, rect);
        }
        None => {
            let tex = app.covers.placeholder(seed);
            Image::new((tex.id(), tex.size_vec2()))
                .corner_radius(radius)
                .paint_at(ui, rect);
        }
    }
}

/// The 120px header cover: a 2×2 mosaic of the playlist's albums (one
/// cover when it has a single album, a gradient when it has none).
fn mosaic(ui: &mut Ui, app: &App, keys: &[String], seed: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(120.0, 120.0), Sense::hover());
    let shadow = egui::epaint::Shadow {
        offset: [0, 10],
        blur: 30,
        spread: 0,
        color: Color32::BLACK.alpha(0.4),
    };
    ui.painter()
        .add(shadow.as_shape(rect, CornerRadius::same(10)));
    if keys.len() < 2 {
        let key = keys.first().map_or(seed, String::as_str);
        cover_image(
            ui,
            &app.covers,
            rect,
            keys.first().and_then(|k| app.covers.thumb(k)),
            key,
            10,
        );
        return;
    }
    let r = 10;
    for q in 0..4usize {
        let min = rect.min + vec2((q % 2) as f32 * 60.0, (q / 2) as f32 * 60.0);
        let radius = CornerRadius {
            nw: if q == 0 { r } else { 0 },
            ne: if q == 1 { r } else { 0 },
            sw: if q == 2 { r } else { 0 },
            se: if q == 3 { r } else { 0 },
        };
        let key = &keys[q % keys.len()];
        tile(
            ui,
            app,
            Rect::from_min_size(min, vec2(60.0, 60.0)),
            app.covers.thumb(key),
            key,
            radius,
        );
    }
}

/// `view` is the scroll viewport and the y of the content's top.
fn track_rows(
    ui: &mut Ui,
    app: &App,
    st: &mut State,
    cur: &Cur,
    tracks: &[Track],
    view: (Rect, f32),
    out: &mut Out,
) {
    let (viewport, origin) = view;
    let pal = app.pal;
    let width = ui.available_width();
    if let Cur::Smart(p) = cur
        && tracks.is_empty()
        && p.track_count > 0
    {
        for _ in 0..p.track_count.min(8) {
            let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
            skeleton(
                ui,
                Rect::from_min_size(
                    pos2(rect.left() + 44.0, rect.center().y - 16.0),
                    vec2(32.0, 32.0),
                ),
                4,
            );
            skeleton(
                ui,
                Rect::from_min_size(
                    pos2(rect.left() + 88.0, rect.center().y - 6.0),
                    vec2((width * 0.4).max(40.0), 12.0),
                ),
                4,
            );
        }
        return;
    }
    if tracks.is_empty() {
        let text = match cur {
            Cur::Plain(_) => "No songs yet. Add songs from the menu of any track.",
            Cur::Smart(_) => "No songs match these rules.",
        };
        ui.label(
            RichText::new(text)
                .font(theme::regular(13.5))
                .color(pal.text_2),
        );
        return;
    }

    let sel = cur.sel();
    let rel = ui.cursor().top() - origin;
    let n = tracks.len();
    let first = (((viewport.min.y - rel) / ROW_H).floor().max(0.0) as usize).min(n);
    let last = (((viewport.max.y - rel) / ROW_H).ceil().max(0.0) as usize + 1).min(n);
    ui.add_space(first as f32 * ROW_H);
    for (i, track) in tracks.iter().enumerate().take(last).skip(first) {
        let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
        let current = app
            .session
            .playback
            .current_track
            .as_ref()
            .is_some_and(|c| c.id == track.id && c.source_uri == track.source_uri);
        let hovered = resp.hovered();
        let radius = CornerRadius::same(6);
        if current {
            ui.painter().rect_filled(rect, radius, app.accent.row_tint);
        } else if st.row_sel == Some(i) {
            ui.painter().rect_filled(rect, radius, pal.row_sel);
        } else if hovered {
            ui.painter().rect_filled(rect, radius, pal.hover);
        }

        let num = Rect::from_min_size(pos2(rect.left() + 8.0, rect.top()), vec2(24.0, ROW_H));
        if current {
            paint_icon(
                ui.painter(),
                num.right_center() - vec2(6.0, 0.0),
                fill::MUSIC_NOTES_SIMPLE,
                13.0,
                app.accent.fill,
                true,
            );
        } else if hovered {
            paint_icon(
                ui.painter(),
                num.right_center() - vec2(6.0, 0.0),
                fill::PLAY,
                12.0,
                pal.text,
                true,
            );
        } else {
            paint_text(
                ui.painter(),
                num,
                Align::Max,
                &(i + 1).to_string(),
                theme::mono(12.0),
                pal.text_3,
            );
        }

        let cover = Rect::from_min_size(
            pos2(num.right() + 12.0, rect.center().y - 16.0),
            vec2(32.0, 32.0),
        );
        let key = track.cover_key();
        cover_image(ui, &app.covers, cover, app.covers.thumb(&key), &key, 4);

        let time_w = 44.0;
        let quality_w = 54.0;
        let time = Rect::from_min_size(
            pos2(rect.right() - 8.0 - time_w, rect.top()),
            vec2(time_w, ROW_H),
        );
        let quality = Rect::from_min_size(
            pos2(time.left() - 12.0 - quality_w, rect.top()),
            vec2(quality_w, ROW_H),
        );
        let title_x = cover.right() + 12.0;
        let title_w = (quality.left() - 12.0 - title_x).max(20.0);
        let title_color = if current { app.accent.text } else { pal.text };
        let mut job = LayoutJob::default();
        job.append(
            &track.title,
            0.0,
            TextFormat {
                font_id: theme::semibold(13.0),
                color: title_color,
                ..Default::default()
            },
        );
        if !track.artist.is_empty() {
            job.append(
                &format!(" · {}", track.artist),
                0.0,
                TextFormat {
                    font_id: theme::regular(13.0),
                    color: pal.text_2,
                    ..Default::default()
                },
            );
        }
        job.wrap = TextWrapping {
            max_width: title_w,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            pos2(title_x, rect.center().y - galley.size().y / 2.0),
            galley,
            title_color,
        );

        if let Some(q) = App::quality_of(track) {
            let badge = Rect::from_min_size(
                pos2(quality.left(), rect.center().y - 7.5),
                vec2(quality_w, 15.0),
            );
            ui.scope_builder(UiBuilder::new().max_rect(badge), |ui| {
                quality_badge(ui, q);
            });
        }
        paint_text(
            ui.painter(),
            time,
            Align::Max,
            &format_duration(track.duration.as_secs()),
            theme::mono(12.0),
            pal.text_2,
        );

        if resp.double_clicked() {
            out.acts.push(Act::Play {
                sel: sel.clone(),
                shuffle: false,
                from: Some(i),
            });
        } else if resp.clicked() {
            st.row_sel = Some(i);
        }
        if let Some(menu) = menus::track_menu(&resp, track, &app.session.playlists.playlists) {
            out.menu = Some(menu);
        }
    }
    ui.add_space((n - last) as f32 * ROW_H);
}

// ---------------------------------------------------------------------------
// The smart-playlist rules editor
// ---------------------------------------------------------------------------

/// A dropdown of `n` entries; returns the newly picked index.
fn pick(
    ui: &mut Ui,
    salt: impl Hash + std::fmt::Debug,
    width: f32,
    current: usize,
    n: usize,
    label: impl Fn(usize) -> String,
) -> Option<usize> {
    let mut sel = current;
    egui::ComboBox::from_id_salt(Id::new(salt))
        .width(width)
        .selected_text(label(current))
        .show_ui(ui, |ui| {
            for i in 0..n {
                ui.selectable_value(&mut sel, i, label(i));
            }
        });
    (sel != current).then_some(sel)
}

fn editor_card(ui: &mut Ui, pal: &Palette, ed: &mut EditorState, out: &mut Out) {
    Frame::NONE
        .fill(pal.surface_1)
        .stroke(Stroke::new(1.0, pal.stroke))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(8.0, 10.0);

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Name")
                        .font(theme::regular(13.0))
                        .color(pal.text_2),
                );
                let mut name = ed.playlist.name.clone();
                if text_field(ui, &mut name, "Playlist name", 280.0).changed() {
                    ed.set_name(name);
                }
            });

            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Match")
                        .font(theme::regular(13.0))
                        .color(pal.text),
                );
                let current = usize::from(ed.playlist.match_mode == MatchMode::Any);
                if let Some(i) = pick(ui, "match-mode", 64.0, current, 2, |i| {
                    if i == 0 { "all".into() } else { "any".into() }
                }) {
                    ed.set_match_mode(i);
                }
                ui.label(
                    RichText::new("of these rules")
                        .font(theme::regular(13.0))
                        .color(pal.text),
                );
            });

            let mut remove = None;
            for i in 0..ed.playlist.rules.len() {
                let (field, op, mut value, mut value2) = {
                    let r = &ed.playlist.rules[i];
                    (r.field, r.op, r.value.clone(), r.value2.clone())
                };
                ui.horizontal(|ui| {
                    let field_i = RuleField::ALL.iter().position(|f| *f == field).unwrap_or(0);
                    if let Some(n) = pick(
                        ui,
                        ("rule-field", i),
                        150.0,
                        field_i,
                        RuleField::ALL.len(),
                        |n| RuleField::ALL[n].label(),
                    ) {
                        ed.set_rule_field(i, n);
                    }
                    let op_i = RuleOp::ALL.iter().position(|o| *o == op).unwrap_or(0);
                    if let Some(n) = pick(ui, ("rule-op", i), 150.0, op_i, RuleOp::ALL.len(), |n| {
                        RuleOp::ALL[n].label()
                    }) {
                        ed.set_rule_op(i, n);
                    }
                    let avail = (ui.available_width() - 28.0 - 8.0).max(80.0);
                    if op == RuleOp::Between {
                        let half = ((avail - 40.0) / 2.0).max(40.0);
                        if text_field(ui, &mut value, "from", half).changed() {
                            ed.set_rule_value(i, value);
                        }
                        ui.label(
                            RichText::new("and")
                                .font(theme::regular(13.0))
                                .color(pal.text_2),
                        );
                        if text_field(ui, &mut value2, "to", half).changed() {
                            ed.set_rule_value2(i, value2);
                        }
                    } else if text_field(ui, &mut value, "value", avail).changed() {
                        ed.set_rule_value(i, value);
                    }
                    if IconBtn::new(regular::MINUS_CIRCLE)
                        .size(28.0)
                        .icon_size(16.0)
                        .color(pal.text_3)
                        .tip("Remove rule")
                        .show(ui)
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                ed.remove_rule(i);
            }
            if pill_button(
                ui,
                "Add rule",
                Some(regular::PLUS),
                PillKind::Ghost,
                30.0,
                0.0,
            )
            .clicked()
            {
                ed.add_rule();
            }

            let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(line, 0.0, pal.stroke);

            ui.horizontal_wrapped(|ui| {
                let mut on = ed.limit_enabled;
                if toggle(ui, &mut on).changed() {
                    ed.set_limit_enabled(on);
                }
                ui.label(
                    RichText::new("Limit to")
                        .font(theme::regular(13.0))
                        .color(pal.text),
                );
                ui.add_enabled_ui(ed.limit_enabled, |ui| {
                    let mut input = ed.limit_input.clone();
                    if text_field(ui, &mut input, "50", 56.0).changed() {
                        ed.set_limit_input(input);
                    }
                });
                ui.label(
                    RichText::new("songs")
                        .font(theme::regular(13.0))
                        .color(pal.text),
                );
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Order by")
                        .font(theme::regular(13.0))
                        .color(pal.text),
                );
                let current = OrderField::ALL
                    .iter()
                    .position(|f| *f == ed.playlist.order_by)
                    .unwrap_or(0);
                if let Some(n) = pick(ui, "order-by", 150.0, current, OrderField::ALL.len(), |n| {
                    OrderField::ALL[n].label()
                }) {
                    ed.set_order_by(n);
                }
                if ed.playlist.order_by != OrderField::Random {
                    let dir = usize::from(ed.playlist.order_desc);
                    if let Some(n) = pick(ui, "order-dir", 120.0, dir, 2, |i| {
                        if i == 0 {
                            "ascending".into()
                        } else {
                            "descending".into()
                        }
                    }) {
                        ed.set_order_desc(n == 1);
                    }
                }
            });

            ui.horizontal(|ui| {
                match ed.errors.first() {
                    Some(err) => {
                        ui.label(
                            RichText::new(err)
                                .font(theme::regular(12.5))
                                .color(pal.danger),
                        );
                    }
                    None => {
                        ui.label(
                            RichText::new("Songs update as your library changes")
                                .font(theme::regular(12.5))
                                .color(pal.text_3),
                        );
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let valid = ed.errors.is_empty();
                    if pill_button_enabled(
                        ui,
                        "Save rules",
                        None,
                        PillKind::Primary,
                        30.0,
                        0.0,
                        valid,
                    )
                    .clicked()
                    {
                        out.acts.push(Act::SaveRules);
                    }
                    if pill_button(ui, "Cancel", None, PillKind::Ghost, 30.0, 0.0).clicked() {
                        out.acts.push(Act::CancelRules);
                    }
                });
            });
        });
}

// ---------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------

enum DialogEnd {
    Open,
    Close,
    Confirm(Act),
}

fn dialogs(ctx: &egui::Context, pal: Palette, st: &mut State, out: &mut Out) {
    let fresh = std::mem::take(&mut st.dialog_fresh);
    let Some(dialog) = st.dialog.as_mut() else {
        return;
    };
    let frame = Frame::NONE
        .fill(pal.surface_1)
        .stroke(Stroke::new(1.0, pal.stroke_strong))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(20))
        .shadow(egui::epaint::Shadow {
            offset: [0, 16],
            blur: 40,
            spread: 0,
            color: Color32::BLACK.alpha(0.5),
        });
    let modal = Modal::new(Id::new("playlists-dialog"))
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(380.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 10.0);
            match dialog {
                Dialog::Create { name, smart } => {
                    ui.label(
                        RichText::new("New playlist")
                            .font(theme::semibold(17.0))
                            .color(pal.text),
                    );
                    let mut kind = usize::from(*smart);
                    if segmented(ui, &mut kind, &["Playlist", "Smart playlist"], 28.0) {
                        *smart = kind == 1;
                    }
                    let hint = if *smart {
                        "A smart playlist fills itself from rules you set next."
                    } else {
                        "An empty playlist; add songs from any track's menu."
                    };
                    ui.label(
                        RichText::new(hint)
                            .font(theme::regular(12.5))
                            .color(pal.text_2),
                    );
                    let field = text_field(ui, name, "Playlist name", 340.0);
                    if fresh {
                        field.request_focus();
                    }
                    let ready = !name.trim().is_empty();
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let mut end = DialogEnd::Open;
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if pill_button_enabled(
                            ui,
                            "Create",
                            None,
                            PillKind::Primary,
                            32.0,
                            0.0,
                            ready,
                        )
                        .clicked()
                            || (enter && ready)
                        {
                            let name = name.trim().to_owned();
                            end = DialogEnd::Confirm(if *smart {
                                Act::CreateSmart(name)
                            } else {
                                Act::CreatePlain(name)
                            });
                        }
                        if pill_button(ui, "Cancel", None, PillKind::Ghost, 32.0, 0.0).clicked() {
                            end = DialogEnd::Close;
                        }
                    });
                    end
                }
                Dialog::Rename { id, name } => {
                    ui.label(
                        RichText::new("Rename playlist")
                            .font(theme::semibold(17.0))
                            .color(pal.text),
                    );
                    let field = text_field(ui, name, "Playlist name", 340.0);
                    if fresh {
                        field.request_focus();
                    }
                    let ready = !name.trim().is_empty();
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let mut end = DialogEnd::Open;
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if pill_button_enabled(
                            ui,
                            "Rename",
                            None,
                            PillKind::Primary,
                            32.0,
                            0.0,
                            ready,
                        )
                        .clicked()
                            || (enter && ready)
                        {
                            end =
                                DialogEnd::Confirm(Act::Rename(id.clone(), name.trim().to_owned()));
                        }
                        if pill_button(ui, "Cancel", None, PillKind::Ghost, 32.0, 0.0).clicked() {
                            end = DialogEnd::Close;
                        }
                    });
                    end
                }
                Dialog::Delete { sel, name } => {
                    ui.label(
                        RichText::new(format!("Delete “{name}”?"))
                            .font(theme::semibold(17.0))
                            .color(pal.text),
                    );
                    ui.label(
                        RichText::new(
                            "This can't be undone. The songs themselves stay in your library.",
                        )
                        .font(theme::regular(13.0))
                        .color(pal.text_2),
                    );
                    let mut end = DialogEnd::Open;
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if pill_button(
                            ui,
                            "Delete",
                            Some(regular::TRASH),
                            PillKind::Danger,
                            32.0,
                            0.0,
                        )
                        .clicked()
                        {
                            end = DialogEnd::Confirm(Act::Delete(sel.clone()));
                        }
                        if pill_button(ui, "Cancel", None, PillKind::Ghost, 32.0, 0.0).clicked() {
                            end = DialogEnd::Close;
                        }
                    });
                    end
                }
            }
        });
    let end = if modal.should_close() {
        DialogEnd::Close
    } else {
        modal.inner
    };
    match end {
        DialogEnd::Open => {}
        DialogEnd::Close => st.dialog = None,
        DialogEnd::Confirm(act) => {
            st.dialog = None;
            out.acts.push(act);
        }
    }
}

// ---------------------------------------------------------------------------
// Applying what the frame asked for
// ---------------------------------------------------------------------------

fn apply(app: &mut App, out: Out) {
    if let Some(menu) = out.menu {
        menus::apply(app, menu);
    }
    for act in out.acts {
        match act {
            Act::Select(sel) => {
                let mut st = std::mem::take(&mut app.views.playlists);
                select_with(app, &mut st, sel);
                app.views.playlists = st;
            }
            Act::BeginCreate => begin_create(app),
            Act::Import => spawn_import(app),
            Act::Play { sel, shuffle, from } => play(app, &sel, shuffle, from),
            Act::EditRules(id) => {
                if let Some(idx) = app
                    .session
                    .playlists
                    .smart_playlists
                    .iter()
                    .position(|p| p.id == id)
                {
                    app.session.playlists.edit_smart_playlist(idx);
                }
            }
            Act::SaveRules => {
                let info = app
                    .session
                    .playlists
                    .smart_playlist_editor
                    .as_ref()
                    .map(|e| {
                        (
                            e.playlist.id == 0,
                            e.playlist.name.clone(),
                            e.errors.is_empty(),
                        )
                    });
                let effect = app.session.playlists.save_smart_editor();
                app.run(effect);
                if let Some((true, name, true)) = info {
                    let born = app.ctx.input(|i| i.time);
                    app.views.playlists.select_new = Some((true, name, born));
                }
            }
            Act::CancelRules => app.session.playlists.cancel_smart_editor(),
            Act::Export(sel) => export(app, &sel),
            Act::OpenRename(sel) => {
                if let Sel::Plain(id) = &sel
                    && let Some(p) = app.session.playlists.playlists.iter().find(|p| p.id == *id)
                {
                    let dialog = Dialog::Rename {
                        id: id.clone(),
                        name: p.name.clone(),
                    };
                    app.views.playlists.dialog = Some(dialog);
                    app.views.playlists.dialog_fresh = true;
                }
            }
            Act::OpenDelete(sel) => {
                let name = resolve(app, &sel).map(|c| c.name().to_owned());
                if let Some(name) = name {
                    app.views.playlists.dialog = Some(Dialog::Delete { sel, name });
                }
            }
            Act::CreatePlain(name) => {
                let effect = app.session.create_playlist(name.clone());
                app.run(effect);
                let born = app.ctx.input(|i| i.time);
                app.views.playlists.select_new = Some((false, name, born));
            }
            Act::CreateSmart(name) => {
                app.session.playlists.new_smart_playlist();
                if let Some(ed) = &mut app.session.playlists.smart_playlist_editor {
                    ed.set_name(name);
                }
            }
            Act::Rename(id, name) => {
                if let Some(idx) = app
                    .session
                    .playlists
                    .playlists
                    .iter()
                    .position(|p| p.id == id)
                {
                    let effect = app.session.rename_playlist(idx, name);
                    app.run(effect);
                }
            }
            Act::Delete(sel) => delete(app, &sel),
        }
    }
}

enum Source {
    Tracks(Vec<Track>),
    /// A smart playlist not currently shown: resolve it by index.
    Resolve(usize),
}

fn play(app: &mut App, sel: &Sel, shuffle: bool, from: Option<usize>) {
    let source = match resolve(app, sel) {
        Some(Cur::Plain(p)) => Some(Source::Tracks(p.tracks.clone())),
        Some(Cur::Smart(p)) => {
            let shown = app.views.playlists.sel.as_ref() == Some(sel);
            let loaded = &app.session.playlists.smart_playlist_tracks;
            if shown && !loaded.is_empty() {
                Some(Source::Tracks(loaded.clone()))
            } else {
                app.session
                    .playlists
                    .smart_playlists
                    .iter()
                    .position(|s| s.id == p.id)
                    .map(Source::Resolve)
            }
        }
        None => None,
    };
    match source {
        Some(Source::Tracks(tracks)) => {
            if tracks.is_empty() {
                return;
            }
            if shuffle {
                home::play_shuffled(app, tracks);
            } else {
                let update = app.session.play_tracks(tracks, from.unwrap_or(0));
                app.run(update);
            }
        }
        Some(Source::Resolve(idx)) => {
            if shuffle && !app.session.config.config.shuffle {
                let update = app.session.toggle_shuffle();
                app.run(update);
            }
            let effect = app.session.playlists.resolve_smart_playlist(idx);
            app.run(effect);
        }
        None => {}
    }
}

fn delete(app: &mut App, sel: &Sel) {
    match sel {
        Sel::Plain(id) => {
            if let Some(idx) = app
                .session
                .playlists
                .playlists
                .iter()
                .position(|p| p.id == *id)
            {
                let effect = app.session.delete_playlist(idx);
                app.run(effect);
            }
        }
        Sel::Smart(id) => {
            let effect = app
                .session
                .playlists
                .smart_playlists
                .iter()
                .position(|p| p.id == *id)
                .and_then(|idx| app.session.playlists.delete_smart_playlist(idx));
            if let Some(effect) = effect {
                app.run(effect);
            }
        }
    }
    let st = &mut app.views.playlists;
    if st.sel.as_ref() == Some(sel) {
        st.sel = None;
        st.row_sel = None;
    }
}

// ---------------------------------------------------------------------------
// Playlist files
// ---------------------------------------------------------------------------

fn io_sender(app: &mut App) -> mpsc::Sender<IoMsg> {
    let io = app.views.playlists.io.get_or_insert_with(|| {
        let (tx, rx) = mpsc::channel();
        Io { tx, rx }
    });
    io.tx.clone()
}

fn spawn_import(app: &mut App) {
    let tx = io_sender(app);
    let ctx = app.ctx.clone();
    app.handle.spawn(async move {
        let msg = match pick_playlist_files().await {
            Ok(paths) => IoMsg::Picked(paths),
            Err(e) => IoMsg::Failed(e),
        };
        let _ = tx.send(msg);
        ctx.request_repaint();
    });
}

fn export(app: &mut App, sel: &Sel) {
    let found = match sel {
        Sel::Plain(id) => app
            .session
            .playlists
            .playlists
            .iter()
            .position(|p| p.id == *id)
            .map(|idx| {
                if app.session.playlists.playlist_tracks(idx).is_none() {
                    None
                } else {
                    app.session.playlists.export_tracks(idx)
                }
            }),
        Sel::Smart(id) => app
            .session
            .playlists
            .smart_playlists
            .iter()
            .position(|p| p.id == *id)
            .map(|idx| app.session.playlists.export_smart(idx)),
    };
    let Some(found) = found else { return };
    let Some((name, source)) = found else {
        app.toast("This playlist is empty, nothing to export");
        return;
    };
    let relative = app.session.config.config.m3u_relative_paths;
    let tx = io_sender(app);
    let ctx = app.ctx.clone();
    app.handle.spawn(async move {
        let result = export_flow(&name, source, relative).await;
        let _ = tx.send(IoMsg::Exported(result));
        ctx.request_repaint();
    });
}

fn poll_io(app: &mut App, st: &mut State) {
    let Some(io) = &st.io else { return };
    let msgs: Vec<IoMsg> = io.rx.try_iter().collect();
    for msg in msgs {
        match msg {
            IoMsg::Picked(paths) => {
                if !paths.is_empty() {
                    let effect = aulos_core::session::import_playlist_files(paths);
                    app.run(effect);
                }
            }
            IoMsg::Failed(reason) => app.toast_error(format!("Playlist import failed: {reason}")),
            IoMsg::Exported(Ok(Some(outcome))) => {
                let file = outcome
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let text = if outcome.skipped > 0 {
                    format!(
                        "Exported {} songs to {file} ({} not local files were skipped)",
                        outcome.written, outcome.skipped
                    )
                } else {
                    format!("Exported {} songs to {file}", outcome.written)
                };
                app.toast(text);
            }
            IoMsg::Exported(Ok(None)) => {}
            IoMsg::Exported(Err(reason)) => {
                app.toast_error(format!("Playlist export failed: {reason}"))
            }
        }
    }
}

async fn export_flow(
    name: &str,
    source: ExportSource,
    relative: bool,
) -> Result<Option<ExportOutcome>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let default_name = format!(
        "{}.m3u8",
        aulos_core::library::playlist_io::sanitize_file_name(name)
    );
    let filter = FileFilter::new("M3U playlist").glob("*.m3u8").glob("*.m3u");
    let request = SelectedFiles::save_file()
        .title("Export playlist")
        .current_name(default_name.as_str())
        .modal(true)
        .filter(filter)
        .send()
        .await
        .map_err(|e| format!("Portal request failed: {e}"))?;
    let selected = match request.response() {
        Ok(selected) => selected,
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => return Ok(None),
        Err(e) => return Err(format!("Portal response failed: {e}")),
    };
    let Some(mut path) = selected
        .uris()
        .first()
        .and_then(|uri| aulos_core::file_uri_to_path(uri.as_str()))
    else {
        return Ok(None);
    };
    if path.extension().is_none() {
        path.set_extension("m3u8");
    }
    aulos_core::session::export_playlist(source, path, relative)
        .await
        .map(Some)
}

async fn pick_playlist_files() -> Result<Vec<PathBuf>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let mut filter = FileFilter::new("Playlists (M3U, M3U8, PLS)");
    for ext in aulos_core::library::m3u::PLAYLIST_EXTENSIONS {
        filter = filter.glob(&format!("*.{ext}"));
    }
    let request = SelectedFiles::open_file()
        .title("Import playlists")
        .multiple(true)
        .modal(true)
        .filter(filter)
        .send()
        .await
        .map_err(|e| format!("Portal request failed: {e}"))?;
    match request.response() {
        Ok(selected) => Ok(selected
            .uris()
            .iter()
            .filter_map(|uri| aulos_core::file_uri_to_path(uri.as_str()))
            .collect()),
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => Ok(Vec::new()),
        Err(e) => Err(format!("Portal response failed: {e}")),
    }
}
