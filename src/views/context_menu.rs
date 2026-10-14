// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Right-click menus for track rows and album cards/heroes.
//!
//! libcosmic's [`menu::items`] needs a payload-free, `Copy + Eq`
//! [`menu::action::MenuAction`], but every message of ours carries data (a
//! track index, a playlist id, …). So the menus are built in two steps:
//!
//! 1. **Selection** — the host view hands over a list of typed entries
//!    ([`TrackMenuAction`] / [`AlbumMenuAction`]); `aulos_core::menu`'s
//!    `track_menu_specs` / `album_menu_specs` decide which items appear, in
//!    which order and where the divider goes, producing plain [`MenuSpec`]
//!    data. That part is widget-free and unit-tested in core.
//! 2. **Rendering** — [`track_menu`] / [`album_menu`] turn the specs into
//!    [`menu::Tree`]s, replicating libcosmic's own `entry_tree` / folder /
//!    divider layout (icon column, ellipsised label, `MenuItem` /
//!    `MenuFolder` button class) with the messages cloned into the buttons.
//!
//! Wayland: the context menu deliberately does **not** call
//! `on_surface_action`, so libcosmic falls back to its overlay path (the same
//! way the app's menu bar renders) instead of spawning a popup surface.
//!
//! Performance: translated static labels are resolved once per process (see
//! `aulos_core::menu`; the UI language is fixed at startup) and handed to the
//! text widgets as `&'static str`, so a row only allocates for what is really
//! per-row — its messages and any playlist names.

use aulos_core::menu::{
    AlbumMenuAction, MenuSpec, TrackMenuAction, album_menu_specs, track_menu_specs,
};
use cosmic::widget::{self, menu};
use std::borrow::Cow;

/// Width of every menu item, and therefore of the menu: wide enough for
/// "Add to playlist" with its submenu arrow and for a typical playlist name
/// (longer names are ellipsised).
const ITEM_WIDTH: u16 = 240;

/// Size of the leading glyph of an item, matching libcosmic's `entry_tree`.
const ICON_SIZE: u16 = 14;

/// The track row context menu: [`track_menu_specs`] rendered to menu trees.
///
/// **Extension point (see [`track_menu_specs`]):** this is the single
/// function the track menu is assembled through.
#[must_use]
pub fn track_menu<M: Clone + 'static>(entries: Vec<TrackMenuAction<M>>) -> Vec<menu::Tree<M>> {
    trees(track_menu_specs(entries))
}

/// The album context menu: [`album_menu_specs`] rendered to menu trees.
#[must_use]
pub fn album_menu<M: Clone + 'static>(entries: Vec<AlbumMenuAction<M>>) -> Vec<menu::Tree<M>> {
    trees(album_menu_specs(entries))
}

// --- Rendering --------------------------------------------------------------

/// Wrap `content` so a right-click (or two-finger tap) opens the menu `menu`
/// builds.
///
/// `menu` is called only when the menu is actually requested — never while
/// building, diffing or laying out a view — so a list of thousands of rows
/// pays nothing per row for its menu beyond the closure itself (see
/// [`lazy`]). It should capture only cheap data (indices, `&'a` borrows).
pub fn with_menu<'a, M: Clone + 'static>(
    content: impl Into<cosmic::Element<'a, M>>,
    menu: impl Fn() -> Vec<menu::Tree<M>> + 'a,
) -> cosmic::Element<'a, M> {
    lazy::LazyMenu {
        content: content.into(),
        build: Box::new(menu),
    }
    .into()
}

/// The lazily built context-menu wrapper behind [`with_menu`].
///
/// libcosmic's [`widget::context_menu`] takes the whole `Vec<menu::Tree>` at
/// construction and then clones it into the widget state, flattens it into
/// per-item widget trees (`children`) and re-diffs it on every view rebuild,
/// all per row. This wrapper keeps *none* of that in the row: its widget tree
/// is just the content's. On the first trigger event over its bounds (a right
/// button release, or the first finger of a touch) it builds the menu and
/// wraps it in a real `widget::context_menu` kept in its state; from then on
/// it forwards every event to that inner widget first and exposes its overlay,
/// so open/close, item hover/selection, submenus, Escape and click-outside
/// all run through libcosmic's own code, exactly as before.
///
/// The inner widget is rebuilt on every trigger, so a state slot that a
/// virtualized list later reuses for another row can never open the previous
/// row's menu.
mod lazy {
    use super::ITEM_WIDTH;
    use cosmic::iced::advanced::clipboard::{Clipboard, DndDestinationRectangles};
    use cosmic::iced::advanced::layout::{self, Layout};
    use cosmic::iced::advanced::widget::{Operation, Tree, Widget, tree};
    use cosmic::iced::advanced::{Shell, mouse, overlay, renderer};
    use cosmic::iced::touch::Finger;
    use cosmic::iced::{Event, Length, Rectangle, Size, Vector, touch};
    use cosmic::widget::{self, menu};
    use std::collections::HashSet;

    pub(super) struct LazyMenu<'a, M> {
        pub(super) content: cosmic::Element<'a, M>,
        pub(super) build: Box<dyn Fn() -> Vec<menu::Tree<M>> + 'a>,
    }

    /// A built menu: libcosmic's context-menu widget (over an empty body,
    /// since only its event handling and overlay are used) and its tree.
    struct Open<M> {
        element: cosmic::Element<'static, M>,
        tree: Tree,
    }

    struct State<M> {
        open: Option<Open<M>>,
        /// Fingers currently down over the row, to spot the *first* one.
        fingers: HashSet<Finger>,
    }

    impl<M: Clone + 'static> Open<M> {
        fn new(menus: Vec<menu::Tree<M>>) -> Self {
            let mut element: cosmic::Element<'static, M> =
                widget::context_menu(widget::Space::new(), Some(menus))
                    .item_width(menu::ItemWidth::Uniform(ITEM_WIDTH))
                    .into();
            let mut tree = Tree::new(&element);
            // libcosmic fills the menu's own widget tree in `diff`, which the
            // runtime normally runs right after a view is built; the menu
            // overlay panics without it.
            element.as_widget_mut().diff(&mut tree);
            Self { element, tree }
        }
    }

    impl<M: Clone + 'static> Widget<M, cosmic::Theme, cosmic::Renderer> for LazyMenu<'_, M> {
        fn tag(&self) -> tree::Tag {
            tree::Tag::of::<State<M>>()
        }

        fn state(&self) -> tree::State {
            tree::State::new(State::<M> {
                open: None,
                fingers: HashSet::new(),
            })
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
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        }

        fn mouse_interaction(
            &self,
            tree: &Tree,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            viewport: &Rectangle,
            renderer: &cosmic::Renderer,
        ) -> mouse::Interaction {
            self.content.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
        }

        fn drag_destinations(
            &self,
            tree: &Tree,
            layout: Layout<'_>,
            renderer: &cosmic::Renderer,
            dnd_rectangles: &mut DndDestinationRectangles,
        ) {
            self.content.as_widget().drag_destinations(
                &tree.children[0],
                layout,
                renderer,
                dnd_rectangles,
            );
        }

        fn operate(
            &mut self,
            tree: &mut Tree,
            layout: Layout<'_>,
            renderer: &cosmic::Renderer,
            operation: &mut dyn Operation,
        ) {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                operation,
            );
        }

        fn update(
            &mut self,
            tree: &mut Tree,
            event: &Event,
            layout: Layout<'_>,
            cursor: mouse::Cursor,
            renderer: &cosmic::Renderer,
            clipboard: &mut dyn Clipboard,
            shell: &mut Shell<'_, M>,
            viewport: &Rectangle,
        ) {
            let state = tree.state.downcast_mut::<State<M>>();
            let over = cursor.is_over(layout.bounds());

            // libcosmic opens on right-button *release* and on the lift of a
            // two-finger tap; build the menu just before the inner widget
            // sees the first event of either gesture.
            match event {
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Right)) if over => {
                    state.open = Some(Open::new((self.build)()));
                }
                Event::Touch(touch::Event::FingerPressed { id, .. }) if over => {
                    if state.fingers.is_empty() {
                        state.open = Some(Open::new((self.build)()));
                    }
                    state.fingers.insert(*id);
                }
                Event::Touch(
                    touch::Event::FingerLifted { id, .. } | touch::Event::FingerLost { id, .. },
                ) => {
                    state.fingers.remove(id);
                }
                _ => {}
            }

            // The inner widget goes first, as it did when it wrapped the
            // content directly: opening captures the event, and the content
            // must not also react to it.
            if let Some(open) = &mut state.open {
                let captured_before = shell.is_event_captured();
                open.element.as_widget_mut().update(
                    &mut open.tree,
                    event,
                    layout,
                    cursor,
                    renderer,
                    clipboard,
                    shell,
                    viewport,
                );
                if !captured_before && shell.is_event_captured() {
                    return;
                }
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

        fn overlay<'b>(
            &'b mut self,
            tree: &'b mut Tree,
            layout: Layout<'b>,
            renderer: &cosmic::Renderer,
            viewport: &Rectangle,
            translation: Vector,
        ) -> Option<overlay::Element<'b, M, cosmic::Theme, cosmic::Renderer>> {
            let content = self.content.as_widget_mut().overlay(
                &mut tree.children[0],
                layout,
                renderer,
                viewport,
                translation,
            );
            let menu = tree
                .state
                .downcast_mut::<State<M>>()
                .open
                .as_mut()
                .and_then(|open| {
                    open.element.as_widget_mut().overlay(
                        &mut open.tree,
                        layout,
                        renderer,
                        viewport,
                        translation,
                    )
                });
            match (content, menu) {
                (Some(content), Some(menu)) => {
                    Some(overlay::Group::with_children(vec![content, menu]).overlay())
                }
                (content, menu) => content.or(menu),
            }
        }
    }

    impl<'a, M: Clone + 'static> From<LazyMenu<'a, M>> for cosmic::Element<'a, M> {
        fn from(widget: LazyMenu<'a, M>) -> Self {
            cosmic::Element::new(widget)
        }
    }
}

fn trees<M: Clone + 'static>(specs: Vec<MenuSpec<M>>) -> Vec<menu::Tree<M>> {
    specs
        .into_iter()
        .map(|spec| match spec {
            MenuSpec::Item {
                label,
                icon,
                message,
            } => menu::Tree::from(cosmic::Element::from(
                menu::menu_button(row_content(label, icon, None)).on_press(message),
            )),
            MenuSpec::Folder {
                label,
                icon,
                children,
            } => {
                let button = menu::menu_button(row_content(
                    label,
                    icon,
                    Some(widget::icon::from_name("pan-end-symbolic").size(16).icon()),
                ))
                // Folders have no `on_press`; `MenuFolder` keeps them styled
                // as enabled, as libcosmic's own folders are.
                .class(cosmic::theme::Button::MenuFolder);
                menu::Tree::with_children(cosmic::Element::from(button), trees(children))
            }
            MenuSpec::Divider => {
                menu::Tree::from(cosmic::Element::from(widget::divider::horizontal::light()))
            }
        })
        .collect()
}

/// Leading glyph (when `icon`), ellipsised label, then the optional trailing
/// `submenu_arrow` pushed to the right edge — the layout of libcosmic's own
/// `entry_tree` and folder items.
fn row_content<M: 'static>(
    label: Cow<'static, str>,
    icon: Option<&'static str>,
    submenu_arrow: Option<widget::icon::Icon>,
) -> Vec<cosmic::Element<'static, M>> {
    use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit};

    let mut content: Vec<cosmic::Element<'static, M>> = Vec::with_capacity(5);
    if let Some(name) = icon {
        content.push(
            widget::icon::icon(widget::icon::from_name(name).handle())
                .size(ICON_SIZE)
                .into(),
        );
        content.push(
            widget::Space::new()
                .width(cosmic::theme::spacing().space_xxs)
                .into(),
        );
    }
    content.push(
        widget::text(label)
            .ellipsize(Ellipsize::Middle(EllipsizeHeightLimit::Lines(1)))
            .into(),
    );
    if let Some(arrow) = submenu_arrow {
        content.push(
            widget::Space::new()
                .width(cosmic::iced::Length::Fill)
                .into(),
        );
        content.push(arrow.into());
    }
    content
}
