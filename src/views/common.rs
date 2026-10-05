// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Shared UI helpers for library views.
//!
//! Every view renders durations, single-line table cells, star ratings and
//! favorite toggles through these helpers so the whole app stays visually
//! consistent.

use std::borrow::Cow;

use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Color, ContentFit, Length};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;
use cosmic::widget::tooltip::Position as TooltipPosition;

use crate::fl;
use crate::library::Playlist;

/// Concrete text widget type returned by `cosmic::widget::text::*` helpers.
pub type Text<'a> = cosmic::iced::widget::Text<'a, cosmic::Theme, cosmic::Renderer>;

/// Fixed width for right-aligned duration cells (fits `H:MM:SS`).
pub const DURATION_WIDTH: f32 = 64.0;

/// Format a duration in whole seconds as `H:MM:SS` when at least an hour,
/// otherwise `M:SS`.
#[must_use]
pub fn format_duration(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Coarse human-readable duration for headers: `2h 44m`, `44m`, `12s`.
#[must_use]
pub fn format_duration_coarse(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{total_secs}s")
    }
}

/// Truncate on a `char` boundary, appending `…` when the input exceeds
/// `max_chars`.
#[must_use]
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", truncated.trim_end())
    }
}

/// Body text pinned to a single line: clipped at the cell edge, never wraps.
pub fn cell_text<'a>(content: impl Into<Cow<'a, str>> + 'a) -> Text<'a> {
    widget::text::body(content).wrapping(Wrapping::None)
}

/// Caption (small, dim) text pinned to a single line.
pub fn cell_caption<'a>(content: impl Into<Cow<'a, str>> + 'a) -> Text<'a> {
    widget::text::caption(content).wrapping(Wrapping::None)
}

/// Right-aligned, fixed-width duration cell for track rows.
pub fn duration_cell<'a, M: 'a>(total_secs: u64) -> cosmic::Element<'a, M> {
    widget::container(cell_caption(format_duration(total_secs)))
        .width(Length::Fixed(DURATION_WIDTH))
        .align_x(Horizontal::Right)
        .into()
}

/// Wrap `content` in a clipped, width-filling, vertically-centered
/// container so single-line cell text (which never wraps) can never paint
/// past the column boundary its parent row assigns it.
pub fn clipped_cell<'a, M: 'a>(content: cosmic::Element<'a, M>) -> cosmic::Element<'a, M> {
    widget::container(content)
        .width(Length::Fill)
        .align_y(Vertical::Center)
        .clip(true)
        .into()
}

/// Compact interactive five-star rating (roughly 100px wide).
///
/// Clicking a star sets the rating; clicking the current rating clears it
/// (sends `0`). Filled stars are tinted with the accent color.
pub fn star_rating<'a, M: Clone + 'static>(
    rating: Option<u8>,
    on_rate: impl Fn(u8) -> M,
) -> cosmic::Element<'a, M> {
    let current = rating.unwrap_or(0);
    let mut row = widget::Row::new().align_y(Alignment::Center);
    for star in 1u8..=5 {
        let icon_name = if star <= current {
            "starred-symbolic"
        } else {
            "non-starred-symbolic"
        };
        let new_rating = if star == current { 0 } else { star };
        row = row.push(
            widget::button::icon(widget::icon::from_name(icon_name).size(14))
                .padding(2)
                .selected(star <= current)
                .on_press(on_rate(new_rating)),
        );
    }
    row.into()
}

/// Button class for the favorite heart: `Button::Icon`'s built-in
/// `selected()` flag never actually recolors the icon (its style always
/// discards the resolved icon color unless the button is disabled), so both
/// states render identically. This custom class controls the color
/// directly: muted when not favorited, accent-tinted when favorited.
fn favorite_button_class(is_favorite: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let color = if is_favorite {
                cosmic.accent_color()
            } else {
                cosmic.icon_button.on_disabled
            };
            ButtonStyle {
                background: None,
                text_color: Some(color.into()),
                icon_color: Some(color.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        hovered: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.icon_button;
            let color = if is_favorite {
                cosmic.accent_color()
            } else {
                comp.on
            };
            ButtonStyle {
                background: Some(Background::Color(comp.hover.into())),
                text_color: Some(color.into()),
                icon_color: Some(color.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        pressed: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.icon_button;
            let color = if is_favorite {
                cosmic.accent_color()
            } else {
                comp.on
            };
            ButtonStyle {
                background: Some(Background::Color(comp.pressed.into())),
                text_color: Some(color.into()),
                icon_color: Some(color.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        disabled: Box::new(|theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.icon_button;
            ButtonStyle {
                background: None,
                text_color: Some(comp.on_disabled.into()),
                icon_color: Some(comp.on_disabled.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
    }
}

/// Heart-shaped favorite toggle with a tooltip.
///
/// Accent-tinted when favorited, visibly muted otherwise — the off state is
/// never confusable with the on state, and never relies on hover alone.
pub fn favorite_button<'a, M: Clone + 'static>(
    is_favorite: bool,
    on_toggle: M,
) -> cosmic::Element<'a, M> {
    let button = widget::button::icon(widget::icon::from_name("emblem-favorite-symbolic").size(16))
        .padding(4)
        .selected(is_favorite)
        .class(favorite_button_class(is_favorite))
        .on_press(on_toggle);
    widget::tooltip(
        button,
        widget::text::caption(if is_favorite {
            fl!("favorite-remove")
        } else {
            fl!("favorite-add")
        }),
        TooltipPosition::Top,
    )
    .into()
}

/// Fixed width for the audio-quality pill: every known tier's icon+label
/// combination fits within it, so a column of these badges never resizes
/// row-to-row the way an unconstrained label would (`"HI-RES"` is much
/// wider than `"CD"`).
pub const QUALITY_BADGE_WIDTH: f32 = 64.0;

/// Compact icon-and-label pill for a track or album's audio quality tier.
///
/// Renders a zero-size element for
/// [`AudioQuality::Unknown`](crate::library::quality::AudioQuality::Unknown)
/// rather than an empty pill, so a track that fails to classify leaves its
/// column blank instead of drawing an empty box; callers still wrap the
/// result in a fixed-width container (the same way `star_rating`'s column
/// is wrapped) so the column itself never jitters.
pub fn quality_badge<'a, M: 'static>(
    quality: crate::library::quality::AudioQuality,
) -> cosmic::Element<'a, M> {
    if !quality.is_known() {
        return widget::Space::new().into();
    }
    let pill = widget::Row::new()
        .push(widget::icon::from_name(quality.icon_name()).size(12))
        .push(widget::text::caption(quality.label()))
        .spacing(4)
        .align_y(Alignment::Center);
    widget::container(pill)
        .padding([2, 6])
        .width(QUALITY_BADGE_WIDTH)
        .align_x(Horizontal::Center)
        .class(cosmic::theme::Container::custom(|theme| {
            let cosmic = theme.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(
                    cosmic.background(false).component.divider.into(),
                )),
                border: cosmic::iced::Border {
                    radius: cosmic.corner_radii.radius_xs.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Icon button wrapped in a caption tooltip — for transport/utility controls.
pub fn icon_button<'a, M: Clone + 'static>(
    icon_name: &'static str,
    icon_size: u16,
    label: &'a str,
    on_press: M,
) -> cosmic::Element<'a, M> {
    widget::tooltip(
        widget::button::icon(widget::icon::from_name(icon_name).size(icon_size)).on_press(on_press),
        widget::text::caption(label),
        TooltipPosition::Top,
    )
    .into()
}

/// Centered empty-state placeholder with an icon, title and hint.
pub fn empty_state<'a, M: 'static>(
    icon_name: &'static str,
    title: impl Into<Cow<'a, str>> + 'a,
    subtitle: impl Into<Cow<'a, str>> + 'a,
) -> cosmic::Element<'a, M> {
    widget::container(
        widget::Column::new()
            .push(widget::icon::from_name(icon_name).size(64))
            .push(widget::text::title3(title))
            .push(widget::text::body(subtitle))
            .spacing(8)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Horizontal::Center)
    .align_y(Vertical::Center)
    .into()
}

/// Shared header for card-grid views: a right-aligned view-mode toggle
/// button that flips between grid and list icons/labels for the current
/// mode. Used by every card-grid view (albums, artists, genres) so the
/// toggle's placement and wording never drift between them.
pub fn view_mode_toggle_header<'a, M: Clone + 'static>(
    mode: crate::config::ViewMode,
    on_toggle: M,
) -> cosmic::Element<'a, M> {
    use crate::config::ViewMode;
    let toggle_icon = match mode {
        ViewMode::Grid => "view-list-symbolic",
        ViewMode::List => "view-grid-symbolic",
    };
    let toggle_label = match mode {
        ViewMode::Grid => fl!("switch-to-list"),
        ViewMode::List => fl!("switch-to-grid"),
    };
    let toggle_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name(toggle_icon).size(16)).on_press(on_toggle),
        widget::text::caption(toggle_label),
        TooltipPosition::Bottom,
    );
    widget::Row::new()
        .push(widget::Space::new().width(Length::Fill))
        .push(toggle_btn)
        .padding(16)
        .into()
}

/// Grid-card artwork tile: the cached cover/avatar icon at `size`, or a
/// card-styled placeholder frame with a 64px fallback icon when nothing is
/// cached yet. Shared by every card grid (albums, artists) so a missing
/// cover's frame never differs from the album/artist that has one.
pub fn grid_art_tile<'a, M: 'static>(
    handle: Option<&widget::icon::Handle>,
    size: u16,
    placeholder_icon: &'static str,
) -> cosmic::Element<'a, M> {
    match handle {
        Some(handle) => widget::icon::icon(handle.clone()).size(size).into(),
        None => {
            let placeholder: cosmic::Element<'a, M> =
                widget::icon::from_name(placeholder_icon).size(64).into();
            widget::container(placeholder)
                .width(f32::from(size))
                .height(f32::from(size))
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center)
                .class(cosmic::theme::Container::Card)
                .into()
        }
    }
}

/// List-row artwork icon: the cached cover/avatar icon at `size`, or an
/// unstyled fallback icon at the same size when nothing is cached.
pub fn list_art_icon<'a, M: 'static>(
    handle: Option<&widget::icon::Handle>,
    size: u16,
    placeholder_icon: &'static str,
) -> cosmic::Element<'a, M> {
    match handle {
        Some(handle) => widget::icon::icon(handle.clone()).size(size).into(),
        None => widget::icon::from_name(placeholder_icon).size(size).into(),
    }
}

/// Round artist avatar: a real photo (clipped to a circle via
/// `widget::image`'s native `border_radius` support) when `photo` is
/// available, otherwise the deterministic-color initials placeholder —
/// see `initials_avatar`. Built entirely from widgets, so it stays crisp
/// at any `size`/HiDPI scale factor, unlike a rasterized bitmap scaled up
/// to fit. Shared by every place an artist avatar is shown (grid, list,
/// detail view).
pub fn artist_avatar<'a, M: 'static>(
    name: &str,
    photo: Option<&widget::image::Handle>,
    size: f32,
) -> cosmic::Element<'a, M> {
    match photo {
        Some(handle) => widget::image(handle.clone())
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .content_fit(ContentFit::Cover)
            .border_radius(size / 2.0)
            .into(),
        None => initials_avatar(name, size),
    }
}

/// Deterministic-color circle with centered initials — no raster image
/// involved, so it never blurs/pixelates at large sizes or on HiDPI
/// displays the way the old rasterized-at-64px avatar did. See
/// `artist_avatar`.
pub fn initials_avatar<'a, M: 'static>(name: &str, size: f32) -> cosmic::Element<'a, M> {
    let initials = crate::library::CoverArt::artist_initials(name);
    let (r, g, b) = crate::library::CoverArt::artist_avatar_color(name);
    let background = Color::from_rgb8(r, g, b);
    let font_size = (size * 0.36).max(11.0);

    widget::container(
        widget::text(initials)
            .size(font_size)
            .class(cosmic::theme::Text::Color(Color::WHITE)),
    )
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .align_x(Horizontal::Center)
    .align_y(Vertical::Center)
    .class(cosmic::theme::Container::custom(move |_theme| {
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(background)),
            border: cosmic::iced::Border {
                radius: (size / 2.0).into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }))
    .into()
}

/// Fixed-height two-line label block under a grid card: a title element
/// above a subtitle element, both pinned to `card_width` so every card's
/// caption block is exactly `label_height` tall regardless of whether the
/// subtitle has text. Callers clip their own title/subtitle content
/// (typically with [`clipped_cell`]) before passing it in, since a
/// subtitle may itself be a row with more than one clipped piece.
pub fn grid_card_label<'a, M: 'a>(
    card_width: f32,
    label_height: f32,
    title: cosmic::Element<'a, M>,
    subtitle: cosmic::Element<'a, M>,
) -> cosmic::Element<'a, M> {
    widget::container(
        widget::Column::new()
            .push(widget::container(title).width(card_width))
            .push(widget::container(subtitle).width(card_width))
            .spacing(2),
    )
    .height(Length::Fixed(label_height))
    .into()
}

/// Assembles a grid card: a fixed `card_width`×`card_width` centered art
/// tile above a label block. Shared by every card grid (albums, artists)
/// so art framing and card spacing never drift between them.
pub fn grid_card<'a, M: 'a>(
    art: cosmic::Element<'a, M>,
    card_width: f32,
    label_block: cosmic::Element<'a, M>,
) -> cosmic::Element<'a, M> {
    widget::Column::new()
        .push(
            widget::container(art)
                .width(card_width)
                .height(card_width)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center),
        )
        .push(label_block)
        .spacing(8)
        .into()
}

/// Add-to-playlist icon button. Adds to the first playlist, honestly
/// labelled via tooltip with that playlist's name. Renders empty space
/// instead of a dead button when there are no playlists yet. Shared by
/// every track row that offers this action (songs, albums).
pub fn add_to_playlist_button<'a, M: 'static + Clone>(
    source_uri: String,
    playlists: &'a [Playlist],
    make_message: impl FnOnce(String, String) -> M,
    empty_width: f32,
) -> cosmic::Element<'a, M> {
    if let Some(playlist) = playlists.first() {
        let button = widget::button::icon(widget::icon::from_name("list-add-symbolic").size(16))
            .on_press(make_message(source_uri, playlist.id.clone()));
        widget::tooltip(
            button,
            widget::text::caption(fl!(
                "songs-add-to-playlist",
                playlist = playlist.name.as_str()
            )),
            TooltipPosition::Top,
        )
        .into()
    } else {
        widget::Space::new().width(empty_width).into()
    }
}

#[cfg(test)]
mod tests {
    use super::{format_duration, format_duration_coarse, truncate_str};

    #[test]
    fn format_duration_sub_minute() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(59), "0:59");
    }

    #[test]
    fn format_duration_minutes_only() {
        assert_eq!(format_duration(60), "1:00");
        assert_eq!(format_duration(3599), "59:59");
    }

    #[test]
    fn format_duration_hour_boundary() {
        assert_eq!(format_duration(3600), "1:00:00");
    }

    #[test]
    fn format_duration_regression_164_minutes() {
        // 9860s = 2h 44m 20s. Previously rendered as the bogus "164:20"
        // because minutes were never rolled over into hours.
        assert_eq!(format_duration(9860), "2:44:20");
    }

    #[test]
    fn format_duration_coarse_seconds_only() {
        assert_eq!(format_duration_coarse(12), "12s");
    }

    #[test]
    fn format_duration_coarse_minutes_only() {
        assert_eq!(format_duration_coarse(300), "5m");
    }

    #[test]
    fn format_duration_coarse_hours_and_minutes() {
        assert_eq!(format_duration_coarse(9860), "2h 44m");
    }

    #[test]
    fn format_duration_coarse_hour_boundary_zero_minutes() {
        assert_eq!(format_duration_coarse(3600), "1h 00m");
    }

    #[test]
    fn truncate_str_shorter_than_max_is_unchanged() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact_max_is_unchanged() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_over_max_ends_with_ellipsis_and_respects_length() {
        let result = truncate_str("hello world", 8);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 8);
    }

    #[test]
    fn truncate_str_trims_trailing_whitespace_before_ellipsis() {
        // Cutting right after "hello" lands on trailing spaces; they must be
        // trimmed so the ellipsis doesn't float after visible whitespace.
        let result = truncate_str("hello   world", 8);
        assert_eq!(result, "hello…");
    }

    #[test]
    fn truncate_str_multibyte_safe_no_panic() {
        // Multibyte (non-ASCII) chars must be counted and sliced by `char`,
        // never by byte, or this would panic on a non-boundary split.
        let cjk = "日本語のテスト";
        let result = truncate_str(cjk, 4);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 4);

        let accented = "ééééééééé";
        let result = truncate_str(accented, 5);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 5);
    }
}
