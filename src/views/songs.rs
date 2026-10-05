// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use crate::fl;
use crate::library::{Playlist, Track};
use crate::views::{common, list_row_button_class};
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::alignment::Horizontal;
use cosmic::iced::{Alignment, Length, Size};
use cosmic::widget;

/// Fixed width of the leading track-number / now-playing indicator column.
const NUM_WIDTH: f32 = 40.0;
/// Fixed width of the genre chip column.
const GENRE_WIDTH: f32 = 130.0;
/// Fixed width of the favorite-heart column.
const HEART_WIDTH: f32 = 32.0;
/// Fixed width of the star-rating column.
const RATING_WIDTH: f32 = 112.0;
/// Fixed width of the add-to-playlist column.
const ADD_WIDTH: f32 = 32.0;
/// Fixed width of each per-row queue-action icon button (play next / add to
/// queue) — shown alongside rating/quality at the 900px breakpoint, where
/// there's ample remaining fill (see the column-width table above).
const QUEUE_ACTION_WIDTH: f32 = 32.0;

/// Minimum responsive width (px) at which the Artist column appears.
const ARTIST_BREAKPOINT: f32 = 640.0;
/// Minimum responsive width (px) at which the Album and Rating columns appear.
const ALBUM_RATING_BREAKPOINT: f32 = 900.0;
/// Minimum responsive width (px) at which the Genre column appears.
const GENRE_BREAKPOINT: f32 = 1100.0;

/// Spacing between adjacent columns, identical in the header and every row.
const COLUMN_SPACING: f32 = 8.0;
/// Leading (left) edge padding for the header and every row.
const ROW_PADDING_LEFT: f32 = 8.0;

// --- Row layout model & column-width arithmetic ----------------------------
//
// `build_header`/`build_row` each build one `widget::Row` with
// `width(Length::Fill)`, `spacing(COLUMN_SPACING)` and left/right padding.
// The `.width(Length::Fill)` on the row itself is load-bearing and must
// never be removed: iced's flex layout (`iced::core::layout::flex::resolve`)
// only distributes `FillPortion` children proportionally in its third pass
// when the row's own main-axis length is *not* `Shrink`; a bare
// `Row::new()` defaults to `Length::Shrink`, which silently turns every
// `FillPortion` title/artist/album cell into "shrink to intrinsic content,
// capped at whatever space is left" instead of "take a fair share of the
// row". That is what let a long title/album steal width meant for the
// columns after it, squeezing rating/add/duration and (without a clipping
// container) letting text paint straight through the neighboring cell — the
// screenshotted overflow bug. `Length::Fill` restores proportional
// distribution; `common::clipped_cell` (via `fill_column` below) is the
// second, independent safety net so even a fair share that's still narrower
// than the text can never bleed past its own cell.
//
// With that fixed, iced's flex algorithm gives every *fixed*-width column
// (num/genre/heart/rating/add/duration) its full declared width before
// splitting whatever remains across the `FillPortion(4/3/3)` title/artist/
// album cells, and clamps that remainder at 0 rather than letting it go
// negative — so the only way rating/add/duration could ever be pushed past
// the right edge is `fixed columns + spacing + padding` exceeding the width
// available inside the row. Checking that at each breakpoint's lower bound
// is sufficient: within a regime the fixed cost is constant while width can
// only grow towards the next breakpoint, so the lower bound is always the
// worst case (except regime "< 640", whose worst case is width -> 0, far
// below any usable window). The right-edge padding includes a `space_s`
// gutter (8/16/24px depending on interface density) reserved for the
// vertical scrollbar, which `iced::widget::scrollable` overlays on top of
// content instead of reserving layout space for (see `Scrollbar::layout`,
// which positions it at `bounds.x + bounds.width - scrollbar_width`) —
// without the gutter the bar would sit on top of the duration column.
//
// | width | columns shown                       | fixed+spacing+padding* | remaining fill |
// |------:|--------------------------------------|------------------------:|---------------:|
// |   640 | num,title,artist,heart,add,duration  |   168 + 40 + 32 = 240    | 400 (~57px/portion of 7) |
// |   900 | + album, rating                      |   280 + 56 + 32 = 368    | 532 (~53px/portion of 10) |
// |  1100 | + genre                              |   410 + 64 + 32 = 506    | 594 (~59px/portion of 10) |
// |  1920 | (same columns as 1100)               |   410 + 64 + 32 = 506    | 1414 (~141px/portion of 10) |
//
// (* padding = ROW_PADDING_LEFT + ROW_PADDING_RIGHT = 8 + (8 + space_s) = 32
// at the default "Standard" density's space_s=16; "remaining fill" only
// shrinks by at most 8px more at "Spacious" density's space_s=24 (vs. the
// Standard-density figures tabulated above), e.g. 400 -> 392 at 640px —
// still comfortably positive.) Every row above stays
// far from zero, so rating/add/duration are never squeezed or pushed past
// the right edge at any audited width: the current widths and breakpoints
// need no adjustment, only the row/clip fixes described above.
fn row_padding_right() -> f32 {
    ROW_PADDING_LEFT + f32::from(cosmic::theme::active().cosmic().spacing.space_s)
}

/// Column cell that claims a proportional share of the row's width via an
/// outer `FillPortion` container, then clips its content with
/// [`common::clipped_cell`] so long text can never bleed into the next
/// column. `clipped_cell` fixes its own container to `Length::Fill`, so the
/// portion ratio has to be established one level up, around it.
fn fill_column<'a>(
    portion: u16,
    content: impl Into<cosmic::Element<'a, SongMessage>>,
) -> cosmic::Element<'a, SongMessage> {
    widget::container(common::clipped_cell(content.into()))
        .width(Length::FillPortion(portion))
        .into()
}

/// Alpha applied to secondary cell text (artist/album) so the title reads as
/// the primary label and artist/album recede into a supporting role.
/// `Component::on`/accent colors are opaque, so `with_alpha` (which sets the
/// channel directly rather than multiplying it) is all that's needed.
const SECONDARY_TEXT_ALPHA: f32 = 0.7;

/// Secondary cell text color for a row that isn't playing: the normal
/// on-surface color at reduced alpha.
fn secondary_text_style(theme: &cosmic::Theme) -> cosmic::iced::widget::text::Style {
    let cosmic = theme.cosmic();
    cosmic::iced::widget::text::Style {
        color: Some(
            cosmic
                .background(false)
                .component
                .on
                .with_alpha(SECONDARY_TEXT_ALPHA)
                .into(),
        ),
        ..Default::default()
    }
}

/// Secondary cell text color for the currently-playing row: the accent
/// color at the same reduced alpha, so artist/album stay tonally in step
/// with the accent-tinted title instead of reading as disconnected from the
/// highlight.
fn secondary_text_style_playing(theme: &cosmic::Theme) -> cosmic::iced::widget::text::Style {
    let cosmic = theme.cosmic();
    cosmic::iced::widget::text::Style {
        color: Some(
            cosmic
                .accent_color()
                .with_alpha(SECONDARY_TEXT_ALPHA)
                .into(),
        ),
        ..Default::default()
    }
}

/// Secondary (dimmer) single-line cell text for the artist/album columns:
/// same `body` size/weight as the title, but visually recedes behind it.
fn secondary_cell_text<'a>(content: &'a str, is_playing: bool) -> common::Text<'a> {
    common::cell_text(content).class(cosmic::theme::Text::Custom(if is_playing {
        secondary_text_style_playing
    } else {
        secondary_text_style
    }))
}

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Title,
    Artist,
    Album,
    Duration,
}

pub fn songs_list_view<'a>(
    tracks: &'a [Track],
    current_sort: SortField,
    sort_descending: bool,
    favorites_filter: bool,
    genre_filter: Option<&'a str>,
    playlists: &'a [Playlist],
    current_track_id: Option<i64>,
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

    // Filter bar: Favorites toggle + genre filter indicator + track count.
    let mut filter_bar = widget::Row::new().spacing(8).align_y(Alignment::Center);

    let fav_icon = if favorites_filter {
        "emblem-favorite-symbolic"
    } else {
        "non-starred-symbolic"
    };
    let fav_button = widget::button::custom(
        widget::Row::new()
            .push(widget::icon::from_name(fav_icon).size(16))
            .push(widget::text::body(fl!("songs-favorites-filter")))
            .spacing(4)
            .align_y(Alignment::Center),
    )
    .on_press(SongMessage::ToggleFavoritesFilter)
    .class(if favorites_filter {
        cosmic::theme::Button::Suggested
    } else {
        cosmic::theme::Button::Standard
    });
    filter_bar = filter_bar.push(fav_button);

    if let Some(genre) = genre_filter {
        let genre_chip = widget::button::custom(
            widget::Row::new()
                .push(widget::text::caption(genre))
                .push(widget::icon::from_name("window-close-symbolic").size(12))
                .spacing(4)
                .align_y(Alignment::Center),
        )
        .on_press(SongMessage::ClearGenreFilter)
        .class(cosmic::theme::Button::Suggested);
        filter_bar = filter_bar.push(genre_chip);
    }

    filter_bar = filter_bar.push(
        common::cell_caption(format!(
            "{} track{}",
            filtered.len(),
            if filtered.len() == 1 { "" } else { "s" }
        ))
        .width(Length::Fill),
    );

    let table_area: cosmic::Element<'a, SongMessage> = if filtered.is_empty() {
        common::empty_state(
            "edit-find-symbolic",
            fl!("songs-no-matches"),
            fl!("songs-no-matches-hint"),
        )
    } else {
        widget::responsive(move |size: Size| {
            let show_artist = size.width >= ARTIST_BREAKPOINT;
            let show_album = size.width >= ALBUM_RATING_BREAKPOINT;
            let show_rating = size.width >= ALBUM_RATING_BREAKPOINT;
            let show_genre = size.width >= GENRE_BREAKPOINT;
            let row_padding_right = row_padding_right();

            let header = build_header(
                current_sort,
                sort_descending,
                show_artist,
                show_album,
                show_rating,
                show_genre,
                row_padding_right,
            );

            let mut track_list = widget::Column::new().spacing(2);
            for &(original_index, track) in &filtered {
                let track_id = track.id.to_string();
                let is_playing = current_track_id == Some(track.id);
                track_list = track_list.push(build_row(
                    original_index,
                    track,
                    track_id,
                    is_playing,
                    playlists,
                    show_artist,
                    show_album,
                    show_rating,
                    show_genre,
                    row_padding_right,
                ));
            }

            widget::Column::new()
                .push(header)
                .push(widget::divider::horizontal::default())
                .push(
                    widget::scrollable(widget::container(track_list).width(Length::Fill))
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
        .spacing(4)
        .into()
}

/// Build the column header row. Uses the exact same fixed widths /
/// `FillPortion`s as [`build_row`] so labels line up with their values.
fn build_header<'a>(
    current_sort: SortField,
    sort_descending: bool,
    show_artist: bool,
    show_album: bool,
    show_rating: bool,
    show_genre: bool,
    row_padding_right: f32,
) -> cosmic::Element<'a, SongMessage> {
    let mut row = widget::Row::new()
        // Load-bearing: see the row layout note above `row_padding_right`.
        .width(Length::Fill)
        .spacing(COLUMN_SPACING)
        .align_y(Alignment::Center)
        .padding([4.0, row_padding_right, 4.0, ROW_PADDING_LEFT]);

    row = row.push(
        widget::container(common::cell_text(fl!("songs-column-number")))
            .width(NUM_WIDTH)
            .align_x(Horizontal::Center)
            // A single "#" can never realistically overflow 40px, but every
            // no-wrap cell gets a clip so none can, on principle.
            .clip(true),
    );

    row = row.push(fill_column(
        4,
        widget::button::custom(common::cell_text(sort_label(
            &fl!("songs-column-title"),
            SortField::Title,
            current_sort,
            sort_descending,
        )))
        .on_press(SongMessage::SortBy(SortField::Title))
        .width(Length::Fill)
        .class(list_row_button_class(false)),
    ));

    if show_artist {
        row = row.push(fill_column(
            3,
            widget::button::custom(common::cell_text(sort_label(
                &fl!("songs-column-artist"),
                SortField::Artist,
                current_sort,
                sort_descending,
            )))
            .on_press(SongMessage::SortBy(SortField::Artist))
            .width(Length::Fill)
            .class(list_row_button_class(false)),
        ));
    }

    if show_album {
        row = row.push(fill_column(
            3,
            widget::button::custom(common::cell_text(sort_label(
                &fl!("songs-column-album"),
                SortField::Album,
                current_sort,
                sort_descending,
            )))
            .on_press(SongMessage::SortBy(SortField::Album))
            .width(Length::Fill)
            .class(list_row_button_class(false)),
        ));
    }

    if show_genre {
        row = row.push(widget::Space::new().width(GENRE_WIDTH));
    }

    row = row.push(widget::Space::new().width(HEART_WIDTH));

    if show_rating {
        row = row.push(widget::Space::new().width(RATING_WIDTH));
    }

    if show_rating {
        row = row.push(widget::Space::new().width(common::QUALITY_BADGE_WIDTH));
    }

    if show_rating {
        row = row.push(widget::Space::new().width(QUEUE_ACTION_WIDTH));
        row = row.push(widget::Space::new().width(QUEUE_ACTION_WIDTH));
    }

    row = row.push(widget::Space::new().width(ADD_WIDTH));

    row = row.push(
        widget::container(
            widget::button::custom(common::cell_text(sort_label(
                &fl!("songs-column-duration"),
                SortField::Duration,
                current_sort,
                sort_descending,
            )))
            .on_press(SongMessage::SortBy(SortField::Duration))
            .class(list_row_button_class(false)),
        )
        .width(common::DURATION_WIDTH)
        .align_x(Horizontal::Right)
        // Manual clip (rather than `fill_column`/`clipped_cell`, which
        // always left-aligns) so a long localized "Duration" label plus its
        // sort arrow can never paint past this fixed-width column while
        // staying right-aligned, matching the data rows' duration cells.
        .clip(true),
    );

    row.into()
}

/// Build a single track row. Column widths mirror [`build_header`] exactly.
#[allow(clippy::too_many_arguments)]
fn build_row<'a>(
    original_index: usize,
    track: &'a Track,
    track_id: String,
    is_playing: bool,
    playlists: &'a [Playlist],
    show_artist: bool,
    show_album: bool,
    show_rating: bool,
    show_genre: bool,
    row_padding_right: f32,
) -> cosmic::Element<'a, SongMessage> {
    let num_col: cosmic::Element<'a, SongMessage> = if is_playing {
        widget::icon::from_name("media-playback-start-symbolic")
            .size(14)
            .into()
    } else {
        common::cell_text((original_index + 1).to_string()).into()
    };

    let mut row = widget::Row::new()
        // Load-bearing: see the row layout note above `row_padding_right`.
        .width(Length::Fill)
        .spacing(COLUMN_SPACING)
        .align_y(Alignment::Center);

    row = row.push(
        widget::container(num_col)
            .width(NUM_WIDTH)
            .align_x(Horizontal::Center)
            // See the matching header cell: clipped on principle, even
            // though the play icon and a formatted position number are
            // always well within 40px in practice.
            .clip(true),
    );

    row = row.push(fill_column(4, common::cell_text(track.title.as_str())));

    if show_artist {
        row = row.push(fill_column(
            3,
            secondary_cell_text(track.artist.as_str(), is_playing),
        ));
    }

    if show_album {
        row = row.push(fill_column(
            3,
            secondary_cell_text(track.album.as_str(), is_playing),
        ));
    }

    if show_genre {
        let genre_col: cosmic::Element<'a, SongMessage> = if track.genre.is_empty() {
            widget::Space::new().width(GENRE_WIDTH).into()
        } else {
            widget::container(common::clipped_cell(
                widget::button::custom(common::cell_caption(track.genre.as_str()))
                    .on_press(SongMessage::FilterByGenre(track.genre.clone()))
                    .class(cosmic::theme::Button::Standard)
                    .into(),
            ))
            .width(GENRE_WIDTH)
            .into()
        };
        row = row.push(genre_col);
    }

    row = row.push(
        widget::container(common::favorite_button(
            track.is_favorite,
            SongMessage::ToggleFavorite(track_id.clone()),
        ))
        .width(HEART_WIDTH)
        .align_x(Horizontal::Center),
    );

    if show_rating {
        let rating_track_id = track_id.clone();
        row = row.push(
            widget::container(common::star_rating(track.rating, move |r| {
                SongMessage::SetRating(rating_track_id.clone(), r)
            }))
            .width(RATING_WIDTH)
            .align_x(Horizontal::Center),
        );
    }

    if show_rating {
        row = row.push(
            widget::container(common::quality_badge(crate::library::quality::classify(
                &track.path,
                track.sample_rate,
                track.bitrate,
            )))
            .width(common::QUALITY_BADGE_WIDTH)
            .align_x(Horizontal::Center),
        );
    }

    if show_rating {
        row = row.push(
            widget::container(
                widget::tooltip(
                    widget::button::icon(widget::icon::from_name("go-next-symbolic").size(16))
                        .on_press(SongMessage::PlayNext(original_index)),
                    widget::text::caption(fl!("queue-play-next")),
                    widget::tooltip::Position::Top,
                ),
            )
            .width(QUEUE_ACTION_WIDTH)
            .align_x(Horizontal::Center),
        );
        row = row.push(
            widget::container(
                widget::tooltip(
                    widget::button::icon(widget::icon::from_name("insert-object-symbolic").size(16))
                        .on_press(SongMessage::AddToQueue(original_index)),
                    widget::text::caption(fl!("queue-add")),
                    widget::tooltip::Position::Top,
                ),
            )
            .width(QUEUE_ACTION_WIDTH)
            .align_x(Horizontal::Center),
        );
    }

    row = row.push(
        widget::container(common::add_to_playlist_button(
            track.source_uri.clone(),
            playlists,
            SongMessage::AddToPlaylist,
            ADD_WIDTH,
        ))
        .width(ADD_WIDTH)
        .align_x(Horizontal::Center),
    );

    row = row.push(common::duration_cell(track.duration.as_secs()));

    widget::button::custom(row.padding([6.0, row_padding_right, 6.0, ROW_PADDING_LEFT]))
        .on_press(SongMessage::PlayTrack(original_index))
        .width(Length::Fill)
        // The row already carries its own padding above; zero the button's
        // own default 5px padding so it doesn't silently add to the
        // column-width arithmetic documented above `row_padding_right`.
        .padding(0)
        .class(list_row_button_class(is_playing))
        .into()
}

fn sort_label(name: &str, field: SortField, current: SortField, descending: bool) -> String {
    if field == current {
        let arrow = if descending { "▼" } else { "▲" };
        format!("{name} {arrow}")
    } else {
        name.to_string()
    }
}
