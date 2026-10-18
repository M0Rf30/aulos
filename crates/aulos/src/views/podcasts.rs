//! The podcasts page: subscriptions, the open show's episodes and Discover.

use crate::app::App;
use crate::theme::{self, Accent, ColorExt, Palette, Theme};
use crate::widgets::{
    IconBtn, PillKind, chip, cover_image, empty_state, empty_state_actions, empty_state_tinted,
    paint_icon, paint_text, pill_button, search_field, segmented, skeleton, text_field,
};
use aulos_core::online::UrlError;
use aulos_core::online::podcast::{Cadence, EpisodeFilter, primary_episode};
use aulos_core::online::store::{Episode, Podcast};
use aulos_core::session::{EpisodePlayback, PodcastsState};
use egui::{
    Align, CornerRadius, Layout, Rect, RichText, ScrollArea, Sense, Stroke, Ui, UiBuilder, pos2,
    vec2,
};
use egui_phosphor::{fill, regular};
use std::hash::{Hash, Hasher};
use std::time::Duration;
use web_time::{Instant, SystemTime, UNIX_EPOCH};

/// How long an empty list counts as "still loading" (the session has no
/// loaded flag for the store reads).
const LOADING_GRACE: Duration = Duration::from_millis(700);
const EP_ROW: f32 = 61.0;
const SHOW_ROW: f32 = 72.0;
const RESULT_ROW: f32 = 64.0;
const TAB_SUBSCRIPTIONS: usize = 0;
const TAB_DISCOVER: usize = 1;

#[derive(Default)]
pub struct State {
    tab: usize,
    add_open: bool,
    add_url: String,
    add_error: Option<String>,
    search: String,
    searched: bool,
    filter: usize,
    text: String,
    desc_open: bool,
    pending_unsub: Option<i64>,
    /// The user went back to the list of shows; until then the first show's
    /// detail opens by itself.
    browsing_list: bool,
    entered: Option<Instant>,
    selected_at: Option<Instant>,
    /// Plain text of the open show's description.
    desc: (i64, String),
    /// Episode indices passing the filters, with the key they were built for.
    cache_key: u64,
    cache_idx: Vec<usize>,
    cache_unplayed: usize,
    cache_downloaded: usize,
}

/// What the page asked for; applied once drawing no longer borrows the session.
enum Intent {
    Select(i64),
    Back,
    Refresh(i64),
    RefreshAll,
    Unsubscribe(i64),
    AskUnsubscribe(i64),
    Play(i64),
    TogglePlayed(i64),
    Download(i64),
    CancelDownload(i64),
    DeleteDownload(i64),
    RetryRefreshes,
    DismissRefreshErrors,
    SubmitSearch,
    Subscribe(String),
}

struct Ctx {
    pal: Palette,
    accent: Accent,
    pad: f32,
    compact: bool,
}

/// Called when the route is entered: start loading what the page needs.
pub fn enter(app: &mut App) {
    app.views.podcasts.entered = Some(Instant::now());
    app.views.podcasts.browsing_list = false;
    let load = app.session.podcasts.load();
    app.run(load);
}

fn recent(since: Option<Instant>) -> bool {
    since.is_some_and(|t| t.elapsed() < LOADING_GRACE)
}

/// Runs `add` in the page's horizontal padding.
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

pub fn show(app: &mut App, ui: &mut Ui) {
    let t = Theme::get(ui.ctx());
    let cx = Ctx {
        pal: t.pal,
        accent: t.accent,
        pad: app.page_pad(),
        compact: app.compact(),
    };
    let mut intents: Vec<Intent> = Vec::new();
    // The page opens on the first show's episodes; Back reaches the list.
    let st = &app.views.podcasts;
    if st.tab == TAB_SUBSCRIPTIONS
        && !st.browsing_list
        && app.session.podcasts.selected.is_none()
        && let Some(first) = app.session.podcasts.podcasts.first().map(|p| p.id)
    {
        apply(app, Intent::Select(first));
    }
    {
        let st = &mut app.views.podcasts;
        let pod = &app.session.podcasts;
        let covers = &app.covers;
        let current = app.session.playback.current_track.as_ref();
        let playing_episode = pod
            .current_episode_id
            .filter(|_| current.is_some_and(|c| &*c.provider_id == "podcast"));
        let is_playing = app.session.playback.is_playing();

        let detail = st.tab == TAB_SUBSCRIPTIONS
            && pod
                .selected
                .and_then(|id| pod.podcasts.iter().find(|p| p.id == id))
                .is_some();

        ui.add_space(4.0);
        header(ui, &cx, st, pod, detail, &mut intents);
        if st.add_open {
            ui.add_space(12.0);
            add_form(ui, &cx, st, &mut intents);
        }
        refresh_banner(ui, &cx, pod, &mut intents);
        ui.add_space(18.0);

        if st.tab == TAB_DISCOVER {
            discover(ui, &cx, st, pod, covers, &mut intents);
        } else if detail {
            show_detail(
                ui,
                &cx,
                st,
                pod,
                covers,
                playing_episode.map(|id| (id, is_playing)),
                &mut intents,
            );
        } else {
            subscriptions(ui, &cx, st, pod, covers, &mut intents);
        }
    }
    for intent in intents {
        apply(app, intent);
    }
}

// -- header ------------------------------------------------------------------

fn header(
    ui: &mut Ui,
    cx: &Ctx,
    st: &mut State,
    pod: &PodcastsState,
    detail: bool,
    intents: &mut Vec<Intent>,
) {
    padded(ui, cx.pad, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            if detail
                && IconBtn::new(regular::CARET_LEFT)
                    .size(btn_h(ui, 30.0))
                    .icon_size(17.0)
                    .tip(&aulos_core::fl!("podcasts-all-shows"))
                    .show(ui)
                    .clicked()
            {
                intents.push(Intent::Back);
            }
            ui.label(
                RichText::new(aulos_core::fl!("podcasts"))
                    .font(theme::semibold(26.0))
                    .color(cx.pal.text),
            );
            let mut tab = st.tab;
            let tab_labels = [
                aulos_core::fl!("subscriptions"),
                aulos_core::fl!("podcast-tab-discover"),
            ];
            if segmented(
                ui,
                &mut tab,
                &[tab_labels[0].as_str(), tab_labels[1].as_str()],
                28.0,
            ) {
                st.tab = tab;
                if detail && tab == TAB_DISCOVER {
                    intents.push(Intent::Back);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if pill_button(
                    ui,
                    &aulos_core::fl!("podcast-add-by-url"),
                    Some(regular::RSS),
                    PillKind::Secondary,
                    btn_h(ui, 30.0),
                    0.0,
                )
                .clicked()
                {
                    st.add_open = !st.add_open;
                    st.add_error = None;
                }
                if !pod.podcasts.is_empty()
                    && IconBtn::new(regular::ARROW_CLOCKWISE)
                        .size(btn_h(ui, 30.0))
                        .icon_size(16.0)
                        .tip(&aulos_core::fl!("podcasts-refresh-all-feeds"))
                        .show(ui)
                        .clicked()
                {
                    intents.push(Intent::RefreshAll);
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
            let url_w = if cx.compact {
                ui.available_width()
            } else {
                380.0
            };
            let resp = text_field(ui, &mut st.add_url, "https://example.com/feed.xml", url_w);
            if resp.changed() {
                st.add_error = None;
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if pill_button(
                ui,
                &aulos_core::fl!("subscribe"),
                None,
                PillKind::Primary,
                btn_h(ui, 32.0),
                0.0,
            )
            .clicked()
                || enter
            {
                intents.push(Intent::Subscribe(st.add_url.trim().to_owned()));
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

/// "Couldn't refresh N feeds" card with a Retry action, while any
/// subscription's last refresh failed.
fn refresh_banner(ui: &mut Ui, cx: &Ctx, pod: &PodcastsState, intents: &mut Vec<Intent>) {
    let failures = pod.refresh_failures();
    if failures.is_empty() {
        return;
    }
    let pal = &cx.pal;
    let n = failures.len();
    let title = aulos_core::fl!("podcasts-refresh-failed-title", count = n);
    let names: Vec<&str> = failures.iter().map(|f| f.podcast.title.as_str()).collect();
    let names = match names.as_slice() {
        [one] => (*one).to_owned(),
        [a, b] => aulos_core::fl!(
            "podcasts-names-two",
            a = (*a).to_owned(),
            b = (*b).to_owned()
        ),
        [a, b, rest @ ..] => {
            aulos_core::fl!(
                "podcasts-names-many",
                a = (*a).to_owned(),
                b = (*b).to_owned(),
                more = (rest.len())
            )
        }
        [] => String::new(),
    };
    let first = failures[0].reason;
    let mut detail = if failures.iter().all(|f| f.reason == first) {
        aulos_core::fl!(
            "podcasts-refresh-failed-detail",
            names = names.as_str(),
            reason = first
        )
    } else {
        aulos_core::fl!(
            "podcasts-refresh-failed-unreachable",
            names = names.as_str()
        )
    };
    let oldest = failures
        .iter()
        .map(|f| f.podcast.last_refreshed)
        .filter(|t| *t > 0)
        .min();
    if let Some(oldest) = oldest {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        detail.push(' ');
        detail.push_str(&aulos_core::fl!(
            "podcasts-cached-ago",
            age = age_label((now - oldest).max(0))
        ));
    }
    let retrying = failures
        .iter()
        .any(|f| pod.refreshing.contains(&f.podcast.id));
    ui.add_space(12.0);
    padded(ui, cx.pad, |ui| {
        egui::Frame::new()
            .fill(pal.danger.alpha(0.08))
            .stroke(Stroke::new(1.0, pal.danger.alpha(0.3)))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    ui.label(
                        RichText::new(regular::WARNING_CIRCLE)
                            .size(18.0)
                            .color(pal.danger),
                    );
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.set_max_width((ui.available_width() - 190.0).max(200.0));
                        ui.label(
                            RichText::new(title)
                                .font(theme::semibold(13.5))
                                .color(pal.text),
                        );
                        ui.label(
                            RichText::new(detail)
                                .font(theme::regular(13.0))
                                .color(pal.text_2),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                        if IconBtn::new(regular::X)
                            .size(btn_h(ui, 30.0))
                            .icon_size(14.0)
                            .tip(&aulos_core::fl!("podcasts-dismiss"))
                            .show(ui)
                            .clicked()
                        {
                            intents.push(Intent::DismissRefreshErrors);
                        }
                        let label = if retrying {
                            aulos_core::fl!("podcasts-retrying")
                        } else {
                            aulos_core::fl!("podcast-retry")
                        };
                        if pill_button(
                            ui,
                            &label,
                            Some(regular::ARROW_CLOCKWISE),
                            PillKind::Secondary,
                            btn_h(ui, 30.0),
                            0.0,
                        )
                        .clicked()
                            && !retrying
                        {
                            intents.push(Intent::RetryRefreshes);
                        }
                    });
                });
            });
    });
}

/// "45 minutes", "3 hours", "2 days": how long ago something was.
fn age_label(secs: i64) -> String {
    match secs {
        s if s < 3_600 => aulos_core::fl!("podcasts-age-minutes", count = (s / 60).max(1)),
        s if s < 86_400 => aulos_core::fl!("podcasts-age-hours", count = (s / 3_600)),
        s => aulos_core::fl!("podcasts-age-days", count = (s / 86_400)),
    }
}

// -- subscriptions -------------------------------------------------------------

fn subscriptions(
    ui: &mut Ui,
    cx: &Ctx,
    st: &mut State,
    pod: &PodcastsState,
    covers: &crate::widgets::Covers,
    intents: &mut Vec<Intent>,
) {
    if pod.podcasts.is_empty() {
        if recent(st.entered) {
            skeleton_rows(ui, cx, 5, 56.0, SHOW_ROW);
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        } else {
            let (discover, add) = empty_state_actions(
                ui,
                regular::MICROPHONE_STAGE,
                &aulos_core::fl!("podcasts-none-title"),
                &aulos_core::fl!("podcasts-none-body"),
                &aulos_core::fl!("podcasts-open-discover"),
                Some(aulos_core::fl!("podcast-add-by-url").as_str()),
                false,
            );
            if discover {
                st.tab = TAB_DISCOVER;
            }
            if add {
                st.add_open = true;
            }
        }
        return;
    }
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, SHOW_ROW, pod.podcasts.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for show in &pod.podcasts[range] {
                show_row(ui, cx, pod, covers, show, intents);
            }
        });
}

fn show_row(
    ui: &mut Ui,
    cx: &Ctx,
    pod: &PodcastsState,
    covers: &crate::widgets::Covers,
    show: &Podcast,
    intents: &mut Vec<Intent>,
) {
    let (full, _) = ui.allocate_exact_size(vec2(ui.available_width(), SHOW_ROW), Sense::hover());
    let rect = full.shrink2(vec2(cx.pad, 0.0));
    let resp = ui.interact(rect, ui.id().with(("show", show.id)), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(
            rect.shrink2(vec2(0.0, 2.0)),
            CornerRadius::same(10),
            cx.pal.hover,
        );
    }
    let art = Rect::from_min_size(
        pos2(rect.left() + 8.0, rect.center().y - 28.0),
        vec2(56.0, 56.0),
    );
    cover_image(
        ui,
        covers,
        art,
        covers.icon(&show.image_url),
        &show.title,
        10,
    );
    let text_l = art.right() + 14.0;
    let text_r = rect.right() - 80.0;
    let title = Rect::from_min_max(
        pos2(text_l, rect.center().y - 20.0),
        pos2(text_r, rect.center().y - 2.0),
    );
    paint_text(
        p,
        title,
        Align::Min,
        &show.title,
        theme::semibold(14.0),
        cx.pal.text,
    );
    let mut meta = show.author.clone();
    if let Some(cadence) = show.cadence {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&cadence_label(cadence));
    }
    if show.unplayed_count > 0 {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&aulos_core::fl!(
            "podcast-unplayed-badge",
            count = show.unplayed_count
        ));
    }
    let meta_r = Rect::from_min_max(
        pos2(text_l, rect.center().y + 2.0),
        pos2(text_r, rect.center().y + 20.0),
    );
    paint_text(
        p,
        meta_r,
        Align::Min,
        &meta,
        theme::regular(12.5),
        cx.pal.text_2,
    );
    if pod.refreshing.contains(&show.id) {
        paint_icon(
            p,
            pos2(rect.right() - 30.0, rect.center().y),
            regular::CIRCLE_NOTCH,
            16.0,
            cx.pal.text_3,
            false,
        );
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    } else {
        paint_icon(
            p,
            pos2(rect.right() - 30.0, rect.center().y),
            regular::CARET_RIGHT,
            15.0,
            cx.pal.text_4,
            false,
        );
    }
    if resp.clicked() {
        intents.push(Intent::Select(show.id));
    }
    resp.context_menu(|ui| {
        if ui.button(aulos_core::fl!("podcasts-menu-open")).clicked() {
            intents.push(Intent::Select(show.id));
            ui.close();
        }
        if ui.button(aulos_core::fl!("podcast-refresh")).clicked() {
            intents.push(Intent::Refresh(show.id));
            ui.close();
        }
        if ui
            .button(aulos_core::fl!("podcasts-menu-unsubscribe"))
            .clicked()
        {
            intents.push(Intent::AskUnsubscribe(show.id));
            ui.close();
        }
    });
}

// -- show detail ---------------------------------------------------------------

fn show_detail(
    ui: &mut Ui,
    cx: &Ctx,
    st: &mut State,
    pod: &PodcastsState,
    covers: &crate::widgets::Covers,
    playing: Option<(i64, bool)>,
    intents: &mut Vec<Intent>,
) {
    let Some(show) = pod
        .selected
        .and_then(|id| pod.podcasts.iter().find(|p| p.id == id))
    else {
        return;
    };
    if st.desc.0 != show.id {
        st.desc = (show.id, plain_text(&show.description));
    }
    hero(ui, cx, st, pod, covers, show, intents);
    ui.add_space(18.0);
    refresh_cache(st, pod);
    padded(ui, cx.pad, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            let labels = [
                aulos_core::fl!("podcast-filter-all"),
                aulos_core::fl!("podcasts-filter-unplayed-count", count = st.cache_unplayed),
                aulos_core::fl!(
                    "podcasts-filter-downloaded-count",
                    count = st.cache_downloaded
                ),
            ];
            for (i, label) in labels.iter().enumerate() {
                if chip(ui, label, st.filter == i, None).clicked() {
                    st.filter = i;
                }
            }
            if cx.compact {
                let w = ui.available_width().max(160.0);
                text_field(
                    ui,
                    &mut st.text,
                    &aulos_core::fl!("podcasts-filter-episodes"),
                    w,
                );
            } else {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    text_field(
                        ui,
                        &mut st.text,
                        &aulos_core::fl!("podcasts-filter-episodes"),
                        220.0,
                    );
                });
            }
        });
    });
    ui.add_space(10.0);

    if pod.episodes.is_empty() {
        if recent(st.selected_at) || pod.refreshing.contains(&show.id) {
            skeleton_rows(ui, cx, 5, 36.0, EP_ROW);
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        } else if empty_state(
            ui,
            regular::MICROPHONE_STAGE,
            &aulos_core::fl!("podcasts-no-episodes-title"),
            &aulos_core::fl!("podcasts-no-episodes-body"),
            Some(aulos_core::fl!("podcast-refresh").as_str()),
        ) {
            intents.push(Intent::Refresh(show.id));
        }
        return;
    }
    if st.cache_idx.is_empty() {
        padded(ui, cx.pad, |ui| {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(aulos_core::fl!("podcasts-no-episodes-match"))
                        .font(theme::regular(13.5))
                        .color(cx.pal.text_2),
                );
            });
        });
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let idx = &st.cache_idx;
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, EP_ROW, idx.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for &i in &idx[range] {
                if let Some(e) = pod.episodes.get(i) {
                    episode_row(ui, cx, pod, e, playing, now, intents);
                }
            }
        });
}

/// Rebuilds the filtered episode list when the episodes or filters changed.
fn refresh_cache(st: &mut State, pod: &PodcastsState) {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    pod.selected.hash(&mut h);
    st.filter.hash(&mut h);
    st.text.hash(&mut h);
    pod.episodes_generation.hash(&mut h);
    let key = h.finish();
    if key == st.cache_key {
        return;
    }
    st.cache_key = key;
    let filter = match st.filter {
        1 => EpisodeFilter::Unplayed,
        2 => EpisodeFilter::Downloaded,
        _ => EpisodeFilter::All,
    };
    st.cache_idx.clear();
    st.cache_idx.extend(
        pod.episodes
            .iter()
            .enumerate()
            .filter(|(_, e)| e.matches(filter, &st.text))
            .map(|(i, _)| i),
    );
    st.cache_unplayed = pod.episodes.iter().filter(|e| !e.played).count();
    st.cache_downloaded = pod
        .episodes
        .iter()
        .filter(|e| !e.downloaded_path.is_empty())
        .count();
}

fn hero(
    ui: &mut Ui,
    cx: &Ctx,
    st: &mut State,
    pod: &PodcastsState,
    covers: &crate::widgets::Covers,
    show: &Podcast,
    intents: &mut Vec<Intent>,
) {
    padded(ui, cx.pad, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 22.0;
            let (art, _) = ui.allocate_exact_size(vec2(132.0, 132.0), Sense::hover());
            cover_image(
                ui,
                covers,
                art,
                covers.icon(&show.image_url),
                &show.title,
                10,
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let w = ui.available_width().min(620.0);
                ui.set_max_width(w);
                crate::widgets::caps_label(ui, &aulos_core::fl!("podcasts-show-caps"));
                crate::widgets::label_text(ui, &show.title, theme::semibold(24.0), cx.pal.text);
                let mut meta = show.author.clone();
                if let Some(cadence) = show.cadence {
                    if !meta.is_empty() {
                        meta.push_str(" · ");
                    }
                    meta.push_str(&cadence_label(cadence));
                }
                if !meta.is_empty() {
                    meta.push_str(" · ");
                }
                meta.push_str(&aulos_core::fl!(
                    "podcast-episode-count",
                    count = pod.episodes.len()
                ));
                if pod.refreshing.contains(&show.id) {
                    meta.push_str(" · ");
                    meta.push_str(&aulos_core::fl!("podcast-refreshing"));
                }
                crate::widgets::label_text(ui, &meta, theme::regular(13.0), cx.pal.text_2);
                if !st.desc.1.is_empty() {
                    description(ui, cx, st, w);
                }
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if let Some((ep, resume)) = primary_episode(&pod.episodes) {
                        let label = if resume {
                            aulos_core::fl!("podcasts-continue")
                        } else {
                            aulos_core::fl!("podcasts-latest")
                        };
                        if pill_button(
                            ui,
                            &label,
                            Some(regular::PLAY),
                            PillKind::Primary,
                            btn_h(ui, 30.0),
                            0.0,
                        )
                        .clicked()
                        {
                            intents.push(Intent::Play(ep.id));
                        }
                    }
                    if st.pending_unsub == Some(show.id) {
                        if pill_button(
                            ui,
                            &aulos_core::fl!("podcast-unsubscribe"),
                            Some(regular::TRASH),
                            PillKind::Danger,
                            btn_h(ui, 30.0),
                            0.0,
                        )
                        .on_hover_text(aulos_core::fl!("podcasts-unsubscribe-hover"))
                        .clicked()
                        {
                            intents.push(Intent::Unsubscribe(show.id));
                        }
                        if pill_button(
                            ui,
                            &aulos_core::fl!("podcast-cancel"),
                            None,
                            PillKind::Ghost,
                            btn_h(ui, 30.0),
                            0.0,
                        )
                        .clicked()
                        {
                            st.pending_unsub = None;
                        }
                    } else if pill_button(
                        ui,
                        &aulos_core::fl!("podcast-subscribed-badge"),
                        Some(regular::CHECK),
                        PillKind::Secondary,
                        btn_h(ui, 30.0),
                        0.0,
                    )
                    .clicked()
                    {
                        st.pending_unsub = Some(show.id);
                    }
                    if IconBtn::new(regular::ARROW_CLOCKWISE)
                        .size(btn_h(ui, 30.0))
                        .icon_size(16.0)
                        .tip(&aulos_core::fl!("podcasts-refresh-this-feed"))
                        .show(ui)
                        .clicked()
                    {
                        intents.push(Intent::Refresh(show.id));
                    }
                });
            });
        });
    });
}

/// The show description, clamped to three lines until clicked.
fn description(ui: &mut Ui, cx: &Ctx, st: &mut State, width: f32) {
    let mut job = egui::text::LayoutJob::single_section(
        st.desc.1.clone(),
        egui::text::TextFormat {
            font_id: theme::regular(13.0),
            color: cx.pal.text_15,
            line_height: Some(19.0),
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: width,
        max_rows: if st.desc_open { usize::MAX } else { 3 },
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
    ui.painter().galley(rect.min, galley, cx.pal.text_15);
    if resp.clicked() {
        st.desc_open = !st.desc_open;
    }
}

fn episode_row(
    ui: &mut Ui,
    cx: &Ctx,
    pod: &PodcastsState,
    e: &Episode,
    playing: Option<(i64, bool)>,
    now: i64,
    intents: &mut Vec<Intent>,
) {
    let (full, _) = ui.allocate_exact_size(vec2(ui.available_width(), EP_ROW), Sense::hover());
    let rect = full.shrink2(vec2(cx.pad, 0.0));
    let resp = ui.interact(rect, ui.id().with(("ep", e.id)), Sense::click());
    let pal = &cx.pal;
    let current = playing.filter(|(id, _)| *id == e.id);
    let download = pod.downloading.get(&e.id);
    let downloading = download.is_some();
    let downloaded = !e.downloaded_path.is_empty();
    let started = !e.played && e.position_ms > 0;
    let fresh = !e.played && e.position_ms == 0 && e.pub_date > 0 && now - e.pub_date < 14 * 86_400;

    let play_rect = Rect::from_center_size(
        pos2(rect.left() + 8.0 + 18.0, rect.center().y),
        vec2(36.0, 36.0),
    );
    let dl_rect = Rect::from_center_size(
        pos2(rect.right() - 8.0 - 18.0, rect.center().y),
        vec2(36.0, 36.0),
    );
    let play_resp = ui.interact(play_rect, ui.id().with(("ep-play", e.id)), Sense::click());
    let dl_resp = ui.interact(dl_rect, ui.id().with(("ep-dl", e.id)), Sense::click());

    let p = ui.painter();
    if resp.hovered() || play_resp.hovered() || dl_resp.hovered() {
        p.rect_filled(
            rect.shrink2(vec2(0.0, 1.0)),
            CornerRadius::same(8),
            pal.hover,
        );
    }
    p.hline(
        rect.x_range(),
        rect.top() + 0.5,
        Stroke::new(1.0, pal.stroke.alpha(0.6)),
    );

    // play disc
    let (disc, fg, glyph, filled) = if let Some((_, is_playing)) = current {
        (
            cx.accent.fill,
            cx.accent.on_fill,
            if is_playing { fill::PAUSE } else { fill::PLAY },
            true,
        )
    } else if e.played {
        (
            egui::Color32::TRANSPARENT,
            pal.text_3,
            regular::ARROW_COUNTER_CLOCKWISE,
            false,
        )
    } else {
        (pal.surface_3, pal.text, fill::PLAY, true)
    };
    p.circle_filled(play_rect.center(), 18.0, disc);
    paint_icon(p, play_rect.center(), glyph, 14.0, fg, filled);

    // columns
    let wide = rect.width() > 560.0;
    let status_w = if wide { 160.0 } else { 0.0 };
    let dl_left = dl_rect.left() - 14.0;
    let status_l = if wide { dl_left - status_w } else { dl_left };
    let text_l = play_rect.right() + 14.0;
    let text_r = (status_l - 14.0).max(text_l + 40.0);

    let mut title_x = text_l;
    let title_y = rect.center().y - 10.0;
    if fresh {
        let w = paint_badge(
            p,
            pos2(title_x, title_y),
            &aulos_core::fl!("podcasts-new-badge"),
            cx.accent.on_fill,
            pal.reed,
        );
        title_x += w + 8.0;
    }
    let title_color = if current.is_some() {
        cx.accent.text
    } else if e.played {
        pal.text_2
    } else {
        pal.text
    };
    let title_rect = Rect::from_min_max(
        pos2(title_x, rect.center().y - 19.0),
        pos2(text_r.max(title_x + 20.0), rect.center().y - 1.0),
    );
    paint_text(
        p,
        title_rect,
        Align::Min,
        &e.title,
        theme::semibold(13.0),
        title_color,
    );
    let mut meta = e.formatted_pub_date();
    if e.duration_secs > 0 {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&fmt_dur(e.duration_secs));
    }
    let meta_rect = Rect::from_min_max(
        pos2(text_l, rect.center().y + 2.0),
        pos2(text_r, rect.center().y + 18.0),
    );
    paint_text(
        p,
        meta_rect,
        Align::Min,
        &meta,
        theme::regular(12.0),
        pal.text_2,
    );

    if wide {
        let (status, color) = if let Some(dl) = download {
            let text = match dl.fraction() {
                _ if dl.is_cancelling() => aulos_core::fl!("podcasts-ep-cancelling"),
                Some(f) => aulos_core::fl!(
                    "podcasts-ep-downloading-pct",
                    pct = ((f * 100.0).floor() as u32)
                ),
                None => aulos_core::fl!("podcasts-ep-downloading"),
            };
            (text, pal.reed)
        } else if e.played {
            (aulos_core::fl!("podcasts-ep-played"), pal.text_3)
        } else if started {
            let left = (e.duration_secs - e.position_ms / 1000).max(0);
            (
                if e.duration_secs > 0 {
                    aulos_core::fl!("podcast-time-left", time = fmt_dur(left))
                } else {
                    aulos_core::fl!("podcasts-ep-in-progress")
                },
                cx.accent.text,
            )
        } else if e.duration_secs > 0 {
            (
                aulos_core::fl!("podcasts-ep-unplayed-dur", time = fmt_dur(e.duration_secs)),
                pal.text_2,
            )
        } else {
            (aulos_core::fl!("podcasts-ep-unplayed"), pal.text_2)
        };
        let status_rect = Rect::from_min_max(
            pos2(status_l, rect.center().y - 15.0),
            pos2(status_l + status_w, rect.center().y + 1.0),
        );
        paint_text(
            p,
            status_rect,
            Align::Min,
            &status,
            theme::mono(11.5),
            color,
        );
        if downloading || started || e.played {
            let bar =
                Rect::from_min_size(pos2(status_l, rect.center().y + 7.0), vec2(status_w, 3.0));
            p.rect_filled(bar, CornerRadius::same(2), pal.track);
            let (frac, color) = if let Some(dl) = download {
                (dl.fraction().unwrap_or(0.0), pal.reed)
            } else if e.played {
                (e.progress_fraction(), pal.text_5)
            } else {
                (e.progress_fraction(), cx.accent.fill)
            };
            let fill_rect = Rect::from_min_size(bar.min, vec2(bar.width() * frac, 3.0));
            p.rect_filled(fill_rect, CornerRadius::same(2), color);
        }
    }

    // download column
    let (dl_glyph, dl_color, dl_fill, dl_tip) = if downloading {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        (
            regular::X_CIRCLE,
            if dl_resp.hovered() {
                pal.text
            } else {
                pal.reed
            },
            false,
            aulos_core::fl!("podcasts-cancel-download"),
        )
    } else if downloaded {
        (
            fill::CHECK_CIRCLE,
            pal.text_2,
            true,
            aulos_core::fl!("podcasts-downloaded-remove"),
        )
    } else {
        (
            regular::DOWNLOAD_SIMPLE,
            if dl_resp.hovered() {
                pal.text
            } else {
                pal.text_3
            },
            false,
            aulos_core::fl!("podcasts-download-offline"),
        )
    };
    ui.painter().circle_filled(
        dl_rect.center(),
        if dl_resp.hovered() { 16.0 } else { 0.0 },
        pal.text.alpha(0.06),
    );
    paint_icon(
        ui.painter(),
        dl_rect.center(),
        dl_glyph,
        17.0,
        dl_color,
        dl_fill,
    );
    let dl_resp = dl_resp.on_hover_text(dl_tip);

    if play_resp.clicked() || (resp.clicked() && !dl_resp.clicked()) {
        intents.push(Intent::Play(e.id));
    }
    if dl_resp.clicked() {
        if downloading {
            intents.push(Intent::CancelDownload(e.id));
        } else if downloaded {
            intents.push(Intent::DeleteDownload(e.id));
        } else {
            intents.push(Intent::Download(e.id));
        }
    }
    resp.context_menu(|ui| {
        if ui
            .button(if current.is_some() {
                aulos_core::fl!("podcasts-play-pause")
            } else {
                aulos_core::fl!("play")
            })
            .clicked()
        {
            intents.push(Intent::Play(e.id));
            ui.close();
        }
        if ui
            .button(if e.played {
                aulos_core::fl!("podcasts-mark-unplayed")
            } else {
                aulos_core::fl!("podcasts-mark-played")
            })
            .clicked()
        {
            intents.push(Intent::TogglePlayed(e.id));
            ui.close();
        }
        if downloading {
            if ui
                .button(aulos_core::fl!("podcasts-cancel-download"))
                .clicked()
            {
                intents.push(Intent::CancelDownload(e.id));
                ui.close();
            }
        } else if downloaded {
            if ui
                .button(aulos_core::fl!("podcasts-remove-download"))
                .clicked()
            {
                intents.push(Intent::DeleteDownload(e.id));
                ui.close();
            }
        } else if ui.button(aulos_core::fl!("podcasts-download")).clicked() {
            intents.push(Intent::Download(e.id));
            ui.close();
        }
    });
}

/// A small mono badge with its top-left at `pos`; returns its width.
fn paint_badge(
    p: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    fg: egui::Color32,
    bg: egui::Color32,
) -> f32 {
    let galley = p.layout_no_wrap(text.to_owned(), theme::mono_semibold(9.5), fg);
    let size = galley.size() + vec2(8.0, 6.0);
    let rect = Rect::from_min_size(pos2(pos.x, pos.y - size.y / 2.0), size);
    p.rect_filled(rect, CornerRadius::same(3), bg);
    p.galley(rect.center() - galley.size() / 2.0, galley, fg);
    size.x
}

// -- discover ------------------------------------------------------------------

fn discover(
    ui: &mut Ui,
    cx: &Ctx,
    st: &mut State,
    pod: &PodcastsState,
    covers: &crate::widgets::Covers,
    intents: &mut Vec<Intent>,
) {
    padded(ui, cx.pad, |ui| {
        let width = 420.0_f32.min(ui.available_width());
        let id = ui.id().with("pod-search");
        let resp = search_field(
            ui,
            &mut st.search,
            &aulos_core::fl!("podcasts-search-hint"),
            width,
            id,
        );
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            intents.push(Intent::SubmitSearch);
        }
    });
    ui.add_space(14.0);
    if pod.search_loading {
        skeleton_rows(ui, cx, 6, 48.0, RESULT_ROW);
        ui.ctx().request_repaint_after(Duration::from_millis(60));
        return;
    }
    if let Some(err) = &pod.search_error {
        if empty_state_tinted(
            ui,
            regular::WARNING_CIRCLE,
            &aulos_core::fl!("podcasts-search-failed"),
            err,
            Some(aulos_core::fl!("podcast-retry").as_str()),
            Some(cx.pal.danger),
        ) {
            intents.push(Intent::SubmitSearch);
        }
        return;
    }
    if pod.search_results.is_empty() {
        if st.searched {
            empty_state(
                ui,
                regular::MAGNIFYING_GLASS,
                &aulos_core::fl!("podcast-no-results"),
                &aulos_core::fl!("podcasts-no-shows-body"),
                None,
            );
        } else {
            empty_state(
                ui,
                regular::MICROPHONE_STAGE,
                &aulos_core::fl!("podcasts-discover-title"),
                &aulos_core::fl!("podcasts-discover-body"),
                None,
            );
        }
        return;
    }
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, RESULT_ROW, pod.search_results.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for r in &pod.search_results[range] {
                let (full, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), RESULT_ROW), Sense::hover());
                let rect = full.shrink2(vec2(cx.pad, 0.0));
                let p = ui.painter();
                if ui.rect_contains_pointer(rect) {
                    p.rect_filled(
                        rect.shrink2(vec2(0.0, 2.0)),
                        CornerRadius::same(10),
                        cx.pal.hover,
                    );
                }
                let art = Rect::from_min_size(
                    pos2(rect.left() + 8.0, rect.center().y - 24.0),
                    vec2(48.0, 48.0),
                );
                cover_image(ui, covers, art, covers.icon(&r.image), &r.title, 8);
                let btn_w = 130.0;
                let text_l = art.right() + 14.0;
                let text_r = rect.right() - btn_w - 20.0;
                let p = ui.painter();
                let title = Rect::from_min_max(
                    pos2(text_l, rect.center().y - 19.0),
                    pos2(text_r, rect.center().y - 1.0),
                );
                paint_text(
                    p,
                    title,
                    Align::Min,
                    &r.title,
                    theme::semibold(13.5),
                    cx.pal.text,
                );
                let author = Rect::from_min_max(
                    pos2(text_l, rect.center().y + 2.0),
                    pos2(text_r, rect.center().y + 18.0),
                );
                paint_text(
                    p,
                    author,
                    Align::Min,
                    &r.author,
                    theme::regular(12.0),
                    cx.pal.text_2,
                );
                let btn = Rect::from_min_max(
                    pos2(rect.right() - 8.0 - btn_w, rect.center().y - 15.0),
                    pos2(rect.right() - 8.0, rect.center().y + 15.0),
                );
                let mut c = ui.new_child(
                    UiBuilder::new()
                        .max_rect(btn)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                if pod.podcasts.iter().any(|s| s.feed_url == r.feed_url) {
                    pill_button(
                        &mut c,
                        &aulos_core::fl!("podcast-subscribed-badge"),
                        Some(regular::CHECK),
                        PillKind::Secondary,
                        30.0,
                        0.0,
                    );
                } else if pill_button(
                    &mut c,
                    &aulos_core::fl!("subscribe"),
                    Some(regular::PLUS),
                    PillKind::Primary,
                    30.0,
                    0.0,
                )
                .clicked()
                {
                    intents.push(Intent::Subscribe(r.feed_url.clone()));
                }
            }
        });
}

// -- shared bits ---------------------------------------------------------------

/// Loading placeholders: `n` rows of `art`-sized discs/blocks and two text bars.
fn skeleton_rows(ui: &mut Ui, cx: &Ctx, n: usize, art: f32, row_h: f32) {
    padded(ui, cx.pad, |ui| {
        for i in 0..n {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
            let fade = 1.0 - i as f32 * 0.12;
            let rounding = if art <= 36.0 { 18 } else { 10 };
            let a = Rect::from_center_size(
                pos2(rect.left() + 8.0 + art / 2.0, rect.center().y),
                vec2(art, art),
            );
            let l = a.right() + 14.0;
            let w = (rect.right() - l).max(40.0);
            let line1 = Rect::from_min_size(pos2(l, rect.center().y - 14.0), vec2(w * 0.5, 12.0));
            let line2 = Rect::from_min_size(pos2(l, rect.center().y + 4.0), vec2(w * 0.28, 10.0));
            ui.scope(|ui| {
                ui.multiply_opacity(fade);
                skeleton(ui, a, rounding);
                skeleton(ui, line1, 4);
                skeleton(ui, line2, 4);
            });
        }
    });
}

fn cadence_label(cadence: Cadence) -> String {
    match cadence {
        Cadence::Daily => aulos_core::fl!("podcasts-cadence-daily"),
        Cadence::TwiceWeekly => aulos_core::fl!("podcasts-cadence-twice-weekly"),
        Cadence::Weekly => aulos_core::fl!("podcasts-cadence-weekly"),
        Cadence::Biweekly => aulos_core::fl!("podcasts-cadence-biweekly"),
        Cadence::Monthly => aulos_core::fl!("podcasts-cadence-monthly"),
    }
}

fn fmt_dur(secs: i64) -> String {
    let mins = secs / 60;
    if mins >= 60 {
        aulos_core::fl!(
            "podcasts-dur-hm",
            h = (mins / 60),
            m = format!("{:02}", mins % 60)
        )
    } else if mins > 0 {
        aulos_core::fl!("podcasts-dur-m", m = mins)
    } else {
        aulos_core::fl!("podcasts-dur-s", s = secs)
    }
}

/// Feed descriptions are often HTML: drop tags and decode the common entities.
fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// -- applying intents ------------------------------------------------------------

fn apply(app: &mut App, intent: Intent) {
    match intent {
        Intent::Select(id) => {
            let st = &mut app.views.podcasts;
            st.tab = TAB_SUBSCRIPTIONS;
            st.filter = 0;
            st.text.clear();
            st.desc_open = false;
            st.pending_unsub = None;
            st.selected_at = Some(Instant::now());
            let effect = app.session.podcasts.select(id);
            app.run(effect);
        }
        Intent::Back => {
            app.session.podcasts.close_show();
            let st = &mut app.views.podcasts;
            st.browsing_list = true;
            st.filter = 0;
            st.text.clear();
            st.desc_open = false;
            st.pending_unsub = None;
        }
        Intent::RetryRefreshes => {
            let effect = app.session.podcasts.retry_failed_refreshes();
            app.run(effect);
        }
        Intent::DismissRefreshErrors => app.session.podcasts.dismiss_refresh_errors(),
        Intent::CancelDownload(id) => app.session.podcasts.cancel_download(id),
        Intent::Refresh(id) => {
            let effect = app.session.podcasts.refresh(id);
            app.run(effect);
        }
        Intent::RefreshAll => {
            let effect = app.session.podcasts.refresh_all();
            app.run(effect);
        }
        Intent::AskUnsubscribe(id) => {
            apply(app, Intent::Select(id));
            app.views.podcasts.pending_unsub = Some(id);
        }
        Intent::Unsubscribe(id) => {
            app.views.podcasts.pending_unsub = None;
            // Land on the list, not on another show, after unsubscribing.
            app.views.podcasts.browsing_list = true;
            if let Some(effect) = app.session.podcasts.unsubscribe(id) {
                app.run(effect);
            }
        }
        Intent::Play(id) => play_episode(app, id),
        Intent::TogglePlayed(id) => {
            let effect = app.session.podcasts.toggle_played(id);
            app.run(effect);
        }
        Intent::Download(id) => {
            let effect = app.session.podcasts.download(id);
            app.run(effect);
        }
        Intent::DeleteDownload(id) => {
            let effect = app.session.podcasts.delete_download(id);
            app.run(effect);
        }
        Intent::SubmitSearch => {
            let query = app.views.podcasts.search.trim().to_owned();
            if query.is_empty() {
                return;
            }
            if query.starts_with("http://") || query.starts_with("https://") {
                apply(app, Intent::Subscribe(query));
                return;
            }
            app.views.podcasts.searched = true;
            let effect = app.session.podcasts.search(query);
            app.run(effect);
        }
        Intent::Subscribe(url) => match app.session.podcasts.subscribe(url) {
            Ok(effect) => {
                app.run(effect);
                app.toast(aulos_core::fl!("podcasts-subscribing-toast"));
                let st = &mut app.views.podcasts;
                st.add_open = false;
                st.add_url.clear();
                st.add_error = None;
            }
            Err(UrlError::Empty) => {
                app.views.podcasts.add_error = Some(aulos_core::fl!("podcast-add-url-required"))
            }
            Err(UrlError::NotHttp) => {
                app.views.podcasts.add_error = Some(aulos_core::fl!("podcast-add-url-invalid"))
            }
        },
    }
}

/// Plays an episode of the open show (toggling it when already current),
/// resuming from its saved position.
fn play_episode(app: &mut App, id: i64) {
    let decision = app
        .session
        .podcasts
        .episode_playback(id, app.session.playback.current_track.as_ref());
    match decision {
        None => {}
        Some(EpisodePlayback::Toggle) => app.toggle_play(),
        Some(EpisodePlayback::Play {
            episode_id,
            track,
            resume,
        }) => {
            let play = app.session.play_tracks(vec![*track], 0);
            app.run(play);
            app.session.podcasts.start_episode(episode_id, resume);
            if let Some(at) = resume {
                let seek = app.session.seek_to(at);
                app.run(seek);
            }
        }
    }
}
