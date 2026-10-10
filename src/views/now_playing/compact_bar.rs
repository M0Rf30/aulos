// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The compact bottom playback bar.
//!
//! Fixed at 84px tall so it can never balloon vertically — every text label
//! in this bar is single-line (`common::cell_text` / `cell_caption`) and
//! clipped to its cell rather than wrapped or character-truncated; every
//! other element has a fixed or portioned width. Clicking the cover art or
//! track info expands into the full now-playing view.

use super::{NowPlayingMessage, format_time};
use crate::config::RepeatMode;
use crate::fl;
use crate::library::Track;
use crate::library::palette::Accent;
use crate::player::PlaybackState;
use crate::views::common;
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use cosmic::widget::tooltip::Position as TooltipPosition;
use std::time::Duration;

/// Fixed height of the compact bar — never grows regardless of content.
const BAR_HEIGHT: f32 = 84.0;

/// Cover art side length.
const COVER_SIZE: f32 = 56.0;

/// Total height of the inert seek-track placeholder shown with no track
/// loaded — matches the interactive slider's default height so nothing
/// shifts in the layout once playback starts.
const SEEK_TRACK_HEIGHT: f32 = 16.0;

/// Visible thickness of the inert seek-track placeholder's rail.
const SEEK_TRACK_THICKNESS: f32 = 4.0;

/// Fixed width of the elapsed / total time labels, so the seek rail never
/// jitters as the digits change.
const TIME_WIDTH: f32 = 40.0;

/// An icon button with a caption tooltip, disabled (and non-interactive)
/// whenever `enabled` is false — for every transport/utility control whose
/// action requires a loaded track.
fn transport_icon_button<'a, M: Clone + 'static>(
    icon_name: &'static str,
    icon_size: u16,
    enabled: bool,
    label: String,
    on_press: M,
) -> cosmic::Element<'a, M> {
    let button = widget::button::icon(widget::icon::from_name(icon_name).size(icon_size))
        .on_press_maybe(enabled.then_some(on_press));
    widget::tooltip(button, widget::text::caption(label), TooltipPosition::Top).into()
}

/// Like [`transport_icon_button`] but also renders an accent-tinted "active"
/// state (shuffle / repeat).
fn toggle_icon_button<'a, M: Clone + 'static>(
    icon_name: &'static str,
    icon_size: u16,
    active: bool,
    enabled: bool,
    label: String,
    on_press: M,
) -> cosmic::Element<'a, M> {
    let button = widget::button::icon(widget::icon::from_name(icon_name).size(icon_size))
        .selected(active)
        .on_press_maybe(enabled.then_some(on_press));
    widget::tooltip(button, widget::text::caption(label), TooltipPosition::Top).into()
}

/// The bar's one primary action: play/pause rendered as a filled accent
/// button (cover-derived accent when available, else the theme's suggested
/// style) so it reads as the focal point of the transport cluster.
fn play_pause_button<'a>(
    icon_name: &'static str,
    enabled: bool,
    label: String,
    accent: Option<&Accent>,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let class = accent
        .map(common::accent_button_class)
        .unwrap_or(cosmic::theme::Button::Suggested);
    let button = widget::button::icon(widget::icon::from_name(icon_name).size(24))
        .class(class)
        .padding(cosmic::theme::active().cosmic().spacing.space_xs)
        .on_press_maybe(enabled.then_some(NowPlayingMessage::TogglePlayback));
    widget::tooltip(button, widget::text::caption(label), TooltipPosition::Top).into()
}

/// Rounded, card-coloured tile shown in place of the cover when the track
/// has no artwork (`has_track`) or nothing is loaded (muted glyph). Same
/// footprint and corner radius as a real cover so nothing shifts.
fn cover_placeholder<'a, M: 'static>(has_track: bool) -> cosmic::Element<'a, M> {
    let glyph = if has_track {
        "media-optical-symbolic"
    } else {
        "audio-x-generic-symbolic"
    };
    widget::container(widget::icon::from_name(glyph).size(28))
        .width(Length::Fixed(COVER_SIZE))
        .height(Length::Fixed(COVER_SIZE))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let component = &cosmic.background(false).component;
            let tint = if has_track {
                component.on
            } else {
                component.on_disabled
            };
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(component.base.into())),
                icon_color: Some(tint.into()),
                border: cosmic::iced::Border {
                    radius: cosmic.corner_radii.radius_s.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Themed, non-interactive placeholder for the seek slider shown when no
/// track is loaded, so there is nothing to drag.
fn inert_seek_track<'a, M: 'a>() -> cosmic::Element<'a, M> {
    let rail = widget::container(
        widget::Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(SEEK_TRACK_THICKNESS)),
    )
    .class(cosmic::theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(
                cosmic.background(false).component.divider.into(),
            )),
            border: cosmic::iced::Border {
                radius: cosmic.corner_radii.radius_xs.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }));
    widget::container(rail)
        .width(Length::Fill)
        .height(Length::Fixed(SEEK_TRACK_HEIGHT))
        .align_y(Vertical::Center)
        .into()
}

/// Small "LIVE" pill shown in place of the elapsed/remaining time labels
/// for a stream with no known duration (radio) — same row slot as the
/// time labels, just different content, so the seek-bar row's tree shape
/// never changes between a normal track and a live stream.
fn live_badge<'a>() -> cosmic::Element<'a, NowPlayingMessage> {
    let pill = widget::Row::new()
        .push(widget::icon::from_name("media-record-symbolic").size(10))
        .push(widget::text::caption(fl!("now-playing-live")))
        .spacing(4)
        .align_y(Alignment::Center);
    widget::container(pill)
        .padding([2, 8])
        .class(cosmic::theme::Container::custom(|theme| {
            let cosmic = theme.cosmic();
            let accent = cosmic.accent_color();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(accent.with_alpha(0.16).into())),
                text_color: Some(accent.into()),
                icon_color: Some(accent.into()),
                border: cosmic::iced::Border {
                    radius: cosmic.corner_radii.radius_xs.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Render the compact bottom playback bar.
///
/// Layout (left to right):
/// ```text
/// [Cover] [Title/Artist] [♥] | [Shuffle Prev Play Stop Next Repeat] / [Seek bar] | [Vol slider] [Lyrics] [Up Next]
/// ```
#[allow(clippy::too_many_arguments)]
pub fn playback_bar<'a>(
    current_track: Option<&'a Track>,
    state: PlaybackState,
    position: Duration,
    duration: Duration,
    volume: f32,
    shuffle: bool,
    repeat_mode: RepeatMode,
    cover_art: Option<&'a widget::icon::Handle>,
    seeking_preview: Option<f32>,
    _blurred_cover: Option<&'a widget::icon::Handle>,
    accent: Option<&'a Accent>,
    is_queue_open: bool,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let has_track = current_track.is_some();
    // Radio/other zero-duration streams: no seek slider, and shuffle/prev/
    // next/repeat are meaningless (there is nothing to shuffle or skip to)
    // so they render disabled rather than disappearing.
    let is_live = current_track.is_some_and(super::is_live_stream);
    let stop_enabled = has_track && state != PlaybackState::Stopped;

    // While dragging, show the preview position; otherwise the backend position.
    let (progress, display_position) = if let Some(frac) = seeking_preview {
        let preview_pos = Duration::from_secs_f32(frac * duration.as_secs_f32());
        (frac, preview_pos)
    } else {
        let p = if duration.as_secs_f32() > 0.0 {
            (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (p, position)
    };

    // --- Cover art: fixed 56x56, rounded; framed placeholder when missing ---
    let art: cosmic::Element<'_, NowPlayingMessage> = if let Some(handle) = cover_art {
        let radius = cosmic::theme::active().cosmic().corner_radii.radius_s[0];
        common::cover_art(handle, COVER_SIZE, radius, false)
    } else {
        // Track loaded but no artwork, or nothing loaded at all (muted).
        cover_placeholder(has_track)
    };

    // --- Track info: single-line title + single-line "artist — album" ---
    let (info_content, heart): (
        cosmic::Element<'_, NowPlayingMessage>,
        cosmic::Element<'_, NowPlayingMessage>,
    ) = if let Some(track) = current_track {
        let column = widget::Column::new()
            .push(common::clipped_cell(
                common::cell_text(track.title.clone())
                    .font(cosmic::font::semibold())
                    .into(),
            ))
            .push(common::clipped_cell(super::artist_album_line(
                track, true, None, " — ",
            )))
            .spacing(2)
            .width(Length::Fill)
            .into();
        let heart = common::favorite_button(
            track.is_favorite,
            NowPlayingMessage::ToggleFavorite(track.id.to_string()),
        );
        (column, heart)
    } else {
        let column = widget::Column::new()
            .push(common::cell_caption(fl!("no-track-playing")))
            .width(Length::Fill)
            .into();
        (column, widget::Space::new().width(0).height(0).into())
    };

    // Cover art + title/artist share a single click target that expands
    // into the full now-playing view.
    let expandable = widget::tooltip(
        widget::mouse_area(
            widget::Row::new()
                .push(
                    widget::container(art)
                        .width(Length::Fixed(COVER_SIZE))
                        .height(Length::Fixed(COVER_SIZE))
                        .align_x(Horizontal::Center)
                        .align_y(Vertical::Center),
                )
                .push(
                    widget::container(info_content)
                        .width(Length::FillPortion(2))
                        .align_y(Vertical::Center),
                )
                .spacing(12)
                .align_y(Alignment::Center),
        )
        .on_press(NowPlayingMessage::ExpandToggle),
        widget::text::caption(fl!("show-now-playing")),
        TooltipPosition::Top,
    );

    // --- Center: transport controls (row 1) + seek bar (row 2) ---
    let play_icon = if state == PlaybackState::Playing {
        "media-playback-pause-symbolic"
    } else {
        "media-playback-start-symbolic"
    };
    let play_label = if state == PlaybackState::Playing {
        fl!("pause")
    } else {
        fl!("play")
    };

    // A single glyph for both states: `toggle_icon_button` already conveys
    // on/off via accent tint (`.selected(active)`). Swapping to an unrelated
    // "sequential playback" arrow glyph for the off state reads as a stray,
    // unrelated button next to the skip/play icons.
    let shuffle_icon = "media-playlist-shuffle-symbolic";
    let repeat_icon = repeat_mode.icon_name();
    let repeat_active = repeat_mode != RepeatMode::None;

    // Secondary controls (shuffle / stop / repeat) are a size smaller than
    // the skip buttons, and play/pause is the filled focal point.
    let transport = widget::Row::new()
        .push(toggle_icon_button(
            shuffle_icon,
            20,
            shuffle,
            has_track && !is_live,
            fl!("shuffle"),
            NowPlayingMessage::ToggleShuffle,
        ))
        .push(transport_icon_button(
            "media-skip-backward-symbolic",
            24,
            has_track && !is_live,
            fl!("previous"),
            NowPlayingMessage::Previous,
        ))
        .push(play_pause_button(play_icon, has_track, play_label, accent))
        .push(transport_icon_button(
            "media-playback-stop-symbolic",
            20,
            stop_enabled,
            fl!("stop"),
            NowPlayingMessage::Stop,
        ))
        .push(transport_icon_button(
            "media-skip-forward-symbolic",
            24,
            has_track && !is_live,
            fl!("next"),
            NowPlayingMessage::Next,
        ))
        .push(toggle_icon_button(
            repeat_icon,
            20,
            repeat_active,
            has_track && !is_live,
            fl!("repeat"),
            NowPlayingMessage::CycleRepeat,
        ))
        .spacing(4)
        .align_y(Alignment::Center);

    let time_start_slot: cosmic::Element<'_, NowPlayingMessage> = if is_live {
        live_badge()
    } else {
        widget::container(common::cell_caption(format_time(display_position)))
            .width(Length::Fixed(TIME_WIDTH))
            .align_x(Horizontal::Right)
            .into()
    };
    let time_end_slot: cosmic::Element<'_, NowPlayingMessage> = if is_live {
        widget::Space::new().width(0).height(0).into()
    } else {
        widget::container(common::cell_caption(format_time(duration)))
            .width(Length::Fixed(TIME_WIDTH))
            .align_x(Horizontal::Left)
            .into()
    };

    let seek_bar = widget::Row::new()
        .push(time_start_slot)
        .push(if has_track && !is_live {
            super::seek_bar::smooth_seek(
                progress,
                duration,
                state == PlaybackState::Playing && seeking_preview.is_none(),
                accent.map(|a| cosmic::iced::Color::from_rgb(a.color[0], a.color[1], a.color[2])),
                NowPlayingMessage::SeekPreview,
                NowPlayingMessage::SeekCommit,
            )
            .into()
        } else {
            inert_seek_track()
        })
        .push(time_end_slot)
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    let center_column = widget::Column::new()
        .push(transport)
        .push(seek_bar)
        .spacing(4)
        .align_x(Alignment::Center)
        .width(Length::FillPortion(3));

    // --- Volume: icon reflects current level, fixed-width slider (always enabled) ---
    let volume = volume.clamp(0.0, 1.0);
    let volume_icon_name = if volume <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if volume < 0.33 {
        "audio-volume-low-symbolic"
    } else if volume < 0.66 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    };

    let volume_block = widget::Row::new()
        .push(widget::icon::from_name(volume_icon_name).size(20))
        .push(
            widget::slider(0.0..=1.0, volume, NowPlayingMessage::SetVolume)
                .step(0.01_f32)
                .on_release(NowPlayingMessage::VolumeCommit)
                .width(Length::Fixed(110.0)),
        )
        .spacing(8)
        .align_y(Alignment::Center);

    // --- Utility buttons ---
    let utility_buttons = widget::Row::new()
        .push(transport_icon_button(
            "format-justify-left-symbolic",
            20,
            has_track,
            fl!("lyrics"),
            NowPlayingMessage::ShowLyrics,
        ))
        .push(toggle_icon_button(
            "go-last-symbolic",
            20,
            crate::player::party::stop_after_flag(),
            has_track && !is_live,
            fl!("stop-after-track"),
            NowPlayingMessage::ToggleStopAfter,
        ))
        .push(toggle_icon_button(
            "media-playlist-consecutive-symbolic",
            20,
            is_queue_open,
            true,
            fl!("queue"),
            NowPlayingMessage::ToggleQueue,
        ))
        .spacing(4)
        .align_y(Alignment::Center);

    let controls_row = widget::Row::new()
        .push(expandable)
        .push(heart)
        .push(center_column)
        .push(volume_block)
        .push(utility_buttons)
        .spacing(16)
        .padding([8, 16])
        .align_y(Alignment::Center);

    widget::container(controls_row)
        .width(Length::Fill)
        .height(Length::Fixed(BAR_HEIGHT))
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::Card)
        .into()
}
