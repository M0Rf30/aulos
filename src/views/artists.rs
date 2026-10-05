// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Artists view - list of artists with album sub-views.

use crate::fl;
use crate::library::{Album, Artist, CoverArt, Track};
use crate::views::common;
use crate::views::{card_button_class, list_row_button_class};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;

/// Messages from the artist view.
#[derive(Debug, Clone)]
pub enum ArtistMessage {
    SelectArtist(usize),
    PlayArtistAlbum(usize, usize),
    PlayTrack(usize, usize, usize),
    BackToList,
    /// Toggle favorite for a track (track ID as string).
    ToggleFavorite(String),
    /// Set rating for a track (track ID, rating 0-5).
    SetRating(String, u8),
    /// Filter by genre.
    FilterByGenre(String),
    /// Toggle between grid and list layout.
    ToggleViewMode,
    /// Insert this album's tracks right after the currently playing one.
    PlayNext(Vec<Track>),
    /// Append this album's tracks to the end of the queue.
    AddToQueue(Vec<Track>),
    /// Expand/collapse the clipped biography preview in the detail view.
    ToggleBioExpanded,
}

/// Card artwork/label width for the grid layout — the avatar frame and
/// clipped labels all share this so a card's art and caption line up.
const CARD_WIDTH: f32 = 160.0;

/// Fixed height for the two-line label block under each grid card, so
/// every card in a row stays the same height regardless of text length.
const CARD_LABEL_HEIGHT: f32 = 40.0;

/// Render the artists view: card grid or list, depending on `mode`.
pub fn artists_view<'a>(
    artists: &'a [Artist],
    artist_photos: &'a std::collections::HashMap<String, widget::image::Handle>,
    mode: crate::config::ViewMode,
) -> cosmic::Element<'a, ArtistMessage> {
    if artists.is_empty() {
        return common::empty_state(
            "system-users-symbolic",
            fl!("no-artists"),
            fl!("artists-empty-hint"),
        );
    }

    use crate::config::ViewMode;

    let header = common::view_mode_toggle_header(mode, ArtistMessage::ToggleViewMode);

    let content: cosmic::Element<'_, ArtistMessage> = match mode {
        ViewMode::List => {
            let mut list = widget::Column::new().spacing(2);

            for (index, artist) in artists.iter().enumerate() {
                let avatar = common::artist_avatar(
                    &artist.name,
                    artist_photos.get(&artist.name),
                    48.0,
                );

                let info = widget::Column::new()
                    .push(common::cell_text(artist.name.as_str()))
                    .push(common::cell_caption(artist_summary(artist)))
                    .spacing(2);

                let row = widget::button::custom(
                    widget::Row::new()
                        .push(avatar)
                        .push(common::clipped_cell(info.into()))
                        .spacing(14)
                        .align_y(Alignment::Center)
                        .padding([10, 8]),
                )
                .on_press(ArtistMessage::SelectArtist(index))
                .width(Length::Fill)
                .class(list_row_button_class(false));

                list = list.push(row);
            }

            widget::scrollable(widget::container(list).padding(16).width(Length::Fill))
                .height(Length::Fill)
                .into()
        }
        ViewMode::Grid => {
            let cards: Vec<cosmic::Element<'_, ArtistMessage>> = artists
                .iter()
                .enumerate()
                .map(|(index, artist)| {
                    let art_widget =
                        common::artist_avatar(&artist.name, artist_photos.get(&artist.name), 128.0);

                    let label_block = common::grid_card_label(
                        CARD_WIDTH,
                        CARD_LABEL_HEIGHT,
                        common::clipped_cell(common::cell_text(artist.name.as_str()).into()),
                        common::clipped_cell(common::cell_caption(artist_summary(artist)).into()),
                    );

                    let artist_card = common::grid_card(art_widget, CARD_WIDTH, label_block);

                    widget::button::custom(artist_card)
                        .on_press(ArtistMessage::SelectArtist(index))
                        .padding(8)
                        .class(card_button_class())
                        .into()
                })
                .collect();

            widget::scrollable(
                widget::container(
                    widget::flex_row(cards)
                        .column_spacing(20)
                        .row_spacing(20)
                        .width(Length::Fill)
                        .justify_content(widget::JustifyContent::Center),
                )
                .padding(16)
                .width(Length::Fill),
            )
            .height(Length::Fill)
            .into()
        }
    };

    widget::Column::new().push(header).push(content).into()
}

/// Render the detail view for a selected artist.
pub fn artist_detail_view<'a>(
    artist: &'a Artist,
    artist_index: usize,
    artist_photos: &'a std::collections::HashMap<String, widget::image::Handle>,
    artist_bio: Option<&'a str>,
    bio_expanded: bool,
    cover_images: &'a std::collections::HashMap<String, widget::icon::Handle>,
    current_track_id: Option<i64>,
) -> cosmic::Element<'a, ArtistMessage> {
    let avatar = common::artist_avatar(&artist.name, artist_photos.get(&artist.name), 120.0);

    let header_info = widget::Column::new()
        .push(widget::text::title1(artist.name.as_str()).wrapping(Wrapping::None))
        .push(common::cell_caption(artist_summary(artist)))
        .spacing(4);

    let header = widget::Row::new()
        .push(widget::tooltip(
            widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                .on_press(ArtistMessage::BackToList),
            widget::text::caption(fl!("back-to-artists")),
            widget::tooltip::Position::Top,
        ))
        .push(avatar)
        .push(common::clipped_cell(header_info.into()))
        .spacing(16)
        .align_y(Alignment::Center);

    let mut content = widget::Column::new().push(header).spacing(16);

    if let Some(bio) = artist_bio.filter(|b| !b.trim().is_empty()) {
        content = content.push(bio_block(bio, bio_expanded));
    }
    for (album_idx, album) in artist.albums.iter().enumerate() {
        let key = CoverArt::album_key(&artist.name, &album.name);
        let album_art: cosmic::Element<'_, ArtistMessage> =
            if let Some(handle) = cover_images.get(&key) {
                widget::icon::icon(handle.clone()).size(64).into()
            } else {
                widget::icon::from_name("media-optical-cd-audio-symbolic")
                    .size(48)
                    .into()
            };

        let album_info = widget::Column::new()
            .push(widget::text::title4(album.name.as_str()).wrapping(Wrapping::None))
            .push(common::cell_caption(album_track_summary(album)));

        let album_header = widget::Row::new()
            .push(
                widget::container(album_art)
                    .width(64)
                    .height(64)
                    .align_x(Horizontal::Center)
                    .align_y(Vertical::Center),
            )
            .push(common::clipped_cell(album_info.into()))
            .push(
                widget::button::suggested(fl!("play"))
                    .on_press(ArtistMessage::PlayArtistAlbum(artist_index, album_idx)),
            )
            .push(
                widget::tooltip(
                    widget::button::icon(widget::icon::from_name("go-next-symbolic").size(16))
                        .on_press(ArtistMessage::PlayNext(album.tracks.clone())),
                    widget::text::caption(fl!("queue-play-next")),
                    widget::tooltip::Position::Top,
                ),
            )
            .push(
                widget::tooltip(
                    widget::button::icon(widget::icon::from_name("insert-object-symbolic").size(16))
                        .on_press(ArtistMessage::AddToQueue(album.tracks.clone())),
                    widget::text::caption(fl!("queue-add")),
                    widget::tooltip::Position::Top,
                ),
            )
            .spacing(12)
            .align_y(Alignment::Center);

        content = content.push(album_header);

        let mut track_list = widget::Column::new().spacing(1);
        for (track_idx, track) in album.tracks.iter().enumerate() {
            let track_id = track.id.to_string();
            let is_playing = current_track_id == Some(track.id);

            let num_col: cosmic::Element<'_, ArtistMessage> = if is_playing {
                widget::icon::from_name("media-playback-start-symbolic")
                    .size(14)
                    .into()
            } else {
                common::cell_text(track.track_number.to_string()).into()
            };

            let heart_btn = common::favorite_button(
                track.is_favorite,
                ArtistMessage::ToggleFavorite(track_id.clone()),
            );

            let rating_row = widget::container(common::star_rating(track.rating, {
                let track_id = track_id.clone();
                move |r| ArtistMessage::SetRating(track_id.clone(), r)
            }))
            .width(112);

            // Audio quality badge, fixed-width so columns stay aligned.
            let quality_row = widget::container(common::quality_badge(
                crate::library::quality::classify(&track.path, track.sample_rate, track.bitrate),
            ))
            .width(common::QUALITY_BADGE_WIDTH);

            let genre_widget: cosmic::Element<'_, ArtistMessage> = if !track.genre.is_empty() {
                widget::button::custom(common::cell_caption(track.genre.as_str()))
                    .on_press(ArtistMessage::FilterByGenre(track.genre.clone()))
                    .class(cosmic::theme::Button::Standard)
                    .into()
            } else {
                widget::Space::new().width(Length::Shrink).into()
            };
            let genre_col = widget::container(common::clipped_cell(genre_widget)).width(130);

            let title_col =
                widget::container(common::clipped_cell(if track.artist != artist.name {
                    widget::Column::new()
                        .push(common::cell_text(track.title.as_str()))
                        .push(common::cell_caption(track.artist.as_str()))
                        .spacing(1)
                        .into()
                } else {
                    common::cell_text(track.title.as_str()).into()
                }))
                .width(Length::FillPortion(4));

            let row = widget::button::custom(
                widget::Row::new()
                    .push(
                        widget::container(num_col)
                            .width(40)
                            .align_x(Horizontal::Center),
                    )
                    .push(title_col)
                    .push(heart_btn)
                    .push(rating_row)
                    .push(quality_row)
                    .push(genre_col)
                    .push(common::duration_cell(track.duration.as_secs()))
                    .spacing(8)
                    .width(Length::Fill)
                    .align_y(Alignment::Center)
                    .padding(4),
            )
            .on_press(ArtistMessage::PlayTrack(artist_index, album_idx, track_idx))
            .width(Length::Fill)
            .class(list_row_button_class(is_playing));

            track_list = track_list.push(row);
        }

        content = content.push(track_list);
        content = content.push(widget::divider::horizontal::default());
    }

    widget::scrollable(widget::container(content).padding(16).width(Length::Fill))
        .height(Length::Fill)
        .into()
}

/// Localized "N albums, M tracks" summary line for an artist.
fn artist_summary(artist: &Artist) -> String {
    let albums = artist.album_count();
    let tracks = artist.track_count();
    let album_str = if albums == 1 {
        fl!("artist-album-count-one", count = albums.to_string())
    } else {
        fl!("artist-album-count-other", count = albums.to_string())
    };
    let track_str = if tracks == 1 {
        fl!("artist-track-count-one", count = tracks.to_string())
    } else {
        fl!("artist-track-count-other", count = tracks.to_string())
    };
    format!("{album_str}, {track_str}")
}

/// Localized "{year} · N tracks" (or just "N tracks") summary for an album.
fn album_track_summary(album: &Album) -> String {
    let n = album.track_count();
    let track_str = if n == 1 {
        fl!("artist-track-count-one", count = n.to_string())
    } else {
        fl!("artist-track-count-other", count = n.to_string())
    };
    if album.year > 0 {
        format!("{} · {}", album.year, track_str)
    } else {
        track_str
    }
}

/// Max characters shown before the biography is clipped with a
/// "show more" link — long enough for a couple of sentences, short
/// enough that the detail view's header stays above the fold.
const BIO_PREVIEW_CHARS: usize = 320;

/// Clipped-biography card: the full text once `expanded`, otherwise a
/// character-capped preview with a trailing ellipsis, plus a
/// show-more/show-less link when the text is actually long enough to
/// need clipping.
fn bio_block<'a>(bio: &'a str, expanded: bool) -> cosmic::Element<'a, ArtistMessage> {
    let char_count = bio.chars().count();
    let is_long = char_count > BIO_PREVIEW_CHARS;

    let shown = if expanded || !is_long {
        bio.to_string()
    } else {
        let mut preview: String = bio.chars().take(BIO_PREVIEW_CHARS).collect();
        preview.push('…');
        preview
    };

    let mut col = widget::Column::new()
        .push(widget::text::body(shown))
        .spacing(8);

    if is_long {
        let label = if expanded {
            fl!("artist-bio-less")
        } else {
            fl!("artist-bio-more")
        };
        col = col.push(
            widget::button::text(label)
                .class(cosmic::theme::Button::Link)
                .on_press(ArtistMessage::ToggleBioExpanded),
        );
    }

    widget::container(col)
        .padding(12)
        .width(Length::Fill)
        .class(cosmic::theme::Container::Card)
        .into()
}
