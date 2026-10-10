// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! "Karaoke" follower for synced lyrics.
//!
//! Lays out a column of lyric lines at full height, then draws it shifted so
//! that the `focus` line sits at the vertical centre of the viewport. When
//! the focus changes, the shift glides to the new line (`iced::Animation`,
//! ease-out) instead of jumping — the Apple Music / Spotify lyrics feel. The
//! animation runs at draw time, so the app view is only rebuilt when the
//! current line actually changes.

use cosmic::iced::advanced::layout::{self, Layout};
use cosmic::iced::advanced::renderer::{self, Renderer as _};
use cosmic::iced::advanced::widget::{Tree, Widget, tree};
use cosmic::iced::advanced::{Clipboard, Shell, mouse};
use cosmic::iced::animation::Easing;
use cosmic::iced::time::Instant;
use cosmic::iced::{Animation, Event, Length, Rectangle, Size, Vector, window};
use std::time::Duration;

const GLIDE: Duration = Duration::from_millis(450);

pub struct Follow<'a, Message> {
    content: cosmic::Element<'a, Message>,
    /// Index of the child (line) to centre; `None` centres the first line.
    focus: Option<usize>,
}

struct State {
    shift: Animation<f32>,
    /// Shift the animation is heading to (NaN until first layout).
    target: f32,
}

/// Centre line `focus` of `column` (a `Column` of one element per line).
pub fn follow<'a, Message>(
    column: impl Into<cosmic::Element<'a, Message>>,
    focus: Option<usize>,
) -> Follow<'a, Message> {
    Follow {
        content: column.into(),
        focus,
    }
}

impl<Message> Follow<'_, Message> {
    /// Shift (px, positive = move content down) that centres the focus line.
    fn target_shift(&self, layout: Layout<'_>) -> f32 {
        let bounds = layout.bounds();
        let Some(column) = layout.children().next() else {
            return 0.0;
        };
        let line = column
            .children()
            .nth(self.focus.unwrap_or(0))
            .or_else(|| column.children().last());
        match line {
            Some(line) => bounds.height / 2.0 - (line.bounds().center_y() - bounds.y),
            None => 0.0,
        }
    }
}

impl<Message> Widget<Message, cosmic::Theme, cosmic::Renderer> for Follow<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            shift: Animation::new(0.0)
                .duration(GLIDE)
                .easing(Easing::EaseOutCubic),
            target: f32::NAN,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        // Lay the lines out at their natural height, however tall.
        let child_limits = layout::Limits::new(Size::ZERO, Size::new(size.width, f32::INFINITY));
        let child =
            self.content
                .as_widget_mut()
                .layout(&mut tree.children[0], renderer, &child_limits);
        layout::Node::with_children(size, vec![child])
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &cosmic::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let Event::Window(window::Event::RedrawRequested(now)) = event else {
            return;
        };
        let state = tree.state.downcast_mut::<State>();
        let target = self.target_shift(layout);
        if state.target.is_nan() {
            // First frame: start on the current line, no glide from the top.
            state.shift = Animation::new(target)
                .duration(GLIDE)
                .easing(Easing::EaseOutCubic);
            state.target = target;
        } else if (target - state.target).abs() > 0.5 {
            // Retarget from wherever the glide currently is.
            let from = state.shift.interpolate_with(|v| v, *now);
            state.shift = Animation::new(from)
                .duration(GLIDE)
                .easing(Easing::EaseOutCubic)
                .go(target, *now);
            state.target = target;
        }
        if state.shift.is_animating(*now) {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let shift = if state.target.is_nan() {
            self.target_shift(layout)
        } else {
            state.shift.interpolate_with(|v| v, Instant::now())
        };
        let Some(child) = layout.children().next() else {
            return;
        };
        // What the child sees as visible, in its own (unshifted) coordinates.
        let child_viewport = Rectangle {
            y: bounds.y - shift,
            ..bounds
        };
        renderer.with_layer(bounds, |renderer| {
            renderer.with_translation(Vector::new(0.0, shift), |renderer| {
                self.content.as_widget().draw(
                    &tree.children[0],
                    renderer,
                    theme,
                    style,
                    child,
                    cursor,
                    &child_viewport,
                );
            });
        });
    }
}

impl<'a, Message: 'a> From<Follow<'a, Message>> for cosmic::Element<'a, Message> {
    fn from(widget: Follow<'a, Message>) -> Self {
        cosmic::Element::new(widget)
    }
}
