//! The settings page: a section list (with search) on the left, the sections
//! themselves in a scroll area on the right. Every control reads and writes
//! the real [`aulos_core::config::Config`] through the session.

use std::path::PathBuf;
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

impl Sec {
    fn label(self) -> &'static str {
        match self {
            Sec::Library => "Library",
            Sec::Sources => "Sources",
            Sec::Playback => "Playback",
            Sec::Equalizer => "Equalizer",
            Sec::Scrobbling => "Scrobbling",
            Sec::Online => "Online data",
            Sec::Tagger => "Tag editor keys",
            Sec::Appearance => "Appearance",
            Sec::Startup => "Startup page",
            Sec::Desktop => "Background playback",
            Sec::Experimental => "Experimental",
            Sec::About => "About",
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
        let lit = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        match self {
            Sec::Library => [
                keywords::library(),
                keywords::artist_tags(),
                keywords::compilations(),
                keywords::playlists(),
                lit(&["Watch for changes", "auto rescan", "file watcher"]),
            ]
            .concat(),
            Sec::Sources => lit(&[
                "Sources",
                "MPD",
                "Subsonic",
                "OpenSubsonic",
                "Navidrome",
                "server",
                "provider",
                "connection",
            ]),
            // `playback_extras()` lists the fade/auto-play/party strings first and the
            // desktop strings (notifications, suspend, background) from index 9 on.
            Sec::Playback => [
                keywords::playback(),
                keywords::playback_extras().into_iter().take(9).collect(),
                lit(&[
                    "Gapless",
                    "Stop after current track",
                    "stop after every track",
                ]),
            ]
            .concat(),
            Sec::Equalizer => lit(&[
                "Equalizer",
                "preamp",
                "preset",
                "AutoEQ",
                "bands",
                "headphone",
            ]),
            Sec::Scrobbling => lit(&[
                "Scrobbling",
                "Last.fm",
                "ListenBrainz",
                "Libre.fm",
                "scrobble",
                "listening history",
                "import",
                "love",
            ]),
            Sec::Online => [
                keywords::artist_info(),
                lit(&[
                    "Artist biographies",
                    "Artist photos",
                    "Radio station icons",
                    "Online data",
                    "network",
                    "suggestions",
                    "Because you listened to",
                ]),
            ]
            .concat(),
            Sec::Tagger => [keywords::tagger(), lit(&["AcoustID", "Cover Art Archive"])].concat(),
            Sec::Appearance => [
                keywords::appearance(),
                lit(&[
                    "Theme",
                    "Dark",
                    "Light",
                    "System",
                    "Tint",
                    "cover colour",
                    "Reduce motion",
                    "animations",
                ]),
            ]
            .concat(),
            Sec::Startup => [
                keywords::startup(),
                lit(&[
                    "Start playing on launch",
                    "auto-play",
                    "autoplay",
                    "Display name",
                    "greeting",
                    "username",
                ]),
            ]
            .concat(),
            Sec::Desktop => [
                keywords::playback_extras().into_iter().skip(9).collect(),
                lit(&["Desktop", "tray", "window close"]),
            ]
            .concat(),
            Sec::Experimental => keywords::experimental(),
            Sec::About => [keywords::about(), lit(&["version", "license", "GPL"])].concat(),
        }
    }
}

// --------------------------------------------------------------------- state

/// Results of off-thread work the page started.
enum Msg {
    Scrobble(ScrobbleMessage),
    /// The chosen music folder; `None` when the dialog was cancelled.
    Folder(Option<PathBuf>),
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
    st.delimiters_input = app.session.config.config.artist_tag_delimiters.join(" | ");
    pump(app);
    let genres = app.session.load_genres();
    app.run(genres);
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    pump(app);
    app.views.settings.ensure_search();
    let pal = app.pal;
    let pad = app.page_pad();
    let nav_w = if app.narrow { 190.0 } else { 220.0 };
    let full = ui.available_rect_before_wrap();
    let top = full.top() + 4.0;

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

// ----------------------------------------------------------------- navigation

fn nav(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
    ui.label(
        egui::RichText::new("Settings")
            .font(theme::semibold(26.0))
            .color(pal.text),
    );
    ui.add_space(10.0);
    search_box(ui, &pal, &mut app.views.settings.query);
    ui.add_space(8.0);

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
                    vec2(ui.available_width(), 30.0),
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
                    sec.label(),
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
            .hint_text(egui::RichText::new("Search settings").color(pal.text_3))
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
                match sec {
                    Sec::Library => sec_library(app, ui),
                    Sec::Sources => sec_sources(app, ui),
                    Sec::Playback => sec_playback(app, ui),
                    Sec::Equalizer => sec_equalizer(app, ui),
                    Sec::Scrobbling => sec_scrobbling(app, ui),
                    Sec::Online => sec_online(app, ui),
                    Sec::Tagger => sec_tagger(app, ui),
                    Sec::Appearance => sec_appearance(app, ui),
                    Sec::Startup => sec_startup(app, ui),
                    Sec::Desktop => sec_desktop(app, ui),
                    Sec::Experimental => sec_experimental(app, ui),
                    Sec::About => sec_about(app, ui),
                }
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
                    "No settings match",
                    "Try another word, like playback or scrobble.",
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
    let text_w = (w - reserve - 16.0).max(80.0);
    let pad = if desc.is_empty() { 10.0 } else { 11.0 };
    let title_g = ui
        .painter()
        .layout_no_wrap(title.to_owned(), theme::semibold(13.0), pal.text);
    let desc_g = (!desc.is_empty()).then(|| {
        ui.painter()
            .layout(desc.to_owned(), theme::regular(12.0), pal.text_2, text_w)
    });
    let text_h = title_g.size().y + desc_g.as_ref().map_or(0.0, |g| g.size().y + 2.0);
    let h = text_h.max(30.0) + pad * 2.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
    let top = rect.top() + (h - text_h) / 2.0;
    let title_w = title_g.size().x;
    ui.painter()
        .galley(pos2(rect.left(), top), title_g.clone(), pal.text);
    if net {
        let bw = text_width(ui, "NETWORK", theme::mono_semibold(9.5)) + 2.8 + 8.0;
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
            "NETWORK",
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
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
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
        Err(message) => app.toast_error(format!("Import from {name} failed: {message}")),
        Ok(out) => {
            if let Some(message) = &out.error {
                app.toast_error(format!("Import from {name} failed: {message}"));
            } else if out.cancelled {
                app.toast(format!("Import from {name} cancelled"));
            } else {
                app.toast(format!(
                    "Imported {} plays from {name}",
                    out.progress.imported
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
    app.session.playback.scrobble.finish_sync(format!(
        "Synced loved tracks: {added} added here, {pushed} sent to Last.fm"
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
        let result = dialogs::pick_folder("Select Music Directory").await;
        if tx.send(Msg::Folder(result)).is_ok() {
            ctx.request_repaint();
        }
    });
}

fn folder_picked(app: &mut App, result: Option<PathBuf>) {
    app.views.settings.picking_folder = false;
    // Dismissing the dialog is not worth a toast.
    if let Some(path) = result {
        add_folder(app, path);
    }
}

fn add_folder(app: &mut App, path: PathBuf) {
    if app.session.add_music_dir(path) {
        rescan(app);
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
                d.is_dir(),
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
}

fn sec_library(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    refresh_folder_cache(app);
    let scanning = app.session.library.scanning;
    heading(
        ui,
        &pal,
        "Library",
        if scanning {
            "Scanning…"
        } else {
            "Music folders are scanned into the local database"
        },
    );
    ui.add_space(10.0);

    let mut act = None;
    let rows: Vec<(String, usize, bool)> = app
        .views
        .settings
        .folders
        .dirs
        .iter()
        .zip(&app.views.settings.folders.info)
        .map(|(d, (n, ok))| (d.display().to_string(), *n, *ok))
        .collect();
    if rows.is_empty() {
        small_text(
            ui,
            "No music folders yet. Add one to build your library.",
            pal.text_2,
        );
        ui.add_space(8.0);
    }
    for (i, (path, tracks, exists)) in rows.iter().enumerate() {
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
            .tip("Remove folder")
            .show(&mut child)
            .clicked()
        {
            act = Some(FolderAct::Remove(i));
        }
        if pill_button_enabled(
            &mut child,
            "Rescan",
            Some(ph::ARROWS_CLOCKWISE),
            PillKind::Secondary,
            28.0,
            0.0,
            !scanning,
        )
        .clicked()
        {
            act = Some(FolderAct::Rescan);
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
        let (meta, color) = if *exists {
            (format!("{tracks} tracks"), pal.text_2)
        } else {
            (
                "Not available · skipped while offline, not removed".to_owned(),
                pal.warn,
            )
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
            "Add folder",
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
        let st = &mut app.views.settings;
        // Leave room for the "Add" pill and the field margins; the field
        // shrinks on narrow pages.
        let field_w = (ui.available_width() - 100.0).clamp(40.0, 240.0);
        input(
            ui,
            &pal,
            &mut st.path_input,
            "…or type a path",
            field_w,
            false,
        );
        let path = PathBuf::from(st.path_input.trim());
        let ok = !st.path_input.trim().is_empty() && path.is_dir();
        if pill_button_enabled(ui, "Add", None, PillKind::Secondary, 28.0, 0.0, ok).clicked() {
            st.path_input.clear();
            add_folder(app, path);
        }
    });

    match act {
        Some(FolderAct::Rescan) => rescan(app),
        Some(FolderAct::Remove(i)) if app.session.remove_music_dir(i) => rescan(app),
        Some(FolderAct::Remove(_)) => {}
        None => {}
    }

    let watch = app.session.config.config.watch_for_changes;
    if let Some(on) = toggle_row(
        ui,
        &pal,
        "Watch for changes",
        "Rescan automatically when files in your music folders change.",
        false,
        watch,
    ) {
        app.set_watch_for_changes(on);
    }

    ui.add_space(SECTION_GAP);
    heading(
        ui,
        &pal,
        "Artist tags",
        "How collaboration credits are grouped",
    );
    ui.add_space(4.0);
    let cfg = &app.session.config.config;
    let split = cfg.split_artist_tags;
    if let Some(v) = toggle_row(
        ui,
        &pal,
        "Split collaboration credits",
        "“A feat. B” and “A; B” appear as separate artists. Your tags are never changed.",
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
        "Credit delimiters",
        "Separators credits are split on (when splitting is on), divided by |.",
        false,
        330.0,
        |ui| {
            if pill_button(ui, "Reset", None, PillKind::Ghost, 28.0, 0.0).clicked() {
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
    heading(ui, &pal, "Compilations and playlists", "");
    ui.add_space(4.0);
    let cfg = &app.session.config.config;
    let (show_comp, relative) = (cfg.show_compilations_in_artists, cfg.m3u_relative_paths);
    if let Some(v) = toggle_row(
        ui,
        &pal,
        "Show “Various Artists” in Artists",
        "Compilations stay reachable from Albums either way.",
        false,
        show_comp,
    ) {
        app.session.set_show_compilations_in_artists(v);
    }
    if let Some(v) = toggle_row(
        ui,
        &pal,
        "Relative paths in exported M3U playlists",
        "Write track paths relative to the playlist file instead of absolute.",
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
        "Sources",
        "MPD and Subsonic / OpenSubsonic servers",
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
        small_text(
            ui,
            "No servers yet. Your local library is always available.",
            pal.text_2,
        );
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
            "Add MPD server",
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
            "Add Subsonic server",
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
        (pal.warn, "Testing…".to_owned(), pal.text_1)
    } else if let Some(s) = &card.status {
        // A failed test reads `<failure>: <reason>`; a success is the bare word.
        if s.contains(':') {
            (pal.danger, s.clone(), pal.danger)
        } else {
            (pal.ok, s.clone(), pal.text_1)
        }
    } else if active {
        (pal.ok, "Active source".to_owned(), pal.text_1)
    } else {
        (pal.text_4, "Not tested".to_owned(), pal.text_3)
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
        ("Testing", None)
    } else if failed {
        ("Retry", Some(ph::ARROW_CLOCKWISE))
    } else {
        ("Test", Some(ph::PLUGS_CONNECTED))
    };
    if testing {
        buttons.add(egui::Spinner::new().size(14.0).color(pal.text_2));
    }
    if pill_button_enabled(
        &mut buttons,
        label,
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
    if pill_button(&mut buttons, "Edit", None, PillKind::Ghost, 26.0, 0.0).clicked() {
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
                    egui::RichText::new("MPD server")
                        .font(theme::semibold(14.0))
                        .color(pal.text),
                );
                ui.add_space(10.0);
                labelled_input(ui, &pal, "Name", &mut s.name, "Living room MPD", false);
                labelled_input(ui, &pal, "Host", &mut s.host, "localhost", false);
                labelled_input(ui, &pal, "Port", &mut s.port, "6600", false);
                labelled_input(ui, &pal, "Password (optional)", &mut s.password, "", true);
            }
            Kind::Subsonic => {
                let s = &mut app.session.providers.subsonic_edit_states[at.index];
                ui.label(
                    egui::RichText::new("Subsonic / OpenSubsonic server")
                        .font(theme::semibold(14.0))
                        .color(pal.text),
                );
                ui.add_space(10.0);
                labelled_input(ui, &pal, "Name", &mut s.name, "Navidrome", false);
                labelled_input(
                    ui,
                    &pal,
                    "Server URL",
                    &mut s.url,
                    "https://music.example.com",
                    false,
                );
                labelled_input(ui, &pal, "Username", &mut s.username, "", false);
                labelled_input(ui, &pal, "Password or API key", &mut s.password, "", true);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    toggle(ui, &mut s.accept_invalid_certs);
                    small_text(
                        ui,
                        "Accept invalid TLS certificates (self-signed servers)",
                        pal.text_2,
                    );
                });
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
            small_text(ui, "Testing connection…", pal.warn);
            ui.add_space(8.0);
        } else if let Some(s) = &status {
            let failed = s.contains(':');
            small_text(ui, s, if failed { pal.danger } else { pal.ok });
            ui.add_space(8.0);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            out.save = pill_button(ui, "Save", None, PillKind::Primary, 30.0, 0.0).clicked();
            out.test = pill_button_enabled(
                ui,
                "Test connection",
                Some(ph::PLUGS_CONNECTED),
                PillKind::Secondary,
                30.0,
                0.0,
                !testing,
            )
            .clicked();
            out.close = pill_button(ui, "Cancel", None, PillKind::Ghost, 30.0, 0.0).clicked();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                out.remove =
                    pill_button(ui, "Remove", Some(ph::TRASH), PillKind::Danger, 30.0, 0.0)
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
                Some("Give the server a name and a host.")
            } else if s.port.trim().parse::<u16>().is_err() {
                Some("The port must be a number between 1 and 65535.")
            } else {
                None
            }
        }
        Kind::Subsonic => {
            let s = &app.session.providers.subsonic_edit_states[at.index];
            if s.name.trim().is_empty() || s.url.trim().is_empty() {
                Some("Give the server a name and a URL.")
            } else if !s.url.trim().starts_with("http://") && !s.url.trim().starts_with("https://")
            {
                Some("The URL must start with http:// or https://.")
            } else if s.username.trim().is_empty() {
                Some("Enter the username.")
            } else {
                None
            }
        }
    };
    if let Some(p) = problem {
        app.views.settings.edit_error = Some(p.to_owned());
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
    app.toast("Server saved");
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

const REPLAY_GAIN: [(&str, ReplayGainMode); 4] = [
    ("Off", ReplayGainMode::Off),
    ("Track", ReplayGainMode::Track),
    ("Album", ReplayGainMode::Album),
    ("Auto", ReplayGainMode::Auto),
];

fn sec_playback(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, "Playback", "");
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
        "Gapless playback",
        "Prepare the next track so consecutive tracks follow without a pause.",
        false,
        gapless,
    ) {
        app.session.set_gapless(on);
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        "Stop after current track",
        "Playback stops at the end of every track instead of continuing with the queue.",
        false,
        stop_after,
    ) {
        app.session.set_stop_after_each_track(on);
    }

    let label = if crossfade < 0.05 {
        "Off".to_owned()
    } else {
        format!("{crossfade:.1} s")
    };
    if let Some(f) = slider_row(
        ui,
        &pal,
        "Crossfade",
        "Overlap the end of one track with the next. Album tracks stay gapless.",
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
        "Off".to_owned()
    } else {
        format!("{fade:.1} s")
    };
    if let Some(f) = slider_row(
        ui,
        &pal,
        "Fade on play, pause and stop",
        "A short volume ramp instead of a hard cut.",
        fade / max_fade,
        &label,
    ) {
        let secs = (f * max_fade * 10.0).round() / 10.0;
        if (secs - fade).abs() > f32::EPSILON {
            app.session.set_fade_duration(secs);
            app.session.config.save();
        }
    }

    let labels: Vec<&str> = REPLAY_GAIN.iter().map(|(l, _)| *l).collect();
    let sel = REPLAY_GAIN
        .iter()
        .position(|(_, m)| *m == replay)
        .unwrap_or(0);
    if let Some(i) = seg_row(
        ui,
        &pal,
        "replaygain",
        "ReplayGain",
        "Album mode keeps the dynamics between tracks of one album.",
        &labels,
        sel,
    ) {
        let effect = app.session.set_replay_gain_mode(REPLAY_GAIN[i].1);
        app.run(effect);
        app.session.config.save();
    }

    let auto_modes = [
        ("Off", AutoPlayMode::Off),
        ("Random", AutoPlayMode::Random),
        ("Similar", AutoPlayMode::Similar),
    ];
    let labels: Vec<&str> = auto_modes.iter().map(|(l, _)| *l).collect();
    let sel = auto_modes.iter().position(|(_, m)| *m == auto).unwrap_or(0);
    if let Some(i) = seg_row(
        ui,
        &pal,
        "autoplay",
        "When the queue ends",
        "Keep playing random albums or music similar to what just played.",
        &labels,
        sel,
    ) {
        app.session.set_auto_play_mode(auto_modes[i].1);
        app.session.config.save();
    }

    if let Some(on) = toggle_row(
        ui,
        &pal,
        "Party mode",
        "Endless random queue from the genres below (the whole library when none).",
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
        let (rect, resp) = ui.allocate_exact_size(
            vec2(text_width(ui, "Genre", theme::regular(12.5)) + 36.0, 26.0),
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
            "Genre",
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
                small_text(ui, "No more genres in the library", pal.text_3);
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
        "Equalizer",
        "Ten bands, applied to local playback",
    );
    ui.add_space(6.0);
    let cfg = &app.session.config.config;
    let (enabled, preamp) = (cfg.equalizer_enabled, cfg.equalizer_preamp);
    let bands: Vec<f32> = cfg.equalizer_bands.clone();

    if let Some(on) = toggle_row(
        ui,
        &pal,
        "Equalizer",
        "Bypass it without losing your settings.",
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
        (Some(n), true) => format!("{n} (modified)"),
        (None, _) => "Custom".to_owned(),
    };
    if let Some(i) = select_row(
        ui,
        &pal,
        "Preset",
        "Built-in shapes and your saved ones.",
        &current,
        &names,
    ) {
        app.session.eq_select_preset(names[i].clone());
        app.session.config.save();
    }

    let label = format!("{preamp:+.1} dB");
    if let Some(f) = slider_row(
        ui,
        &pal,
        "Preamp",
        "Lower it when boosted bands start to clip.",
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
            "Reset to flat",
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
                "Save preset",
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
                "Delete preset",
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
            "Save current settings as…",
            220.0,
            false,
        );
        let name = st.eq_name.trim().to_owned();
        if pill_button_enabled(
            ui,
            "Save as",
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
        egui::RichText::new("AutoEQ headphone profiles")
            .font(theme::semibold(13.0))
            .color(pal.text),
    );
    small_text(
        ui,
        "Correction curves for your headphones, downloaded from the AutoEQ project.",
        pal.text_2,
    );
    ui.add_space(8.0);
    let loading = app.session.equalizer.autoeq_loading;
    let loaded = !app.session.equalizer.autoeq_profiles.is_empty();
    if !loaded {
        let label = if loading {
            "Loading list…"
        } else {
            "Load profile list"
        };
        if pill_button_enabled(
            ui,
            label,
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
        "Search headphones (2+ letters)",
        260.0,
        false,
    );
    let query = st.eq_search.clone();
    let mut pick = None;
    if let Some(results) = autoeq::search_profiles(&app.session.equalizer.autoeq_profiles, &query) {
        ui.add_space(6.0);
        if results.is_empty() {
            small_text(ui, "No profile matches", pal.text_3);
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
            small_text(ui, "Keep typing to narrow the list.", pal.text_3);
        }
    }
    if let Some(path) = pick {
        let effect = app.session.fetch_autoeq_profile(path);
        app.run(effect);
        app.session.config.save();
        app.toast("Applying AutoEQ profile");
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
        "Scrobbling",
        "A play counts after 50% or 4 minutes",
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
        "Scrobble radio and podcasts",
        "Streams are off by default.",
        false,
        streams,
    ) {
        msgs.push(ScrobbleMessage::SetStreams(v));
    }
    if lastfm {
        if let Some(v) = toggle_row(
            ui,
            &pal,
            "Love tracks on Last.fm",
            "Favoriting a track loves it there too.",
            true,
            love,
        ) {
            msgs.push(ScrobbleMessage::SetLoveSync(v));
        }
        let busy = app.session.playback.scrobble.is_busy(Service::LastFm);
        row(
            ui,
            &pal,
            "Sync loved tracks",
            "Loved tracks become favorites here, favorites become loved there.",
            true,
            120.0,
            |ui| {
                if pill_button_enabled(
                    ui,
                    "Sync now",
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
        "Import history after connecting",
        "Reads the service's listening history once a service is connected.",
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
            let mut s = format!("Connected as {user}");
            if !enabled {
                s.push_str(" · paused");
            } else if queued > 0 {
                s.push_str(&format!(" · {queued} waiting to send"));
            }
            (s, pal.text_2)
        }
        (None, false) => ("Not connected".to_owned(), pal.text_3),
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
            "Disconnect",
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
                "Cancel import",
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
            "Import history",
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
                "Complete connection",
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
            "Connect",
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
                    "Paste your ListenBrainz user token",
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
                    if busy { "Checking…" } else { "Connect" },
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
            small_text(
                ui,
                "Find the token on listenbrainz.org under Settings → User token.",
                pal.text_3,
            );
        }
        Service::LastFm => {
            ui.add_space(8.0);
            small_text(
                ui,
                "Last.fm needs your own API key and shared secret (last.fm/api/account/create).",
                pal.text_3,
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                input(
                    ui,
                    &pal,
                    &mut app.session.playback.scrobble.lastfm_key_input,
                    "API key",
                    240.0,
                    false,
                );
                input(
                    ui,
                    &pal,
                    &mut app.session.playback.scrobble.lastfm_secret_input,
                    "Shared secret",
                    240.0,
                    true,
                );
                if pill_button(ui, "Save keys", None, PillKind::Secondary, 30.0, 0.0).clicked() {
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
            None => format!("{fetched} read"),
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
            &format!(
                "Importing listening history from {}",
                service.display_name()
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
            &format!(
                "{matched} matched in your library · {imported} added to the history. Matched plays power Home’s “rediscover” shelf."
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
        "Online data",
        "Off by default. Nothing leaves your machine until you turn it on.",
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
        "Artist biographies",
        "Wikipedia text for Local and MPD libraries. Subsonic servers use their own.",
        true,
        bio,
    ) {
        let update = app.session.set_artist_info_fetching(on, photos);
        app.run(update);
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        "Artist photos",
        "Deezer images for Local and MPD libraries. Subsonic servers use their own.",
        true,
        photos,
    ) {
        let update = app.session.set_artist_info_fetching(bio, on);
        app.run(update);
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        "Radio station icons",
        "Downloaded from each station's own server.",
        true,
        radio_icons,
    ) {
        let effect = app.session.set_fetch_radio_icons(on);
        app.run(SessionUpdate::from(effect));
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        "“Because you listened to…” on Home",
        "Similar-artist shelves from your connected scrobbling service.",
        true,
        suggestions,
    ) {
        scrobble_update(app, ScrobbleMessage::SetOnlineSuggestions(on));
    }
}

fn sec_tagger(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, "Tag editor keys", "");
    ui.add_space(4.0);
    let mut key = app.session.config.config.acoustid_api_key.clone();
    let mut key_changed = false;
    row(
        ui,
        &pal,
        "AcoustID application key",
        "Required for fingerprint lookups. Not a secret.",
        true,
        260.0,
        |ui| {
            key_changed = input(ui, &pal, &mut key, "Application key", 240.0, false).changed();
        },
    );
    if key_changed {
        app.session.config.config.acoustid_api_key = key;
        app.session.config.save();
    }

    let stored = app.session.config.acoustid_key_stored;
    let desc = match stored {
        Some(true) => {
            "Your own key is stored in the system keyring. Enter a new one to replace it, or save an empty field to delete it."
        }
        Some(false) => {
            "Optional. Lets you submit fingerprints. Stored in the system keyring, never in the settings file."
        }
        None => "Checking the system keyring…",
    };
    let mut save = false;
    row(ui, &pal, "AcoustID user key", desc, true, 330.0, |ui| {
        save = pill_button(ui, "Save", None, PillKind::Secondary, 28.0, 0.0).clicked();
        input(
            ui,
            &pal,
            &mut app.views.settings.acoustid_input,
            if stored == Some(true) {
                "••••••••"
            } else {
                "User key"
            },
            240.0,
            true,
        );
    });
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
        "Prefer Cover Art Archive covers",
        "When applying a MusicBrainz release in the tag editor.",
        true,
        caa,
    ) {
        app.session.config.config.tagger_use_caa_cover = on;
        app.session.config.save();
    }
}

fn sec_appearance(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, "Appearance", "");
    ui.add_space(4.0);
    let mode = app.session.config.config.theme_mode;
    let labels = ["System", "Dark", "Light"];
    let sel = ThemeMode::ALL.iter().position(|m| *m == mode).unwrap_or(0);
    if let Some(i) = seg_row(
        ui,
        &pal,
        "theme",
        "Theme",
        "System follows your desktop's dark or light setting.",
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
        "Tint the interface with cover colour",
        "The accent follows the dominant colour of the playing album.",
        false,
        tint,
    ) {
        app.set_tint_ui_with_cover(on);
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        "Reduce motion",
        "Turns off fades, slides and other interface animations.",
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
        "Interface size",
        "Scales the whole window. Ctrl + = / Ctrl + − / Ctrl + 0 change it anywhere.",
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
        "Grid card size",
        "Size of the cards in album, artist, genre, radio and podcast grids.",
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

const STARTUP_PAGES: [(&str, &str); 10] = [
    ("Home", "home"),
    ("Last visited page", "last"),
    ("Albums", "albums"),
    ("Artists", "artists"),
    ("Songs", "songs"),
    ("Genres", "genres"),
    ("Folders", "folders"),
    ("Playlists", "playlists"),
    ("Podcasts", "podcasts"),
    ("Radio", "radio"),
];

fn sec_startup(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, "Startup page", "");
    ui.add_space(4.0);
    let mut name = app.session.config.config.user_name.clone();
    let mut edited = false;
    row(
        ui,
        &pal,
        "Display name",
        "Greets you on the Home page (“Good evening, Sam”). Leave empty for no name.",
        false,
        250.0,
        |ui| {
            edited = input(ui, &pal, &mut name, "Your name", 220.0, false).changed();
        },
    );
    if edited {
        app.session.set_user_name(&name);
    }
    let cfg = &app.session.config.config;
    let page = cfg.startup_page.trim().to_owned();
    let page_label = STARTUP_PAGES
        .iter()
        .find(|(_, k)| *k == page)
        .map_or("Home", |(l, _)| *l);
    let options: Vec<String> = STARTUP_PAGES.iter().map(|(l, _)| (*l).to_owned()).collect();
    if let Some(i) = select_row(
        ui,
        &pal,
        "Startup page",
        "Opened when Aulos launches.",
        page_label,
        &options,
    ) {
        app.session.config.config.startup_page = STARTUP_PAGES[i].1.to_owned();
        app.session.config.save();
    }

    let provider = app.session.config.config.startup_provider.trim().to_owned();
    let mut ids: Vec<(String, String)> = vec![("last".to_owned(), "Last used source".to_owned())];
    ids.extend(app.session.providers.provider_list.iter().cloned());
    let label = ids
        .iter()
        .find(|(id, _)| *id == provider || (provider.is_empty() && id == "last"))
        .map_or_else(|| "Last used source".to_owned(), |(_, n)| n.clone());
    let names: Vec<String> = ids.iter().map(|(_, n)| n.clone()).collect();
    if let Some(i) = select_row(
        ui,
        &pal,
        "Startup source",
        "The library Aulos opens with.",
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
        "Start playing on launch",
        "Plays your library as soon as it has loaded.",
        false,
        auto_play,
    ) {
        app.session.config.config.auto_play_on_launch = on;
        app.session.config.save();
    }
}

fn sec_desktop(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, "Background playback", "Desktop integration");
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
        "Keep playing when the window is closed",
        "The window minimizes instead of quitting while music plays; MPRIS and media keys keep working.",
        false,
        background,
    ) {
        app.session.config.config.background_playback = on;
        app.session.config.save();
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        "Notify on track change",
        "A desktop notification with the cover while the window isn't focused.",
        false,
        notify,
    ) {
        app.session.config.config.notify_track_change = on;
        app.session.config.save();
    }
    if let Some(on) = toggle_row(
        ui,
        &pal,
        "Prevent suspend while playing",
        "Holds a desktop inhibit lock during playback.",
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
    heading(ui, &pal, "Experimental", "Features still being tested");
    ui.add_space(4.0);
    let on = app.session.config.config.experimental_converter;
    if let Some(v) = toggle_row(
        ui,
        &pal,
        "File converter",
        "Shows the Converter page: transcode and rip local files.",
        false,
        on,
    ) {
        app.session.config.config.experimental_converter = v;
        app.session.config.save();
    }
}

fn sec_about(app: &mut App, ui: &mut Ui) {
    let pal = app.pal;
    heading(ui, &pal, "About", "");
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Aulos")
            .font(theme::semibold(15.0))
            .color(pal.text),
    );
    small_text(
        ui,
        &format!("Version {}", env!("CARGO_PKG_VERSION")),
        pal.text_2,
    );
    small_text(
        ui,
        "A music player for your own library. Licensed under GPL-3.0-only.",
        pal.text_2,
    );
    ui.add_space(10.0);
    let repository = env!("CARGO_PKG_REPOSITORY");
    if !repository.is_empty()
        && pill_button(
            ui,
            "Project page",
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
