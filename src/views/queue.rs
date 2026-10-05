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

/// One queue row: small art, title/artist, duration, and move-up/down +
/// remove icon buttons. Clicking the row jumps to and plays that entry.
#[allow(clippy::too_many_arguments)]
fn queue_row<'a>(
    index: usize,
    track: &'a Track,
    is_current: bool,
    can_move_up: bool,
    can_move_down: bool,
    cover_images: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, QueueMessage> {
    let art = common::list_art_icon(
        cover_images.get(&cover_key(track)),
        40,
        "media-optical-cd-audio-symbolic",
    );

    let info = widget::Column::new()
        .push(common::cell_text(track.title.as_str()))
        .push(common::cell_caption(track.artist.as_str()))
        .spacing(2);

    let move_up = widget::tooltip(
        widget::button::icon(widget::icon::from_name("go-up-symbolic").size(14))
            .on_press_maybe(can_move_up.then_some(QueueMessage::MoveUp(index))),
        widget::text::caption(fl!("queue-move-up")),
        TooltipPosition::Top,
    );
    let move_down = widget::tooltip(
        widget::button::icon(widget::icon::from_name("go-down-symbolic").size(14))
            .on_press_maybe(can_move_down.then_some(QueueMessage::MoveDown(index))),
        widget::text::caption(fl!("queue-move-down")),
        TooltipPosition::Top,
    );
    let remove = widget::tooltip(
        widget::button::icon(widget::icon::from_name("list-remove-symbolic").size(14))
            .on_press(QueueMessage::Remove(index)),
        widget::text::caption(fl!("queue-remove")),
        TooltipPosition::Top,
    );

    let row = widget::Row::new()
        .push(art)
        .push(common::clipped_cell(info.into()))
        .push(common::duration_cell(track.duration.as_secs()))
        .push(move_up)
        .push(move_down)
        .push(remove)
        .spacing(10)
        .align_y(Alignment::Center)
        .padding([6, 8]);

    widget::button::custom(row)
        .on_press(QueueMessage::Jump(index))
        .width(Length::Fill)
        .padding(0)
        .class(list_row_button_class(is_current))
        .into()
}

/// Render the queue drawer.
///
/// `queue_data` is `self.player.as_ref().map(|p| (p.queue(), p.queue_index()))`
/// — `None` when there is no active player (nothing has ever played).
pub fn queue_view<'a>(
    queue_data: Option<(&'a [Track], usize)>,
    current_track: Option<&'a Track>,
    state: PlaybackState,
    cover_images: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, QueueMessage> {
    let _ = state;

    let Some((queue, current_index)) = queue_data.filter(|(q, _)| !q.is_empty()) else {
        return common::empty_state(
            "media-playlist-consecutive-symbolic",
            fl!("queue-empty"),
            fl!("queue-empty-hint"),
        );
    };

    let current_index = current_index.min(queue.len() - 1);
    let upcoming_count = queue.len() - (current_index + 1);
    let upcoming_duration: Duration = queue[current_index + 1..].iter().map(|t| t.duration).sum();

    // Compact, non-interactive summary of the current entry: stays visible
    // even once the row list below has scrolled past it.
    let now_playing_row: cosmic::Element<'a, QueueMessage> = if let Some(track) = current_track {
        let art = common::list_art_icon(
            cover_images.get(&cover_key(track)),
            48,
            "media-optical-cd-audio-symbolic",
        );
        widget::container(
            widget::Row::new()
                .push(art)
                .push(common::clipped_cell(
                    widget::Column::new()
                        .push(common::cell_text(track.title.as_str()))
                        .push(common::cell_caption(track.artist.as_str()))
                        .spacing(2)
                        .into(),
                ))
                .spacing(10)
                .align_y(Alignment::Center)
                .padding(8),
        )
        .class(cosmic::theme::Container::Card)
        .width(Length::Fill)
        .into()
    } else {
        widget::Space::new().width(0).height(0).into()
    };

    let clear_enabled = upcoming_count > 0;
    let header = widget::Column::new()
        .push(widget::text::title4(fl!("queue-now-playing-label")))
        .push(now_playing_row)
        .push(
            widget::Row::new()
                .push(
                    widget::text::body(up_next_label(upcoming_count, upcoming_duration))
                        .width(Length::Fill),
                )
                .push(
                    widget::button::standard(fl!("queue-clear"))
                        .on_press_maybe(clear_enabled.then_some(QueueMessage::Clear)),
                )
                .spacing(8)
                .align_y(Alignment::Center),
        )
        .spacing(8)
        .padding([12, 12, 4, 12]);

    let window = visible_window(queue.len(), current_index);
    let hidden_after = queue.len() - window.end;

    let mut rows = widget::Column::new().spacing(2);
    for i in window {
        rows = rows.push(queue_row(
            i,
            &queue[i],
            i == current_index,
            i > 0,
            i + 1 < queue.len(),
            cover_images,
        ));
    }
    if hidden_after > 0 {
        rows = rows.push(
            widget::container(common::cell_caption(fl!(
                "queue-more-hidden",
                count = hidden_after.to_string()
            )))
            .padding([6, 8]),
        );
    }

    widget::Column::new()
        .push(header)
        .push(widget::divider::horizontal::default())
        .push(widget::scrollable(widget::container(rows).width(Length::Fill)).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
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
