//! The settings page: a section list (with search) on the left, the sections
//! themselves in a scroll area on the right. Every control reads and writes
//! the real [`aulos_core::config::Config`] through the session.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use aulos_core::autoeq;
use aulos_core::config::{AutoPlayMode, ReplayGainMode, ThemeMode};
use aulos_core::library::artist_tags::{DEFAULT_DELIMITERS, parse_delimiters_input};
use aulos_core::online::scrobble::controller::{enabled_of, is_connected, user_of};
use aulos_core::online::scrobble::import::ImportOutcome;
use aulos_core::online::scrobble::loved::{LovedSyncPlan, plan_sync};
use aulos_core::online::scrobble::{ScrobbleMessage, Service};
use aulos_core::session::SessionUpdate;
use aulos_core::settings_search::{SearchQuery, keywords};
use egui::{
    Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, Rect, Sense, Shape, Stroke,
    StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use egui_phosphor::regular as ph;

use crate::app::App;
use crate::dialogs;
use crate::theme::{self, ColorExt, Palette};
use crate::widgets::{
    IconBtn, PillKind, empty_state, paint_icon, paint_text, paint_text_spaced, pill_button,
    pill_button_enabled, segmented, text_width, toggle,
};

/// Widest the settings column grows.
const CONTENT_MAX_W: f32 = 700.0;
/// Gap between the section list and the settings column, and between sections.
const COLUMN_GAP: f32 = 28.0;
const SECTION_GAP: f32 = 26.0;
/// Most genre suggestions listed by the party-mode "+ Genre" menu.
const MAX_GENRE_SUGGESTIONS: usize = 60;

// ------------------------------------------------------------------ sections

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sec {
    Library,
    Sources,
    Playback,
    Equalizer,
    Scrobbling,
    Online,
    Tagger,
    Appearance,
    Startup,
    Desktop,
    Experimental,
    About,
}

const SECTIONS: [Sec; 12] = [
    Sec::Library,
    Sec::Sources,
    Sec::Playback,
    Sec::Equalizer,
    Sec::Scrobbling,
    Sec::Online,
    Sec::Tagger,
    Sec::Appearance,
    Sec::Startup,
    Sec::Desktop,
    Sec::Experimental,
    Sec::About,
];

/// Splits a localized `a | b | c` list of search terms.
fn terms(list: &str) -> Vec<String> {
    list.split('|')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

impl Sec {
    fn label(self) -> String {
        match self {
            Sec::Library => aulos_core::fl!("settings-library"),
            Sec::Sources => aulos_core::fl!("settings-sec-sources"),
            Sec::Playback => aulos_core::fl!("settings-playback"),
            Sec::Equalizer => aulos_core::fl!("equalizer"),
            Sec::Scrobbling => aulos_core::fl!("scrobbling"),
            Sec::Online => aulos_core::fl!("settings-sec-online"),
            Sec::Tagger => aulos_core::fl!("settings-sec-tagger"),
            Sec::Appearance => aulos_core::fl!("settings-appearance"),
            Sec::Startup => aulos_core::fl!("settings-sec-startup"),
            Sec::Desktop => aulos_core::fl!("settings-sec-desktop"),
            Sec::Experimental => aulos_core::fl!("settings-experimental"),
            Sec::About => aulos_core::fl!("settings-about"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Sec::Library => ph::FOLDER_SIMPLE,
            Sec::Sources => ph::HARD_DRIVES,
            Sec::Playback => ph::PLAY_CIRCLE,
            Sec::Equalizer => ph::SLIDERS_HORIZONTAL,
            Sec::Scrobbling => ph::BROADCAST,
            Sec::Online => ph::GLOBE_SIMPLE,
            Sec::Tagger => ph::KEY,
            Sec::Appearance => ph::PAINT_BRUSH,
            Sec::Startup => ph::HOUSE,
            Sec::Desktop => ph::MOON,
            Sec::Experimental => ph::FLASK,
            Sec::About => ph::INFO,
        }
    }

    /// Everything the search box matches this section against.
    fn keywords(self) -> Vec<String> {
        match self {
            Sec::Library => [
                keywords::library(),
                keywords::artist_tags(),
                keywords::compilations(),
                keywords::playlists(),
                terms(&aulos_core::fl!("settings-kw-library")),
            ]
            .concat(),
            Sec::Sources => terms(&aulos_core::fl!("settings-kw-sources")),
            // `playback_extras()` lists the fade/auto-play/party strings first and the
            // desktop strings (notifications, suspend, background) from index 9 on.
            Sec::Playback => [
                keywords::playback(),
                keywords::playback_extras().into_iter().take(9).collect(),
                terms(&aulos_core::fl!("settings-kw-playback")),
            ]
            .concat(),
            Sec::Equalizer => terms(&aulos_core::fl!("settings-kw-equalizer")),
            Sec::Scrobbling => terms(&aulos_core::fl!("settings-kw-scrobbling")),
            Sec::Online => [
                keywords::artist_info(),
                terms(&aulos_core::fl!("settings-kw-online")),
            ]
            .concat(),
            Sec::Tagger => [
                keywords::tagger(),
                terms(&aulos_core::fl!("settings-kw-tagger")),
            ]
            .concat(),
            Sec::Appearance => [
                keywords::appearance(),
                terms(&aulos_core::fl!("settings-kw-appearance")),
            ]
            .concat(),
            Sec::Startup => [
                keywords::startup(),
                terms(&aulos_core::fl!("settings-kw-startup")),
            ]
            .concat(),
            Sec::Desktop => [
                keywords::playback_extras().into_iter().skip(9).collect(),
                terms(&aulos_core::fl!("settings-kw-desktop")),
            ]
            .concat(),
            Sec::Experimental => keywords::experimental(),
            Sec::About => [
                keywords::about(),
                terms(&aulos_core::fl!("settings-kw-about")),
            ]
            .concat(),
        }
    }
}

// --------------------------------------------------------------------- state

/// Results of off-thread work the page started.
enum Msg {
    Scrobble(ScrobbleMessage),
    /// The chosen music folder; `Ok(None)` when the dialog was cancelled.
    Folder(Result<Option<PathBuf>, String>),
    /// The outcome of asking the browser for access to music folders.
    Access(Result<(), String>),
}

/// Whether the files of a music folder can be read right now. Only the
/// browser ever lacks access (see `aulos_core::web_fs`).
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(dead_code, reason = "only the browser lacks access to its folders")
)]
enum Access {
    Granted,
    /// Saved from an earlier visit; the browser asks again on a click.
    NeedsPermission,
    /// Picked in a browser that cannot save folders: pick it again.
    NeedsRepick,
}

fn access_of(dir: &Path) -> Access {
    #[cfg(target_arch = "wasm32")]
    match aulos_core::web_fs::status(dir) {
        aulos_core::web_fs::RootStatus::Ready => Access::Granted,
        aulos_core::web_fs::RootStatus::NeedsPermission => Access::NeedsPermission,
        aulos_core::web_fs::RootStatus::NeedsRepick => Access::NeedsRepick,
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        _ = dir;
        Access::Granted
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mpd,
    Subsonic,
}

/// A server card whose editor is open (or whose connection test is running).
#[derive(Clone, Copy, PartialEq, Eq)]
struct CardRef {
    kind: Kind,
    index: usize,
}

#[derive(Default)]
struct FolderCache {
    generation: Option<u64>,
    dirs: Vec<PathBuf>,
    /// Per directory: tracks under it, and whether it exists on disk.
    info: Vec<(usize, bool)>,
}

#[derive(Default)]
pub struct State {
    channel: Option<(Sender<Msg>, Receiver<Msg>)>,
    query: String,
    parsed: SearchQuery,
    parsed_for: Option<String>,
    keywords: Vec<Vec<String>>,
    /// Per section: how many of its strings match the query (0: hidden while searching).
    hits: Vec<usize>,
    current: usize,
    /// Phone layout: the sections list is showing (`false`) or one section.
    open: bool,
    scroll_to: Option<usize>,
    folders: FolderCache,
    path_input: String,
    picking_folder: bool,
    delimiters_input: String,
    edit: Option<CardRef>,
    edit_error: Option<String>,
    testing: Vec<CardRef>,
    acoustid_input: String,
    eq_name: String,
    eq_search: String,
}

impl State {
    fn sender(&mut self) -> Sender<Msg> {
        self.channel.get_or_insert_with(mpsc::channel).0.clone()
    }

    fn ensure_search(&mut self) {
        if self.keywords.is_empty() {
            self.keywords = SECTIONS.iter().map(|s| s.keywords()).collect();
        }
        if self.parsed_for.as_deref() == Some(self.query.as_str())
            && self.hits.len() == SECTIONS.len()
        {
            return;
        }
        self.parsed = SearchQuery::new(&self.query);
        self.hits = self
            .keywords
            .iter()
            .map(|kw| {
                if self.parsed.is_empty() {
                    1
                } else if self.parsed.matches(kw) {
                    kw.iter()
                        .filter(|k| self.parsed.matches(&[k.as_str()]))
                        .count()
                        .max(1)
                } else {
                    0
                }
            })
            .collect();
        self.parsed_for = Some(self.query.clone());
    }
}

/// Called when the route is entered: start loading what the page needs.
pub fn enter(app: &mut App) {
    let st = &mut app.views.settings;
    st.edit = None;
    st.edit_error = None;
    st.open = false;
    st.delimiters_input = app.session.config.config.artist_tag_delimiters.join(" | ");
    pump(app);
    let genres = app.session.load_genres();
    app.run(genres);
}

/// Opens the `n`-th section (the smoke runs' `settings:<n>`).
pub fn open_section(app: &mut App, n: usize) {
    let st = &mut app.views.settings;
    st.current = n.min(SECTIONS.len() - 1);
    st.scroll_to = Some(st.current);
    st.open = true;
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    pump(app);
    app.views.settings.ensure_search();
    let pal = app.pal;
    let pad = app.page_pad();
    let nav_w = if app.narrow() { 190.0 } else { 220.0 };
    let full = ui.available_rect_before_wrap();
    let top = full.top() + 4.0;

    if app.compact() {
        show_compact(app, ui, full, pad);
        return;
    }

    let nav_rect = Rect::from_min_max(
        pos2(full.left() + pad, top),
        pos2(full.left() + pad + nav_w, full.bottom()),
    );
    let mut nav_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(nav_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    nav(app, &mut nav_ui);

    let content_left = nav_rect.right() + COLUMN_GAP;
    let content_rect = Rect::from_min_max(pos2(content_left, top), full.right_bottom());
    let mut content_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(content_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    content(app, &mut content_ui, pal, pad);
    ui.advance_cursor_after_rect(full);
}

/// Phones: the section list fills the page; picking one swaps it for that
/// section's settings under a back row.
fn show_compact(app: &mut App, ui: &mut egui::Ui, full: Rect, pad: f32) {
    let pal = app.pal;
    let top = full.top() + 4.0;
    if !app.views.settings.open {
        let rect = Rect::from_min_max(
            pos2(full.left() + pad, top),
            full.right_bottom() - vec2(pad, 0.0),
        );
        let mut nav_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        nav(app, &mut nav_ui);
        ui.advance_cursor_after_rect(full);
        return;
    }
    let row = Rect::from_min_size(
        pos2(full.left() + pad - 8.0, top),
        vec2(full.width() - pad, theme::size::TOUCH),
    );
    if crate::widgets::back_row(ui, row, &aulos_core::fl!("settings")) {
        app.views.settings.open = false;
    }
    let content_rect = Rect::from_min_max(
        pos2(full.left() + pad, row.bottom() + 4.0),
        full.right_bottom(),
    );
    let mut content_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(content_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    content(app, &mut content_ui, pal, pad);
    ui.advance_cursor_after_rect(full);
}

// ----------------------------------------------------------------- navigation

fn nav(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
    ui.label(
        egui::RichText::new(aulos_core::fl!("settings"))
            .font(theme::semibold(26.0))
            .color(pal.text),
    );
    ui.add_space(10.0);
    search_box(ui, &pal, &mut app.views.settings.query);
    ui.add_space(8.0);

    let row_h = if app.compact() {
        theme::size::TOUCH
    } else {
        30.0
    };
    let st = &mut app.views.settings;
    let searching = !st.parsed.is_empty();
    let mut clicked = None;
    // Scrollable so the last entries stay reachable on short windows.
    egui::ScrollArea::vertical()
        .id_salt("settings-nav")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, sec) in SECTIONS.iter().enumerate() {
                let hit = st.hits.get(i).copied().unwrap_or(0);
                let miss = searching && hit == 0;
                let (rect, resp) = ui.allocate_exact_size(
                    vec2(ui.available_width(), row_h),
                    if miss { Sense::hover() } else { Sense::click() },
                );
                let on = i == st.current && !searching;
                if on {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), pal.surface_sel);
                } else if resp.hovered() && !miss {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), pal.text.alpha(0.05));
                }
                let fade = if miss { 0.38 } else { 1.0 };
                let fg = (if on { pal.text } else { pal.text_2 }).gamma_multiply(fade);
                paint_icon(
                    ui.painter(),
                    pos2(rect.left() + 17.5, rect.center().y),
                    sec.icon(),
                    15.0,
                    fg,
                    false,
                );
                let label_rect = Rect::from_min_max(
                    pos2(rect.left() + 35.0, rect.top()),
                    pos2(rect.right() - 30.0, rect.bottom()),
                );
                paint_text(
                    ui.painter(),
                    label_rect,
                    Align::Min,
                    &sec.label(),
                    theme::regular(13.0),
                    fg,
                );
                if searching && !miss {
                    let hit_rect = Rect::from_min_max(
                        pos2(rect.right() - 30.0, rect.top()),
                        pos2(rect.right() - 10.0, rect.bottom()),
                    );
                    paint_text(
                        ui.painter(),
                        hit_rect,
                        Align::Max,
                        &hit.to_string(),
                        theme::mono(10.5),
                        pal.text_3,
                    );
                }
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        });
    if let Some(i) = clicked {
        st.scroll_to = Some(i);
        st.current = i;
        st.open = true;
    }
}

fn search_box(ui: &mut Ui, pal: &Palette, text: &mut String) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    let id = egui::Id::new("settings-search");
    let focused = ui.memory(|m| m.has_focus(id));
    let r = CornerRadius::same(8);
    ui.painter().rect_filled(rect, r, pal.surface_2);
    ui.painter().rect_stroke(
        rect,
        r,
        Stroke::new(1.0, if focused { pal.text } else { pal.stroke }),
        StrokeKind::Inside,
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(10.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 8.0;
    child.label(
        egui::RichText::new(ph::MAGNIFYING_GLASS)
            .font(theme::icon(14.0))
            .color(pal.text_2),
    );
    let w = child.available_width();
    child.add(
        egui::TextEdit::singleline(text)
            .id(id)
            .hint_text(
                egui::RichText::new(aulos_core::fl!("settings-search-placeholder"))
                    .color(pal.text_3),
            )
            .frame(egui::Frame::NONE)
            .desired_width(w)
            .font(theme::regular(13.0))
            .vertical_align(Align::Center),
    );
}

// -------------------------------------------------------------------- content

fn content(app: &mut App, ui: &mut Ui, _pal: Palette, pad: f32) {
    let st = &app.views.settings;
    let visible: Vec<bool> = (0..SECTIONS.len())
        .map(|i| st.hits.get(i).copied().unwrap_or(0) > 0)
        .collect();
    let scroll_to = st.scroll_to;
    let searching = !st.parsed.is_empty();
    let mut jumped = false;
    let mut current = st.current;

    egui::ScrollArea::vertical()
        .id_salt("settings-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let avail = (ui.available_width() - pad).max(200.0);
            let w = avail.min(CONTENT_MAX_W);
            ui.set_max_width(w);
            ui.set_min_width(w);
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            let mut any = false;
            for (i, sec) in SECTIONS.iter().enumerate() {
                if !visible[i] {
                    continue;
                }
                any = true;
                let top = ui.cursor().min;
                // Each section lays out in a fixed-width child: a widget wider
                // than the column (the zoom steps, the EQ bands) would
                // otherwise widen this ui, and with it every later row.
                let mut section = ui.new_child(
                    UiBuilder::new()
                        .max_rect(Rect::from_min_size(top, vec2(w, f32::MAX / 4.0)))
                        .layout(Layout::top_down(Align::Min)),
                );
                let ui_s = &mut section;
                match sec {
                    Sec::Library => sec_library(app, ui_s),
                    Sec::Sources => sec_sources(app, ui_s),
                    Sec::Playback => sec_playback(app, ui_s),
                    Sec::Equalizer => sec_equalizer(app, ui_s),
                    Sec::Scrobbling => sec_scrobbling(app, ui_s),
                    Sec::Online => sec_online(app, ui_s),
                    Sec::Tagger => sec_tagger(app, ui_s),
                    Sec::Appearance => sec_appearance(app, ui_s),
                    Sec::Startup => sec_startup(app, ui_s),
                    Sec::Desktop => sec_desktop(app, ui_s),
                    Sec::Experimental => sec_experimental(app, ui_s),
                    Sec::About => sec_about(app, ui_s),
                }
                ui.advance_cursor_after_rect(Rect::from_min_size(
                    top,
                    vec2(w, section.min_rect().height()),
                ));
                if scroll_to == Some(i) {
                    ui.scroll_to_rect(Rect::from_min_size(top, vec2(w, 24.0)), Some(Align::TOP));
                    jumped = true;
                }
                if !searching && top.y <= ui.clip_rect().top() + 40.0 {
                    current = i;
                }
                ui.add_space(SECTION_GAP);
            }
            if !any {
                empty_state(
                    ui,
                    ph::MAGNIFYING_GLASS,
                    &aulos_core::fl!("settings-no-match-title"),
                    &aulos_core::fl!("settings-no-match-hint"),
                    None,
                );
            }
            ui.add_space(32.0);
        });

    let st = &mut app.views.settings;
    if jumped {
        st.scroll_to = None;
    } else {
        st.current = current;
    }
}

// ---------------------------------------------------------------- primitives

fn heading(ui: &mut Ui, pal: &Palette, title: &str, note: &str) {
    let note = if note.is_empty() {
        String::new()
    } else {
        format!("   {note}")
    };
    crate::widgets::truncated_runs(
        ui,
        &[
            (title, theme::semibold(17.0), pal.text),
            (&note, theme::regular(12.5), pal.text_3),
        ],
    );
}

/// A setting row: title and wrapped description left, `control` right, a
/// hairline under it. `reserve` is the width the control may take, so the
/// description wraps before running under it.
fn row(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    desc: &str,
    net: bool,
    reserve: f32,
    control: impl FnOnce(&mut Ui),
) {
    let w = ui.available_width();
    // A wide control (segmented, slider) would squeeze the text to a sliver on
    // a phone: it drops under the text instead.
    let stacked = w - reserve - 16.0 < 140.0 && reserve > 100.0;
    let text_w = if stacked {
        w
    } else {
        (w - reserve - 16.0).max(80.0)
    };
    let pad = if desc.is_empty() { 10.0 } else { 11.0 };
    let title_g = ui
        .painter()
        .layout_no_wrap(title.to_owned(), theme::semibold(13.0), pal.text);
    let desc_g = (!desc.is_empty()).then(|| {
        ui.painter()
            .layout(desc.to_owned(), theme::regular(12.0), pal.text_2, text_w)
    });
    let text_h = title_g.size().y + desc_g.as_ref().map_or(0.0, |g| g.size().y + 2.0);
    let control_h = if stacked { 36.0 } else { 0.0 };
    let h = text_h.max(30.0) + pad * 2.0 + control_h;
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let top = if stacked {
        rect.top() + pad
    } else {
        rect.top() + (h - text_h) / 2.0
    };
    let title_w = title_g.size().x;
    ui.painter()
        .galley(pos2(rect.left(), top), title_g.clone(), pal.text);
    if net {
        let badge = aulos_core::fl!("settings-badge-network");
        let bw = text_width(ui, &badge, theme::mono_semibold(9.5)) + 2.8 + 8.0;
        let b = Rect::from_min_size(
            pos2(
                rect.left() + title_w + 8.0,
                top + (title_g.size().y - 15.0) / 2.0,
            ),
            vec2(bw, 15.0),
        );
        ui.painter().rect_stroke(
            b,
            CornerRadius::same(3),
            Stroke::new(1.0, pal.stroke_strong),
            StrokeKind::Inside,
        );
        paint_text_spaced(
            ui.painter(),
            b,
            Align::Center,
            &badge,
            theme::mono_semibold(9.5),
            pal.text_2,
            0.4,
        );
    }
    if let Some(g) = desc_g {
        ui.painter().galley(
            pos2(rect.left(), top + title_g.size().y + 2.0),
            g,
            pal.text_2,
        );
    }
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, pal.stroke.alpha(0.6)),
    );
    let (area, layout) = if stacked {
        (
            Rect::from_min_max(
                pos2(rect.left(), rect.bottom() - pad - control_h),
                pos2(rect.right(), rect.bottom() - pad),
            ),
            Layout::left_to_right(Align::Center),
        )
    } else {
        (rect, Layout::right_to_left(Align::Center))
    };
    let mut child = ui.new_child(UiBuilder::new().max_rect(area).layout(layout));
    child.spacing_mut().item_spacing.x = 10.0;
    control(&mut child);
}

fn toggle_row(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    desc: &str,
    net: bool,
    value: bool,
) -> Option<bool> {
    let mut v = value;
    row(ui, pal, title, desc, net, 34.0, |ui| {
        toggle(ui, &mut v);
    });
    (v != value).then_some(v)
}

fn seg_row(
    ui: &mut Ui,
    pal: &Palette,
    id: &str,
    title: &str,
    desc: &str,
    labels: &[&str],
    selected: usize,
) -> Option<usize> {
    let mut sel = selected;
    let mut changed = false;
    let reserve = labels
        .iter()
        .map(|l| l.len() as f32 * 7.5 + 24.0)
        .sum::<f32>()
        + 8.0;
    row(ui, pal, title, desc, false, reserve, |ui| {
        ui.push_id(id, |ui| {
            changed = segmented(ui, &mut sel, labels, 26.0);
        });
    });
    changed.then_some(sel)
}

fn slider_row(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    desc: &str,
    frac: f32,
    label: &str,
) -> Option<f32> {
    let mut out = None;
    row(ui, pal, title, desc, false, 230.0, |ui| {
        let (r, _) = ui.allocate_exact_size(vec2(44.0, 16.0), Sense::hover());
        paint_text(
            ui.painter(),
            r,
            Align::Max,
            label,
            theme::mono(12.0),
            pal.text,
        );
        out = value_slider(ui, pal, frac, 160.0);
    });
    out
}

fn select_row(
    ui: &mut Ui,
    pal: &Palette,
    title: &str,
    desc: &str,
    current: &str,
    options: &[String],
) -> Option<usize> {
    let mut out = None;
    row(ui, pal, title, desc, false, 190.0, |ui| {
        out = select(ui, pal, current, options);
    });
    out
}

/// A rail with an always-visible thumb; the new fraction while it is dragged.
fn value_slider(ui: &mut Ui, pal: &Palette, frac: f32, width: f32) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(vec2(width + 14.0, 18.0), Sense::click_and_drag());
    let rail = Rect::from_center_size(rect.center(), vec2(width, 4.0));
    let frac = frac.clamp(0.0, 1.0);
    ui.painter()
        .rect_filled(rail, CornerRadius::same(2), pal.track);
    ui.painter().rect_filled(
        Rect::from_min_size(rail.min, vec2(rail.width() * frac, 4.0)),
        CornerRadius::same(2),
        pal.text_1,
    );
    ui.painter().circle_filled(
        pos2(rail.left() + rail.width() * frac, rail.center().y),
        7.0,
        pal.text,
    );
    if resp.dragged() || resp.clicked() {
        resp.interact_pointer_pos()
            .map(|p| ((p.x - rail.left()) / rail.width()).clamp(0.0, 1.0))
    } else {
        None
    }
}

/// A dropdown: the current value and, in a menu, the options. The picked index.
fn select(ui: &mut Ui, pal: &Palette, current: &str, options: &[String]) -> Option<usize> {
    let font = theme::regular(13.0);
    let w = (text_width(ui, current, font.clone()) + 20.0 + 11.0 + 10.0).max(140.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
    let r = CornerRadius::same(6);
    ui.painter().rect_filled(rect, r, pal.bg);
    ui.painter().rect_stroke(
        rect,
        r,
        Stroke::new(
            1.0,
            if resp.hovered() {
                pal.stroke_strong
            } else {
                pal.input_bd
            },
        ),
        StrokeKind::Inside,
    );
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 10.0, rect.top()),
        pos2(rect.right() - 28.0, rect.bottom()),
    );
    paint_text(ui.painter(), text_rect, Align::Min, current, font, pal.text);
    paint_icon(
        ui.painter(),
        pos2(rect.right() - 15.0, rect.center().y),
        ph::CARET_DOWN,
        11.0,
        pal.text_3,
        false,
    );

    let mut picked = None;
    egui::Popup::menu(&resp).width(rect.width()).show(|ui| {
        ui.set_min_width(rect.width());
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                for (i, option) in options.iter().enumerate() {
                    let (r, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                    if resp.hovered() {
                        ui.painter()
                            .rect_filled(r, CornerRadius::same(5), pal.hover);
                    }
                    let selected = option == current;
                    let rr = Rect::from_min_max(
                        pos2(r.left() + 8.0, r.top()),
                        pos2(r.right() - 26.0, r.bottom()),
                    );
                    paint_text(
                        ui.painter(),
                        rr,
                        Align::Min,
                        option,
                        theme::regular(13.0),
                        if selected { pal.text } else { pal.text_1 },
                    );
                    if selected {
                        paint_icon(
                            ui.painter(),
                            pos2(r.right() - 14.0, r.center().y),
                            ph::CHECK,
                            13.0,
                            pal.text_2,
                            false,
                        );
                    }
                    if resp.clicked() {
                        picked = Some(i);
                    }
                }
            });
    });
    picked
}

fn input(
    ui: &mut Ui,
    pal: &Palette,
    text: &mut String,
    hint: &str,
    width: f32,
    password: bool,
) -> egui::Response {
    let edit = egui::TextEdit::singleline(text)
        .hint_text(egui::RichText::new(hint).color(pal.text_3))
        .desired_width(width)
        .margin(vec2(10.0, 6.0))
        .password(password)
        .font(theme::regular(13.0))
        .background_color(pal.bg);
    let resp = ui.add(edit);
    if password {
        crate::widgets::mark_password(ui.ctx(), resp.id);
    }
    ui.painter().rect_stroke(
        resp.rect,
        CornerRadius::same(6),
        Stroke::new(
            1.0,
            if resp.has_focus() {
                pal.text
            } else {
                pal.input_bd
            },
        ),
        StrokeKind::Inside,
    );
    resp
}

fn labelled_input(
    ui: &mut Ui,
    pal: &Palette,
    label: &str,
    text: &mut String,
    hint: &str,
    password: bool,
) -> egui::Response {
    ui.label(
        egui::RichText::new(label)
            .font(theme::regular(12.0))
            .color(pal.text_2),
    );
    ui.add_space(4.0);
    let w = ui.available_width().min(420.0);
    let resp = input(ui, pal, text, hint, w, password);
    ui.add_space(10.0);
    resp
}

fn card_frame(pal: &Palette) -> Frame {
    Frame::new()
        .fill(pal.surface_1)
        .stroke(Stroke::new(1.0, pal.stroke))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
}

fn card_bg(ui: &Ui, pal: &Palette, rect: Rect, border: Color32) {
    ui.painter()
        .rect_filled(rect, CornerRadius::same(10), pal.surface_1);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, border),
        StrokeKind::Inside,
    );
}

fn dashed_card(ui: &Ui, rect: Rect, color: Color32) {
    let (l, t, r, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    ui.painter().extend(Shape::dashed_line(
        &[pos2(l, t), pos2(r, t), pos2(r, b), pos2(l, b), pos2(l, t)],
        Stroke::new(1.0, color),
        4.0,
        3.0,
    ));
}

fn small_text(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::regular(12.0))
            .color(color),
    );
}

// ------------------------------------------------------------ async plumbing

/// Applies what off-thread work (scrobbling flows, the folder picker) produced.
pub fn pump(app: &mut App) {
    let msgs: Vec<Msg> = app
        .views
        .settings
        .channel
        .as_ref()
        .map(|(_, rx)| rx.try_iter().collect())
        .unwrap_or_default();
    for msg in msgs {
        match msg {
            Msg::Scrobble(m) => scrobble_update(app, m),
            Msg::Folder(result) => folder_picked(app, result),
            Msg::Access(result) => access_asked(app, result),
        }
    }
}

fn scrobble_update(app: &mut App, msg: ScrobbleMessage) {
    if let ScrobbleMessage::LovedFetched(Ok(loved)) = &msg {
        apply_loved(app, loved);
        return;
    }
    let import_done = match &msg {
        ScrobbleMessage::ImportDone(service, res) => Some((*service, res.clone())),
        _ => None,
    };
    let affects_suggestions = matches!(
        msg,
        ScrobbleMessage::SetOnlineSuggestions(_)
            | ScrobbleMessage::LbValidated(_)
            | ScrobbleMessage::AuthCompleted(..)
            | ScrobbleMessage::Disconnect(_)
            | ScrobbleMessage::SaveLastFmKeys
    );
    let (effect, changed) = app
        .session
        .playback
        .scrobble
        .update(msg, &mut app.session.config.config);
    if changed {
        app.session.config.save();
    }
    let tx = app.views.settings.sender();
    let ctx = app.ctx.clone();
    let _ = effect.spawn(&app.handle, move |m| {
        if tx.send(Msg::Scrobble(m)).is_ok() {
            ctx.request_repaint();
        }
    });
    if changed && affects_suggestions {
        let similar = app.session.load_similar();
        app.run(similar);
    }
    if let Some((service, res)) = import_done {
        finish_import(app, service, res);
    }
}

fn finish_import(app: &mut App, service: Service, res: Result<ImportOutcome, String>) {
    let name = service.display_name();
    match res {
        Err(message) => app.toast_error(aulos_core::fl!(
            "settings-import-failed",
            service = name,
            reason = message
        )),
        Ok(out) => {
            if let Some(message) = &out.error {
                app.toast_error(aulos_core::fl!(
                    "settings-import-failed",
                    service = name,
                    reason = message.as_str()
                ));
            } else if out.cancelled {
                app.toast(aulos_core::fl!("settings-import-cancelled", service = name));
            } else {
                app.toast(aulos_core::fl!(
                    "settings-import-done",
                    count = out.progress.imported,
                    service = name
                ));
            }
            if out.progress.imported > 0 {
                let home = app.session.load_home(true);
                app.run(home);
            }
        }
    }
}

/// Two-way sync with Last.fm's loved tracks.
fn apply_loved(app: &mut App, loved: &[(String, String)]) {
    let LovedSyncPlan {
        to_favorite,
        to_love,
    } = plan_sync(&app.session.library.tracks, loved);
    let (added, pushed) = (to_favorite.len(), to_love.len());
    for (artist, title) in to_love {
        app.session.playback.scrobble.love_bulk(artist, title);
    }
    for id in to_favorite {
        // These toggles come from Last.fm itself; don't echo them back.
        app.session.playback.scrobble.suppress_next_love(id.clone());
        let effect = app.session.toggle_favorite(id);
        app.run(effect);
    }
    app.session.playback.scrobble.finish_sync(aulos_core::fl!(
        "settings-loved-synced",
        added = added,
        pushed = pushed
    ));
}

/// Opens the native folder chooser; the answer arrives as [`Msg::Folder`].
fn pick_folder(app: &mut App) {
    if app.views.settings.picking_folder {
        return;
    }
    app.views.settings.picking_folder = true;
    let tx = app.views.settings.sender();
    let ctx = app.ctx.clone();
    app.handle.spawn(async move {
        let result =
            dialogs::pick_music_folder(&aulos_core::fl!("settings-pick-folder-title")).await;
        if tx.send(Msg::Folder(result)).is_ok() {
            ctx.request_repaint();
        }
    });
}

fn folder_picked(app: &mut App, result: Result<Option<PathBuf>, String>) {
    app.views.settings.picking_folder = false;
    match result {
        Ok(Some(path)) => add_folder(app, path),
        // Dismissing the dialog is not worth a toast.
        Ok(None) => {}
        Err(reason) => app.toast_error(aulos_core::fl!(
            "settings-add-folder-failed",
            reason = reason
        )),
    }
}

fn add_folder(app: &mut App, path: PathBuf) {
    let added = app.session.add_music_dir(path);
    // In the browser a folder picked again (to hand its files back after a
    // reload) is already listed, but still has to be read.
    if added || cfg!(target_arch = "wasm32") {
        rescan(app);
    }
}

/// Asks the browser for access to the music folder `path`, then rescans.
fn allow_folder(app: &mut App, path: PathBuf) {
    allow_folders(app, vec![path]);
}

/// Asks the browser for access to every music folder still waiting for it
/// (one click, so one gesture, grants all), then rescans.
pub fn allow_pending_folders(app: &mut App) {
    let pending = pending_permission(app);
    allow_folders(app, pending);
}

fn allow_folders(app: &mut App, paths: Vec<PathBuf>) {
    let tx = app.views.settings.sender();
    let ctx = app.ctx.clone();
    app.handle.spawn(async move {
        let mut result = Ok(());
        for path in paths {
            if let Err(reason) = dialogs::allow_music_folder(path).await {
                result = Err(reason);
            }
        }
        if tx.send(Msg::Access(result)).is_ok() {
            ctx.request_repaint();
        }
    });
}

fn access_asked(app: &mut App, result: Result<(), String>) {
    if let Err(reason) = result {
        app.toast_error(reason);
    }
    // What was granted can be read (and shown) now.
    rescan(app);
}

fn pending_permission(app: &App) -> Vec<PathBuf> {
    app.session
        .config
        .config
        .music_dirs
        .iter()
        .filter(|d| access_of(d) == Access::NeedsPermission)
        .cloned()
        .collect()
}

/// At startup: tells the user what a music folder needs before its files can
/// be played again (the browser forgets permissions between visits).
pub fn notify_folders_needing_access(app: &mut App) {
    let dirs = &app.session.config.config.music_dirs;
    if !pending_permission(app).is_empty() {
        app.toast_rich(
            ph::FOLDER_SIMPLE,
            "",
            aulos_core::fl!("settings-folders-need-access-subject"),
            format!(
                " {}",
                aulos_core::fl!("settings-folders-need-access-suffix")
            ),
            Some(crate::app::ToastAction::AllowMusicFolders),
        );
    } else if dirs.iter().any(|d| access_of(d) == Access::NeedsRepick) {
        app.toast(aulos_core::fl!("settings-folders-repick-notice"));
    }
}

fn rescan(app: &mut App) {
    let reload = app.session.scan_library();
    app.run(reload);
}

// ------------------------------------------------------------------- library

fn refresh_folder_cache(app: &mut App) {
    let generation = app.session.library.generation();
    let dirs = &app.session.config.config.music_dirs;
    let cache = &app.views.settings.folders;
    if cache.generation == Some(generation) && cache.dirs == *dirs {
        return;
    }
    let info = dirs
        .iter()
        .map(|d| {
            (
                app.session
                    .library
                    .tracks
                    .iter()
                    .filter(|t| t.path.starts_with(d))
                    .count(),
                aulos_core::library::music_dir_exists(d),
            )
        })
        .collect();
    app.views.settings.folders = FolderCache {
        generation: Some(generation),
        dirs: dirs.clone(),
        info,
    };
}

enum FolderAct {
    Rescan,
    Remove(usize),
    /// Ask the browser for access to folder `usize`.
    Allow(usize),
    /// Pick the folder again to hand its files back to the browser.
    PickAgain,
}

fn sec_library(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    refresh_folder_cache(app);
    let scanning = app.session.library.scanning;
    heading(
        ui,
        &pal,
        &aulos_core::fl!("settings-library"),
        &if scanning {
            aulos_core::fl!("settings-library-scanning")
        } else {
            aulos_core::fl!("settings-library-note")
        },
    );
    ui.add_space(10.0);

    let mut act = None;
    let rows: Vec<(String, usize, bool, Access)> = app
        .views
        .settings
        .folders
        .dirs
        .iter()
        .zip(&app.views.settings.folders.info)
        .map(|(d, (n, ok))| (d.display().to_string(), *n, *ok, access_of(d)))
        .collect();
    if rows.is_empty() {
        small_text(ui, &aulos_core::fl!("settings-folders-empty"), pal.text_2);
        ui.add_space(8.0);
    }
    for (i, (path, tracks, exists, access)) in rows.iter().enumerate() {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
        card_bg(ui, &pal, rect, pal.stroke);
        paint_icon(
            ui.painter(),
            pos2(rect.left() + 21.0, rect.center().y),
            ph::FOLDER_SIMPLE,
            18.0,
            pal.text_2,
            false,
        );
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(rect.shrink2(vec2(12.0, 0.0)))
                .layout(Layout::right_to_left(Align::Center)),
        );
        child.spacing_mut().item_spacing.x = 8.0;
        if IconBtn::new(ph::TRASH)
            .size(28.0)
            .icon_size(15.0)
            .color(pal.text_3)
            .tip(aulos_core::fl!("settings-folder-remove-tip"))
            .show(&mut child)
            .clicked()
        {
            act = Some(FolderAct::Remove(i));
        }
        let (label, icon, next) = match access {
            Access::Granted => (
                aulos_core::fl!("settings-folder-rescan"),
                ph::ARROWS_CLOCKWISE,
                FolderAct::Rescan,
            ),
            Access::NeedsPermission => (
                aulos_core::fl!("settings-folder-allow"),
                ph::LOCK_KEY_OPEN,
                FolderAct::Allow(i),
            ),
            Access::NeedsRepick => (
                aulos_core::fl!("settings-folder-pick-again"),
                ph::FOLDER_OPEN,
                FolderAct::PickAgain,
            ),
        };
        let enabled = match access {
            Access::Granted => !scanning,
            Access::NeedsPermission => true,
            Access::NeedsRepick => !app.views.settings.picking_folder,
        };
        if pill_button_enabled(
            &mut child,
            &label,
            Some(icon),
            PillKind::Secondary,
            28.0,
            0.0,
            enabled,
        )
        .clicked()
        {
            act = Some(next);
        }
        let text_right = child.min_rect().left() - 12.0;
        let tx = rect.left() + 12.0 + 18.0 + 12.0;
        let line1 = Rect::from_min_max(
            pos2(tx, rect.top() + 9.0),
            pos2(text_right, rect.top() + 26.0),
        );
        let line2 = Rect::from_min_max(
            pos2(tx, rect.top() + 27.0),
            pos2(text_right, rect.top() + 43.0),
        );
        paint_text(
            ui.painter(),
            line1,
            Align::Min,
            path,
            theme::mono(12.5),
            pal.text,
        );
        let (meta, color) = if *access == Access::NeedsPermission {
            (
                aulos_core::fl!("settings-folder-tracks-allow", count = (*tracks)),
                pal.warn,
            )
        } else if *access == Access::NeedsRepick {
            (
                aulos_core::fl!("settings-folder-tracks-repick", count = (*tracks)),
                pal.warn,
            )
        } else if *exists {
            (
                aulos_core::fl!("settings-folder-tracks", count = (*tracks)),
                pal.text_2,
            )
        } else {
            (aulos_core::fl!("settings-folder-unavailable"), pal.warn)
        };
        paint_text(
            ui.painter(),
            line2,
            Align::Min,
            &meta,
            theme::regular(12.0),
            color,
        );
        ui.add_space(10.0);
    }

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if pill_button_enabled(
            ui,
            &aulos_core::fl!("settings-folder-add"),
            Some(ph::PLUS),
            PillKind::Outline,
            28.0,
            0.0,
            !app.views.settings.picking_folder,
        )
        .clicked()
        {
            pick_folder(app);
        }
        // A page has no paths to type.
        if !cfg!(target_arch = "wasm32") {
            let st = &mut app.views.settings;
            // Leave room for the "Add" pill and the field margins; the field
            // shrinks on narrow pages.
            let field_w = (ui.available_width() - 100.0).clamp(40.0, 240.0);
            input(
                ui,
                &pal,
                &mut st.path_input,
                &aulos_core::fl!("settings-folder-path-hint"),
                field_w,
                false,
            );
            let path = PathBuf::from(st.path_input.trim());
            let ok = !st.path_input.trim().is_empty() && path.is_dir();
            if pill_button_enabled(
                ui,
                &aulos_core::fl!("settings-folder-add-path"),
                None,
                PillKind::Secondary,
                28.0,
                0.0,
                ok,
            )
            .clicked()
            {
                st.path_input.clear();
                add_folder(app, path);
            }
        }
    });

    match act {
        Some(FolderAct::Rescan) => rescan(app),
        Some(FolderAct::Remove(i)) if app.session.remove_music_dir(i) => rescan(app),
        Some(FolderAct::Remove(_)) => {}
        Some(FolderAct::Allow(i)) => {
            if let Some(path) = app.session.config.config.music_dirs.get(i).cloned() {
                allow_folder(app, path);
            }
        }
        Some(FolderAct::PickAgain) => pick_folder(app),
        None => {}
    }

    // A page cannot watch a folder for changes.
    if !cfg!(target_arch = "wasm32") {
        let watch = app.session.config.config.watch_for_changes;
        if let Some(on) = toggle_row(
            ui,
            &pal,
            &aulos_core::fl!("settings-watch-title"),
            &aulos_core::fl!("settings-watch-desc"),
            false,
            watch,
        ) {
            app.set_watch_for_changes(on);
        }
    }

    ui.add_space(SECTION_GAP);
    heading(
        ui,
        &pal,
        &aulos_core::fl!("settings-artist-tags-heading"),
        &aulos_core::fl!("settings-artist-tags-note"),
    );
    ui.add_space(4.0);
    let cfg = &app.session.config.config;
    let split = cfg.split_artist_tags;
    if let Some(v) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-split-title"),
        &aulos_core::fl!("settings-split-desc"),
        false,
        split,
    ) {
        app.session.set_split_artist_tags(v);
    }
    let mut submit = false;
    let mut reset = false;
    row(
        ui,
        &pal,
        &aulos_core::fl!("settings-delimiters-title"),
        &aulos_core::fl!("settings-delimiters-desc"),
        false,
        330.0,
        |ui| {
            if pill_button(
                ui,
                &aulos_core::fl!("settings-reset"),
                None,
                PillKind::Ghost,
                28.0,
                0.0,
            )
            .clicked()
            {
                reset = true;
            }
            let resp = input(
                ui,
                &pal,
                &mut app.views.settings.delimiters_input,
                "; | feat. | &",
                250.0,
                false,
            );
            submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        },
    );
    if reset {
        let defaults: Vec<String> = DEFAULT_DELIMITERS.iter().map(|s| (*s).to_owned()).collect();
        app.views.settings.delimiters_input = defaults.join(" | ");
        app.session.set_artist_tag_delimiters(defaults);
    } else if submit {
        let parsed = parse_delimiters_input(&app.views.settings.delimiters_input);
        app.session.set_artist_tag_delimiters(parsed);
    }

    ui.add_space(SECTION_GAP);
    heading(
        ui,
        &pal,
        &aulos_core::fl!("settings-compilations-heading"),
        "",
    );
    ui.add_space(4.0);
    let cfg = &app.session.config.config;
    let (show_comp, relative) = (cfg.show_compilations_in_artists, cfg.m3u_relative_paths);
    if let Some(v) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-show-va-title"),
        &aulos_core::fl!("settings-show-va-desc"),
        false,
        show_comp,
    ) {
        app.session.set_show_compilations_in_artists(v);
    }
    if let Some(v) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-m3u-relative-title"),
        &aulos_core::fl!("settings-m3u-relative-desc"),
        false,
        relative,
    ) {
        app.session.config.config.m3u_relative_paths = v;
        app.session.config.save();
    }
}

// ------------------------------------------------------------------- sources

#[derive(Clone)]
struct Card {
    at: CardRef,
    id: String,
    name: String,
    addr: String,
    status: Option<String>,
}

fn server_cards(app: &App) -> Vec<Card> {
    let p = &app.session.providers;
    let mpd = p.mpd_edit_states.iter().enumerate().map(|(i, s)| Card {
        at: CardRef {
            kind: Kind::Mpd,
            index: i,
        },
        id: s.id.clone(),
        name: s.name.clone(),
        addr: format!("{}:{}", s.host, s.port),
        status: p.mpd_connection_status.get(i).cloned().flatten(),
    });
    let sub = p
        .subsonic_edit_states
        .iter()
        .enumerate()
        .map(|(i, s)| Card {
            at: CardRef {
                kind: Kind::Subsonic,
                index: i,
            },
            id: s.id.clone(),
            name: s.name.clone(),
            addr: s.url.clone(),
            status: p.subsonic_connection_status.get(i).cloned().flatten(),
        });
    mpd.chain(sub).collect()
}

enum ServerAct {
    Test(CardRef),
    Edit(CardRef),
}

fn start_test(app: &mut App, at: CardRef) {
    let p = &mut app.session.providers;
    let effect = match at.kind {
        Kind::Mpd => {
            if let Some(s) = p.mpd_connection_status.get_mut(at.index) {
                *s = None;
            }
            p.test_mpd_connection(at.index)
        }
        Kind::Subsonic => {
            if let Some(s) = p.subsonic_connection_status.get_mut(at.index) {
                *s = None;
            }
            p.test_subsonic_connection(at.index)
        }
    };
    if !app.views.settings.testing.contains(&at) {
        app.views.settings.testing.push(at);
    }
    app.run(effect);
}

fn sec_sources(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(
        ui,
        &pal,
        &aulos_core::fl!("settings-sec-sources"),
        &aulos_core::fl!("settings-sources-note"),
    );
    ui.add_space(12.0);

    let cards = server_cards(app);
    // A finished test (its status arrived) is no longer "testing".
    app.views
        .settings
        .testing
        .retain(|c| cards.iter().any(|k| k.at == *c && k.status.is_none()));
    let active_id = app.session.providers.registry.active_id().to_owned();

    let mut act = None;
    if cards.is_empty() {
        small_text(ui, &aulos_core::fl!("settings-sources-empty"), pal.text_2);
        ui.add_space(8.0);
    } else {
        const CARD_H: f32 = 114.0;
        const GAP: f32 = 10.0;
        let w = ui.available_width();
        let cols = (((w + GAP) / (210.0 + GAP)).floor() as usize).max(1);
        let card_w = (w - GAP * (cols - 1) as f32) / cols as f32;
        let rows = cards.len().div_ceil(cols);
        let (base, _) = ui.allocate_exact_size(
            vec2(w, rows as f32 * CARD_H + (rows - 1) as f32 * GAP),
            Sense::hover(),
        );
        for (n, card) in cards.iter().enumerate() {
            let (col, r) = (n % cols, n / cols);
            let rect = Rect::from_min_size(
                base.min + vec2(col as f32 * (card_w + GAP), r as f32 * (CARD_H + GAP)),
                vec2(card_w, CARD_H),
            );
            let testing = app.views.settings.testing.contains(&card.at);
            let editing = app.views.settings.edit == Some(card.at);
            let view = CardView {
                testing,
                editing,
                active: card.id == active_id,
            };
            if let Some(a) = server_card(ui, &pal, app.accent.fill, rect, card, view) {
                act = Some(a);
            }
        }
    }
    match act {
        Some(ServerAct::Test(at)) => start_test(app, at),
        Some(ServerAct::Edit(at)) => {
            let st = &mut app.views.settings;
            st.edit = if st.edit == Some(at) { None } else { Some(at) };
            st.edit_error = None;
        }
        None => {}
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if pill_button(
            ui,
            &aulos_core::fl!("settings-add-mpd"),
            Some(ph::PLUS),
            PillKind::Ghost,
            28.0,
            0.0,
        )
        .clicked()
        {
            app.session.providers.add_mpd_card();
            app.views.settings.edit = Some(CardRef {
                kind: Kind::Mpd,
                index: app.session.providers.mpd_edit_states.len() - 1,
            });
            app.views.settings.edit_error = None;
        }
        if pill_button(
            ui,
            &aulos_core::fl!("settings-add-subsonic"),
            Some(ph::PLUS),
            PillKind::Ghost,
            28.0,
            0.0,
        )
        .clicked()
        {
            app.session.providers.add_subsonic_card();
            app.views.settings.edit = Some(CardRef {
                kind: Kind::Subsonic,
                index: app.session.providers.subsonic_edit_states.len() - 1,
            });
            app.views.settings.edit_error = None;
        }
    });

    if let Some(at) = app.views.settings.edit {
        ui.add_space(8.0);
        server_editor(app, ui, at);
    }
}

/// How a server card is currently drawn.
struct CardView {
    testing: bool,
    editing: bool,
    active: bool,
}

fn server_card(
    ui: &mut Ui,
    pal: &Palette,
    accent: Color32,
    rect: Rect,
    card: &Card,
    view: CardView,
) -> Option<ServerAct> {
    let CardView {
        testing,
        editing,
        active,
    } = view;
    let (dot, text, text_color) = if testing {
        (
            pal.warn,
            aulos_core::fl!("settings-card-testing"),
            pal.text_1,
        )
    } else if let Some(s) = &card.status {
        // A failed test reads `<failure>: <reason>`; a success is the bare word.
        if s.contains(':') {
            (pal.danger, s.clone(), pal.danger)
        } else {
            (pal.ok, s.clone(), pal.text_1)
        }
    } else if active {
        (pal.ok, aulos_core::fl!("settings-card-active"), pal.text_1)
    } else {
        (
            pal.text_4,
            aulos_core::fl!("settings-card-untested"),
            pal.text_3,
        )
    };
    let failed = !testing && card.status.as_deref().is_some_and(|s| s.contains(':'));
    let border = if editing {
        accent
    } else if failed {
        pal.danger.alpha(0.35)
    } else {
        pal.stroke
    };
    card_bg(ui, pal, rect, border);
    let tile = Rect::from_min_size(rect.min + vec2(12.0, 12.0), vec2(30.0, 30.0));
    ui.painter()
        .rect_filled(tile, CornerRadius::same(7), pal.surface_sel);
    let glyph = if card.at.kind == Kind::Mpd {
        ph::HARD_DRIVES
    } else {
        ph::CLOUD
    };
    paint_icon(ui.painter(), tile.center(), glyph, 16.0, pal.text, false);
    let tx = tile.right() + 10.0;
    let name = Rect::from_min_max(
        pos2(tx, rect.top() + 10.0),
        pos2(rect.right() - 12.0, rect.top() + 28.0),
    );
    let addr = Rect::from_min_max(
        pos2(tx, rect.top() + 28.0),
        pos2(rect.right() - 12.0, rect.top() + 44.0),
    );
    paint_text(
        ui.painter(),
        name,
        Align::Min,
        &card.name,
        theme::semibold(13.0),
        pal.text,
    );
    paint_text(
        ui.painter(),
        addr,
        Align::Min,
        &card.addr,
        theme::mono(11.0),
        pal.text_2,
    );
    let cy = rect.top() + 59.0;
    ui.painter()
        .circle_filled(pos2(rect.left() + 16.0, cy), 4.0, dot);
    let status = Rect::from_min_max(
        pos2(rect.left() + 28.0, cy - 8.0),
        pos2(rect.right() - 12.0, cy + 8.0),
    );
    paint_text(
        ui.painter(),
        status,
        Align::Min,
        &text,
        theme::regular(12.0),
        text_color,
    );
    if failed {
        // The status line is truncated; the full reason is on hover.
        ui.interact(
            status,
            ui.id().with(("server-status", card.at.index)),
            Sense::hover(),
        )
        .on_hover_text(&text);
    }

    let mut act = None;
    let mut buttons = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_size(
                pos2(rect.left() + 12.0, rect.top() + 76.0),
                vec2(rect.width() - 24.0, 26.0),
            ))
            .layout(Layout::left_to_right(Align::Center)),
    );
    buttons.spacing_mut().item_spacing.x = 6.0;
    let (label, icon) = if testing {
        (aulos_core::fl!("settings-card-testing-btn"), None)
    } else if failed {
        (aulos_core::fl!("settings-retry"), Some(ph::ARROW_CLOCKWISE))
    } else {
        (aulos_core::fl!("settings-test"), Some(ph::PLUGS_CONNECTED))
    };
    if testing {
        buttons.add(egui::Spinner::new().size(14.0).color(pal.text_2));
    }
    if pill_button_enabled(
        &mut buttons,
        &label,
        icon,
        PillKind::Secondary,
        26.0,
        0.0,
        !testing,
    )
    .clicked()
    {
        act = Some(ServerAct::Test(card.at));
    }
    if pill_button(
        &mut buttons,
        &aulos_core::fl!("settings-edit"),
        None,
        PillKind::Ghost,
        26.0,
        0.0,
    )
    .clicked()
    {
        act = Some(ServerAct::Edit(card.at));
    }
    act
}

#[derive(Default)]
struct EditorOut {
    test: bool,
    save: bool,
    remove: bool,
    close: bool,
}

fn server_editor(app: &mut App, ui: &mut Ui, at: CardRef) {
    let pal = app.pal;
    let valid = match at.kind {
        Kind::Mpd => at.index < app.session.providers.mpd_edit_states.len(),
        Kind::Subsonic => at.index < app.session.providers.subsonic_edit_states.len(),
    };
    if !valid {
        app.views.settings.edit = None;
        return;
    }
    let status = match at.kind {
        Kind::Mpd => app
            .session
            .providers
            .mpd_connection_status
            .get(at.index)
            .cloned()
            .flatten(),
        Kind::Subsonic => app
            .session
            .providers
            .subsonic_connection_status
            .get(at.index)
            .cloned()
            .flatten(),
    };
    let testing = app.views.settings.testing.contains(&at);
    let error = app.views.settings.edit_error.clone();
    let mut out = EditorOut::default();
    card_frame(&pal).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        match at.kind {
            Kind::Mpd => {
                let s = &mut app.session.providers.mpd_edit_states[at.index];
                ui.label(
                    egui::RichText::new(aulos_core::fl!("settings-editor-mpd-title"))
                        .font(theme::semibold(14.0))
                        .color(pal.text),
                );
                ui.add_space(10.0);
                labelled_input(
                    ui,
                    &pal,
                    &aulos_core::fl!("settings-field-name"),
                    &mut s.name,
                    &aulos_core::fl!("settings-field-mpd-name-hint"),
                    false,
                );
                labelled_input(
                    ui,
                    &pal,
                    &aulos_core::fl!("settings-field-host"),
                    &mut s.host,
                    "localhost",
                    false,
                );
                labelled_input(
                    ui,
                    &pal,
                    &aulos_core::fl!("settings-field-port"),
                    &mut s.port,
                    "6600",
                    false,
                );
                labelled_input(
                    ui,
                    &pal,
                    &aulos_core::fl!("settings-field-password-optional"),
                    &mut s.password,
                    "",
                    true,
                );
            }
            Kind::Subsonic => {
                let s = &mut app.session.providers.subsonic_edit_states[at.index];
                ui.label(
                    egui::RichText::new(aulos_core::fl!("settings-editor-subsonic-title"))
                        .font(theme::semibold(14.0))
                        .color(pal.text),
                );
                ui.add_space(10.0);
                labelled_input(
                    ui,
                    &pal,
                    &aulos_core::fl!("settings-field-name"),
                    &mut s.name,
                    "Navidrome",
                    false,
                );
                labelled_input(
                    ui,
                    &pal,
                    &aulos_core::fl!("settings-field-server-url"),
                    &mut s.url,
                    "https://music.example.com",
                    false,
                );
                labelled_input(
                    ui,
                    &pal,
                    &aulos_core::fl!("settings-field-username"),
                    &mut s.username,
                    "",
                    false,
                );
                labelled_input(
                    ui,
                    &pal,
                    &aulos_core::fl!("settings-field-password-or-key"),
                    &mut s.password,
                    "",
                    true,
                );
                if cfg!(target_arch = "wasm32") {
                    // `fetch` cannot skip certificate checks: the browser
                    // alone decides which certificates it trusts.
                    s.accept_invalid_certs = false;
                    small_text(ui, &aulos_core::fl!("settings-web-tls-note"), pal.text_2);
                    ui.add_space(6.0);
                    small_text(ui, &aulos_core::fl!("settings-web-password-note"), pal.warn);
                } else {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        toggle(ui, &mut s.accept_invalid_certs);
                        small_text(
                            ui,
                            &aulos_core::fl!("settings-accept-invalid-certs"),
                            pal.text_2,
                        );
                    });
                }
                ui.add_space(10.0);
            }
        }
        if let Some(e) = &error {
            ui.label(
                egui::RichText::new(e)
                    .font(theme::regular(12.0))
                    .color(pal.danger),
            );
            ui.add_space(8.0);
        }
        if testing {
            small_text(
                ui,
                &aulos_core::fl!("settings-testing-connection"),
                pal.warn,
            );
            ui.add_space(8.0);
        } else if let Some(s) = &status {
            let failed = s.contains(':');
            small_text(ui, s, if failed { pal.danger } else { pal.ok });
            ui.add_space(8.0);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            out.save = pill_button(
                ui,
                &aulos_core::fl!("save"),
                None,
                PillKind::Primary,
                30.0,
                0.0,
            )
            .clicked();
            out.test = pill_button_enabled(
                ui,
                &aulos_core::fl!("settings-test-connection"),
                Some(ph::PLUGS_CONNECTED),
                PillKind::Secondary,
                30.0,
                0.0,
                !testing,
            )
            .clicked();
            out.close = pill_button(
                ui,
                &aulos_core::fl!("settings-cancel"),
                None,
                PillKind::Ghost,
                30.0,
                0.0,
            )
            .clicked();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                out.remove = pill_button(
                    ui,
                    &aulos_core::fl!("remove"),
                    Some(ph::TRASH),
                    PillKind::Danger,
                    30.0,
                    0.0,
                )
                .clicked();
            });
        });
    });

    if out.test {
        start_test(app, at);
    }
    if out.save {
        save_server(app, at);
    }
    if out.remove || out.close {
        let configured = match at.kind {
            Kind::Mpd => at.index < app.session.config.config.mpd_servers.len(),
            Kind::Subsonic => at.index < app.session.config.config.subsonic_servers.len(),
        };
        // Cancelling a saved card only closes the editor; a card never saved is dropped.
        if out.remove || !configured {
            remove_server(app, at);
        } else {
            reset_editor_fields(app, at);
        }
        app.views.settings.edit = None;
        app.views.settings.edit_error = None;
    }
}

/// Drops unsaved edits of a configured card by rebuilding it from the config.
fn reset_editor_fields(app: &mut App, at: CardRef) {
    use aulos_core::config::edit_state::{MpdEditState, SubsonicEditState};
    match at.kind {
        Kind::Mpd => {
            if let Some(entry) = app.session.config.config.mpd_servers.get(at.index).cloned()
                && let Some(slot) = app.session.providers.mpd_edit_states.get_mut(at.index)
            {
                *slot = MpdEditState::from_config(&entry);
            }
        }
        Kind::Subsonic => {
            if let Some(entry) = app
                .session
                .config
                .config
                .subsonic_servers
                .get(at.index)
                .cloned()
                && let Some(slot) = app.session.providers.subsonic_edit_states.get_mut(at.index)
            {
                *slot = SubsonicEditState::from_config(&entry);
            }
        }
    }
}

fn save_server(app: &mut App, at: CardRef) {
    let problem = match at.kind {
        Kind::Mpd => {
            let s = &app.session.providers.mpd_edit_states[at.index];
            if s.name.trim().is_empty() || s.host.trim().is_empty() {
                Some(aulos_core::fl!("settings-err-name-host"))
            } else if s.port.trim().parse::<u16>().is_err() {
                Some(aulos_core::fl!("settings-err-port"))
            } else {
                None
            }
        }
        Kind::Subsonic => {
            let s = &app.session.providers.subsonic_edit_states[at.index];
            if s.name.trim().is_empty() || s.url.trim().is_empty() {
                Some(aulos_core::fl!("settings-err-name-url"))
            } else if !s.url.trim().starts_with("http://") && !s.url.trim().starts_with("https://")
            {
                Some(aulos_core::fl!("settings-err-url-scheme"))
            } else if s.username.trim().is_empty() {
                Some(aulos_core::fl!("settings-err-username"))
            } else {
                None
            }
        }
    };
    if let Some(p) = problem {
        app.views.settings.edit_error = Some(p);
        return;
    }
    match at.kind {
        Kind::Mpd => {
            let reload = app.session.save_mpd_server(at.index);
            app.run(reload);
        }
        Kind::Subsonic => {
            let reinit = app.session.save_subsonic_server(at.index);
            app.run(reinit);
        }
    }
    app.views.settings.edit = None;
    app.views.settings.edit_error = None;
    app.views.settings.testing.clear();
    app.toast(aulos_core::fl!("settings-server-saved"));
}

fn remove_server(app: &mut App, at: CardRef) {
    match at.kind {
        Kind::Mpd => {
            let reload = app.session.remove_mpd_server(at.index);
            app.run(reload);
        }
        Kind::Subsonic => {
            let reinit = app.session.remove_subsonic_server(at.index);
            app.run(reinit);
        }
    }
    app.views.settings.testing.clear();
}

// ------------------------------------------------------------------ playback

const REPLAY_GAIN: [ReplayGainMode; 4] = [
    ReplayGainMode::Off,
    ReplayGainMode::Track,
    ReplayGainMode::Album,
    ReplayGainMode::Auto,
];

fn replay_gain_label(mode: ReplayGainMode) -> String {
    match mode {
        ReplayGainMode::Off => aulos_core::fl!("replay-gain-off"),
        ReplayGainMode::Track => aulos_core::fl!("replay-gain-track"),
        ReplayGainMode::Album => aulos_core::fl!("replay-gain-album"),
        ReplayGainMode::Auto => aulos_core::fl!("replay-gain-auto"),
    }
}

fn auto_play_label(mode: AutoPlayMode) -> String {
    match mode {
        AutoPlayMode::Off => aulos_core::fl!("auto-play-off"),
        AutoPlayMode::Random => aulos_core::fl!("auto-play-random"),
        AutoPlayMode::Similar => aulos_core::fl!("auto-play-similar"),
    }
}

fn sec_playback(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, &aulos_core::fl!("settings-playback"), "");
    ui.add_space(6.0);
    let cfg = &app.session.config.config;
    let (gapless, stop_after, crossfade, fade, replay, auto, party) = (
        cfg.gapless,
        cfg.stop_after_current,
        cfg.crossfade_duration_secs,
        cfg.fade_duration_secs,
        cfg.replay_gain_mode,
        cfg.auto_play_mode,
        cfg.party_mode,
    );
    let max_fade = aulos_core::player::engine::fade::MAX_FADE_SECS;

    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-gapless-title"),
        &aulos_core::fl!("settings-gapless-desc"),
        false,
        gapless,
    ) {
        app.session.set_gapless(on);
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-stop-after-title"),
        &aulos_core::fl!("settings-stop-after-desc"),
        false,
        stop_after,
    ) {
        app.session.set_stop_after_each_track(on);
    }

    let label = if crossfade < 0.05 {
        aulos_core::fl!("settings-off")
    } else {
        aulos_core::fl!("settings-seconds", value = format!("{crossfade:.1}"))
    };
    if let Some(f) = slider_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-crossfade-title"),
        &aulos_core::fl!("settings-crossfade-desc"),
        crossfade / 12.0,
        &label,
    ) {
        let secs = (f * 12.0 * 2.0).round() / 2.0;
        if (secs - crossfade).abs() > f32::EPSILON {
            let effect = app.session.set_crossfade(secs);
            app.run(effect);
            app.session.config.save();
        }
    }

    let label = if fade < 0.05 {
        aulos_core::fl!("settings-off")
    } else {
        aulos_core::fl!("settings-seconds", value = format!("{fade:.1}"))
    };
    if let Some(f) = slider_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-fade-title"),
        &aulos_core::fl!("settings-fade-desc"),
        fade / max_fade,
        &label,
    ) {
        let secs = (f * max_fade * 10.0).round() / 10.0;
        if (secs - fade).abs() > f32::EPSILON {
            app.session.set_fade_duration(secs);
            app.session.config.save();
        }
    }

    let labels: Vec<String> = REPLAY_GAIN.iter().map(|m| replay_gain_label(*m)).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let sel = REPLAY_GAIN.iter().position(|m| *m == replay).unwrap_or(0);
    if let Some(i) = seg_row(
        ui,
        &pal,
        "replaygain",
        "ReplayGain",
        &aulos_core::fl!("settings-replaygain-desc"),
        &labels,
        sel,
    ) {
        let effect = app.session.set_replay_gain_mode(REPLAY_GAIN[i]);
        app.run(effect);
        app.session.config.save();
    }

    let auto_modes = [
        AutoPlayMode::Off,
        AutoPlayMode::Random,
        AutoPlayMode::Similar,
    ];
    let labels: Vec<String> = auto_modes.iter().map(|m| auto_play_label(*m)).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let sel = auto_modes.iter().position(|m| *m == auto).unwrap_or(0);
    if let Some(i) = seg_row(
        ui,
        &pal,
        "autoplay",
        &aulos_core::fl!("settings-autoplay-title"),
        &aulos_core::fl!("settings-autoplay-desc"),
        &labels,
        sel,
    ) {
        app.session.set_auto_play_mode(auto_modes[i]);
        app.session.config.save();
    }

    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-party-title"),
        &aulos_core::fl!("settings-party-desc"),
        false,
        party,
    ) {
        let update = app.session.set_party_mode(on);
        app.run(update);
        app.session.config.save();
    }
    party_genres(app, ui);
}

fn party_genres(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    let picked: Vec<String> = app.session.config.config.party_genres.clone();
    let mut toggle_genre: Option<String> = None;
    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for genre in &picked {
            let w = text_width(ui, genre, theme::regular(12.5)) + 10.0 + 6.0 + 4.0 + 11.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
            let bg = if resp.hovered() {
                pal.surface_hi
            } else {
                pal.surface_sel
            };
            ui.painter().rect_filled(rect, CornerRadius::same(13), bg);
            let label = Rect::from_min_max(
                pos2(rect.left() + 10.0, rect.top()),
                pos2(rect.right() - 25.0, rect.bottom()),
            );
            paint_text(
                ui.painter(),
                label,
                Align::Min,
                genre,
                theme::regular(12.5),
                pal.text,
            );
            paint_icon(
                ui.painter(),
                pos2(rect.right() - 12.0, rect.center().y),
                ph::X,
                11.0,
                pal.text_3,
                false,
            );
            if resp.clicked() {
                toggle_genre = Some(genre.clone());
            }
        }
        let add_label = aulos_core::fl!("genre-label");
        let (rect, resp) = ui.allocate_exact_size(
            vec2(
                text_width(ui, &add_label, theme::regular(12.5)) + 36.0,
                26.0,
            ),
            Sense::click(),
        );
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(13),
            Stroke::new(
                1.0,
                if resp.hovered() {
                    pal.text_3
                } else {
                    pal.stroke_strong
                },
            ),
            StrokeKind::Inside,
        );
        paint_icon(
            ui.painter(),
            pos2(rect.left() + 15.0, rect.center().y),
            ph::PLUS,
            11.0,
            pal.text_2,
            false,
        );
        let label = Rect::from_min_max(
            pos2(rect.left() + 26.0, rect.top()),
            pos2(rect.right() - 6.0, rect.bottom()),
        );
        paint_text(
            ui.painter(),
            label,
            Align::Min,
            &add_label,
            theme::regular(12.5),
            pal.text_2,
        );
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(200.0);
            let genres: Vec<&String> = app
                .session
                .library
                .genres
                .iter()
                .filter(|g| !picked.contains(g))
                .take(MAX_GENRE_SUGGESTIONS)
                .collect();
            if genres.is_empty() {
                small_text(
                    ui,
                    &aulos_core::fl!("settings-party-no-more-genres"),
                    pal.text_3,
                );
            }
            egui::ScrollArea::vertical()
                .max_height(260.0)
                .show(ui, |ui| {
                    for g in genres {
                        let (r, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                        if resp.hovered() {
                            ui.painter()
                                .rect_filled(r, CornerRadius::same(5), pal.hover);
                        }
                        let rr = Rect::from_min_max(
                            pos2(r.left() + 8.0, r.top()),
                            pos2(r.right() - 8.0, r.bottom()),
                        );
                        paint_text(
                            ui.painter(),
                            rr,
                            Align::Min,
                            g,
                            theme::regular(13.0),
                            pal.text_1,
                        );
                        if resp.clicked() {
                            toggle_genre = Some(g.clone());
                        }
                    }
                });
        });
    });
    if let Some(g) = toggle_genre {
        app.session.toggle_party_genre(g);
        app.session.config.save();
    }
}

// ----------------------------------------------------------------- equalizer

const EQ_FREQS: [&str; 10] = [
    "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
];

fn sec_equalizer(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    let accent = app.accent;
    heading(
        ui,
        &pal,
        &aulos_core::fl!("equalizer"),
        &aulos_core::fl!("settings-eq-note"),
    );
    ui.add_space(6.0);
    let cfg = &app.session.config.config;
    let (enabled, preamp) = (cfg.equalizer_enabled, cfg.equalizer_preamp);
    let bands: Vec<f32> = cfg.equalizer_bands.clone();

    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("equalizer"),
        &aulos_core::fl!("settings-eq-enabled-desc"),
        false,
        enabled,
    ) {
        app.session.eq_set_enabled(on);
        app.session.config.save();
    }

    let names: Vec<String> = app
        .session
        .equalizer
        .all_presets
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let active = app.session.equalizer.active_preset_name.clone();
    let dirty = app.session.equalizer.dirty;
    let current = match (&active, dirty) {
        (Some(n), false) => n.clone(),
        (Some(n), true) => aulos_core::fl!("settings-eq-modified", name = n.as_str()),
        (None, _) => aulos_core::fl!("settings-eq-custom"),
    };
    if let Some(i) = select_row(
        ui,
        &pal,
        &aulos_core::fl!("equalizer-section-preset"),
        &aulos_core::fl!("settings-eq-preset-desc"),
        &current,
        &names,
    ) {
        app.session.eq_select_preset(names[i].clone());
        app.session.config.save();
    }

    let label = aulos_core::fl!("equalizer-preamp-value", db = format!("{preamp:+.1}"));
    if let Some(f) = slider_row(
        ui,
        &pal,
        &aulos_core::fl!("equalizer-section-preamp"),
        &aulos_core::fl!("settings-eq-preamp-desc"),
        (preamp + 20.0) / 30.0,
        &label,
    ) {
        let db = ((f * 30.0 - 20.0) * 2.0).round() / 2.0;
        if (db - preamp).abs() > f32::EPSILON {
            app.session.eq_set_preamp(db);
            app.session.config.save();
        }
    }

    // The ten band sliders.
    ui.add_space(14.0);
    let w = ui.available_width();
    let col_w = w / 10.0;
    let height = 150.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
    let mut change = None;
    for (i, label) in EQ_FREQS.iter().enumerate() {
        let gain = bands.get(i).copied().unwrap_or(0.0);
        let col = Rect::from_min_size(rect.min + vec2(i as f32 * col_w, 0.0), vec2(col_w, height));
        let resp = ui.interact(
            col,
            egui::Id::new(("settings-eq-band", i)),
            Sense::click_and_drag(),
        );
        let track = Rect::from_min_max(
            pos2(col.center().x - 2.0, col.top() + 20.0),
            pos2(col.center().x + 2.0, col.bottom() - 20.0),
        );
        let frac = (12.0 - gain) / 24.0;
        let y = track.top() + track.height() * frac;
        ui.painter()
            .rect_filled(track, CornerRadius::same(2), pal.track);
        let mid = track.center().y;
        let fill = Rect::from_min_max(
            pos2(track.left(), y.min(mid)),
            pos2(track.right(), y.max(mid)),
        );
        ui.painter().rect_filled(
            fill,
            CornerRadius::same(2),
            if enabled { accent.fill } else { pal.text_3 },
        );
        ui.painter().hline(
            (track.left() - 4.0)..=(track.right() + 4.0),
            mid,
            Stroke::new(1.0, pal.stroke_strong),
        );
        ui.painter()
            .circle_filled(pos2(col.center().x, y), 6.0, pal.text);
        paint_text(
            ui.painter(),
            Rect::from_min_max(col.min, pos2(col.right(), col.top() + 16.0)),
            Align::Center,
            &format!("{gain:+.0}"),
            theme::mono(10.5),
            pal.text_2,
        );
        paint_text(
            ui.painter(),
            Rect::from_min_max(pos2(col.left(), col.bottom() - 16.0), col.max),
            Align::Center,
            label,
            theme::mono(10.5),
            pal.text_3,
        );
        if resp.double_clicked() {
            change = Some((i, 0.0));
        } else if (resp.dragged() || resp.clicked())
            && let Some(p) = resp.interact_pointer_pos()
        {
            let f = ((p.y - track.top()) / track.height()).clamp(0.0, 1.0);
            change = Some((i, ((12.0 - f * 24.0) * 2.0).round() / 2.0));
        }
    }
    if let Some((i, db)) = change {
        let current = bands.get(i).copied().unwrap_or(0.0);
        if (db - current).abs() > f32::EPSILON {
            app.session.eq_set_band(i, db);
            app.session.config.save();
        }
    }

    ui.add_space(12.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
        if pill_button(
            ui,
            &aulos_core::fl!("settings-eq-reset-flat"),
            Some(ph::ARROWS_CLOCKWISE),
            PillKind::Secondary,
            28.0,
            0.0,
        )
        .clicked()
        {
            app.session.eq_reset();
            app.session.config.save();
        }
        let custom_active = active
            .as_deref()
            .is_some_and(|n| !app.session.equalizer.preset_manager.is_builtin_name(n));
        if custom_active {
            if pill_button_enabled(
                ui,
                &aulos_core::fl!("settings-eq-save-preset"),
                None,
                PillKind::Secondary,
                28.0,
                0.0,
                dirty,
            )
            .clicked()
            {
                app.session.eq_save_preset();
            }
            if pill_button(
                ui,
                &aulos_core::fl!("settings-eq-delete-preset"),
                Some(ph::TRASH),
                PillKind::Danger,
                28.0,
                0.0,
            )
            .clicked()
            {
                app.session.eq_delete_preset();
                app.session.config.save();
            }
        }
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let st = &mut app.views.settings;
        input(
            ui,
            &pal,
            &mut st.eq_name,
            &aulos_core::fl!("settings-eq-name-hint"),
            220.0,
            false,
        );
        let name = st.eq_name.trim().to_owned();
        if pill_button_enabled(
            ui,
            &aulos_core::fl!("settings-eq-save-as"),
            None,
            PillKind::Secondary,
            28.0,
            0.0,
            !name.is_empty(),
        )
        .clicked()
        {
            st.eq_name.clear();
            app.session.eq_save_preset_as(name);
            app.session.config.save();
        }
    });

    autoeq_picker(app, ui);
}

fn autoeq_picker(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    ui.add_space(14.0);
    ui.label(
        egui::RichText::new(aulos_core::fl!("settings-autoeq-title"))
            .font(theme::semibold(13.0))
            .color(pal.text),
    );
    small_text(ui, &aulos_core::fl!("settings-autoeq-desc"), pal.text_2);
    ui.add_space(8.0);
    let loading = app.session.equalizer.autoeq_loading;
    let loaded = !app.session.equalizer.autoeq_profiles.is_empty();
    if !loaded {
        let label = if loading {
            aulos_core::fl!("settings-autoeq-loading-list")
        } else {
            aulos_core::fl!("settings-autoeq-load-list")
        };
        if pill_button_enabled(
            ui,
            &label,
            Some(ph::DOWNLOAD_SIMPLE),
            PillKind::Secondary,
            28.0,
            0.0,
            !loading,
        )
        .clicked()
        {
            let effect = app.session.fetch_autoeq_index();
            app.run(effect);
        }
        return;
    }
    let st = &mut app.views.settings;
    input(
        ui,
        &pal,
        &mut st.eq_search,
        &aulos_core::fl!("settings-autoeq-search-hint"),
        260.0,
        false,
    );
    let query = st.eq_search.clone();
    let mut pick = None;
    if let Some(results) = autoeq::search_profiles(&app.session.equalizer.autoeq_profiles, &query) {
        ui.add_space(6.0);
        if results.is_empty() {
            small_text(ui, &aulos_core::fl!("settings-autoeq-no-match"), pal.text_3);
        }
        for p in results.iter().take(8) {
            let (r, resp) =
                ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
            if resp.hovered() {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(6), pal.hover);
            }
            let name = Rect::from_min_max(
                pos2(r.left() + 8.0, r.top()),
                pos2(r.right() - 160.0, r.bottom()),
            );
            let source = Rect::from_min_max(
                pos2(r.right() - 150.0, r.top()),
                pos2(r.right() - 8.0, r.bottom()),
            );
            paint_text(
                ui.painter(),
                name,
                Align::Min,
                &p.name,
                theme::regular(13.0),
                pal.text_1,
            );
            paint_text(
                ui.painter(),
                source,
                Align::Max,
                &p.source,
                theme::regular(12.0),
                pal.text_3,
            );
            if resp.clicked() {
                pick = Some(p.path.clone());
            }
        }
        if results.len() > 8 {
            small_text(
                ui,
                &aulos_core::fl!("settings-autoeq-keep-typing"),
                pal.text_3,
            );
        }
    }
    if let Some(path) = pick {
        let effect = app.session.fetch_autoeq_profile(path);
        app.run(effect);
        app.session.config.save();
        app.toast(aulos_core::fl!("settings-autoeq-applying"));
    }
}

// ---------------------------------------------------------------- scrobbling

fn service_style(service: Service) -> (&'static str, Color32) {
    match service {
        Service::ListenBrainz => ("LB", theme::oklch(0.5, 0.12, 300.0)),
        Service::LastFm => ("fm", theme::oklch(0.5, 0.15, 25.0)),
        Service::LibreFm => ("Li", theme::oklch(0.5, 0.10, 150.0)),
    }
}

fn sec_scrobbling(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(
        ui,
        &pal,
        &aulos_core::fl!("scrobbling"),
        &aulos_core::fl!("settings-scrobbling-note"),
    );
    ui.add_space(10.0);
    let mut msgs: Vec<ScrobbleMessage> = Vec::new();
    for service in Service::ALL {
        service_card(app, ui, service, &mut msgs);
        ui.add_space(10.0);
    }
    import_progress_card(app, ui);

    let cfg = &app.session.config.config;
    let (streams, love, on_connect, lastfm) = (
        cfg.scrobble_streams,
        cfg.scrobble_lastfm_love_sync,
        cfg.scrobble_import_on_connect,
        is_connected(cfg, Service::LastFm),
    );
    if let Some(v) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-scrobble-streams-title"),
        &aulos_core::fl!("settings-scrobble-streams-desc"),
        false,
        streams,
    ) {
        msgs.push(ScrobbleMessage::SetStreams(v));
    }
    if lastfm {
        if let Some(v) = toggle_row(
            ui,
            &pal,
            &aulos_core::fl!("settings-love-title"),
            &aulos_core::fl!("settings-love-desc"),
            true,
            love,
        ) {
            msgs.push(ScrobbleMessage::SetLoveSync(v));
        }
        let busy = app.session.playback.scrobble.is_busy(Service::LastFm);
        row(
            ui,
            &pal,
            &aulos_core::fl!("settings-sync-loved-title"),
            &aulos_core::fl!("settings-sync-loved-desc"),
            true,
            120.0,
            |ui| {
                if pill_button_enabled(
                    ui,
                    &aulos_core::fl!("settings-sync-now"),
                    Some(ph::ARROWS_CLOCKWISE),
                    PillKind::Secondary,
                    28.0,
                    0.0,
                    !busy,
                )
                .clicked()
                {
                    msgs.push(ScrobbleMessage::SyncLoved);
                }
            },
        );
    }
    if let Some(v) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-import-on-connect-title"),
        &aulos_core::fl!("settings-import-on-connect-desc"),
        true,
        on_connect,
    ) {
        msgs.push(ScrobbleMessage::SetImportOnConnect(v));
    }
    for m in msgs {
        scrobble_update(app, m);
    }
}

fn service_card(app: &mut App, ui: &mut Ui, service: Service, msgs: &mut Vec<ScrobbleMessage>) {
    let pal = app.pal;
    let cfg = &app.session.config.config;
    let connected = is_connected(cfg, service);
    let enabled = enabled_of(cfg, service);
    let user = user_of(cfg, service).to_owned();
    let sc = &app.session.playback.scrobble;
    let busy = sc.is_busy(service);
    let pending = sc.is_pending(service);
    let importing = sc.import_progress(service).is_some();
    let any_import = sc.is_importing();
    let queued = sc.queued(service);
    let status_msg = sc.status(service).map(str::to_owned);
    let failed = sc.error(service).is_some()
        || status_msg
            .as_deref()
            .is_some_and(|s| s.starts_with("Error"));
    let (status, status_color) = match (&status_msg, connected) {
        (Some(s), _) => (s.clone(), if failed { pal.danger } else { pal.text_2 }),
        (None, true) => {
            let s = if !enabled {
                aulos_core::fl!("settings-svc-connected-paused", user = user.as_str())
            } else if queued > 0 {
                aulos_core::fl!(
                    "settings-svc-connected-queued",
                    user = user.as_str(),
                    count = queued
                )
            } else {
                aulos_core::fl!("settings-svc-connected", user = user.as_str())
            };
            (s, pal.text_2)
        }
        (None, false) => (aulos_core::fl!("settings-svc-not-connected"), pal.text_3),
    };

    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
    card_bg(ui, &pal, rect, pal.stroke);
    let (initials, color) = service_style(service);
    let tile = Rect::from_min_size(rect.min + vec2(12.0, 11.0), vec2(30.0, 30.0));
    ui.painter().rect_filled(tile, CornerRadius::same(7), color);
    ui.painter().text(
        tile.center(),
        Align2::CENTER_CENTER,
        initials,
        theme::mono_semibold(11.0),
        Color32::WHITE,
    );

    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(12.0, 0.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 8.0;
    if connected {
        if pill_button(
            &mut child,
            &aulos_core::fl!("scrobble-disconnect"),
            None,
            PillKind::Secondary,
            28.0,
            0.0,
        )
        .clicked()
        {
            msgs.push(ScrobbleMessage::Disconnect(service));
        }
        if importing {
            if pill_button(
                &mut child,
                &aulos_core::fl!("scrobble-import-cancel"),
                None,
                PillKind::Ghost,
                28.0,
                0.0,
            )
            .clicked()
            {
                msgs.push(ScrobbleMessage::ImportCancel(service));
            }
        } else if pill_button_enabled(
            &mut child,
            &aulos_core::fl!("settings-import-history"),
            Some(ph::DOWNLOAD_SIMPLE),
            PillKind::Ghost,
            28.0,
            0.0,
            !any_import,
        )
        .clicked()
        {
            msgs.push(ScrobbleMessage::ImportStart(service));
        }
        let mut on = enabled;
        if toggle(&mut child, &mut on).changed() {
            msgs.push(ScrobbleMessage::SetEnabled(service, on));
        }
    } else if service != Service::ListenBrainz {
        if pending {
            if pill_button_enabled(
                &mut child,
                &aulos_core::fl!("settings-complete-connection"),
                None,
                PillKind::Primary,
                28.0,
                0.0,
                !busy,
            )
            .clicked()
            {
                msgs.push(ScrobbleMessage::CompleteAuth(service));
            }
        } else if pill_button_enabled(
            &mut child,
            &aulos_core::fl!("scrobble-connect"),
            None,
            PillKind::Primary,
            28.0,
            0.0,
            !busy,
        )
        .clicked()
        {
            msgs.push(ScrobbleMessage::StartAuth(service));
        }
    }
    let text_right = child.min_rect().left() - 12.0;
    let tx = tile.right() + 12.0;
    let l1 = Rect::from_min_max(
        pos2(tx, rect.top() + 9.0),
        pos2(text_right, rect.top() + 27.0),
    );
    let l2 = Rect::from_min_max(
        pos2(tx, rect.top() + 27.0),
        pos2(text_right, rect.top() + 43.0),
    );
    paint_text(
        ui.painter(),
        l1,
        Align::Min,
        service.display_name(),
        theme::semibold(13.0),
        pal.text,
    );
    paint_text(
        ui.painter(),
        l2,
        Align::Min,
        &status,
        theme::regular(12.0),
        status_color,
    );

    if connected {
        return;
    }
    match service {
        Service::ListenBrainz => {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                input(
                    ui,
                    &pal,
                    &mut app.session.playback.scrobble.lb_token_input,
                    &aulos_core::fl!("settings-lb-token-hint"),
                    320.0,
                    true,
                );
                let token_set = !app
                    .session
                    .playback
                    .scrobble
                    .lb_token_input
                    .trim()
                    .is_empty();
                if pill_button_enabled(
                    ui,
                    &if busy {
                        aulos_core::fl!("settings-checking")
                    } else {
                        aulos_core::fl!("scrobble-connect")
                    },
                    None,
                    PillKind::Primary,
                    30.0,
                    0.0,
                    token_set && !busy,
                )
                .clicked()
                {
                    msgs.push(ScrobbleMessage::LbConnect);
                }
            });
            ui.add_space(2.0);
            small_text(ui, &aulos_core::fl!("settings-lb-token-help"), pal.text_3);
        }
        Service::LastFm => {
            ui.add_space(8.0);
            small_text(
                ui,
                &aulos_core::fl!("settings-lastfm-keys-note"),
                pal.text_3,
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                input(
                    ui,
                    &pal,
                    &mut app.session.playback.scrobble.lastfm_key_input,
                    &aulos_core::fl!("settings-api-key"),
                    240.0,
                    false,
                );
                input(
                    ui,
                    &pal,
                    &mut app.session.playback.scrobble.lastfm_secret_input,
                    &aulos_core::fl!("settings-shared-secret"),
                    240.0,
                    true,
                );
                if pill_button(
                    ui,
                    &aulos_core::fl!("settings-save-keys"),
                    None,
                    PillKind::Secondary,
                    30.0,
                    0.0,
                )
                .clicked()
                {
                    msgs.push(ScrobbleMessage::SaveLastFmKeys);
                }
            });
        }
        Service::LibreFm => {}
    }
}

fn import_progress_card(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    for service in Service::ALL {
        let Some(p) = app.session.playback.scrobble.import_progress(service) else {
            continue;
        };
        let (fetched, total, matched, imported) = (p.fetched, p.total, p.matched, p.imported);
        let counts = match total {
            Some(t) => format!("{fetched} / {t}"),
            None => aulos_core::fl!("settings-import-read", fetched = fetched),
        };
        let frac = total
            .filter(|t| *t > 0)
            .map_or(0.0, |t| (fetched as f32 / t as f32).clamp(0.0, 1.0));
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 74.0), Sense::hover());
        dashed_card(ui, rect, pal.stroke_strong);
        paint_icon(
            ui.painter(),
            pos2(rect.left() + 20.0, rect.top() + 21.0),
            ph::DOWNLOAD_SIMPLE,
            16.0,
            pal.text_2,
            false,
        );
        let title = Rect::from_min_max(
            pos2(rect.left() + 36.0, rect.top() + 10.0),
            pos2(rect.right() - 170.0, rect.top() + 32.0),
        );
        paint_text(
            ui.painter(),
            title,
            Align::Min,
            &aulos_core::fl!(
                "settings-import-progress-title",
                service = service.display_name()
            ),
            theme::semibold(13.0),
            pal.text,
        );
        let count = Rect::from_min_max(
            pos2(rect.right() - 160.0, rect.top() + 10.0),
            pos2(rect.right() - 12.0, rect.top() + 32.0),
        );
        paint_text(
            ui.painter(),
            count,
            Align::Max,
            &counts,
            theme::mono(11.5),
            pal.text_2,
        );
        let bar = Rect::from_min_size(
            pos2(rect.left() + 12.0, rect.top() + 39.0),
            vec2(rect.width() - 24.0, 3.0),
        );
        ui.painter()
            .rect_filled(bar, CornerRadius::same(2), pal.track);
        ui.painter().rect_filled(
            Rect::from_min_size(bar.min, vec2(bar.width() * frac, 3.0)),
            CornerRadius::same(2),
            pal.reed,
        );
        let note = Rect::from_min_max(
            pos2(rect.left() + 12.0, rect.top() + 47.0),
            pos2(rect.right() - 12.0, rect.top() + 65.0),
        );
        paint_text(
            ui.painter(),
            note,
            Align::Min,
            &aulos_core::fl!(
                "settings-import-summary",
                matched = matched,
                imported = imported
            ),
            theme::regular(12.0),
            pal.text_2,
        );
        ui.add_space(10.0);
    }
}

// -------------------------------------------------------------------- others

fn sec_online(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(
        ui,
        &pal,
        &aulos_core::fl!("settings-sec-online"),
        &aulos_core::fl!("settings-online-note"),
    );
    ui.add_space(4.0);
    let cfg = &app.session.config.config;
    let (bio, photos, radio_icons, suggestions) = (
        cfg.fetch_artist_bio,
        cfg.fetch_artist_photos,
        cfg.fetch_radio_icons,
        cfg.home_online_suggestions,
    );
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-bio-title"),
        &aulos_core::fl!("settings-bio-desc"),
        true,
        bio,
    ) {
        let update = app.session.set_artist_info_fetching(on, photos);
        app.run(update);
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-photos-title"),
        &aulos_core::fl!("settings-photos-desc"),
        true,
        photos,
    ) {
        let update = app.session.set_artist_info_fetching(bio, on);
        app.run(update);
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-radio-icons-title"),
        &aulos_core::fl!("settings-radio-icons-desc"),
        true,
        radio_icons,
    ) {
        let effect = app.session.set_fetch_radio_icons(on);
        app.run(SessionUpdate::from(effect));
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-because-title"),
        &aulos_core::fl!("settings-because-desc"),
        true,
        suggestions,
    ) {
        scrobble_update(app, ScrobbleMessage::SetOnlineSuggestions(on));
    }
}

fn sec_tagger(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, &aulos_core::fl!("settings-sec-tagger"), "");
    ui.add_space(4.0);
    let mut key = app.session.config.config.acoustid_api_key.clone();
    let mut key_changed = false;
    row(
        ui,
        &pal,
        &aulos_core::fl!("settings-acoustid-app-title"),
        &aulos_core::fl!("settings-acoustid-app-desc"),
        true,
        260.0,
        |ui| {
            key_changed = input(
                ui,
                &pal,
                &mut key,
                &aulos_core::fl!("settings-acoustid-app-hint"),
                240.0,
                false,
            )
            .changed();
        },
    );
    if key_changed {
        app.session.config.config.acoustid_api_key = key;
        app.session.config.save();
    }

    let stored = app.session.config.acoustid_key_stored;
    let desc = match stored {
        Some(true) => aulos_core::fl!("settings-acoustid-user-desc-stored"),
        Some(false) => aulos_core::fl!("settings-acoustid-user-desc-optional"),
        None => aulos_core::fl!("settings-acoustid-user-desc-checking"),
    };
    let mut save = false;
    row(
        ui,
        &pal,
        &aulos_core::fl!("settings-acoustid-user-title"),
        &desc,
        true,
        330.0,
        |ui| {
            save = pill_button(
                ui,
                &aulos_core::fl!("save"),
                None,
                PillKind::Secondary,
                28.0,
                0.0,
            )
            .clicked();
            input(
                ui,
                &pal,
                &mut app.views.settings.acoustid_input,
                &if stored == Some(true) {
                    "••••••••".to_owned()
                } else {
                    aulos_core::fl!("settings-acoustid-user-hint")
                },
                240.0,
                true,
            );
        },
    );
    if save {
        let key = app.views.settings.acoustid_input.trim().to_owned();
        app.views.settings.acoustid_input.clear();
        let effect = app.session.config.save_acoustid_user_key(key);
        app.run(effect);
    }

    let caa = app.session.config.config.tagger_use_caa_cover;
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-caa-title"),
        &aulos_core::fl!("settings-caa-desc"),
        true,
        caa,
    ) {
        app.session.config.config.tagger_use_caa_cover = on;
        app.session.config.save();
    }
}

fn sec_appearance(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, &aulos_core::fl!("settings-appearance"), "");
    ui.add_space(4.0);
    let tags = aulos_core::i18n::available_languages();
    let mut names = vec![aulos_core::fl!("language-system")];
    names.extend(tags.iter().map(|t| aulos_core::i18n::native_name(t)));
    let current = &app.session.config.config.language;
    let at = tags.iter().position(|t| t == current).map_or(0, |i| i + 1);
    if let Some(i) = select_row(
        ui,
        &pal,
        &aulos_core::fl!("language"),
        &aulos_core::fl!("language-desc"),
        &names[at],
        &names,
    ) {
        let tag = if i == 0 { "" } else { tags[i - 1].as_str() };
        app.set_language(tag);
    }
    let mode = app.session.config.config.theme_mode;
    let labels = [
        aulos_core::fl!("settings-theme-system"),
        aulos_core::fl!("settings-theme-dark"),
        aulos_core::fl!("settings-theme-light"),
    ];
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let sel = ThemeMode::ALL.iter().position(|m| *m == mode).unwrap_or(0);
    if let Some(i) = seg_row(
        ui,
        &pal,
        "theme",
        &aulos_core::fl!("settings-theme-title"),
        &aulos_core::fl!("settings-theme-desc"),
        &labels,
        sel,
    ) {
        app.set_theme_mode(ThemeMode::ALL[i]);
    }
    let cfg = &app.session.config.config;
    let (tint, reduce) = (cfg.tint_ui_with_cover, cfg.reduce_motion);
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-tint-title"),
        &aulos_core::fl!("settings-tint-desc"),
        false,
        tint,
    ) {
        app.set_tint_ui_with_cover(on);
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-reduce-motion-title"),
        &aulos_core::fl!("settings-reduce-motion-desc"),
        false,
        reduce,
    ) {
        app.session.config.config.reduce_motion = on;
        app.session.config.save();
    }
    let zoom = app.session.config.config.ui_scale;
    let sel = crate::app::UI_SCALE_STEPS
        .iter()
        .position(|s| (s - zoom).abs() < 0.01)
        .unwrap_or(2);
    let labels: Vec<String> = crate::app::UI_SCALE_STEPS
        .iter()
        .map(|s| format!("{:.0}%", s * 100.0))
        .collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    if let Some(i) = seg_row(
        ui,
        &pal,
        "ui-scale",
        &aulos_core::fl!("settings-ui-scale-title"),
        &aulos_core::fl!("settings-ui-scale-desc"),
        &labels,
        sel,
    ) {
        app.set_ui_scale(crate::app::UI_SCALE_STEPS[i]);
    }
    let scale = app.session.config.config.grid_scale;
    let label = format!("{:.0}%", scale * 100.0);
    if let Some(f) = slider_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-grid-scale-title"),
        &aulos_core::fl!("settings-grid-scale-desc"),
        (scale - 0.7) / 0.9,
        &label,
    ) {
        let value = (((0.7 + f * 0.9) * 20.0).round() / 20.0).clamp(0.7, 1.6);
        if (value - scale).abs() > f32::EPSILON {
            app.session.config.config.grid_scale = value;
            app.session.config.save();
        }
    }
}

/// The values `startup_page` is stored as, in the order the dropdown lists them.
const STARTUP_PAGES: [&str; 10] = [
    "home",
    "last",
    "albums",
    "artists",
    "songs",
    "genres",
    "folders",
    "playlists",
    "podcasts",
    "radio",
];

fn startup_page_label(key: &str) -> String {
    match key {
        "last" => aulos_core::fl!("settings-startup-last-visited"),
        "albums" => aulos_core::fl!("albums"),
        "artists" => aulos_core::fl!("artists"),
        "songs" => aulos_core::fl!("songs"),
        "genres" => aulos_core::fl!("genres"),
        "folders" => aulos_core::fl!("folders"),
        "playlists" => aulos_core::fl!("playlists"),
        "podcasts" => aulos_core::fl!("podcasts"),
        "radio" => aulos_core::fl!("radio"),
        _ => aulos_core::fl!("home"),
    }
}

fn sec_startup(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, &aulos_core::fl!("settings-sec-startup"), "");
    ui.add_space(4.0);
    let mut name = app.session.config.config.user_name.clone();
    let mut edited = false;
    row(
        ui,
        &pal,
        &aulos_core::fl!("settings-display-name-title"),
        &aulos_core::fl!("settings-display-name-desc"),
        false,
        250.0,
        |ui| {
            edited = input(
                ui,
                &pal,
                &mut name,
                &aulos_core::fl!("settings-display-name-hint"),
                220.0,
                false,
            )
            .changed();
        },
    );
    if edited {
        app.session.set_user_name(&name);
    }
    let cfg = &app.session.config.config;
    let page = cfg.startup_page.trim().to_owned();
    let page_label = startup_page_label(&page);
    let options: Vec<String> = STARTUP_PAGES
        .iter()
        .map(|k| startup_page_label(k))
        .collect();
    if let Some(i) = select_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-startup-page-title"),
        &aulos_core::fl!("settings-startup-page-desc"),
        &page_label,
        &options,
    ) {
        app.session.config.config.startup_page = STARTUP_PAGES[i].to_owned();
        app.session.config.save();
    }

    let provider = app.session.config.config.startup_provider.trim().to_owned();
    let last_source = aulos_core::fl!("settings-startup-last-source");
    let mut ids: Vec<(String, String)> = vec![("last".to_owned(), last_source.clone())];
    ids.extend(app.session.providers.provider_list.iter().cloned());
    let label = ids
        .iter()
        .find(|(id, _)| *id == provider || (provider.is_empty() && id == "last"))
        .map_or_else(|| last_source.clone(), |(_, n)| n.clone());
    let names: Vec<String> = ids.iter().map(|(_, n)| n.clone()).collect();
    if let Some(i) = select_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-startup-source-title"),
        &aulos_core::fl!("settings-startup-source-desc"),
        &label,
        &names,
    ) {
        app.session.config.config.startup_provider = ids[i].0.clone();
        app.session.config.save();
    }

    let auto_play = app.session.config.config.auto_play_on_launch;
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-autoplay-launch-title"),
        &aulos_core::fl!("settings-autoplay-launch-desc"),
        false,
        auto_play,
    ) {
        app.session.config.config.auto_play_on_launch = on;
        app.session.config.save();
    }
}

fn sec_desktop(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(
        ui,
        &pal,
        &aulos_core::fl!("settings-sec-desktop"),
        &aulos_core::fl!("settings-desktop-note"),
    );
    ui.add_space(4.0);
    let cfg = &app.session.config.config;
    let (background, notify, inhibit) = (
        cfg.background_playback,
        cfg.notify_track_change,
        cfg.inhibit_while_playing,
    );
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-background-title"),
        &aulos_core::fl!("settings-background-desc"),
        false,
        background,
    ) {
        app.session.config.config.background_playback = on;
        app.session.config.save();
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-notify-title"),
        &aulos_core::fl!("settings-notify-desc"),
        false,
        notify,
    ) {
        app.session.config.config.notify_track_change = on;
        app.session.config.save();
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-inhibit-title"),
        &aulos_core::fl!("settings-inhibit-desc"),
        false,
        inhibit,
    ) {
        let effect = app.session.set_inhibit_while_playing(on);
        app.run(effect);
        app.session.config.save();
    }
}

fn sec_experimental(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(
        ui,
        &pal,
        &aulos_core::fl!("settings-experimental"),
        &aulos_core::fl!("settings-experimental-note"),
    );
    ui.add_space(4.0);
    let on = app.session.config.config.experimental_converter;
    if let Some(v) = toggle_row(
        ui,
        &pal,
        &aulos_core::fl!("settings-converter-title"),
        &aulos_core::fl!("settings-converter-desc"),
        false,
        on,
    ) {
        app.session.config.config.experimental_converter = v;
        app.session.config.save();
    }
}

fn sec_about(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, &aulos_core::fl!("settings-about"), "");
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Aulos")
            .font(theme::semibold(15.0))
            .color(pal.text),
    );
    small_text(
        ui,
        &aulos_core::fl!("settings-version", version = env!("CARGO_PKG_VERSION")),
        pal.text_2,
    );
    small_text(ui, &aulos_core::fl!("settings-about-blurb"), pal.text_2);
    ui.add_space(10.0);
    let repository = env!("CARGO_PKG_REPOSITORY");
    if !repository.is_empty()
        && pill_button(
            ui,
            &aulos_core::fl!("settings-project-page"),
            Some(ph::LINK),
            PillKind::Secondary,
            28.0,
            0.0,
        )
        .clicked()
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(repository));
    }
}
