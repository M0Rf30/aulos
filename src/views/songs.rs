// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use crate::fl;
use crate::library::{Playlist, Track};
use crate::views::common;
use crate::views::track_row::{self, HeaderColumn};
use cosmic::iced::alignment::Horizontal;
use cosmic::iced::{Alignment, Length, Size};
use cosmic::widget;

#[derive(Debug, Clone)]
pub enum SongMessage {
    PlayTrack(usize),
    /// Insert this track right after the currently playing one.
    PlayNext(usize),
    /// Append this track to the end of the queue.
    AddToQueue(usize),
    SortBy(SortField),
    ToggleFavorite(String),
    SetRating(String, u8),
    AddToPlaylist(String, String),
    ToggleFavoritesFilter,
    FilterByGenre(String),
    ClearGenreFilter,
    /// The track list was scrolled to this vertical offset (px).
    Scrolled(f32),
    /// Jump to another view (artist / album page).
    Navigate(crate::views::Route),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Title,
    Artist,
    Album,
    Duration,
}

/// Row height/gap come from the shared track row. Rows must be uniform for
/// the list virtualization below: the visible window is computed
/// arithmetically from the scroll offset.
const ROW_STRIDE: f32 = track_row::ROW_STRIDE;
/// Extra rows built above and below the viewport so fast scrolling never
/// reveals a blank edge before the next view rebuild.
const OVERSCAN_ROWS: usize = 8;

/// Half-open range of row indices to build for a `viewport_height`-tall
/// list scrolled to `offset`, out of `count` rows.
fn visible_rows(offset: f32, viewport_height: f32, count: usize) -> std::ops::Range<usize> {
    let max_offset = (count as f32 * ROW_STRIDE - viewport_height).max(0.0);
    let offset = offset.clamp(0.0, max_offset);
    let first = ((offset / ROW_STRIDE).floor() as usize).saturating_sub(OVERSCAN_ROWS);
    let last =
        (((offset + viewport_height) / ROW_STRIDE).ceil() as usize + OVERSCAN_ROWS).min(count);
    first.min(last)..last
}

#[allow(clippy::too_many_arguments)]
pub fn songs_list_view<'a>(
    tracks: &'a [Track],
    current_sort: SortField,
    sort_descending: bool,
    favorites_filter: bool,
    genre_filter: Option<&'a str>,
    playlists: &'a [Playlist],
    current_track_id: Option<i64>,
    scroll_offset: f32,
) -> cosmic::Element<'a, SongMessage> {
    if tracks.is_empty() {
        return common::empty_state(
            "audio-x-generic-symbolic",
            fl!("no-songs"),
            fl!("songs-empty-hint"),
        );
    }

    let filtered: Vec<(usize, &Track)> = tracks
        .iter()
        .enumerate()
        .filter(|(_i, t)| {
            if favorites_filter && !t.is_favorite {
                return false;
            }
            if let Some(genre) = genre_filter
                && !t.genre.eq_ignore_ascii_case(genre)
            {
                return false;
            }
            true
        })
        .collect();

    // Filter bar: favorites pill, active genre pill, track count.
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut filter_bar = widget::Row::new()
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center);

    let fav_icon = if favorites_filter {
        "emblem-favorite-symbolic"
    } else {
        "non-starred-symbolic"
    };
    filter_bar = filter_bar.push(
        widget::button::custom(
            widget::Row::new()
                .push(widget::icon::from_name(fav_icon).size(16))
                .push(widget::text::body(fl!("songs-favorites-filter")))
                .spacing(spacing.space_xxs)
                .align_y(Alignment::Center),
        )
        .padding([5, 14])
        .on_press(SongMessage::ToggleFavoritesFilter)
        .class(track_row::pill_class(favorites_filter)),
    );

    if let Some(genre) = genre_filter {
        filter_bar = filter_bar.push(widget::tooltip(
            widget::button::custom(
                widget::Row::new()
                    .push(widget::text::body(genre))
                    .push(widget::icon::from_name("window-close-symbolic").size(12))
                    .spacing(spacing.space_xs)
                    .align_y(Alignment::Center),
            )
            .padding([5, 14])
            .on_press(SongMessage::ClearGenreFilter)
            .class(track_row::pill_class(true)),
            widget::text::caption(fl!("songs-clear-genre-filter")),
            widget::tooltip::Position::Bottom,
        ));
    }

    filter_bar = filter_bar.push(
        widget::container(common::cell_caption(track_count_label(filtered.len())))
            .width(Length::Fill)
            .align_x(Horizontal::Right),
    );

    let table_area: cosmic::Element<'a, SongMessage> = if filtered.is_empty() {
        common::empty_state(
            "edit-find-symbolic",
            fl!("songs-no-matches"),
            fl!("songs-no-matches-hint"),
        )
    } else {
        let has_playlists = !playlists.is_empty();
        widget::responsive(move |size: Size| {
            // The right edge reserves a gutter for the vertical scrollbar,
            // which iced overlays on content instead of laying out around.
            let gutter = f32::from(cosmic::theme::active().cosmic().spacing.space_s);
            let columns = songs_columns(size.width, has_playlists, gutter);
            let header = build_header(columns, current_sort, sort_descending);

            // Virtualized: only rows intersecting the viewport (plus
            // overscan) are built; fixed-height spacers stand in for the
            // rest so the scrollbar still reflects the whole list. With
            // thousands of tracks this keeps every view rebuild — and the
            // layout/draw passes after it — proportional to the screen,
            // not to the library.
            let range = visible_rows(scroll_offset, size.height, filtered.len());
            let spacer = |rows: usize| {
                widget::Space::new()
                    .width(Length::Fill)
                    .height(Length::Fixed(rows as f32 * ROW_STRIDE))
            };
            let mut track_list = widget::Column::new().push(spacer(range.start));
            for &(original_index, track) in &filtered[range.clone()] {
                let is_playing = current_track_id == Some(track.id);
                track_list = track_list.push(
                    widget::container(build_row(
                        original_index,
                        track,
                        is_playing,
                        playlists,
                        columns,
                    ))
                    .height(Length::Fixed(ROW_STRIDE)),
                );
            }
            track_list = track_list.push(spacer(filtered.len() - range.end));

            widget::Column::new()
                .push(header)
                .push(
                    widget::scrollable(widget::container(track_list).width(Length::Fill))
                        .on_scroll(|viewport| SongMessage::Scrolled(viewport.absolute_offset().y))
                        .height(Length::Fill),
                )
                .spacing(4)
                .into()
        })
        .into()
    };

    widget::Column::new()
        .push(filter_bar)
        .push(table_area)
        .padding(16)
        .spacing(8)
        .into()
}

/// Column set for the Songs table at `width` px (see
/// [`track_row::Columns::responsive`]); `has_playlists` hides the
/// add-to-playlist column when there is nowhere to add to.
fn songs_columns(width: f32, has_playlists: bool, gutter: f32) -> track_row::Columns {
    track_row::Columns {
        artist: true,
        album: true,
        genre: true,
        favorite: true,
        rating: true,
        quality: true,
        queue_actions: true,
        playlist: has_playlists,
        trailing: false,
        gutter,
    }
    .responsive(width)
}

/// Build the column header row: dim sortable labels over the exact same
/// columns the rows use.
fn build_header<'a>(
    columns: track_row::Columns,
    current_sort: SortField,
    sort_descending: bool,
) -> cosmic::Element<'a, SongMessage> {
    let sorted = |field: SortField| (field == current_sort).then_some(sort_descending);
    track_row::Header::new(columns)
        .sortable(
            HeaderColumn::Title,
            SongMessage::SortBy(SortField::Title),
            sorted(SortField::Title),
        )
        .sortable(
            HeaderColumn::Artist,
            SongMessage::SortBy(SortField::Artist),
            sorted(SortField::Artist),
        )
        .sortable(
            HeaderColumn::Album,
            SongMessage::SortBy(SortField::Album),
            sorted(SortField::Album),
        )
        .sortable(
            HeaderColumn::Duration,
            SongMessage::SortBy(SortField::Duration),
            sorted(SortField::Duration),
        )
        .view()
}

/// Build a single track row from the shared [`track_row::TrackRow`].
fn build_row<'a>(
    original_index: usize,
    track: &'a Track,
    is_playing: bool,
    playlists: &'a [Playlist],
    columns: track_row::Columns,
) -> cosmic::Element<'a, SongMessage> {
    let track_id = track.id.to_string();
    let rating_track_id = track_id.clone();
    track_row::TrackRow::new(
        track,
        (original_index + 1).to_string(),
        is_playing,
        columns,
        SongMessage::PlayTrack(original_index),
    )
    .with_navigate(SongMessage::Navigate)
    .with_favorite(SongMessage::ToggleFavorite(track_id))
    .with_rating(move |r| SongMessage::SetRating(rating_track_id.clone(), r))
    .with_genre_filter(SongMessage::FilterByGenre(track.genre.clone()))
    .with_queue_actions(
        SongMessage::PlayNext(original_index),
        SongMessage::AddToQueue(original_index),
    )
    .with_add_to_playlist(playlists, SongMessage::AddToPlaylist)
    // Without an artist column the artist moves under the title.
    .with_artist_subtitle(!columns.artist)
    .view()
}

/// Localized "N tracks" label for the filter bar.
fn track_count_label(count: usize) -> String {
    if count == 1 {
        fl!("songs-track-count-one", count = count.to_string())
    } else {
        fl!("songs-track-count-other", count = count.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_rows_at_top_covers_viewport_plus_overscan() {
        let r = visible_rows(0.0, 10.0 * ROW_STRIDE, 1000);
        assert_eq!(r, 0..10 + OVERSCAN_ROWS);
    }

    #[test]
    fn visible_rows_mid_list() {
        let r = visible_rows(100.0 * ROW_STRIDE, 10.0 * ROW_STRIDE, 1000);
        assert_eq!(r, 100 - OVERSCAN_ROWS..110 + OVERSCAN_ROWS);
    }

    #[test]
    fn visible_rows_clamps_stale_offset_after_list_shrinks() {
        // Offset left over from a long list, now filtered to 5 rows.
        let r = visible_rows(50_000.0, 10.0 * ROW_STRIDE, 5);
        assert_eq!(r, 0..5);
    }

    #[test]
    fn visible_rows_empty() {
        assert_eq!(visible_rows(0.0, 500.0, 0), 0..0);
    }
}
