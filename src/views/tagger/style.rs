// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Colours, surfaces and small widgets shared by the tag editor's panes.
//!
//! Everything resolves against the active theme, so it follows light/dark
//! and the user's accent. The helpers mirror the Convert page's (tinted
//! chips and banners, muted captions) so both pages read as one app.

use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::core::Background;
use cosmic::iced::{Alignment, Border, Color, Length};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;

use aulos_core::tagger::labels::Tone;

use crate::views::common;

pub fn tone_color(theme: &cosmic::Theme, tone: Tone) -> Color {
    let cosmic = theme.cosmic();
    match tone {
        Tone::Neutral => cosmic.palette.neutral_7.into(),
        Tone::Accent => cosmic.accent_color().into(),
        Tone::Success => cosmic.success_color().into(),
        Tone::Warning => cosmic.warning_color().into(),
        Tone::Danger => cosmic.destructive_color().into(),
    }
}

/// Soft tinted background with tone-coloured text and icons.
pub fn tint_class(tone: Tone, pill: bool) -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::custom(move |theme| {
        let color = tone_color(theme, tone);
        let radii = theme.cosmic().corner_radii;
        let radius = if pill {
            radii.radius_xl
        } else {
            radii.radius_m
        };
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(Color { a: 0.14, ..color })),
            text_color: Some(color),
            icon_color: Some(color),
            border: Border {
                radius: radius.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
}

/// Tinted banner surface with a thin tone-coloured outline; only icons take
/// the tone colour so multi-line text stays readable.
pub fn banner_class(tone: Tone) -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::custom(move |theme| {
        let color = tone_color(theme, tone);
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(Color { a: 0.10, ..color })),
            icon_color: Some(color),
            border: Border {
                radius: theme.cosmic().corner_radii.radius_m.into(),
                width: 1.0,
                color: Color { a: 0.45, ..color },
            },
            ..Default::default()
        }
    })
}

/// Background of a file row: accent tint while selected, nothing otherwise.
pub fn row_class(selected: bool) -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::custom(move |theme| {
        let cosmic = theme.cosmic();
        cosmic::iced::widget::container::Style {
            background: selected
                .then(|| Background::Color(cosmic.accent_color().with_alpha(0.12).into())),
            border: Border {
                radius: cosmic.corner_radii.radius_s.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
}

/// Background of a metadata row whose value was changed.
pub fn changed_row_class(changed: bool) -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::custom(move |theme| {
        let cosmic = theme.cosmic();
        cosmic::iced::widget::container::Style {
            background: changed
                .then(|| Background::Color(cosmic.accent_color().with_alpha(0.10).into())),
            border: Border {
                radius: cosmic.corner_radii.radius_s.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
}

/// Background of a thin separator line.
pub fn divider_class() -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::custom(|theme| cosmic::iced::widget::container::Style {
        background: Some(Background::Color(
            theme.cosmic().palette.neutral_7.with_alpha(0.25).into(),
        )),
        ..Default::default()
    })
}

/// A one-pixel horizontal divider.
pub fn divider<'a, M: 'a>() -> cosmic::Element<'a, M> {
    widget::container(widget::Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .class(divider_class())
        .into()
}

/// A softly tinted tile, for items inside a card (picture cards).
pub fn tile_class() -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(
                cosmic.palette.neutral_7.with_alpha(0.10).into(),
            )),
            border: Border {
                radius: cosmic.corner_radii.radius_m.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
}

/// Transparent button surface that only tints on hover/press, for rows that
/// carry their own selection background.
pub fn quiet_button_class() -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| ButtonStyle {
            background: None,
            text_color: Some(theme.cosmic().background(false).component.on.into()),
            icon_color: Some(theme.cosmic().background(false).component.on.into()),
            border_radius: theme.cosmic().corner_radii.radius_s.into(),
            ..ButtonStyle::new()
        }),
        hovered: Box::new(|_focused, theme| {
            let comp = &theme.cosmic().background(false).component;
            ButtonStyle {
                background: Some(Background::Color(comp.hover.with_alpha(0.6).into())),
                text_color: Some(comp.on.into()),
                icon_color: Some(comp.on.into()),
                border_radius: theme.cosmic().corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        pressed: Box::new(|_focused, theme| {
            let comp = &theme.cosmic().background(false).component;
            ButtonStyle {
                background: Some(Background::Color(comp.pressed.into())),
                text_color: Some(comp.on.into()),
                icon_color: Some(comp.on.into()),
                border_radius: theme.cosmic().corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        disabled: Box::new(|theme| ButtonStyle {
            background: None,
            text_color: Some(
                theme
                    .cosmic()
                    .background(false)
                    .component
                    .on_disabled
                    .into(),
            ),
            icon_color: Some(
                theme
                    .cosmic()
                    .background(false)
                    .component
                    .on_disabled
                    .into(),
            ),
            border_radius: theme.cosmic().corner_radii.radius_s.into(),
            ..ButtonStyle::new()
        }),
    }
}

/// A small indeterminate circular spinner.
pub fn spinner<'a, M: Clone + 'static>(size: f32) -> cosmic::Element<'a, M> {
    widget::container(widget::progress_bar::indeterminate_circular().size(size))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}

fn muted_style(theme: &cosmic::Theme) -> cosmic::iced::widget::text::Style {
    cosmic::iced::widget::text::Style {
        color: Some(theme.cosmic().palette.neutral_7.into()),
        ..Default::default()
    }
}

/// Single-line caption dimmed to the theme's secondary colour.
pub fn muted<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Custom(muted_style))
}

/// Wrapping caption dimmed to the theme's secondary colour, for hints.
pub fn hint<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    widget::text::caption(content)
        .wrapping(cosmic::iced::widget::text::Wrapping::Word)
        .class(cosmic::theme::Text::Custom(muted_style))
}

/// Single-line caption in a tone's colour.
pub fn tone_caption<'a>(
    content: impl Into<std::borrow::Cow<'a, str>> + 'a,
    tone: Tone,
) -> common::Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Color(tone_color(
        &cosmic::theme::active(),
        tone,
    )))
}

/// Wrapping caption in a tone's colour, for messages that may be long.
pub fn tone_hint<'a>(
    content: impl Into<std::borrow::Cow<'a, str>> + 'a,
    tone: Tone,
) -> common::Text<'a> {
    widget::text::caption(content)
        .wrapping(cosmic::iced::widget::text::Wrapping::Word)
        .class(cosmic::theme::Text::Color(tone_color(
            &cosmic::theme::active(),
            tone,
        )))
}

/// A symbolic icon tinted with a tone's colour.
pub fn tone_icon<'a, M: 'a>(name: &'static str, size: u16, tone: Tone) -> cosmic::Element<'a, M> {
    widget::icon::icon(widget::icon::from_name(name).handle())
        .size(size)
        .class(cosmic::theme::Svg::custom(move |theme| {
            cosmic::iced::widget::svg::Style {
                color: Some(tone_color(theme, tone)),
            }
        }))
        .into()
}

/// Small tinted pill with an optional leading icon.
pub fn chip<'a, M: Clone + 'static>(
    icon_name: Option<&'static str>,
    label: String,
    tone: Tone,
) -> cosmic::Element<'a, M> {
    let mut row = widget::Row::new().spacing(4).align_y(Alignment::Center);
    if let Some(name) = icon_name {
        row = row.push(widget::icon::from_name(name).size(12));
    }
    row = row.push(common::cell_caption(label));
    widget::container(row)
        .padding([2, 8])
        .class(tint_class(tone, true))
        .into()
}

/// Icon button wrapped in a caption tooltip. `None` renders it disabled.
pub fn icon_button<'a, M: Clone + 'static>(
    icon_name: &'static str,
    label: String,
    on_press: Option<M>,
) -> cosmic::Element<'a, M> {
    widget::tooltip(
        widget::button::icon(widget::icon::from_name(icon_name).size(16))
            .extra_small()
            .on_press_maybe(on_press),
        widget::text::caption(label),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// Width reserved for a trailing icon button so rows with and without it
/// stay aligned.
pub const ICON_SLOT: f32 = 28.0;

/// An empty slot as wide as an icon button.
pub fn icon_slot<'a, M: 'a>() -> cosmic::Element<'a, M> {
    widget::Space::new()
        .width(Length::Fixed(ICON_SLOT))
        .height(Length::Fixed(ICON_SLOT))
        .into()
}

/// A titled card: heading on the left, optional controls on the right, then
/// the body.
pub fn section<'a, M: Clone + 'static>(
    title: String,
    trailing: Option<cosmic::Element<'a, M>>,
    body: cosmic::Element<'a, M>,
) -> cosmic::Element<'a, M> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut head = widget::Row::new()
        .push(widget::text::heading(title))
        .push(widget::Space::new().width(Length::Fill))
        .spacing(spacing.space_s)
        .align_y(Alignment::Center);
    if let Some(trailing) = trailing {
        head = head.push(trailing);
    }
    widget::container(
        widget::Column::new()
            .push(head)
            .push(body)
            .spacing(spacing.space_s)
            .padding(spacing.space_s),
    )
    .width(Length::Fill)
    .class(cosmic::theme::Container::Card)
    .into()
}

/// Full-width banner: tone icon, bold title, body lines and trailing
/// actions.
pub fn banner<'a, M: Clone + 'static>(
    icon_name: &'static str,
    title: String,
    body: Vec<cosmic::Element<'a, M>>,
    actions: Vec<cosmic::Element<'a, M>>,
    tone: Tone,
) -> cosmic::Element<'a, M> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut text = widget::Column::new()
        .push(widget::text::body(title).font(cosmic::font::semibold()))
        .spacing(2);
    for line in body {
        text = text.push(line);
    }
    let mut row = widget::Row::new()
        .push(widget::icon::from_name(icon_name).size(20))
        .push(text.width(Length::Fill))
        .spacing(spacing.space_s)
        .align_y(Alignment::Center);
    for action in actions {
        row = row.push(action);
    }
    widget::container(row)
        .padding([spacing.space_xs, spacing.space_s])
        .width(Length::Fill)
        .class(banner_class(tone))
        .into()
}
