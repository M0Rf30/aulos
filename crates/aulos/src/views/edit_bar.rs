// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Text editing on Android. winit's NativeActivity backend has neither a
//! system clipboard nor an input connection, so the soft keyboard cannot
//! paste and a text field has no selection menu. This module bridges the
//! gap: the system clipboard (`android::clipboard_text`) feeds egui's paste
//! events and receives its copies, and a long press in a text field opens a
//! small bar above it with Paste, Select all, Copy and Cut (the last two not
//! on password fields, where egui ignores them), kept above the soft
//! keyboard. As on Android, focusing a field only shows the keyboard.
//!
//! A bar button cannot act in the frame it is tapped (the field was drawn
//! earlier in that frame), so it records a [`Command`] that [`begin`]
//! replays as an egui event at the start of the next frame.

use crate::android;
use crate::theme::{self, Theme};
use egui::text_edit::TextEditState;
use egui::{Area, Context, Event, Frame, Id, Key, Modifiers, Order, OutputCommand, RichText};

/// What the bar asks of the focused field.
#[derive(Clone, Copy)]
enum Command {
    Paste,
    SelectAll,
    Copy,
    Cut,
}

/// The command waiting for the next frame, with the field it targets.
const PENDING: &str = "edit_bar_pending";

/// Bar height, comfortable for a finger.
const HEIGHT: f32 = 44.0;

/// Start of a frame. Replays the command tapped last frame, and makes
/// hardware-keyboard pastes read the system clipboard instead of egui's
/// private one. Taps outside a field are handled by [`show`], so egui's own
/// "click elsewhere drops focus" rule is off (it would also fire on the bar).
pub fn begin(ctx: &Context) {
    ctx.options_mut(|o| o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
    if ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Paste(_))))
        && let Some(text) = android::clipboard_text()
    {
        ctx.input_mut(|i| {
            for event in &mut i.events {
                if let Event::Paste(pasted) = event {
                    pasted.clone_from(&text);
                }
            }
        });
    }
    let pending = Id::new(PENDING);
    let Some((id, command)) = ctx.data_mut(|d| {
        let value = d.get_temp::<(Id, Command)>(pending);
        d.remove::<(Id, Command)>(pending);
        value
    }) else {
        return;
    };
    // Only when focus was actually lost: `request_focus` interrupts the IME
    // composition, which egui-winit turns into hide-then-show of the soft
    // keyboard, so a plain Paste would flash or re-open a dismissed keyboard.
    ctx.memory_mut(|m| {
        if m.focused() != Some(id) {
            m.request_focus(id);
        }
    });
    let key = |key| Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    };
    let event = match command {
        Command::Paste => match android::clipboard_text() {
            Some(text) => Event::Paste(text),
            None => return,
        },
        Command::SelectAll => key(Key::A),
        Command::Copy => Event::Copy,
        Command::Cut => Event::Cut,
    };
    ctx.input_mut(|i| i.events.push(event));
    ctx.request_repaint();
}

/// End of a frame. Mirrors copies to the system clipboard, drops the focus
/// when the finger lands away from field and bar, and draws the bar.
pub fn show(ctx: &Context) {
    let copied = ctx.output(|o| {
        o.commands.iter().rev().find_map(|c| match c {
            OutputCommand::CopyText(text) => Some(text.clone()),
            _ => None,
        })
    });
    if let Some(text) = copied {
        android::set_clipboard_text(&text);
    }
    record_long_press(ctx);
    let Some(id) = ctx.memory(|m| m.focused()) else {
        return;
    };
    if !ctx.egui_wants_keyboard_input() {
        return;
    }
    let Some(state) = TextEditState::load(ctx, id) else {
        return;
    };
    let Some(field) = ctx.read_response(id).map(|r| r.rect) else {
        return;
    };
    let bar_rect: Option<egui::Rect> = ctx.data(|d| d.get_temp(Id::new(BAR_RECT)));
    let pressed_at = ctx.input(|i| {
        i.pointer
            .any_pressed()
            .then(|| i.pointer.interact_pos())
            .flatten()
    });
    // A tap away from field and bar drops the focus (and the keyboard).
    if pressed_at
        .is_some_and(|p| !field.expand(4.0).contains(p) && !bar_rect.is_some_and(|b| b.contains(p)))
    {
        ctx.memory_mut(|m| m.surrender_focus(id));
        close_menu(ctx);
        return;
    }
    if !menu_open(ctx, id, field, bar_rect, pressed_at) {
        ctx.data_mut(|d| d.remove::<egui::Rect>(Id::new(BAR_RECT)));
        return;
    }
    let selected = state.cursor.char_range().is_some_and(|r| !r.is_empty());
    // egui ignores Copy and Cut on a password field: do not offer them.
    let copyable = !crate::widgets::is_password(ctx, id);
    // Above the soft keyboard too. The safe area may already include it; the
    // higher of the two limits wins.
    let screen = {
        let content = ctx.content_rect();
        let keyboard_top =
            ctx.viewport_rect().bottom() - android::ime_bottom() / ctx.pixels_per_point();
        egui::Rect::from_min_max(
            content.min,
            egui::pos2(
                content.right(),
                content.bottom().min(keyboard_top).max(content.top()),
            ),
        )
    };
    let above = field.top() - HEIGHT - 10.0 >= screen.top();
    let pos = egui::pos2(
        field.left(),
        if above {
            field.top() - HEIGHT - 8.0
        } else {
            field.bottom() + 8.0
        },
    );
    let t = Theme::get(ctx);
    let mut chosen = None;
    let bar = Area::new(Id::new("edit_bar"))
        .order(Order::Foreground)
        .fixed_pos(pos)
        .constrain_to(screen)
        .show(ctx, |ui| {
            Frame::new()
                .fill(t.pal.surface_3)
                .stroke(egui::Stroke::new(1.0, t.pal.stroke_strong))
                .corner_radius(10)
                .shadow(theme::shadow([0, 8], 24, 90))
                .show(ui, |ui| {
                    ui.set_height(HEIGHT);
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.horizontal_centered(|ui| {
                        let mut button = |ui: &mut egui::Ui, label: String, command| {
                            let text = RichText::new(label)
                                .font(theme::medium(14.0))
                                .color(t.pal.text);
                            let b = egui::Button::new(text)
                                .frame(false)
                                .min_size(egui::vec2(72.0, HEIGHT));
                            if ui.add(b).clicked() {
                                chosen = Some(command);
                            }
                        };
                        button(ui, aulos_core::fl!("edit-paste"), Command::Paste);
                        button(ui, aulos_core::fl!("edit-select-all"), Command::SelectAll);
                        if selected && copyable {
                            button(ui, aulos_core::fl!("edit-copy"), Command::Copy);
                            button(ui, aulos_core::fl!("edit-cut"), Command::Cut);
                        }
                    });
                });
        })
        .response
        .rect;
    ctx.data_mut(|d| d.insert_temp(Id::new(BAR_RECT), bar));
    if let Some(command) = chosen {
        ctx.data_mut(|d| d.insert_temp(Id::new(PENDING), (id, command)));
        close_menu(ctx);
        ctx.request_repaint();
    }
}

/// Where the bar was drawn last frame (a tap on it is not "away").
const BAR_RECT: &str = "edit_bar_rect";
/// The field whose menu is open.
const MENU: &str = "edit_bar_menu";
/// Where and when the last long press was held.
const LONG_PRESS: &str = "edit_bar_long_press";
/// How long a finger stays down for a long press.
const LONG_PRESS_SECS: f64 = 0.5;

/// Remembers where a finger has been held still for [`LONG_PRESS_SECS`].
/// Runs every frame, also while no field has focus yet.
fn record_long_press(ctx: &Context) {
    let now = ctx.input(|i| i.time);
    let (down, held, pos) = ctx.input(|i| {
        let down = i.pointer.primary_down();
        let held = down
            && i.pointer
                .press_start_time()
                .is_some_and(|t| now - t >= LONG_PRESS_SECS)
            && !i.pointer.is_decidedly_dragging();
        (down, held, i.pointer.interact_pos())
    });
    if down && !held {
        // Keep frames coming until the hold is long enough.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    if held && let Some(pos) = pos {
        ctx.data_mut(|d| d.insert_temp(Id::new(LONG_PRESS), (pos, now)));
    }
}

fn close_menu(ctx: &Context) {
    ctx.data_mut(|d| {
        d.remove::<Id>(Id::new(MENU));
        d.remove::<(egui::Pos2, f64)>(Id::new(LONG_PRESS));
    });
}

/// Android's rule: the Paste / Select all menu appears on a long press in a
/// field, not when it gets focus, and goes away on typing, on a tap in the
/// field or away from it, and once an entry is chosen. A press held still for
/// [`LONG_PRESS_SECS`] counts even if the finger lifted before the field had
/// focus (egui focuses a field on release), so the press is remembered for a
/// moment.
fn menu_open(
    ctx: &Context,
    id: Id,
    field: egui::Rect,
    bar: Option<egui::Rect>,
    pressed_at: Option<egui::Pos2>,
) -> bool {
    let now = ctx.input(|i| i.time);
    let typed = ctx.input(|i| {
        i.events
            .iter()
            .any(|e| matches!(e, Event::Text(_) | Event::Paste(_)))
    });
    // A tap in the field closes an open menu; a tap on the bar is a choice.
    let tapped_field = pressed_at
        .is_some_and(|p| field.expand(4.0).contains(p) && !bar.is_some_and(|b| b.contains(p)));
    if typed || tapped_field {
        close_menu(ctx);
    }
    let long: Option<(egui::Pos2, f64)> = ctx.data(|d| d.get_temp(Id::new(LONG_PRESS)));
    if let Some((at, when)) = long
        && now - when < 1.5
        && field.expand(4.0).contains(at)
    {
        ctx.data_mut(|d| {
            d.insert_temp(Id::new(MENU), id);
            d.remove::<(egui::Pos2, f64)>(Id::new(LONG_PRESS));
        });
    }
    ctx.data(|d| d.get_temp::<Id>(Id::new(MENU))) == Some(id)
}
