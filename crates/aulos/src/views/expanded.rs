//! The expanded now-playing view (replaces the whole window), plus the
//! backdrop / seek / transport helpers the mini player shares.

use crate::app::{App, Sheet};
use crate::theme::{self, ColorExt, Theme, hex};
use crate::widgets::{
    Covers, IconBtn, PillKind, cover_image, paint_icon, paint_text, paint_text_spaced, pill_button,
    slider_bar, text_width,
};
use aulos_core::config::RepeatMode;
use aulos_core::format::{format_duration, format_duration_coarse};
use aulos_core::library::{Lyrics, Track};
use egui::{
    Align, Color32, CornerRadius, FontId, Mesh, Rect, Sense, Shadow, Shape, Ui, UiBuilder, pos2,
    vec2,
};
use egui_phosphor::{fill, regular};
use std::time::Duration;

/// Secondary text on the dark backdrop.
const SUB: Color32 = hex(0xC9D2D6);
/// Caption text on the dark backdrop.
const CAPTION: Color32 = hex(0xB9C4C8);
/// Chip / button text on the dark backdrop.
const SOFT: Color32 = hex(0xD5DCDF);

pub struct State {
    /// Lyrics tab selected (otherwise "Up next"). Also set by the now bar's
    /// lyrics button.
    pub show_lyrics: bool,
    /// The track (id, source) lyrics were last requested for.
    lyrics_for: Option<(i64, String)>,
    /// The synced line the lyrics list last scrolled to.
    lyric_cur: Option<usize>,
    /// An online lookup was requested for `lyrics_for`.
    online_asked: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            show_lyrics: true,
            lyrics_for: None,
            lyric_cur: None,
            online_asked: false,
        }
    }
}

// -- shared helpers (also used by the mini player) ---------------------------

fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, (a * 255.0).round() as u8)
}

/// Paints the blurred cover (cover-cropped) over a dark base, then a vertical
/// veil from `top` to `bottom`.
pub(super) fn paint_backdrop(ui: &Ui, app: &App, rect: Rect, top: Color32, bottom: Color32) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, hex(0x061218));
    match app.backdrop.as_ref() {
        Some(tex) => {
            let [w, h] = tex.size();
            let (tw, th) = (w.max(1) as f32, h.max(1) as f32);
            let target = rect.width() / rect.height().max(1.0);
            let uv = if tw / th > target {
                let span = target * th / tw;
                Rect::from_min_max(pos2((1.0 - span) / 2.0, 0.0), pos2((1.0 + span) / 2.0, 1.0))
            } else {
                let span = tw / th / target;
                Rect::from_min_max(pos2(0.0, (1.0 - span) / 2.0), pos2(1.0, (1.0 + span) / 2.0))
            };
            let t = app
                .art_fade
                .progress(ui.ctx(), theme::motion::CROSSFADE_SLOW);
            if let (Some(prev), true) = (app.art_fade.backdrop.as_ref(), t < 1.0) {
                painter.image(prev.id(), rect, uv_cover(prev.size(), rect), Color32::WHITE);
            }
            painter.image(tex.id(), rect, uv, Color32::WHITE.gamma_multiply(t));
        }
        None => {
            painter.rect_filled(rect, 0.0, app.accent.fill.alpha(0.22));
        }
    }
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

fn uv_cover(size: [usize; 2], rect: Rect) -> Rect {
    let (tw, th) = (size[0].max(1) as f32, size[1].max(1) as f32);
    let target = rect.width() / rect.height().max(1.0);
    if tw / th > target {
        let span = target * th / tw;
        Rect::from_min_max(pos2((1.0 - span) / 2.0, 0.0), pos2((1.0 + span) / 2.0, 1.0))
    } else {
        let span = tw / th / target;
        Rect::from_min_max(pos2(0.0, (1.0 - span) / 2.0), pos2(1.0, (1.0 + span) / 2.0))
    }
}

/// Seek position labels and fraction of the current track.
pub(super) struct Progress {
    pub frac: f32,
    pub elapsed: String,
    pub remaining: String,
    pub enabled: bool,
}

pub(super) fn progress(app: &App) -> Progress {
    let track = app.session.playback.current_track.as_ref();
    let live = track.is_some_and(Track::is_live_stream);
    let duration = track.map_or(Duration::ZERO, |t| t.duration);
    let position = app.session.playback.position;
    let frac = match app.seek_preview {
        Some(f) => f,
        None if duration.is_zero() => 0.0,
        None => (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0),
    };
    let shown = Duration::from_secs_f32(frac * duration.as_secs_f32());
    let elapsed = if live {
        aulos_core::fl!("now-playing-live")
    } else {
        format_duration(shown.as_secs())
    };
    let remaining = if live || duration < Duration::from_secs(1) {
        String::new()
    } else {
        format!(
            "\u{2212}{}",
            format_duration(duration.saturating_sub(shown).as_secs())
        )
    };
    Progress {
        frac,
        elapsed,
        remaining,
        enabled: track.is_some() && !live && !duration.is_zero(),
    }
}

/// Draws a seek bar of `height` on the middle of `rect` (the hit area) over
/// the backdrop, and applies drag / release.
pub(super) fn seek_row(
    app: &mut App,
    ui: &mut Ui,
    rect: Rect,
    height: f32,
    enabled: bool,
    frac: f32,
    thumb: bool,
) {
    let t = Theme::get(ui.ctx());
    let resp = ui.interact(
        rect,
        ui.id().with(("seek-row", height as u32)),
        if enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let pressed = resp.dragged() || resp.is_pointer_button_down_on();
    let under_pointer = resp
        .interact_pointer_pos()
        .map(|p| ((p.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0));
    let at = under_pointer.filter(|_| pressed);
    let shown = at.unwrap_or(frac).clamp(0.0, 1.0);

    let rail = Rect::from_center_size(rect.center(), vec2(rect.width(), height));
    let round = CornerRadius::same((height / 2.0).ceil() as u8);
    ui.painter()
        .rect_filled(rail, round, Color32::WHITE.alpha(0.16));
    if shown > 0.0 {
        let done = Rect::from_min_size(rail.min, vec2((rail.width() * shown).max(height), height));
        ui.painter().rect_filled(done, round, t.accent.fill);
    }
    if thumb && enabled || resp.hovered() && enabled {
        let x = rail.left() + rail.width() * shown;
        let r = if thumb { 7.0 } else { height / 2.0 + 3.0 };
        ui.painter().circle_filled(
            pos2(x.clamp(rail.left() + r, rail.right() - r), rail.center().y),
            r,
            Color32::WHITE,
        );
    }

    app.seek_preview = at.filter(|_| enabled);
    if enabled && (resp.drag_stopped() || resp.clicked()) {
        let f = under_pointer.unwrap_or(frac);
        let u = app.session.seek_fraction(f);
        app.run(u);
        app.seek_preview = None;
    }
}

/// Geometry of a transport row: shuffle, previous, play disc, next, repeat.
pub(super) struct TransportStyle {
    pub side: f32,
    pub side_icon: f32,
    pub skip: f32,
    pub skip_icon: f32,
    pub disc: f32,
    pub disc_icon: f32,
    /// Fixed gap between the slots; `None` spreads them over the row.
    pub gap: Option<f32>,
    pub halo: bool,
}

pub(super) fn transport_row(app: &mut App, ui: &mut Ui, row: Rect, st: &TransportStyle) {
    let t = Theme::get(ui.ctx());
    let has_track = app.session.playback.current_track.is_some();
    let playing = app.session.playback.is_playing();
    let shuffle = app.session.config.config.shuffle;
    let repeat = app.session.config.config.repeat_mode;

    let sizes = [st.side, st.skip, st.disc, st.skip, st.side];
    let total: f32 = sizes.iter().sum();
    let gap = st.gap.unwrap_or((row.width() - total) / 4.0).max(0.0);
    let mut x = row.center().x - (total + gap * 4.0) / 2.0;
    let mut slots = [Rect::NOTHING; 5];
    for (slot, size) in slots.iter_mut().zip(sizes) {
        *slot = Rect::from_center_size(pos2(x + size / 2.0, row.center().y), vec2(size, size));
        x += size + gap;
    }

    let button = |ui: &mut Ui, slot: Rect, btn: IconBtn<'_>| {
        ui.scope_builder(UiBuilder::new().max_rect(slot), |ui| btn.show(ui))
            .inner
            .clicked()
    };

    if button(
        ui,
        slots[0],
        IconBtn::new(regular::SHUFFLE)
            .size(st.side)
            .icon_size(st.side_icon)
            .color(if shuffle { t.accent.text } else { SUB })
            .dot(shuffle)
            .tip(aulos_core::fl!("shuffle")),
    ) {
        let u = app.session.toggle_shuffle();
        app.run(u);
    }
    if button(
        ui,
        slots[1],
        IconBtn::new(fill::SKIP_BACK)
            .filled(true)
            .size(st.skip)
            .icon_size(st.skip_icon)
            .color(Color32::WHITE)
            .enabled(has_track)
            .tip(aulos_core::fl!("previous")),
    ) {
        let u = app.session.previous_track();
        app.run(u);
    }

    let resp = ui.interact(slots[2], ui.id().with("transport-disc"), Sense::click());
    let centre = slots[2].center();
    if st.halo {
        ui.painter()
            .circle_filled(centre, st.disc / 2.0 + 5.0, t.accent.fill.alpha(0.16));
    }
    let disc_fill = if resp.hovered() {
        t.accent.fill.gamma_multiply(0.92)
    } else {
        t.accent.fill
    };
    ui.painter().circle_filled(centre, st.disc / 2.0, disc_fill);
    paint_icon(
        ui.painter(),
        centre,
        if playing { fill::PAUSE } else { fill::PLAY },
        st.disc_icon,
        t.accent.on_fill,
        true,
    );
    if resp
        .on_hover_text(if playing {
            aulos_core::fl!("pause")
        } else {
            aulos_core::fl!("play")
        })
        .clicked()
    {
        app.toggle_play();
    }

    if button(
        ui,
        slots[3],
        IconBtn::new(fill::SKIP_FORWARD)
            .filled(true)
            .size(st.skip)
            .icon_size(st.skip_icon)
            .color(Color32::WHITE)
            .enabled(has_track)
            .tip(aulos_core::fl!("next")),
    ) {
        let u = app.session.next_track();
        app.run(u);
    }
    let (glyph, on) = match repeat {
        RepeatMode::None => (regular::REPEAT, false),
        RepeatMode::All => (regular::REPEAT, true),
        RepeatMode::One => (regular::REPEAT_ONCE, true),
    };
    if button(
        ui,
        slots[4],
        IconBtn::new(glyph)
            .size(st.side)
            .icon_size(st.side_icon)
            .color(if on { t.accent.text } else { SUB })
            .dot(on)
            .tip(aulos_core::fl!("repeat")),
    ) {
        let u = app.session.cycle_repeat();
        app.run(u);
    }
}

// -- the view ------------------------------------------------------------------

pub fn show(app: &mut App, ui: &mut Ui) {
    let rect = ui.max_rect();
    paint_backdrop(ui, app, rect, rgba(6, 18, 24, 0.6), rgba(6, 14, 18, 0.88));

    let track = app.session.playback.current_track.clone();
    header(app, ui, rect, track.as_ref());
    let Some(track) = track else {
        let mid = Rect::from_center_size(rect.center(), vec2(rect.width(), 40.0));
        paint_text(
            ui.painter(),
            mid,
            Align::Center,
            &aulos_core::fl!("expanded-nothing-playing"),
            theme::semibold(16.0),
            SUB,
        );
        return;
    };

    // Lyrics are looked up once per track, when the tab is showing.
    let key_matches = app
        .views
        .expanded
        .lyrics_for
        .as_ref()
        .is_some_and(|(id, src)| *id == track.id && *src == track.source_uri);
    if app.views.expanded.show_lyrics && !key_matches {
        app.views.expanded.lyrics_for = Some((track.id, track.source_uri.clone()));
        app.views.expanded.lyric_cur = None;
        app.views.expanded.online_asked = false;
        if app.session.playback.lyrics.is_none() {
            app.session.playback.load_embedded_lyrics();
        }
    }

    let pad = if rect.width() < 760.0 { 24.0 } else { 56.0 };
    let body = Rect::from_min_max(
        pos2(rect.left() + pad, rect.top() + 64.0),
        pos2(rect.right() - pad, rect.bottom() - 32.0),
    );
    if rect.width() < 760.0 {
        left_column(app, ui, tall(body), &track);
    } else {
        let col_w = (body.width() - 48.0) / 2.0;
        let left = Rect::from_min_size(body.min, vec2(col_w, body.height()));
        let right = Rect::from_min_size(
            pos2(body.left() + col_w + 48.0, body.top()),
            vec2(col_w, body.height()),
        );
        left_column(app, ui, tall(left), &track);
        right_column(app, ui, right, &track);
    }
    // The "…" menu's Properties opens the same sheet the library chrome draws.
    let ctx = ui.ctx().clone();
    super::queue::show(app, &ctx);
    super::properties::show(app, &ctx);
}

/// The left column may use the strip between the header and the body top, and
/// part of the bottom padding, so the 380pt cover fits shorter windows.
fn tall(body: Rect) -> Rect {
    Rect::from_min_max(
        pos2(body.left(), body.top() - 8.0),
        pos2(body.right(), body.bottom() + 16.0),
    )
}

fn header(app: &mut App, ui: &mut Ui, rect: Rect, track: Option<&Track>) {
    let btn = Rect::from_min_size(
        pos2(rect.left() + 20.0, rect.top() + 12.0),
        vec2(32.0, 32.0),
    );
    ui.painter()
        .rect_filled(btn, CornerRadius::same(8), Color32::WHITE.alpha(0.08));
    let collapse = ui.scope_builder(UiBuilder::new().max_rect(btn), |ui| {
        IconBtn::new(regular::CARET_DOWN)
            .size(32.0)
            .icon_size(16.0)
            .color(Color32::WHITE)
            .tip(aulos_core::fl!("expanded-collapse-tip"))
            .show(ui)
    });
    if collapse.inner.clicked() {
        app.expanded = false;
    }

    // One column (phones): the upcoming list is not on the page, so the
    // queue sheet gets a button here.
    if rect.width() < 760.0 {
        let q = Rect::from_min_size(pos2(btn.right() + 4.0, btn.top()), vec2(32.0, 32.0));
        let open = app.sheet == Some(Sheet::Queue);
        let queue = ui.scope_builder(UiBuilder::new().max_rect(q), |ui| {
            IconBtn::new(regular::QUEUE)
                .size(32.0)
                .icon_size(17.0)
                .active(open)
                .color(SOFT)
                .tip(aulos_core::fl!("nowbar-queue"))
                .show(ui)
        });
        if queue.inner.clicked() {
            app.sheet = if open { None } else { Some(Sheet::Queue) };
        }
    }

    let pip = Rect::from_min_size(
        pos2(rect.right() - 20.0 - 32.0, rect.top() + 12.0),
        vec2(32.0, 32.0),
    );
    let mini = ui.scope_builder(UiBuilder::new().max_rect(pip), |ui| {
        IconBtn::new(regular::PICTURE_IN_PICTURE)
            .size(32.0)
            .icon_size(17.0)
            .color(SOFT)
            .tip(aulos_core::fl!("mini-player"))
            .show(ui)
    });
    if mini.inner.clicked() {
        let ctx = ui.ctx().clone();
        app.expanded = false;
        app.set_mini(&ctx, true);
    }

    // The visualizer toggle sits left of the mini-player button.
    #[cfg(feature = "visualizer")]
    let buttons_left = if app.viz_available() {
        let viz = Rect::from_min_size(pos2(pip.left() - 4.0 - 32.0, pip.top()), vec2(32.0, 32.0));
        let toggle = ui.scope_builder(UiBuilder::new().max_rect(viz), |ui| {
            IconBtn::new(regular::WAVEFORM)
                .size(32.0)
                .icon_size(17.0)
                .color(SOFT)
                .tip(aulos_core::fl!("viz-toggle"))
                .show(ui)
        });
        if toggle.inner.clicked() {
            app.set_visualizer(true);
        }
        viz.left()
    } else {
        pip.left()
    };
    #[cfg(not(feature = "visualizer"))]
    let buttons_left = pip.left();
    #[cfg(feature = "visualizer")]
    let extra = 36.0;
    #[cfg(not(feature = "visualizer"))]
    let extra = 0.0;

    // Volume sits left of the buttons.
    let with_volume = rect.width() >= 640.0;
    let side = if with_volume {
        20.0 + 32.0 + extra + 8.0 + VOLUME_W + 30.0 + 12.0
    } else {
        20.0 + 32.0 + extra + 36.0 + 12.0
    };
    if with_volume {
        volume_control(app, ui, buttons_left - 8.0, rect.top() + 28.0);
    } else {
        volume_popover(app, ui, buttons_left - 4.0, rect.top() + 28.0);
    }

    let mid_w = (rect.width() - 2.0 * side).max(40.0);
    let heading = if track.is_some_and(|t| !t.album.is_empty()) {
        aulos_core::fl!("expanded-heading-album").to_uppercase()
    } else {
        aulos_core::fl!("expanded-heading-now-playing").to_uppercase()
    };
    let label = Rect::from_center_size(pos2(rect.center().x, rect.top() + 18.0), vec2(mid_w, 14.0));
    paint_text_spaced(
        ui.painter(),
        label,
        Align::Center,
        &heading,
        theme::semibold(10.5),
        CAPTION,
        1.05,
    );
    if let Some(track) = track.filter(|t| !t.album.is_empty()) {
        let name =
            Rect::from_center_size(pos2(rect.center().x, rect.top() + 34.0), vec2(mid_w, 18.0));
        let font = theme::semibold(13.0);
        if crate::widgets::text_width(ui, &track.album, font.clone()) <= name.width() {
            paint_text(
                ui.painter(),
                name,
                Align::Center,
                &track.album,
                font,
                Color32::WHITE,
            );
        } else {
            crate::widgets::marquee_text(
                ui,
                name,
                crate::widgets::marquee_id("expanded-album", &track.album),
                &track.album,
                font,
                Color32::WHITE,
                rgba(6, 14, 18, 0.75),
                true,
            );
        }
    }
}

const VOLUME_W: f32 = 96.0;

/// Mute button + volume slider whose right edge is `right`, centred on `cy`.
/// Narrow layouts: a speaker button opening a volume popover.
fn volume_popover(app: &mut App, ui: &mut Ui, right: f32, cy: f32) {
    let vol = app.volume();
    let muted = vol <= 0.001;
    let glyph = if muted {
        regular::SPEAKER_X
    } else if vol < 0.4 {
        regular::SPEAKER_LOW
    } else {
        regular::SPEAKER_HIGH
    };
    let t = Theme::get(ui.ctx());
    let rect = Rect::from_min_size(pos2(right - 32.0, cy - 16.0), vec2(32.0, 32.0));
    let resp = ui
        .scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            IconBtn::new(glyph)
                .size(32.0)
                .icon_size(17.0)
                .color(if muted { t.pal.danger } else { SOFT })
                .tip(aulos_core::fl!("volume"))
                .show(ui)
        })
        .inner;
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(150.0);
        ui.horizontal(|ui| {
            if IconBtn::new(glyph)
                .size(28.0)
                .icon_size(16.0)
                .color(if muted { t.pal.danger } else { t.pal.text_2 })
                .tip(if muted {
                    aulos_core::fl!("expanded-unmute")
                } else {
                    aulos_core::fl!("expanded-mute")
                })
                .show(ui)
                .clicked()
            {
                let u = app.session.toggle_mute();
                app.run(u);
            }
            let (r, new) = slider_bar(ui, app.volume(), vec2(110.0, 20.0), t.pal.text_1);
            if let Some(v) = new {
                let u = app.session.set_volume(v);
                app.run(u);
            }
            if r.drag_stopped() || r.clicked() {
                app.session.commit_volume();
            }
        });
    });
}

fn volume_control(app: &mut App, ui: &mut Ui, right: f32, cy: f32) {
    let vol = app.volume();
    let glyph = if vol <= 0.001 {
        regular::SPEAKER_X
    } else if vol < 0.4 {
        regular::SPEAKER_LOW
    } else {
        regular::SPEAKER_HIGH
    };
    let slider = Rect::from_min_size(pos2(right - VOLUME_W, cy - 10.0), vec2(VOLUME_W, 20.0));
    let mute = Rect::from_min_size(pos2(slider.left() - 30.0, cy - 14.0), vec2(28.0, 28.0));
    let clicked = ui
        .scope_builder(UiBuilder::new().max_rect(mute), |ui| {
            IconBtn::new(glyph)
                .size(28.0)
                .icon_size(16.0)
                .color(if vol <= 0.001 {
                    Theme::get(ui.ctx()).pal.danger
                } else {
                    SOFT
                })
                .tip(aulos_core::fl!("expanded-mute"))
                .show(ui)
        })
        .inner
        .clicked();
    if clicked {
        let u = app.session.toggle_mute();
        app.run(u);
    }
    let (resp, new) = ui
        .scope_builder(UiBuilder::new().max_rect(slider), |ui| {
            slider_bar(ui, vol, slider.size(), Color32::WHITE)
        })
        .inner;
    if let Some(v) = new {
        let u = app.session.set_volume(v);
        app.run(u);
    }
    if resp.drag_stopped() || resp.clicked() {
        app.session.commit_volume();
    }
}

/// A 10.5px mono chip at (`x`, `y`) of height 18; returns its width.
fn chip(
    ui: &Ui,
    at: egui::Pos2,
    text: &str,
    semibold: bool,
    fg: Color32,
    bg: Color32,
    border: Option<Color32>,
) -> f32 {
    let font: FontId = if semibold {
        theme::mono_semibold(10.5)
    } else {
        theme::mono(10.5)
    };
    let spacing = if semibold { 0.42 } else { 0.0 };
    let w = text_width(ui, text, font.clone()) + spacing * text.chars().count() as f32 + 12.0;
    let rect = Rect::from_min_size(at, vec2(w, 18.0));
    ui.painter().rect_filled(rect, CornerRadius::same(4), bg);
    if let Some(b) = border {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(4),
            egui::Stroke::new(1.0, b),
            egui::StrokeKind::Inside,
        );
    }
    paint_text_spaced(
        ui.painter(),
        rect.shrink2(vec2(6.0, 0.0)),
        Align::Min,
        text,
        font,
        fg,
        spacing,
    );
    w
}

fn left_column(app: &mut App, ui: &mut Ui, col: Rect, track: &Track) {
    const INFO_H: f32 = 203.0;
    let t = Theme::get(ui.ctx());
    // Title, seek bar and controls use one width (380pt, less in a narrow
    // column, never under 280pt); the square cover shrinks below it when the
    // window is too short, so the whole column always fits.
    let w = col.width().min(380.0).max(col.width().min(280.0));
    let mut title_job = egui::text::LayoutJob::simple(
        track.title.clone(),
        theme::semibold(26.0),
        Color32::WHITE,
        w - 68.0,
    );
    title_job.wrap.max_rows = 2;
    title_job.wrap.overflow_character = Some('\u{2026}');
    let title_galley = ui.painter().layout_job(title_job);
    // A two-row title pushes the info block down; the cover gives up that height.
    let extra = (title_galley.size().y - 30.0).max(0.0);
    let cover = w.min(col.height() - INFO_H - 22.0 - extra).max(64.0);
    let x0 = col.center().x - w / 2.0;
    let total = cover + 22.0 + INFO_H + extra;
    let top = (col.center().y - total / 2.0).max(col.top());

    // Cover.
    let cover_rect =
        Rect::from_center_size(pos2(col.center().x, top + cover / 2.0), vec2(cover, cover));
    ui.painter().add(
        Shadow {
            offset: [0, 30],
            blur: 60,
            spread: 0,
            color: Color32::from_black_alpha(140),
        }
        .as_shape(cover_rect, CornerRadius::same(10)),
    );
    let key = track.cover_key();
    let tex = app.cover_large.as_ref().or_else(|| app.covers.thumb(&key));
    cover_image(ui, &app.covers, cover_rect, tex, &key, 10);
    let fade = app.art_fade.progress(ui.ctx(), theme::motion::CROSSFADE);
    if let (Some(prev), true) = (app.art_fade.cover.as_ref(), fade < 1.0) {
        ui.painter().add(
            egui::epaint::RectShape::filled(
                cover_rect,
                CornerRadius::same(10),
                Color32::WHITE.gamma_multiply(1.0 - fade),
            )
            .with_texture(
                prev.id(),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            ),
        );
    }

    let info = top + cover + 22.0;

    // Title, subtitle, heart.
    ui.painter()
        .galley(pos2(x0, info), title_galley, Color32::WHITE);
    let year = (track.year > 0).then(|| track.year.to_string());
    let sub = [
        Some(track.artist.as_str()),
        Some(track.album.as_str()),
        year.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" \u{00B7} ");
    let sub_rect = Rect::from_min_size(pos2(x0, info + 33.0 + extra), vec2(w - 68.0, 20.0));
    crate::widgets::marquee_text(
        ui,
        sub_rect,
        crate::widgets::marquee_id("expanded-sub", &sub),
        &sub,
        theme::regular(14.0),
        SUB,
        rgba(6, 14, 18, 0.9),
        true,
    );

    let fav = app
        .session
        .library
        .favorite_state(Some(track), &track.key())
        .unwrap_or(track.is_favorite);
    let heart = Rect::from_min_size(pos2(x0 + w - 30.0, info + 28.0 + extra), vec2(30.0, 30.0));
    // Track menu ("…") left of the heart.
    let more = Rect::from_min_size(pos2(x0 + w - 64.0, info + 28.0 + extra), vec2(30.0, 30.0));
    let more_resp = ui
        .scope_builder(UiBuilder::new().max_rect(more), |ui| {
            IconBtn::new(regular::DOTS_THREE)
                .size(30.0)
                .icon_size(22.0)
                .color(SUB)
                .tip(aulos_core::fl!("expanded-more"))
                .show(ui)
        })
        .inner;
    if let Some(action) =
        super::menus::track_menu_click(&more_resp, track, &app.session.playlists.playlists)
    {
        super::menus::apply(app, action);
    }
    let hresp = ui.interact(heart, ui.id().with("expanded-heart"), Sense::click());
    let (glyph, color) = if fav {
        (fill::HEART, t.accent.fill)
    } else {
        (
            regular::HEART,
            if hresp.hovered() { Color32::WHITE } else { SUB },
        )
    };
    paint_icon(ui.painter(), heart.center(), glyph, 22.0, color, fav);
    if hresp
        .on_hover_text(if fav {
            aulos_core::fl!("favorite-remove")
        } else {
            aulos_core::fl!("favorite-add")
        })
        .clicked()
    {
        app.toggle_favorite(track);
    }

    // Quality chips.
    let chips_y = info + 67.0 + extra;
    let mut cx = x0;
    if let Some(q) = App::quality_of(track) {
        let dark = t.pal.dark;
        cx += chip(
            ui,
            pos2(cx, chips_y),
            q.label(),
            true,
            q.fg(dark),
            q.bg(dark),
            Some(q.border(dark)),
        ) + 6.0;
    }
    let fmt = track.format_chip();
    if !fmt.is_empty() {
        cx += chip(
            ui,
            pos2(cx, chips_y),
            &fmt,
            false,
            SOFT,
            Color32::WHITE.alpha(0.09),
            None,
        ) + 6.0;
    }
    if let Some(gain) = track.rg_track_gain.or(track.rg_album_gain) {
        chip(
            ui,
            pos2(cx, chips_y),
            &aulos_core::fl!(
                "expanded-replaygain-chip",
                gain = format!("{gain:+.1}").replace('-', "\u{2212}")
            ),
            false,
            SOFT,
            Color32::WHITE.alpha(0.09),
            None,
        );
    }

    // Seek bar and times.
    let prog = progress(app);
    let seek_rect = Rect::from_min_size(pos2(x0, info + 93.0 + extra), vec2(w, 18.0));
    seek_row(
        app,
        ui,
        seek_rect,
        6.0,
        prog.enabled,
        prog.frac,
        prog.enabled,
    );
    let times = Rect::from_min_size(pos2(x0, info + 111.0 + extra), vec2(w, 14.0));
    paint_text(
        ui.painter(),
        times,
        Align::Min,
        &prog.elapsed,
        theme::mono(11.5),
        SUB,
    );
    paint_text(
        ui.painter(),
        times,
        Align::Max,
        &prog.remaining,
        theme::mono(11.5),
        SUB,
    );

    // Transport.
    let row = Rect::from_min_size(pos2(x0, info + 139.0 + extra), vec2(w, 64.0));
    let style = TransportStyle {
        side: 40.0,
        side_icon: 20.0,
        skip: 44.0,
        skip_icon: 26.0,
        disc: 64.0,
        disc_icon: 28.0,
        gap: None,
        halo: true,
    };
    transport_row(app, ui, row, &style);
}

fn right_column(app: &mut App, ui: &mut Ui, col: Rect, track: &Track) {
    // Tabs.
    let labels = [
        aulos_core::fl!("lyrics"),
        aulos_core::fl!("expanded-up-next"),
    ];
    let widths = labels
        .each_ref()
        .map(|l| text_width(ui, l, theme::semibold(13.0)) + 28.0);
    let tabs = Rect::from_min_size(col.min, vec2(widths[0] + widths[1] + 4.0 + 6.0, 36.0));
    ui.painter()
        .rect_filled(tabs, CornerRadius::same(8), Color32::from_black_alpha(71));
    let mut x = tabs.left() + 3.0;
    for (i, label) in labels.iter().enumerate() {
        let item = Rect::from_min_size(pos2(x, tabs.top() + 3.0), vec2(widths[i], 30.0));
        let selected = app.views.expanded.show_lyrics == (i == 0);
        let resp = ui.interact(item, ui.id().with(("expanded-tab", i)), Sense::click());
        if selected {
            ui.painter()
                .rect_filled(item, CornerRadius::same(6), Color32::WHITE.alpha(0.14));
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(item, CornerRadius::same(6), Color32::WHITE.alpha(0.06));
        }
        let (font, color) = if selected {
            (theme::semibold(13.0), Color32::WHITE)
        } else {
            (theme::medium(13.0), SUB)
        };
        paint_text(ui.painter(), item, Align::Center, label, font, color);
        if resp.clicked() {
            app.views.expanded.show_lyrics = i == 0;
        }
        x += widths[i] + 4.0;
    }

    // Queue snapshot for the "up next" card and tab.
    let (queue, index) = match app.session.playback.player.as_ref() {
        Some(p) => (p.queue(), p.queue_index()),
        None => (&[][..], 0),
    };
    let upcoming = queue.get(index + 1..).unwrap_or(&[]);
    let shown = if app.views.expanded.show_lyrics {
        upcoming.len().min(3)
    } else {
        0
    };
    let card_h = if shown > 0 {
        12.0 + 20.0 + shown as f32 * 42.0 + 12.0
    } else {
        0.0
    };
    let content_bottom = if shown > 0 {
        col.bottom() - card_h - 16.0
    } else {
        col.bottom()
    };
    let content = Rect::from_min_max(
        pos2(col.left(), tabs.bottom() + 16.0),
        pos2(col.right(), content_bottom.max(tabs.bottom() + 16.0)),
    );

    let mut jump: Option<usize> = None;
    let mut action = LyricsAction::None;
    let duration_ms = track.duration.as_millis() as u64;

    ui.scope_builder(UiBuilder::new().max_rect(content), |ui| {
        ui.set_clip_rect(content.intersect(ui.clip_rect()));
        if app.views.expanded.show_lyrics {
            let pos_ms = app.session.playback.position.as_millis() as u64;
            let status = LyricsStatus {
                loading: app.session.playback.lyrics_loading,
                searched: app.views.expanded.online_asked,
                searchable: !track.is_live_stream(),
            };
            action = lyrics_view(
                ui,
                content,
                &app.session.playback.lyrics,
                status,
                pos_ms,
                &mut app.views.expanded.lyric_cur,
            );
        } else if upcoming.is_empty() {
            let mid = Rect::from_center_size(content.center(), vec2(content.width(), 40.0));
            paint_text(
                ui.painter(),
                mid,
                Align::Center,
                &aulos_core::fl!("expanded-nothing-queued"),
                theme::semibold(15.0),
                SUB,
            );
        } else {
            egui::ScrollArea::vertical()
                .id_salt("expanded-upnext")
                .auto_shrink([false, false])
                .show_rows(ui, 44.0, upcoming.len(), |ui, range| {
                    for i in range {
                        let w = ui.available_width();
                        if queue_row(ui, &app.covers, &upcoming[i], w, 44.0).clicked() {
                            jump = Some(index + 1 + i);
                        }
                    }
                });
        }
    });

    // The UP NEXT card.
    if shown > 0 {
        let card = Rect::from_min_size(
            pos2(col.left(), col.bottom() - card_h),
            vec2(col.width(), card_h),
        );
        ui.painter()
            .rect_filled(card, CornerRadius::same(12), Color32::from_black_alpha(77));
        let head = Rect::from_min_size(
            pos2(card.left() + 16.0, card.top() + 12.0),
            vec2(card.width() - 32.0, 14.0),
        );
        paint_text_spaced(
            ui.painter(),
            head,
            Align::Min,
            &aulos_core::fl!("expanded-up-next").to_uppercase(),
            theme::semibold(10.5),
            CAPTION,
            1.05,
        );
        let total: Duration = upcoming.iter().map(|t| t.duration).sum();
        let more = aulos_core::fl!(
            "expanded-more-queued",
            count = upcoming.len(),
            duration = format_duration_coarse(total.as_secs())
        );
        paint_text(
            ui.painter(),
            head,
            Align::Max,
            &more,
            theme::regular(12.0),
            CAPTION,
        );
        for (i, next) in upcoming.iter().take(shown).enumerate() {
            let row = Rect::from_min_size(
                pos2(card.left() + 12.0, card.top() + 32.0 + i as f32 * 42.0),
                vec2(card.width() - 24.0, 42.0),
            );
            let resp = ui
                .scope_builder(UiBuilder::new().max_rect(row), |ui| {
                    queue_row(ui, &app.covers, next, row.width(), 42.0)
                })
                .inner;
            if resp.clicked() {
                jump = Some(index + 1 + i);
            }
        }
    }

    if let Some(i) = jump {
        let u = app.session.queue_jump(i);
        app.run(u);
    }
    match action {
        LyricsAction::Seek(ms) if duration_ms > 0 => {
            let u = app
                .session
                .seek_fraction((ms as f32 / duration_ms as f32).clamp(0.0, 1.0));
            app.run(u);
        }
        LyricsAction::Search => {
            app.views.expanded.online_asked = true;
            let e = app.session.playback.fetch_lyrics_online();
            app.run(e);
        }
        LyricsAction::Seek(_) | LyricsAction::None => {}
    }
}

/// One queue row: cover, title / artist, duration.
fn queue_row(
    ui: &mut Ui,
    covers: &Covers,
    track: &Track,
    width: f32,
    height: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), Color32::WHITE.alpha(0.06));
    }
    let art = Rect::from_min_size(
        pos2(rect.left() + 4.0, rect.center().y - 16.0),
        vec2(32.0, 32.0),
    );
    let key = track.cover_key();
    cover_image(ui, covers, art, covers.thumb(&key), &key, 4);
    let dur = track.duration_string();
    let dur_w = text_width(ui, &dur, theme::mono(11.5)) + 4.0;
    let text_x = art.right() + 10.0;
    let text_w = (rect.right() - 4.0 - dur_w - 10.0 - text_x).max(10.0);
    let title = Rect::from_min_size(pos2(text_x, rect.center().y - 17.0), vec2(text_w, 18.0));
    let artist = Rect::from_min_size(pos2(text_x, rect.center().y + 1.0), vec2(text_w, 16.0));
    paint_text(
        ui.painter(),
        title,
        Align::Min,
        &track.title,
        theme::semibold(13.0),
        Color32::WHITE,
    );
    paint_text(
        ui.painter(),
        artist,
        Align::Min,
        &track.artist,
        theme::regular(12.0),
        CAPTION,
    );
    let dur_rect = Rect::from_min_size(
        pos2(rect.right() - 4.0 - dur_w, rect.top()),
        vec2(dur_w, rect.height()),
    );
    paint_text(
        ui.painter(),
        dur_rect,
        Align::Max,
        &dur,
        theme::mono(11.5),
        CAPTION,
    );
    resp
}

/// What the user did in the lyrics pane.
enum LyricsAction {
    None,
    /// Seek to a clicked synced line (ms).
    Seek(u64),
    /// Look the lyrics up online.
    Search,
}

/// Where the lyrics lookup stands.
#[derive(Clone, Copy)]
struct LyricsStatus {
    /// An online lookup is in flight.
    loading: bool,
    /// An online lookup already came back empty.
    searched: bool,
    /// The track can be looked up (not a live stream).
    searchable: bool,
}

/// The lyrics pane.
fn lyrics_view(
    ui: &mut Ui,
    area: Rect,
    lyrics: &Option<Lyrics>,
    status: LyricsStatus,
    pos_ms: u64,
    last_cur: &mut Option<usize>,
) -> LyricsAction {
    let mut action = LyricsAction::None;
    match lyrics {
        None => {
            let (title, hint) = if status.loading {
                (aulos_core::fl!("expanded-lyrics-searching"), String::new())
            } else if status.searched {
                (
                    aulos_core::fl!("expanded-lyrics-not-found"),
                    aulos_core::fl!("expanded-lyrics-not-found-hint"),
                )
            } else {
                (
                    aulos_core::fl!("expanded-lyrics-none"),
                    aulos_core::fl!("expanded-lyrics-none-hint"),
                )
            };
            let mid = Rect::from_center_size(area.center(), vec2(area.width(), 44.0));
            paint_text(
                ui.painter(),
                Rect::from_min_size(mid.min, vec2(mid.width(), 22.0)),
                Align::Center,
                &title,
                theme::semibold(16.0),
                SUB,
            );
            paint_text(
                ui.painter(),
                Rect::from_min_size(pos2(mid.left(), mid.top() + 24.0), vec2(mid.width(), 20.0)),
                Align::Center,
                &hint,
                theme::regular(13.0),
                CAPTION,
            );
            if !status.loading && !status.searched && status.searchable {
                let at = pos2(area.center().x, mid.bottom() + 28.0);
                let btn = Rect::from_center_size(at, vec2(132.0, 32.0));
                let clicked = ui
                    .scope_builder(UiBuilder::new().max_rect(btn), |ui| {
                        pill_button(
                            ui,
                            &aulos_core::fl!("expanded-search-online"),
                            Some(regular::MAGNIFYING_GLASS),
                            PillKind::Secondary,
                            32.0,
                            132.0,
                        )
                    })
                    .inner
                    .clicked();
                if clicked {
                    action = LyricsAction::Search;
                }
            }
        }
        Some(Lyrics::Synced(lines)) => {
            let cur = lines
                .partition_point(|l| l.timestamp_ms <= pos_ms)
                .checked_sub(1);
            egui::ScrollArea::vertical()
                .id_salt("expanded-lyrics")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 14.0;
                    ui.add_space(20.0);
                    for (i, line) in lines.iter().enumerate() {
                        let is_cur = Some(i) == cur;
                        let (size, color) = if is_cur {
                            (26.0, Color32::WHITE)
                        } else {
                            (22.0, Color32::WHITE.alpha(0.45))
                        };
                        let text = if line.text.is_empty() {
                            "\u{266A}"
                        } else {
                            line.text.as_str()
                        };
                        let resp = lyric_line(ui, area, text, theme::semibold(size), color);
                        if is_cur && *last_cur != Some(i) {
                            resp.scroll_to_me(Some(Align::Center));
                        }
                        if resp.clicked() {
                            action = LyricsAction::Seek(line.timestamp_ms);
                        }
                    }
                    ui.add_space(20.0);
                });
            *last_cur = cur;
        }
        Some(Lyrics::Unsynced(text)) => {
            egui::ScrollArea::vertical()
                .id_salt("expanded-lyrics")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.add_space(20.0);
                    for line in text.lines() {
                        if line.trim().is_empty() {
                            ui.add_space(12.0);
                        } else {
                            lyric_line(ui, area, line, theme::semibold(20.0), SOFT);
                        }
                    }
                    ui.add_space(20.0);
                });
        }
    }
    action
}

/// One wrapped lyric line, faded towards the top and bottom of `area`
/// (the design's 0 → 14% … 78% → 100% mask).
fn lyric_line(ui: &mut Ui, area: Rect, text: &str, font: FontId, color: Color32) -> egui::Response {
    let galley = ui
        .painter()
        .layout(text.to_owned(), font, color, ui.available_width());
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
    let t = ((rect.center().y - area.top()) / area.height().max(1.0)).clamp(0.0, 1.0);
    let fade = if t < 0.14 {
        t / 0.14
    } else if t > 0.78 {
        (1.0 - t) / 0.22
    } else {
        1.0
    };
    if fade > 0.01 && ui.is_rect_visible(rect) {
        ui.painter()
            .add(egui::epaint::TextShape::new(rect.min, galley, color).with_opacity_factor(fade));
    }
    resp
}
