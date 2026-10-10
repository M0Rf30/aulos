// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Podcasts view — subscribed shows ("Subscriptions") and an iTunes
//! directory search ("Discover"), behind a stable two-tab layout, plus a
//! show detail view listing episodes for a subscribed show.
//!
//! Mirrors `crate::views::radio`'s stable-tree-shape approach: every
//! reactive slot (add-by-URL card, Discover results) stays present at all
//! times — only its *content* changes with state. Messages carry the
//! podcast's/episode's stable db id, never a `Vec` position, so a reload,
//! re-sort or filter never leaves a message pointing at the wrong row.
//! Selecting a show swaps the whole list/tabs subtree for the detail
//! subtree — like every other library page's list/detail split (albums,
//! artists, playlists) — since that's an explicit user navigation, not
//! incidental reactive state.

use crate::fl;
use crate::online::podcast::PodcastSearchResult;
use crate::online::store::{Episode, Podcast};
use crate::views::common;
use crate::views::{card_button_class, list_row_button_class};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// Which of the two Podcasts tabs is shown. Kept in `AppModel` for the
/// session (not persisted to disk).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PodcastTab {
    #[default]
    Subscriptions,
    Discover,
}

/// Episode-list category filter, applied alongside the free-text filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EpisodeFilter {
    #[default]
    All,
    Unplayed,
    Downloaded,
}

/// Messages from the podcasts view. Every message that refers to a
/// specific show or episode carries its stable db id — never a `Vec`
/// position, which goes stale the moment a list reloads or is filtered.
#[derive(Debug, Clone)]
pub enum PodcastMessage {
    /// Switch between "Subscriptions" and "Discover".
    TabSelected(PodcastTab),

    // -- Subscriptions --
    /// Toggle the inline add-by-URL card open/closed.
    ToggleAddForm,
    AddUrlChanged(String),
    SubmitAdd,
    /// Open a subscribed show's episode list, by its db id.
    SelectPodcast(i64),
    /// Re-fetch a single show's feed, by its db id.
    RefreshPodcast(i64),
    /// Re-fetch every subscribed show's feed.
    RefreshAll,
    /// Ask for confirmation before unsubscribing from a show.
    StartUnsubscribe(i64),
    CancelUnsubscribe,
    /// Unsubscribe from a show, by its db id, after confirmation.
    ConfirmUnsubscribe(i64),

    // -- Discover --
    SearchChanged(String),
    SearchSubmit,
    RetrySearch,
    /// Subscribe to a directory result by its feed URL.
    SubscribeFromSearch(String),

    // -- Show detail --
    /// Return to the Subscriptions/Discover list.
    BackToList,
    ToggleDescriptionExpanded,
    EpisodeFilterSelected(EpisodeFilter),
    EpisodeTextFilterChanged(String),
    /// Play (or, if already the current episode, toggle play/pause) an
    /// episode by its db id. Resuming from a saved position is decided by
    /// the page controller, not the view.
    PlayEpisode(i64),
    /// Toggle the played marker for an episode by its db id.
    TogglePlayed(i64),
    /// Download an episode's enclosure for offline playback, by its db id.
    Download(i64),
    /// Delete an episode's downloaded local file, by its db id.
    DeleteDownload(i64),
}

/// Everything the podcasts view needs to render, borrowed from `AppModel`.
/// A struct rather than a long parameter list — this view has enough
/// independent bits of state (two list tabs, plus a detail view) that
/// positional args would be unreadable and error-prone to reorder.
pub struct PodcastViewProps<'a> {
    // Subscriptions
    pub podcasts: &'a [Podcast],
    pub tab: PodcastTab,
    pub add_open: bool,
    pub add_url: &'a str,
    pub add_error: Option<&'a str>,
    /// Podcast ids currently being refreshed (per-show spinner).
    pub refreshing: &'a HashSet<i64>,
    /// Podcast id awaiting an inline unsubscribe confirmation, if any.
    pub pending_unsubscribe: Option<i64>,

    // Discover
    pub search_query: &'a str,
    pub search_results: &'a [PodcastSearchResult],
    pub search_loading: bool,
    pub search_error: Option<&'a str>,

    // Show detail — `selected` is `Some` while showing a show's episodes.
    pub selected: Option<&'a Podcast>,
    pub episodes: &'a [Episode],
    pub episode_filter: EpisodeFilter,
    pub episode_text_filter: &'a str,
    pub description_expanded: bool,
    /// Episode ids currently downloading.
    pub downloading: &'a HashSet<i64>,
    pub current_episode_id: Option<i64>,
    /// Whether `current_episode_id`'s episode is the actual current
    /// track AND playback is active (Playing or Paused) — gates the
    /// now-playing row highlight so a stopped episode never shows as if
    /// it were still current.
    pub is_episode_playing: bool,
    pub is_paused: bool,

    // Shared
    pub icons: &'a HashMap<String, widget::icon::Handle>,
}

/// Minimum cover/label width of a show card; the fluid grid stretches
/// cards from this up to [`CARD_MAX_WIDTH`] so rows fill the view.
const CARD_WIDTH: f32 = 150.0;
const CARD_MAX_WIDTH: f32 = 210.0;
/// Padding of the card button around its artwork and labels.
const CARD_PADDING: f32 = 8.0;
/// Fixed two-line label block height (body + caption lines) so every card
/// in a row has the same height whether or not it has an author line.
const CARD_LABEL_HEIGHT: f32 = 40.0;
/// Artwork size in the show detail hero.
const HERO_ART_SIZE: f32 = 184.0;
/// Diameter of the round play/pause affordance on episode rows.
const PLAY_CIRCLE: f32 = 40.0;
/// Descriptions longer than this collapse behind a "Show more" toggle.
const DESCRIPTION_COLLAPSED_CHARS: usize = 240;

type Text<'a> = common::Text<'a>;

/// Dim a text widget to the theme's secondary (neutral_7) colour.
fn dim(text: Text<'_>) -> Text<'_> {
    text.class(cosmic::theme::Text::Custom(|theme| {
        cosmic::iced::widget::text::Style {
            color: Some(theme.cosmic().palette.neutral_7.into()),
            ..Default::default()
        }
    }))
}

/// Single-line caption in the secondary colour.
fn secondary_caption<'a>(content: impl Into<Cow<'a, str>> + 'a) -> Text<'a> {
    dim(common::cell_caption(content))
}

/// Single-line caption in the theme accent colour.
fn accent_caption<'a>(content: impl Into<Cow<'a, str>> + 'a) -> Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Custom(|theme| {
        cosmic::iced::widget::text::Style {
            color: Some(theme.cosmic().accent_color().into()),
            ..Default::default()
        }
    }))
}

/// A zero-size placeholder that keeps a reactive slot in the widget tree.
fn empty_slot<'a>() -> cosmic::Element<'a, PodcastMessage> {
    widget::Space::new()
        .width(Length::Shrink)
        .height(Length::Fixed(0.0))
        .into()
}

// ---------------------------------------------------------------------------
// Segmented control (tabs + episode filters)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Interaction {
    Idle,
    Hover,
    Down,
}

fn segment_style(selected: bool, theme: &cosmic::Theme, state: Interaction) -> ButtonStyle {
    let cosmic = theme.cosmic();
    let radius = cosmic.corner_radii.radius_xl;
    if selected {
        let c = &cosmic.accent_button;
        let bg = match state {
            Interaction::Idle => c.base,
            Interaction::Hover => c.hover,
            Interaction::Down => c.pressed,
        };
        ButtonStyle {
            background: Some(Background::Color(bg.into())),
            text_color: Some(c.on.into()),
            icon_color: Some(c.on.into()),
            border_radius: radius.into(),
            ..ButtonStyle::new()
        }
    } else {
        let c = &cosmic.background(false).component;
        let bg = match state {
            Interaction::Idle => None,
            Interaction::Hover => Some(c.hover),
            Interaction::Down => Some(c.pressed),
        };
        ButtonStyle {
            background: bg.map(|b| Background::Color(b.into())),
            text_color: Some(c.on.into()),
            icon_color: Some(c.on.into()),
            border_radius: radius.into(),
            ..ButtonStyle::new()
        }
    }
}

fn segment_class(selected: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| segment_style(selected, theme, Interaction::Idle)),
        hovered: Box::new(move |_focused, theme| {
            segment_style(selected, theme, Interaction::Hover)
        }),
        pressed: Box::new(move |_focused, theme| segment_style(selected, theme, Interaction::Down)),
        disabled: Box::new(move |theme| segment_style(selected, theme, Interaction::Idle)),
    }
}

/// Pill-shaped segmented control: a rounded track holding one button per
/// item, the selected one filled with the accent colour.
fn segmented<'a, M: Clone + 'static>(items: Vec<(String, bool, M)>) -> cosmic::Element<'a, M> {
    let mut row = widget::Row::new().spacing(2).align_y(Alignment::Center);
    for (label, selected, msg) in items {
        row = row.push(
            widget::button::custom(widget::text::body(label).wrapping(Wrapping::None))
                .padding([6, 16])
                .on_press(msg)
                .class(segment_class(selected)),
        );
    }
    widget::container(row)
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
        }))
        .into()
}

/// Small accent-filled count pill (unplayed badge on artwork).
fn count_pill<'a>(count: i64) -> cosmic::Element<'a, PodcastMessage> {
    let label = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    widget::container(
        widget::text::caption(label).class(cosmic::theme::Text::Custom(|theme| {
            cosmic::iced::widget::text::Style {
                color: Some(theme.cosmic().accent_button.on.into()),
                ..Default::default()
            }
        })),
    )
    .padding([2, 8])
    .class(cosmic::theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        cosmic::iced::widget::container::Style {
            background: Some(Background::Color(cosmic.accent_button.base.into())),
            border: cosmic::iced::Border {
                radius: cosmic.corner_radii.radius_xl.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }))
    .into()
}

// ---------------------------------------------------------------------------
// Page: header, tabs, add form
// ---------------------------------------------------------------------------

/// Render the full podcasts page: either the show detail view, or the
/// header + tabs + add-by-URL card + active tab's content.
pub fn podcast_view<'a>(props: PodcastViewProps<'a>) -> cosmic::Element<'a, PodcastMessage> {
    if let Some(podcast) = props.selected {
        return podcast_detail_view(podcast, &props);
    }

    let content = match props.tab {
        PodcastTab::Subscriptions => subscriptions_tab(&props),
        PodcastTab::Discover => discover_tab(&props),
    };
    widget::Column::new()
        .push(header_row(&props))
        .push(add_form_card(
            props.add_open,
            props.add_url,
            props.add_error,
        ))
        .push(content)
        .into()
}

/// Segmented tab switch + refresh-all + add-by-URL toggle. Always the same
/// widgets in the same order — only styles/state change.
fn header_row<'a>(props: &PodcastViewProps<'a>) -> cosmic::Element<'a, PodcastMessage> {
    let tabs = segmented(vec![
        (
            fl!("subscriptions"),
            props.tab == PodcastTab::Subscriptions,
            PodcastMessage::TabSelected(PodcastTab::Subscriptions),
        ),
        (
            fl!("podcast-tab-discover"),
            props.tab == PodcastTab::Discover,
            PodcastMessage::TabSelected(PodcastTab::Discover),
        ),
    ]);

    // Refresh-all only makes sense on the Subscriptions tab; the slot
    // stays (collapsed) on Discover so the row's shape never changes.
    let busy = !props.refreshing.is_empty();
    let refresh_all: cosmic::Element<'a, PodcastMessage> =
        if props.tab == PodcastTab::Subscriptions && !props.podcasts.is_empty() {
            let icon = if busy {
                "content-loading-symbolic"
            } else {
                "view-refresh-symbolic"
            };
            widget::tooltip(
                widget::button::icon(widget::icon::from_name(icon).size(16))
                    .on_press_maybe((!busy).then_some(PodcastMessage::RefreshAll)),
                widget::text::caption(fl!("refresh-all")),
                widget::tooltip::Position::Bottom,
            )
            .into()
        } else {
            empty_slot()
        };

    let add_btn = widget::button::standard(fl!("podcast-add-by-url"))
        .leading_icon(widget::icon::from_name("list-add-symbolic").size(16))
        .on_press(PodcastMessage::ToggleAddForm)
        .class(if props.add_open {
            cosmic::theme::Button::Suggested
        } else {
            cosmic::theme::Button::Standard
        });

    widget::Row::new()
        .push(tabs)
        .push(widget::Space::new().width(Length::Fill))
        .push(refresh_all)
        .push(add_btn)
        .spacing(8)
        .padding([16, 16, 12, 16])
        .align_y(Alignment::Center)
        .into()
}

/// The add-by-URL card's slot. Always present; empty (zero-height) when
/// closed, so opening/closing it never shifts anything below it.
fn add_form_card<'a>(
    open: bool,
    url: &'a str,
    error: Option<&'a str>,
) -> cosmic::Element<'a, PodcastMessage> {
    if !open {
        return empty_slot();
    }
    let fields = widget::Row::new()
        .push(
            widget::text_input(fl!("podcast-url-placeholder"), url)
                .on_input(PodcastMessage::AddUrlChanged)
                .on_submit(|_| PodcastMessage::SubmitAdd)
                .width(Length::Fill),
        )
        .push(widget::button::suggested(fl!("subscribe")).on_press(PodcastMessage::SubmitAdd))
        .push(
            widget::button::standard(fl!("podcast-cancel")).on_press(PodcastMessage::ToggleAddForm),
        )
        .spacing(8)
        .align_y(Alignment::Center);

    let mut card = widget::Column::new().spacing(8).push(fields);
    if let Some(err) = error {
        card = card.push(
            widget::Row::new()
                .push(widget::icon::from_name("dialog-error-symbolic").size(16))
                .push(
                    widget::text::caption(err).class(cosmic::theme::Text::Custom(|theme| {
                        cosmic::iced::widget::text::Style {
                            color: Some(theme.cosmic().destructive_color().into()),
                            ..Default::default()
                        }
                    })),
                )
                .spacing(6)
                .align_y(Alignment::Center),
        );
    }
    widget::container(
        widget::container(card)
            .class(cosmic::theme::Container::Card)
            .padding(12)
            .width(Length::Fill),
    )
    .padding([0, 16, 12, 16])
    .width(Length::Fill)
    .into()
}

// ---------------------------------------------------------------------------
// Subscriptions tab
// ---------------------------------------------------------------------------

/// "Subscriptions" tab: a fluid grid of show cards, or an inviting empty
/// state with the two ways to get started.
fn subscriptions_tab<'a>(props: &PodcastViewProps<'a>) -> cosmic::Element<'a, PodcastMessage> {
    if props.podcasts.is_empty() {
        return widget::container(
            widget::Column::new()
                .push(widget::icon::from_name("application-rss+xml-symbolic").size(64))
                .push(widget::text::title3(fl!("no-podcasts")))
                .push(dim(widget::text::body(fl!("podcasts-empty-hint"))))
                .push(
                    widget::Row::new()
                        .push(
                            widget::button::suggested(fl!("podcast-tab-discover"))
                                .leading_icon(
                                    widget::icon::from_name("system-search-symbolic").size(16),
                                )
                                .on_press(PodcastMessage::TabSelected(PodcastTab::Discover)),
                        )
                        .push(
                            widget::button::standard(fl!("podcast-add-by-url"))
                                .leading_icon(widget::icon::from_name("list-add-symbolic").size(16))
                                .on_press(PodcastMessage::ToggleAddForm),
                        )
                        .spacing(8),
                )
                .spacing(12)
                .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into();
    }

    let podcasts = props.podcasts;
    let icons = props.icons;
    let refreshing = props.refreshing;
    common::fluid_card_grid(
        podcasts.len(),
        CARD_WIDTH + 2.0 * CARD_PADDING,
        CARD_MAX_WIDTH + 2.0 * CARD_PADDING,
        move |index, outer| subscription_card(&podcasts[index], icons, refreshing, outer),
    )
}

fn subscription_card<'a>(
    podcast: &'a Podcast,
    icons: &'a HashMap<String, widget::icon::Handle>,
    refreshing: &HashSet<i64>,
    outer: f32,
) -> cosmic::Element<'a, PodcastMessage> {
    let art_size = outer - 2.0 * CARD_PADDING;
    let tile = common::grid_art_tile(
        icons.get(&podcast.image_url),
        art_size as u16,
        "application-rss+xml-symbolic",
    );
    let art: cosmic::Element<'a, PodcastMessage> = if podcast.unplayed_count > 0 {
        cosmic::iced::widget::Stack::new()
            .width(Length::Fixed(art_size))
            .height(Length::Fixed(art_size))
            .push(tile)
            .push(
                widget::container(widget::tooltip(
                    count_pill(podcast.unplayed_count),
                    widget::text::caption(fl!(
                        "podcast-unplayed-badge",
                        count = podcast.unplayed_count
                    )),
                    widget::tooltip::Position::Bottom,
                ))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Right)
                .align_y(Vertical::Top)
                .padding(8),
            )
            .into()
    } else {
        tile
    };

    let subtitle: String = if refreshing.contains(&podcast.id) {
        fl!("podcast-refreshing")
    } else if !podcast.author.trim().is_empty() {
        podcast.author.clone()
    } else {
        format_last_refreshed(podcast.last_refreshed)
    };

    let label = common::grid_card_label(
        art_size,
        CARD_LABEL_HEIGHT,
        common::clipped_cell(common::cell_text(podcast.title.as_str()).into()),
        common::clipped_cell(secondary_caption(subtitle).into()),
    );

    widget::tooltip(
        widget::button::custom(common::grid_card(art, art_size, label))
            .on_press(PodcastMessage::SelectPodcast(podcast.id))
            .padding(CARD_PADDING as u16)
            .class(card_button_class()),
        widget::text::caption(podcast.title.as_str()),
        widget::tooltip::Position::Top,
    )
    .into()
}

// ---------------------------------------------------------------------------
// Discover tab
// ---------------------------------------------------------------------------

/// "Discover" tab: search box, then a single fixed results slot (loading /
/// error+retry / intro / empty / grid) so the search input is never rebuilt.
fn discover_tab<'a>(props: &PodcastViewProps<'a>) -> cosmic::Element<'a, PodcastMessage> {
    let search = widget::Row::new()
        .push(
            widget::search_input(fl!("podcast-search-placeholder"), props.search_query)
                .on_input(PodcastMessage::SearchChanged)
                .on_submit(|_| PodcastMessage::SearchSubmit)
                .on_clear(PodcastMessage::SearchChanged(String::new()))
                .width(Length::Fill),
        )
        .push(widget::button::suggested(fl!("search")).on_press(PodcastMessage::SearchSubmit))
        .spacing(8)
        .padding([0, 16, 12, 16])
        .align_y(Alignment::Center);

    let body: cosmic::Element<'a, PodcastMessage> = if props.search_loading {
        common::empty_state(
            "view-refresh-symbolic",
            fl!("searching"),
            fl!("podcast-searching-hint"),
        )
    } else if let Some(err) = props.search_error {
        widget::container(
            widget::Column::new()
                .spacing(12)
                .align_x(Alignment::Center)
                .push(widget::icon::from_name("dialog-error-symbolic").size(48))
                .push(widget::text::body(err))
                .push(
                    widget::button::standard(fl!("podcast-retry"))
                        .on_press(PodcastMessage::RetrySearch),
                ),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
    } else if props.search_results.is_empty() {
        if props.search_query.trim().is_empty() {
            common::empty_state(
                "system-search-symbolic",
                fl!("podcast-discover-title"),
                fl!("podcast-discover-hint"),
            )
        } else {
            common::empty_state(
                "edit-find-symbolic",
                fl!("podcast-no-results"),
                fl!("podcast-no-results-hint"),
            )
        }
    } else {
        let results = props.search_results;
        let podcasts = props.podcasts;
        let icons = props.icons;
        common::fluid_card_grid(
            results.len(),
            CARD_WIDTH + 2.0 * CARD_PADDING,
            CARD_MAX_WIDTH + 2.0 * CARD_PADDING,
            move |index, outer| {
                let result = &results[index];
                let subscribed_id = podcasts
                    .iter()
                    .find(|p| p.feed_url == result.feed_url)
                    .map(|p| p.id);
                discover_card(result, icons, subscribed_id, outer)
            },
        )
    };

    widget::Column::new().push(search).push(body).into()
}

fn discover_card<'a>(
    result: &'a PodcastSearchResult,
    icons: &'a HashMap<String, widget::icon::Handle>,
    subscribed_id: Option<i64>,
    outer: f32,
) -> cosmic::Element<'a, PodcastMessage> {
    let art_size = outer - 2.0 * CARD_PADDING;
    let art = common::grid_art_tile(
        icons.get(&result.image),
        art_size as u16,
        "application-rss+xml-symbolic",
    );
    let label = common::grid_card_label(
        art_size,
        CARD_LABEL_HEIGHT,
        common::clipped_cell(common::cell_text(result.title.as_str()).into()),
        common::clipped_cell(secondary_caption(result.author.as_str()).into()),
    );

    let action: cosmic::Element<'a, PodcastMessage> = match subscribed_id {
        Some(id) => widget::tooltip(
            widget::button::standard(fl!("podcast-subscribed-badge"))
                .leading_icon(widget::icon::from_name("emblem-ok-symbolic").size(16))
                .on_press(PodcastMessage::SelectPodcast(id))
                .width(Length::Fixed(art_size)),
            widget::text::caption(fl!("podcast-open-show-tooltip")),
            widget::tooltip::Position::Top,
        )
        .into(),
        None => widget::button::suggested(fl!("subscribe"))
            .on_press(PodcastMessage::SubscribeFromSearch(result.feed_url.clone()))
            .width(Length::Fixed(art_size))
            .into(),
    };

    widget::container(
        widget::Column::new()
            .push(common::grid_card(art, art_size, label))
            .push(action)
            .spacing(8),
    )
    .padding(CARD_PADDING as u16)
    .into()
}

// ---------------------------------------------------------------------------
// Show detail
// ---------------------------------------------------------------------------

/// Render a subscribed show's detail: hero (art, title, author, stats,
/// play/refresh/unsubscribe), description, then the episodes section with
/// a segmented filter and search.
fn podcast_detail_view<'a>(
    podcast: &'a Podcast,
    props: &PodcastViewProps<'a>,
) -> cosmic::Element<'a, PodcastMessage> {
    let hero = common::hero_header(
        None,
        None,
        Some(common::hero_back_button(
            fl!("back-to-podcasts"),
            PodcastMessage::BackToList,
        )),
        detail_header(podcast, props),
    );

    let mut page = widget::Column::new()
        .push(hero)
        .push(unsubscribe_confirm(podcast, props.pending_unsubscribe))
        .spacing(16)
        .padding(16);
    if !podcast.description.trim().is_empty() {
        page = page.push(about_card(&podcast.description, props.description_expanded));
    }
    page = page.push(episodes_section(podcast, props));

    widget::scrollable(page).height(Length::Fill).into()
}

/// Pick the episode the hero's primary button acts on: the most recent
/// partly-played one ("Continue listening"), else the newest episode
/// ("Play latest").
fn primary_episode(episodes: &[Episode]) -> Option<(&Episode, bool)> {
    episodes
        .iter()
        .find(|e| should_show_resume(e))
        .map(|e| (e, true))
        .or_else(|| episodes.first().map(|e| (e, false)))
}

fn detail_header<'a>(
    podcast: &'a Podcast,
    props: &PodcastViewProps<'a>,
) -> cosmic::Element<'a, PodcastMessage> {
    let art = common::grid_art_tile(
        props.icons.get(&podcast.image_url),
        HERO_ART_SIZE as u16,
        "application-rss+xml-symbolic",
    );

    let mut meta = widget::Column::new().spacing(8).width(Length::Fill);
    meta = meta.push(
        widget::container(common::clipped_cell(
            widget::text::title2(podcast.title.as_str())
                .wrapping(Wrapping::None)
                .into(),
        ))
        .width(Length::Fill),
    );
    if !podcast.author.trim().is_empty() {
        meta = meta.push(common::clipped_cell(
            dim(common::cell_text(podcast.author.as_str())).into(),
        ));
    }

    let episode_total = props.episodes.len() as i64;
    let mut stats: Vec<String> = Vec::new();
    if !props.episodes.is_empty() {
        stats.push(fl!("podcast-episode-count", count = episode_total));
    }
    if podcast.unplayed_count > 0 {
        stats.push(fl!(
            "podcast-unplayed-badge",
            count = podcast.unplayed_count
        ));
    }
    stats.push(format_last_refreshed(podcast.last_refreshed));
    meta = meta.push(common::clipped_cell(
        secondary_caption(stats.join("  \u{b7}  ")).into(),
    ));

    let refreshing = props.refreshing.contains(&podcast.id);
    let mut actions = widget::Row::new().spacing(8).align_y(Alignment::Center);
    if let Some((episode, in_progress)) = primary_episode(props.episodes) {
        let is_current = props.is_episode_playing && props.current_episode_id == Some(episode.id);
        let (icon, label) = if is_current && !props.is_paused {
            ("media-playback-pause-symbolic", fl!("pause"))
        } else if in_progress {
            ("media-playback-start-symbolic", fl!("podcast-continue"))
        } else {
            ("media-playback-start-symbolic", fl!("podcast-play-latest"))
        };
        actions = actions.push(
            widget::button::suggested(label)
                .leading_icon(widget::icon::from_name(icon).size(16))
                .on_press(PodcastMessage::PlayEpisode(episode.id)),
        );
    }
    actions = actions
        .push(
            widget::button::standard(if refreshing {
                fl!("podcast-refreshing")
            } else {
                fl!("podcast-refresh")
            })
            .leading_icon(widget::icon::from_name("view-refresh-symbolic").size(16))
            .on_press_maybe((!refreshing).then_some(PodcastMessage::RefreshPodcast(podcast.id))),
        )
        .push(
            widget::button::standard(fl!("podcast-unsubscribe"))
                .on_press(PodcastMessage::StartUnsubscribe(podcast.id)),
        );
    meta = meta.push(actions);

    widget::Row::new()
        .push(art)
        .push(meta)
        .spacing(24)
        .align_y(Alignment::Center)
        .into()
}

/// Inline unsubscribe confirmation, in a slot that is collapsed unless a
/// confirmation is pending for this show.
fn unsubscribe_confirm<'a>(
    podcast: &'a Podcast,
    pending: Option<i64>,
) -> cosmic::Element<'a, PodcastMessage> {
    if pending != Some(podcast.id) {
        return empty_slot();
    }
    widget::container(
        widget::Row::new()
            .push(widget::icon::from_name("dialog-warning-symbolic").size(24))
            .push(common::clipped_cell(
                common::cell_text(fl!(
                    "podcast-confirm-unsubscribe",
                    name = podcast.title.clone()
                ))
                .into(),
            ))
            .push(
                widget::button::standard(fl!("podcast-cancel"))
                    .on_press(PodcastMessage::CancelUnsubscribe),
            )
            .push(
                widget::button::destructive(fl!("podcast-confirm-unsubscribe-yes"))
                    .on_press(PodcastMessage::ConfirmUnsubscribe(podcast.id)),
            )
            .spacing(12)
            .align_y(Alignment::Center),
    )
    .class(cosmic::theme::Container::Card)
    .padding(12)
    .width(Length::Fill)
    .into()
}

/// "About" card: the show description, collapsed to a few lines with a
/// "Show more" toggle when it is long.
fn about_card<'a>(description: &'a str, expanded: bool) -> cosmic::Element<'a, PodcastMessage> {
    let long = description.chars().count() > DESCRIPTION_COLLAPSED_CHARS;
    let text: cosmic::Element<'a, PodcastMessage> = if expanded || !long {
        widget::text::body(description).into()
    } else {
        widget::text::body(common::truncate_str(
            description,
            DESCRIPTION_COLLAPSED_CHARS,
        ))
        .into()
    };

    let mut col = widget::Column::new()
        .push(widget::text::heading(fl!("podcast-about")))
        .push(text)
        .spacing(8);
    if long {
        col = col.push(
            widget::button::link(if expanded {
                fl!("podcast-description-less")
            } else {
                fl!("podcast-description-more")
            })
            .padding(0)
            .on_press(PodcastMessage::ToggleDescriptionExpanded),
        );
    }
    widget::container(col)
        .class(cosmic::theme::Container::Card)
        .padding(16)
        .width(Length::Fill)
        .into()
}

/// Inline (non-expanding) state block for use inside a scrollable.
fn inline_state<'a>(
    icon: &'static str,
    title: String,
    hint: String,
) -> cosmic::Element<'a, PodcastMessage> {
    widget::container(
        widget::Column::new()
            .push(widget::icon::from_name(icon).size(48))
            .push(widget::text::title4(title))
            .push(dim(widget::text::body(hint)))
            .spacing(8)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([32, 0])
    .align_x(Horizontal::Center)
    .into()
}

fn episodes_section<'a>(
    podcast: &'a Podcast,
    props: &PodcastViewProps<'a>,
) -> cosmic::Element<'a, PodcastMessage> {
    let episode_total = props.episodes.len() as i64;
    let heading = widget::Row::new()
        .push(widget::text::title3(fl!("podcast-episodes")))
        .push(secondary_caption(if props.episodes.is_empty() {
            String::new()
        } else {
            fl!("podcast-episode-count", count = episode_total)
        }))
        .spacing(10)
        .align_y(Alignment::Center);

    let filters = widget::Row::new()
        .push(segmented(vec![
            (
                fl!("podcast-filter-all"),
                props.episode_filter == EpisodeFilter::All,
                PodcastMessage::EpisodeFilterSelected(EpisodeFilter::All),
            ),
            (
                fl!("podcast-filter-unplayed"),
                props.episode_filter == EpisodeFilter::Unplayed,
                PodcastMessage::EpisodeFilterSelected(EpisodeFilter::Unplayed),
            ),
            (
                fl!("podcast-filter-downloaded"),
                props.episode_filter == EpisodeFilter::Downloaded,
                PodcastMessage::EpisodeFilterSelected(EpisodeFilter::Downloaded),
            ),
        ]))
        .push(
            widget::search_input(
                fl!("podcast-episode-filter-placeholder"),
                props.episode_text_filter,
            )
            .on_input(PodcastMessage::EpisodeTextFilterChanged)
            .on_clear(PodcastMessage::EpisodeTextFilterChanged(String::new()))
            .width(Length::Fill),
        )
        .spacing(12)
        .align_y(Alignment::Center);

    let filtered: Vec<&Episode> = props
        .episodes
        .iter()
        .filter(|ep| episode_matches(ep, props.episode_filter, props.episode_text_filter))
        .collect();

    let mut list = widget::Column::new().spacing(2);
    if props.episodes.is_empty() {
        list = list.push(if props.refreshing.contains(&podcast.id) {
            inline_state(
                "view-refresh-symbolic",
                fl!("podcast-loading-episodes"),
                fl!("podcast-loading-episodes-hint"),
            )
        } else {
            inline_state(
                "application-rss+xml-symbolic",
                fl!("no-episodes"),
                fl!("no-episodes-hint"),
            )
        });
    } else if filtered.is_empty() {
        list = list.push(inline_state(
            "edit-find-symbolic",
            fl!("podcast-no-filter-matches"),
            fl!("podcast-no-filter-matches-hint"),
        ));
    } else {
        for episode in filtered {
            let is_current =
                props.is_episode_playing && props.current_episode_id == Some(episode.id);
            list = list.push(episode_row(
                episode,
                props.downloading,
                is_current,
                props.is_paused,
            ));
        }
    }

    widget::Column::new()
        .push(heading)
        .push(filters)
        .push(list)
        .spacing(12)
        .into()
}

/// Round play/pause affordance at the start of an episode row: accent
/// filled for the current episode, a quiet surface disc otherwise.
fn play_circle<'a>(is_current: bool, is_paused: bool) -> cosmic::Element<'a, PodcastMessage> {
    let icon = if is_current && !is_paused {
        "media-playback-pause-symbolic"
    } else {
        "media-playback-start-symbolic"
    };
    widget::container(widget::icon::from_name(icon).size(20))
        .width(Length::Fixed(PLAY_CIRCLE))
        .height(Length::Fixed(PLAY_CIRCLE))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::custom(move |theme| {
            let cosmic = theme.cosmic();
            let (bg, fg) = if is_current {
                (cosmic.accent_button.base, cosmic.accent_button.on)
            } else {
                (
                    cosmic.background(false).component.base,
                    cosmic.background(false).component.on,
                )
            };
            cosmic::iced::widget::container::Style {
                icon_color: Some(fg.into()),
                text_color: Some(fg.into()),
                background: Some(Background::Color(bg.into())),
                border: cosmic::iced::Border {
                    radius: (PLAY_CIRCLE / 2.0).into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

fn episode_row<'a>(
    episode: &'a Episode,
    downloading: &HashSet<i64>,
    is_current: bool,
    is_paused: bool,
) -> cosmic::Element<'a, PodcastMessage> {
    let resumable = should_show_resume(episode);

    // Title: unplayed/current are emphasised, played episodes recede.
    let title = common::cell_text(episode.title.as_str());
    let title = if episode.played && !is_current {
        dim(title)
    } else {
        title.font(cosmic::font::semibold())
    };

    // Meta line: [now playing] date · duration (or time left).
    let mut parts: Vec<String> = Vec::new();
    let date = format_pub_date(episode.pub_date);
    if !date.is_empty() {
        parts.push(date);
    }
    let duration = episode.duration_secs.max(0) as u64;
    if resumable && duration > 0 {
        let left = duration.saturating_sub((episode.position_ms / 1000).max(0) as u64);
        parts.push(fl!(
            "podcast-time-left",
            time = common::format_duration_coarse(left)
        ));
    } else if resumable {
        parts.push(fl!(
            "resume-at",
            position = common::format_duration_coarse((episode.position_ms / 1000).max(0) as u64)
        ));
    } else if duration > 0 {
        parts.push(common::format_duration_coarse(duration));
    }
    let mut meta = widget::Row::new().spacing(8).align_y(Alignment::Center);
    if is_current {
        meta = meta.push(accent_caption(fl!("podcast-now-playing-badge")));
    }
    meta = meta.push(secondary_caption(parts.join("  \u{b7}  ")));

    let mut info = widget::Column::new().push(title).push(meta).spacing(2);
    if resumable && duration > 0 {
        info = info.push(
            widget::container(
                widget::progress_bar::determinate_linear(progress_fraction(episode)).girth(4),
            )
            .width(Length::Fixed(220.0)),
        );
    }

    let played_icon = if episode.played {
        "emblem-ok-symbolic"
    } else {
        "emblem-default-symbolic"
    };
    let played_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name(played_icon).size(16))
            .on_press(PodcastMessage::TogglePlayed(episode.id)),
        widget::text::caption(fl!("mark-played-tooltip")),
        widget::tooltip::Position::Top,
    );

    // Download state: fixed-width slot so the columns line up whichever
    // state a row is in.
    let download_control: cosmic::Element<'a, PodcastMessage> =
        if !episode.downloaded_path.is_empty() {
            widget::tooltip(
                widget::button::icon(widget::icon::from_name("emblem-downloads-symbolic").size(16))
                    .on_press(PodcastMessage::DeleteDownload(episode.id)),
                widget::text::caption(fl!("delete-download-tooltip")),
                widget::tooltip::Position::Top,
            )
            .into()
        } else if downloading.contains(&episode.id) {
            widget::tooltip(
                widget::button::icon(widget::icon::from_name("content-loading-symbolic").size(16))
                    .on_press_maybe(None::<PodcastMessage>),
                widget::text::caption(fl!("podcast-downloading-tooltip")),
                widget::tooltip::Position::Top,
            )
            .into()
        } else {
            widget::tooltip(
                widget::button::icon(widget::icon::from_name("document-save-symbolic").size(16))
                    .on_press(PodcastMessage::Download(episode.id)),
                widget::text::caption(fl!("download-episode-tooltip")),
                widget::tooltip::Position::Top,
            )
            .into()
        };

    widget::button::custom(
        widget::Row::new()
            .push(play_circle(is_current, is_paused))
            .push(common::clipped_cell(info.into()))
            .push(played_btn)
            .push(download_control)
            .spacing(12)
            .width(Length::Fill)
            .align_y(Alignment::Center)
            .padding([8, 10]),
    )
    .on_press(PodcastMessage::PlayEpisode(episode.id))
    .width(Length::Fill)
    .class(list_row_button_class(is_current))
    .into()
}

/// Pure category + free-text episode filter predicate. Shared by the view
/// and its tests.
pub fn episode_matches(episode: &Episode, filter: EpisodeFilter, text: &str) -> bool {
    let category_ok = match filter {
        EpisodeFilter::All => true,
        EpisodeFilter::Unplayed => !episode.played,
        EpisodeFilter::Downloaded => !episode.downloaded_path.is_empty(),
    };
    if !category_ok {
        return false;
    }
    let needle = text.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    episode.title.to_lowercase().contains(&needle)
        || episode.description.to_lowercase().contains(&needle)
}

/// Progress fraction (0.0-1.0) for an episode's playback progress bar.
/// Fully played episodes always report 1.0 regardless of the last saved
/// position; an unknown/zero duration (missing feed metadata) always
/// reports 0.0 rather than dividing by zero.
pub fn progress_fraction(episode: &Episode) -> f32 {
    if episode.played {
        return 1.0;
    }
    if episode.duration_secs <= 0 || episode.position_ms <= 0 {
        return 0.0;
    }
    let duration_ms = episode.duration_secs as f64 * 1000.0;
    ((episode.position_ms as f64) / duration_ms).clamp(0.0, 1.0) as f32
}

/// Whether an episode has a meaningful saved position to resume from
/// (partway through, not already marked played).
pub fn should_show_resume(episode: &Episode) -> bool {
    !episode.played && episode.position_ms > 0
}

/// Format an episode's `pub_date` (epoch seconds) as `YYYY-MM-DD`, or an
/// empty string when unset.
fn format_pub_date(epoch_secs: i64) -> String {
    format_epoch_date(epoch_secs).unwrap_or_default()
}

/// A subscribed show's `last_refreshed` (epoch seconds), formatted as a
/// localized "Updated <date>" caption, or "Never updated" when it hasn't
/// been refreshed yet.
fn format_last_refreshed(epoch_secs: i64) -> String {
    match format_epoch_date(epoch_secs) {
        Some(date) => fl!("podcast-updated-at", when = date),
        None => fl!("podcast-never-updated"),
    }
}

/// Pure epoch-seconds-to-`YYYY-MM-DD` formatting shared by
/// `format_pub_date` and `format_last_refreshed`. Returns `None` for a
/// non-positive/unset epoch.
fn format_epoch_date(epoch_secs: i64) -> Option<String> {
    if epoch_secs <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(epoch_secs, 0).map(|dt| dt.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(
        played: bool,
        position_ms: i64,
        duration_secs: i64,
        downloaded_path: &str,
        title: &str,
        description: &str,
    ) -> Episode {
        Episode {
            id: 1,
            podcast_id: 1,
            guid: "guid".to_string(),
            title: title.to_string(),
            enclosure_url: String::new(),
            mime: String::new(),
            duration_secs,
            pub_date: 0,
            description: description.to_string(),
            position_ms,
            played,
            downloaded_path: downloaded_path.to_string(),
        }
    }

    #[test]
    fn progress_fraction_is_zero_for_untouched_episode() {
        assert_eq!(progress_fraction(&ep(false, 0, 600, "", "t", "")), 0.0);
    }

    #[test]
    fn progress_fraction_reflects_partial_position() {
        let e = ep(false, 150_000, 600, "", "t", ""); // 150s of 600s = 25%
        assert!((progress_fraction(&e) - 0.25).abs() < 0.001);
    }

    #[test]
    fn progress_fraction_clamps_when_position_exceeds_duration() {
        let e = ep(false, 900_000, 600, "", "t", "");
        assert_eq!(progress_fraction(&e), 1.0);
    }

    #[test]
    fn progress_fraction_is_one_when_marked_played_even_with_no_position() {
        assert_eq!(progress_fraction(&ep(true, 0, 600, "", "t", "")), 1.0);
    }

    #[test]
    fn progress_fraction_is_zero_for_unknown_duration() {
        assert_eq!(progress_fraction(&ep(false, 1000, 0, "", "t", "")), 0.0);
    }

    #[test]
    fn should_show_resume_true_only_when_partway_and_not_played() {
        assert!(should_show_resume(&ep(false, 1000, 600, "", "t", "")));
        assert!(!should_show_resume(&ep(true, 1000, 600, "", "t", "")));
        assert!(!should_show_resume(&ep(false, 0, 600, "", "t", "")));
    }

    #[test]
    fn episode_matches_filters_by_category() {
        let unplayed = ep(false, 0, 600, "", "Title", "desc");
        let played = ep(true, 0, 600, "", "Title", "desc");
        let downloaded = ep(false, 0, 600, "/tmp/x.mp3", "Title", "desc");
        assert!(episode_matches(&unplayed, EpisodeFilter::All, ""));
        assert!(episode_matches(&unplayed, EpisodeFilter::Unplayed, ""));
        assert!(!episode_matches(&played, EpisodeFilter::Unplayed, ""));
        assert!(episode_matches(&downloaded, EpisodeFilter::Downloaded, ""));
        assert!(!episode_matches(&unplayed, EpisodeFilter::Downloaded, ""));
    }

    #[test]
    fn episode_matches_filters_by_text_case_insensitively() {
        let e = ep(false, 0, 600, "", "Rust Talk Show", "About programming");
        assert!(episode_matches(&e, EpisodeFilter::All, "rust"));
        assert!(episode_matches(&e, EpisodeFilter::All, "PROGRAMMING"));
        assert!(!episode_matches(&e, EpisodeFilter::All, "python"));
    }

    #[test]
    fn episode_matches_combines_category_and_text_filters() {
        let played_match = ep(true, 0, 600, "", "Rust Talk", "");
        assert!(!episode_matches(
            &played_match,
            EpisodeFilter::Unplayed,
            "rust"
        ));
    }

    #[test]
    fn format_pub_date_formats_epoch_seconds_or_empty() {
        assert_eq!(format_pub_date(1704067200), "2024-01-01");
        assert_eq!(format_pub_date(0), "");
        assert_eq!(format_pub_date(-10), "");
    }

    #[test]
    fn format_epoch_date_reports_none_when_unset_or_negative() {
        assert_eq!(format_epoch_date(0), None);
        assert_eq!(format_epoch_date(-5), None);
        assert_eq!(
            format_epoch_date(1704067200),
            Some("2024-01-01".to_string())
        );
    }
}
