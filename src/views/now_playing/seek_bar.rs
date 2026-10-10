// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Smoothly-advancing seek bar.
//!
//! The backend position only reaches the UI every ~500ms (`PlaybackTick`),
//! which made a stock slider visibly step forward twice a second. This
//! widget extrapolates the played fraction between ticks *inside `draw`*,
//! scheduling its own redraws (`Shell::request_redraw_at`) at the rate the
//! fill actually moves by about half a pixel — so playback looks continuous
//! without rebuilding the application's view tree every frame.
//!
//! It also animates its hover state (rail thickens, handle grows in) via
//! `iced::Animation`, instead of snapping between styles.

use cosmic::iced::advanced::layout::{self, Layout};
use cosmic::iced::advanced::renderer::{self, Renderer as _};
use cosmic::iced::advanced::widget::{Tree, Widget, tree};
use cosmic::iced::advanced::{Clipboard, Shell, mouse};
use cosmic::iced::time::Instant;
use cosmic::iced::{
    Animation, Background, Border, Color, Event, Length, Rectangle, Size, touch, window,
};
use std::cell::Cell;
use std::time::Duration;

/// Widget height; matches the stock slider so layouts don't shift.
const HEIGHT: f32 = 16.0;
/// Rail thickness at rest / while hovered or dragged.
const RAIL_REST: f32 = 4.0;
const RAIL_ACTIVE: f32 = 6.0;
/// Handle diameter when fully shown.
const HANDLE: f32 = 14.0;
/// Never extrapolate further than this past the last backend position: if
/// ticks stall (buffering, a stuck backend) the bar stops rather than racing
/// ahead and snapping back.
const MAX_EXTRAPOLATION: Duration = Duration::from_millis(750);

pub struct SmoothSeek<'a, Message> {
    progress: f32,
    duration_secs: f32,
    playing: bool,
    accent: Option<Color>,
    on_change: Box<dyn Fn(f32) -> Message + 'a>,
    on_release: Message,
    width: Length,
}

struct State {
    dragging: bool,
    hover: Animation<bool>,
    /// Last `progress` received from the app, and when it was first seen.
    anchor: Cell<(f32, Instant)>,
    /// Drag value, kept locally so the fill follows the pointer exactly.
    drag_value: f32,
    now: Instant,
}

/// Create a smooth seek bar.
///
/// `progress` is the played fraction (0..=1) — or the drag preview while
/// seeking, in which case `playing` should be passed as `false`.
pub fn smooth_seek<'a, Message: Clone + 'a>(
    progress: f32,
    duration: Duration,
    playing: bool,
    accent: Option<Color>,
    on_change: impl Fn(f32) -> Message + 'a,
    on_release: Message,
) -> SmoothSeek<'a, Message> {
    SmoothSeek {
        progress: progress.clamp(0.0, 1.0),
        duration_secs: duration.as_secs_f32(),
        playing,
        accent,
        on_change: Box::new(on_change),
        on_release,
        width: Length::Fill,
    }
}

impl<Message> SmoothSeek<'_, Message> {
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    fn animating(&self) -> bool {
        self.playing && self.duration_secs > 0.0
    }

    /// The fraction to display right now.
    fn displayed(&self, state: &State, now: Instant) -> f32 {
        if state.dragging {
            return state.drag_value;
        }
        let (base, since) = state.anchor.get();
        if !self.animating() {
            return base;
        }
        let elapsed = now.saturating_duration_since(since).min(MAX_EXTRAPOLATION);
        (base + elapsed.as_secs_f32() / self.duration_secs).min(1.0)
    }

    fn locate(bounds: Rectangle, x: f32) -> f32 {
        let inner_x = bounds.x + HANDLE / 2.0;
        let inner_w = (bounds.width - HANDLE).max(1.0);
        ((x - inner_x) / inner_w).clamp(0.0, 1.0)
    }
}

impl<Message: Clone> Widget<Message, cosmic::Theme, cosmic::Renderer> for SmoothSeek<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        let now = Instant::now();
        tree::State::new(State {
            dragging: false,
            hover: Animation::new(false).quick(),
            anchor: Cell::new((self.progress, now)),
            drag_value: self.progress,
            now,
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, Length::Fixed(HEIGHT))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, self.width, Length::Fixed(HEIGHT))
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &cosmic::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let bounds = layout.bounds();

        // A new backend position re-anchors the extrapolation. Compared
        // against the anchor (not the extrapolated value) so an unchanged
        // `progress` across view rebuilds doesn't reset the clock.
        if (state.anchor.get().0 - self.progress).abs() > f32::EPSILON {
            state.anchor.set((self.progress, Instant::now()));
        }

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                if let Some(pos) = cursor.position_over(bounds) {
                    state.dragging = true;
                    state.drag_value = Self::locate(bounds, pos.x);
                    shell.publish((self.on_change)(state.drag_value));
                    shell.capture_event();
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Touch(touch::Event::FingerMoved { .. }) => {
                if state.dragging {
                    if let Some(pos) = cursor.land().position() {
                        let value = Self::locate(bounds, pos.x);
                        if (value - state.drag_value).abs() > f32::EPSILON {
                            state.drag_value = value;
                            shell.publish((self.on_change)(value));
                        }
                    }
                    shell.capture_event();
                    shell.request_redraw();
                }
                let hovered = state.dragging || cursor.is_over(bounds);
                if hovered != state.hover.value() {
                    state.hover.go_mut(hovered, Instant::now());
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. }) => {
                if state.dragging {
                    state.dragging = false;
                    // Hold the dropped position until the app reports the
                    // seek result, instead of flashing back to the old one.
                    state.anchor.set((state.drag_value, Instant::now()));
                    shell.publish(self.on_release.clone());
                    let hovered = cursor.is_over(bounds);
                    if hovered != state.hover.value() {
                        state.hover.go_mut(hovered, Instant::now());
                    }
                    shell.request_redraw();
                }
            }
            Event::Window(window::Event::RedrawRequested(now)) => {
                state.now = *now;
                if state.hover.is_animating(*now) {
                    shell.request_redraw();
                } else if self.animating() && !state.dragging {
                    // Redraw when the fill has moved about half a pixel:
                    // ~3 fps for a long track in a narrow bar, capped at
                    // 60 fps for short tracks in a wide one.
                    let px_per_sec = bounds.width.max(1.0) / self.duration_secs;
                    let interval = (0.5 / px_per_sec).clamp(1.0 / 60.0, 0.25);
                    shell.request_redraw_at(*now + Duration::from_secs_f32(interval));
                }
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let now = Instant::now().max(state.now);
        let bounds = layout.bounds();
        let cosmic = theme.cosmic();

        let hover: f32 = state.hover.interpolate(0.0, 1.0, now);
        let fill_color = self.accent.unwrap_or_else(|| cosmic.accent_color().into());
        let rail_color: Color = cosmic.background(false).component.divider.into();

        let thickness = RAIL_REST + (RAIL_ACTIVE - RAIL_REST) * hover;
        let inner_x = bounds.x + HANDLE / 2.0;
        let inner_w = (bounds.width - HANDLE).max(0.0);
        let rail_y = bounds.center_y() - thickness / 2.0;
        let value = self.displayed(state, now);
        let fill_w = inner_w * value;

        let quad = |x: f32, y: f32, w: f32, h: f32| renderer::Quad {
            bounds: Rectangle {
                x,
                y,
                width: w,
                height: h,
            },
            border: Border {
                radius: (h / 2.0).into(),
                ..Border::default()
            },
            ..renderer::Quad::default()
        };

        renderer.fill_quad(
            quad(inner_x, rail_y, inner_w, thickness),
            Background::Color(rail_color),
        );
        if fill_w > 0.0 {
            renderer.fill_quad(
                quad(inner_x, rail_y, fill_w.max(thickness), thickness),
                Background::Color(fill_color),
            );
        }

        // Handle grows in on hover rather than always sitting on the rail.
        let handle = HANDLE * hover;
        if handle > 0.5 {
            let cx = inner_x + fill_w;
            let cy = bounds.center_y();
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: cx - handle / 2.0,
                        y: cy - handle / 2.0,
                        width: handle,
                        height: handle,
                    },
                    border: Border {
                        radius: (handle / 2.0).into(),
                        ..Border::default()
                    },
                    shadow: cosmic::iced::Shadow {
                        color: Color::from_rgba(0.0, 0.0, 0.0, 0.3 * hover),
                        offset: cosmic::iced::Vector::new(0.0, 1.0),
                        blur_radius: 3.0,
                    },
                    ..renderer::Quad::default()
                },
                Background::Color(fill_color),
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if state.dragging {
            mouse::Interaction::Grabbing
        } else if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

impl<'a, Message: Clone + 'a> From<SmoothSeek<'a, Message>> for cosmic::Element<'a, Message> {
    fn from(widget: SmoothSeek<'a, Message>) -> Self {
        cosmic::Element::new(widget)
    }
}
