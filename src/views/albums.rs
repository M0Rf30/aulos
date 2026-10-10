// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Albums grid view - displays album covers in a responsive grid (Lollypop-style).

use crate::config::ViewMode;
use crate::fl;
use crate::library::{Album, CoverArt, Playlist, Track};
use crate::views::common;
use crate::views::track_row;
use crate::views::{card_button_class, list_row_button_class};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;

/// Messages from the album view.
#[derive(Debug, Clone)]
pub enum AlbumMessage {
    /// User clicked on an album (index in the albums list).
    SelectAlbum(usize),
    /// User wants to play the whole album.
    PlayAlbum(usize),
    /// User clicked a specific track within the album detail.
    PlayTrack(usize, usize),
    /// Go back to the grid from detail view.
    BackToGrid,
    /// Toggle favorite for a track (track ID as string).
    ToggleFavorite(String),
    /// Set rating for a track (track ID, rating 0-5).
    SetRating(String, u8),
    /// Filter by genre.
    FilterByGenre(String),
    /// Add track to playlist (source_uri, playlist_id).
    AddToPlaylist(String, String),
    /// Toggle between grid and list layout.
    ToggleViewMode,
    /// Insert these tracks right after the currently playing one. Carries
    /// the already-resolved tracks (whole album, or a single row) so
    /// `view.rs` needs no extra index lookup.
    PlayNext(Vec<Track>),
    /// Append these tracks to the end of the queue.
    AddToQueue(Vec<Track>),
    /// Jump to another view (artist page, genre…).
    Navigate(crate::views::Route),
}

/// Minimum cover/label width of a grid card; the fluid grid stretches
/// cards from this up to [`CARD_MAX_WIDTH`] so rows fill the view.
const CARD_WIDTH: f32 = 160.0;
const CARD_MAX_WIDTH: f32 = 220.0;

/// Padding of the card button around its cover and labels.
const CARD_PADDING: f32 = 8.0;

/// Fixed height for the two-line label block under each card: body
/// line-height (21px) + caption line-height (17px) + 2px inter-line spacing.
/// A constant height (rather than sizing to content) is what keeps every
/// card in a row the same height, whether or not the artist line has text.
const CARD_LABEL_HEIGHT: f32 = 40.0;

/// Fixed width for genre chip buttons so a row of chips lines up neatly.
const GENRE_CHIP_WIDTH: f32 = 130.0;

/// Caption-styled, single-line text dimmed to the theme's secondary
/// (neutral_7) color — used for artist subtitles under a bolder title so the
/// two lines read with clear hierarchy instead of matching weight and color.
fn secondary_caption<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Custom(|theme| {
        cosmic::iced::widget::text::Style {
            color: Some(theme.cosmic().palette.neutral_7.into()),
            ..Default::default()
        }
    }))
}

/// Render the albums view: card grid or list, depending on `mode`.
pub fn albums_view<'a>(
    albums: &'a [Album],
    cover_images: &'a std::collections::HashMap<String, widget::icon::Handle>,
    mode: ViewMode,
) -> cosmic::Element<'a, AlbumMessage> {
    if albums.is_empty() {
        return common::empty_state(
            "folder-music-symbolic",
            fl!("no-albums"),
            fl!("albums-empty-hint"),
        );
    }

    let header = common::view_mode_toggle_header(mode, AlbumMessage::ToggleViewMode);

    let content: cosmic::Element<'a, AlbumMessage> = match mode {
        ViewMode::Grid => common::fluid_card_grid(
            albums.len(),
            CARD_WIDTH + 2.0 * CARD_PADDING,
            CARD_MAX_WIDTH + 2.0 * CARD_PADDING,
            move |index, outer| {
                let album = &albums[index];
                let art_size = outer - 2.0 * CARD_PADDING;
                let key = CoverArt::album_key(&album.artist, &album.name);
                let art_widget = common::grid_art_tile(
                    cover_images.get(&key),
                    art_size as u16,
                    "media-optical-symbolic",
                );

                // A missing or title-echoing artist still reserves the caption
                // line's height (a non-breaking space) so every card's label
                // block is exactly two lines tall and rows stay aligned.
                let has_distinct_artist = !album.artist.trim().is_empty()
                    && !album.artist.trim().eq_ignore_ascii_case(album.name.trim());
                let artist_display = if has_distinct_artist {
                    album.artist.as_str()
                } else {
                    "\u{a0}"
                };

                let label_block = common::grid_card_label(
                    art_size,
                    CARD_LABEL_HEIGHT,
                    common::clipped_cell(if album.name.trim().is_empty() {
                        secondary_caption(fl!("unknown-album")).into()
                    } else {
                        common::cell_text(album.name.as_str())
                            .font(cosmic::font::semibold())
                            .into()
                    }),
                    widget::Row::new()
                        .push(common::clipped_cell(if has_distinct_artist {
                            common::link(
                                secondary_caption(artist_display),
                                true,
                                AlbumMessage::Navigate(crate::views::Route::Artist(
                                    album.artist.clone(),
                                )),
                            )
                        } else {
                            secondary_caption(artist_display).into()
                        }))
                        .push(common::quality_badge(
                            crate::library::quality::album_quality(&album.tracks),
                        ))
                        .spacing(4)
                        .align_y(Alignment::Center)
                        .into(),
                );

                let album_card = common::grid_card(art_widget, art_size, label_block);

                let tooltip_label = if has_distinct_artist {
                    fl!(
                        "album-tooltip",
                        title = album.name.clone(),
                        artist = album.artist.clone()
                    )
                } else {
                    album.name.clone()
                };

                widget::tooltip(
                    widget::button::custom(album_card)
                        .on_press(AlbumMessage::SelectAlbum(index))
                        .padding(CARD_PADDING as u16)
                        .class(card_button_class()),
                    widget::text::caption(tooltip_label),
                    widget::tooltip::Position::Top,
                )
                .into()
            },
        ),
        ViewMode::List => {
            let mut list = widget::Column::new().spacing(2);

            for (index, album) in albums.iter().enumerate() {
                let key = CoverArt::album_key(&album.artist, &album.name);
                let art_widget: cosmic::Element<'_, AlbumMessage> =
                    common::list_art_icon(cover_images.get(&key), 52, "media-optical-symbolic");

                let has_distinct_artist = !album.artist.trim().is_empty()
                    && !album.artist.trim().eq_ignore_ascii_case(album.name.trim());

                let title: cosmic::Element<'_, AlbumMessage> = if album.name.trim().is_empty() {
                    secondary_caption(fl!("unknown-album")).into()
                } else {
                    common::cell_text(album.name.as_str())
                        .font(cosmic::font::semibold())
                        .into()
                };
                let subtitle: cosmic::Element<'_, AlbumMessage> = if has_distinct_artist {
                    common::link(
                        secondary_caption(album.artist.as_str()),
                        true,
                        AlbumMessage::Navigate(crate::views::Route::Artist(album.artist.clone())),
                    )
                } else {
                    secondary_caption("\u{a0}").into()
                };
                let info = widget::Column::new()
                    .push(common::clipped_cell(title))
                    .push(common::clipped_cell(subtitle))
                    .spacing(2);

                let year: cosmic::Element<'_, AlbumMessage> = if album.year > 0 {
                    secondary_caption(album.year.to_string()).into()
                } else {
                    widget::Space::new().into()
                };
                let track_count = album.track_count();
                let tracks_label = format!(
                    "{track_count} track{}",
                    if track_count == 1 { "" } else { "s" }
                );

                let play_button = widget::tooltip(
                    widget::button::icon(
                        widget::icon::from_name("media-playback-start-symbolic").size(16),
                    )
                    .on_press(AlbumMessage::PlayAlbum(index)),
                    widget::text::caption(fl!("play-album")),
                    widget::tooltip::Position::Top,
                );

                let row = widget::button::custom(
                    widget::Row::new()
                        .push(art_widget)
                        .push(
                            widget::container(common::clipped_cell(info.into()))
                                .width(Length::FillPortion(5)),
                        )
                        .push(
                            widget::container(common::quality_badge(
                                crate::library::quality::album_quality(&album.tracks),
                            ))
                            .width(LIST_BADGE_WIDTH)
                            .align_x(Horizontal::Center),
                        )
                        .push(
                            widget::container(year)
                                .width(LIST_YEAR_WIDTH)
                                .align_x(Horizontal::Right),
                        )
                        .push(
                            widget::container(secondary_caption(tracks_label))
                                .width(LIST_TRACKS_WIDTH)
                                .align_x(Horizontal::Right),
                        )
                        .push(
                            widget::container(secondary_caption(common::format_duration_coarse(
                                album.total_duration().as_secs(),
                            )))
                            .width(LIST_DURATION_WIDTH)
                            .align_x(Horizontal::Right),
                        )
                        .push(play_button)
                        .spacing(16)
                        .height(Length::Fill)
                        .align_y(Alignment::Center)
                        .padding([0, 12]),
                )
                .on_press(AlbumMessage::SelectAlbum(index))
                .width(Length::Fill)
                .height(Length::Fixed(LIST_ROW_HEIGHT))
                .padding(0)
                .class(list_row_button_class(false));

                list = list.push(row);
            }

            let spacing = cosmic::theme::active().cosmic().spacing;
            widget::scrollable(
                widget::container(list)
                    .padding([
                        spacing.space_s,
                        spacing.space_m + 16,
                        spacing.space_m,
                        spacing.space_m,
                    ])
                    .width(Length::Fill),
            )
            .height(Length::Fill)
            .into()
        }
    };

    widget::Column::new().push(header).push(content).into()
}

/// List-mode row geometry: fixed height (uniform rhythm) and right-hand
/// metadata column widths, so every row's columns line up.
const LIST_ROW_HEIGHT: f32 = 68.0;
const LIST_BADGE_WIDTH: f32 = 72.0;
const LIST_YEAR_WIDTH: f32 = 48.0;
const LIST_TRACKS_WIDTH: f32 = 80.0;
const LIST_DURATION_WIDTH: f32 = 64.0;

pub fn album_detail_view<'a>(
    album: &'a Album,
    album_index: usize,
    cover_images: &'a std::collections::HashMap<String, widget::icon::Handle>,
    playlists: &'a [Playlist],
    current_track_id: Option<i64>,
    hero: Option<(
        Option<&'a widget::icon::Handle>,
        Option<&'a crate::library::palette::Accent>,
    )>,
) -> cosmic::Element<'a, AlbumMessage> {
    let key = CoverArt::album_key(&album.artist, &album.name);
    let art_widget: cosmic::Element<'_, AlbumMessage> = if let Some(handle) = cover_images.get(&key)
    {
        let radius = cosmic::theme::active().cosmic().corner_radii.radius_m[0];
        common::cover_art(handle, 160.0, radius, true)
    } else {
        widget::icon::from_name("media-optical-symbolic")
            .size(120)
            .into()
    };

    // Task 103: Collect distinct genres from album tracks for header chips
    let genres: Vec<String> = {
        // Tags often pack several genres into one field ("Trance;
        // Electronic"): split them so each chip is a single genre.
        let mut g: Vec<String> = album
            .tracks
            .iter()
            .flat_map(|t| t.genre.split([';', ',', '/']))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        g.sort();
        g.dedup();
        g
    };

    let track_count = album.track_count();
    let track_label = format!(
        "{track_count} track{}",
        if track_count == 1 { "" } else { "s" }
    );
    let duration_label = common::format_duration_coarse(album.total_duration().as_secs());
    let spacing = cosmic::theme::active().cosmic().spacing;

    let title_line = widget::container(common::clipped_cell(
        widget::text::title2(album.name.as_str())
            .wrapping(Wrapping::None)
            .into(),
    ))
    .width(Length::Fill);

    let artist_line = widget::container(common::clipped_cell(common::link_cell(
        album.artist.as_str(),
        true,
        || AlbumMessage::Navigate(crate::views::Route::Artist(album.artist.clone())),
    )))
    .width(Length::Fill);

    let mut meta_col = widget::Column::new()
        .push(title_line)
        .push(artist_line)
        .push(common::cell_caption(format!(
            "{track_label} \u{b7} {duration_label}"
        )))
        .push(
            widget::button::suggested(fl!("play-album"))
                .on_press(AlbumMessage::PlayAlbum(album_index))
                .class(
                    hero.and_then(|h| h.1)
                        .map(common::accent_button_class)
                        .unwrap_or(cosmic::theme::Button::Suggested),
                ),
        )
        .push(
            widget::Row::new()
                .push(
                    widget::button::standard(fl!("queue-play-next"))
                        .on_press(AlbumMessage::PlayNext(album.tracks.clone())),
                )
                .push(
                    widget::button::standard(fl!("queue-add"))
                        .on_press(AlbumMessage::AddToQueue(album.tracks.clone())),
                )
                .spacing(8),
        )
        .width(Length::Fill)
        .spacing(8);

    // Task 103: Genre chips in album header
    if !genres.is_empty() {
        let mut genre_row = widget::Row::new()
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center);
        for genre in genres {
            // Use the owned String for both the message and the label.
            let label = genre.clone();
            genre_row = genre_row.push(
                widget::button::custom(common::clipped_cell(common::cell_caption(label).into()))
                    .on_press(AlbumMessage::Navigate(crate::views::Route::Genre(genre)))
                    .class(cosmic::theme::Button::Standard)
                    .width(GENRE_CHIP_WIDTH),
            );
        }
        meta_col = meta_col.push(genre_row);
    }
    let header = widget::Row::new()
        .push(
            widget::container(art_widget)
                .width(160)
                .height(160)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center),
        )
        .push(meta_col)
        .spacing(24)
        .align_y(Alignment::Center);

    // The artist column only earns its space on compilations / split
    // albums; on a single-artist album it just repeats the hero.
    let artist_differs = album.tracks.iter().any(|t| t.artist != album.artist);
    let columns = track_row::Columns {
        artist: artist_differs,
        favorite: true,
        rating: true,
        quality: true,
        queue_actions: true,
        playlist: !playlists.is_empty(),
        ..Default::default()
    };
    let track_list = track_row::width_aware(album.tracks.len(), false, move |width| {
        let columns = columns.responsive(width);
        track_row::rows_column(
            None,
            album.tracks.iter().enumerate().map(|(track_idx, track)| {
                let track_id = track.id.to_string();
                let rating_track_id = track_id.clone();
                let number = if track.track_number > 0 {
                    track.track_number.to_string()
                } else {
                    (track_idx + 1).to_string()
                };
                track_row::TrackRow::new(
                    track,
                    number,
                    current_track_id == Some(track.id),
                    columns,
                    AlbumMessage::PlayTrack(album_index, track_idx),
                )
                .with_navigate(AlbumMessage::Navigate)
                .with_favorite(AlbumMessage::ToggleFavorite(track_id))
                .with_rating(move |r| AlbumMessage::SetRating(rating_track_id.clone(), r))
                .with_queue_actions(
                    AlbumMessage::PlayNext(vec![track.clone()]),
                    AlbumMessage::AddToQueue(vec![track.clone()]),
                )
                .with_add_to_playlist(playlists, AlbumMessage::AddToPlaylist)
                .with_artist_subtitle(artist_differs && !columns.artist)
                .view()
            }),
        )
    });

    widget::scrollable(
        widget::Column::new()
            .push(common::hero_header(
                hero.and_then(|h| h.0),
                hero.and_then(|h| h.1),
                Some(common::hero_back_button(
                    fl!("back-to-albums"),
                    AlbumMessage::BackToGrid,
                )),
                header.into(),
            ))
            .push(track_list)
            .spacing(16)
            .padding(16),
    )
    .height(Length::Fill)
    .into()
}
