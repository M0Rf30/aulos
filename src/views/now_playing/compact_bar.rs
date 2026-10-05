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
use cosmic::iced::{Alignment, ContentFit, Length};
use cosmic::widget;
use cosmic::widget::tooltip::Position as TooltipPosition;
use std::rc::Rc;
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

/// Slider class that tints only the active (played) portion of the rail
/// and the drag handle with the cover-art accent colour, starting from
/// `Slider::Standard`'s own computed style via `Catalog::style` so border,
/// thickness and handle shape stay in sync with the theme — mirrors
/// `expanded_view::accent_slider_class` but kept file-local since the two
/// views don't otherwise share private helpers.
fn accent_slider_class(accent: &Accent) -> cosmic::theme::iced::Slider {
    let color = cosmic::iced::Color::from_rgb(accent.color[0], accent.color[1], accent.color[2]);
    let style = move |theme: &cosmic::Theme, status: widget::slider::Status| {
        let mut base =
            widget::slider::Catalog::style(theme, &cosmic::theme::iced::Slider::default(), status);
        base.rail.backgrounds.0 = Background::Color(color);
        base.handle.background = Background::Color(color);
        base
    };
    cosmic::theme::iced::Slider::Custom {
        active: Rc::new(move |t| style(t, widget::slider::Status::Active)),
        hovered: Rc::new(move |t| style(t, widget::slider::Status::Hovered)),
        dragging: Rc::new(move |t| style(t, widget::slider::Status::Dragged)),
    }
}

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

    // --- Cover art: fixed 56x56, cropped to fill; placeholder when missing ---
    let art: cosmic::Element<'_, NowPlayingMessage> = if let Some(handle) = cover_art {
        widget::icon::icon(handle.clone())
            .content_fit(ContentFit::Cover)
            .width(Length::Fixed(COVER_SIZE))
            .height(Length::Fixed(COVER_SIZE))
            .into()
    } else if has_track {
        // Track loaded but no artwork available for it.
        widget::icon::from_name("media-optical-cd-audio-symbolic")
            .size(40)
            .into()
    } else {
        // No track loaded — muted placeholder, same footprint as a loaded cover.
        widget::icon::icon(widget::icon::from_name("audio-x-generic-symbolic").handle())
            .size(56)
            .class(cosmic::theme::Svg::custom(|theme| {
                cosmic::iced::widget::svg::Style {
                    color: Some(
                        theme
                            .cosmic()
                            .background(false)
                            .component
                            .on_disabled
                            .into(),
                    ),
                }
            }))
            .into()
    };

    // --- Track info: single-line title + single-line "artist — album" ---
    let (info_content, heart): (
        cosmic::Element<'_, NowPlayingMessage>,
        cosmic::Element<'_, NowPlayingMessage>,
    ) = if let Some(track) = current_track {
        let subtitle = if track.album.is_empty() {
            track.artist.clone()
        } else {
            format!("{} — {}", track.artist, track.album)
        };
        let column = widget::Column::new()
            .push(common::clipped_cell(
                common::cell_text(track.title.clone()).into(),
            ))
            .push(common::clipped_cell(common::cell_caption(subtitle).into()))
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

    let transport = widget::Row::new()
        .push(toggle_icon_button(
            shuffle_icon,
            24,
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
        .push(transport_icon_button(
            play_icon,
            32,
            has_track,
            play_label,
            NowPlayingMessage::TogglePlayback,
        ))
        .push(transport_icon_button(
            "media-playback-stop-symbolic",
            24,
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
            24,
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
        common::cell_caption(format_time(display_position)).into()
    };
    let time_end_slot: cosmic::Element<'_, NowPlayingMessage> = if is_live {
        widget::Space::new().width(0).height(0).into()
    } else {
        common::cell_caption(format_time(duration)).into()
    };

    let seek_bar = widget::Row::new()
        .push(time_start_slot)
        .push(if has_track && !is_live {
            let mut seek_slider =
                widget::slider(0.0..=1.0, progress, NowPlayingMessage::SeekPreview)
                    .step(0.001_f32)
                    .on_release(NowPlayingMessage::SeekCommit)
                    .width(Length::Fill);
            if let Some(accent) = accent {
                seek_slider = seek_slider.class(accent_slider_class(accent));
            }
            seek_slider.into()
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
                .width(Length::Fixed(120.0)),
        )
        .spacing(8)
        .align_y(Alignment::Center);

    // --- Utility buttons ---
    let utility_buttons = widget::Row::new()
        .push(transport_icon_button(
            "view-list-lyrics-symbolic",
            20,
            has_track,
            fl!("lyrics"),
            NowPlayingMessage::ShowLyrics,
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
        .spacing(12)
        .padding([8, 16])
        .align_y(Alignment::Center);

    widget::container(controls_row)
        .width(Length::Fill)
        .height(Length::Fixed(BAR_HEIGHT))
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::Card)
        .into()
}
