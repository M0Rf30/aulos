// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Radio view — saved internet radio stations ("My stations") and a
//! radio-browser.info directory search ("Discover"), behind a stable
//! two-tab layout.
//!
//! Every reactive slot (add-by-URL card, rename card, now-playing banner,
//! Discover results) stays present in the tree at all times; only its
//! *content* changes with state. That's deliberate: iced keeps widget state
//! (scroll offset, text-input focus/cursor) by tree position, so
//! conditionally inserting/removing a sibling ahead of a
//! `scrollable`/`text_input` resets it. Switching tabs is the one place the
//! tree shape does change, since that's an explicit user navigation, not
//! incidental reactive state.

use crate::fl;
use crate::library::{CoverArt, Track};
use crate::online::radio::{SortOrder, StationSearchResult};
use crate::online::store::RadioStation;
use crate::player::PlaybackState;
use crate::views::common;
use crate::views::{card_button_class, list_row_button_class};
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::{Alignment, Color, Length, Padding};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;
use std::collections::HashMap;

/// Id of the "Add station" name input, focused when the card opens.
pub const ADD_NAME_INPUT_ID: &str = "radio-add-name-input";
/// Id of the rename input, focused when a rename starts.
pub const RENAME_INPUT_ID: &str = "radio-rename-input";

/// Station grid card geometry (outer width includes the button padding).
const CARD_PADDING: f32 = 8.0;
const CARD_WIDTH: f32 = 148.0;
const CARD_MAX_WIDTH: f32 = 200.0;
const CARD_LABEL_HEIGHT: f32 = 40.0;

/// Which of the two Radio tabs is shown. Kept in `AppModel` for the
/// session (not persisted to disk) so navigating away and back preserves
/// the tab the user was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RadioTab {
    #[default]
    MyStations,
    Discover,
}

/// A one-shot Discover preset that bypasses the free-text/tag/country
/// query and fetches a globally ranked list instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverPreset {
    Popular,
    TopVoted,
}

/// Quick-tag chips offered above the Discover results. A fixed, curated
/// set rather than a fetched tag cloud -- covers common genres without a
/// network round trip just to populate the chip row.
pub const QUICK_TAGS: [&str; 9] = [
    "Pop",
    "Rock",
    "Jazz",
    "Classical",
    "Electronic",
    "News",
    "Talk",
    "Ambient",
    "Lo-fi",
];

/// Messages from the radio view. Every message that refers to a specific
/// row carries a stable key (the saved station's db id, or the directory
/// result's `stationuuid`) -- never a `Vec` position, which goes stale the
/// moment a list reloads, re-sorts or is filtered.
#[derive(Debug, Clone)]
pub enum RadioMessage {
    /// Switch between "My stations" and "Discover".
    TabSelected(RadioTab),

    // -- My stations --
    /// Live filter text over saved stations.
    FilterChanged(String),
    /// Toggle the inline add-by-URL card open/closed.
    ToggleAddForm,
    AddNameChanged(String),
    AddUrlChanged(String),
    SubmitAdd,
    /// Play a saved station by its db id.
    PlaySaved(i64),
    /// Remove a saved station by its db id.
    RemoveSaved(i64),
    /// Undo a removal: re-save the station exactly as it was.
    UndoRemove(RadioStation),
    /// Start renaming a saved station: its id and current name (seeds the
    /// rename field).
    StartRename(i64, String),
    RenameInputChanged(String),
    CommitRename(i64),
    CancelRename,

    // -- Discover --
    SearchChanged(String),
    SearchSubmit,
    /// Toggle a quick-tag chip; `None` clears the tag filter.
    TagSelected(Option<&'static str>),
    /// Toggle filtering to the user's own locale country on/off.
    CountryToggled,
    /// Index into `SortOrder::ALL`.
    SortSelected(usize),
    Preset(DiscoverPreset),
    RetrySearch,
    /// Play a directory result by its `stationuuid`.
    PlayResult(String),
    /// Save a directory result by its `stationuuid`.
    SaveResult(String),

    /// Stop the currently-playing station.
    ///
    /// Deliberately identical to the top-level `Message::Stop`: player
    /// transport is owned by the QueueEngine slice, not Radio, so
    /// `src/app/view.rs` maps this one variant straight to `Message::Stop`
    /// at the call site instead of routing it through
    /// `AppModel::update_radio`.
    Stop,
}

/// Everything the radio view needs to render, borrowed from `AppModel`.
/// A struct rather than a long parameter list -- this view has enough
/// independent bits of state (two tabs' worth) that positional args would
/// be unreadable and error-prone to reorder.
pub struct RadioViewProps<'a> {
    // My stations
    pub stations: &'a [RadioStation],
    pub tab: RadioTab,
    pub filter: &'a str,
    pub add_open: bool,
    pub add_name: &'a str,
    pub add_url: &'a str,
    pub add_error: Option<&'a str>,
    pub renaming_id: Option<i64>,
    pub rename_input: &'a str,

    // Discover
    pub search_query: &'a str,
    pub search_tag: Option<&'static str>,
    pub search_country: &'a str,
    pub locale_country: &'a str,
    pub sort: SortOrder,
    pub results: &'a [StationSearchResult],
    pub results_loading: bool,
    pub results_error: Option<&'a str>,

    // Shared
    pub icons: &'a HashMap<String, widget::icon::Handle>,
    pub current_track: Option<&'a Track>,
    pub playback_state: Option<PlaybackState>,
    /// Favicon URL and pre-resolution stream/result URL ("key") of the
    /// station currently loaded into the player, captured when playback
    /// started. Matching on this instead of `current_track.source_uri`
    /// means the "currently playing" indicator keeps working even after
    /// stream-URL resolution (following a `.pls`/`.m3u` playlist) rewrites
    /// the actual URL to something that no longer matches the saved/
    /// search row it came from.
    pub now_playing_favicon: &'a str,
    pub now_playing_key: &'a str,
}

// ---------------------------------------------------------------------
// Styling helpers
// ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Interaction {
    Rest,
    Hover,
    Press,
}

/// Build a `Button::Custom` class from a single style function over the
/// interaction state.
fn button_class(
    style: impl Fn(&cosmic::Theme, Interaction) -> ButtonStyle + Clone + Send + Sync + 'static,
) -> cosmic::theme::Button {
    let (a, h, p, d) = (style.clone(), style.clone(), style.clone(), style);
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| a(theme, Interaction::Rest)),
        hovered: Box::new(move |_focused, theme| h(theme, Interaction::Hover)),
        pressed: Box::new(move |_focused, theme| p(theme, Interaction::Press)),
        disabled: Box::new(move |theme| d(theme, Interaction::Rest)),
    }
}

/// Fully rounded pill button: used for the segmented tabs and the filter
/// chips. Selected pills are accent-tinted; unselected ones are flat
/// (`rest_bg == false`) or sit on a subtle surface (`rest_bg == true`).
fn pill_class(selected: bool, rest_bg: bool) -> cosmic::theme::Button {
    button_class(move |theme, state| {
        let cosmic = theme.cosmic();
        let comp = &cosmic.background(false).component;
        let radius = cosmic.corner_radii.radius_xl;
        if selected {
            let accent = cosmic.accent_color();
            let alpha = match state {
                Interaction::Rest => 0.18,
                Interaction::Hover => 0.26,
                Interaction::Press => 0.32,
            };
            ButtonStyle {
                background: Some(Background::Color(accent.with_alpha(alpha).into())),
                text_color: Some(accent.into()),
                icon_color: Some(accent.into()),
                border_radius: radius.into(),
                ..ButtonStyle::new()
            }
        } else {
            let background = match state {
                Interaction::Rest => rest_bg.then(|| Background::Color(comp.base.into())),
                Interaction::Hover => Some(Background::Color(comp.hover.into())),
                Interaction::Press => Some(Background::Color(comp.pressed.into())),
            };
            ButtonStyle {
                background,
                text_color: Some(comp.on.into()),
                icon_color: Some(comp.on.into()),
                border_radius: radius.into(),
                ..ButtonStyle::new()
            }
        }
    })
}

/// Selected-card variant of the grid card button: accent wash.
fn station_card_class(is_current: bool) -> cosmic::theme::Button {
    if !is_current {
        return card_button_class();
    }
    button_class(|theme, state| {
        let cosmic = theme.cosmic();
        let accent = cosmic.accent_color();
        let alpha = match state {
            Interaction::Rest => 0.12,
            Interaction::Hover => 0.18,
            Interaction::Press => 0.24,
        };
        ButtonStyle {
            background: Some(Background::Color(accent.with_alpha(alpha).into())),
            text_color: Some(cosmic.background(false).component.on.into()),
            icon_color: Some(cosmic.background(false).component.on.into()),
            border_radius: cosmic.corner_radii.radius_m.into(),
            ..ButtonStyle::new()
        }
    })
}

/// Round translucent button that floats over station artwork.
fn overlay_button_class() -> cosmic::theme::Button {
    button_class(|_theme, state| ButtonStyle {
        background: match state {
            Interaction::Rest => None,
            Interaction::Hover => Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.18))),
            Interaction::Press => Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.28))),
        },
        text_color: Some(Color::WHITE),
        icon_color: Some(Color::WHITE),
        border_radius: [99.0; 4].into(),
        ..ButtonStyle::new()
    })
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

/// Caption text dimmed to the theme's secondary colour.
fn secondary_caption<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Custom(|theme| {
        cosmic::iced::widget::text::Style {
            color: Some(theme.cosmic().palette.neutral_7.into()),
            ..Default::default()
        }
    }))
}

fn destructive_caption<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    widget::text::caption(content).class(cosmic::theme::Text::Custom(|theme| {
        cosmic::iced::widget::text::Style {
            color: Some(theme.cosmic().destructive_color().into()),
            ..Default::default()
        }
    }))
}

fn pad(top: f32, right: f32, bottom: f32, left: f32) -> Padding {
    Padding {
        top,
        right,
        bottom,
        left,
    }
}

/// First `n` non-empty comma-separated tags.
fn tag_list(tags: &str, n: usize) -> Vec<String> {
    tags.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .take(n)
        .map(|t| common::truncate_str(t, 18))
        .collect()
}

// ---------------------------------------------------------------------
// Small building blocks
// ---------------------------------------------------------------------

/// Station artwork tile: the favicon on a deterministic per-station tinted
/// surface (initials when the favicon is missing or not yet fetched), with
/// an accent ring while the station is live.
fn station_tile<'a, M: 'a + 'static>(
    name: &str,
    favicon: &str,
    icons: &HashMap<String, widget::icon::Handle>,
    size: f32,
    radius: f32,
    elevated: bool,
    live: bool,
) -> cosmic::Element<'a, M> {
    let (r, g, b) = CoverArt::artist_avatar_color(name);
    let hue = Color::from_rgb8(r, g, b);

    let inner: cosmic::Element<'a, M> = match icons.get(favicon).filter(|_| !favicon.is_empty()) {
        Some(handle) => common::cover_art(handle, (size * 0.62).round(), radius * 0.6, false),
        None => {
            let initials = CoverArt::artist_initials(name);
            if initials.is_empty() {
                widget::icon::from_name("network-wireless-symbolic")
                    .size((size * 0.42) as u16)
                    .into()
            } else {
                widget::text(initials)
                    .size((size * 0.34).max(11.0))
                    .class(cosmic::theme::Text::Color(hue))
                    .into()
            }
        }
    };

    widget::container(inner)
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::custom(move |theme| {
            use cosmic::iced::gradient::Linear;
            use cosmic::iced::{Gradient, Radians};
            let cosmic = theme.cosmic();
            let base: Color = cosmic.background(false).component.base.into();
            let top = mix(base, hue, 0.30);
            let bottom = mix(base, hue, 0.10);
            cosmic::iced::widget::container::Style {
                background: Some(Background::Gradient(Gradient::Linear(
                    Linear::new(Radians(std::f32::consts::PI))
                        .add_stop(0.0, top)
                        .add_stop(1.0, bottom),
                ))),
                border: cosmic::iced::Border {
                    radius: radius.into(),
                    width: if live { 2.0 } else { 0.0 },
                    color: cosmic.accent_color().into(),
                },
                shadow: if elevated {
                    cosmic::iced::Shadow {
                        color: Color::from_rgba(
                            0.0,
                            0.0,
                            0.0,
                            if cosmic.is_dark { 0.40 } else { 0.18 },
                        ),
                        offset: cosmic::iced::Vector::new(0.0, 4.0),
                        blur_radius: 12.0,
                    }
                } else {
                    cosmic::iced::Shadow::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Three static equaliser bars; flat while paused.
fn eq_bars<'a, M: 'a + 'static>(color: Color, paused: bool) -> cosmic::Element<'a, M> {
    let heights: [f32; 3] = if paused { [4.0; 3] } else { [6.0, 11.0, 8.0] };
    let mut row = widget::Row::new().spacing(2).align_y(Alignment::Center);
    for h in heights {
        row = row.push(
            widget::container(
                widget::Space::new()
                    .width(Length::Fixed(3.0))
                    .height(Length::Fixed(h)),
            )
            .class(cosmic::theme::Container::custom(move |_theme| {
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(color)),
                    border: cosmic::iced::Border {
                        radius: 1.5.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            })),
        );
    }
    row.into()
}

/// Solid accent "LIVE" pill with equaliser bars (or "paused" when paused).
fn live_pill<'a, M: 'a + 'static>(paused: bool) -> cosmic::Element<'a, M> {
    let on: Color = cosmic::theme::active().cosmic().accent.on.into();
    let label = if paused {
        fl!("radio-paused")
    } else {
        fl!("now-playing-live")
    };
    let pill = widget::Row::new()
        .push(eq_bars(on, paused))
        .push(widget::text::caption(label))
        .spacing(5)
        .align_y(Alignment::Center);
    widget::container(pill)
        .padding([2, 8])
        .class(cosmic::theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(cosmic.accent_color().into())),
                text_color: Some(on),
                icon_color: Some(on),
                border: cosmic::iced::Border {
                    radius: cosmic.corner_radii.radius_xl.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Small non-interactive chip (tag, codec/bitrate).
fn static_chip<'a, M: 'a + 'static>(label: String, accent: bool) -> cosmic::Element<'a, M> {
    widget::container(
        widget::text::caption(label).wrapping(cosmic::iced::core::text::Wrapping::None),
    )
    .padding([1, 8])
    .class(cosmic::theme::Container::custom(move |theme| {
        let cosmic = theme.cosmic();
        let accent_color = cosmic.accent_color();
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(if accent {
                accent_color.with_alpha(0.14).into()
            } else {
                cosmic.background(false).component.base.into()
            })),
            text_color: accent.then(|| accent_color.into()),
            border: cosmic::iced::Border {
                radius: cosmic.corner_radii.radius_xl.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }))
    .into()
}

/// Circular tinted badge around a symbolic icon.
fn icon_badge<'a, M: 'a + 'static>(icon_name: &'static str, size: f32) -> cosmic::Element<'a, M> {
    widget::container(widget::icon::from_name(icon_name).size((size * 0.5) as u16))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let accent = cosmic.accent_color();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(accent.with_alpha(0.14).into())),
                icon_color: Some(accent.into()),
                text_color: Some(accent.into()),
                border: cosmic::iced::Border {
                    radius: (size / 2.0).into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Centered status panel (empty / loading / error) with an optional action.
fn status_panel<'a>(
    icon_name: &'static str,
    title: String,
    hint: String,
    action: Option<cosmic::Element<'a, RadioMessage>>,
) -> cosmic::Element<'a, RadioMessage> {
    let mut col = widget::Column::new()
        .spacing(8)
        .align_x(Alignment::Center)
        .max_width(420.0)
        .push(icon_badge(icon_name, 72.0))
        .push(widget::Space::new().height(Length::Fixed(4.0)))
        .push(widget::text::title3(title))
        .push(widget::text::body(hint).align_x(Horizontal::Center).class(
            cosmic::theme::Text::Custom(|theme| cosmic::iced::widget::text::Style {
                color: Some(theme.cosmic().palette.neutral_7.into()),
                ..Default::default()
            }),
        ));
    if let Some(action) = action {
        col = col
            .push(widget::Space::new().height(Length::Fixed(8.0)))
            .push(action);
    }
    widget::container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(24)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
}

/// Pill chip button (filters / presets).
fn chip_button<'a>(
    label: String,
    icon_name: Option<&'static str>,
    selected: bool,
    on_press: RadioMessage,
) -> cosmic::Element<'a, RadioMessage> {
    let mut row = widget::Row::new().spacing(6).align_y(Alignment::Center);
    if let Some(icon_name) = icon_name {
        row = row.push(widget::icon::from_name(icon_name).size(14));
    }
    row = row.push(widget::text::body(label));
    widget::button::custom(row)
        .padding([4, 12])
        .on_press(on_press)
        .class(pill_class(selected, true))
        .into()
}

/// Translucent dark circular icon button floating on top of artwork.
fn overlay_button<'a>(
    icon_name: &'static str,
    tip: String,
    on_press: RadioMessage,
) -> cosmic::Element<'a, RadioMessage> {
    let disc = widget::container(widget::icon::from_name(icon_name).size(14))
        .padding(6)
        .class(cosmic::theme::Container::custom(|_theme| {
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.55))),
                icon_color: Some(Color::WHITE),
                border: cosmic::iced::Border {
                    radius: 99.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }));
    widget::tooltip(
        widget::button::custom(disc)
            .padding(0)
            .on_press(on_press)
            .class(overlay_button_class()),
        widget::text::caption(tip),
        widget::tooltip::Position::Bottom,
    )
    .into()
}

// ---------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------

/// Render the full radio page: header, add/rename cards, now-playing
/// banner and the active tab's content.
pub fn radio_view<'a>(props: RadioViewProps<'a>) -> cosmic::Element<'a, RadioMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let space_m = f32::from(spacing.space_m);

    let is_radio_track = props
        .current_track
        .is_some_and(|t| &*t.provider_id == "radio");
    let is_live = is_radio_track
        && matches!(
            props.playback_state,
            Some(PlaybackState::Playing) | Some(PlaybackState::Paused)
        );
    let current_stream_url = if is_live {
        Some(props.now_playing_key)
    } else {
        None
    };
    let is_paused = matches!(props.playback_state, Some(PlaybackState::Paused));

    let top = widget::Column::new()
        .spacing(spacing.space_s)
        .push(header_row(props.tab, props.add_open, props.stations.len()))
        .push(add_form_card(
            props.add_open,
            props.add_name,
            props.add_url,
            props.add_error,
        ))
        .push(rename_card(
            props.renaming_id,
            props.rename_input,
            props.stations,
            props.icons,
        ))
        .push(now_playing_banner(
            is_live,
            is_paused,
            props.current_track,
            props.now_playing_favicon,
            props.icons,
        ));

    let content = match props.tab {
        RadioTab::MyStations => my_stations_tab(
            props.stations,
            props.filter,
            props.icons,
            current_stream_url,
            is_live,
            is_paused,
        ),
        RadioTab::Discover => discover_tab(
            props.search_query,
            props.search_tag,
            props.search_country,
            props.locale_country,
            props.sort,
            props.results,
            props.results_loading,
            props.results_error,
            props.stations,
            props.icons,
            current_stream_url,
            is_live,
            is_paused,
        ),
    };

    widget::Column::new()
        .spacing(spacing.space_s)
        .push(widget::container(top).padding(pad(space_m, space_m, 0.0, space_m)))
        .push(content)
        .into()
}

/// Title + segmented tab switch + add-station toggle. Always the same
/// widgets in the same order -- only styles/state change.
fn header_row<'a>(
    tab: RadioTab,
    add_open: bool,
    station_count: usize,
) -> cosmic::Element<'a, RadioMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let title = widget::Column::new()
        .push(widget::text::title2(fl!("radio")))
        .push(secondary_caption(match station_count {
            0 => fl!("radio-subtitle"),
            1 => fl!("radio-station-count-one"),
            n => fl!("radio-station-count-other", count = n),
        }))
        .spacing(2);

    let tabs = widget::container(
        widget::Row::new()
            .push(tab_button(
                fl!("radio-tab-my-stations"),
                "emblem-favorite-symbolic",
                tab == RadioTab::MyStations,
                RadioTab::MyStations,
            ))
            .push(tab_button(
                fl!("radio-tab-discover"),
                "edit-find-symbolic",
                tab == RadioTab::Discover,
                RadioTab::Discover,
            ))
            .spacing(2),
    )
    .padding(3)
    .class(cosmic::theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(
                cosmic.background(false).component.base.into(),
            )),
            border: cosmic::iced::Border {
                radius: cosmic.corner_radii.radius_xl.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }));

    let add_btn = widget::button::custom(
        widget::Row::new()
            .push(
                widget::icon::from_name(if add_open {
                    "window-close-symbolic"
                } else {
                    "list-add-symbolic"
                })
                .size(16),
            )
            .push(widget::text::body(if add_open {
                fl!("radio-cancel")
            } else {
                fl!("radio-add-station")
            }))
            .spacing(6)
            .align_y(Alignment::Center),
    )
    .padding([6, 14])
    .on_press(RadioMessage::ToggleAddForm)
    .class(pill_class(add_open, true));

    widget::Row::new()
        .push(title)
        .push(widget::Space::new().width(Length::Fixed(f32::from(spacing.space_m))))
        .push(tabs)
        .push(widget::Space::new().width(Length::Fill))
        .push(add_btn)
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center)
        .into()
}

fn tab_button<'a>(
    label: String,
    icon_name: &'static str,
    selected: bool,
    target: RadioTab,
) -> cosmic::Element<'a, RadioMessage> {
    widget::button::custom(
        widget::Row::new()
            .push(widget::icon::from_name(icon_name).size(16))
            .push(widget::text::body(label))
            .spacing(6)
            .align_y(Alignment::Center),
    )
    .padding([5, 14])
    .on_press(RadioMessage::TabSelected(target))
    .class(pill_class(selected, false))
    .into()
}

/// Card-surface container used by the add / rename panels.
fn panel<'a>(
    content: impl Into<cosmic::Element<'a, RadioMessage>>,
) -> cosmic::Element<'a, RadioMessage> {
    widget::container(content)
        .class(cosmic::theme::Container::Card)
        .padding(cosmic::theme::active().cosmic().spacing.space_s)
        .width(Length::Fill)
        .into()
}

/// A tiny field label above an input.
fn field<'a>(
    label: String,
    input: impl Into<cosmic::Element<'a, RadioMessage>>,
    width: Length,
) -> cosmic::Element<'a, RadioMessage> {
    widget::container(
        widget::Column::new()
            .push(secondary_caption(label))
            .push(input)
            .spacing(4),
    )
    .width(width)
    .into()
}

fn empty_slot<'a>() -> cosmic::Element<'a, RadioMessage> {
    widget::Space::new()
        .width(Length::Shrink)
        .height(Length::Fixed(0.0))
        .into()
}

/// The add-by-URL card's slot. Always present; zero-height when closed.
fn add_form_card<'a>(
    open: bool,
    name: &'a str,
    url: &'a str,
    error: Option<&'a str>,
) -> cosmic::Element<'a, RadioMessage> {
    if !open {
        return empty_slot();
    }
    let heading = widget::Row::new()
        .push(icon_badge("list-add-symbolic", 36.0))
        .push(
            widget::Column::new()
                .push(widget::text::title4(fl!("radio-add-title")))
                .push(secondary_caption(fl!("radio-add-hint")))
                .spacing(2),
        )
        .spacing(12)
        .align_y(Alignment::Center);

    let fields = widget::Row::new()
        .push(field(
            fl!("radio-name-label"),
            widget::text_input(fl!("station-name-placeholder"), name)
                .id(widget::Id::new(ADD_NAME_INPUT_ID))
                .on_input(RadioMessage::AddNameChanged)
                .width(Length::Fill),
            Length::FillPortion(1),
        ))
        .push(field(
            fl!("radio-url-label"),
            widget::text_input(fl!("station-url-placeholder"), url)
                .on_input(RadioMessage::AddUrlChanged)
                .on_submit(|_| RadioMessage::SubmitAdd)
                .width(Length::Fill),
            Length::FillPortion(2),
        ))
        .spacing(12);

    let mut footer = widget::Row::new().spacing(8).align_y(Alignment::Center);
    footer = match error {
        Some(err) => footer
            .push(widget::icon::from_name("dialog-error-symbolic").size(14))
            .push(destructive_caption(err)),
        None => footer,
    };
    footer = footer
        .push(widget::Space::new().width(Length::Fill))
        .push(widget::button::standard(fl!("radio-cancel")).on_press(RadioMessage::ToggleAddForm))
        .push(widget::button::suggested(fl!("save")).on_press(RadioMessage::SubmitAdd));

    panel(
        widget::Column::new()
            .push(heading)
            .push(fields)
            .push(footer)
            .spacing(12),
    )
}

/// The rename card's slot: always present, zero-height unless a saved
/// station is being renamed.
fn rename_card<'a>(
    renaming_id: Option<i64>,
    rename_input: &'a str,
    stations: &'a [RadioStation],
    icons: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, RadioMessage> {
    let Some(id) = renaming_id else {
        return empty_slot();
    };
    let station = stations.iter().find(|s| s.id == id);
    let radius = cosmic::theme::active().cosmic().corner_radii.radius_s[0];
    let tile = match station {
        Some(s) => station_tile(&s.name, &s.favicon_url, icons, 48.0, radius, false, false),
        None => widget::icon::from_name("network-wireless-symbolic")
            .size(32)
            .into(),
    };
    let input = widget::text_input(fl!("station-name-placeholder"), rename_input)
        .id(widget::Id::new(RENAME_INPUT_ID))
        .on_input(RadioMessage::RenameInputChanged)
        .on_submit(move |_| RadioMessage::CommitRename(id))
        .width(Length::Fill);

    panel(
        widget::Row::new()
            .push(tile)
            .push(field(fl!("radio-rename-tooltip"), input, Length::Fill))
            .push(
                widget::button::standard(fl!("radio-cancel")).on_press(RadioMessage::CancelRename),
            )
            .push(widget::button::suggested(fl!("save")).on_press(RadioMessage::CommitRename(id)))
            .spacing(12)
            .align_y(Alignment::End),
    )
}

/// The now-playing banner's permanent slot: an accent-washed card with the
/// live station and Stop, or a quiet idle card. `is_live` already folds in
/// playback state, so a stopped station never renders as if still playing.
fn now_playing_banner<'a>(
    is_live: bool,
    is_paused: bool,
    current_track: Option<&'a Track>,
    favicon: &'a str,
    icons: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, RadioMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let radius_l = cosmic::theme::active().cosmic().corner_radii.radius_l;

    if !is_live {
        let body = widget::Row::new()
            .push(icon_badge("audio-input-microphone-symbolic", 40.0))
            .push(
                widget::Column::new()
                    .push(common::cell_text(fl!("radio-idle-title")))
                    .push(secondary_caption(fl!("radio-idle-hint")))
                    .spacing(2),
            )
            .spacing(12)
            .align_y(Alignment::Center);
        return widget::container(body)
            .padding([spacing.space_xs, spacing.space_s])
            .width(Length::Fill)
            .class(cosmic::theme::Container::custom(move |theme| {
                let cosmic = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    background: Some(Background::Color(
                        cosmic.background(false).component.base.into(),
                    )),
                    border: cosmic::iced::Border {
                        radius: radius_l.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
            .into();
    }

    let title = current_track.map(|t| t.title.as_str()).unwrap_or_default();
    let tile = station_tile(
        title,
        favicon,
        icons,
        64.0,
        radius_l[0].min(14.0),
        true,
        false,
    );
    let info = widget::Column::new()
        .push(live_pill(is_paused))
        .push(widget::text::title4(title).wrapping(cosmic::iced::core::text::Wrapping::None))
        .spacing(6);
    let stop = widget::button::custom(
        widget::Row::new()
            .push(widget::icon::from_name("media-playback-stop-symbolic").size(16))
            .push(widget::text::body(fl!("stop")))
            .spacing(6)
            .align_y(Alignment::Center),
    )
    .padding([6, 16])
    .on_press(RadioMessage::Stop)
    .class(pill_class(false, true));

    widget::container(
        widget::Row::new()
            .push(tile)
            .push(common::clipped_cell(info.into()))
            .push(stop)
            .spacing(16)
            .align_y(Alignment::Center),
    )
    .padding(spacing.space_s)
    .width(Length::Fill)
    .class(cosmic::theme::Container::custom(move |theme| {
        use cosmic::iced::gradient::Linear;
        use cosmic::iced::{Gradient, Radians};
        let cosmic = theme.cosmic();
        let base: Color = cosmic.background(false).component.base.into();
        let accent: Color = cosmic.accent_color().into();
        cosmic::iced::widget::container::Style {
            background: Some(Background::Gradient(Gradient::Linear(
                Linear::new(Radians(std::f32::consts::FRAC_PI_2))
                    .add_stop(0.0, mix(base, accent, 0.30))
                    .add_stop(1.0, base),
            ))),
            border: cosmic::iced::Border {
                radius: radius_l.into(),
                width: 1.0,
                color: Color { a: 0.35, ..accent },
            },
            ..Default::default()
        }
    }))
    .into()
}

/// Search field with a leading magnifier and a clear button.
fn search_field<'a>(
    placeholder: String,
    value: &'a str,
    on_input: impl Fn(String) -> RadioMessage + 'a,
    clear: RadioMessage,
) -> widget::TextInput<'a, RadioMessage> {
    widget::search_input(placeholder, value)
        .on_input(on_input)
        .on_clear(clear)
        .width(Length::Fill)
}

/// "My stations" tab: filter box, then either an empty state or the
/// saved-station card grid. `is_current` also requires `is_live` so a
/// stopped station's card loses its highlight.
fn my_stations_tab<'a>(
    stations: &'a [RadioStation],
    filter: &'a str,
    icons: &'a HashMap<String, widget::icon::Handle>,
    current_stream_url: Option<&'a str>,
    is_live: bool,
    is_paused: bool,
) -> cosmic::Element<'a, RadioMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let space_m = f32::from(spacing.space_m);

    let filter_row = widget::container(search_field(
        fl!("radio-filter-placeholder"),
        filter,
        RadioMessage::FilterChanged,
        RadioMessage::FilterChanged(String::new()),
    ))
    .padding(pad(0.0, space_m, 0.0, space_m));

    let mut col = widget::Column::new()
        .spacing(spacing.space_xs)
        .push(filter_row);

    if stations.is_empty() {
        let actions = widget::Row::new()
            .spacing(spacing.space_xs)
            .push(
                widget::button::suggested(fl!("radio-go-discover"))
                    .on_press(RadioMessage::TabSelected(RadioTab::Discover)),
            )
            .push(
                widget::button::standard(fl!("radio-add-by-url"))
                    .on_press(RadioMessage::ToggleAddForm),
            );
        return col
            .push(status_panel(
                "network-wireless-symbolic",
                fl!("no-stations"),
                fl!("stations-empty-hint"),
                Some(actions.into()),
            ))
            .into();
    }

    let filter_lower = filter.trim().to_lowercase();
    let filtered: Vec<&'a RadioStation> = stations
        .iter()
        .filter(|s| {
            filter_lower.is_empty()
                || s.name.to_lowercase().contains(&filter_lower)
                || s.tags.to_lowercase().contains(&filter_lower)
        })
        .collect();

    if filtered.is_empty() {
        col = col.push(status_panel(
            "edit-find-symbolic",
            fl!("radio-no-filter-matches"),
            fl!("radio-no-filter-matches-hint"),
            None,
        ));
        return col.into();
    }

    let count = filtered.len();
    col = col.push(common::fluid_card_grid(
        count,
        CARD_WIDTH + 2.0 * CARD_PADDING,
        CARD_MAX_WIDTH + 2.0 * CARD_PADDING,
        move |index, outer| {
            let station = filtered[index];
            let is_current = is_live && current_stream_url == Some(station.stream_url.as_str());
            station_card(station, icons, outer, is_current, is_paused)
        },
    ));
    col.into()
}

/// One saved-station grid card: elevated artwork with floating rename /
/// remove actions (and a LIVE pill when current), name and tags below.
fn station_card<'a>(
    station: &'a RadioStation,
    icons: &'a HashMap<String, widget::icon::Handle>,
    outer: f32,
    is_current: bool,
    is_paused: bool,
) -> cosmic::Element<'a, RadioMessage> {
    let art_size = outer - 2.0 * CARD_PADDING;
    let radius = cosmic::theme::active().cosmic().corner_radii.radius_m[0];
    let id = station.id;

    let tile = station_tile(
        &station.name,
        &station.favicon_url,
        icons,
        art_size,
        radius,
        true,
        is_current,
    );

    let actions = widget::container(
        widget::Row::new()
            .push(overlay_button(
                "document-edit-symbolic",
                fl!("radio-rename-tooltip"),
                RadioMessage::StartRename(id, station.name.clone()),
            ))
            .push(overlay_button(
                "user-trash-symbolic",
                fl!("remove"),
                RadioMessage::RemoveSaved(id),
            ))
            .spacing(4),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding(6)
    .align_x(Horizontal::Right)
    .align_y(Vertical::Top);

    let mut art = cosmic::iced::widget::Stack::new()
        .width(Length::Fixed(art_size))
        .height(Length::Fixed(art_size))
        .push(tile);
    if is_current {
        art = art.push(
            widget::container(live_pill(is_paused))
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(8)
                .align_x(Horizontal::Left)
                .align_y(Vertical::Bottom),
        );
    }
    art = art.push(actions);

    let tags = tag_list(&station.tags, 2).join(" · ");
    let subtitle = if tags.is_empty() {
        "\u{a0}".to_string()
    } else {
        tags
    };
    let label = common::grid_card_label(
        art_size,
        CARD_LABEL_HEIGHT,
        common::clipped_cell(common::cell_text(station.name.as_str()).into()),
        common::clipped_cell(secondary_caption(subtitle).into()),
    );

    widget::button::custom(common::grid_card(art.into(), art_size, label))
        .on_press(RadioMessage::PlaySaved(id))
        .padding(CARD_PADDING as u16)
        .class(station_card_class(is_current))
        .into()
}

/// "Discover" tab: search controls, quick filters, then a single fixed
/// results slot (loading / error+retry / empty / list) so the scrollable
/// itself is always present.
#[allow(clippy::too_many_arguments)]
fn discover_tab<'a>(
    query: &'a str,
    tag: Option<&'static str>,
    country: &'a str,
    locale_country: &'a str,
    sort: SortOrder,
    results: &'a [StationSearchResult],
    loading: bool,
    error: Option<&'a str>,
    saved: &'a [RadioStation],
    icons: &'a HashMap<String, widget::icon::Handle>,
    current_stream_url: Option<&'a str>,
    is_live: bool,
    is_paused: bool,
) -> cosmic::Element<'a, RadioMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let space_m = f32::from(spacing.space_m);

    let search_row = widget::Row::new()
        .push(search_field(
            fl!("radio-search-placeholder"),
            query,
            RadioMessage::SearchChanged,
            RadioMessage::SearchChanged(String::new()),
        ))
        .push(widget::button::suggested(fl!("search")).on_press(RadioMessage::SearchSubmit))
        .push(widget::dropdown(
            sort_labels(),
            Some(sort_index(sort)),
            RadioMessage::SortSelected,
        ))
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center);

    let mut chips: Vec<cosmic::Element<'a, RadioMessage>> = vec![
        chip_button(
            fl!("radio-preset-popular"),
            Some("starred-symbolic"),
            false,
            RadioMessage::Preset(DiscoverPreset::Popular),
        ),
        chip_button(
            fl!("radio-preset-top-voted"),
            Some("emblem-favorite-symbolic"),
            false,
            RadioMessage::Preset(DiscoverPreset::TopVoted),
        ),
    ];
    for name in QUICK_TAGS {
        let selected = tag == Some(name);
        let target = if selected { None } else { Some(name) };
        chips.push(chip_button(
            name.to_string(),
            None,
            selected,
            RadioMessage::TagSelected(target),
        ));
    }
    if !locale_country.is_empty() {
        chips.push(chip_button(
            fl!("radio-only-country", code = locale_country.to_string()),
            Some("mark-location-symbolic"),
            !country.is_empty(),
            RadioMessage::CountryToggled,
        ));
    }

    let controls = widget::Column::new()
        .spacing(spacing.space_xs)
        .push(search_row)
        .push(widget::flex_row(chips).spacing(spacing.space_xxs));

    let body: cosmic::Element<'a, RadioMessage> = if loading {
        status_panel(
            "view-refresh-symbolic",
            fl!("searching"),
            fl!("radio-searching-hint"),
            None,
        )
    } else if let Some(err) = error {
        status_panel(
            "network-error-symbolic",
            fl!("radio-search-failed"),
            err.to_string(),
            Some(
                widget::button::standard(fl!("radio-retry"))
                    .on_press(RadioMessage::RetrySearch)
                    .into(),
            ),
        )
    } else if results.is_empty() {
        status_panel(
            "edit-find-symbolic",
            fl!("radio-no-results"),
            fl!("radio-no-results-hint"),
            None,
        )
    } else {
        let mut list = widget::Column::new().spacing(2);
        list = list.push(
            widget::container(secondary_caption(fl!(
                "radio-results-count",
                count = results.len()
            )))
            .padding([0, 4, 4, 4]),
        );
        for result in results {
            let already_saved = saved.iter().any(|s| s.stream_url == result.url);
            let is_current = is_live && current_stream_url == Some(result.url.as_str());
            list = list.push(discover_result_row(
                result,
                icons,
                already_saved,
                is_current,
                is_paused,
            ));
        }
        widget::scrollable(widget::container(list).width(Length::Fill).padding(pad(
            0.0,
            space_m + 8.0,
            space_m,
            space_m,
        )))
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    };

    widget::Column::new()
        .spacing(spacing.space_xs)
        .push(widget::container(controls).padding(pad(0.0, space_m, 0.0, space_m)))
        .push(body)
        .into()
}

fn discover_result_row<'a>(
    result: &'a StationSearchResult,
    icons: &'a HashMap<String, widget::icon::Handle>,
    already_saved: bool,
    is_current: bool,
    is_paused: bool,
) -> cosmic::Element<'a, RadioMessage> {
    let radius = cosmic::theme::active().cosmic().corner_radii.radius_s[0];
    let tile = station_tile(
        &result.name,
        &result.favicon,
        icons,
        56.0,
        radius,
        false,
        is_current,
    );

    let mut title_row = widget::Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(common::clipped_cell(
            common::cell_text(result.name.as_str()).into(),
        ));
    if is_current {
        title_row = title_row.push(live_pill(is_paused));
    }

    let mut meta = widget::Row::new().spacing(6).align_y(Alignment::Center);
    let country = if result.country.is_empty() {
        fl!("radio-unknown-country")
    } else {
        result.country.clone()
    };
    meta = meta.push(secondary_caption(country));
    let quality = match (result.codec.trim(), result.bitrate) {
        ("", 0) => String::new(),
        (codec, 0) => codec.to_uppercase(),
        ("", kbps) => format!("{kbps} kbps"),
        (codec, kbps) => format!("{} {kbps} kbps", codec.to_uppercase()),
    };
    if !quality.is_empty() {
        meta = meta.push(static_chip(quality, true));
    }
    for t in tag_list(&result.tags, 3) {
        meta = meta.push(static_chip(t, false));
    }

    let info = widget::Column::new()
        .push(title_row)
        .push(common::clipped_cell(meta.into()))
        .spacing(4);

    let uuid = result.stationuuid.clone();
    let save: cosmic::Element<'a, RadioMessage> = if already_saved {
        widget::Row::new()
            .push(widget::icon::from_name("object-select-symbolic").size(16))
            .push(secondary_caption(fl!("radio-saved")))
            .spacing(6)
            .align_y(Alignment::Center)
            .padding([0, 8])
            .into()
    } else {
        widget::button::custom(
            widget::Row::new()
                .push(widget::icon::from_name("list-add-symbolic").size(14))
                .push(widget::text::body(fl!("save")))
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .padding([4, 12])
        .on_press(RadioMessage::SaveResult(uuid.clone()))
        .class(pill_class(false, true))
        .into()
    };

    widget::tooltip(
        widget::button::custom(
            widget::Row::new()
                .push(tile)
                .push(info)
                .push(save)
                .spacing(12)
                .align_y(Alignment::Center)
                .padding(8),
        )
        .on_press(RadioMessage::PlayResult(uuid))
        .width(Length::Fill)
        .class(list_row_button_class(is_current)),
        widget::text::caption(if is_current {
            fl!("radio-now-playing-badge")
        } else {
            fl!("play-station-tooltip")
        }),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// Localized labels for the sort dropdown, in `SortOrder::ALL` order.
fn sort_labels() -> Vec<String> {
    vec![
        fl!("radio-sort-most-played"),
        fl!("radio-sort-most-voted"),
        fl!("radio-sort-name"),
        fl!("radio-sort-bitrate"),
    ]
}

fn sort_index(order: SortOrder) -> usize {
    SortOrder::ALL.iter().position(|o| *o == order).unwrap_or(0)
}
