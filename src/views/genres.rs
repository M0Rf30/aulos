// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Genres view - colourful category tiles; clicking one shows its tracks.

use crate::fl;
use crate::library::Track;
use crate::library::palette::Accent;
use crate::views::card_button_class;
use crate::views::common;
use crate::views::list_row_button_class;
use crate::views::track_row;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::gradient::Linear;
use cosmic::iced::{Alignment, Background, Border, Color, Gradient, Length, Radians, Shadow};
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
    /// Play the whole genre in shuffled order.
    Shuffle,
    /// Insert tracks right after the current one.
    PlayNext(Vec<Track>),
    /// Append tracks to the end of the queue.
    AddToQueue(Vec<Track>),
    /// Toggle between grid and list layout.
    ToggleViewMode,
    /// Jump to another view (artist / album page).
    Navigate(crate::views::Route),
}

/// Minimum / maximum outer card width (including button padding).
const CARD_WIDTH: f32 = 160.0;
const CARD_MAX_WIDTH: f32 = 240.0;
/// Padding of the card button around its tile.
const CARD_PADDING: f32 = 8.0;
/// Tile height relative to its width.
const TILE_ASPECT: f32 = 0.64;
/// Size of the cover-like tile on the detail page.
const HERO_TILE: f32 = 160.0;

/// Curated gradient pairs (start, end) as sRGB bytes. All are dark enough
/// for white text to stay legible on top.
const GRADIENTS: [([u8; 3], [u8; 3]); 12] = [
    ([214, 40, 70], [130, 20, 70]),
    ([30, 140, 110], [16, 80, 85]),
    ([50, 80, 170], [85, 45, 150]),
    ([200, 100, 20], [140, 50, 25]),
    ([140, 100, 180], [80, 55, 135]),
    ([20, 120, 225], [20, 60, 160]),
    ([180, 45, 145], [110, 30, 125]),
    ([10, 140, 165], [20, 80, 120]),
    ([100, 110, 125], [55, 60, 75]),
    ([205, 65, 35], [130, 30, 45]),
    ([100, 140, 30], [45, 95, 45]),
    ([205, 85, 120], [150, 45, 95]),
];

/// Deterministic gradient for a genre name (case/whitespace-insensitive).
fn genre_gradient(genre: &str) -> (Color, Color) {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in genre.trim().to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    let (a, b) = GRADIENTS[(hash % GRADIENTS.len() as u64) as usize];
    (
        Color::from_rgb8(a[0], a[1], a[2]),
        Color::from_rgb8(b[0], b[1], b[2]),
    )
}

/// Rounded gradient tile of fixed size with white foreground defaults.
fn genre_tile<'a, M: 'a>(
    genre: &str,
    width: f32,
    height: f32,
    radius: f32,
    elevated: bool,
    content: cosmic::Element<'a, M>,
) -> cosmic::Element<'a, M> {
    let (start, end) = genre_gradient(genre);
    widget::container(content)
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .clip(true)
        .class(cosmic::theme::Container::custom(move |_theme| {
            cosmic::iced::widget::container::Style {
                icon_color: Some(Color::WHITE),
                text_color: Some(Color::WHITE),
                background: Some(Background::Gradient(Gradient::Linear(
                    Linear::new(Radians(std::f32::consts::PI * 0.75))
                        .add_stop(0.0, start)
                        .add_stop(1.0, end),
                ))),
                border: Border {
                    radius: radius.into(),
                    ..Default::default()
                },
                shadow: if elevated {
                    Shadow {
                        color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
                        offset: cosmic::iced::Vector::new(0.0, 4.0),
                        blur_radius: 14.0,
                    }
                } else {
                    Shadow::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Card tile for the grid: big bold title top-left, genre icon bottom-right.
fn genre_card_tile<'a>(genre: &'a str, width: f32) -> cosmic::Element<'a, GenreMessage> {
    let radius = cosmic::theme::active().cosmic().corner_radii.radius_m[0];
    let height = (width * TILE_ASPECT).round();
    let icon_size = (height * 0.42).clamp(32.0, 64.0) as u16;

    let title = widget::container(
        widget::text::title4(genre)
            .font(cosmic::font::bold())
            .wrapping(Wrapping::WordOrGlyph)
            .class(cosmic::theme::Text::Color(Color::WHITE)),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(14)
    .clip(true);

    let icon = widget::container(widget::icon::from_name(genre_icon_name(genre)).size(icon_size))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Right)
        .align_y(Vertical::Bottom)
        .padding(10);

    let stack = cosmic::iced::widget::Stack::new()
        .width(Length::Fill)
        .height(Length::Fill)
        .push(icon)
        .push(title);

    genre_tile(genre, width, height, radius, false, stack.into())
}

/// Render the genres view: tile grid or list, depending on `mode`.
pub fn genres_view(
    genres: &[String],
    mode: crate::config::ViewMode,
) -> cosmic::Element<'_, GenreMessage> {
    if genres.is_empty() {
        return common::empty_state(
            "folder-music-symbolic",
            fl!("no-genres"),
            fl!("genres-empty-hint"),
        );
    }

    use crate::config::ViewMode;

    let header = common::view_mode_toggle_header(mode, GenreMessage::ToggleViewMode);

    let content: cosmic::Element<'_, GenreMessage> = match mode {
        ViewMode::Grid => common::fluid_card_grid(
            genres.len(),
            CARD_WIDTH + 2.0 * CARD_PADDING,
            CARD_MAX_WIDTH + 2.0 * CARD_PADDING,
            move |index, outer| {
                let genre = genres[index].as_str();
                let tile = genre_card_tile(genre, outer - 2.0 * CARD_PADDING);
                widget::tooltip(
                    widget::button::custom(tile)
                        .on_press(GenreMessage::SelectGenre(index))
                        .padding(CARD_PADDING as u16)
                        .class(card_button_class()),
                    widget::text::caption(genre),
                    widget::tooltip::Position::Top,
                )
                .into()
            },
        ),
        ViewMode::List => {
            let radius = cosmic::theme::active().cosmic().corner_radii.radius_s[0];
            let mut list = widget::Column::new().spacing(2);

            for (index, genre) in genres.iter().enumerate() {
                let swatch_icon: cosmic::Element<'_, GenreMessage> =
                    widget::container(widget::icon::from_name(genre_icon_name(genre)).size(24))
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .align_x(Horizontal::Center)
                        .align_y(Vertical::Center)
                        .into();
                let swatch = genre_tile(genre, 48.0, 48.0, radius, false, swatch_icon);

                let row = widget::button::custom(
                    widget::Row::new()
                        .push(swatch)
                        .push(common::clipped_cell(
                            common::cell_text(genre.as_str()).into(),
                        ))
                        .push(widget::icon::from_name("go-next-symbolic").size(16))
                        .spacing(14)
                        .align_y(Alignment::Center)
                        .padding([8, 8]),
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
    current_track_id: Option<i64>,
) -> cosmic::Element<'a, GenreMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let radius = cosmic::theme::active().cosmic().corner_radii.radius_m[0];

    let (start, _) = genre_gradient(genre_name);
    let accent = Accent {
        color: [start.r, start.g, start.b],
        on_color: [1.0, 1.0, 1.0],
    };

    let tile_icon: cosmic::Element<'_, GenreMessage> =
        widget::container(widget::icon::from_name(genre_icon_name(genre_name)).size(72))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .into();
    let tile = genre_tile(genre_name, HERO_TILE, HERO_TILE, radius, true, tile_icon);

    let total_secs: u64 = tracks.iter().map(|t| t.duration.as_secs()).sum();
    let summary = if tracks.is_empty() {
        genre_track_count_label(0)
    } else {
        format!(
            "{} \u{b7} {}",
            genre_track_count_label(tracks.len()),
            common::format_duration_coarse(total_secs)
        )
    };
    let has_tracks = !tracks.is_empty();

    let meta =
        widget::Column::new()
            .push(common::cell_caption(fl!("genre-label")))
            .push(widget::container(common::clipped_cell(
                widget::text::title1(genre_name)
                    .wrapping(Wrapping::None)
                    .into(),
            )))
            .push(common::cell_caption(summary))
            .push(
                widget::Row::new()
                    .push(
                        widget::button::suggested(fl!("play-all"))
                            .on_press_maybe(has_tracks.then_some(GenreMessage::PlayTrack(0)))
                            .class(common::accent_button_class(&accent)),
                    )
                    .push(
                        widget::button::standard(fl!("shuffle"))
                            .on_press_maybe(has_tracks.then_some(GenreMessage::Shuffle)),
                    )
                    .push(
                        widget::button::standard(fl!("queue-play-next")).on_press_maybe(
                            has_tracks.then(|| GenreMessage::PlayNext(tracks.to_vec())),
                        ),
                    )
                    .push(widget::button::standard(fl!("queue-add")).on_press_maybe(
                        has_tracks.then(|| GenreMessage::AddToQueue(tracks.to_vec())),
                    ))
                    .spacing(spacing.space_xs),
            )
            .width(Length::Fill)
            .spacing(spacing.space_xxs);

    let header = widget::Row::new()
        .push(tile)
        .push(meta)
        .spacing(spacing.space_m)
        .align_y(Alignment::Center);

    let columns = track_row::Columns {
        artist: true,
        album: true,
        queue_actions: true,
        ..Default::default()
    };
    let track_list = track_row::width_aware(tracks.len(), true, move |width| {
        let columns = columns.responsive(width);
        track_row::rows_column(
            Some(track_row::Header::new(columns)),
            tracks.iter().enumerate().map(|(index, track)| {
                track_row::TrackRow::new(
                    track,
                    (index + 1).to_string(),
                    current_track_id == Some(track.id),
                    columns,
                    GenreMessage::PlayTrack(index),
                )
                .with_navigate(GenreMessage::Navigate)
                .with_queue_actions(
                    GenreMessage::PlayNext(vec![track.clone()]),
                    GenreMessage::AddToQueue(vec![track.clone()]),
                )
                .with_artist_subtitle(!columns.artist)
                .view()
            }),
        )
    });

    let list_section: cosmic::Element<'_, GenreMessage> = if tracks.is_empty() {
        widget::container(
            widget::Column::new()
                .push(widget::icon::from_name("folder-music-symbolic").size(48))
                .push(widget::text::title3(fl!("no-tracks-found")))
                .push(widget::text::body(fl!("genre-empty-hint")))
                .spacing(spacing.space_xs)
                .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .padding(spacing.space_xl)
        .align_x(Horizontal::Center)
        .into()
    } else {
        track_list
    };

    widget::scrollable(
        widget::Column::new()
            .push(common::hero_header(
                None,
                Some(&accent),
                Some(common::hero_back_button(
                    fl!("back-to-genres"),
                    GenreMessage::BackToGrid,
                )),
                header.into(),
            ))
            .push(widget::divider::horizontal::default())
            .push(list_section)
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
        "media-optical-symbolic"
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
