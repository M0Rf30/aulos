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

/// Theme-driven dimmed text colour for secondary labels.
fn dim_color() -> Color {
    cosmic::theme::active().cosmic().palette.neutral_7.into()
}

/// Cover-art accent when one was extracted, else the theme accent.
fn highlight_color(accent: Option<&Accent>) -> Color {
    accent
        .map(|a| Color::from_rgb(a.color[0], a.color[1], a.color[2]))
        .unwrap_or_else(|| cosmic::theme::active().cosmic().accent_color().into())
}

/// Pinned header for the lyrics drawer (placed above the scrolling lines via
/// `ContextDrawer::header`): track title/artist and, for synced lyrics, a
/// "now singing" card with the current line so it stays visible while the
/// list below is scrolled elsewhere.
pub fn lyrics_header_view<'a>(
    lyrics: Option<&'a Lyrics>,
    track_title: &'a str,
    track_artist: &'a str,
    playback_position: Duration,
    accent: Option<&Accent>,
) -> cosmic::Element<'a, LyricsMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;

    let mut col = widget::Column::new()
        .push(
            widget::text::title4(track_title)
                .width(Length::Fill)
                .align_x(Horizontal::Center),
        )
        .push(
            widget::text::caption(track_artist)
                .class(cosmic::theme::Text::Color(dim_color()))
                .width(Length::Fill)
                .align_x(Horizontal::Center),
        )
        .spacing(2)
        .width(Length::Fill);

    if let Some(Lyrics::Synced(lines)) = lyrics
        && let Some(idx) = find_current_line_index(lines, playback_position)
        && !lines[idx].text.trim().is_empty()
    {
        let color = highlight_color(accent);
        col = col.push(
            widget::container(
                widget::text::body(lines[idx].text.as_str())
                    .class(cosmic::theme::Text::Color(color))
                    .width(Length::Fill)
                    .align_x(Horizontal::Center),
            )
            .padding([sp.space_xs, sp.space_s])
            .width(Length::Fill)
            .class(cosmic::theme::Container::Card),
        );
        // Breathing room between the card and the title block.
        col = col.spacing(sp.space_xs);
    }

    col.into()
}

/// Render the lyrics panel body (shown in the context drawer, which already
/// supplies the padding and the scrolling).
///
/// Task 104-105: `playback_position` drives synced lyrics highlighting.
/// `accent` (cover-art accent, when one was extracted) tints the current
/// line instead of the theme accent; `None` falls back to
/// `cosmic().accent_color()`.
pub fn lyrics_view<'a>(
    lyrics: Option<&'a Lyrics>,
    is_loading: bool,
    playback_position: Duration,
    accent: Option<&Accent>,
) -> cosmic::Element<'a, LyricsMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let current_line_color = highlight_color(accent);

    if is_loading {
        return centered_block(
            widget::Column::new()
                .push(
                    widget::icon::icon(
                        widget::icon::from_name("content-loading-symbolic").handle(),
                    )
                    .size(32),
                )
                .push(
                    widget::text::body(fl!("lyrics-loading"))
                        .class(cosmic::theme::Text::Color(dim_color())),
                )
                .spacing(sp.space_xs)
                .align_x(Alignment::Center)
                .into(),
        );
    }

    match lyrics {
        Some(Lyrics::Synced(lines)) => {
            let current_idx = find_current_line_index(lines, playback_position);
            let mut col = widget::Column::new()
                .spacing(sp.space_xs)
                .width(Length::Fill);
            for (i, line) in lines.iter().enumerate() {
                let is_current = current_idx == Some(i);
                // Lines fade with distance from the current one, so the eye
                // stays on the line being sung.
                let distance = current_idx.map_or(i + 1, |c| c.abs_diff(i)) as f32;
                let fade = (1.0 - 0.12 * (distance - 1.0).max(0.0)).max(0.45);
                col = col.push(synced_line_widget(
                    line,
                    is_current,
                    current_line_color,
                    fade,
                ));
            }
            widget::container(col)
                .width(Length::Fill)
                .padding([sp.space_xxs, 0])
                .into()
        }
        Some(Lyrics::Unsynced(text)) => {
            widget::container(widget::text::body(text.as_str()).width(Length::Fill))
                .width(Length::Fill)
                .padding([sp.space_xxs, 0])
                .into()
        }
        None => centered_block(
            widget::Column::new()
                .push(
                    widget::icon::icon(
                        widget::icon::from_name("audio-x-generic-symbolic").handle(),
                    )
                    .size(48)
                    .class(cosmic::theme::Svg::custom(|theme| {
                        cosmic::iced::widget::svg::Style {
                            color: Some(theme.cosmic().palette.neutral_6.into()),
                        }
                    })),
                )
                .push(widget::text::title4(fl!("lyrics-unavailable")))
                .push(
                    widget::text::caption(fl!("lyrics-unavailable-hint"))
                        .class(cosmic::theme::Text::Color(dim_color()))
                        .align_x(Horizontal::Center),
                )
                .push(
                    widget::button::suggested(fl!("lyrics-search-online"))
                        .leading_icon(widget::icon::from_name("system-search-symbolic").size(16))
                        .on_press(LyricsMessage::FetchLyrics),
                )
                .spacing(sp.space_s)
                .align_x(Alignment::Center)
                .into(),
        ),
    }
}

/// Centred placeholder block with generous vertical padding. The drawer's
/// own scrollable gives its content unbounded height, so `Length::Fill`
/// heights can't be used for centring here.
fn centered_block<'a>(
    content: cosmic::Element<'a, LyricsMessage>,
) -> cosmic::Element<'a, LyricsMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    widget::container(content)
        .width(Length::Fill)
        .padding([sp.space_xl, sp.space_s])
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
}

/// Render a single synced lyric line.
///
/// Task 104: the current line is larger and uses `current_color` (the
/// cover-art accent when available, else the theme accent — resolved once
/// by the caller); others are dimmed with the theme's secondary colour,
/// faded by `fade` (1.0 = next to the current line).
fn synced_line_widget(
    line: &LyricLine,
    is_current: bool,
    current_color: Color,
    fade: f32,
) -> cosmic::Element<'_, LyricsMessage> {
    let dim = dim_color();
    let text = if line.text.trim().is_empty() {
        "♪"
    } else {
        line.text.as_str()
    };

    if is_current {
        widget::text::title4(text)
            .class(cosmic::theme::Text::Color(current_color))
            .width(Length::Fill)
            .into()
    } else {
        widget::text::body(text)
            .class(cosmic::theme::Text::Color(Color {
                a: dim.a * fade,
                ..dim
            }))
            .width(Length::Fill)
            .into()
    }
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
                // Lead slightly: positions arrive every ~500ms, and a line
                // that lights up a beat early reads better than one late.
                let current_idx =
                    find_current_line_index(lines, playback_position + Duration::from_millis(250));
                let mut col = widget::Column::new()
                    .spacing(14)
                    .width(Length::Fill)
                    .padding([0, 24]);
                for (i, line) in lines.iter().enumerate() {
                    let is_current = current_idx == Some(i);
                    // Lines fade with distance from the current one, so the
                    // eye stays on the line being sung.
                    let distance = current_idx.map_or(i + 1, |c| c.abs_diff(i)) as f32;
                    let fade = (1.0 - 0.18 * (distance - 1.0).max(0.0)).max(0.3);
                    col = col.push(overlay_line_widget(
                        line,
                        is_current,
                        current_line_color,
                        Color {
                            a: subtext_color.a * fade,
                            ..subtext_color
                        },
                    ));
                }
                super::lyrics_follow::follow(col, current_idx).into()
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
