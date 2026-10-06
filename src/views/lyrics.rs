// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Lyrics display view (context drawer panel).
//!
//! Tasks 104-105: Synced lyrics rendering with highlighted current line.

use crate::fl;
use crate::library::palette::Accent;
use crate::library::{LyricLine, Lyrics};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Color;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use std::time::Duration;

/// Messages from the lyrics view.
#[derive(Debug, Clone)]
pub enum LyricsMessage {
    FetchLyrics,
    Close,
}

/// Find the index of the current lyric line based on playback position.
///
/// Returns the index of the last line whose timestamp is <= current position,
/// or `None` if no line has started yet.
fn find_current_line_index(lines: &[LyricLine], position: Duration) -> Option<usize> {
    let pos_ms = position.as_millis() as u64;
    // Find the last line whose timestamp is <= pos_ms.
    let mut current = None;
    for (i, line) in lines.iter().enumerate() {
        if line.timestamp_ms <= pos_ms {
            current = Some(i);
        } else {
            break;
        }
    }
    current
}

/// Render the lyrics panel (shown in the context drawer).
///
/// Task 104-105: `playback_position` drives synced lyrics highlighting.
/// `accent` (cover-art accent, when one was extracted) tints the current
/// line instead of the theme accent; `None` falls back to
/// `cosmic().accent_color()`.
pub fn lyrics_view<'a>(
    lyrics: Option<&'a Lyrics>,
    track_title: &'a str,
    track_artist: &'a str,
    is_loading: bool,
    playback_position: Duration,
    accent: Option<&Accent>,
) -> cosmic::Element<'a, LyricsMessage> {
    let current_line_color = accent
        .map(|a| Color::from_rgb(a.color[0], a.color[1], a.color[2]))
        .unwrap_or_else(|| cosmic::theme::active().cosmic().accent_color().into());

    let header = widget::Column::new()
        .push(widget::text::title4(track_title))
        .push(widget::text::caption(track_artist))
        .spacing(4);

    let content: cosmic::Element<'_, LyricsMessage> = if is_loading {
        widget::container(widget::text(fl!("lyrics-loading")))
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .into()
    } else if let Some(lyrics_data) = lyrics {
        match lyrics_data {
            Lyrics::Synced(lines) => {
                let current_idx = find_current_line_index(lines, playback_position);
                let mut col = widget::Column::new().spacing(4);
                for (i, line) in lines.iter().enumerate() {
                    let is_current = current_idx == Some(i);
                    col = col.push(synced_line_widget(line, is_current, current_line_color));
                }
                widget::scrollable(widget::container(col).padding(8))
                    .height(Length::Fill)
                    .into()
            }
            Lyrics::Unsynced(text) => {
                widget::scrollable(widget::container(widget::text(text.as_str())).padding(8))
                    .height(Length::Fill)
                    .into()
            }
        }
    } else {
        widget::container(
            widget::Column::new()
                .push(widget::text(fl!("lyrics-unavailable")))
                .push(
                    widget::button::suggested(fl!("lyrics-search-online"))
                        .on_press(LyricsMessage::FetchLyrics),
                )
                .spacing(8)
                .align_x(Alignment::Center),
        )
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
    };

    widget::Column::new()
        .push(header)
        .push(widget::divider::horizontal::default())
        .push(content)
        .spacing(12)
        .padding(16)
        .width(Length::Fill)
        .height(Length::Shrink)
        .into()
}

/// Render a single synced lyric line with a timestamp prefix.
///
/// Task 104: the current line uses `current_color` (the cover-art accent
/// when available, else the theme accent — resolved once by the caller);
/// others are dimmed gray.
fn synced_line_widget(
    line: &LyricLine,
    is_current: bool,
    current_color: Color,
) -> cosmic::Element<'_, LyricsMessage> {
    let total_secs = line.timestamp_ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    let timestamp = format!("[{mins:02}:{secs:02}]");

    let color = if is_current {
        current_color
    } else {
        Color::from_rgba(0.6, 0.6, 0.6, 0.7) // dimmed
    };

    widget::Row::new()
        .push(widget::text::caption(timestamp).class(cosmic::theme::Text::Color(color)))
        .push(widget::text(&line.text).class(cosmic::theme::Text::Color(color)))
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

/// Render lyrics as an in-view overlay for the expanded now-playing view:
/// no card/header chrome, transparent background (the cover-art or
/// visualizer backdrop shows through), and caller-supplied colors so it can
/// match whichever backdrop treatment is active (see
/// `expanded_view::BACKDROP_TEXT`/`BACKDROP_SUBTEXT`). Unlike `lyrics_view`
/// there's no "Search Online" affordance here — that stays on the sidebar
/// panel reachable from the collapsed bar, keeping this overlay read-only
/// and free of extra message plumbing. `accent`, when present, tints the
/// current line in place of `text_color`; dimmed lines always stay
/// `subtext_color` regardless of accent.
pub fn lyrics_overlay_view<'a, M: 'static>(
    lyrics: Option<&'a Lyrics>,
    is_loading: bool,
    playback_position: Duration,
    text_color: Color,
    subtext_color: Color,
    accent: Option<&Accent>,
) -> cosmic::Element<'a, M> {
    let current_line_color = accent
        .map(|a| Color::from_rgb(a.color[0], a.color[1], a.color[2]))
        .unwrap_or(text_color);

    let content: cosmic::Element<'_, M> = if is_loading {
        widget::container(
            widget::text(fl!("lyrics-loading")).class(cosmic::theme::Text::Color(subtext_color)),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
    } else if let Some(lyrics_data) = lyrics {
        match lyrics_data {
            Lyrics::Synced(lines) => {
                let current_idx = find_current_line_index(lines, playback_position);
                let mut col = widget::Column::new().spacing(10).width(Length::Fill);
                for (i, line) in lines.iter().enumerate() {
                    let is_current = current_idx == Some(i);
                    col = col.push(overlay_line_widget(
                        line,
                        is_current,
                        current_line_color,
                        subtext_color,
                    ));
                }
                widget::scrollable(widget::container(col).width(Length::Fill).padding(24))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            }
            Lyrics::Unsynced(text) => widget::scrollable(
                widget::container(
                    widget::text(text.as_str())
                        .class(cosmic::theme::Text::Color(text_color))
                        .align_x(Horizontal::Center),
                )
                .width(Length::Fill)
                .padding(24),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        }
    } else {
        widget::container(
            widget::text(fl!("lyrics-unavailable"))
                .class(cosmic::theme::Text::Color(subtext_color)),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
    };

    content
}

/// Overlay variant of `synced_line_widget`: bigger, centered, and using
/// caller-supplied colors instead of a fixed accent/dimmed pair, since this
/// renders over arbitrary cover-art/visualizer backdrops rather than a flat
/// theme surface.
fn overlay_line_widget<'a, M: 'static>(
    line: &LyricLine,
    is_current: bool,
    text_color: Color,
    subtext_color: Color,
) -> cosmic::Element<'a, M> {
    let color = if is_current {
        text_color
    } else {
        subtext_color
    };
    let text_widget = if is_current {
        widget::text::title4(line.text.clone())
    } else {
        widget::text::body(line.text.clone())
    };
    widget::container(text_widget.class(cosmic::theme::Text::Color(color)))
        .width(Length::Fill)
        .align_x(Horizontal::Center)
        .into()
}
