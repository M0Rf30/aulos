//! The folders page: browse the library by on-disk (or on-provider) directory
//! structure — a folder tree on the left, the open folder's subfolders and
//! tracks on the right.

use crate::app::App;
use crate::theme::{self, Theme};
use crate::views::menus::{self, MenuAction};
use crate::widgets::{self, Columns, PillKind, paint_icon, paint_text, row_height};
use aulos_core::library::folder_tree::{FolderState, FolderTree};
use aulos_core::session::QueuePlacement;
use egui::{
    Align, CornerRadius, Frame, Margin, Rect, ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder,
    pos2, vec2,
};
use egui_phosphor::{fill, regular};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const TREE_ROW_H: f32 = 28.0;
/// How many breadcrumb segments are shown before the middle is elided.
const MAX_CRUMBS: usize = 4;

#[derive(Default)]
pub struct State {
    folder: FolderState,
    /// Directories opened in the tree.
    expanded: HashSet<PathBuf>,
    /// Bumped whenever `expanded` changes, to invalidate `rows`.
    expand_version: u64,
    rows: Vec<TreeRow>,
    rows_key: Option<(u64, PathBuf, u64)>,
    /// Id of the selected (single-clicked) track.
    selected: Option<i64>,
}

/// One visible row of the folder tree.
struct TreeRow {
    dir: PathBuf,
    depth: usize,
    expandable: bool,
    open: bool,
    count: usize,
}

/// Called when the route is entered: start loading what the page needs.
pub fn enter(_app: &mut App) {}

enum Intent {
    /// Open a directory (and reveal it in the tree).
    Open(PathBuf),
    /// Collapse/expand a tree node without navigating.
    Toggle(PathBuf),
    PlayFolder,
    QueueFolder,
    /// Play track `index` (into the library) from the open folder's own tracks.
    PlayTrack(usize),
    Select(i64),
    Favorite(usize),
    Rate(usize, u8),
    Menu(Box<MenuAction>),
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let mut state = std::mem::take(&mut app.views.folders);
    let generation = app.session.library.generation();
    if app.session.library.folder_tree_gen != Some(generation) {
        state
            .folder
            .set_tree(FolderTree::build(&app.session.library.tracks));
        state.expanded.clear();
        state.expand_version += 1;
        app.session.library.folder_tree_gen = Some(generation);
    }
    let intents = draw(app, ui, &mut state);
    app.views.folders = state;
    apply(app, intents);
}

fn draw(app: &App, ui: &mut Ui, state: &mut State) -> Vec<Intent> {
    let pal = app.pal;
    let pad = app.page_pad();
    let library = &app.session.library;
    let mut intents = Vec::new();
    let has_tracks = !library.tracks.is_empty();

    let margin = Margin::symmetric(pad as i8, 0);
    ui.add_space(4.0);
    Frame::NONE.inner_margin(margin).show(ui, |ui| {
        ui.label(
            egui::RichText::new(aulos_core::fl!("folders"))
                .font(theme::semibold(26.0))
                .color(pal.text),
        );
    });
    ui.add_space(14.0);

    if !has_tracks {
        if library.scanning {
            loading(ui, pad, app.narrow());
        } else {
            widgets::empty_state(
                ui,
                regular::FOLDER_SIMPLE,
                &aulos_core::fl!("folders-empty-title"),
                &aulos_core::fl!("folders-empty-body"),
                None,
            );
        }
        return intents;
    }

    let current = state.folder.effective_current();
    let base = state.folder.base();
    let crumbs = state.folder.visible_breadcrumbs();

    // Breadcrumb + folder actions.
    Frame::NONE.inner_margin(margin).show(ui, |ui| {
        if app.compact() {
            // Phones: the actions get a row of their own under the crumbs.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                breadcrumbs(ui, &crumbs, &mut intents);
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                folder_actions(ui, &mut intents, false);
            });
        } else {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                breadcrumbs(ui, &crumbs, &mut intents);
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    folder_actions(ui, &mut intents, true);
                });
            });
        }
    });
    ui.add_space(14.0);

    // Split the rest of the page: tree panel | listing.
    let avail = ui.available_rect_before_wrap();
    let area = Rect::from_min_max(
        pos2(avail.left() + pad, avail.top()),
        pos2(
            avail.right() - pad,
            (avail.bottom() - 24.0).max(avail.top()),
        ),
    );
    // Phones drop the tree: the breadcrumbs and the folder rows of the
    // listing already navigate.
    let tree_w: f32 = if app.compact() {
        0.0
    } else if app.narrow() {
        220.0
    } else {
        260.0
    };
    let tree_rect = Rect::from_min_size(area.min, vec2(tree_w.min(area.width()), area.height()));
    let list_rect = Rect::from_min_max(
        pos2(
            (tree_rect.right() + if tree_w > 0.0 { 16.0 } else { 0.0 }).min(area.right()),
            area.top(),
        ),
        area.max,
    );

    // Rebuild the flattened tree when the library or the expansion changed.
    let key = (library.generation(), base.clone(), state.expand_version);
    if state.rows_key.as_ref() != Some(&key) {
        state.rows.clear();
        let tree = state.folder.tree();
        flatten(tree, &base, 0, &state.expanded, &mut state.rows);
        state.rows_key = Some(key);
    }

    if tree_w > 0.0 {
        ui.scope_builder(UiBuilder::new().max_rect(tree_rect), |ui| {
            tree_panel(ui, app, state, &current, &mut intents)
        });
    }
    ui.scope_builder(UiBuilder::new().max_rect(list_rect), |ui| {
        listing(ui, app, state, &current, &mut intents)
    });
    ui.allocate_rect(avail, Sense::hover());
    intents
}

/// "Play folder" and "Add to queue", in reading order (`rtl`: the layout
/// places the first one rightmost, so Play goes last).
fn folder_actions(ui: &mut Ui, intents: &mut Vec<Intent>, rtl: bool) {
    let add = |ui: &mut Ui, intents: &mut Vec<Intent>| {
        if widgets::pill_button(
            ui,
            &aulos_core::fl!("queue-add"),
            Some(regular::LIST_PLUS),
            PillKind::Secondary,
            30.0,
            0.0,
        )
        .clicked()
        {
            intents.push(Intent::QueueFolder);
        }
    };
    let play = |ui: &mut Ui, intents: &mut Vec<Intent>| {
        if widgets::pill_button(
            ui,
            &aulos_core::fl!("play-folder"),
            Some(regular::PLAY),
            PillKind::Primary,
            30.0,
            0.0,
        )
        .clicked()
        {
            intents.push(Intent::PlayFolder);
        }
    };
    if rtl {
        add(ui, intents);
        play(ui, intents);
    } else {
        play(ui, intents);
        add(ui, intents);
    }
}

fn flatten(
    tree: &FolderTree,
    dir: &Path,
    depth: usize,
    expanded: &HashSet<PathBuf>,
    out: &mut Vec<TreeRow>,
) {
    let children = tree.child_dirs(dir);
    let open = !children.is_empty() && (depth == 0 || expanded.contains(dir));
    out.push(TreeRow {
        dir: dir.to_path_buf(),
        depth,
        expandable: !children.is_empty(),
        open,
        count: tree.track_count_in(dir, true),
    });
    if open {
        for child in children {
            flatten(tree, child, depth + 1, expanded, out);
        }
    }
}

/// A 28px breadcrumb segment; `active` marks the open folder (last one).
fn crumb(
    ui: &mut Ui,
    label: &str,
    icon: Option<&str>,
    active: bool,
    clickable: bool,
) -> egui::Response {
    let pal = Theme::get(ui.ctx()).pal;
    let font = if active {
        theme::semibold(13.0)
    } else {
        theme::regular(13.0)
    };
    let text_w = widgets::text_width(ui, label, font.clone());
    let icon_w = if icon.is_some() { 14.0 + 6.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(
        vec2(text_w + icon_w + 16.0, 28.0),
        if clickable {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let r = CornerRadius::same(6);
    if active {
        ui.painter().rect_filled(rect, r, pal.surface_sel);
    } else if clickable && resp.hovered() {
        ui.painter().rect_filled(rect, r, pal.hover);
    }
    let color = if active { pal.text } else { pal.text_2 };
    let mut x = rect.left() + 8.0;
    if let Some(g) = icon {
        paint_icon(
            ui.painter(),
            pos2(x + 7.0, rect.center().y),
            g,
            14.0,
            color,
            false,
        );
        x += icon_w;
    }
    paint_text(
        ui.painter(),
        Rect::from_min_max(pos2(x, rect.top()), pos2(rect.right() - 8.0, rect.bottom())),
        Align::Min,
        label,
        font,
        color,
    );
    resp
}

fn chevron(ui: &mut Ui) {
    let pal = Theme::get(ui.ctx()).pal;
    let (rect, _) = ui.allocate_exact_size(vec2(11.0, 28.0), Sense::hover());
    paint_icon(
        ui.painter(),
        rect.center(),
        regular::CARET_RIGHT,
        11.0,
        pal.text_4,
        false,
    );
}

fn breadcrumbs(ui: &mut Ui, crumbs: &[(PathBuf, String)], intents: &mut Vec<Intent>) {
    let last = crumbs.len().saturating_sub(1);
    // Long paths keep the first segment, an ellipsis and the last two.
    let hidden = if crumbs.len() > MAX_CRUMBS {
        1..crumbs.len() - 2
    } else {
        0..0
    };
    for (i, (dir, label)) in crumbs.iter().enumerate() {
        if hidden.contains(&i) {
            if i == hidden.start {
                chevron(ui);
                crumb(ui, "…", None, false, false);
            }
            continue;
        }
        if i > 0 {
            chevron(ui);
        }
        let icon = (i == 0).then_some(regular::HOUSE_SIMPLE);
        if crumb(ui, label, icon, i == last, i != last).clicked() {
            intents.push(Intent::Open(dir.clone()));
        }
    }
}

fn panel_frame(ui: &Ui, rect: Rect) {
    let pal = Theme::get(ui.ctx()).pal;
    ui.painter()
        .rect_filled(rect, CornerRadius::same(10), pal.surface_1);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, pal.stroke),
        StrokeKind::Inside,
    );
}

fn tree_panel(ui: &mut Ui, app: &App, state: &State, current: &Path, intents: &mut Vec<Intent>) {
    let pal = app.pal;
    let rect = ui.max_rect();
    panel_frame(ui, rect);
    let inner = rect.shrink(9.0);
    ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        let rows = &state.rows;
        ScrollArea::vertical()
            .id_salt("folders-tree")
            .auto_shrink([false, false])
            .show_rows(ui, TREE_ROW_H, rows.len(), |ui, range| {
                for i in range {
                    let row = &rows[i];
                    let (r, resp) = ui.allocate_exact_size(
                        vec2(ui.available_width(), TREE_ROW_H),
                        Sense::click(),
                    );
                    let selected = row.dir == current;
                    let rr = CornerRadius::same(6);
                    if selected {
                        ui.painter().rect_filled(r, rr, pal.surface_sel);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(r, rr, pal.hover);
                    }
                    let x0 = r.left() + 8.0 + row.depth as f32 * 16.0;
                    let cy = r.center().y;
                    let caret_rect =
                        Rect::from_center_size(pos2(x0 + 6.0, cy), vec2(18.0, TREE_ROW_H));
                    if row.expandable {
                        let glyph = if row.open {
                            regular::CARET_DOWN
                        } else {
                            regular::CARET_RIGHT
                        };
                        paint_icon(
                            ui.painter(),
                            caret_rect.center(),
                            glyph,
                            11.0,
                            pal.text_4,
                            false,
                        );
                        if row.depth > 0
                            && ui
                                .interact(caret_rect, resp.id.with("caret"), Sense::click())
                                .clicked()
                        {
                            intents.push(Intent::Toggle(row.dir.clone()));
                        }
                    }
                    let icon_color = if selected { pal.text } else { pal.text_2 };
                    let icon_c = pos2(x0 + 25.5, cy);
                    if row.depth == 0 {
                        paint_icon(
                            ui.painter(),
                            icon_c,
                            regular::HARD_DRIVES,
                            15.0,
                            icon_color,
                            false,
                        );
                    } else if row.open {
                        paint_icon(
                            ui.painter(),
                            icon_c,
                            fill::FOLDER_OPEN,
                            15.0,
                            icon_color,
                            true,
                        );
                    } else {
                        paint_icon(
                            ui.painter(),
                            icon_c,
                            regular::FOLDER_SIMPLE,
                            15.0,
                            icon_color,
                            false,
                        );
                    }
                    let count_w = widgets::paint_text(
                        ui.painter(),
                        Rect::from_min_max(
                            pos2(r.right() - 60.0, r.top()),
                            pos2(r.right() - 8.0, r.bottom()),
                        ),
                        Align::Max,
                        &row.count.to_string(),
                        theme::mono(10.5),
                        pal.text_4,
                    );
                    let label = dir_label(&row.dir);
                    let text_rect = Rect::from_min_max(
                        pos2(x0 + 39.0, r.top()),
                        pos2((r.right() - 14.0 - count_w).max(x0 + 39.0), r.bottom()),
                    );
                    let color = if selected { pal.text } else { pal.text_15 };
                    paint_text(
                        ui.painter(),
                        text_rect,
                        Align::Min,
                        &label,
                        theme::regular(13.0),
                        color,
                    );
                    if resp.clicked() {
                        intents.push(Intent::Open(row.dir.clone()));
                    }
                }
            });
    });
}

/// The folder's own name; the library root has none.
fn dir_label(dir: &Path) -> String {
    match dir.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None if dir.as_os_str().is_empty() => aulos_core::fl!("folders-root"),
        None => dir.to_string_lossy().into_owned(),
    }
}

fn listing(ui: &mut Ui, app: &App, state: &State, current: &Path, intents: &mut Vec<Intent>) {
    let pal = app.pal;
    let library = &app.session.library;
    let tree = state.folder.tree();
    let dirs = tree.child_dirs(current);
    let direct = tree.direct_tracks(current);
    let row_h = row_height(ui.ctx());
    if dirs.is_empty() && direct.is_empty() {
        widgets::empty_state(
            ui,
            regular::FOLDER_OPEN,
            &aulos_core::fl!("folder-empty"),
            &aulos_core::fl!("folder-empty-hint"),
            None,
        );
        return;
    }
    let cols = Columns {
        artist: false,
        album: !app.compact(),
        quality: !app.compact(),
        rating: !app.narrow(),
    };
    widgets::track_header(ui, cols, &aulos_core::fl!("songs-column-number"));
    let total = dirs.len() + direct.len();
    let playing = app.session.playback.current_track.as_ref().map(|t| t.id);
    ui.spacing_mut().item_spacing.y = 0.0;
    ScrollArea::vertical()
        .id_salt("folders-list")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, total, |ui, range| {
            for i in range {
                if i < dirs.len() {
                    let dir = &dirs[i];
                    let (r, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    if resp.hovered() {
                        ui.painter()
                            .rect_filled(r, CornerRadius::same(6), pal.hover);
                    }
                    let cy = r.center().y;
                    paint_icon(
                        ui.painter(),
                        pos2(r.left() + 24.0, cy),
                        fill::FOLDER_SIMPLE,
                        16.0,
                        pal.text_2,
                        true,
                    );
                    let count = tree.track_count_in(dir, true);
                    let count_text = aulos_core::fl!("folders-song-count", count = count);
                    let w = paint_text(
                        ui.painter(),
                        Rect::from_min_max(
                            pos2(r.right() - 160.0, r.top()),
                            pos2(r.right() - 10.0, r.bottom()),
                        ),
                        Align::Max,
                        &count_text,
                        theme::mono(11.5),
                        pal.text_2,
                    );
                    let name = Rect::from_min_max(
                        pos2(r.left() + 50.0, r.top()),
                        pos2((r.right() - 22.0 - w).max(r.left() + 50.0), r.bottom()),
                    );
                    paint_text(
                        ui.painter(),
                        name,
                        Align::Min,
                        &dir_label(dir),
                        theme::medium(13.0),
                        pal.text,
                    );
                    if resp.clicked() {
                        intents.push(Intent::Open(dir.clone()));
                    }
                } else {
                    let n = i - dirs.len();
                    let real = direct[n];
                    let Some(track) = library.tracks.get(real) else {
                        ui.allocate_space(vec2(ui.available_width(), row_h));
                        continue;
                    };
                    let number = (n + 1).to_string();
                    let out = widgets::track_row(
                        ui,
                        app,
                        track,
                        &number,
                        playing == Some(track.id),
                        state.selected == Some(track.id),
                        cols,
                    );
                    if out.response.clicked()
                        && !out.toggle_favorite
                        && out.set_rating.is_none()
                        && !out.more.clicked()
                    {
                        intents.push(Intent::Select(track.id));
                        intents.push(Intent::PlayTrack(real));
                    }
                    if out.toggle_favorite {
                        intents.push(Intent::Favorite(real));
                    }
                    if let Some(stars) = out.set_rating {
                        intents.push(Intent::Rate(real, stars));
                    }
                    let playlists = &app.session.playlists.playlists;
                    if let Some(action) = menus::track_menu(&out.response, track, playlists)
                        .or_else(|| menus::track_menu_click(&out.more, track, playlists))
                    {
                        intents.push(Intent::Menu(Box::new(action)));
                    }
                }
            }
        });
}

/// Skeleton panels while the first scan has not produced any track yet.
fn loading(ui: &mut Ui, pad: f32, narrow: bool) {
    let pal = Theme::get(ui.ctx()).pal;
    let avail = ui.available_rect_before_wrap();
    let area = Rect::from_min_max(
        pos2(avail.left() + pad, avail.top()),
        pos2(
            avail.right() - pad,
            (avail.bottom() - 24.0).max(avail.top()),
        ),
    );
    let tree_w = (if narrow { 220.0_f32 } else { 260.0 }).min(area.width());
    let tree = Rect::from_min_size(area.min, vec2(tree_w, area.height()));
    panel_frame(ui, tree);
    for i in 0..((tree.height() / 29.0) as usize).min(12) {
        let y = tree.top() + 9.0 + i as f32 * 29.0;
        let indent = if i % 3 == 0 { 0.0 } else { 16.0 };
        let w = (tree_w - 34.0 - indent) * if i % 2 == 0 { 0.8 } else { 0.55 };
        widgets::skeleton(
            ui,
            Rect::from_min_size(pos2(tree.left() + 17.0 + indent, y + 7.0), vec2(w, 14.0)),
            4,
        );
    }
    let list_left = tree.right() + 16.0;
    let list_w = (area.right() - list_left).max(0.0);
    ui.painter().hline(
        list_left..=area.right(),
        area.top() + 30.0,
        Stroke::new(1.0, pal.stroke),
    );
    let row_h = row_height(ui.ctx());
    for i in 0..((area.height() / row_h) as usize).saturating_sub(1).min(12) {
        let y = area.top() + 40.0 + i as f32 * row_h;
        widgets::skeleton(
            ui,
            Rect::from_min_size(pos2(list_left + 50.0, y), vec2(list_w * 0.4, 14.0)),
            4,
        );
    }
    ui.allocate_rect(avail, Sense::hover());
}

fn apply(app: &mut App, intents: Vec<Intent>) {
    for intent in intents {
        match intent {
            Intent::Open(dir) => open(app, dir),
            Intent::Toggle(dir) => {
                let s = &mut app.views.folders;
                if !s.expanded.remove(&dir) {
                    s.expanded.insert(dir);
                }
                s.expand_version += 1;
            }
            Intent::Select(id) => app.views.folders.selected = Some(id),
            Intent::PlayFolder => {
                let tracks = folder_tracks(app);
                if !tracks.is_empty() {
                    let update = app.session.play_tracks(tracks, 0);
                    app.run(update);
                }
            }
            Intent::QueueFolder => {
                let tracks = folder_tracks(app);
                if !tracks.is_empty() {
                    let update = app.session.queue_tracks(tracks, QueuePlacement::End);
                    app.run(update);
                }
            }
            Intent::PlayTrack(index) => {
                let folder = &app.views.folders.folder;
                let dir = folder.effective_current();
                let direct = folder.tree().direct_tracks(&dir);
                let tracks: Vec<_> = direct
                    .iter()
                    .filter_map(|&i| app.session.library.tracks.get(i).cloned())
                    .collect();
                if let Some(position) = direct.iter().position(|&i| i == index) {
                    let update = app.session.play_tracks(tracks, position);
                    app.run(update);
                }
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

/// Navigates to `dir` and reveals it (and its ancestors) in the tree.
fn open(app: &mut App, dir: PathBuf) {
    let s = &mut app.views.folders;
    let base = s.folder.base();
    let mut cursor = Some(dir.as_path());
    while let Some(d) = cursor {
        if d == base || d.as_os_str().is_empty() {
            break;
        }
        s.expanded.insert(d.to_path_buf());
        cursor = d.parent();
    }
    s.expand_version += 1;
    s.folder.open(dir);
}

/// Every track under the open folder, recursively, in path order.
fn folder_tracks(app: &App) -> Vec<aulos_core::library::Track> {
    let folder = &app.views.folders.folder;
    let dir = folder.effective_current();
    folder
        .tree()
        .tracks_in(&dir, true)
        .into_iter()
        .filter_map(|i| app.session.library.tracks.get(i).cloned())
        .collect()
}
