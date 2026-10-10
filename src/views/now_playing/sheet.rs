// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Slide-up sheet transition for the expanded now-playing view.
//!
//! Two thin wrappers sharing one animation curve:
//! - [`sheet`] slides its content up from the bottom edge when `open`
//!   becomes true (and back down when it becomes false);
//! - [`underlay`] wraps the library layout underneath and dims it while the
//!   sheet covers it, then stops drawing it (and routing input to it) once
//!   fully covered — without unmounting it, so its scroll positions survive.
//!
//! The animation runs entirely at draw time (`iced::Animation` + redraw
//! requests): the application's view tree is *not* rebuilt every frame,
//! which is what made the old 16ms `ExpandAnimTick` subscription costly.

use cosmic::iced::advanced::layout::{self, Layout};
use cosmic::iced::advanced::renderer::{self, Renderer as _};
use cosmic::iced::advanced::widget::{Operation, Tree, Widget, tree};
use cosmic::iced::advanced::{Clipboard, Shell, mouse, overlay};
use cosmic::iced::animation::Easing;
use cosmic::iced::time::Instant;
use cosmic::iced::{Animation, Background, Color, Event, Length, Rectangle, Size, Vector, window};
use std::time::Duration;

/// Shared transition length; the app waits this long before unmounting
/// the sheet after a collapse.
pub const DURATION: Duration = Duration::from_millis(280);

/// Maximum dim applied to the underlay when fully covered.
const MAX_DIM: f32 = 0.35;

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Sheet,
    Underlay,
}

pub struct Sheet<'a, Message> {
    content: cosmic::Element<'a, Message>,
    open: bool,
    role: Role,
    /// Whether the app is mid expand/collapse. A freshly mounted widget only
    /// animates in that case; otherwise it starts settled. Widget state is
    /// keyed by tree position, so unrelated layout changes (opening the
    /// context drawer, toggling fullscreen…) can remount it — that must not
    /// replay the slide.
    transitioning: bool,
}

struct State {
    anim: Animation<bool>,
}

/// Content that slides up over the window when `open`.
pub fn sheet<'a, Message>(
    content: impl Into<cosmic::Element<'a, Message>>,
    open: bool,
    transitioning: bool,
) -> Sheet<'a, Message> {
    Sheet {
        content: content.into(),
        open,
        role: Role::Sheet,
        transitioning,
    }
}

/// Content underneath a [`sheet`]; `covered` should be the sheet's `open`.
pub fn underlay<'a, Message>(
    content: impl Into<cosmic::Element<'a, Message>>,
    covered: bool,
    transitioning: bool,
) -> Sheet<'a, Message> {
    Sheet {
        content: content.into(),
        open: covered,
        role: Role::Underlay,
        transitioning,
    }
}

impl<Message> Sheet<'_, Message> {
    fn progress(state: &State, now: Instant) -> f32 {
        state.anim.interpolate(0.0, 1.0, now)
    }

    /// Underlay is fully hidden: skip drawing it and routing input to it.
    fn hidden(&self, state: &State, now: Instant) -> bool {
        self.role == Role::Underlay && state.anim.value() && !state.anim.is_animating(now)
    }

    /// The sheet only accepts input once it has settled open.
    fn inert(&self, state: &State, now: Instant) -> bool {
        match self.role {
            Role::Sheet => !state.anim.value() || state.anim.is_animating(now),
            Role::Underlay => self.hidden(state, now),
        }
    }

    fn offset(&self, state: &State, bounds: Rectangle, now: Instant) -> f32 {
        match self.role {
            Role::Sheet => (1.0 - Self::progress(state, now)) * bounds.height,
            Role::Underlay => 0.0,
        }
    }
}

impl<Message> Widget<Message, cosmic::Theme, cosmic::Renderer> for Sheet<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        // Mounted mid-transition: start from the opposite state and animate.
        // Otherwise start settled in the requested state (no replay).
        let initial = if self.transitioning {
            !self.open
        } else {
            self.open
        };
        let anim = Animation::new(initial)
            .duration(DURATION)
            .easing(Easing::EaseOutCubic)
            .go(self.open, Instant::now());
        tree::State::new(State { anim })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let now = Instant::now();
        let state = tree.state.downcast_mut::<State>();
        if state.anim.value() != self.open {
            state.anim.go_mut(self.open, now);
            shell.request_redraw();
        }
        if let Event::Window(window::Event::RedrawRequested(at)) = event
            && state.anim.is_animating(*at)
        {
            shell.request_redraw();
        }

        let inert = self.inert(state, now);
        // Redraw events must still reach the content (its own animations
        // and the seek bar depend on them); input must not while moving.
        let is_redraw = matches!(event, Event::Window(window::Event::RedrawRequested(_)));
        if inert && !is_redraw {
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let now = Instant::now();
        let state = tree.state.downcast_ref::<State>();
        if self.hidden(state, now) {
            return;
        }
        let bounds = layout.bounds();
        let cursor = if self.inert(state, now) {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };

        match self.role {
            Role::Sheet => {
                let dy = self.offset(state, bounds, now);
                // Once settled, draw directly: wrapping in a layer +
                // translation breaks custom `shader` primitives (the
                // projectM visualizer would render nothing).
                if dy.abs() < 0.5 {
                    self.content.as_widget().draw(
                        &tree.children[0],
                        renderer,
                        theme,
                        style,
                        layout,
                        cursor,
                        viewport,
                    );
                    return;
                }
                renderer.with_layer(bounds, |renderer| {
                    renderer.with_translation(Vector::new(0.0, dy), |renderer| {
                        self.content.as_widget().draw(
                            &tree.children[0],
                            renderer,
                            theme,
                            style,
                            layout,
                            cursor,
                            viewport,
                        );
                    });
                });
            }
            Role::Underlay => {
                self.content.as_widget().draw(
                    &tree.children[0],
                    renderer,
                    theme,
                    style,
                    layout,
                    cursor,
                    viewport,
                );
                let dim = Self::progress(state, now) * MAX_DIM;
                if dim > 0.001 {
                    renderer.with_layer(bounds, |renderer| {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds,
                                ..renderer::Quad::default()
                            },
                            Background::Color(Color::from_rgba(0.0, 0.0, 0.0, dim)),
                        );
                    });
                }
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        if self.inert(state, Instant::now()) {
            return mouse::Interaction::default();
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, cosmic::Theme, cosmic::Renderer>> {
        let state = tree.state.downcast_ref::<State>();
        if self.inert(state, Instant::now()) {
            return None;
        }
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message: 'a> From<Sheet<'a, Message>> for cosmic::Element<'a, Message> {
    fn from(widget: Sheet<'a, Message>) -> Self {
        cosmic::Element::new(widget)
    }
}
