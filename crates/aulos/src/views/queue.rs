//! The Queue side sheet: now playing, up next, drag to reorder, and save as
//! a playlist. It floats over the right edge of the content area.

use super::{menus, overlays};
use crate::app::{App, Sheet};
use crate::theme::{self, Palette};
use crate::widgets::{
    PillKind, caps_label, cover_image, empty_state, icon_button, paint_icon, paint_text,
    pill_button, pill_button_enabled,
};
use aulos_core::format::format_duration_coarse;
use aulos_core::library::Track;
use aulos_core::player::queue::{QueueGroup, QueueOrigin};
use egui::{
    Align, Area, Context, CornerRadius, Frame, Id, Key, Layout, Order, Rect, RichText, ScrollArea,
    Sense, Stroke, Ui, pos2, vec2,
};
use egui_phosphor::regular;

const HEADER_H: f32 = 52.0;
const FOOTER_H: f32 = 52.0;
const NOW_H: f32 = 56.0;
const ROW_H: f32 = 46.0;

#[derive(Default)]
pub struct State {
    /// Absolute queue index of the row being dragged.
    dragging: Option<usize>,
    /// Absolute insert slot under the pointer while dragging: the row the
    /// dragged item goes in front of, or the queue length for the end.
    drop_slot: Option<usize>,
    /// Absolute queue index and screen rect of every upcoming row drawn last
    /// frame (read by the smoke drag script).
    rows: Vec<(usize, Rect)>,
}

impl State {
    pub fn rows(&self) -> &[(usize, Rect)] {
        &self.rows
    }
}

enum Intent {
    Close,
    Jump(usize),
    Remove(usize),
    Move { from: usize, to: usize },
    Clear,
    Save,
}

pub fn show(app: &mut App, ctx: &Context) {
    if app.sheet != Some(Sheet::Queue) {
        theme::motion::slide_in(ctx, Id::new("aulos_queue_sheet_slide"), false);
        return;
    }
    let mut st = std::mem::take(&mut app.views.queue);
    let intents = draw(app, ctx, &mut st);
    app.views.queue = st;
    for intent in intents {
        apply(app, intent);
    }
}

fn draw(app: &App, ctx: &Context, st: &mut State) -> Vec<Intent> {
    let pal = app.pal;
    let mut out = Vec::new();
    if ctx.input(|i| i.key_pressed(Key::Escape)) && !overlays::active(app) {
        out.push(Intent::Close);
    }

    let player = app.session.playback.player.as_ref();
    let queue: &[Track] = player.map_or(&[], |p| p.queue());
    let idx = player.map_or(0, |p| p.queue_index());
    let has_now = idx < queue.len();
    let now = has_now.then(|| &queue[idx]);
    let up_start = if has_now { idx + 1 } else { 0 };
    let upcoming = &queue[up_start.min(queue.len())..];
    let groups = player.map_or_else(Vec::new, |p| p.queue_groups());
    let context_label = player.and_then(|p| p.queue_context_label());
    let total: u64 = queue.iter().map(|t| t.duration.as_secs()).sum();
    let meta = if queue.is_empty() {
        aulos_core::fl!("queue-empty-meta")
    } else {
        format!("{} · {}", queue.len(), format_duration_coarse(total))
    };

    let content = ctx.content_rect();
    let width = if app.narrow { 340.0 } else { 360.0 };
    let top = content.top() + theme::size::TOOLBAR;
    let bottom = content.bottom() - theme::size::NOW_BAR - 8.0;
    let height = (bottom - top).max(0.0);
    // Slides in from the right edge.
    let slide = theme::motion::slide_in(ctx, Id::new("aulos_queue_sheet_slide"), true);
    let origin = pos2(
        content.right() - 8.0 - width + (1.0 - slide) * (width + 16.0),
        top,
    );
    let footer_h = if queue.is_empty() { 0.0 } else { FOOTER_H };
    let body_h = (height - 2.0 - HEADER_H - footer_h).max(0.0);

    Area::new(Id::new("aulos_queue_sheet"))
        .order(Order::Foreground)
        .fixed_pos(origin)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.set_height(height);
            Frame::new()
                .fill(pal.surface_1)
                .stroke(Stroke::new(1.0, pal.track))
                .corner_radius(CornerRadius::same(12))
                .shadow(theme::shadow([-12, 0], 40, 102))
                .show(ui, |ui| {
                    ui.set_width(width - 2.0);
                    ui.set_height(height - 2.0);
                    // Header, body and footer stack flush: spacing would push
                    // the footer under the now bar.
                    ui.spacing_mut().item_spacing.y = 0.0;
                    header(ui, pal, &meta, !upcoming.is_empty(), &mut out);
                    ScrollArea::vertical()
                        .max_height(body_h)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            body(
                                ui,
                                app,
                                st,
                                now,
                                upcoming,
                                up_start,
                                (&groups, context_label),
                                &mut out,
                            );
                        });
                    if !queue.is_empty() {
                        footer(ui, pal, &mut out);
                    }
                });
        });
    out
}

fn header(ui: &mut Ui, pal: Palette, meta: &str, can_clear: bool, out: &mut Vec<Intent>) {
    let r = ui.horizontal(|ui| {
        ui.set_height(HEADER_H);
        ui.add_space(16.0);
        ui.label(
            RichText::new(aulos_core::fl!("queue-title"))
                .font(theme::semibold(15.0))
                .color(pal.text),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(meta)
                .font(theme::regular(12.0))
                .color(pal.text_2),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            if icon_button(ui, regular::X, 28.0).clicked() {
                out.push(Intent::Close);
            }
            ui.add_space(4.0);
            if pill_button_enabled(
                ui,
                &aulos_core::fl!("queue-clear"),
                None,
                PillKind::Ghost,
                28.0,
                0.0,
                can_clear,
            )
            .clicked()
            {
                out.push(Intent::Clear);
            }
        });
    });
    let rect = r.response.rect;
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, pal.stroke),
    );
}

fn footer(ui: &mut Ui, pal: Palette, out: &mut Vec<Intent>) {
    let r = ui.horizontal(|ui| {
        ui.set_height(FOOTER_H);
        ui.add_space(16.0);
        pill_button(
            ui,
            &aulos_core::fl!("queue-save-as-playlist"),
            Some(regular::PLAYLIST),
            PillKind::Secondary,
            30.0,
            0.0,
        )
    });
    if r.inner.clicked() {
        out.push(Intent::Save);
    }
    let rect = r.response.rect;
    ui.painter().hline(
        rect.x_range(),
        rect.top() + 0.5,
        Stroke::new(1.0, pal.stroke),
    );
}

#[allow(clippy::too_many_arguments)]
fn body(
    ui: &mut Ui,
    app: &App,
    st: &mut State,
    now: Option<&Track>,
    upcoming: &[Track],
    up_start: usize,
    (groups, context_label): (&[QueueGroup], Option<&str>),
    out: &mut Vec<Intent>,
) {
    ui.add_space(8.0);
    if now.is_none() && upcoming.is_empty() {
        empty_state(
            ui,
            regular::LIST_PLUS,
            &aulos_core::fl!("queue-empty-title"),
            &aulos_core::fl!("queue-empty-hint"),
            None,
        );
        return;
    }
    if let Some(track) = now {
        group_label(ui, &aulos_core::fl!("queue-group-now-playing"));
        now_row(ui, app, track);
        ui.add_space(8.0);
    }
    if upcoming.is_empty() {
        empty_state(
            ui,
            regular::ARROW_BEND_DOWN_RIGHT,
            &aulos_core::fl!("queue-nothing-next-title"),
            &aulos_core::fl!("queue-nothing-next-hint"),
            None,
        );
        return;
    }
    let mut rows = Vec::with_capacity(upcoming.len());
    for group in groups {
        group_label(ui, &group_title(group, context_label));
        for abs in group.start..group.start + group.len {
            let Some(track) = abs.checked_sub(up_start).and_then(|n| upcoming.get(n)) else {
                continue;
            };
            let rect = up_row(ui, app, st, track, abs, out);
            rows.push((abs, rect));
        }
    }

    st.rows.clone_from(&rows);
    // Drop slot and indicator, from this frame's row rects.
    let pointer_y = ui.ctx().input(|i| i.pointer.interact_pos()).map(|p| p.y);
    st.drop_slot = match (st.dragging, pointer_y) {
        (Some(_), Some(y)) => Some(
            rows.iter()
                .find(|(_, r)| y < r.center().y)
                .map_or(up_start + upcoming.len(), |(i, _)| *i),
        ),
        _ => None,
    };
    if let (Some(_), Some(slot)) = (st.dragging, st.drop_slot) {
        let y = rows
            .iter()
            .find(|(i, _)| *i == slot)
            .map(|(_, r)| r.top())
            .or_else(|| rows.last().map(|(_, r)| r.bottom()));
        if let (Some(y), Some((_, first))) = (y, rows.first()) {
            ui.painter().hline(
                first.left() + 8.0..=first.right(),
                y,
                Stroke::new(2.0, app.accent.fill),
            );
        }
    }
    ui.add_space(8.0);
}

/// Heading of a run of upcoming rows: `PLAYING NEXT`, `FROM <ALBUM> · 9 LEFT`
/// (`UP NEXT` when the queue was not started from a single album) or
/// `ADDED TO QUEUE`.
fn group_title(group: &QueueGroup, context_label: Option<&str>) -> String {
    match (group.origin, context_label) {
        (QueueOrigin::PlayNext, _) => aulos_core::fl!("queue-group-playing-next"),
        (QueueOrigin::Added, _) => aulos_core::fl!("queue-group-added"),
        (QueueOrigin::Context, Some(album)) => {
            aulos_core::fl!(
                "queue-group-from-album",
                album = album.to_uppercase(),
                count = group.len
            )
        }
        (QueueOrigin::Context, None) => aulos_core::fl!("queue-group-up-next"),
    }
}

fn group_label(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        caps_label(ui, text);
    });
}

/// The playing track: tinted panel, cover, title and artist, and a static
/// equaliser glyph on the right.
fn now_row(ui: &mut Ui, app: &App, track: &Track) -> Rect {
    let pal = app.pal;
    let acc = app.accent;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let width = ui.available_width() - 8.0;
        let (rect, _) = ui.allocate_exact_size(vec2(width, NOW_H), Sense::hover());
        let cy = rect.center().y;
        let cover_rect = Rect::from_min_size(pos2(rect.left() + 8.0, cy - 20.0), vec2(40.0, 40.0));
        let key = track.cover_key();
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), acc.row_tint);
        cover_image(ui, &app.covers, cover_rect, app.covers.thumb(&key), &key, 5);

        let bars_left = rect.right() - 14.0 - 11.0;
        let p = ui.painter();
        for (n, h) in [10.0_f32, 15.0, 7.0].into_iter().enumerate() {
            let x = bars_left + n as f32 * 5.0;
            p.rect_filled(
                Rect::from_min_max(pos2(x, cy + 7.0 - h), pos2(x + 3.0, cy + 7.0)),
                CornerRadius::same(1),
                acc.fill,
            );
        }
        let x0 = cover_rect.right() + 10.0;
        let x1 = bars_left - 8.0;
        paint_text(
            p,
            Rect::from_min_max(pos2(x0, cy - 18.0), pos2(x1, cy)),
            Align::Min,
            &track.title,
            theme::semibold(13.0),
            acc.text,
        );
        paint_text(
            p,
            Rect::from_min_max(pos2(x0, cy + 1.0), pos2(x1, cy + 18.0)),
            Align::Min,
            track.grouping_artist(),
            theme::regular(11.5),
            pal.text_2,
        );
        rect
    })
    .inner
}

/// An upcoming row: grip, cover, title and artist, duration, and remove.
/// Click jumps to it; dragging moves it.
fn up_row(
    ui: &mut Ui,
    app: &App,
    st: &mut State,
    track: &Track,
    abs: usize,
    out: &mut Vec<Intent>,
) -> Rect {
    let pal = app.pal;
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        let width = ui.available_width() - 8.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click_and_drag());

        if resp.drag_started() {
            st.dragging = Some(abs);
        }
        if resp.drag_stopped()
            && let (Some(from), Some(slot)) = (st.dragging.take(), st.drop_slot.take())
            && let Some(to) = super::drop_target(from, slot)
        {
            out.push(Intent::Move { from, to });
        }
        if resp.clicked() {
            out.push(Intent::Jump(abs));
        }

        let cy = rect.center().y;
        let x_rect = Rect::from_center_size(pos2(rect.right() - 18.0, cy), vec2(24.0, 24.0));
        let remove = ui.interact(x_rect, Id::new(("aulos_queue_remove", abs)), Sense::click());
        if remove.clicked() {
            out.push(Intent::Remove(abs));
        }

        let hovered = resp.hovered() && st.dragging.is_none();
        let key = track.cover_key();
        let cover_rect = Rect::from_min_size(pos2(rect.left() + 26.0, cy - 18.0), vec2(36.0, 36.0));
        if hovered || st.dragging == Some(abs) {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), pal.seg_sel);
        }
        cover_image(ui, &app.covers, cover_rect, app.covers.thumb(&key), &key, 4);

        let p = ui.painter();
        // Grip and remove show on hover only.
        if hovered || st.dragging == Some(abs) || remove.hovered() {
            paint_icon(
                p,
                pos2(rect.left() + 10.0, cy),
                regular::DOTS_SIX_VERTICAL,
                14.0,
                pal.text_3,
                false,
            );
            paint_icon(
                p,
                x_rect.center(),
                regular::X,
                13.0,
                if remove.hovered() {
                    pal.text
                } else {
                    pal.text_3
                },
                false,
            );
        }
        paint_text(
            p,
            Rect::from_min_max(
                pos2(rect.right() - 90.0, cy - 8.0),
                pos2(rect.right() - 36.0, cy + 8.0),
            ),
            Align::Max,
            &track.duration_string(),
            theme::mono(11.0),
            pal.text_2,
        );
        let x0 = cover_rect.right() + 10.0;
        let x1 = rect.right() - 96.0;
        paint_text(
            p,
            Rect::from_min_max(pos2(x0, cy - 17.0), pos2(x1, cy)),
            Align::Min,
            &track.title,
            theme::regular(13.0),
            pal.text,
        );
        paint_text(
            p,
            Rect::from_min_max(pos2(x0, cy), pos2(x1, cy + 16.0)),
            Align::Min,
            track.grouping_artist(),
            theme::regular(11.5),
            pal.text_2,
        );
        rect
    })
    .inner
}

fn apply(app: &mut App, intent: Intent) {
    match intent {
        Intent::Close => app.sheet = None,
        Intent::Jump(index) => {
            let update = app.session.queue_jump(index);
            app.run(update);
        }
        Intent::Remove(index) => {
            let update = app.session.queue_remove(index);
            app.run(update);
        }
        Intent::Move { from, to } => app.session.queue_move(from, to),
        Intent::Clear => app.session.queue_clear(),
        Intent::Save => {
            let uris: Vec<String> = app
                .session
                .playback
                .player
                .as_ref()
                .map(|p| p.queue().iter().map(|t| t.source_uri.clone()).collect())
                .unwrap_or_default();
            overlays::prompt(
                app,
                &aulos_core::fl!("queue-save-title"),
                &aulos_core::fl!("queue-save-name"),
                &aulos_core::fl!("queue-save-hint"),
                &aulos_core::fl!("queue-save-submit"),
                move |app, name| {
                    menus::save_as_playlist(app, name, uris);
                },
            );
        }
    }
}
