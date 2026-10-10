// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Artists view - list of artists with album sub-views.

use crate::fl;
use crate::library::{Album, Artist, CoverArt, Track};
use crate::views::common;
use crate::views::track_row;
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
    /// Jump to another view (album page, genre…).
    Navigate(crate::views::Route),
}

/// Minimum avatar-frame/label width of a grid card; the fluid grid
/// stretches cards from this up to [`CARD_MAX_WIDTH`] so rows fill the view.
const CARD_WIDTH: f32 = 160.0;
const CARD_MAX_WIDTH: f32 = 220.0;

/// Padding of the card button around its avatar and labels.
const CARD_PADDING: f32 = 8.0;

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
            let dim = |s: String| {
                common::cell_caption(s).class(cosmic::theme::Text::Custom(|theme| {
                    cosmic::iced::widget::text::Style {
                        color: Some(theme.cosmic().palette.neutral_7.into()),
                        ..Default::default()
                    }
                }))
            };

            for (index, artist) in artists.iter().enumerate() {
                let avatar =
                    common::artist_avatar(&artist.name, artist_photos.get(&artist.name), 52.0);

                let albums = artist.album_count();
                let tracks = artist.track_count();
                let duration: u64 = artist
                    .albums
                    .iter()
                    .map(|a| a.total_duration().as_secs())
                    .sum();

                let info = common::cell_text(artist.name.as_str()).font(cosmic::font::semibold());

                let row = widget::button::custom(
                    widget::Row::new()
                        .push(avatar)
                        .push(
                            widget::container(common::clipped_cell(info.into()))
                                .width(Length::FillPortion(5)),
                        )
                        .push(
                            widget::container(dim(format!(
                                "{albums} album{}",
                                if albums == 1 { "" } else { "s" }
                            )))
                            .width(80)
                            .align_x(Horizontal::Right),
                        )
                        .push(
                            widget::container(dim(format!(
                                "{tracks} track{}",
                                if tracks == 1 { "" } else { "s" }
                            )))
                            .width(80)
                            .align_x(Horizontal::Right),
                        )
                        .push(
                            widget::container(dim(common::format_duration_coarse(duration)))
                                .width(64)
                                .align_x(Horizontal::Right),
                        )
                        .push(widget::icon::from_name("go-next-symbolic").size(16))
                        .spacing(16)
                        .height(Length::Fill)
                        .align_y(Alignment::Center)
                        .padding([0, 12]),
                )
                .on_press(ArtistMessage::SelectArtist(index))
                .width(Length::Fill)
                .height(Length::Fixed(68.0))
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
        ViewMode::Grid => common::fluid_card_grid(
            artists.len(),
            CARD_WIDTH + 2.0 * CARD_PADDING,
            CARD_MAX_WIDTH + 2.0 * CARD_PADDING,
            move |index, outer| {
                let artist = &artists[index];
                let art_size = outer - 2.0 * CARD_PADDING;
                let art_widget = common::artist_avatar(
                    &artist.name,
                    artist_photos.get(&artist.name),
                    art_size * 0.8,
                );

                let label_block = common::grid_card_label(
                    art_size,
                    CARD_LABEL_HEIGHT,
                    common::clipped_cell(common::cell_text(artist.name.as_str()).into()),
                    common::clipped_cell(common::cell_caption(artist_summary(artist)).into()),
                );

                let artist_card = common::grid_card(art_widget, art_size, label_block);

                widget::button::custom(artist_card)
                    .on_press(ArtistMessage::SelectArtist(index))
                    .padding(CARD_PADDING as u16)
                    .class(card_button_class())
                    .into()
            },
        ),
    };

    widget::Column::new().push(header).push(content).into()
}

/// Render the detail view for a selected artist.
#[allow(clippy::too_many_arguments)]
pub fn artist_detail_view<'a>(
    artist: &'a Artist,
    artist_index: usize,
    artist_photos: &'a std::collections::HashMap<String, widget::image::Handle>,
    artist_bio: Option<&'a str>,
    bio_expanded: bool,
    cover_images: &'a std::collections::HashMap<String, widget::icon::Handle>,
    current_track_id: Option<i64>,
    hero: Option<(
        Option<&'a widget::icon::Handle>,
        Option<&'a crate::library::palette::Accent>,
    )>,
) -> cosmic::Element<'a, ArtistMessage> {
    let avatar = common::artist_avatar(&artist.name, artist_photos.get(&artist.name), 120.0);

    let header_info = widget::Column::new()
        .push(widget::text::title1(artist.name.as_str()).wrapping(Wrapping::None))
        .push(common::cell_caption(artist_summary(artist)))
        .spacing(4);

    let header = widget::Row::new()
        .push(avatar)
        .push(common::clipped_cell(header_info.into()))
        .spacing(24)
        .align_y(Alignment::Center);

    let header = common::hero_header(
        hero.and_then(|h| h.0),
        hero.and_then(|h| h.1),
        Some(common::hero_back_button(
            fl!("back-to-artists"),
            ArtistMessage::BackToList,
        )),
        header.into(),
    );
    let mut content = widget::Column::new().push(header).spacing(16);

    if let Some(bio) = artist_bio.filter(|b| !b.trim().is_empty()) {
        content = content.push(bio_block(bio, bio_expanded));
    }
    for (album_idx, album) in artist.albums.iter().enumerate() {
        let key = CoverArt::album_key(&artist.name, &album.name);
        let open_album = ArtistMessage::Navigate(crate::views::Route::Album {
            artist: artist.name.clone(),
            album: album.name.clone(),
        });
        let album_art: cosmic::Element<'_, ArtistMessage> =
            if let Some(handle) = cover_images.get(&key) {
                let radius = cosmic::theme::active().cosmic().corner_radii.radius_s[0];
                widget::button::custom(common::cover_art(handle, 64.0, radius, true))
                    .padding(0)
                    .on_press(open_album.clone())
                    .class(cosmic::theme::Button::Transparent)
                    .into()
            } else {
                widget::icon::from_name("media-optical-symbolic")
                    .size(48)
                    .into()
            };

        let album_info = widget::Column::new()
            .push(common::link(
                widget::text::title4(album.name.as_str()).wrapping(Wrapping::None),
                false,
                open_album,
            ))
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
            .push(widget::tooltip(
                widget::button::icon(widget::icon::from_name("go-next-symbolic").size(16))
                    .on_press(ArtistMessage::PlayNext(album.tracks.clone())),
                widget::text::caption(fl!("queue-play-next")),
                widget::tooltip::Position::Top,
            ))
            .push(widget::tooltip(
                widget::button::icon(
                    widget::icon::from_name("view-list-ordered-symbolic").size(16),
                )
                .on_press(ArtistMessage::AddToQueue(album.tracks.clone())),
                widget::text::caption(fl!("queue-add")),
                widget::tooltip::Position::Top,
            ))
            .spacing(12)
            .align_y(Alignment::Center);

        content = content.push(album_header);

        let columns = track_row::Columns {
            favorite: true,
            rating: true,
            quality: true,
            queue_actions: true,
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
                        ArtistMessage::PlayTrack(artist_index, album_idx, track_idx),
                    )
                    .with_navigate(ArtistMessage::Navigate)
                    .with_favorite(ArtistMessage::ToggleFavorite(track_id))
                    .with_rating(move |r| ArtistMessage::SetRating(rating_track_id.clone(), r))
                    .with_queue_actions(
                        ArtistMessage::PlayNext(vec![track.clone()]),
                        ArtistMessage::AddToQueue(vec![track.clone()]),
                    )
                    // Show the artist under the title only for guest /
                    // compilation tracks, where it differs from this page.
                    .with_artist_subtitle(track.artist != artist.name)
                    .view()
                }),
            )
        });

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
