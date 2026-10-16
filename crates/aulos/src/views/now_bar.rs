//! The 76px now-playing bar: track, transport + seek, and the side controls.

use crate::app::{App, Sheet};
use crate::theme::{self, Theme};
use crate::widgets::{
    IconBtn, cover_image, paint_icon, paint_text, quality_badge, seek_bar, slider_bar,
};
use aulos_core::config::RepeatMode;
use aulos_core::format::format_duration;
use egui::{Align, Layout, Rect, Sense, Ui, pos2, vec2};
use egui_phosphor::{fill, regular};

pub fn show(app: &mut App, ui: &mut Ui) {
    let bar = ui.max_rect();
    let inner = Rect::from_min_max(
        pos2(bar.left() + 12.0, bar.top() + 1.0),
        pos2(bar.right() - 16.0, bar.bottom()),
    );
    let gap = 16.0;
    let right_auto = 3.0 * 30.0 + 2.0 * 2.0 + 6.0 + 30.0;
    let (left_w, mid_w, right_w) = if app.narrow {
        let flex = inner.width() - 2.0 * gap - right_auto;
        (flex / 2.2, flex * 1.2 / 2.2, right_auto)
    } else {
        let flex = inner.width() - 2.0 * gap;
        (flex / 3.3, flex * 1.3 / 3.3, flex / 3.3)
    };
    let col = |x: f32, w: f32| Rect::from_min_size(pos2(x, inner.top()), vec2(w, inner.height()));
    let left = col(inner.left(), left_w);
    let mid = col(left.right() + gap, mid_w);
    let right = col(mid.right() + gap, right_w);

    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(left)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| track_info(app, ui),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(mid)
            .layout(Layout::top_down(Align::Center)),
        |ui| transport(app, ui, mid),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(right)
            .layout(Layout::right_to_left(Align::Center)),
        |ui| side_controls(app, ui),
    );
}

fn track_info(app: &mut App, ui: &mut Ui) {
    let t = Theme::get(ui.ctx());
    ui.spacing_mut().item_spacing.x = 12.0;
    let Some(track) = app.session.playback.current_track.clone() else {
        // Nothing playing: an empty cover slot and a hint.
        let (rect, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
        ui.painter().rect_filled(rect, 6.0, t.pal.surface_2);
        paint_icon(
            ui.painter(),
            rect.center(),
            regular::MUSIC_NOTES,
            20.0,
            t.pal.text_4,
            false,
        );
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(aulos_core::fl!("nowbar-nothing-playing"))
                    .font(theme::semibold(13.5))
                    .color(t.pal.text_2),
            );
            ui.label(
                egui::RichText::new(aulos_core::fl!("nowbar-pick-something"))
                    .font(theme::regular(12.5))
                    .color(t.pal.text_3),
            );
        });
        return;
    };

    let (rect, resp) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::click());
    let key = track.cover_key();
    cover_image(ui, &app.covers, rect, app.covers.thumb(&key), &key, 6);
    if resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        app.expanded = true;
    }

    let fav = app
        .session
        .library
        .favorite_state(Some(&track), &track.id.to_string())
        .unwrap_or(track.is_favorite);
    let heart_w = 16.0 + 12.0 + 2.0;
    let quality = App::quality_of(&track);
    // The block is as wide as its widest line (as in the design), so the heart
    // sits right after it; long titles stop at the column edge.
    let badge_w = if quality.is_some() { 52.0 } else { 0.0 };
    let title_w = crate::widgets::text_width(ui, &track.title, theme::semibold(13.5));
    let artist_w = crate::widgets::text_width(ui, &track.artist, theme::regular(12.5)) + badge_w;
    let text_w = (title_w.max(artist_w) + 2.0).min((ui.available_width() - heart_w).max(40.0));
    ui.allocate_ui_with_layout(vec2(text_w, 52.0), Layout::top_down(Align::Min), |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add_space(8.0);
        let (title_rect, _) = ui.allocate_exact_size(vec2(text_w, 17.0), Sense::hover());
        crate::widgets::marquee_text(
            ui,
            title_rect,
            crate::widgets::marquee_id("nowbar", &track.title),
            &track.title,
            theme::semibold(13.5),
            t.pal.text,
            t.pal.bg_bar,
            true,
        );
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (artist_rect, _) =
                ui.allocate_exact_size(vec2((text_w - badge_w).max(20.0), 17.0), Sense::hover());
            let used = paint_text(
                ui.painter(),
                artist_rect,
                Align::Min,
                &track.artist,
                theme::regular(12.5),
                t.pal.text_2,
            );
            if let Some(q) = quality {
                // Badge sits right after the artist text.
                let badge = Rect::from_min_size(
                    pos2(artist_rect.left() + used + 8.0, artist_rect.top() + 1.0),
                    vec2(48.0, 15.0),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(badge), |ui| {
                    quality_badge(ui, q);
                });
            }
        });
    });
    let (hrect, hresp) = ui.allocate_exact_size(vec2(heart_w - 2.0, 24.0), Sense::click());
    let (glyph, filled, color) = if fav {
        (fill::HEART, true, t.accent.fill)
    } else {
        (
            regular::HEART,
            false,
            if hresp.hovered() {
                t.pal.text
            } else {
                t.pal.text_3
            },
        )
    };
    paint_icon(ui.painter(), hrect.center(), glyph, 16.0, color, filled);
    if hresp
        .on_hover_text(if fav {
            aulos_core::fl!("nowbar-favorite-remove")
        } else {
            aulos_core::fl!("nowbar-favorite-add")
        })
        .clicked()
    {
        app.toggle_favorite(&track);
    }
}

/// Width of the elapsed / remaining labels: fits `\u{2212}1:00:00`.
const LABEL_W: f32 = 56.0;

fn transport(app: &mut App, ui: &mut Ui, col: Rect) {
    let t = Theme::get(ui.ctx());
    let playing = app.session.playback.is_playing();
    let track = app.session.playback.current_track.as_ref();
    let live = track.is_some_and(|t| t.is_live_stream());
    let has_track = track.is_some();
    let duration = track.map_or(Duration::ZERO, |t| t.duration);
    let shuffle = app.session.config.config.shuffle;
    let repeat = app.session.config.config.repeat_mode;
    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);

    ui.add_space(((col.height() - (36.0 + 6.0 + 16.0)) / 2.0).max(0.0));
    // Buttons row, centred.
    let n = if app.narrow { 5.0 } else { 6.0 };
    let row_w = (n - 1.0) * 30.0 + 36.0 + n * 6.0 - 6.0;
    let (row, _) = ui.allocate_exact_size(vec2(row_w, 36.0), Sense::hover());
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if IconBtn::new(regular::SHUFFLE)
                .icon_size(16.0)
                .color(if shuffle { t.accent.text } else { t.pal.text_2 })
                .dot(shuffle)
                .tip(aulos_core::fl!("nowbar-shuffle"))
                .show(ui)
                .clicked()
            {
                let u = app.session.toggle_shuffle();
                app.run(u);
            }
            if IconBtn::new(fill::SKIP_BACK)
                .filled(true)
                .icon_size(17.0)
                .color(t.pal.text)
                .enabled(has_track)
                .tip(aulos_core::fl!("nowbar-previous"))
                .show(ui)
                .clicked()
            {
                let u = app.session.previous_track();
                app.run(u);
            }
            let (disc, resp) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::click());
            let fill_c = if resp.hovered() {
                t.accent.fill.gamma_multiply(0.92)
            } else {
                t.accent.fill
            };
            ui.painter().circle_filled(disc.center(), 18.0, fill_c);
            paint_icon(
                ui.painter(),
                disc.center(),
                if playing { fill::PAUSE } else { fill::PLAY },
                17.0,
                t.accent.on_fill,
                true,
            );
            if resp
                .on_hover_text(if playing {
                    aulos_core::fl!("nowbar-pause")
                } else {
                    aulos_core::fl!("nowbar-play")
                })
                .clicked()
            {
                app.toggle_play();
            }
            if !app.narrow
                && IconBtn::new(fill::STOP)
                    .filled(true)
                    .icon_size(15.0)
                    .color(t.pal.text_2)
                    .enabled(has_track)
                    .tip(aulos_core::fl!("nowbar-stop"))
                    .show(ui)
                    .clicked()
            {
                let u = app.session.stop();
                app.run(u);
            }
            if IconBtn::new(fill::SKIP_FORWARD)
                .filled(true)
                .icon_size(17.0)
                .color(t.pal.text)
                .enabled(has_track)
                .tip(aulos_core::fl!("nowbar-next"))
                .show(ui)
                .clicked()
            {
                let u = app.session.next_track();
                app.run(u);
            }
            let (glyph, on) = match repeat {
                RepeatMode::None => (regular::REPEAT, false),
                RepeatMode::All => (regular::REPEAT, true),
                RepeatMode::One => (regular::REPEAT_ONCE, true),
            };
            if IconBtn::new(glyph)
                .icon_size(16.0)
                .color(if on { t.accent.text } else { t.pal.text_2 })
                .dot(on)
                .tip(aulos_core::fl!("nowbar-repeat"))
                .show(ui)
                .clicked()
            {
                let u = app.session.cycle_repeat();
                app.run(u);
            }
        },
    );

    // Times + seek bar.
    let position = app.session.playback.position;
    let frac = match app.seek_preview {
        Some(f) => f,
        None if duration.is_zero() => 0.0,
        None => (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0),
    };
    let width = col.width().min(520.0);
    let (line, _) = ui.allocate_exact_size(vec2(width, 16.0), Sense::hover());
    let shown = Duration::from_secs_f32(frac * duration.as_secs_f32());
    let elapsed = if live {
        aulos_core::fl!("nowbar-live")
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
    let mono = theme::mono(11.0);
    paint_text(
        ui.painter(),
        Rect::from_min_size(line.min, vec2(LABEL_W, 16.0)),
        Align::Max,
        &elapsed,
        mono.clone(),
        t.pal.text_2,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_size(
            pos2(line.right() - LABEL_W, line.top()),
            vec2(LABEL_W, 16.0),
        ),
        Align::Min,
        &remaining,
        mono,
        t.pal.text_2,
    );
    let rail = Rect::from_min_max(
        pos2(line.left() + LABEL_W + 10.0, line.top()),
        pos2(line.right() - LABEL_W - 10.0, line.bottom()),
    );
    let seek = ui
        .scope_builder(egui::UiBuilder::new().max_rect(rail), |ui| {
            seek_bar(
                ui,
                frac,
                4.0,
                rail.width(),
                has_track && !live && !duration.is_zero(),
                duration.as_secs_f32(),
            )
        })
        .inner;
    app.seek_preview = seek.dragging;
    if let Some(f) = seek.released {
        let u = app.session.seek_fraction(f);
        app.run(u);
        app.seek_preview = None;
    }
}

use std::time::Duration;

fn side_controls(app: &mut App, ui: &mut Ui) {
    let t = Theme::get(ui.ctx());
    ui.spacing_mut().item_spacing.x = 2.0;
    // Right-to-left: volume first, then the buttons.
    if !app.narrow {
        let vol = app.volume();
        let (resp, new) = slider_bar(ui, vol, vec2(84.0, 20.0), t.pal.text_1);
        if let Some(v) = new {
            let u = app.session.set_volume(v);
            app.run(u);
        }
        if resp.drag_stopped() || resp.clicked() {
            app.session.commit_volume();
        }
    }
    let vol = app.volume();
    let speaker = if vol <= 0.001 {
        regular::SPEAKER_X
    } else if vol < 0.4 {
        regular::SPEAKER_LOW
    } else {
        regular::SPEAKER_HIGH
    };
    let muted = vol <= 0.001;
    let mut speaker_btn = IconBtn::new(speaker).icon_size(16.0).tip(if app.narrow {
        aulos_core::fl!("nowbar-volume")
    } else {
        aulos_core::fl!("nowbar-mute")
    });
    if muted {
        speaker_btn = speaker_btn.color(t.pal.danger);
    }
    let speaker_resp = speaker_btn.show(ui);
    if app.narrow {
        // Narrow: the speaker opens a volume popover (with a mute button).
        egui::Popup::menu(&speaker_resp).show(|ui| {
            ui.set_min_width(150.0);
            ui.horizontal(|ui| {
                if IconBtn::new(speaker)
                    .size(28.0)
                    .icon_size(16.0)
                    .color(if muted { t.pal.danger } else { t.pal.text_2 })
                    .tip(if muted {
                        aulos_core::fl!("nowbar-unmute")
                    } else {
                        aulos_core::fl!("nowbar-mute")
                    })
                    .show(ui)
                    .clicked()
                {
                    let u = app.session.toggle_mute();
                    app.run(u);
                }
                let (resp, new) = slider_bar(ui, app.volume(), vec2(110.0, 20.0), t.pal.text_1);
                if let Some(v) = new {
                    let u = app.session.set_volume(v);
                    app.run(u);
                }
                if resp.drag_stopped() || resp.clicked() {
                    app.session.commit_volume();
                }
            });
        });
    } else if speaker_resp.clicked() {
        let u = app.session.toggle_mute();
        app.run(u);
    }
    ui.add_space(4.0);

    // Buttons, right to left: expand, [visualizer], properties, lyrics, queue.
    if IconBtn::new(regular::ARROWS_OUT_SIMPLE)
        .icon_size(16.0)
        .tip(aulos_core::fl!("nowbar-expand"))
        .show(ui)
        .clicked()
    {
        app.expanded = true;
    }
    #[cfg(feature = "visualizer")]
    if !app.narrow
        && app.viz_available()
        && IconBtn::new(regular::WAVEFORM)
            .icon_size(16.0)
            .tip(aulos_core::fl!("nowbar-visualizer"))
            .show(ui)
            .clicked()
    {
        app.set_visualizer(true);
    }
    if !app.narrow {
        let open = app.sheet == Some(Sheet::Properties);
        if IconBtn::new(regular::INFO)
            .icon_size(16.0)
            .active(open)
            .tip(aulos_core::fl!("nowbar-properties"))
            .show(ui)
            .clicked()
        {
            if open {
                app.sheet = None;
            } else {
                if let Some(track) = app.session.playback.current_track.clone() {
                    let effect = app.session.open_track_properties(track);
                    app.run(effect);
                }
                app.sheet = Some(Sheet::Properties);
            }
        }
    }
    if IconBtn::new(regular::MICROPHONE_STAGE)
        .icon_size(16.0)
        .tip(aulos_core::fl!("nowbar-lyrics"))
        .show(ui)
        .clicked()
    {
        app.views.expanded.show_lyrics = true;
        app.expanded = true;
    }
    let open = app.sheet == Some(Sheet::Queue);
    if IconBtn::new(regular::QUEUE)
        .icon_size(16.0)
        .active(open)
        .tip(aulos_core::fl!("nowbar-queue"))
        .show(ui)
        .clicked()
    {
        app.sheet = if open { None } else { Some(Sheet::Queue) };
    }
}
