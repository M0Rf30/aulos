// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Mini player: a compact stand-alone player layout (cover, title/artist,
//! transport, seek bar) shown in place of the whole library UI while the
//! window is shrunk to mini-player mode. The headerbar is hidden in this
//! mode, so the view carries its own thin drag strip with restore/close.

use crate::fl;
use crate::views::common;
use crate::views::now_playing::seek_bar;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::{Alignment, Color, Length};
use cosmic::widget;
use std::time::Duration;

/// Below this window height the cover moves beside the controls.
const COMPACT_BELOW: f32 = 300.0;
/// Height of the drag strip across the top of the mini player.
const DRAG_STRIP: f32 = 32.0;

#[derive(Debug, Clone)]
pub enum MiniMsg {
    /// Start moving the window (pressed on the top strip).
    Drag,
    /// Leave mini-player mode.
    Restore,
    /// Close the application.
    Close,
    TogglePlayback,
    Previous,
    Next,
    SeekPreview(f32),
    SeekCommit,
    ToggleFavorite(String),
}

/// Everything the mini player renders, owned so the responsive layout
/// closure can rebuild the widgets on every size change.
#[derive(Clone)]
pub struct MiniProps {
    pub title: String,
    pub subtitle: String,
    pub track_id: Option<String>,
    pub is_favorite: bool,
    pub has_track: bool,
    pub is_live: bool,
    pub cover: Option<widget::icon::Handle>,
    pub playing: bool,
    /// Played fraction (or the drag preview while seeking).
    pub progress: f32,
    pub position: Duration,
    pub duration: Duration,
    pub seeking: bool,
    pub accent: Option<Color>,
}

pub fn view(props: MiniProps) -> cosmic::Element<'static, MiniMsg> {
    widget::responsive(move |size| layout(&props, size.width, size.height)).into()
}

fn layout(p: &MiniProps, width: f32, height: f32) -> cosmic::Element<'static, MiniMsg> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let pad = f32::from(sp.space_m);

    let body: cosmic::Element<'static, MiniMsg> = if height < COMPACT_BELOW {
        compact_body(p)
    } else {
        // Leave room for the strip, title block, seek row, and transport.
        let side = (width - 2.0 * pad).min(height - 200.0).clamp(64.0, 480.0);
        tall_body(p, side)
    };

    widget::Column::new()
        .push(top_strip())
        .push(
            widget::container(body)
                .padding([0, sp.space_m, sp.space_m, sp.space_m])
                .width(Length::Fill)
                .height(Length::Fill)
                .align_y(Vertical::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn top_strip() -> cosmic::Element<'static, MiniMsg> {
    let small = |icon: &'static str, label: String, msg: MiniMsg| {
        widget::tooltip(
            widget::button::icon(widget::icon::from_name(icon).size(16)).on_press(msg),
            widget::text::caption(label),
            widget::tooltip::Position::Bottom,
        )
    };
    widget::Row::new()
        .push(
            widget::mouse_area(
                widget::Space::new()
                    .width(Length::Fill)
                    .height(Length::Fixed(DRAG_STRIP)),
            )
            .on_press(MiniMsg::Drag),
        )
        .push(small(
            "window-restore-symbolic",
            fl!("mini-player-exit"),
            MiniMsg::Restore,
        ))
        .push(small("window-close-symbolic", fl!("quit"), MiniMsg::Close))
        .align_y(Alignment::Center)
        .padding([0, 4])
        .width(Length::Fill)
        .into()
}

fn tall_body(p: &MiniProps, side: f32) -> cosmic::Element<'static, MiniMsg> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let radius = cosmic::theme::active().cosmic().corner_radii.radius_m[0];
    let art: cosmic::Element<'static, MiniMsg> = match &p.cover {
        Some(handle) => common::cover_art(handle, side, radius, true),
        None => common::grid_art_tile(None, side as u16, "media-optical-symbolic"),
    };

    widget::Column::new()
        .push(
            widget::container(art)
                .width(Length::Fill)
                .align_x(Horizontal::Center),
        )
        .push(info_row(p, true))
        .push(seek_row(p))
        .push(
            widget::container(transport(p))
                .width(Length::Fill)
                .align_x(Horizontal::Center),
        )
        .spacing(sp.space_s)
        .align_x(Alignment::Center)
        .width(Length::Fill)
        .into()
}

fn compact_body(p: &MiniProps) -> cosmic::Element<'static, MiniMsg> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let radius = cosmic::theme::active().cosmic().corner_radii.radius_s[0];
    let art: cosmic::Element<'static, MiniMsg> = match &p.cover {
        Some(handle) => common::cover_art(handle, 72.0, radius, false),
        None => common::grid_art_tile(None, 72, "media-optical-symbolic"),
    };

    widget::Column::new()
        .push(
            widget::Row::new()
                .push(art)
                .push(
                    widget::Column::new()
                        .push(info_row(p, false))
                        .push(transport(p))
                        .spacing(sp.space_xxs)
                        .width(Length::Fill),
                )
                .spacing(sp.space_s)
                .align_y(Alignment::Center),
        )
        .push(seek_row(p))
        .spacing(sp.space_xxs)
        .width(Length::Fill)
        .into()
}

fn info_row(p: &MiniProps, centered: bool) -> cosmic::Element<'static, MiniMsg> {
    let align = if centered {
        Horizontal::Center
    } else {
        Horizontal::Left
    };
    let (title, subtitle) = if p.has_track {
        (p.title.clone(), p.subtitle.clone())
    } else {
        (fl!("no-track-playing"), String::new())
    };

    let text = widget::Column::new()
        .push(common::clipped_cell(
            common::cell_text(title)
                .font(cosmic::font::semibold())
                .width(Length::Fill)
                .align_x(align)
                .into(),
        ))
        .push(common::clipped_cell(
            common::cell_caption(subtitle)
                .width(Length::Fill)
                .align_x(align)
                .into(),
        ))
        .spacing(2)
        .width(Length::Fill);

    let mut row = widget::Row::new()
        .push(text)
        .align_y(Alignment::Center)
        .width(Length::Fill);
    if let Some(id) = &p.track_id {
        row = row.push(common::favorite_button(
            p.is_favorite,
            MiniMsg::ToggleFavorite(id.clone()),
        ));
    }
    row.into()
}

fn seek_row(p: &MiniProps) -> cosmic::Element<'static, MiniMsg> {
    const TIME_WIDTH: f32 = 44.0;
    let shown = if p.seeking {
        Duration::from_secs_f32(p.progress * p.duration.as_secs_f32())
    } else {
        p.position
    };
    let slider: cosmic::Element<'static, MiniMsg> = if p.has_track && !p.is_live {
        seek_bar::smooth_seek(
            p.progress,
            p.duration,
            p.playing && !p.seeking,
            p.accent,
            MiniMsg::SeekPreview,
            MiniMsg::SeekCommit,
        )
        .into()
    } else {
        widget::Space::new().width(Length::Fill).height(8).into()
    };
    let time = |d: Duration, align| {
        widget::container(common::cell_caption(if p.has_track && !p.is_live {
            common::format_duration(d.as_secs())
        } else {
            String::new()
        }))
        .width(Length::Fixed(TIME_WIDTH))
        .align_x(align)
    };
    widget::Row::new()
        .push(time(shown, Horizontal::Right))
        .push(slider)
        .push(time(p.duration, Horizontal::Left))
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .into()
}

fn transport(p: &MiniProps) -> cosmic::Element<'static, MiniMsg> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let enabled = p.has_track && !p.is_live;
    let skip = |icon: &'static str, label: String, msg: MiniMsg| {
        widget::tooltip(
            widget::button::icon(widget::icon::from_name(icon).size(24))
                .on_press_maybe(enabled.then_some(msg)),
            widget::text::caption(label),
            widget::tooltip::Position::Top,
        )
    };
    let (play_icon, play_label) = if p.playing {
        ("media-playback-pause-symbolic", fl!("pause"))
    } else {
        ("media-playback-start-symbolic", fl!("play"))
    };
    let play = widget::tooltip(
        widget::button::icon(widget::icon::from_name(play_icon).size(24))
            .class(cosmic::theme::Button::Suggested)
            .padding(sp.space_xs)
            .on_press_maybe(p.has_track.then_some(MiniMsg::TogglePlayback)),
        widget::text::caption(play_label),
        widget::tooltip::Position::Top,
    );
    widget::Row::new()
        .push(skip(
            "media-skip-backward-symbolic",
            fl!("previous"),
            MiniMsg::Previous,
        ))
        .push(play)
        .push(skip(
            "media-skip-forward-symbolic",
            fl!("next"),
            MiniMsg::Next,
        ))
        .spacing(sp.space_s)
        .align_y(Alignment::Center)
        .into()
}
