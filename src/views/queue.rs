// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The "Up Next" play-queue drawer.
//!
//! The queue can be the entire library (thousands of tracks), so this view
//! never materializes a row per track: [`visible_window`] computes a
//! bounded slice — a short lookback of already-played entries for context,
//! the current entry, and a capped run of upcoming ones — and only that
//! slice is rendered, with a trailing caption for whatever's left out.

use crate::fl;
use crate::library::{CoverArt, Track};
use crate::player::PlaybackState;
use crate::views::{common, list_row_button_class};
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use cosmic::widget::tooltip::Position as TooltipPosition;
use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;

/// Messages from the queue drawer. Every index is a position in *play
/// order* (`Player::queue()`), matching `Message::Queue*`'s contract.
#[derive(Debug, Clone)]
pub enum QueueMessage {
    /// Jump to and play the entry at this play-order index.
    Jump(usize),
    /// Move the entry at this index one position earlier.
    MoveUp(usize),
    /// Move the entry at this index one position later.
    MoveDown(usize),
    /// Remove the entry at this index.
    Remove(usize),
    /// Drop every entry except the one currently playing.
    Clear,
    /// Toggle "stop after the current track".
    ToggleStopAfter,
    /// Jump to another view (artist page).
    Navigate(crate::views::Route),
}

/// Artist caption that links to the artist page (plain when empty).
fn artist_link(track: &Track) -> cosmic::Element<'_, QueueMessage> {
    if track.artist.trim().is_empty() {
        return common::cell_caption(track.artist.as_str()).into();
    }
    common::link(
        common::cell_caption(track.artist.as_str()),
        true,
        QueueMessage::Navigate(crate::views::Route::Artist(track.artist.clone())),
    )
}

/// How many already-played entries stay visible above the current one, for
/// context (like "recently played").
const LOOKBACK: usize = 10;
/// How many upcoming entries render at most below the current one.
const MAX_UPCOMING: usize = 200;

/// The `[start, end)` slice of a `queue_len`-long queue to actually render
/// around `current` — never the whole queue.
pub fn visible_window(queue_len: usize, current: usize) -> Range<usize> {
    if queue_len == 0 {
        return 0..0;
    }
    let current = current.min(queue_len - 1);
    let start = current.saturating_sub(LOOKBACK);
    let end = (current + 1 + MAX_UPCOMING).min(queue_len);
    start..end
}

/// Cover-art cache key for `track`, matching the lookup in `view.rs`'s
/// `current_cover`: album_artist when present, else artist.
fn cover_key(track: &Track) -> String {
    let artist = if track.album_artist.is_empty() {
        &track.artist
    } else {
        &track.album_artist
    };
    CoverArt::album_key(artist, &track.album)
}

/// Localized "Up next · N tracks · duration" section label.
fn up_next_label(count: usize, duration: Duration) -> String {
    let duration = common::format_duration_coarse(duration.as_secs());
    if count == 1 {
        fl!(
            "queue-up-next-one",
            count = count.to_string(),
            duration = duration
        )
    } else {
        fl!(
            "queue-up-next-other",
            count = count.to_string(),
            duration = duration
        )
    }
}

/// Theme-driven dimmed text colour for already-played entries.
fn dim_text() -> cosmic::theme::Text {
    cosmic::theme::Text::Color(cosmic::theme::active().cosmic().palette.neutral_7.into())
}

/// Where a queue row sits relative to the entry being played.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Played,
    Current,
    Upcoming,
}

/// Small quiet icon button with a tooltip, for the per-row actions.
fn row_action<'a>(
    icon_name: &'static str,
    label: String,
    on_press: Option<QueueMessage>,
) -> cosmic::Element<'a, QueueMessage> {
    widget::tooltip(
        widget::button::icon(widget::icon::from_name(icon_name).size(14))
            .extra_small()
            .on_press_maybe(on_press),
        widget::text::caption(label),
        TooltipPosition::Top,
    )
    .into()
}

/// One queue row: small art, title/artist, duration, and move-up/down +
/// remove icon buttons. Clicking the row jumps to and plays that entry.
#[allow(clippy::too_many_arguments)]
fn queue_row<'a>(
    index: usize,
    track: &'a Track,
    kind: RowKind,
    state: PlaybackState,
    can_move_up: bool,
    can_move_down: bool,
    cover_images: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, QueueMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let art = common::list_art_icon(
        cover_images.get(&cover_key(track)),
        40,
        "media-optical-symbolic",
    );

    let title = if kind == RowKind::Played {
        common::cell_text(track.title.as_str()).class(dim_text())
    } else {
        common::cell_text(track.title.as_str())
    };
    let info = widget::Column::new()
        .push(title)
        .push(artist_link(track))
        .spacing(2);

    let mut row = widget::Row::new()
        .push(art)
        .push(common::clipped_cell(info.into()))
        .spacing(sp.space_xs)
        .align_y(Alignment::Center)
        .padding([sp.space_xxs, sp.space_xs]);

    if kind == RowKind::Current {
        let icon_name = if state == PlaybackState::Playing {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        };
        row = row.push(widget::icon::from_name(icon_name).size(16));
    }

    row = row
        .push(common::duration_cell(track.duration.as_secs()))
        .push(
            widget::Row::new()
                .push(row_action(
                    "go-up-symbolic",
                    fl!("queue-move-up"),
                    can_move_up.then_some(QueueMessage::MoveUp(index)),
                ))
                .push(row_action(
                    "go-down-symbolic",
                    fl!("queue-move-down"),
                    can_move_down.then_some(QueueMessage::MoveDown(index)),
                ))
                .push(row_action(
                    "list-remove-symbolic",
                    fl!("queue-remove"),
                    Some(QueueMessage::Remove(index)),
                ))
                .align_y(Alignment::Center),
        );

    widget::button::custom(row)
        .on_press(QueueMessage::Jump(index))
        .width(Length::Fill)
        .padding(0)
        .class(list_row_button_class(kind == RowKind::Current))
        .into()
}

/// Pinned header for the queue drawer (placed above the scrolling rows via
/// `ContextDrawer::header`): a now-playing card with art, then the
/// "Up next · N tracks · duration" summary with the Clear action. `None`
/// when there is nothing queued.
pub fn queue_header_view<'a>(
    queue_data: Option<(&'a [Track], usize)>,
    current_track: Option<&'a Track>,
    state: PlaybackState,
    cover_images: &'a HashMap<String, widget::icon::Handle>,
) -> Option<cosmic::Element<'a, QueueMessage>> {
    let (queue, current_index) = queue_data.filter(|(q, _)| !q.is_empty())?;
    let sp = cosmic::theme::active().cosmic().spacing;

    let current_index = current_index.min(queue.len() - 1);
    let upcoming_count = queue.len() - (current_index + 1);
    let upcoming_duration: Duration = queue[current_index + 1..].iter().map(|t| t.duration).sum();

    let mut col = widget::Column::new()
        .spacing(sp.space_s)
        .width(Length::Fill);

    if let Some(track) = current_track {
        let art: cosmic::Element<'a, QueueMessage> = match cover_images.get(&cover_key(track)) {
            Some(handle) => common::cover_art(
                handle,
                56.0,
                cosmic::theme::active().cosmic().corner_radii.radius_s[0],
                true,
            ),
            None => common::list_art_icon(None, 56, "media-optical-symbolic"),
        };
        let indicator = if state == PlaybackState::Playing {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        };
        col = col.push(
            widget::container(
                widget::Row::new()
                    .push(art)
                    .push(common::clipped_cell(
                        widget::Column::new()
                            .push(
                                widget::text::caption(fl!("queue-now-playing-label"))
                                    .class(dim_text()),
                            )
                            .push(
                                widget::text::heading(track.title.as_str())
                                    .wrapping(cosmic::iced::core::text::Wrapping::None),
                            )
                            .push(artist_link(track))
                            .spacing(2)
                            .into(),
                    ))
                    .push(widget::icon::from_name(indicator).size(20))
                    .spacing(sp.space_s)
                    .align_y(Alignment::Center)
                    .padding(sp.space_xs),
            )
            .class(cosmic::theme::Container::Card)
            .width(Length::Fill),
        );
    }

    let clear_enabled = upcoming_count > 0;
    col = col.push(
        widget::Row::new()
            .push(
                widget::text::body(up_next_label(upcoming_count, upcoming_duration))
                    .class(dim_text())
                    .width(Length::Fill),
            )
            .push(
                widget::button::text(fl!("queue-clear"))
                    .on_press_maybe(clear_enabled.then_some(QueueMessage::Clear)),
            )
            .spacing(sp.space_xs)
            .align_y(Alignment::Center),
    );

    if current_track.is_some() {
        col = col.push(
            widget::Row::new()
                .push(
                    widget::text::body(fl!("stop-after-track"))
                        .class(dim_text())
                        .width(Length::Fill),
                )
                .push(
                    widget::toggler(crate::player::party::stop_after_flag())
                        .on_toggle(|_| QueueMessage::ToggleStopAfter),
                )
                .spacing(sp.space_xs)
                .align_y(Alignment::Center),
        );
    }

    Some(col.into())
}

/// Empty-queue placeholder. Not `common::empty_state`: the drawer's own
/// scrollable gives its content unbounded height, so a `Length::Fill`
/// height can't centre anything here.
fn empty_queue<'a>() -> cosmic::Element<'a, QueueMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    widget::container(
        widget::Column::new()
            .push(
                widget::icon::icon(
                    widget::icon::from_name("media-playlist-consecutive-symbolic").handle(),
                )
                .size(56)
                .class(cosmic::theme::Svg::custom(|theme| {
                    cosmic::iced::widget::svg::Style {
                        color: Some(theme.cosmic().palette.neutral_6.into()),
                    }
                })),
            )
            .push(widget::text::title3(fl!("queue-empty")))
            .push(
                widget::text::body(fl!("queue-empty-hint"))
                    .class(dim_text())
                    .align_x(cosmic::iced::alignment::Horizontal::Center),
            )
            .spacing(sp.space_xs)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([sp.space_xl, sp.space_s])
    .align_x(cosmic::iced::alignment::Horizontal::Center)
    .into()
}

/// Render the queue drawer body: the current entry, the upcoming rows, and
/// (at the end, so the list opens on what's playing) a short run of
/// recently played entries. The now-playing card and Clear action live in
/// [`queue_header_view`], pinned above this scrolling list.
///
/// `queue_data` is `self.player.as_ref().map(|p| (p.queue(), p.queue_index()))`
/// — `None` when there is no active player (nothing has ever played).
pub fn queue_view<'a>(
    queue_data: Option<(&'a [Track], usize)>,
    current_track: Option<&'a Track>,
    state: PlaybackState,
    cover_images: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, QueueMessage> {
    let _ = current_track;
    let sp = cosmic::theme::active().cosmic().spacing;

    let Some((queue, current_index)) = queue_data.filter(|(q, _)| !q.is_empty()) else {
        return empty_queue();
    };

    let current_index = current_index.min(queue.len() - 1);
    let window = visible_window(queue.len(), current_index);
    let hidden_after = queue.len() - window.end;

    let make_row = |i: usize, kind: RowKind| {
        queue_row(
            i,
            &queue[i],
            kind,
            state,
            i > 0,
            i + 1 < queue.len(),
            cover_images,
        )
    };

    let mut rows = widget::Column::new().spacing(2).width(Length::Fill);
    rows = rows.push(make_row(current_index, RowKind::Current));
    for i in current_index + 1..window.end {
        rows = rows.push(make_row(i, RowKind::Upcoming));
    }
    if hidden_after > 0 {
        rows = rows.push(
            widget::container(
                common::cell_caption(fl!("queue-more-hidden", count = hidden_after.to_string()))
                    .class(dim_text()),
            )
            .padding([sp.space_xxs, sp.space_xs]),
        );
    }

    if window.start < current_index {
        rows = rows.push(
            widget::container(widget::text::heading(fl!("queue-history-label"))).padding([
                sp.space_s,
                sp.space_xs,
                sp.space_xxs,
                sp.space_xs,
            ]),
        );
        // Most recently played first.
        for i in (window.start..current_index).rev() {
            rows = rows.push(make_row(i, RowKind::Played));
        }
    }

    rows.into()
}

#[cfg(test)]
mod tests {
    use super::visible_window;

    #[test]
    fn empty_queue_yields_empty_window() {
        assert_eq!(visible_window(0, 0), 0..0);
    }

    #[test]
    fn small_queue_window_covers_the_whole_queue() {
        assert_eq!(visible_window(5, 2), 0..5);
    }

    #[test]
    fn window_clamps_lookback_at_the_start() {
        // current=3, lookback=10 -> start would be negative; clamps to 0.
        assert_eq!(visible_window(500, 3), 0..204);
    }

    #[test]
    fn window_clamps_upcoming_at_the_end() {
        // current near the end: end can't exceed queue_len.
        assert_eq!(visible_window(500, 490), 480..500);
    }

    #[test]
    fn large_queue_window_is_bounded_on_both_sides() {
        // Deep in a huge (whole-library) queue: window stays small.
        let window = visible_window(50_000, 25_000);
        assert_eq!(window, 24_990..25_201);
        assert_eq!(window.len(), 211);
    }

    #[test]
    fn current_index_past_queue_end_is_clamped() {
        // Defensive: a stale/out-of-range index (e.g. mid-mutation) must
        // never panic on the `queue_len - 1` subtraction.
        assert_eq!(visible_window(10, 999), 0..10);
    }
}
