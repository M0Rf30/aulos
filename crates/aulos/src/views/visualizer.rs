//! The visualizer view (feature `visualizer`): the lactea render target full
//! bleed, a HUD that fades after 2.5 s without input, and the preset browser
//! sheet. Esc closes (browser first, then fullscreen, then the view); ←/→
//! step through the presets, L locks the preset, F toggles fullscreen, P the
//! browser.

use crate::app::App;
use crate::theme::{self, ColorExt, Theme};
use crate::viz::Frame as VizFrame;
use crate::widgets::{Covers, IconBtn, cover_image, paint_text, search_field, slider_bar};
use aulos_core::visualizer::VizCommand;
use aulos_core::visualizer::browser::{Row, build_rows, current_row};
use aulos_core::visualizer::presets::PresetEntry;
use egui::{
    Align, Align2, Area, Color32, Context, CornerRadius, CursorIcon, Frame, Id, Key, Margin, Order,
    Rect, ScrollArea, Sense, Ui, pos2, vec2,
};
use egui_phosphor::{fill, regular};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Seconds without input before the HUD starts to fade.
const HUD_IDLE: f64 = 2.5;
/// Fade duration, seconds.
const HUD_FADE: f32 = 0.6;
const CHIP_H: f32 = 30.0;
const BROWSER_W: f32 = 280.0;
const BROWSER_ROW_H: f32 = 28.0;

#[derive(Default)]
pub struct State {
    pub fullscreen: bool,
    /// `Context` time of the last input.
    last_input: f64,
    pub browser: bool,
    query: String,
    rows: Vec<Row>,
    /// What `rows` was built from.
    rows_for: Option<(String, usize)>,
    /// Scroll the browser to the current preset once.
    reveal: bool,
}

impl State {
    /// Shows the preset browser, scrolled to the current preset.
    pub fn open_browser(&mut self) {
        self.browser = true;
        self.reveal = true;
    }

    /// Called when the view opens.
    pub fn reset(&mut self, now: f64) {
        self.last_input = now;
        self.browser = false;
        self.query.clear();
        self.rows.clear();
        self.rows_for = None;
    }
}

enum Intent {
    Close,
    Step(i32),
    Random,
    Load(PathBuf),
    ToggleLock,
    Sensitivity(f32),
    ToggleBrowser,
    ToggleFullscreen,
}

pub fn show(app: &mut App, ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let rect = ui.max_rect();
    let now = ctx.input(|i| i.time);
    let ppp = ctx.pixels_per_point();
    let Some(viz) = app.viz.as_mut() else {
        return;
    };
    viz.want_size([
        (rect.width() * ppp).round() as u32,
        (rect.height() * ppp).round() as u32,
    ]);
    let frame = viz.poll();

    // -- the render target -----------------------------------------------------
    ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
    if let Some(texture) = frame.texture {
        ui.painter().image(
            texture,
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    }
    let surface = ui.allocate_rect(rect, Sense::click());

    // -- input ----------------------------------------------------------------
    let st = &mut app.views.visualizer;
    let active = ctx.input(|i| {
        i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::PointerMoved(_)
                    | egui::Event::PointerButton { .. }
                    | egui::Event::MouseWheel { .. }
                    | egui::Event::Key { .. }
                    | egui::Event::Touch { .. }
            )
        })
    });
    if active {
        st.last_input = now;
    }
    let mut intents = Vec::new();
    if surface.double_clicked() {
        intents.push(Intent::ToggleFullscreen);
    }
    let typing = ctx.egui_wants_keyboard_input();
    ctx.input(|i| {
        if i.key_pressed(Key::Escape) {
            intents.push(if st.browser {
                Intent::ToggleBrowser
            } else if st.fullscreen {
                Intent::ToggleFullscreen
            } else {
                Intent::Close
            });
        }
        if !typing {
            if i.key_pressed(Key::ArrowLeft) {
                intents.push(Intent::Step(-1));
            }
            if i.key_pressed(Key::ArrowRight) {
                intents.push(Intent::Step(1));
            }
            if i.key_pressed(Key::L) {
                intents.push(Intent::ToggleLock);
            }
            if i.key_pressed(Key::F) {
                intents.push(Intent::ToggleFullscreen);
            }
            if i.key_pressed(Key::P) {
                intents.push(Intent::ToggleBrowser);
            }
        }
    });

    // -- HUD ------------------------------------------------------------------
    let visible = st.browser || now - st.last_input < HUD_IDLE;
    let alpha = ctx.animate_bool_with_time(Id::new("viz_hud"), visible, HUD_FADE);
    if !visible && alpha == 0.0 && st.fullscreen {
        ctx.set_cursor_icon(CursorIcon::None);
    }
    if alpha > 0.0 {
        let t = Theme::get(&ctx);
        top_bar(&ctx, rect, alpha, st, &app.viz_settings, &mut intents);
        let track = app.session.playback.current_track.as_ref();
        now_playing(&ctx, rect, alpha, &app.covers, track, &frame);
        if st.browser {
            browser(&ctx, rect, alpha, &t, st, &frame, &mut intents);
        }
    }
    if let Some(failure) = &frame.failure {
        notice(
            &ctx,
            rect,
            &aulos_core::fl!("viz-start-failed", reason = failure.to_string()),
        );
    } else if frame.entries.is_none() {
        notice(&ctx, rect, &aulos_core::fl!("viz-starting"));
    } else if frame.entries.as_ref().is_some_and(|e| e.is_empty()) {
        let dir = aulos_core::visualizer::presets::user_preset_dir().map_or_else(
            || aulos_core::fl!("viz-presets-folder"),
            |dir| dir.display().to_string(),
        );
        notice(&ctx, rect, &aulos_core::fl!("viz-no-presets", dir = dir));
    }

    for intent in intents {
        apply(app, &ctx, &frame, intent);
    }
    // Remember lock, sensitivity and the preset on screen (writes only on change).
    app.session.set_visualizer_prefs(
        app.viz_settings.locked,
        app.viz_settings.sensitivity,
        frame.preset.as_deref(),
    );
}

/// A centred message over the video.
fn notice(ctx: &Context, rect: Rect, text: &str) {
    Area::new(Id::new("viz_notice"))
        .order(Order::Foreground)
        .fixed_pos(rect.center())
        .pivot(Align2::CENTER_CENTER)
        .interactable(false)
        .show(ctx, |ui| {
            Frame::new()
                .fill(Color32::BLACK.alpha(0.55))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(text)
                            .font(theme::medium(13.0))
                            .color(Color32::WHITE),
                    );
                });
        });
}

/// A dark pill that groups HUD controls.
fn chip<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(Color32::BLACK.alpha(0.5))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(2, 0))
        .show(ui, |ui| {
            ui.set_height(CHIP_H);
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal_centered(add).inner
        })
        .inner
}

fn hud_button<'a>(glyph: &'a str, tip: impl Into<std::borrow::Cow<'a, str>>) -> IconBtn<'a> {
    IconBtn::new(glyph)
        .size(CHIP_H - 2.0)
        .icon_size(15.0)
        .color(Color32::WHITE)
        .tip(tip)
}

fn top_bar(
    ctx: &Context,
    rect: Rect,
    alpha: f32,
    st: &State,
    settings: &crate::viz::Settings,
    out: &mut Vec<Intent>,
) {
    Area::new(Id::new("viz_close"))
        .order(Order::Foreground)
        .fixed_pos(pos2(rect.left() + 16.0, rect.top() + 14.0))
        .show(ctx, |ui| {
            ui.set_opacity(alpha);
            chip(ui, |ui| {
                if hud_button(regular::X, aulos_core::fl!("viz-close"))
                    .show(ui)
                    .clicked()
                {
                    out.push(Intent::Close);
                }
            });
        });

    Area::new(Id::new("viz_controls"))
        .order(Order::Foreground)
        .pivot(Align2::RIGHT_TOP)
        .fixed_pos(pos2(rect.right() - 16.0, rect.top() + 14.0))
        .show(ctx, |ui| {
            ui.set_opacity(alpha);
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.horizontal(|ui| {
                chip(ui, |ui| {
                    let (glyph, tip, filled) = if settings.locked {
                        (fill::LOCK, aulos_core::fl!("viz-preset-locked-tip"), true)
                    } else {
                        (
                            regular::LOCK_OPEN,
                            aulos_core::fl!("viz-lock-preset-tip"),
                            false,
                        )
                    };
                    let mut button = hud_button(glyph, tip).filled(filled);
                    if settings.locked {
                        button = button.active(true);
                    }
                    if button.show(ui).clicked() {
                        out.push(Intent::ToggleLock);
                    }
                    if settings.locked {
                        ui.label(
                            egui::RichText::new(aulos_core::fl!("viz-preset-locked"))
                                .font(theme::regular(12.5))
                                .color(Color32::WHITE),
                        );
                        ui.add_space(8.0);
                    }
                });
                chip(ui, |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(aulos_core::fl!("viz-beat"))
                            .font(theme::regular(12.5))
                            .color(Color32::WHITE),
                    );
                    let (resp, new) = slider_bar(
                        ui,
                        settings.sensitivity / 2.0,
                        vec2(60.0, 20.0),
                        Color32::WHITE,
                    );
                    resp.on_hover_text(aulos_core::fl!("viz-beat-sensitivity"));
                    if let Some(v) = new {
                        out.push(Intent::Sensitivity(v * 2.0));
                    }
                    ui.add_space(6.0);
                });
                chip(ui, |ui| {
                    if hud_button(regular::SKIP_BACK, aulos_core::fl!("viz-previous-preset"))
                        .show(ui)
                        .clicked()
                    {
                        out.push(Intent::Step(-1));
                    }
                    if hud_button(regular::SHUFFLE, aulos_core::fl!("viz-random-preset"))
                        .show(ui)
                        .clicked()
                    {
                        out.push(Intent::Random);
                    }
                    if hud_button(regular::SKIP_FORWARD, aulos_core::fl!("viz-next-preset"))
                        .show(ui)
                        .clicked()
                    {
                        out.push(Intent::Step(1));
                    }
                });
                chip(ui, |ui| {
                    if hud_button(
                        regular::LIST_MAGNIFYING_GLASS,
                        aulos_core::fl!("viz-preset-browser"),
                    )
                    .active(st.browser)
                    .show(ui)
                    .clicked()
                    {
                        out.push(Intent::ToggleBrowser);
                    }
                    let (glyph, tip) = if st.fullscreen {
                        (
                            regular::ARROWS_IN_SIMPLE,
                            aulos_core::fl!("viz-leave-fullscreen"),
                        )
                    } else {
                        (
                            regular::ARROWS_OUT_SIMPLE,
                            aulos_core::fl!("viz-fullscreen"),
                        )
                    };
                    if hud_button(glyph, tip).show(ui).clicked() {
                        out.push(Intent::ToggleFullscreen);
                    }
                });
            });
        });
}

fn now_playing(
    ctx: &Context,
    rect: Rect,
    alpha: f32,
    covers: &Covers,
    track: Option<&aulos_core::library::Track>,
    frame: &VizFrame,
) {
    let Some(track) = track else { return };
    let preset = frame
        .preset
        .as_deref()
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned());
    Area::new(Id::new("viz_now"))
        .order(Order::Foreground)
        .pivot(Align2::LEFT_BOTTOM)
        .fixed_pos(pos2(rect.left() + 16.0, rect.bottom() - 16.0))
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_opacity(alpha);
            Frame::new()
                .fill(Color32::BLACK.alpha(0.5))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin {
                    left: 10,
                    right: 14,
                    top: 10,
                    bottom: 10,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let (cover, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                        let key = track.cover_key();
                        cover_image(ui, covers, cover, covers.thumb(&key), &key, 5);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.set_max_width((rect.width() * 0.5).max(160.0));
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&track.title)
                                        .font(theme::semibold(14.0))
                                        .color(Color32::WHITE),
                                )
                                .truncate(),
                            );
                            let mut sub = track.artist.clone();
                            if let Some(preset) = preset {
                                if !sub.is_empty() {
                                    sub.push_str(" · ");
                                }
                                sub.push_str(&preset);
                            }
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(sub)
                                        .font(theme::regular(12.0))
                                        .color(Color32::from_rgb(0xC9, 0xD2, 0xD6)),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
        });
}

fn browser(
    ctx: &Context,
    rect: Rect,
    alpha: f32,
    t: &Theme,
    st: &mut State,
    frame: &VizFrame,
    out: &mut Vec<Intent>,
) {
    let entries: Arc<Vec<PresetEntry>> = frame.entries.clone().unwrap_or_default();
    // Rebuild the flattened rows only when the query or the list changed.
    let stamp = (st.query.clone(), entries.len());
    if st.rows_for.as_ref() != Some(&stamp) {
        st.rows = build_rows(&entries, &st.query);
        st.rows_for = Some(stamp);
    }
    let current = current_row(&st.rows, &entries, frame.preset.as_deref());
    let top = rect.top() + 56.0;
    let height = (rect.bottom() - 16.0 - top).max(120.0);
    let search_id = Id::new("viz_browser_search");

    Area::new(Id::new("viz_browser"))
        .order(Order::Foreground)
        .fixed_pos(pos2(rect.right() - 16.0 - BROWSER_W, top))
        .show(ctx, |ui| {
            ui.set_opacity(alpha);
            Frame::new()
                .fill(Color32::from_rgba_unmultiplied(20, 19, 18, 219))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(BROWSER_W - 20.0);
                    ui.set_height(height - 20.0);
                    ui.spacing_mut().item_spacing.y = 4.0;
                    let hint = aulos_core::fl!("viz-search-presets", count = entries.len());
                    let before = st.query.clone();
                    search_field(ui, &mut st.query, &hint, BROWSER_W - 20.0, search_id);
                    if before != st.query {
                        st.reveal = false;
                    }
                    ui.add_space(4.0);
                    // `show_rows` strides by the spacing in force right here.
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut scroll = ScrollArea::vertical().auto_shrink([false, false]);
                    if st.reveal
                        && let Some(row) = current
                    {
                        st.reveal = false;
                        let y = (row as f32 * BROWSER_ROW_H - 120.0).max(0.0);
                        scroll = scroll.vertical_scroll_offset(y);
                    }
                    if st.rows.is_empty() {
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new(aulos_core::fl!("viz-no-match"))
                                .font(theme::regular(12.5))
                                .color(t.pal.text_3),
                        );
                    }
                    scroll.show_rows(ui, BROWSER_ROW_H, st.rows.len(), |ui, range| {
                        for i in range {
                            let Some(row) = st.rows.get(i).copied() else {
                                continue;
                            };
                            let (cell, resp) = ui.allocate_exact_size(
                                vec2(ui.available_width(), BROWSER_ROW_H),
                                Sense::click(),
                            );
                            match row {
                                Row::Category(e) => {
                                    paint_text(
                                        ui.painter(),
                                        Rect::from_min_max(
                                            pos2(cell.left() + 8.0, cell.top() + 6.0),
                                            cell.right_bottom(),
                                        ),
                                        Align::Min,
                                        &entries[e].category.to_uppercase(),
                                        theme::semibold(10.5),
                                        t.pal.text_3,
                                    );
                                }
                                Row::Preset(e) => {
                                    let entry = &entries[e];
                                    let is_current = current == Some(i);
                                    let color = if is_current {
                                        t.accent.text
                                    } else {
                                        Color32::from_rgb(0xD6, 0xD2, 0xCA)
                                    };
                                    if is_current {
                                        ui.painter().rect_filled(
                                            cell.shrink2(vec2(0.0, 1.0)),
                                            CornerRadius::same(6),
                                            t.accent.fill.alpha(0.14),
                                        );
                                    } else if resp.hovered() {
                                        ui.painter().rect_filled(
                                            cell.shrink2(vec2(0.0, 1.0)),
                                            CornerRadius::same(6),
                                            Color32::WHITE.alpha(0.06),
                                        );
                                    }
                                    paint_text(
                                        ui.painter(),
                                        cell.shrink2(vec2(8.0, 0.0)),
                                        Align::Min,
                                        &entry.name,
                                        theme::regular(12.5),
                                        color,
                                    );
                                    if resp.clicked() {
                                        out.push(Intent::Load(entry.path.clone()));
                                    }
                                }
                            }
                        }
                    });
                });
        });
}

/// The entry `delta` places from `current` in list order, wrapping around.
fn neighbour(entries: &[PresetEntry], current: Option<&Path>, delta: i32) -> Option<PathBuf> {
    let len = i32::try_from(entries.len()).ok().filter(|n| *n > 0)?;
    let at = current
        .and_then(|c| entries.iter().position(|e| e.path == c))
        .and_then(|i| i32::try_from(i).ok());
    let next = match at {
        Some(i) => (i + delta).rem_euclid(len),
        None if delta < 0 => len - 1,
        None => 0,
    };
    entries
        .get(usize::try_from(next).ok()?)
        .map(|e| e.path.clone())
}

fn apply(app: &mut App, ctx: &Context, frame: &VizFrame, intent: Intent) {
    match intent {
        Intent::Close => app.set_visualizer(false),
        Intent::Step(delta) => {
            let entries = frame.entries.as_deref().map_or(&[][..], Vec::as_slice);
            if let Some(path) = neighbour(entries, frame.preset.as_deref(), delta)
                && let Some(viz) = &app.viz
            {
                viz.send(VizCommand::LoadPreset(path));
            }
        }
        Intent::Random => {
            if let Some(viz) = &app.viz {
                viz.send(VizCommand::NextPreset);
            }
        }
        Intent::Load(path) => {
            if let Some(viz) = &app.viz {
                viz.send(VizCommand::LoadPreset(path));
            }
        }
        Intent::ToggleLock => {
            let locked = !app.viz_settings.locked;
            app.viz_settings.locked = locked;
            if let Some(viz) = &app.viz {
                viz.send(VizCommand::SetLocked(locked));
            }
        }
        Intent::Sensitivity(value) => {
            app.viz_settings.sensitivity = value.clamp(0.0, 2.0);
            if let Some(viz) = &app.viz {
                viz.send(VizCommand::SetBeatSensitivity(value));
            }
        }
        Intent::ToggleBrowser => {
            let st = &mut app.views.visualizer;
            if st.browser {
                st.browser = false;
                ctx.memory_mut(|m| m.surrender_focus(Id::new("viz_browser_search")));
            } else {
                st.open_browser();
            }
        }
        Intent::ToggleFullscreen => {
            let st = &mut app.views.visualizer;
            st.fullscreen = !st.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(st.fullscreen));
        }
    }
}
