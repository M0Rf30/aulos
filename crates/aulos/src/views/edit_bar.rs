// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Text editing on Android. winit's NativeActivity backend has neither a
//! system clipboard nor an input connection, so the soft keyboard cannot
//! paste and a text field has no selection menu. This module bridges the
//! gap: the system clipboard (`android::clipboard_text`) feeds egui's paste
//! events and receives its copies, and a small bar above the focused text
//! field offers Paste, Select all, Copy and Cut.
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
    let selected = state.cursor.char_range().is_some_and(|r| !r.is_empty());
    let screen = ctx.content_rect();
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
                        if selected {
                            button(ui, aulos_core::fl!("edit-copy"), Command::Copy);
                            button(ui, aulos_core::fl!("edit-cut"), Command::Cut);
                        }
                    });
                });
        })
        .response
        .rect;
    if let Some(command) = chosen {
        ctx.data_mut(|d| d.insert_temp(Id::new(PENDING), (id, command)));
        ctx.request_repaint();
        return;
    }
    let pressed_away = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .interact_pos()
                .is_some_and(|p| !field.expand(4.0).contains(p) && !bar.contains(p))
    });
    if pressed_away {
        ctx.memory_mut(|m| m.surrender_focus(id));
    }
}
