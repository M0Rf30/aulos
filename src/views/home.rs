// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Home page: a time-of-day greeting over horizontally scrolling shelves of
//! suggested albums and artists, driven by the play history (see
//! `crate::library::history`).
//!
//! Cards are fully self-contained (`AlbumRef` carries the identity, year and
//! artist), so the view never indexes into the album list; the app resolves
//! an album by `(artist, name)` only when a card is clicked.

mod similar;

use crate::fl;
use crate::library::CoverArt;
use crate::library::history::{AlbumRef, ArtistRef, DecadeCount, HomeData, PlayTracker};
use crate::views::{Route, card_button_class, common};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::widget::Stack;
use cosmic::iced::{Alignment, Color, Length};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Messages emitted by (and for) the Home page.
#[derive(Debug, Clone)]
pub enum HomeMessage {
    /// Open an album's detail page.
    OpenAlbum { artist: String, name: String },
    /// Play an album from its first track.
    PlayAlbum { artist: String, name: String },
    /// Open an artist's detail page.
    OpenArtist(String),
    /// Play everything by an artist.
    PlayArtist(String),
    /// Re-roll just the "Random picks" shelf.
    ShuffleRandom,
    /// Reload every shelf.
    Refresh,
    /// Open the filtered album grid for a decade (`1980`).
    OpenDecade(u32),
    /// Leave the decade grid.
    CloseDecade,
    /// Shuffle-play every album of the open decade.
    PlayDecade,
    /// Welcome block: pick a music folder.
    AddMusicDir,
    /// Welcome block: open the Providers drawer.
    ConnectServer,
    /// Welcome block: open the Settings drawer.
    OpenSettings,
    /// Welcome block: go to the Radio page.
    BrowseRadio,
    /// Welcome block: go to the Podcasts page.
    DiscoverPodcasts,
    /// Open a web page (a "discover" card).
    OpenUrl(String),

    // -- Async results (never emitted by the view) --
    /// Shelves loaded; `request` ties the result to the latest load. With
    /// `keep_picks` the previous random/rediscover shelves are kept, so
    /// returning to Home doesn't reshuffle what the user just saw.
    Loaded {
        request: u64,
        keep_picks: bool,
        data: Box<HomeData>,
    },
    /// A "Because you listened to …" shelf is ready; `request` ties it to
    /// the Home load it was computed for.
    SimilarShelf {
        request: u64,
        shelf: Box<crate::online::similar::SimilarShelf>,
    },
    /// All similar-artist shelves of `request` were delivered (`count` of
    /// them) or the lookup gave up (offline).
    SimilarDone { request: u64, count: usize },
    /// A play was written to the history (no-op acknowledgement).
    PlayRecorded,
    /// A re-rolled random shelf.
    RandomLoaded { request: u64, albums: Vec<AlbumRef> },
    /// Albums of a decade.
    DecadeLoaded { decade: u32, albums: Vec<AlbumRef> },
}

/// The decade grid currently open on the Home page.
#[derive(Debug, Clone)]
pub struct DecadeView {
    pub decade: u32,
    /// `None` while the albums are still loading.
    pub albums: Option<Vec<AlbumRef>>,
}

/// Home page state, owned by the app.
#[derive(Debug, Default)]
pub struct HomeState {
    /// Loaded shelves; `None` until the first load completes.
    pub data: Option<HomeData>,
    /// Bumped on every load so a slow, superseded result is dropped.
    pub request: u64,
    /// The decade grid, when one is open.
    pub decade: Option<DecadeView>,
    /// Listening-time accumulator for the current track.
    pub tracker: PlayTracker,
    /// Track id `tracker` is currently following.
    pub tracked_track: Option<i64>,
    /// "Because you listened to …" shelves, loaded after the rest.
    pub similar: Vec<crate::online::similar::SimilarShelf>,
    /// The Home load `similar` belongs to.
    pub similar_request: u64,
    /// Stops the in-flight similar-artist lookup when a new one starts.
    pub similar_cancel: Arc<AtomicBool>,
}

/// Cheap, per-frame facts about the library the Home view needs.
#[derive(Debug, Clone, Copy)]
pub struct HomeContext {
    /// Albums in the active provider's library.
    pub library_albums: usize,
    /// Whether a library scan/load is running.
    pub scanning: bool,
    /// Whether any music folder is configured (marks step 1 done).
    pub has_music_dirs: bool,
    /// Whether any MPD/Subsonic server is configured (marks step 2 done).
    pub has_servers: bool,
}

/// Cover/avatar size inside a shelf card.
const CARD_ART: f32 = 160.0;
const CARD_PADDING: f32 = 8.0;
/// Two-line label block (body + caption), see `albums.rs`.
const CARD_LABEL_HEIGHT: f32 = 40.0;
/// Clearance for the page's vertical scrollbar.
const SCROLLBAR_CLEARANCE: f32 = 16.0;

/// Which greeting suits an hour of the day (0–23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Greeting {
    Morning,
    Afternoon,
    Evening,
}

impl Greeting {
    pub fn for_hour(hour: u32) -> Self {
        match hour {
            5..=11 => Self::Morning,
            12..=17 => Self::Afternoon,
            _ => Self::Evening,
        }
    }

    fn text(self) -> String {
        match self {
            Self::Morning => fl!("home-greeting-morning"),
            Self::Afternoon => fl!("home-greeting-afternoon"),
            Self::Evening => fl!("home-greeting-evening"),
        }
    }
}

/// Label for a decade chip/title, e.g. `1980` → `"1980s"`.
pub fn decade_label(decade: u32) -> String {
    format!("{decade}s")
}

// ---------------------------------------------------------------------
// Styling helpers
// ---------------------------------------------------------------------

fn dim_text() -> cosmic::theme::Text {
    cosmic::theme::Text::Custom(|theme| cosmic::iced::widget::text::Style {
        color: Some(theme.cosmic().palette.neutral_7.into()),
        ..Default::default()
    })
}

fn secondary_caption<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    common::cell_caption(content).class(dim_text())
}

/// Accent-filled round play button that floats over card artwork.
fn play_button_class() -> cosmic::theme::Button {
    fn style(theme: &cosmic::Theme, alpha: f32) -> ButtonStyle {
        let cosmic = theme.cosmic();
        let accent: Color = cosmic.accent_color().into();
        let on: Color = cosmic.accent.on.into();
        ButtonStyle {
            background: Some(Background::Color(Color { a: alpha, ..accent })),
            text_color: Some(on),
            icon_color: Some(on),
            border_radius: [99.0; 4].into(),
            ..ButtonStyle::new()
        }
    }
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| style(theme, 0.95)),
        hovered: Box::new(|_focused, theme| style(theme, 1.0)),
        pressed: Box::new(|_focused, theme| style(theme, 0.8)),
        disabled: Box::new(|theme| style(theme, 0.4)),
    }
}

/// Decade chip: a pill on a subtle surface, brightening on hover.
fn chip_class() -> cosmic::theme::Button {
    fn style(theme: &cosmic::Theme, state: u8) -> ButtonStyle {
        let cosmic = theme.cosmic();
        let comp = &cosmic.background(false).component;
        let background = match state {
            0 => comp.base,
            1 => comp.hover,
            _ => comp.pressed,
        };
        ButtonStyle {
            background: Some(Background::Color(background.into())),
            text_color: Some(comp.on.into()),
            icon_color: Some(comp.on.into()),
            border_radius: cosmic.corner_radii.radius_xl.into(),
            ..ButtonStyle::new()
        }
    }
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| style(theme, 0)),
        hovered: Box::new(|_focused, theme| style(theme, 1)),
        pressed: Box::new(|_focused, theme| style(theme, 2)),
        disabled: Box::new(|theme| style(theme, 0)),
    }
}

/// `art` with an accent play button floating over its bottom-right corner.
fn with_play_overlay<'a>(
    art: cosmic::Element<'a, HomeMessage>,
    size: f32,
    on_play: HomeMessage,
    tooltip: String,
) -> cosmic::Element<'a, HomeMessage> {
    let disc = widget::container(widget::icon::from_name("media-playback-start-symbolic").size(18))
        .padding(9);
    let button = widget::tooltip(
        widget::button::custom(disc)
            .padding(0)
            .on_press(on_play)
            .class(play_button_class()),
        widget::text::caption(tooltip),
        widget::tooltip::Position::Top,
    );
    Stack::new()
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .push(art)
        .push(
            widget::container(button)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(8)
                .align_x(Horizontal::Right)
                .align_y(Vertical::Bottom),
        )
        .into()
}

// ---------------------------------------------------------------------
// Cards
// ---------------------------------------------------------------------

/// Album card at outer width `outer` (card button padding included).
fn album_card<'a>(
    album: &'a AlbumRef,
    outer: f32,
    covers: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, HomeMessage> {
    let art_size = outer - 2.0 * CARD_PADDING;
    let key = CoverArt::album_key(&album.artist, &album.name);
    let tile = common::grid_art_tile(covers.get(&key), art_size as u16, "media-optical-symbolic");
    let art = with_play_overlay(
        tile,
        art_size,
        HomeMessage::PlayAlbum {
            artist: album.artist.clone(),
            name: album.name.clone(),
        },
        fl!("play-album"),
    );

    let title: cosmic::Element<'a, HomeMessage> = if album.name.trim().is_empty() {
        secondary_caption(fl!("unknown-album")).into()
    } else {
        common::cell_text(album.name.as_str())
            .font(cosmic::font::semibold())
            .into()
    };
    let has_artist = !album.artist.trim().is_empty()
        && !album.artist.trim().eq_ignore_ascii_case(album.name.trim());
    let subtitle: cosmic::Element<'a, HomeMessage> = if has_artist {
        common::link(
            secondary_caption(album.artist.as_str()),
            true,
            HomeMessage::OpenArtist(album.artist.clone()),
        )
    } else {
        secondary_caption("\u{a0}").into()
    };
    let label = common::grid_card_label(
        art_size,
        CARD_LABEL_HEIGHT,
        common::clipped_cell(title),
        common::clipped_cell(subtitle),
    );

    let tooltip = if has_artist {
        fl!(
            "album-tooltip",
            title = album.name.clone(),
            artist = album.artist.clone()
        )
    } else {
        album.name.clone()
    };
    widget::tooltip(
        widget::button::custom(common::grid_card(art, art_size, label))
            .on_press(HomeMessage::OpenAlbum {
                artist: album.artist.clone(),
                name: album.name.clone(),
            })
            .padding(CARD_PADDING as u16)
            .class(card_button_class()),
        widget::text::caption(tooltip),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// Artist card: round avatar, name and play count.
fn artist_card<'a>(
    artist: &'a ArtistRef,
    photos: &'a HashMap<String, widget::image::Handle>,
) -> cosmic::Element<'a, HomeMessage> {
    let art_size = CARD_ART;
    let avatar = common::artist_avatar(&artist.name, photos.get(&artist.name), art_size);
    let art = with_play_overlay(
        avatar,
        art_size,
        HomeMessage::PlayArtist(artist.name.clone()),
        fl!("home-play-artist"),
    );
    let label = common::grid_card_label(
        art_size,
        CARD_LABEL_HEIGHT,
        common::clipped_cell(
            common::cell_text(artist.name.as_str())
                .font(cosmic::font::semibold())
                .into(),
        ),
        common::clipped_cell(secondary_caption(fl!("home-plays", count = artist.plays)).into()),
    );
    widget::button::custom(common::grid_card(art, art_size, label))
        .on_press(HomeMessage::OpenArtist(artist.name.clone()))
        .padding(CARD_PADDING as u16)
        .class(card_button_class())
        .into()
}

// ---------------------------------------------------------------------
// Layout pieces
// ---------------------------------------------------------------------

/// A titled, horizontally scrolling row of cards with an optional action
/// (e.g. the shuffle button) at the end of the title line.
fn shelf<'a>(
    title: String,
    hint: Option<String>,
    action: Option<cosmic::Element<'a, HomeMessage>>,
    cards: Vec<cosmic::Element<'a, HomeMessage>>,
) -> cosmic::Element<'a, HomeMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let mut titles = widget::Column::new().push(widget::text::title4(title));
    if let Some(hint) = hint {
        titles = titles.push(widget::text::caption(hint).class(dim_text()));
    }
    let mut header = widget::Row::new()
        .push(titles)
        .push(widget::space::horizontal())
        .align_y(Alignment::Center)
        .padding([0, spacing.space_m]);
    if let Some(action) = action {
        header = header.push(action);
    }

    let row = widget::Row::with_children(cards)
        .spacing(spacing.space_xxs)
        .padding([0, spacing.space_s, spacing.space_s, spacing.space_s]);

    widget::Column::new()
        .push(header)
        .push(widget::scrollable::horizontal(row))
        .spacing(spacing.space_xxs)
        .into()
}

fn album_shelf<'a>(
    title: String,
    hint: Option<String>,
    action: Option<cosmic::Element<'a, HomeMessage>>,
    albums: &'a [AlbumRef],
    covers: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, HomeMessage> {
    let cards = albums
        .iter()
        .map(|album| album_card(album, CARD_ART + 2.0 * CARD_PADDING, covers))
        .collect();
    shelf(title, hint, action, cards)
}

fn decade_chips<'a>(decades: &'a [DecadeCount]) -> cosmic::Element<'a, HomeMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let chips: Vec<cosmic::Element<'a, HomeMessage>> = decades
        .iter()
        .map(|d| {
            let label = widget::Row::new()
                .push(widget::text::body(decade_label(d.decade)).font(cosmic::font::semibold()))
                .push(widget::text::caption(d.albums.to_string()).class(dim_text()))
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center);
            widget::button::custom(label)
                .padding([spacing.space_xs, spacing.space_m])
                .on_press(HomeMessage::OpenDecade(d.decade))
                .class(chip_class())
                .into()
        })
        .collect();

    widget::Column::new()
        .push(
            widget::container(widget::text::title4(fl!("home-browse-decades")))
                .padding([0, spacing.space_m]),
        )
        .push(
            widget::container(
                widget::flex_row(chips)
                    .row_spacing(spacing.space_xs)
                    .column_spacing(spacing.space_xs),
            )
            .padding([0, spacing.space_m]),
        )
        .spacing(spacing.space_xs)
        .into()
}

/// Greeting header with the library summary and a refresh button.
fn header<'a>(ctx: HomeContext) -> cosmic::Element<'a, HomeMessage> {
    use chrono::Timelike;
    let spacing = cosmic::theme::active().cosmic().spacing;
    let greeting = Greeting::for_hour(chrono::Local::now().hour());

    let mut titles = widget::Column::new()
        .push(widget::text::title1(greeting.text()))
        .spacing(spacing.space_xxs);
    if ctx.library_albums > 0 {
        titles = titles.push(
            widget::text::body(fl!("home-subtitle", count = (ctx.library_albums as u32)))
                .class(dim_text()),
        );
    }

    widget::Row::new()
        .push(titles)
        .push(widget::space::horizontal())
        .push(widget::tooltip(
            widget::button::icon(widget::icon::from_name("view-refresh-symbolic").size(16))
                .on_press(HomeMessage::Refresh),
            widget::text::caption(fl!("home-refresh")),
            widget::tooltip::Position::Bottom,
        ))
        .align_y(Alignment::Center)
        .padding([
            spacing.space_m,
            spacing.space_m,
            spacing.space_xs,
            spacing.space_m,
        ])
        .into()
}

/// Application icon shown (small, static) at the top of the welcome state.
const APP_ICON: &[u8] =
    include_bytes!("../../resources/icons/hicolor/scalable/apps/io.github.m0rf30.Aulos.svg");

/// Width and height of a "Get started" step card. Fixed so the fluid row
/// wraps cleanly and every card has the same footprint.
const STEP_WIDTH: f32 = 264.0;

/// Numbered accent badge of a step card; a green check once it is done.
fn step_badge<'a>(number: u8, done: bool) -> cosmic::Element<'a, HomeMessage> {
    let content: cosmic::Element<'a, HomeMessage> = if done {
        widget::icon::from_name("object-select-symbolic")
            .size(16)
            .into()
    } else {
        widget::text::body(number.to_string())
            .font(cosmic::font::semibold())
            .class(cosmic::theme::Text::Custom(|theme| {
                cosmic::iced::widget::text::Style {
                    color: Some(theme.cosmic().accent.on.into()),
                    ..Default::default()
                }
            }))
            .into()
    };
    widget::container(content)
        .width(Length::Fixed(28.0))
        .height(Length::Fixed(28.0))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let (background, on): (Color, Color) = if done {
                (cosmic.success_color().into(), cosmic.success.on.into())
            } else {
                (cosmic.accent_color().into(), cosmic.accent.on.into())
            };
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(background)),
                icon_color: Some(on),
                border: cosmic::iced::Border {
                    radius: 99.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

/// One "Get started" card: badge + icon, title, explanation and actions.
/// A finished step is rendered quieter (dimmed title, check badge).
fn step_card<'a>(
    number: u8,
    icon_name: &'static str,
    title: String,
    body: String,
    done: bool,
    actions: Vec<cosmic::Element<'a, HomeMessage>>,
) -> cosmic::Element<'a, HomeMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let top = widget::Row::new()
        .push(step_badge(number, done))
        .push(widget::space::horizontal())
        .push(
            widget::icon::icon(widget::icon::from_name(icon_name).handle())
                .size(24)
                .class(cosmic::theme::Svg::custom(|theme| {
                    cosmic::iced::widget::svg::Style {
                        color: Some(theme.cosmic().palette.neutral_7.into()),
                    }
                })),
        )
        .align_y(Alignment::Center);

    let title = widget::text::title4(title);
    let title = if done { title.class(dim_text()) } else { title };

    let text = widget::Column::new()
        .push(title)
        .push(widget::text::body(body).class(dim_text()))
        .spacing(spacing.space_xxs);

    // Height follows the content (a fixed height clipped the longer
    // descriptions and the action buttons); the row aligns cards to the top.
    widget::container(
        widget::Column::new()
            .push(top)
            .push(text)
            .push(
                widget::flex_row(actions)
                    .row_spacing(spacing.space_xxs)
                    .column_spacing(spacing.space_xs),
            )
            .spacing(spacing.space_m),
    )
    .padding(spacing.space_m)
    .width(Length::Fixed(STEP_WIDTH))
    .class(cosmic::theme::Container::Card)
    .into()
}

/// Footer tip: a key-cap chip next to a short explanation.
fn tip<'a>(keys: Option<&'static str>, text: String) -> cosmic::Element<'a, HomeMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let cap: cosmic::Element<'a, HomeMessage> = match keys {
        Some(keys) => widget::container(widget::text::caption(keys).font(cosmic::font::semibold()))
            .padding([2, spacing.space_xs])
            .class(cosmic::theme::Container::Card)
            .into(),
        None => widget::icon::from_name("input-mouse-symbolic")
            .size(16)
            .into(),
    };
    widget::Row::new()
        .push(cap)
        .push(widget::text::caption(text).class(dim_text()))
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center)
        .into()
}

/// Onboarding for a fresh install / empty library: app icon, welcome text,
/// a numbered "Get started" guide and a few tips — with a scanning banner
/// above the steps while a scan is running.
fn welcome_view<'a>(ctx: HomeContext) -> cosmic::Element<'a, HomeMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let italic = cosmic::iced::Font {
        style: cosmic::iced::font::Style::Italic,
        ..cosmic::font::default()
    };

    let intro = widget::Column::new()
        .push(widget::icon::icon(widget::icon::from_svg_bytes(APP_ICON)).size(96))
        .push(widget::text::title1(fl!("home-welcome-title")))
        .push(
            widget::text::body(fl!("app-motto"))
                .font(italic)
                .class(dim_text()),
        )
        .push(
            widget::text::body(fl!("home-welcome-intro"))
                .align_x(Horizontal::Center)
                .class(dim_text()),
        )
        .align_x(Alignment::Center)
        .spacing(spacing.space_xs);

    let mut col = widget::Column::new()
        .push(intro)
        .align_x(Alignment::Center)
        .spacing(spacing.space_l)
        .max_width(1120.0);

    if ctx.scanning {
        col = col.push(
            widget::container(
                widget::Row::new()
                    .push(widget::indeterminate_circular().size(28.0).bar_height(3.0))
                    .push(
                        widget::Column::new()
                            .push(widget::text::title4(fl!("home-scanning-title")))
                            .push(widget::text::body(fl!("home-scanning-hint")).class(dim_text()))
                            .spacing(spacing.space_xxs),
                    )
                    .spacing(spacing.space_s)
                    .align_y(Alignment::Center),
            )
            .padding(spacing.space_m)
            .class(cosmic::theme::Container::Card),
        );
    }

    // -- Step 1: local music folder --
    let add_label = fl!("add-music-folder");
    let add_icon = widget::icon::from_name("list-add-symbolic").size(16);
    let add_button: cosmic::Element<'a, HomeMessage> = if ctx.has_music_dirs {
        widget::button::standard(add_label)
            .leading_icon(add_icon)
            .on_press(HomeMessage::AddMusicDir)
            .into()
    } else {
        widget::button::suggested(add_label)
            .leading_icon(add_icon)
            .on_press(HomeMessage::AddMusicDir)
            .into()
    };

    let steps = vec![
        step_card(
            1,
            "folder-music-symbolic",
            fl!("home-step-folder-title"),
            fl!("home-step-folder-body"),
            ctx.has_music_dirs,
            vec![add_button],
        ),
        step_card(
            2,
            "network-server-symbolic",
            fl!("home-step-server-title"),
            fl!("home-step-server-body"),
            ctx.has_servers,
            vec![
                widget::button::standard(fl!("home-connect-server"))
                    .on_press(HomeMessage::ConnectServer)
                    .into(),
            ],
        ),
        step_card(
            3,
            "preferences-system-symbolic",
            fl!("home-step-customize-title"),
            fl!("home-step-customize-body"),
            false,
            vec![
                widget::button::standard(fl!("home-open-settings"))
                    .on_press(HomeMessage::OpenSettings)
                    .into(),
            ],
        ),
        step_card(
            4,
            "network-wireless-symbolic",
            fl!("home-step-wait-title"),
            fl!("home-step-wait-body"),
            false,
            vec![
                widget::button::link(fl!("home-browse-radio"))
                    .on_press(HomeMessage::BrowseRadio)
                    .into(),
                widget::button::link(fl!("home-discover-podcasts"))
                    .on_press(HomeMessage::DiscoverPodcasts)
                    .into(),
            ],
        ),
    ];

    col = col
        .push(
            widget::Column::new()
                .push(widget::text::title3(fl!("home-guide-title")))
                .push(
                    widget::flex_row(steps)
                        .row_spacing(spacing.space_s)
                        .column_spacing(spacing.space_s)
                        .justify_content(widget::JustifyContent::Center),
                )
                .align_x(Alignment::Center)
                .spacing(spacing.space_s),
        )
        .push(
            widget::Column::new()
                .push(widget::text::title4(fl!("home-tips-title")))
                .push(
                    widget::flex_row(vec![
                        tip(Some("Ctrl+F"), fl!("home-tip-search")),
                        tip(Some("Ctrl+M"), fl!("home-tip-mini-player")),
                        tip(None, fl!("home-tip-links")),
                    ])
                    .row_spacing(spacing.space_xs)
                    .column_spacing(spacing.space_l)
                    .justify_content(widget::JustifyContent::Center),
                )
                .align_x(Alignment::Center)
                .spacing(spacing.space_xs),
        );

    widget::scrollable(
        widget::container(col)
            .padding(spacing.space_l)
            .width(Length::Fill)
            .align_x(Horizontal::Center),
    )
    .height(Length::Fill)
    .into()
}

/// Gentle hint shown while there is no listening history yet.
fn no_history_hint<'a>() -> cosmic::Element<'a, HomeMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    widget::container(
        widget::Row::new()
            .push(widget::icon::from_name("emblem-music-symbolic").size(24))
            .push(widget::text::body(fl!("home-no-history-hint")).width(Length::Fill))
            .spacing(spacing.space_s)
            .align_y(Alignment::Center),
    )
    .padding(spacing.space_s)
    .class(cosmic::theme::Container::Card)
    .width(Length::Fill)
    .into()
}

// ---------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------

/// Render the Home page (or the open decade grid).
pub fn home_view<'a>(
    state: &'a HomeState,
    ctx: HomeContext,
    covers: &'a HashMap<String, widget::icon::Handle>,
    photos: &'a HashMap<String, widget::image::Handle>,
) -> cosmic::Element<'a, HomeMessage> {
    if let Some(decade) = &state.decade {
        return decade_view(decade, covers);
    }

    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut page = widget::Column::new().push(header(ctx));

    let empty = ctx.library_albums == 0 && state.data.as_ref().is_none_or(|d| d.total_albums == 0);
    if empty {
        return welcome_view(ctx);
    }

    let Some(data) = &state.data else {
        return page
            .push(common::empty_state(
                "folder-music-symbolic",
                fl!("home-loading"),
                String::new(),
            ))
            .into();
    };

    let mut shelves = widget::Column::new().spacing(spacing.space_m);

    if data.history_available && data.total_plays == 0 {
        shelves = shelves.push(widget::container(no_history_hint()).padding([0, spacing.space_m]));
    }
    if !data.recently_played.is_empty() {
        shelves = shelves.push(album_shelf(
            fl!("home-continue-listening"),
            None,
            None,
            &data.recently_played,
            covers,
        ));
    }
    if !data.most_played_month.is_empty() {
        shelves = shelves.push(album_shelf(
            fl!("home-most-played-month"),
            None,
            None,
            &data.most_played_month,
            covers,
        ));
    }
    if !data.top_artists_month.is_empty() {
        let cards = data
            .top_artists_month
            .iter()
            .map(|a| artist_card(a, photos))
            .collect();
        shelves = shelves.push(shelf(fl!("home-top-artists-month"), None, None, cards));
    }
    for shelf in similar::similar_shelves(&state.similar, photos) {
        shelves = shelves.push(shelf);
    }
    if !data.recently_added.is_empty() {
        shelves = shelves.push(album_shelf(
            fl!("home-recently-added"),
            None,
            None,
            &data.recently_added,
            covers,
        ));
    }
    if !data.rediscover.is_empty() {
        shelves = shelves.push(album_shelf(
            fl!("home-rediscover"),
            Some(fl!("home-rediscover-hint")),
            None,
            &data.rediscover,
            covers,
        ));
    }
    if !data.random.is_empty() {
        let shuffle = widget::tooltip(
            widget::button::icon(
                widget::icon::from_name("media-playlist-shuffle-symbolic").size(16),
            )
            .on_press(HomeMessage::ShuffleRandom),
            widget::text::caption(fl!("home-shuffle-picks")),
            widget::tooltip::Position::Bottom,
        )
        .into();
        shelves = shelves.push(album_shelf(
            fl!("home-random-picks"),
            None,
            Some(shuffle),
            &data.random,
            covers,
        ));
    }
    if !data.decades.is_empty() {
        shelves = shelves.push(decade_chips(&data.decades));
    }

    page = page.push(
        widget::scrollable(
            widget::container(shelves)
                .padding(cosmic::iced::Padding {
                    top: 0.0,
                    right: SCROLLBAR_CLEARANCE,
                    bottom: f32::from(spacing.space_m),
                    left: 0.0,
                })
                .width(Length::Fill),
        )
        .height(Length::Fill),
    );
    page.into()
}

/// The filtered album grid for one decade, with Back and Shuffle play.
fn decade_view<'a>(
    view: &'a DecadeView,
    covers: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, HomeMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let mut titles = widget::Column::new()
        .push(widget::text::title2(fl!(
            "home-decade-title",
            decade = view.decade.to_string()
        )))
        .spacing(spacing.space_xxs);
    if let Some(albums) = &view.albums {
        titles = titles.push(
            widget::text::caption(fl!("home-decade-albums", count = (albums.len() as u32)))
                .class(dim_text()),
        );
    }

    let mut header = widget::Row::new()
        .push(widget::tooltip(
            widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                .on_press(HomeMessage::CloseDecade),
            widget::text::caption(fl!("home-back")),
            widget::tooltip::Position::Bottom,
        ))
        .push(titles)
        .push(widget::space::horizontal())
        .spacing(spacing.space_s)
        .align_y(Alignment::Center)
        .padding(spacing.space_m);
    if view.albums.as_ref().is_some_and(|a| !a.is_empty()) {
        header = header.push(
            widget::button::suggested(fl!("home-decade-play")).on_press(HomeMessage::PlayDecade),
        );
    }

    let body: cosmic::Element<'a, HomeMessage> = match &view.albums {
        None => common::empty_state("folder-music-symbolic", fl!("home-loading"), String::new()),
        Some(albums) if albums.is_empty() => common::empty_state(
            "folder-music-symbolic",
            fl!("no-albums"),
            fl!("home-decade-empty-hint"),
        ),
        Some(albums) => common::fluid_card_grid(
            albums.len(),
            CARD_ART + 2.0 * CARD_PADDING,
            220.0 + 2.0 * CARD_PADDING,
            move |index, outer| album_card(&albums[index], outer, covers),
        ),
    };

    widget::Column::new().push(header).push(body).into()
}

/// `Route` an album card navigates to.
pub fn album_route(artist: &str, name: &str) -> Route {
    Route::Album {
        artist: artist.to_string(),
        album: name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_follows_the_hour() {
        assert_eq!(Greeting::for_hour(4), Greeting::Evening);
        assert_eq!(Greeting::for_hour(5), Greeting::Morning);
        assert_eq!(Greeting::for_hour(11), Greeting::Morning);
        assert_eq!(Greeting::for_hour(12), Greeting::Afternoon);
        assert_eq!(Greeting::for_hour(17), Greeting::Afternoon);
        assert_eq!(Greeting::for_hour(18), Greeting::Evening);
        assert_eq!(Greeting::for_hour(23), Greeting::Evening);
        assert_eq!(Greeting::for_hour(0), Greeting::Evening);
    }

    #[test]
    fn decade_labels() {
        assert_eq!(decade_label(1980), "1980s");
        assert_eq!(decade_label(2020), "2020s");
    }

    #[test]
    fn album_route_carries_identity() {
        assert_eq!(
            album_route("Beta", "B"),
            Route::Album {
                artist: "Beta".into(),
                album: "B".into()
            }
        );
    }
}
