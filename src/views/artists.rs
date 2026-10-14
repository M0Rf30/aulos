// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Artists view - list of artists with album sub-views.

use crate::views::common;
use crate::views::context_menu;
use crate::views::track_row;
use crate::views::{card_button_class, list_row_button_class};
use aulos_core::fl;
use aulos_core::library::Artist;
use aulos_core::library::display::bio_preview;
use aulos_core::menu::AlbumMenuAction;
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
    /// Insert tracks right after the currently playing one: track `.2` of
    /// album `.1` of artist `.0`, or the whole album when `None`. Carries
    /// indices only; `update` resolves them, so `view()` never clones
    /// track lists.
    PlayNext(usize, usize, Option<usize>),
    /// Append tracks (see `PlayNext`) to the end of the queue.
    AddToQueue(usize, usize, Option<usize>),
    /// A virtualized grid/list scrolled (area, absolute y offset).
    Scrolled(common::ScrollArea, f32),
    /// Expand/collapse the clipped biography preview in the detail view.
    ToggleBioExpanded,
    /// Jump to another view (album page, genre…).
    Navigate(crate::views::Route),
    /// Open the Properties drawer for this track.
    ShowProperties(aulos_core::library::Track),
    /// Open the Properties drawer for album `.1` of artist `.0`.
    ShowAlbumProperties(usize, usize),
    /// Open the tag editor on this (local) track.
    Tag(aulos_core::library::Track, aulos_core::tagger::TagAction),
    /// Open the tag editor on album `.1` of artist `.0`.
    TagAlbum(usize, usize, aulos_core::tagger::TagAction),
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
    mode: aulos_core::config::ViewMode,
    scroll: &common::ScrollOffsets,
) -> cosmic::Element<'a, ArtistMessage> {
    if artists.is_empty() {
        return common::empty_state(
            "system-users-symbolic",
            fl!("no-artists"),
            fl!("artists-empty-hint"),
        );
    }

    use aulos_core::config::ViewMode;

    let header = common::view_mode_toggle_header(mode, ArtistMessage::ToggleViewMode);

    let content: cosmic::Element<'_, ArtistMessage> = match mode {
        ViewMode::List => common::virtual_list(
            artists.len(),
            LIST_ROW_HEIGHT,
            common::Scroll {
                area: common::ScrollArea::ArtistsList,
                offset: scroll.get(common::ScrollArea::ArtistsList),
                on_scroll: ArtistMessage::Scrolled,
            },
            move |index| list_row(index, &artists[index], artist_photos),
        ),
        ViewMode::Grid => common::fluid_card_grid_virtual(
            artists.len(),
            CARD_WIDTH + 2.0 * CARD_PADDING,
            CARD_MAX_WIDTH + 2.0 * CARD_PADDING,
            common::square_card_height(CARD_LABEL_HEIGHT),
            common::Scroll {
                area: common::ScrollArea::ArtistsGrid,
                offset: scroll.get(common::ScrollArea::ArtistsGrid),
                on_scroll: ArtistMessage::Scrolled,
            },
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
                    common::clipped_cell(common::cell_caption(artist.summary()).into()),
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

/// Fixed height of an artists-list row.
const LIST_ROW_HEIGHT: f32 = 68.0;

/// One fixed-height row of the artists list (`index` is the artist's
/// position in the slice handed to [`artists_view`]).
fn list_row<'a>(
    index: usize,
    artist: &'a Artist,
    artist_photos: &'a std::collections::HashMap<String, widget::image::Handle>,
) -> cosmic::Element<'a, ArtistMessage> {
    let dim = |s: String| {
        common::cell_caption(s).class(cosmic::theme::Text::Custom(|theme| {
            cosmic::iced::widget::text::Style {
                color: Some(theme.cosmic().palette.neutral_7.into()),
                ..Default::default()
            }
        }))
    };

    let avatar = common::artist_avatar(&artist.name, artist_photos.get(&artist.name), 52.0);

    let info = common::cell_text(artist.name.as_str()).font(cosmic::font::semibold());

    widget::button::custom(
        widget::Row::new()
            .push(avatar)
            .push(
                widget::container(common::clipped_cell(info.into())).width(Length::FillPortion(5)),
            )
            .push(
                widget::container(dim(artist.albums_label()))
                    .width(80)
                    .align_x(Horizontal::Right),
            )
            .push(
                widget::container(dim(artist.tracks_label()))
                    .width(80)
                    .align_x(Horizontal::Right),
            )
            .push(
                widget::container(dim(artist.duration_label()))
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
    .height(Length::Fixed(LIST_ROW_HEIGHT))
    .padding(0)
    .class(list_row_button_class(false))
    .into()
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
        Option<&'a aulos_core::library::palette::Accent>,
    )>,
) -> cosmic::Element<'a, ArtistMessage> {
    let avatar = common::artist_avatar(&artist.name, artist_photos.get(&artist.name), 120.0);

    let header_info = widget::Column::new()
        .push(widget::text::title1(artist.name.as_str()).wrapping(Wrapping::None))
        .push(common::cell_caption(artist.summary()))
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
        let open_album = ArtistMessage::Navigate(crate::views::Route::Album {
            artist: artist.name.clone(),
            album: album.name.clone(),
        });
        let album_art: cosmic::Element<'_, ArtistMessage> =
            if let Some(handle) = cover_images.get(&album.cover_key) {
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
            .push(common::cell_caption(album.track_summary()));

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
                    .on_press(ArtistMessage::PlayNext(artist_index, album_idx, None)),
                widget::text::caption(fl!("queue-play-next")),
                widget::tooltip::Position::Top,
            ))
            .push(widget::tooltip(
                widget::button::icon(
                    widget::icon::from_name("view-list-ordered-symbolic").size(16),
                )
                .on_press(ArtistMessage::AddToQueue(artist_index, album_idx, None)),
                widget::text::caption(fl!("queue-add")),
                widget::tooltip::Position::Top,
            ))
            .spacing(12)
            .align_y(Alignment::Center);

        // Right-clicking an album's header offers the album-level actions.
        let taggable = album.is_taggable();
        content = content.push(context_menu::with_menu(album_header, move || {
            let mut entries = vec![
                AlbumMenuAction::Play(ArtistMessage::PlayArtistAlbum(artist_index, album_idx)),
                AlbumMenuAction::PlayNext(ArtistMessage::PlayNext(artist_index, album_idx, None)),
                AlbumMenuAction::AddToQueue(ArtistMessage::AddToQueue(
                    artist_index,
                    album_idx,
                    None,
                )),
                AlbumMenuAction::Properties(ArtistMessage::ShowAlbumProperties(
                    artist_index,
                    album_idx,
                )),
            ];
            if taggable {
                use aulos_core::tagger::TagAction;
                entries.extend([
                    AlbumMenuAction::EditTags(ArtistMessage::TagAlbum(
                        artist_index,
                        album_idx,
                        TagAction::Edit,
                    )),
                    AlbumMenuAction::LookUpMusicBrainz(ArtistMessage::TagAlbum(
                        artist_index,
                        album_idx,
                        TagAction::LookUp,
                    )),
                    AlbumMenuAction::ScanAcoustId(ArtistMessage::TagAlbum(
                        artist_index,
                        album_idx,
                        TagAction::AcoustId,
                    )),
                ]);
            }
            context_menu::album_menu(entries)
        }));

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
                    let number = track.display_number(track_idx);
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
                        ArtistMessage::PlayNext(artist_index, album_idx, Some(track_idx)),
                        ArtistMessage::AddToQueue(artist_index, album_idx, Some(track_idx)),
                    )
                    .with_properties(ArtistMessage::ShowProperties(track.clone()))
                    .with_tagging(ArtistMessage::Tag)
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

/// Clipped-biography card: the full text once `expanded`, otherwise a
/// character-capped preview with a trailing ellipsis, plus a
/// show-more/show-less link when the text is actually long enough to
/// need clipping.
fn bio_block<'a>(bio: &'a str, expanded: bool) -> cosmic::Element<'a, ArtistMessage> {
    let preview = bio_preview(bio, expanded);

    let mut col = widget::Column::new()
        .push(widget::text::body(preview.text))
        .spacing(8);

    if preview.is_long {
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
