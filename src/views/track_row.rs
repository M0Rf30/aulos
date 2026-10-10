// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The one track row (and matching column header) every track table in the
//! app is built from: Songs, album/artist/genre detail, playlists, smart
//! playlists and folders.
//!
//! A row is described by a [`TrackRow`] (the track, which messages its
//! actions emit) plus a [`Columns`] set (which optional columns exist and the
//! gutter reserved on the right). The header built from the *same*
//! [`Columns`] shares every fixed width and `FillPortion`, so labels always
//! line up with their values.
//!
//! Layout notes (load-bearing):
//! * Rows are a fixed [`ROW_HEIGHT`]; Songs' list virtualization computes the
//!   visible window arithmetically from [`ROW_STRIDE`].
//! * The row's own `Length::Fill` width is what lets the `FillPortion`
//!   title/artist/album cells share the leftover width proportionally; each
//!   cell is also clipped ([`common::clipped_cell`]) so long text can never
//!   paint into its neighbour.
//! * Fixed-width columns are always emitted when their [`Columns`] flag is set
//!   — even when the cell has no content for this row — so columns never jump
//!   between rows.

use crate::fl;
use crate::library::{Playlist, Track};
use crate::views::{Route, common, list_row_button_class};
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::{Alignment, Border, Color, Length, Size};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;
use cosmic::widget::tooltip::Position as TooltipPosition;

/// Fixed height of a track row.
pub const ROW_HEIGHT: f32 = 48.0;
/// Gap between consecutive rows.
pub const ROW_GAP: f32 = 2.0;
/// Distance from the top of one row to the top of the next.
pub const ROW_STRIDE: f32 = ROW_HEIGHT + ROW_GAP;
/// Height of the column header row.
pub const HEADER_HEIGHT: f32 = 28.0;

/// Width of the leading number / now-playing indicator column.
pub const NUM_WIDTH: f32 = 40.0;
/// Width of the genre pill column.
pub const GENRE_WIDTH: f32 = 130.0;
/// Width of every single-icon column (favorite, queue actions, playlist, …).
pub const ICON_WIDTH: f32 = 32.0;
/// Width of the star-rating column.
pub const RATING_WIDTH: f32 = 112.0;
/// Spacing between adjacent columns, identical in the header and every row.
pub const COLUMN_SPACING: f32 = 8.0;
/// Left edge padding for the header and every row.
pub const ROW_PADDING_LEFT: f32 = 8.0;

/// Width at which the artist column and the queue actions appear.
pub const ARTIST_BREAKPOINT: f32 = 640.0;
/// Width at which the album, rating and quality columns appear.
pub const ALBUM_RATING_BREAKPOINT: f32 = 900.0;
/// Width at which the genre column appears.
pub const GENRE_BREAKPOINT: f32 = 1100.0;

/// Which optional columns a table shows.
///
/// The title, number and duration columns are always present. Build one with
/// the columns a view *could* show, then pass it through
/// [`Columns::responsive`] with the available width to drop the ones that
/// don't fit.
#[derive(Debug, Clone, Copy, Default)]
pub struct Columns {
    pub artist: bool,
    pub album: bool,
    pub genre: bool,
    pub favorite: bool,
    pub rating: bool,
    pub quality: bool,
    /// Two columns: "play next" and "add to queue".
    pub queue_actions: bool,
    pub playlist: bool,
    /// One extra action column at the right edge (e.g. "remove").
    pub trailing: bool,
    /// Extra right padding beyond the base 8px — Songs reserves a gutter
    /// for the overlaid vertical scrollbar.
    pub gutter: f32,
}

impl Columns {
    /// Drop the columns that don't fit in `width` px, using the shared
    /// breakpoints.
    #[must_use]
    pub fn responsive(mut self, width: f32) -> Self {
        self.artist &= width >= ARTIST_BREAKPOINT;
        self.album &= width >= ALBUM_RATING_BREAKPOINT;
        self.rating &= width >= ALBUM_RATING_BREAKPOINT;
        self.quality &= width >= ALBUM_RATING_BREAKPOINT;
        self.queue_actions &= width >= ARTIST_BREAKPOINT;
        self.genre &= width >= GENRE_BREAKPOINT;
        self
    }

    fn padding(&self, vertical: f32) -> [f32; 4] {
        [
            vertical,
            ROW_PADDING_LEFT + self.gutter,
            vertical,
            ROW_PADDING_LEFT,
        ]
    }
}

// --- Shared styling ---------------------------------------------------------

/// Muted secondary text (artist/album/number) in the theme's dim colour.
fn dim_text_style(theme: &cosmic::Theme) -> cosmic::iced::widget::text::Style {
    cosmic::iced::widget::text::Style {
        color: Some(theme.cosmic().palette.neutral_7.into()),
        ..Default::default()
    }
}

/// Dim single-line caption.
fn dim_caption<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Custom(dim_text_style))
}

/// Column cell that claims a proportional share of the row's width and clips
/// its content to it.
fn fill_cell<'a, M: 'a>(
    portion: u16,
    content: impl Into<cosmic::Element<'a, M>>,
) -> cosmic::Element<'a, M> {
    widget::container(common::clipped_cell(content.into()))
        .width(Length::FillPortion(portion))
        .into()
}

/// Fixed-width, horizontally centred cell.
fn fixed_cell<'a, M: 'a>(
    width: f32,
    content: impl Into<cosmic::Element<'a, M>>,
) -> cosmic::Element<'a, M> {
    widget::container(content)
        .width(width)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .clip(true)
        .into()
}

fn blank<'a, M: 'a>(width: f32) -> cosmic::Element<'a, M> {
    widget::Space::new().width(width).into()
}

/// Three accent bars of different heights — a static equaliser glyph that
/// marks the currently playing track.
fn eq_glyph<'a, M: 'a>() -> cosmic::Element<'a, M> {
    let bar = |height: f32| {
        widget::container(widget::Space::new().width(3).height(height)).class(
            cosmic::theme::Container::custom(|theme| cosmic::iced::widget::container::Style {
                background: Some(Background::Color(theme.cosmic().accent_text_color().into())),
                border: Border {
                    radius: 1.5.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
    };
    widget::Row::new()
        .push(bar(8.0))
        .push(bar(14.0))
        .push(bar(10.0))
        .spacing(2)
        .align_y(Alignment::End)
        .height(14)
        .into()
}

/// Icon-button class for the row's secondary actions: dim at rest so a table
/// of tracks doesn't read as a wall of buttons, full-strength on hover.
fn quiet_button_class() -> cosmic::theme::Button {
    fn style(theme: &cosmic::Theme, hovered: Option<Color>, strong: bool) -> ButtonStyle {
        let cosmic = theme.cosmic();
        let on = cosmic.background(false).component.on;
        let color: Color = if strong {
            on.into()
        } else {
            on.with_alpha(0.55).into()
        };
        ButtonStyle {
            background: hovered.map(Background::Color),
            text_color: Some(color),
            icon_color: Some(color),
            border_radius: cosmic.corner_radii.radius_s.into(),
            ..ButtonStyle::new()
        }
    }
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| style(theme, None, false)),
        hovered: Box::new(|_focused, theme| {
            let hover = theme.cosmic().background(false).component.hover.into();
            style(theme, Some(hover), true)
        }),
        pressed: Box::new(|_focused, theme| {
            let pressed = theme.cosmic().background(false).component.pressed.into();
            style(theme, Some(pressed), true)
        }),
        disabled: Box::new(|theme| style(theme, None, false)),
    }
}

/// Quiet icon button with a caption tooltip.
fn quiet_icon_button<'a, M: Clone + 'static>(
    icon_name: &'static str,
    label: String,
    on_press: M,
) -> cosmic::Element<'a, M> {
    widget::tooltip(
        widget::button::icon(widget::icon::from_name(icon_name).size(16))
            .class(quiet_button_class())
            .on_press(on_press),
        widget::text::caption(label),
        TooltipPosition::Top,
    )
    .into()
}

/// Pill style shared by the genre cells and the Songs filter bar: a soft
/// filled capsule, accent-tinted (with a hairline accent border) when
/// `active`.
pub fn pill_class(active: bool) -> cosmic::theme::Button {
    fn style(theme: &cosmic::Theme, active: bool, level: u8) -> ButtonStyle {
        let cosmic = theme.cosmic();
        let comp = &cosmic.background(false).component;
        let radius = cosmic.corner_radii.radius_xl;
        if active {
            let accent: Color = cosmic.accent_text_color().into();
            let alpha = [0.14, 0.22, 0.30][usize::from(level)];
            ButtonStyle {
                background: Some(Background::Color(Color { a: alpha, ..accent })),
                text_color: Some(accent),
                icon_color: Some(accent),
                border_radius: radius.into(),
                border_width: 1.0,
                border_color: Color { a: 0.35, ..accent },
                ..ButtonStyle::new()
            }
        } else {
            let background: Color = match level {
                0 => comp.base.into(),
                1 => comp.hover.into(),
                _ => comp.pressed.into(),
            };
            let text: Color = if level == 0 {
                theme.cosmic().palette.neutral_8.into()
            } else {
                comp.on.into()
            };
            ButtonStyle {
                background: Some(Background::Color(background)),
                text_color: Some(text),
                icon_color: Some(text),
                border_radius: radius.into(),
                border_width: 1.0,
                border_color: comp.divider.with_alpha(0.5).into(),
                ..ButtonStyle::new()
            }
        }
    }
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| style(theme, active, 0)),
        hovered: Box::new(move |_focused, theme| style(theme, active, 1)),
        pressed: Box::new(move |_focused, theme| style(theme, active, 2)),
        disabled: Box::new(move |theme| style(theme, active, 0)),
    }
}

// --- Row --------------------------------------------------------------------

type Navigate<M> = fn(Route) -> M;

/// Builds the add-to-playlist message from `(source_uri, playlist_id)`.
type AddToPlaylistFn<M> = fn(String, String) -> M;

/// Description of one track row. Create with [`TrackRow::new`], enable the
/// actions the host view supports with the `with_*` methods, then call
/// [`TrackRow::view`]. A column whose [`Columns`] flag is set but whose action
/// is not provided renders as empty space of the right width.
pub struct TrackRow<'a, M> {
    track: &'a Track,
    number: String,
    is_playing: bool,
    columns: Columns,
    on_press: M,
    navigate: Option<Navigate<M>>,
    on_favorite: Option<M>,
    on_rate: Option<Box<dyn Fn(u8) -> M + 'a>>,
    on_genre: Option<M>,
    on_play_next: Option<M>,
    on_add_to_queue: Option<M>,
    add_to_playlist: Option<(&'a [Playlist], AddToPlaylistFn<M>)>,
    trailing: Option<(&'static str, String, M)>,
    artist_subtitle: bool,
}

impl<'a, M: Clone + 'static> TrackRow<'a, M> {
    /// `number` is the text shown in the leading column when the track is not
    /// playing (position or track number); `on_press` plays the track.
    pub fn new(
        track: &'a Track,
        number: impl Into<String>,
        is_playing: bool,
        columns: Columns,
        on_press: M,
    ) -> Self {
        Self {
            track,
            number: number.into(),
            is_playing,
            columns,
            on_press,
            navigate: None,
            on_favorite: None,
            on_rate: None,
            on_genre: None,
            on_play_next: None,
            on_add_to_queue: None,
            add_to_playlist: None,
            trailing: None,
            artist_subtitle: false,
        }
    }

    /// Make artist / album names links that emit `navigate(route)`.
    #[must_use]
    pub fn with_navigate(mut self, navigate: Navigate<M>) -> Self {
        self.navigate = Some(navigate);
        self
    }

    /// Favorite heart; `on_toggle` is the message for a click.
    #[must_use]
    pub fn with_favorite(mut self, on_toggle: M) -> Self {
        self.on_favorite = Some(on_toggle);
        self
    }

    /// Star rating; `on_rate(r)` is emitted with `r` in 0..=5 (0 clears).
    #[must_use]
    pub fn with_rating(mut self, on_rate: impl Fn(u8) -> M + 'a) -> Self {
        self.on_rate = Some(Box::new(on_rate));
        self
    }

    /// Clickable genre pill.
    #[must_use]
    pub fn with_genre_filter(mut self, on_genre: M) -> Self {
        self.on_genre = Some(on_genre);
        self
    }

    /// "Play next" and "Add to queue" icon actions.
    #[must_use]
    pub fn with_queue_actions(mut self, play_next: M, add_to_queue: M) -> Self {
        self.on_play_next = Some(play_next);
        self.on_add_to_queue = Some(add_to_queue);
        self
    }

    /// "Add to <first playlist>" icon action.
    #[must_use]
    pub fn with_add_to_playlist(
        mut self,
        playlists: &'a [Playlist],
        make_message: fn(String, String) -> M,
    ) -> Self {
        self.add_to_playlist = Some((playlists, make_message));
        self
    }

    /// One extra icon action in the trailing column (e.g. remove).
    #[must_use]
    pub fn with_trailing_action(
        mut self,
        icon_name: &'static str,
        label: String,
        on_press: M,
    ) -> Self {
        self.trailing = Some((icon_name, label, on_press));
        self
    }

    /// Show the artist as a caption under the title (used when there is no
    /// artist column, or the artist is notable for this row).
    #[must_use]
    pub fn with_artist_subtitle(mut self, show: bool) -> Self {
        self.artist_subtitle = show;
        self
    }

    fn secondary_link(
        &self,
        text: &'a str,
        route: impl FnOnce() -> Route,
    ) -> cosmic::Element<'a, M> {
        match self.navigate {
            Some(navigate) => common::link_cell(text, true, || navigate(route())),
            None => common::cell_text(text)
                .class(cosmic::theme::Text::Custom(dim_text_style))
                .into(),
        }
    }

    fn title_cell(&self) -> cosmic::Element<'a, M> {
        let track = self.track;
        let mut title = common::cell_text(track.title.as_str());
        if self.is_playing {
            title = title.font(cosmic::font::semibold());
        }
        if self.artist_subtitle && !track.artist.trim().is_empty() {
            let subtitle: cosmic::Element<'a, M> = match self.navigate {
                Some(navigate) => common::link(
                    common::cell_caption(track.artist.as_str()),
                    true,
                    navigate(Route::Artist(track.artist.clone())),
                ),
                None => dim_caption(track.artist.as_str()).into(),
            };
            fill_cell(
                4,
                widget::Column::new().push(title).push(subtitle).spacing(1),
            )
        } else {
            fill_cell(4, title)
        }
    }

    /// Build the row element.
    pub fn view(self) -> cosmic::Element<'a, M> {
        let track = self.track;
        let c = self.columns;

        let number: cosmic::Element<'a, M> = if self.is_playing {
            eq_glyph()
        } else {
            dim_caption(self.number.clone()).into()
        };

        let mut row = widget::Row::new()
            // Load-bearing: without a non-Shrink width the FillPortion cells
            // collapse to their content (see module docs).
            .width(Length::Fill)
            .height(Length::Fill)
            .spacing(COLUMN_SPACING)
            .align_y(Alignment::Center)
            .push(fixed_cell(NUM_WIDTH, number))
            .push(self.title_cell());

        if c.artist {
            row = row.push(fill_cell(
                3,
                self.secondary_link(track.artist.as_str(), || {
                    Route::Artist(track.artist.clone())
                }),
            ));
        }
        if c.album {
            row = row.push(fill_cell(
                3,
                self.secondary_link(track.album.as_str(), || Route::album_of(track)),
            ));
        }

        if c.genre {
            let cell: cosmic::Element<'a, M> = match (&self.on_genre, track.genre.is_empty()) {
                (Some(on_genre), false) => widget::container(common::clipped_cell(
                    widget::button::custom(common::cell_caption(track.genre.as_str()))
                        .padding([2, 10])
                        .on_press(on_genre.clone())
                        .class(pill_class(false))
                        .into(),
                ))
                .width(GENRE_WIDTH)
                .into(),
                _ => blank(GENRE_WIDTH),
            };
            row = row.push(cell);
        }

        if c.favorite {
            row = row.push(fixed_cell(
                ICON_WIDTH,
                match &self.on_favorite {
                    Some(toggle) => common::favorite_button(track.is_favorite, toggle.clone()),
                    None => blank(ICON_WIDTH),
                },
            ));
        }

        if c.rating {
            row = row.push(fixed_cell(
                RATING_WIDTH,
                match &self.on_rate {
                    Some(on_rate) => common::star_rating(track.rating, on_rate),
                    None => blank(RATING_WIDTH),
                },
            ));
        }

        if c.quality {
            row = row.push(fixed_cell(
                common::QUALITY_BADGE_WIDTH,
                common::quality_badge(crate::library::quality::classify(
                    &track.path,
                    track.sample_rate,
                    track.bitrate,
                )),
            ));
        }

        if c.queue_actions {
            row = row
                .push(fixed_cell(
                    ICON_WIDTH,
                    match &self.on_play_next {
                        Some(msg) => quiet_icon_button(
                            "go-next-symbolic",
                            fl!("queue-play-next"),
                            msg.clone(),
                        ),
                        None => blank(ICON_WIDTH),
                    },
                ))
                .push(fixed_cell(
                    ICON_WIDTH,
                    match &self.on_add_to_queue {
                        Some(msg) => quiet_icon_button(
                            "view-list-ordered-symbolic",
                            fl!("queue-add"),
                            msg.clone(),
                        ),
                        None => blank(ICON_WIDTH),
                    },
                ));
        }

        if c.playlist {
            let cell: cosmic::Element<'a, M> = match self.add_to_playlist {
                Some((playlists, make_message)) => match playlists.first() {
                    Some(playlist) => quiet_icon_button(
                        "list-add-symbolic",
                        fl!("songs-add-to-playlist", playlist = playlist.name.as_str()),
                        make_message(track.source_uri.clone(), playlist.id.clone()),
                    ),
                    None => blank(ICON_WIDTH),
                },
                None => blank(ICON_WIDTH),
            };
            row = row.push(fixed_cell(ICON_WIDTH, cell));
        }

        if c.trailing {
            row = row.push(fixed_cell(
                ICON_WIDTH,
                match &self.trailing {
                    Some((icon_name, label, msg)) => {
                        quiet_icon_button(icon_name, label.clone(), msg.clone())
                    }
                    None => blank(ICON_WIDTH),
                },
            ));
        }

        row = row.push(common::duration_cell(track.duration.as_secs()));

        widget::button::custom(row.padding(c.padding(4.0)))
            .on_press(self.on_press)
            .width(Length::Fill)
            // Uniform height is load-bearing for Songs' virtualization.
            .height(Length::Fixed(ROW_HEIGHT))
            // The row carries its own padding; don't let the button add more.
            .padding(0)
            .class(list_row_button_class(self.is_playing))
            .into()
    }
}

// --- Header -----------------------------------------------------------------

/// One header label, optionally a sort toggle.
#[derive(Debug, Clone)]
pub struct HeaderCell<M> {
    label: String,
    on_sort: Option<M>,
    /// `Some(descending)` when this is the active sort column.
    sorted: Option<bool>,
}

impl<M> HeaderCell<M> {
    fn plain(label: String) -> Self {
        Self {
            label,
            on_sort: None,
            sorted: None,
        }
    }
}

/// Which header cell [`Header::sortable`] applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderColumn {
    Title,
    Artist,
    Album,
    Duration,
}

/// Column header matching a [`Columns`] set. Starts out with plain dim
/// labels; make columns sortable with [`Header::sortable`].
#[derive(Debug, Clone)]
pub struct Header<M> {
    columns: Columns,
    title: HeaderCell<M>,
    artist: HeaderCell<M>,
    album: HeaderCell<M>,
    duration: HeaderCell<M>,
}

impl<M: Clone + 'static> Header<M> {
    pub fn new(columns: Columns) -> Self {
        Self {
            columns,
            title: HeaderCell::plain(fl!("songs-column-title")),
            artist: HeaderCell::plain(fl!("songs-column-artist")),
            album: HeaderCell::plain(fl!("songs-column-album")),
            duration: HeaderCell::plain(fl!("songs-column-duration")),
        }
    }

    /// Turn `column`'s label into a sort toggle emitting `on_sort`.
    /// `sorted` is `Some(descending)` when it is the active sort column.
    #[must_use]
    pub fn sortable(mut self, column: HeaderColumn, on_sort: M, sorted: Option<bool>) -> Self {
        let cell = match column {
            HeaderColumn::Title => &mut self.title,
            HeaderColumn::Artist => &mut self.artist,
            HeaderColumn::Album => &mut self.album,
            HeaderColumn::Duration => &mut self.duration,
        };
        cell.on_sort = Some(on_sort);
        cell.sorted = sorted;
        self
    }

    /// Build the header element ([`HEADER_HEIGHT`] tall, with a hairline
    /// divider beneath it).
    pub fn view<'a>(self) -> cosmic::Element<'a, M> {
        let c = self.columns;
        let mut row = widget::Row::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .spacing(COLUMN_SPACING)
            .align_y(Alignment::Center)
            .push(fixed_cell(
                NUM_WIDTH,
                header_label(fl!("songs-column-number"), false),
            ))
            .push(fill_cell(4, header_cell(self.title, false)));

        if c.artist {
            row = row.push(fill_cell(3, header_cell(self.artist, false)));
        }
        if c.album {
            row = row.push(fill_cell(3, header_cell(self.album, false)));
        }
        if c.genre {
            row = row.push(
                widget::container(header_label(fl!("songs-column-genre"), false))
                    .width(GENRE_WIDTH)
                    .clip(true),
            );
        }
        if c.favorite {
            row = row.push(blank(ICON_WIDTH));
        }
        if c.rating {
            row = row.push(
                widget::container(header_label(fl!("songs-column-rating"), false))
                    .width(RATING_WIDTH)
                    .align_x(Horizontal::Center)
                    .clip(true),
            );
        }
        if c.quality {
            row = row.push(blank(common::QUALITY_BADGE_WIDTH));
        }
        if c.queue_actions {
            row = row.push(blank(ICON_WIDTH)).push(blank(ICON_WIDTH));
        }
        if c.playlist {
            row = row.push(blank(ICON_WIDTH));
        }
        if c.trailing {
            row = row.push(blank(ICON_WIDTH));
        }
        row = row.push(
            widget::container(header_cell(self.duration, true))
                .width(common::DURATION_WIDTH)
                .align_x(Horizontal::Right)
                // A long localized label plus its sort arrow must never
                // paint past this fixed column.
                .clip(true),
        );

        widget::Column::new()
            .push(
                widget::container(row.padding(c.padding(0.0)))
                    .width(Length::Fill)
                    .height(Length::Fixed(HEADER_HEIGHT)),
            )
            .push(widget::divider::horizontal::default())
            .into()
    }
}

/// Dim, semibold-when-active caption used for every header label.
fn header_text<'a>(label: String, active: bool) -> common::Text<'a> {
    let text = common::cell_caption(label);
    if active {
        text.font(cosmic::font::semibold())
    } else {
        text.font(cosmic::font::semibold())
            .class(cosmic::theme::Text::Custom(dim_text_style))
    }
}

fn header_label<'a, M: 'a>(label: String, active: bool) -> cosmic::Element<'a, M> {
    header_text(label, active).into()
}

/// Header-looking sort toggle: no fill, no border — dim caption text that
/// brightens on hover, with an arrow on the active column.
fn header_button_class(active: bool) -> cosmic::theme::Button {
    fn style(theme: &cosmic::Theme, bright: bool) -> ButtonStyle {
        let cosmic = theme.cosmic();
        let color: Color = if bright {
            cosmic.background(false).component.on.into()
        } else {
            cosmic.palette.neutral_7.into()
        };
        ButtonStyle {
            background: None,
            text_color: Some(color),
            icon_color: Some(color),
            border_radius: cosmic.corner_radii.radius_xs.into(),
            ..ButtonStyle::new()
        }
    }
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| style(theme, active)),
        hovered: Box::new(|_focused, theme| style(theme, true)),
        pressed: Box::new(|_focused, theme| style(theme, true)),
        disabled: Box::new(move |theme| style(theme, active)),
    }
}

fn header_cell<'a, M: Clone + 'static>(
    cell: HeaderCell<M>,
    arrow_first: bool,
) -> cosmic::Element<'a, M> {
    let Some(on_sort) = cell.on_sort else {
        return header_label(cell.label, false);
    };
    let active = cell.sorted.is_some();
    let label: cosmic::Element<'a, M> = widget::text::caption(cell.label)
        .font(cosmic::font::semibold())
        .wrapping(cosmic::iced::core::text::Wrapping::None)
        .into();
    let mut content = widget::Row::new().spacing(4).align_y(Alignment::Center);
    let arrow = cell.sorted.map(|descending| {
        widget::icon::from_name(if descending {
            "go-down-symbolic"
        } else {
            "go-up-symbolic"
        })
        .size(12)
    });
    if arrow_first {
        if let Some(arrow) = arrow {
            content = content.push(arrow);
        }
        content = content.push(label);
    } else {
        content = content.push(label);
        if let Some(arrow) = arrow {
            content = content.push(arrow);
        }
    }
    widget::button::custom(content)
        .padding([2, 0])
        .on_press(on_sort)
        .class(header_button_class(active))
        .into()
}

// --- Width-aware lists -------------------------------------------------------

/// Wrap a non-scrolling track table so it learns the width it is laid out in
/// (to pick its [`Columns`]) even though it sits inside a vertical
/// scrollable. `build(width)` must return a table of exactly `row_count` rows
/// (plus a [`Header`] when `with_header`), each [`ROW_HEIGHT`] apart by
/// [`ROW_GAP`] — see [`rows_column`].
pub fn width_aware<'a, M: 'a>(
    row_count: usize,
    with_header: bool,
    build: impl Fn(f32) -> cosmic::Element<'a, M> + 'a,
) -> cosmic::Element<'a, M> {
    let rows = row_count as f32 * ROW_STRIDE;
    let header = if with_header {
        HEADER_HEIGHT + 1.0 + HEADER_GAP
    } else {
        0.0
    };
    widget::container(widget::responsive(move |size: Size| build(size.width)))
        .width(Length::Fill)
        // A couple of px of slack: a clipped last row is worse than a blank
        // gutter.
        .height(Length::Fixed(rows + header + 2.0))
        .into()
}

/// Gap between the header divider and the first row.
const HEADER_GAP: f32 = 4.0;

/// Stack rows with the shared gap, optionally under a header.
pub fn rows_column<'a, M: Clone + 'static>(
    header: Option<Header<M>>,
    rows: impl IntoIterator<Item = cosmic::Element<'a, M>>,
) -> cosmic::Element<'a, M> {
    let mut list = widget::Column::new().spacing(ROW_GAP);
    for row in rows {
        list = list.push(row);
    }
    match header {
        Some(header) => widget::Column::new()
            .push(header.view())
            .push(widget::Space::new().height(HEADER_GAP))
            .push(list)
            .into(),
        None => list.into(),
    }
}
