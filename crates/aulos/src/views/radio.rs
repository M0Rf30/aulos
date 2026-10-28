//! The radio page: saved stations and the station directory (Discover).

use crate::app::App;
use crate::theme::{self, Accent, ColorExt, Palette, Theme};
use crate::widgets::{
    Covers, PillKind, avatar_tile, chip, cover_image, empty_state, empty_state_actions, paint_icon,
    paint_text, pill_button, segmented, skeleton, text_field, text_width,
};
use aulos_core::online::UrlError;
use aulos_core::online::radio::{
    DISCOVER_LIMIT, DiscoverPreset, QUICK_TAGS, SortOrder, StationQuery, StationSearchResult,
    format_label, tag_list,
};
use aulos_core::online::store::RadioStation;
use aulos_core::session::RadioState;
use egui::{
    Align, CornerRadius, Layout, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit,
    Ui, UiBuilder, pos2, vec2,
};
use egui_phosphor::{fill, regular};
use std::hash::{Hash, Hasher};
use std::time::Duration;
use web_time::Instant;

/// How long an empty saved list counts as "still loading".
const LOADING_GRACE: Duration = Duration::from_millis(700);
const ROW_H: f32 = 58.0;
const TAB_SAVED: usize = 0;
const TAB_DISCOVER: usize = 1;

fn mode_labels() -> [String; 3] {
    [
        aulos_core::fl!("radio-mode-name"),
        aulos_core::fl!("radio-mode-tag"),
        aulos_core::fl!("radio-mode-country"),
    ]
}

/// What the Discover list currently shows, so Retry can repeat it.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Last {
    #[default]
    Query,
    Preset(PresetKind),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PresetKind {
    Popular,
    TopVoted,
}

#[derive(Default)]
pub struct State {
    tab: usize,
    filter: String,
    search: String,
    mode: usize,
    sort: Option<SortOrder>,
    tag: Option<usize>,
    last: Last,
    discover_started: bool,
    add_open: bool,
    add_name: String,
    add_url: String,
    add_error: Option<String>,
    renaming: Option<(i64, String)>,
    rename_focus: bool,
    entered: Option<Instant>,
    cache_key: u64,
    cache_idx: Vec<usize>,
}

#[derive(Clone)]
enum Target {
    Saved(i64),
    Result(String),
}

enum Intent {
    Play(Target),
    SaveResult(String),
    Remove(i64),
    Rename(i64, String),
    Search,
    Preset(PresetKind),
    SubmitAdd,
    DiscoverTab,
}

struct Ctx {
    pal: Palette,
    accent: Accent,
    pad: f32,
    compact: bool,
    /// Live ICY line ("artist — title") of the playing station, if any.
    now: Option<String>,
}

/// Called when the route is entered: start loading what the page needs.
pub fn enter(app: &mut App) {
    app.views.radio.entered = Some(Instant::now());
    // A rename left open on an earlier visit is dropped, not resumed.
    app.views.radio.renaming = None;
    app.views.radio.rename_focus = false;
    let load = app.session.radio.load();
    app.run(load);
}

fn recent(since: Option<Instant>) -> bool {
    since.is_some_and(|t| t.elapsed() < LOADING_GRACE)
}

/// `base`, raised to a finger-sized hit height on touch devices.
fn btn_h(ui: &Ui, base: f32) -> f32 {
    if theme::touch(ui.ctx()) {
        base.max(theme::size::TOUCH)
    } else {
        base
    }
}

fn padded<R>(ui: &mut Ui, pad: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = ui.available_rect_before_wrap();
    let inner = Rect::from_min_max(
        pos2(r.left() + pad, r.top()),
        pos2((r.right() - pad).max(r.left() + pad), r.bottom()),
    );
    ui.scope_builder(UiBuilder::new().max_rect(inner), add)
        .inner
}

fn sort_label(order: SortOrder) -> String {
    match order {
        SortOrder::Clickcount => aulos_core::fl!("radio-sort-most-played"),
        SortOrder::Votes => aulos_core::fl!("radio-sort-most-voted"),
        SortOrder::Name => aulos_core::fl!("radio-sort-name"),
        SortOrder::Bitrate => aulos_core::fl!("radio-sort-bitrate"),
    }
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let t = Theme::get(ui.ctx());
    let cx = Ctx {
        pal: t.pal,
        accent: t.accent,
        pad: app.page_pad(),
        compact: app.compact(),
        now: app
            .session
            .playback
            .current_track
            .as_ref()
            .filter(|c| &*c.provider_id == "radio")
            .map(|c| {
                let title = c.title.replace(" - ", " \u{2014} ");
                if c.artist.is_empty() || title.contains(" \u{2014} ") {
                    title
                } else {
                    format!("{} \u{2014} {title}", c.artist)
                }
            }),
    };
    let mut intents: Vec<Intent> = Vec::new();
    {
        let st = &mut app.views.radio;
        let radio = &app.session.radio;
        let covers = &app.covers;
        let playing_key = app
            .session
            .playback
            .current_track
            .as_ref()
            .filter(|c| &*c.provider_id == "radio")
            .map(|_| radio.now_playing_key.as_str());

        ui.add_space(4.0);
        header(ui, &cx, st, radio, &mut intents);
        if st.add_open {
            ui.add_space(12.0);
            add_form(ui, &cx, st, &mut intents);
        }
        ui.add_space(16.0);
        padded(ui, cx.pad, |ui| {
            let id = ui.id().with("radio-search");
            if search_box(ui, &cx, st, id) && st.tab == TAB_DISCOVER {
                intents.push(Intent::Search);
            }
        });
        if st.tab == TAB_DISCOVER {
            ui.add_space(10.0);
            chips(ui, &cx, st, &mut intents);
        }
        ui.add_space(12.0);

        if st.tab == TAB_SAVED {
            saved(ui, &cx, st, radio, covers, playing_key, &mut intents);
        } else {
            discover(ui, &cx, st, radio, covers, playing_key, &mut intents);
        }
    }
    for intent in intents {
        apply(app, intent);
    }
}

// -- header and filters --------------------------------------------------------

fn header(ui: &mut Ui, cx: &Ctx, st: &mut State, radio: &RadioState, intents: &mut Vec<Intent>) {
    padded(ui, cx.pad, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.label(
                RichText::new(aulos_core::fl!("radio"))
                    .font(theme::semibold(26.0))
                    .color(cx.pal.text),
            );
            let saved = aulos_core::fl!("radio-tab-saved", count = radio.stations.len());
            let discover_label = aulos_core::fl!("radio-tab-discover");
            let mut tab = st.tab;
            if segmented(
                ui,
                &mut tab,
                &[saved.as_str(), discover_label.as_str()],
                28.0,
            ) {
                st.tab = tab;
                if tab == TAB_DISCOVER {
                    intents.push(Intent::DiscoverTab);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if pill_button(
                    ui,
                    &aulos_core::fl!("radio-add-by-url"),
                    Some(regular::PLUS),
                    PillKind::Secondary,
                    btn_h(ui, 30.0),
                    0.0,
                )
                .clicked()
                {
                    st.add_open = !st.add_open;
                    st.add_error = None;
                }
            });
        });
    });
}

fn add_form(ui: &mut Ui, cx: &Ctx, st: &mut State, intents: &mut Vec<Intent>) {
    padded(ui, cx.pad, |ui| {
        let layout = if cx.compact {
            Layout::top_down(Align::Min)
        } else {
            Layout::left_to_right(Align::Center)
        };
        ui.with_layout(layout, |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            let full = ui.available_width();
            let (name_w, url_w) = if cx.compact {
                (full, full)
            } else {
                (180.0, 320.0)
            };
            text_field(
                ui,
                &mut st.add_name,
                &aulos_core::fl!("radio-add-name-placeholder"),
                name_w,
            );
            let resp = text_field(ui, &mut st.add_url, "https://example.com/stream.mp3", url_w);
            if resp.changed() {
                st.add_error = None;
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if pill_button(
                ui,
                &aulos_core::fl!("radio-add-station"),
                None,
                PillKind::Primary,
                btn_h(ui, 32.0),
                0.0,
            )
            .clicked()
                || enter
            {
                intents.push(Intent::SubmitAdd);
            }
            if let Some(err) = &st.add_error {
                ui.label(
                    RichText::new(err)
                        .font(theme::regular(12.5))
                        .color(cx.pal.danger),
                );
            }
        });
    });
}

/// The search/filter field. On Discover it carries the Name/Tag/Country mode
/// switch. Returns true when Enter was pressed in it.
fn search_box(ui: &mut Ui, cx: &Ctx, st: &mut State, id: egui::Id) -> bool {
    let pal = &cx.pal;
    let discover = st.tab == TAB_DISCOVER;
    let width = 420.0_f32.min(ui.available_width());
    let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
    let focused = ui.memory(|m| m.has_focus(id));
    let r = CornerRadius::same(8);
    ui.painter().rect_filled(rect, r, pal.surface_2);
    ui.painter().rect_stroke(
        rect,
        r,
        Stroke::new(1.0, if focused { pal.text } else { pal.input_bd }),
        StrokeKind::Inside,
    );

    let mode_font = theme::medium(12.0);
    let modes = mode_labels();
    let widths: Vec<f32> = modes
        .iter()
        .map(|m| text_width(ui, m, mode_font.clone()) + 16.0)
        .collect();
    let mode_w = if discover {
        widths.iter().sum::<f32>() + 6.0
    } else {
        0.0
    };
    if discover {
        let seg = Rect::from_min_size(
            pos2(rect.right() - 4.0 - mode_w, rect.top() + 5.0),
            vec2(mode_w, 24.0),
        );
        ui.painter().rect_filled(seg, CornerRadius::same(5), pal.bg);
        let mut x = seg.left() + 2.0;
        for (i, (label, w)) in modes.iter().zip(&widths).enumerate() {
            let cell = Rect::from_min_size(pos2(x, seg.top() + 2.0), vec2(*w, 20.0));
            let resp = ui.interact(cell, id.with(("mode", i)), Sense::click());
            let sel = st.mode == i;
            if sel {
                ui.painter()
                    .rect_filled(cell, CornerRadius::same(4), pal.seg_sel);
            } else if resp.hovered() {
                ui.painter()
                    .rect_filled(cell, CornerRadius::same(4), pal.text.alpha(0.05));
            }
            ui.painter().text(
                cell.center(),
                egui::Align2::CENTER_CENTER,
                label,
                if sel {
                    theme::semibold(12.0)
                } else {
                    mode_font.clone()
                },
                if sel { pal.text } else { pal.text_2 },
            );
            if resp.clicked() {
                st.mode = i;
            }
            x += *w;
        }
    }

    let inner = Rect::from_min_max(
        pos2(rect.left() + 10.0, rect.top()),
        pos2(rect.right() - 6.0 - mode_w, rect.bottom()),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 8.0;
    child.label(
        RichText::new(regular::MAGNIFYING_GLASS)
            .font(theme::icon(15.0))
            .color(pal.text_2),
    );
    let hint = if discover {
        match st.mode {
            0 => aulos_core::fl!("radio-search-by-name"),
            1 => aulos_core::fl!("radio-search-by-tag"),
            _ => aulos_core::fl!("radio-search-by-country"),
        }
    } else {
        aulos_core::fl!("radio-filter-saved")
    };
    let text = if discover {
        &mut st.search
    } else {
        &mut st.filter
    };
    let edit = TextEdit::singleline(text)
        .id(id)
        .hint_text(RichText::new(hint).color(pal.text_3))
        .frame(egui::Frame::NONE)
        .desired_width((child.available_width() - 4.0).max(20.0))
        .font(theme::regular(13.0))
        .vertical_align(Align::Center);
    let resp = child.add(edit);
    resp.lost_focus() && child.input(|i| i.key_pressed(egui::Key::Enter))
}

fn chips(ui: &mut Ui, cx: &Ctx, st: &mut State, intents: &mut Vec<Intent>) {
    padded(ui, cx.pad, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            if chip(
                ui,
                &aulos_core::fl!("radio-preset-popular"),
                st.last == Last::Preset(PresetKind::Popular),
                None,
            )
            .clicked()
            {
                intents.push(Intent::Preset(PresetKind::Popular));
            }
            if chip(
                ui,
                &aulos_core::fl!("radio-preset-top-voted"),
                st.last == Last::Preset(PresetKind::TopVoted),
                None,
            )
            .clicked()
            {
                intents.push(Intent::Preset(PresetKind::TopVoted));
            }
            for (i, tag) in QUICK_TAGS.iter().enumerate() {
                if chip(ui, tag, st.last == Last::Query && st.tag == Some(i), None).clicked() {
                    st.tag = if st.tag == Some(i) { None } else { Some(i) };
                    intents.push(Intent::Search);
                }
            }
            let order = st.sort.unwrap_or_default();
            let sort = chip(
                ui,
                &aulos_core::fl!("radio-sort-label", order = sort_label(order)),
                false,
                Some(regular::CARET_DOWN),
            );
            egui::Popup::menu(&sort).show(|ui| {
                for o in SortOrder::ALL {
                    if ui.selectable_label(o == order, sort_label(o)).clicked() {
                        st.sort = Some(o);
                        intents.push(Intent::Search);
                        ui.close();
                    }
                }
            });
        });
    });
}

// -- saved ----------------------------------------------------------------------

/// Row content shared by saved stations and directory results.
struct RowData<'a> {
    name: &'a str,
    meta: String,
    format: String,
    icon_url: &'a str,
    current: bool,
    saved: bool,
    target: Target,
    /// Id of the saved station behind the row (rename / remove).
    saved_id: Option<i64>,
}

fn refresh_cache(st: &mut State, radio: &RadioState) {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    st.filter.hash(&mut h);
    radio.stations_generation.hash(&mut h);
    let key = h.finish();
    if key == st.cache_key {
        return;
    }
    st.cache_key = key;
    let needle = st.filter.trim().to_lowercase();
    st.cache_idx.clear();
    st.cache_idx.extend(
        radio
            .stations
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                needle.is_empty()
                    || s.name.to_lowercase().contains(&needle)
                    || s.tags.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i),
    );
}

fn saved(
    ui: &mut Ui,
    cx: &Ctx,
    st: &mut State,
    radio: &RadioState,
    covers: &Covers,
    playing_key: Option<&str>,
    intents: &mut Vec<Intent>,
) {
    if radio.stations.is_empty() {
        if recent(st.entered) {
            skeleton_rows(ui, cx, 6);
        } else if empty_state(
            ui,
            regular::RADIO,
            &aulos_core::fl!("radio-saved-empty-title"),
            &aulos_core::fl!("radio-saved-empty-body"),
            Some(aulos_core::fl!("radio-open-discover").as_str()),
        ) {
            st.tab = TAB_DISCOVER;
            intents.push(Intent::DiscoverTab);
        }
        return;
    }
    refresh_cache(st, radio);
    if st.cache_idx.is_empty() {
        empty_state(
            ui,
            regular::MAGNIFYING_GLASS,
            &aulos_core::fl!("radio-no-filter-matches"),
            &aulos_core::fl!("radio-no-filter-matches-hint"),
            None,
        );
        return;
    }
    let idx = std::mem::take(&mut st.cache_idx);
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, idx.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for &i in &idx[range] {
                let Some(s) = radio.stations.get(i) else {
                    continue;
                };
                let data = saved_row(s, playing_key);
                station_row(ui, cx, st, covers, &data, intents);
            }
        });
    st.cache_idx = idx;
}

fn saved_row<'a>(s: &'a RadioStation, playing_key: Option<&str>) -> RowData<'a> {
    let mut meta = tag_list(&s.tags, 3).join(", ");
    if meta.is_empty() {
        meta = s.homepage.clone();
    }
    RowData {
        name: &s.name,
        meta,
        format: format_label(&s.codec, s.bitrate_kbps),
        icon_url: &s.favicon_url,
        current: playing_key == Some(s.stream_url.as_str()),
        saved: true,
        target: Target::Saved(s.id),
        saved_id: Some(s.id),
    }
}

// -- discover -------------------------------------------------------------------

fn discover(
    ui: &mut Ui,
    cx: &Ctx,
    st: &mut State,
    radio: &RadioState,
    covers: &Covers,
    playing_key: Option<&str>,
    intents: &mut Vec<Intent>,
) {
    if radio.search_loading {
        skeleton_rows(ui, cx, 7);
        return;
    }
    if radio.search_error.is_some() {
        let body = aulos_core::fl!("radio-unreachable-body", count = radio.stations.len());
        let (retry, saved) = empty_state_actions(
            ui,
            regular::CELL_SIGNAL_X,
            &aulos_core::fl!("radio-unreachable-title"),
            &body,
            &aulos_core::fl!("radio-retry"),
            Some(aulos_core::fl!("radio-go-to-saved").as_str()),
            true,
        );
        if retry {
            intents.push(match st.last {
                Last::Preset(p) => Intent::Preset(p),
                Last::Query => Intent::Search,
            });
        }
        if saved {
            st.tab = TAB_SAVED;
        }
        return;
    }
    if radio.search_results.is_empty() {
        if st.discover_started {
            empty_state(
                ui,
                regular::RADIO,
                &aulos_core::fl!("radio-no-results"),
                &aulos_core::fl!("radio-no-results-hint"),
                None,
            );
        }
        return;
    }
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, radio.search_results.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for r in &radio.search_results[range] {
                let saved = radio.stations.iter().find(|s| s.stream_url == r.url);
                let data = result_row(r, saved, playing_key);
                station_row(ui, cx, st, covers, &data, intents);
            }
        });
}

fn result_row<'a>(
    r: &'a StationSearchResult,
    saved: Option<&RadioStation>,
    playing_key: Option<&str>,
) -> RowData<'a> {
    let mut parts: Vec<String> = Vec::new();
    let tags = tag_list(&r.tags, 3).join(", ");
    if !tags.is_empty() {
        parts.push(tags);
    }
    if !r.countrycode.is_empty() {
        parts.push(r.countrycode.clone());
    }
    if r.votes > 0 {
        parts.push(aulos_core::fl!("radio-votes", votes = compact(r.votes)));
    }
    RowData {
        name: &r.name,
        meta: parts.join(" · "),
        format: format_label(&r.codec, r.bitrate),
        icon_url: &r.favicon,
        current: playing_key == Some(r.url.as_str()),
        saved: saved.is_some(),
        target: Target::Result(r.stationuuid.clone()),
        saved_id: saved.map(|s| s.id),
    }
}

fn compact(n: i64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

// -- rows -----------------------------------------------------------------------

fn station_row(
    ui: &mut Ui,
    cx: &Ctx,
    st: &mut State,
    covers: &Covers,
    d: &RowData<'_>,
    intents: &mut Vec<Intent>,
) {
    let pal = &cx.pal;
    let (full, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
    let rect = full.shrink2(vec2(cx.pad, 1.0));
    let uuid = match &d.target {
        Target::Result(u) => u.as_str(),
        Target::Saved(_) => "",
    };
    let row_id = ui.id().with(("station", d.saved_id, d.name, uuid));
    let resp = ui.interact(rect, row_id, Sense::click());
    let heart_rect = Rect::from_center_size(
        pos2(rect.right() - 10.0 - 16.0, rect.center().y),
        vec2(32.0, 32.0),
    );
    let heart = ui.interact(heart_rect, row_id.with("heart"), Sense::click());

    if d.current {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), cx.accent.row_tint);
    } else if resp.hovered() || heart.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), pal.hover);
    }

    let icon = Rect::from_min_size(
        pos2(rect.left() + 10.0, rect.center().y - 20.0),
        vec2(40.0, 40.0),
    );
    match covers.icon(d.icon_url) {
        Some(tex) => cover_image(ui, covers, icon, Some(tex), d.name, 8),
        None => avatar_tile(ui, covers, icon, d.name, None, false),
    }

    let wide = rect.width() > 520.0;
    let fmt_w = if wide && !d.format.is_empty() {
        80.0
    } else {
        0.0
    };
    let text_l = icon.right() + 12.0;
    let text_r = heart_rect.left() - 12.0 - fmt_w;
    let renaming = st
        .renaming
        .as_ref()
        .is_some_and(|(id, _)| Some(*id) == d.saved_id);

    let mut name_x = text_l;
    let title_rect = Rect::from_min_max(
        pos2(name_x, rect.center().y - 19.0),
        pos2(text_r, rect.center().y - 1.0),
    );
    if renaming {
        rename_edit(ui, st, title_rect, intents);
    } else {
        let color = if d.current { cx.accent.text } else { pal.text };
        let w = paint_text(
            ui.painter(),
            title_rect,
            Align::Min,
            d.name,
            theme::semibold(13.5),
            color,
        );
        name_x += w + 8.0;
        if d.current && name_x + 50.0 < text_r {
            badge(
                ui.painter(),
                pos2(name_x, rect.center().y - 19.0),
                &aulos_core::fl!("radio-on-air"),
                cx.accent.text,
                cx.accent.fill.alpha(0.16),
            );
        }
    }
    let meta_rect = Rect::from_min_max(
        pos2(text_l, rect.center().y + 2.0),
        pos2(text_r, rect.center().y + 18.0),
    );
    let now = cx
        .now
        .as_deref()
        .filter(|n| d.current && !n.is_empty() && *n != d.name);
    match now {
        Some(n) => {
            paint_text(
                ui.painter(),
                meta_rect,
                Align::Min,
                &aulos_core::fl!("radio-now", title = n),
                theme::regular(12.0),
                cx.accent.text,
            );
        }
        None => {
            paint_text(
                ui.painter(),
                meta_rect,
                Align::Min,
                &d.meta,
                theme::regular(12.0),
                if d.current { pal.text_15 } else { pal.text_2 },
            );
        }
    }
    if fmt_w > 0.0 {
        let r = Rect::from_min_max(
            pos2(heart_rect.left() - 12.0 - fmt_w, rect.top()),
            pos2(heart_rect.left() - 12.0, rect.bottom()),
        );
        paint_text(
            ui.painter(),
            r,
            Align::Max,
            &d.format,
            theme::mono(11.0),
            pal.text_2,
        );
    }
    let (glyph, color, filled) = if d.saved {
        (fill::HEART, pal.text_1, true)
    } else {
        (
            regular::HEART,
            if resp.hovered() || heart.hovered() {
                pal.text_2
            } else {
                pal.text_4
            },
            false,
        )
    };
    paint_icon(
        ui.painter(),
        heart_rect.center(),
        glyph,
        16.0,
        color,
        filled,
    );
    let heart = heart.on_hover_text(if d.saved {
        aulos_core::fl!("radio-remove-from-saved")
    } else {
        aulos_core::fl!("radio-save-station")
    });

    if heart.clicked() {
        match (&d.target, d.saved_id) {
            (_, Some(id)) => intents.push(Intent::Remove(id)),
            (Target::Result(uuid), None) => intents.push(Intent::SaveResult(uuid.clone())),
            (Target::Saved(_), None) => {}
        }
    } else if resp.clicked() && !renaming {
        intents.push(Intent::Play(d.target.clone()));
    }
    resp.context_menu(|ui| {
        if ui.button(aulos_core::fl!("play")).clicked() {
            intents.push(Intent::Play(d.target.clone()));
            ui.close();
        }
        match d.saved_id {
            Some(id) => {
                if ui.button(aulos_core::fl!("radio-rename")).clicked() {
                    st.renaming = Some((id, d.name.to_owned()));
                    st.rename_focus = true;
                    ui.close();
                }
                if ui
                    .button(aulos_core::fl!("radio-remove-from-saved"))
                    .clicked()
                {
                    intents.push(Intent::Remove(id));
                    ui.close();
                }
            }
            None => {
                if let Target::Result(uuid) = &d.target
                    && ui.button(aulos_core::fl!("radio-save-station")).clicked()
                {
                    intents.push(Intent::SaveResult(uuid.clone()));
                    ui.close();
                }
            }
        }
    });
}

/// The inline rename editor in a saved row's title slot.
fn rename_edit(ui: &mut Ui, st: &mut State, rect: Rect, intents: &mut Vec<Intent>) {
    let focus = std::mem::take(&mut st.rename_focus);
    let Some((id, buf)) = st.renaming.as_mut() else {
        return;
    };
    let edit = TextEdit::singleline(buf)
        .font(theme::semibold(13.5))
        .desired_width(rect.width());
    let resp = ui.put(rect, edit);
    if focus {
        resp.request_focus();
    }
    let id = *id;
    let name = buf.trim().to_owned();
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.renaming = None;
    } else if resp.lost_focus() {
        st.renaming = None;
        if !name.is_empty() {
            intents.push(Intent::Rename(id, name));
        }
    }
}

fn badge(p: &egui::Painter, pos: egui::Pos2, text: &str, fg: egui::Color32, bg: egui::Color32) {
    let galley = p.layout_no_wrap(text.to_owned(), theme::mono_semibold(9.5), fg);
    let size = galley.size() + vec2(10.0, 6.0);
    let rect = Rect::from_min_size(pos2(pos.x, pos.y), size);
    p.rect_filled(rect, CornerRadius::same(3), bg);
    p.galley(rect.center() - galley.size() / 2.0, galley, fg);
}

fn skeleton_rows(ui: &mut Ui, cx: &Ctx, n: usize) {
    padded(ui, cx.pad, |ui| {
        for i in 0..n {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
            let icon = Rect::from_min_size(
                pos2(rect.left() + 10.0, rect.center().y - 20.0),
                vec2(40.0, 40.0),
            );
            let l = icon.right() + 12.0;
            let w = (rect.right() - l).max(40.0);
            let a = Rect::from_min_size(pos2(l, rect.center().y - 14.0), vec2(w * 0.3, 12.0));
            let b = Rect::from_min_size(pos2(l, rect.center().y + 4.0), vec2(w * 0.5, 10.0));
            ui.scope(|ui| {
                ui.multiply_opacity(1.0 - i as f32 * 0.1);
                skeleton(ui, icon, 8);
                skeleton(ui, a, 4);
                skeleton(ui, b, 4);
            });
        }
    });
    ui.ctx().request_repaint_after(Duration::from_millis(60));
}

// -- applying intents -------------------------------------------------------------

fn apply(app: &mut App, intent: Intent) {
    match intent {
        Intent::DiscoverTab => {
            if !app.views.radio.discover_started {
                apply(app, Intent::Preset(PresetKind::Popular));
            }
        }
        Intent::Play(target) => play(app, target),
        Intent::SaveResult(uuid) => {
            let effect = app.session.radio.save_search_result(&uuid);
            app.run(effect);
        }
        Intent::Remove(id) => {
            if let Some((station, effect)) = app.session.radio.remove(id) {
                app.run(effect);
                let name = station.name.clone();
                // Fluent can't split around the bold station name, so format
                // once with a sentinel and cut the message at it.
                const MARK: &str = "\u{1}";
                let msg = aulos_core::fl!("radio-removed-toast", name = MARK);
                let (prefix, suffix) = msg.split_once(MARK).unwrap_or((&msg, ""));
                let prefix = prefix.trim_end_matches('\u{2068}').to_owned();
                let suffix = suffix.trim_start_matches('\u{2069}').to_owned();
                app.toast_rich(
                    regular::TRASH,
                    prefix,
                    name,
                    suffix,
                    Some(crate::app::ToastAction::RestoreStation(station)),
                );
            }
        }
        Intent::Rename(id, name) => {
            let effect = app.session.radio.rename(id, name);
            app.run(effect);
        }
        Intent::Preset(kind) => {
            let st = &mut app.views.radio;
            st.discover_started = true;
            st.last = Last::Preset(kind);
            st.tag = None;
            let preset = match kind {
                PresetKind::Popular => DiscoverPreset::Popular,
                PresetKind::TopVoted => DiscoverPreset::TopVoted,
            };
            let effect = app.session.radio.preset(preset);
            app.run(effect);
        }
        Intent::Search => {
            let st = &mut app.views.radio;
            st.discover_started = true;
            st.last = Last::Query;
            let text = st.search.trim().to_owned();
            let mut query = StationQuery {
                order: st.sort.unwrap_or_default(),
                limit: DISCOVER_LIMIT,
                ..StationQuery::default()
            };
            match st.mode {
                0 => query.name = text,
                1 => query.tag = text,
                _ => query.countrycode = text.to_uppercase(),
            }
            if let Some(tag) = st.tag.and_then(|i| QUICK_TAGS.get(i)) {
                query.tag = (*tag).to_owned();
            }
            let effect = app.session.radio.search(query);
            app.run(effect);
        }
        Intent::SubmitAdd => {
            let st = &mut app.views.radio;
            let name = st.add_name.trim().to_owned();
            let url = st.add_url.trim().to_owned();
            match app.session.radio.add_by_url(name, url) {
                Ok(effect) => {
                    app.run(effect);
                    app.toast(aulos_core::fl!("radio-adding-toast"));
                    let st = &mut app.views.radio;
                    st.add_open = false;
                    st.add_name.clear();
                    st.add_url.clear();
                    st.add_error = None;
                }
                Err(UrlError::Empty) => {
                    app.views.radio.add_error = Some(aulos_core::fl!("radio-add-url-required"))
                }
                Err(UrlError::NotHttp) => {
                    app.views.radio.add_error = Some(aulos_core::fl!("radio-add-url-invalid"))
                }
            }
        }
    }
}

/// Starts a station; clicking the one that is already playing toggles pause.
fn play(app: &mut App, target: Target) {
    let key = match &target {
        Target::Saved(id) => app
            .session
            .radio
            .stations
            .iter()
            .find(|s| s.id == *id)
            .map(|s| s.stream_url.clone()),
        Target::Result(uuid) => app
            .session
            .radio
            .search_results
            .iter()
            .find(|r| r.stationuuid == *uuid)
            .map(|r| r.url.clone()),
    };
    let current = app
        .session
        .playback
        .current_track
        .as_ref()
        .is_some_and(|c| &*c.provider_id == "radio")
        && key.as_deref() == Some(app.session.radio.now_playing_key.as_str());
    if current {
        app.toggle_play();
        return;
    }
    let effect = match target {
        Target::Saved(id) => app.session.radio.play_saved(id),
        Target::Result(uuid) => app.session.radio.play_search_result(&uuid),
    };
    app.run(effect);
}
