// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Genres view - grid of all genres; clicking one shows filtered tracks.

use crate::fl;
use crate::library::Track;
use crate::views::card_button_class;
use crate::views::common;
use crate::views::list_row_button_class;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;

/// Messages from the genres view.
#[derive(Debug, Clone)]
pub enum GenreMessage {
    /// User selected a genre to view its tracks.
    SelectGenre(usize),
    /// Go back to the genre grid from the filtered track list.
    BackToGrid,
    /// Play a specific track in the genre detail view.
    PlayTrack(usize),
    /// Toggle between grid and list layout.
    ToggleViewMode,
}

/// Render the genres view: card grid or list, depending on `mode`.
pub fn genres_view(
    genres: &[String],
    mode: crate::config::ViewMode,
) -> cosmic::Element<'_, GenreMessage> {
    if genres.is_empty() {
        return common::empty_state(
            "audio-x-generic-symbolic",
            fl!("no-genres"),
            fl!("genres-empty-hint"),
        );
    }

    use crate::config::ViewMode;

    let header = common::view_mode_toggle_header(mode, GenreMessage::ToggleViewMode);

    let content: cosmic::Element<'_, GenreMessage> = match mode {
        ViewMode::Grid => {
            let cards: Vec<cosmic::Element<'_, GenreMessage>> = genres
                .iter()
                .enumerate()
                .map(|(index, genre)| {
                    let icon_name = genre_icon_name(genre);
                    let genre_icon: cosmic::Element<'_, GenreMessage> =
                        widget::icon::from_name(icon_name).size(48).into();

                    let icon_container: cosmic::Element<'_, GenreMessage> =
                        widget::container(genre_icon)
                            .width(140)
                            .height(100)
                            .align_x(Horizontal::Center)
                            .align_y(Vertical::Center)
                            .class(cosmic::theme::Container::Card)
                            .into();

                    let label = widget::container(common::clipped_cell(
                        common::cell_text(genre.as_str())
                            .width(140)
                            .align_x(Horizontal::Center)
                            .into(),
                    ))
                    .width(140)
                    .height(Length::Fixed(20.0))
                    .align_x(Horizontal::Center)
                    .align_y(Vertical::Center);

                    let card = widget::Column::new()
                        .push(icon_container)
                        .push(label)
                        .spacing(6)
                        .align_x(Alignment::Center);

                    widget::button::custom(card)
                        .on_press(GenreMessage::SelectGenre(index))
                        .padding(8)
                        .class(card_button_class())
                        .into()
                })
                .collect();

            let grid = widget::flex_row(cards)
                .column_spacing(20)
                .row_spacing(20)
                .width(Length::Fill);

            widget::scrollable(widget::container(grid).padding(16).width(Length::Fill))
                .height(Length::Fill)
                .into()
        }
        ViewMode::List => {
            let mut list = widget::Column::new().spacing(2);

            for (index, genre) in genres.iter().enumerate() {
                let icon_name = genre_icon_name(genre);
                let genre_icon: cosmic::Element<'_, GenreMessage> =
                    widget::icon::from_name(icon_name).size(48).into();

                let row = widget::button::custom(
                    widget::Row::new()
                        .push(genre_icon)
                        .push(common::clipped_cell(common::cell_text(genre.as_str()).into()))
                        .spacing(14)
                        .align_y(Alignment::Center)
                        .padding([10, 8]),
                )
                .on_press(GenreMessage::SelectGenre(index))
                .width(Length::Fill)
                .class(list_row_button_class(false));

                list = list.push(row);
            }

            widget::scrollable(widget::container(list).padding(16).width(Length::Fill))
                .height(Length::Fill)
                .into()
        }
    };

    widget::Column::new().push(header).push(content).into()
}

/// Render the detail view for a selected genre, showing filtered tracks.
pub fn genre_detail_view<'a>(
    genre_name: &'a str,
    tracks: &'a [Track],
) -> cosmic::Element<'a, GenreMessage> {
    let detail_icon: cosmic::Element<'_, GenreMessage> =
        widget::icon::from_name(genre_icon_name(genre_name))
            .size(64)
            .into();

    let header = widget::Row::new()
        .push(
            widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                .on_press(GenreMessage::BackToGrid),
        )
        .push(detail_icon)
        .push(common::clipped_cell(
            widget::Column::new()
                .push(widget::text::title1(genre_name).wrapping(Wrapping::None))
                .push(common::cell_caption(genre_track_count_label(tracks.len())))
                .spacing(4)
                .into(),
        ))
        .spacing(16)
        .align_y(Alignment::Center);

    let mut track_list = widget::Column::new().spacing(1);

    for (index, track) in tracks.iter().enumerate() {
        let title_col = widget::container(common::clipped_cell(
            common::cell_text(track.title.as_str()).into(),
        ))
        .width(Length::FillPortion(4));
        let artist_col = widget::container(common::clipped_cell(
            common::cell_text(track.artist.as_str()).into(),
        ))
        .width(Length::FillPortion(3));
        let album_col = widget::container(common::clipped_cell(
            common::cell_text(track.album.as_str()).into(),
        ))
        .width(Length::FillPortion(3));

        let row = widget::button::custom(
            widget::Row::new()
                .push(common::cell_text((index + 1).to_string()).width(40))
                .push(title_col)
                .push(artist_col)
                .push(album_col)
                .push(common::duration_cell(track.duration.as_secs()))
                .spacing(8)
                .width(Length::Fill)
                .align_y(Alignment::Center)
                .padding([4, 8]),
        )
        .on_press(GenreMessage::PlayTrack(index))
        .width(Length::Fill)
        .class(list_row_button_class(false));

        track_list = track_list.push(row);
    }

    if tracks.is_empty() {
        track_list = track_list.push(common::empty_state(
            "audio-x-generic-symbolic",
            fl!("no-tracks-found"),
            fl!("genre-empty-hint"),
        ));
    }

    widget::scrollable(
        widget::Column::new()
            .push(header)
            .push(widget::divider::horizontal::default())
            .push(track_list)
            .spacing(16)
            .padding(16),
    )
    .height(Length::Fill)
    .into()
}

fn genre_icon_name(genre: &str) -> &'static str {
    let lower = genre.to_lowercase();
    if lower.contains("rock") || lower.contains("metal") || lower.contains("punk") {
        "audio-x-generic-symbolic"
    } else if lower.contains("classic") || lower.contains("orchestra") || lower.contains("opera") {
        "media-optical-cd-audio-symbolic"
    } else if lower.contains("jazz") || lower.contains("blues") || lower.contains("soul") {
        "audio-card-symbolic"
    } else if lower.contains("electronic")
        || lower.contains("techno")
        || lower.contains("trance")
        || lower.contains("house")
        || lower.contains("edm")
        || lower.contains("synth")
    {
        "computer-symbolic"
    } else if lower.contains("folk") || lower.contains("country") || lower.contains("acoustic") {
        "emblem-music-symbolic"
    } else if lower.contains("hip") || lower.contains("rap") || lower.contains("r&b") {
        "media-record-symbolic"
    } else if lower.contains("pop") {
        "starred-symbolic"
    } else if lower.contains("ambient") || lower.contains("new age") || lower.contains("asmr") {
        "weather-clear-night-symbolic"
    } else if lower.contains("soundtrack") || lower.contains("score") || lower.contains("theme") {
        "applications-multimedia-symbolic"
    } else if lower.contains("reggae") || lower.contains("ska") {
        "weather-clear-symbolic"
    } else {
        "audio-x-generic-symbolic"
    }
}

/// Localized "N tracks" label used in the genre detail header.
fn genre_track_count_label(count: usize) -> String {
    if count == 1 {
        fl!("genre-track-count-one", count = count.to_string())
    } else {
        fl!("genre-track-count-other", count = count.to_string())
    }
}
