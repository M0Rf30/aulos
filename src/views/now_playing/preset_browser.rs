// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Preset browser overlay for the ProjectM visualizer (behind the
//! `visualizer` feature flag). Lists every discovered `.milk` preset,
//! grouped by category, with a live search filter, click-to-load, and
//! playback controls (next/lock/beat sensitivity). Stacked as the topmost
//! layer over the visualizer in `expanded_view::expanded_now_playing`.

use super::NowPlayingMessage;
use super::expanded_view::{BACKDROP_SUBTEXT, BACKDROP_TEXT};
use super::visualizer::PresetEntry;
use crate::fl;
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::widget::Stack;
use cosmic::iced::{Alignment, Color, Length};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;
use cosmic::widget::tooltip::Position as TooltipPosition;

/// Height of the scrollable preset list — the "fixed height + scrollable"
/// half of the panel's approximate 70%-of-viewport cap (the other half is
/// the header/divider/controls, which size to content).
const LIST_HEIGHT: f32 = 380.0;
const PANEL_MAX_WIDTH: f32 = 720.0;

/// List-row style for preset rows. Unlike `list_row_button_class` (tuned
/// for rows on the app's normal themed surface), this panel always sits on
/// a fixed dark backdrop regardless of the active COSMIC theme, so
/// unselected rows use the fixed `BACKDROP_TEXT`/`BACKDROP_SUBTEXT` pair
/// instead of theme on-background colors, which could render near-black
/// (and unreadable) in a light theme.
fn preset_row_class(selected: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.2).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: None,
                    text_color: Some(BACKDROP_TEXT),
                    icon_color: Some(BACKDROP_TEXT),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        hovered: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.28).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.08))),
                    text_color: Some(BACKDROP_TEXT),
                    icon_color: Some(BACKDROP_TEXT),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        pressed: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            ButtonStyle {
                background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.14))),
                text_color: Some(BACKDROP_TEXT),
                icon_color: Some(BACKDROP_TEXT),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        disabled: Box::new(|theme| ButtonStyle {
            background: None,
            text_color: Some(BACKDROP_SUBTEXT),
            icon_color: Some(BACKDROP_SUBTEXT),
            border_radius: theme.cosmic().corner_radii.radius_s.into(),
            ..ButtonStyle::new()
        }),
    }
}

/// Bare icon button with a caption tooltip, styled for this panel's fixed
/// dark backdrop. Takes an owned `String` label (rather than
/// `common::icon_button`'s borrowed `&'a str`) so it can be built from
/// `fl!()` — see `expanded_view::transport_button`'s doc comment for why
/// the borrowed signature can't take an `fl!()` value here.
fn panel_icon_button<'a>(
    icon_name: &'static str,
    icon_size: u16,
    label: String,
    on_press: NowPlayingMessage,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let button =
        widget::button::icon(widget::icon::from_name(icon_name).size(icon_size)).on_press(on_press);
    widget::tooltip(button, widget::text::caption(label), TooltipPosition::Top).into()
}

/// Build the full preset browser overlay: a dimming backdrop (click to
/// close) beneath a centered panel with a search header, a scrollable
/// category-grouped preset list, and a next/lock/beat-sensitivity controls
/// row.
pub fn preset_browser_overlay<'a>(
    entries: &'a [PresetEntry],
    search: &'a str,
    current_preset_name: Option<&'a str>,
    locked: bool,
    beat_sensitivity: f32,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let space_xs = f32::from(spacing.space_xs);
    let space_s = f32::from(spacing.space_s);
    let space_m = f32::from(spacing.space_m);

    let header = widget::Row::new()
        .push(
            widget::text::title4(fl!("viz-presets"))
                .class(cosmic::theme::Text::Color(BACKDROP_TEXT)),
        )
        .push(
            widget::text_input(fl!("viz-preset-search"), search)
                .on_input(NowPlayingMessage::PresetSearchInput)
                .width(Length::Fill),
        )
        .push(panel_icon_button(
            "window-close-symbolic",
            20,
            fl!("viz-close-presets"),
            NowPlayingMessage::TogglePresetBrowser,
        ))
        .spacing(space_s)
        .align_y(Alignment::Center);

    let query = search.trim().to_lowercase();
    let mut list = widget::Column::new().spacing(2);
    let mut last_category: Option<&str> = None;
    let mut any_match = false;
    for entry in entries {
        if !query.is_empty()
            && !entry.name.to_lowercase().contains(&query)
            && !entry.category.to_lowercase().contains(&query)
        {
            continue;
        }
        any_match = true;

        if last_category != Some(entry.category.as_str()) {
            list = list.push(
                widget::container(
                    widget::text::caption(entry.category.as_str())
                        .class(cosmic::theme::Text::Color(BACKDROP_SUBTEXT)),
                )
                .padding([space_xs, 0.0, 0.0, 0.0]),
            );
            last_category = Some(entry.category.as_str());
        }

        let is_active = current_preset_name == Some(entry.name.as_str());
        let row = widget::button::custom(
            widget::text::body(entry.name.as_str()).wrapping(Wrapping::None),
        )
        .on_press(NowPlayingMessage::LoadVizPreset(entry.path.clone()))
        .padding([4.0, space_s])
        .width(Length::Fill)
        .class(preset_row_class(is_active));
        list = list.push(row);
    }
    if !any_match {
        list = list.push(
            widget::text::body(fl!("viz-preset-empty"))
                .class(cosmic::theme::Text::Color(BACKDROP_SUBTEXT)),
        );
    }

    let scroll = widget::scrollable(widget::container(list).width(Length::Fill))
        .height(Length::Fixed(LIST_HEIGHT));

    let controls = widget::Row::new()
        .push(panel_icon_button(
            "media-skip-forward-symbolic",
            20,
            fl!("viz-next-preset"),
            NowPlayingMessage::NextPreset,
        ))
        .push(widget::toggler(locked).on_toggle(NowPlayingMessage::SetVizLocked))
        .push(widget::text::body(fl!("viz-lock")).class(cosmic::theme::Text::Color(BACKDROP_TEXT)))
        .push(
            widget::text::body(fl!("viz-beat-sensitivity"))
                .class(cosmic::theme::Text::Color(BACKDROP_TEXT)),
        )
        .push(
            widget::slider(
                0.0..=2.0,
                beat_sensitivity,
                NowPlayingMessage::SetVizBeatSensitivity,
            )
            .step(0.05_f32)
            .width(Length::Fixed(160.0)),
        )
        .spacing(space_s)
        .align_y(Alignment::Center);

    let panel_col = widget::Column::new()
        .push(header)
        .push(widget::divider::horizontal::default())
        .push(scroll)
        .push(widget::divider::horizontal::default())
        .push(controls)
        .spacing(space_xs)
        .width(Length::Fill);

    let panel = widget::container(panel_col)
        .padding(space_m)
        .width(Length::Fill)
        .max_width(PANEL_MAX_WIDTH)
        .class(cosmic::theme::Container::custom(|_theme| {
            cosmic::iced::widget::container::Style {
                background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.55).into()),
                text_color: Some(BACKDROP_TEXT),
                border: cosmic::iced::Border {
                    radius: [16.0; 4].into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }));

    // Swallows presses anywhere on the panel (including its own blank
    // background — `MouseArea::update` calls `shell.capture_event()`
    // unconditionally on a left press within its bounds) so they never
    // fall through to the backdrop's close-on-click-outside handler below.
    // Interactive children (buttons, the search input, the scrollable)
    // still get their own clicks first, unaffected.
    let panel_swallow = widget::mouse_area(panel);

    let centered_panel = widget::container(panel_swallow)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .padding(space_m);

    let backdrop = widget::mouse_area(
        widget::container(widget::Space::new().width(0).height(0))
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(|_theme| {
                cosmic::iced::widget::container::Style {
                    background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.45).into()),
                    ..Default::default()
                }
            })),
    )
    .on_press(NowPlayingMessage::TogglePresetBrowser);

    Stack::new().push(backdrop).push(centered_panel).into()
}
