// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Shared UI helpers for library views.
//!
//! Every view renders durations, single-line table cells, star ratings and
//! favorite toggles through these helpers so the whole app stays visually
//! consistent.

use std::borrow::Cow;

use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit, Wrapping};
use cosmic::iced::{Alignment, Color, ContentFit, Length};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;
use cosmic::widget::tooltip::Position as TooltipPosition;

use crate::fl;
use crate::library::Playlist;

/// Concrete text widget type returned by `cosmic::widget::text::*` helpers.
pub type Text<'a> = cosmic::iced::widget::Text<'a, cosmic::Theme, cosmic::Renderer>;

/// Fixed width for right-aligned duration cells (fits `H:MM:SS`).
pub const DURATION_WIDTH: f32 = 64.0;

/// Format a duration in whole seconds as `H:MM:SS` when at least an hour,
/// otherwise `M:SS`.
#[must_use]
pub fn format_duration(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Coarse human-readable duration for headers: `2h 44m`, `44m`, `12s`.
#[must_use]
pub fn format_duration_coarse(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{total_secs}s")
    }
}

/// Truncate on a `char` boundary, appending `…` when the input exceeds
/// `max_chars`.
#[must_use]
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", truncated.trim_end())
    }
}

/// Body text pinned to a single line, ending in "…" when it doesn't fit
/// (the containing cell still clips as a safety net).
pub fn cell_text<'a>(content: impl Into<Cow<'a, str>> + 'a) -> Text<'a> {
    widget::text::body(content)
        .wrapping(Wrapping::None)
        .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
}

/// Caption (small, dim) text pinned to a single line, ellipsized like
/// [`cell_text`].
pub fn cell_caption<'a>(content: impl Into<Cow<'a, str>> + 'a) -> Text<'a> {
    widget::text::caption(content)
        .wrapping(Wrapping::None)
        .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)))
}

/// Right-aligned, fixed-width duration cell for track rows.
pub fn duration_cell<'a, M: 'a>(total_secs: u64) -> cosmic::Element<'a, M> {
    widget::container(cell_caption(format_duration(total_secs)))
        .width(Length::Fixed(DURATION_WIDTH))
        .align_x(Horizontal::Right)
        .into()
}

/// Wrap `content` in a clipped, width-filling, vertically-centered
/// container so single-line cell text (which never wraps) can never paint
/// past the column boundary its parent row assigns it.
pub fn clipped_cell<'a, M: 'a>(content: cosmic::Element<'a, M>) -> cosmic::Element<'a, M> {
    widget::container(content)
        .width(Length::Fill)
        .align_y(Vertical::Center)
        .clip(true)
        .into()
}

/// Single-line clickable text that navigates somewhere (artist, album,
/// genre…). Reads as plain text — `secondary` dims it like the caption
/// columns — and turns accent-coloured on hover, like a web link, so
/// interlinked metadata is discoverable without cluttering the layout.
/// Nested inside a row button, it captures its own click, so the row's
/// action (usually "play") only fires outside the link.
pub fn link<'a, M: Clone + 'a>(
    content: Text<'a>,
    secondary: bool,
    on_press: M,
) -> cosmic::Element<'a, M> {
    link_styled(content, on_press, move |theme, hovered| {
        let cosmic = theme.cosmic();
        if hovered {
            cosmic.accent_text_color().into()
        } else if secondary {
            cosmic.palette.neutral_7.into()
        } else {
            cosmic.background(false).component.on.into()
        }
    })
}

/// [`link`] for text drawn on a fixed-colour backdrop (e.g. the blurred
/// cover behind the expanded now-playing view): `color` at rest, fully
/// opaque on hover, independent of the theme.
pub fn link_on<'a, M: Clone + 'a>(
    content: Text<'a>,
    color: Color,
    on_press: M,
) -> cosmic::Element<'a, M> {
    link_styled(content, on_press, move |_, hovered| {
        if hovered {
            Color { a: 1.0, ..color }
        } else {
            color
        }
    })
}

fn link_styled<'a, M: Clone + 'a>(
    content: Text<'a>,
    on_press: M,
    color: impl Fn(&cosmic::Theme, bool) -> Color + Copy + 'static,
) -> cosmic::Element<'a, M> {
    let style = move |theme: &cosmic::Theme, hovered: bool| ButtonStyle {
        background: None,
        text_color: Some(color(theme, hovered)),
        icon_color: Some(color(theme, hovered)),
        ..ButtonStyle::new()
    };
    widget::button::custom(content)
        .padding(0)
        .on_press(on_press)
        .class(cosmic::theme::Button::Custom {
            active: Box::new(move |_, theme| style(theme, false)),
            hovered: Box::new(move |_, theme| style(theme, true)),
            pressed: Box::new(move |_, theme| style(theme, true)),
            disabled: Box::new(move |theme| style(theme, false)),
        })
        .into()
}

/// Button class that fills with the cover-art accent colour instead of the
/// theme accent, mirroring `Button::Suggested`'s filled/pill shape via
/// `radius_xl` corners so the play/pause button stays visually consistent
/// while matching the current artwork. Hover/pressed/disabled states dim
/// the same fill via alpha rather than deriving separate colours from the
/// palette.
pub fn accent_button_class(accent: &crate::library::palette::Accent) -> cosmic::theme::Button {
    let background = Color::from_rgb(accent.color[0], accent.color[1], accent.color[2]);
    let on = Color::from_rgb(accent.on_color[0], accent.on_color[1], accent.on_color[2]);
    let style = move |theme: &cosmic::Theme, alpha: f32| cosmic::widget::button::Style {
        background: Some(Background::Color(Color {
            a: alpha,
            ..background
        })),
        text_color: Some(on),
        icon_color: Some(on),
        border_radius: theme.cosmic().corner_radii.radius_xl.into(),
        ..cosmic::widget::button::Style::new()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| style(theme, 1.0)),
        hovered: Box::new(move |_focused, theme| style(theme, 0.85)),
        pressed: Box::new(move |_focused, theme| style(theme, 0.7)),
        disabled: Box::new(move |theme| style(theme, 0.5)),
    }
}

/// Height of the hero header on album/artist detail pages.
pub const HERO_HEIGHT: f32 = 280.0;

/// Detail-page hero header: `content` (cover, title, actions) laid over the
/// album's blurred artwork, washed toward the window background so theme
/// text stays legible, and tinted with the cover's accent colour. Without
/// artwork it degrades to an accent glow, and without either to a plain
/// card — so the layout never changes as artwork finishes loading.
pub fn hero_header<'a, M: 'a>(
    blurred: Option<&widget::icon::Handle>,
    accent: Option<&crate::library::palette::Accent>,
    back: Option<cosmic::Element<'a, M>>,
    content: cosmic::Element<'a, M>,
) -> cosmic::Element<'a, M> {
    use cosmic::iced::gradient::Linear;
    use cosmic::iced::{Gradient, Radians};

    let radius = cosmic::theme::active().cosmic().corner_radii.radius_l[0];
    let accent = accent.map(|a| Color::from_rgb(a.color[0], a.color[1], a.color[2]));
    let has_art = blurred.is_some();

    let mut stack = cosmic::iced::widget::Stack::new()
        .width(Length::Fill)
        .height(Length::Fixed(HERO_HEIGHT))
        // Base layer fixes the stack's size.
        .push(
            widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fixed(HERO_HEIGHT)),
        );

    if let Some(widget::icon::Data::Image(image)) = blurred.map(|h| &h.data) {
        stack = stack.push(
            widget::container(
                widget::image(image.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(ContentFit::Cover)
                    .border_radius([radius; 4]),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true),
        );
    }

    // Veil: wash the artwork toward the page background (top lighter,
    // bottom nearly opaque), with an accent tint near the top.
    stack = stack.push(
        widget::container(
            widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .class(cosmic::theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let bg: Color = cosmic.background(false).base.into();
            let top = match (has_art, accent) {
                (true, Some(a)) => mix(Color { a: 0.55, ..bg }, Color { a: 0.55, ..a }, 0.35),
                (true, None) => Color { a: 0.5, ..bg },
                (false, Some(a)) => Color { a: 0.28, ..a },
                (false, None) => cosmic.background(false).component.base.into(),
            };
            // With artwork, fade fully into the page so the hero has no
            // visible bottom edge.
            let bottom = if has_art {
                bg
            } else if accent.is_some() {
                Color { a: 0.0, ..bg }
            } else {
                top
            };
            cosmic::iced::widget::container::Style {
                background: Some(Background::Gradient(Gradient::Linear(
                    Linear::new(Radians(std::f32::consts::PI))
                        .add_stop(0.0, top)
                        .add_stop(1.0, bottom),
                ))),
                border: cosmic::iced::Border {
                    radius: radius.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })),
    );

    stack = stack.push(
        widget::container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(24)
            .align_y(Vertical::Bottom),
    );
    if let Some(back) = back {
        stack = stack.push(
            widget::container(back)
                .padding(12)
                .align_x(Horizontal::Left)
                .align_y(Vertical::Top),
        );
    }
    stack.into()
}

/// Round, translucent "back" button for the top-left corner of a
/// [`hero_header`], with a tooltip.
pub fn hero_back_button<'a, M: Clone + 'static>(
    label: String,
    on_press: M,
) -> cosmic::Element<'a, M> {
    widget::tooltip(
        widget::button::icon(widget::icon::from_name("go-previous-symbolic").size(16))
            .on_press(on_press)
            .padding(8)
            .class(cosmic::theme::Button::Custom {
                active: Box::new(|_, theme| hero_back_style(theme, 0.35)),
                hovered: Box::new(|_, theme| hero_back_style(theme, 0.55)),
                pressed: Box::new(|_, theme| hero_back_style(theme, 0.7)),
                disabled: Box::new(|theme| hero_back_style(theme, 0.2)),
            }),
        widget::text::caption(label),
        TooltipPosition::Bottom,
    )
    .into()
}

fn hero_back_style(theme: &cosmic::Theme, alpha: f32) -> ButtonStyle {
    let cosmic = theme.cosmic();
    let bg: Color = cosmic.background(false).base.into();
    let on: Color = cosmic.background(false).on.into();
    ButtonStyle {
        background: Some(Background::Color(Color { a: alpha, ..bg })),
        text_color: Some(on),
        icon_color: Some(on),
        border_radius: cosmic.corner_radii.radius_xl.into(),
        ..ButtonStyle::new()
    }
}

/// Linear blend of two colours (`t` = share of `b`).
fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// [`link`] over single-line body text for a table cell that may be empty:
/// plain (non-clickable) text when `content` is blank, so missing metadata
/// never becomes a link.
pub fn link_cell<'a, M: Clone + 'a>(
    content: &'a str,
    secondary: bool,
    on_press: impl FnOnce() -> M,
) -> cosmic::Element<'a, M> {
    if content.trim().is_empty() {
        cell_text(content).into()
    } else {
        link(cell_text(content), secondary, on_press())
    }
}

/// Compact interactive five-star rating (roughly 100px wide).
///
/// Clicking a star sets the rating; clicking the current rating clears it
/// (sends `0`). Filled stars are tinted with the accent color.
pub fn star_rating<'a, M: Clone + 'static>(
    rating: Option<u8>,
    on_rate: impl Fn(u8) -> M,
) -> cosmic::Element<'a, M> {
    let current = rating.unwrap_or(0);
    let mut row = widget::Row::new().align_y(Alignment::Center);
    for star in 1u8..=5 {
        let icon_name = if star <= current {
            "starred-symbolic"
        } else {
            "non-starred-symbolic"
        };
        let new_rating = if star == current { 0 } else { star };
        row = row.push(
            widget::button::icon(widget::icon::from_name(icon_name).size(14))
                .padding(2)
                .selected(star <= current)
                .on_press(on_rate(new_rating)),
        );
    }
    row.into()
}

/// Button class for the favorite heart: `Button::Icon`'s built-in
/// `selected()` flag never actually recolors the icon (its style always
/// discards the resolved icon color unless the button is disabled), so both
/// states render identically. This custom class controls the color
/// directly: muted when not favorited, accent-tinted when favorited.
fn favorite_button_class(is_favorite: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let color = if is_favorite {
                cosmic.accent_color()
            } else {
                cosmic.icon_button.on_disabled
            };
            ButtonStyle {
                background: None,
                text_color: Some(color.into()),
                icon_color: Some(color.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        hovered: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.icon_button;
            let color = if is_favorite {
                cosmic.accent_color()
            } else {
                comp.on
            };
            ButtonStyle {
                background: Some(Background::Color(comp.hover.into())),
                text_color: Some(color.into()),
                icon_color: Some(color.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        pressed: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.icon_button;
            let color = if is_favorite {
                cosmic.accent_color()
            } else {
                comp.on
            };
            ButtonStyle {
                background: Some(Background::Color(comp.pressed.into())),
                text_color: Some(color.into()),
                icon_color: Some(color.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        disabled: Box::new(|theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.icon_button;
            ButtonStyle {
                background: None,
                text_color: Some(comp.on_disabled.into()),
                icon_color: Some(comp.on_disabled.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
    }
}

/// Heart-shaped favorite toggle with a tooltip.
///
/// Accent-tinted when favorited, visibly muted otherwise — the off state is
/// never confusable with the on state, and never relies on hover alone.
pub fn favorite_button<'a, M: Clone + 'static>(
    is_favorite: bool,
    on_toggle: M,
) -> cosmic::Element<'a, M> {
    let button = widget::button::icon(widget::icon::from_name("emblem-favorite-symbolic").size(16))
        .padding(4)
        .selected(is_favorite)
        .class(favorite_button_class(is_favorite))
        .on_press(on_toggle);
    widget::tooltip(
        button,
        widget::text::caption(if is_favorite {
            fl!("favorite-remove")
        } else {
            fl!("favorite-add")
        }),
        TooltipPosition::Top,
    )
    .into()
}

/// Fixed width for the audio-quality pill: every known tier's icon+label
/// combination fits within it, so a column of these badges never resizes
/// row-to-row the way an unconstrained label would (`"HI-RES"` is much
/// wider than `"CD"`).
pub const QUALITY_BADGE_WIDTH: f32 = 64.0;

/// Compact pill marking a track or album as high-resolution (Hi-Res / DSD).
///
/// Only premium tiers get a badge: CD-quality and lossy are the norm for
/// most libraries, and labelling every row "CD"/"LOSSY" was pure noise.
/// Anything else renders a zero-size element; callers still wrap the result
/// in a fixed-width container so columns never jitter.
pub fn quality_badge<'a, M: 'static>(
    quality: crate::library::quality::AudioQuality,
) -> cosmic::Element<'a, M> {
    use crate::library::quality::AudioQuality;
    if !matches!(quality, AudioQuality::HiRes | AudioQuality::Dsd) {
        return widget::Space::new().into();
    }
    widget::container(
        widget::text::caption(quality.label())
            .wrapping(Wrapping::None)
            .font(cosmic::font::semibold()),
    )
    .padding([1, 8])
    .class(cosmic::theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let accent: Color = cosmic.accent_text_color().into();
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(Color { a: 0.14, ..accent })),
            text_color: Some(accent),
            border: cosmic::iced::Border {
                radius: cosmic.corner_radii.radius_xl.into(),
                color: Color { a: 0.35, ..accent },
                width: 1.0,
            },
            ..Default::default()
        }
    }))
    .into()
}

/// Icon button wrapped in a caption tooltip — for transport/utility controls.
pub fn icon_button<'a, M: Clone + 'static>(
    icon_name: &'static str,
    icon_size: u16,
    label: &'a str,
    on_press: M,
) -> cosmic::Element<'a, M> {
    widget::tooltip(
        widget::button::icon(widget::icon::from_name(icon_name).size(icon_size)).on_press(on_press),
        widget::text::caption(label),
        TooltipPosition::Top,
    )
    .into()
}

/// Centered empty-state placeholder with an icon, title and hint.
pub fn empty_state<'a, M: 'static>(
    icon_name: &'static str,
    title: impl Into<Cow<'a, str>> + 'a,
    subtitle: impl Into<Cow<'a, str>> + 'a,
) -> cosmic::Element<'a, M> {
    widget::container(
        widget::Column::new()
            .push(
                widget::icon::icon(widget::icon::from_name(icon_name).handle())
                    .size(56)
                    .class(cosmic::theme::Svg::custom(|theme| {
                        cosmic::iced::widget::svg::Style {
                            color: Some(theme.cosmic().palette.neutral_6.into()),
                        }
                    })),
            )
            .push(widget::text::title3(title))
            .push(widget::text::body(subtitle))
            .spacing(8)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Horizontal::Center)
    .align_y(Vertical::Center)
    .into()
}

/// Shared header for card-grid views: a right-aligned view-mode toggle
/// button that flips between grid and list icons/labels for the current
/// mode. Used by every card-grid view (albums, artists, genres) so the
/// toggle's placement and wording never drift between them.
pub fn view_mode_toggle_header<'a, M: Clone + 'static>(
    mode: crate::config::ViewMode,
    on_toggle: M,
) -> cosmic::Element<'a, M> {
    use crate::config::ViewMode;
    let toggle_icon = match mode {
        ViewMode::Grid => "view-list-symbolic",
        ViewMode::List => "view-grid-symbolic",
    };
    let toggle_label = match mode {
        ViewMode::Grid => fl!("switch-to-list"),
        ViewMode::List => fl!("switch-to-grid"),
    };
    let toggle_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name(toggle_icon).size(16)).on_press(on_toggle),
        widget::text::caption(toggle_label),
        TooltipPosition::Bottom,
    );
    widget::Row::new()
        .push(widget::Space::new().width(Length::Fill))
        .push(toggle_btn)
        .padding(16)
        .into()
}

/// Rounded cover artwork at `size`×`size`, optionally lifted off the
/// surface with a soft drop shadow.
///
/// Raster covers are drawn through `widget::image` (which supports a
/// native corner radius) using the very same `image::Handle` the icon
/// handle wraps, so the GPU texture cache is shared rather than doubled;
/// anything else (SVG/symbolic) falls back to a plain icon.
pub fn cover_art<'a, M: 'static>(
    handle: &widget::icon::Handle,
    size: f32,
    radius: f32,
    elevated: bool,
) -> cosmic::Element<'a, M> {
    let art: cosmic::Element<'a, M> = match &handle.data {
        widget::icon::Data::Image(image) => widget::image(image.clone())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .content_fit(ContentFit::Cover)
            .border_radius([radius; 4])
            .into(),
        widget::icon::Data::Svg(_) => widget::icon::icon(handle.clone()).size(size as u16).into(),
    };
    if !elevated {
        return art;
    }
    widget::container(art)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .class(cosmic::theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            cosmic::iced::widget::container::Style {
                // Painted under the cover only so the shadow quad has a
                // body; never visible once the artwork is drawn on top.
                background: Some(Background::Color(
                    cosmic.background(false).component.base.into(),
                )),
                border: cosmic::iced::Border {
                    radius: radius.into(),
                    ..Default::default()
                },
                shadow: cosmic::iced::Shadow {
                    color: Color::from_rgba(
                        0.0,
                        0.0,
                        0.0,
                        if cosmic.is_dark { 0.45 } else { 0.22 },
                    ),
                    offset: cosmic::iced::Vector::new(0.0, 4.0),
                    blur_radius: 14.0,
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Grid-card artwork tile: the cached cover/avatar, rounded and elevated,
/// or a card-styled placeholder frame with a 64px fallback icon when
/// nothing is cached yet. Shared by every card grid (albums, artists) so a
/// missing cover's frame never differs from the album/artist that has one.
pub fn grid_art_tile<'a, M: 'static>(
    handle: Option<&widget::icon::Handle>,
    size: u16,
    placeholder_icon: &'static str,
) -> cosmic::Element<'a, M> {
    let radius = cosmic::theme::active().cosmic().corner_radii.radius_m[0];
    match handle {
        Some(handle) => cover_art(handle, f32::from(size), radius, true),
        None => {
            let placeholder: cosmic::Element<'a, M> =
                widget::icon::icon(widget::icon::from_name(placeholder_icon).handle())
                    .size(size / 3)
                    .class(cosmic::theme::Svg::custom(|theme| {
                        cosmic::iced::widget::svg::Style {
                            color: Some(theme.cosmic().palette.neutral_6.into()),
                        }
                    }))
                    .into();
            widget::container(placeholder)
                .width(f32::from(size))
                .height(f32::from(size))
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center)
                .class(cosmic::theme::Container::Card)
                .into()
        }
    }
}

/// List-row artwork: the cached cover/avatar at `size`, with small rounded
/// corners, or an unstyled fallback icon at the same size when nothing is
/// cached.
pub fn list_art_icon<'a, M: 'static>(
    handle: Option<&widget::icon::Handle>,
    size: u16,
    placeholder_icon: &'static str,
) -> cosmic::Element<'a, M> {
    match handle {
        Some(handle) => {
            let radius = cosmic::theme::active().cosmic().corner_radii.radius_s[0];
            cover_art(handle, f32::from(size), radius, false)
        }
        // Same footprint as real artwork: a rounded card tile with a
        // smaller, dimmed glyph, instead of a bare full-size white icon.
        None => widget::container(
            widget::icon::icon(widget::icon::from_name(placeholder_icon).handle())
                .size(size / 2)
                .class(cosmic::theme::Svg::custom(|theme| {
                    cosmic::iced::widget::svg::Style {
                        color: Some(theme.cosmic().palette.neutral_6.into()),
                    }
                })),
        )
        .width(f32::from(size))
        .height(f32::from(size))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::Card)
        .into(),
    }
}

/// Horizontal space a scrollable's scrollbar may overlay on the right.
const GRID_SCROLLBAR_CLEARANCE: f32 = 16.0;

/// Allowed range for the grid card size multiplier.
pub const GRID_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.7..=1.6;

/// Current grid card size multiplier, mirrored from `Config::grid_scale`
/// by the app (stored as f32 bits) so every `fluid_card_grid` call honours
/// it without threading the value through each view's signature.
static GRID_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000); // 1.0

pub fn set_grid_scale(scale: f32) {
    let scale = scale.clamp(*GRID_SCALE_RANGE.start(), *GRID_SCALE_RANGE.end());
    GRID_SCALE.store(scale.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

pub fn grid_scale() -> f32 {
    f32::from_bits(GRID_SCALE.load(std::sync::atomic::Ordering::Relaxed))
}

/// Scrollable card grid that fills the available width exactly.
///
/// Fits as many columns as possible at `min_card` (outer card width,
/// including the card button's own padding), then stretches every card —
/// up to `max_card` — so rows span the full width with uniform gaps,
/// instead of a fixed-size `flex_row` leaving ragged, jumpy margins as the
/// window resizes. The last row stays left-aligned with the rows above it.
///
/// `make_card(index, card_width)` builds card `index` at the given outer
/// width.
pub fn fluid_card_grid<'a, M: 'a>(
    count: usize,
    min_card: f32,
    max_card: f32,
    make_card: impl Fn(usize, f32) -> cosmic::Element<'a, M> + 'a,
) -> cosmic::Element<'a, M> {
    // User-chosen card size (Settings / header zoom slider).
    let scale = grid_scale();
    let (min_card, max_card) = (min_card * scale, max_card * scale);
    widget::responsive(move |size| {
        let spacing = cosmic::theme::active().cosmic().spacing;
        let pad = f32::from(spacing.space_m);
        let gap = f32::from(spacing.space_s);
        let avail = (size.width - 2.0 * pad - GRID_SCROLLBAR_CLEARANCE).max(min_card);
        let columns = (((avail + gap) / (min_card + gap)).floor() as usize).max(1);
        let card = ((avail - gap * (columns - 1) as f32) / columns as f32).min(max_card);

        let mut grid = widget::Column::new().spacing(gap);
        for start in (0..count).step_by(columns) {
            let mut row = widget::Row::new().spacing(gap);
            for index in start..(start + columns).min(count) {
                row = row.push(
                    widget::container(make_card(index, card))
                        .width(Length::Fixed(card))
                        .align_x(Horizontal::Center),
                );
            }
            grid = grid.push(row);
        }

        widget::scrollable(
            widget::container(grid)
                .padding(cosmic::iced::Padding {
                    top: pad,
                    right: pad + GRID_SCROLLBAR_CLEARANCE,
                    bottom: pad,
                    left: pad,
                })
                .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    })
    .into()
}

/// Round artist avatar: a real photo (clipped to a circle via
/// `widget::image`'s native `border_radius` support) when `photo` is
/// available, otherwise the deterministic-color initials placeholder —
/// see `initials_avatar`. Built entirely from widgets, so it stays crisp
/// at any `size`/HiDPI scale factor, unlike a rasterized bitmap scaled up
/// to fit. Shared by every place an artist avatar is shown (grid, list,
/// detail view).
pub fn artist_avatar<'a, M: 'static>(
    name: &str,
    photo: Option<&widget::image::Handle>,
    size: f32,
) -> cosmic::Element<'a, M> {
    match photo {
        Some(handle) => widget::image(handle.clone())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .content_fit(ContentFit::Cover)
            .border_radius(size / 2.0)
            .into(),
        None => initials_avatar(name, size),
    }
}

/// Deterministic-color circle with centered initials — no raster image
/// involved, so it never blurs/pixelates at large sizes or on HiDPI
/// displays the way the old rasterized-at-64px avatar did. See
/// `artist_avatar`.
pub fn initials_avatar<'a, M: 'static>(name: &str, size: f32) -> cosmic::Element<'a, M> {
    let initials = crate::library::CoverArt::artist_initials(name);
    let (r, g, b) = crate::library::CoverArt::artist_avatar_color(name);
    let background = Color::from_rgb8(r, g, b);
    let font_size = (size * 0.36).max(11.0);

    widget::container(
        widget::text(initials)
            .size(font_size)
            .class(cosmic::theme::Text::Color(Color::WHITE)),
    )
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .align_x(Horizontal::Center)
    .align_y(Vertical::Center)
    .class(cosmic::theme::Container::custom(move |_theme| {
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(background)),
            border: cosmic::iced::Border {
                radius: (size / 2.0).into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }))
    .into()
}

/// Fixed-height two-line label block under a grid card: a title element
/// above a subtitle element, both pinned to `card_width` so every card's
/// caption block is exactly `label_height` tall regardless of whether the
/// subtitle has text. Callers clip their own title/subtitle content
/// (typically with [`clipped_cell`]) before passing it in, since a
/// subtitle may itself be a row with more than one clipped piece.
pub fn grid_card_label<'a, M: 'a>(
    card_width: f32,
    label_height: f32,
    title: cosmic::Element<'a, M>,
    subtitle: cosmic::Element<'a, M>,
) -> cosmic::Element<'a, M> {
    widget::container(
        widget::Column::new()
            .push(widget::container(title).width(card_width))
            .push(widget::container(subtitle).width(card_width))
            .spacing(2),
    )
    .height(Length::Fixed(label_height))
    .into()
}

/// Assembles a grid card: a fixed `card_width`×`card_width` centered art
/// tile above a label block. Shared by every card grid (albums, artists)
/// so art framing and card spacing never drift between them.
pub fn grid_card<'a, M: 'a>(
    art: cosmic::Element<'a, M>,
    card_width: f32,
    label_block: cosmic::Element<'a, M>,
) -> cosmic::Element<'a, M> {
    widget::Column::new()
        .push(
            widget::container(art)
                .width(card_width)
                .height(card_width)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center),
        )
        .push(label_block)
        .spacing(8)
        .into()
}

/// Add-to-playlist icon button. Adds to the first playlist, honestly
/// labelled via tooltip with that playlist's name. Renders empty space
/// instead of a dead button when there are no playlists yet. Shared by
/// every track row that offers this action (songs, albums).
pub fn add_to_playlist_button<'a, M: 'static + Clone>(
    source_uri: String,
    playlists: &'a [Playlist],
    make_message: impl FnOnce(String, String) -> M,
    empty_width: f32,
) -> cosmic::Element<'a, M> {
    if let Some(playlist) = playlists.first() {
        let button = widget::button::icon(widget::icon::from_name("list-add-symbolic").size(16))
            .on_press(make_message(source_uri, playlist.id.clone()));
        widget::tooltip(
            button,
            widget::text::caption(fl!(
                "songs-add-to-playlist",
                playlist = playlist.name.as_str()
            )),
            TooltipPosition::Top,
        )
        .into()
    } else {
        widget::Space::new().width(empty_width).into()
    }
}

#[cfg(test)]
mod tests {
    use super::{format_duration, format_duration_coarse, truncate_str};

    #[test]
    fn format_duration_sub_minute() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(59), "0:59");
    }

    #[test]
    fn format_duration_minutes_only() {
        assert_eq!(format_duration(60), "1:00");
        assert_eq!(format_duration(3599), "59:59");
    }

    #[test]
    fn format_duration_hour_boundary() {
        assert_eq!(format_duration(3600), "1:00:00");
    }

    #[test]
    fn format_duration_regression_164_minutes() {
        // 9860s = 2h 44m 20s. Previously rendered as the bogus "164:20"
        // because minutes were never rolled over into hours.
        assert_eq!(format_duration(9860), "2:44:20");
    }

    #[test]
    fn format_duration_coarse_seconds_only() {
        assert_eq!(format_duration_coarse(12), "12s");
    }

    #[test]
    fn format_duration_coarse_minutes_only() {
        assert_eq!(format_duration_coarse(300), "5m");
    }

    #[test]
    fn format_duration_coarse_hours_and_minutes() {
        assert_eq!(format_duration_coarse(9860), "2h 44m");
    }

    #[test]
    fn format_duration_coarse_hour_boundary_zero_minutes() {
        assert_eq!(format_duration_coarse(3600), "1h 00m");
    }

    #[test]
    fn truncate_str_shorter_than_max_is_unchanged() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact_max_is_unchanged() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_over_max_ends_with_ellipsis_and_respects_length() {
        let result = truncate_str("hello world", 8);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 8);
    }

    #[test]
    fn truncate_str_trims_trailing_whitespace_before_ellipsis() {
        // Cutting right after "hello" lands on trailing spaces; they must be
        // trimmed so the ellipsis doesn't float after visible whitespace.
        let result = truncate_str("hello   world", 8);
        assert_eq!(result, "hello…");
    }

    #[test]
    fn truncate_str_multibyte_safe_no_panic() {
        // Multibyte (non-ASCII) chars must be counted and sliced by `char`,
        // never by byte, or this would panic on a non-boundary split.
        let cjk = "日本語のテスト";
        let result = truncate_str(cjk, 4);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 4);

        let accented = "ééééééééé";
        let result = truncate_str(accented, 5);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 5);
    }
}
