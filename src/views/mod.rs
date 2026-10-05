// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

pub mod albums;
pub mod artists;
pub mod common;
pub mod convert;
pub mod equalizer;
pub mod folders;
pub mod genres;
pub mod lyrics;
pub mod now_playing;
pub mod playlists;
pub mod podcasts;
pub mod providers;
pub mod queue;
pub mod radio;
pub mod settings;
pub mod smart_playlists;
pub mod songs;

use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::core::Background;
use cosmic::widget::button::Style as ButtonStyle;

pub fn card_button_class() -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| {
            let cosmic = theme.cosmic();
            ButtonStyle {
                background: None,
                text_color: Some(cosmic.background(false).component.on.into()),
                icon_color: Some(cosmic.background(false).component.on.into()),
                border_radius: cosmic.corner_radii.radius_m.into(),
                ..ButtonStyle::new()
            }
        }),
        hovered: Box::new(|_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.background(false).component;
            ButtonStyle {
                background: Some(Background::Color(comp.hover.into())),
                text_color: Some(comp.on.into()),
                icon_color: Some(comp.on.into()),
                border_radius: cosmic.corner_radii.radius_m.into(),
                ..ButtonStyle::new()
            }
        }),
        pressed: Box::new(|_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.background(false).component;
            ButtonStyle {
                background: Some(Background::Color(comp.pressed.into())),
                text_color: Some(comp.on.into()),
                icon_color: Some(comp.on.into()),
                border_radius: cosmic.corner_radii.radius_m.into(),
                ..ButtonStyle::new()
            }
        }),
        disabled: Box::new(|theme| {
            let cosmic = theme.cosmic();
            ButtonStyle {
                background: None,
                text_color: Some(cosmic.background(false).component.on_disabled.into()),
                icon_color: Some(cosmic.background(false).component.on_disabled.into()),
                border_radius: cosmic.corner_radii.radius_m.into(),
                ..ButtonStyle::new()
            }
        }),
    }
}

/// Button class for interactive list rows across library views.
///
/// Replaces `cosmic::theme::Button::Text` on list rows: unselected rows keep
/// the default on-surface text/icon color (not accent blue) so ordinary rows
/// read as plain text, while the row that is actually playing gets an
/// accent-tinted label on a subtle accent-alpha background — unmistakable in
/// both light and dark themes.
pub fn list_row_button_class(selected: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.12).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: None,
                    text_color: Some(cosmic.background(false).component.on.into()),
                    icon_color: Some(cosmic.background(false).component.on.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        hovered: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.background(false).component;
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.18).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: Some(Background::Color(comp.hover.into())),
                    text_color: Some(comp.on.into()),
                    icon_color: Some(comp.on.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        pressed: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.background(false).component;
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.24).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: Some(Background::Color(comp.pressed.into())),
                    text_color: Some(comp.on.into()),
                    icon_color: Some(comp.on.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        disabled: Box::new(|theme| {
            let cosmic = theme.cosmic();
            ButtonStyle {
                background: None,
                text_color: Some(cosmic.background(false).component.on_disabled.into()),
                icon_color: Some(cosmic.background(false).component.on_disabled.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
    }
}
