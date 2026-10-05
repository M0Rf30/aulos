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
use crate::views::list_row_button_class;
use cosmic::iced::core::Color;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
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

fn podcast_icon<'a, M: 'a + 'static>(
    image_url: &str,
    icons: &HashMap<String, widget::icon::Handle>,
    size: u16,
) -> cosmic::Element<'a, M> {
    common::list_art_icon(icons.get(image_url), size, "application-rss+xml-symbolic")
}

/// Render the full podcasts page: either the show detail view, or the
/// header + tabs + add-by-URL card + active tab's content.
pub fn podcast_view<'a>(props: PodcastViewProps<'a>) -> cosmic::Element<'a, PodcastMessage> {
    if let Some(podcast) = props.selected {
        return podcast_detail_view(podcast, &props);
    }

    let mut col = widget::Column::new().spacing(12).padding(16);
    col = col.push(header_row(props.tab, props.add_open));
    col = col.push(add_form_card(props.add_open, props.add_url, props.add_error));
    col = col.push(widget::divider::horizontal::default());

    let content = match props.tab {
        PodcastTab::Subscriptions => subscriptions_tab(&props),
        PodcastTab::Discover => discover_tab(&props),
    };
    col = col.push(content);
    col.into()
}

/// Title + tab switch + add-by-URL toggle. Always the same widgets in the
/// same order — only styles/state change.
fn header_row<'a>(tab: PodcastTab, add_open: bool) -> cosmic::Element<'a, PodcastMessage> {
    let add_btn = widget::button::text(fl!("podcast-add-by-url"))
        .on_press(PodcastMessage::ToggleAddForm)
        .class(if add_open { cosmic::theme::Button::Suggested } else { cosmic::theme::Button::Standard });
    widget::Row::new()
        .push(widget::text::title3(fl!("podcasts")))
        .push(widget::Space::new().width(Length::Fill))
        .push(tab_button(fl!("subscriptions"), tab == PodcastTab::Subscriptions, PodcastTab::Subscriptions))
        .push(tab_button(fl!("podcast-tab-discover"), tab == PodcastTab::Discover, PodcastTab::Discover))
        .push(add_btn)
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
}

fn tab_button<'a>(label: String, selected: bool, target: PodcastTab) -> cosmic::Element<'a, PodcastMessage> {
    let btn = widget::button::text(label).on_press(PodcastMessage::TabSelected(target));
    if selected { btn.class(cosmic::theme::Button::Suggested) } else { btn.class(cosmic::theme::Button::Standard) }
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
        return widget::Space::new().width(Length::Shrink).height(Length::Fixed(0.0)).into();
    }
    let fields = widget::Row::new()
        .push(
            widget::text_input(fl!("podcast-url-placeholder"), url)
                .on_input(PodcastMessage::AddUrlChanged)
                .on_submit(|_| PodcastMessage::SubmitAdd)
                .width(Length::Fill),
        )
        .push(widget::button::suggested(fl!("subscribe")).on_press(PodcastMessage::SubmitAdd))
        .push(widget::button::standard(fl!("podcast-cancel")).on_press(PodcastMessage::ToggleAddForm))
        .spacing(8)
        .align_y(Alignment::Center);

    let mut card = widget::Column::new().spacing(6).push(fields);
    if let Some(err) = error {
        card = card.push(
            widget::text::caption(err).class(cosmic::theme::Text::Color(Color::from_rgb(0.9, 0.2, 0.2))),
        );
    }
    widget::container(card)
        .class(cosmic::theme::Container::Card)
        .padding(12)
        .width(Length::Fill)
        .into()
}

/// "Subscriptions" tab: refresh-all header, then either the empty state
/// or the subscribed-shows list.
fn subscriptions_tab<'a>(props: &PodcastViewProps<'a>) -> cosmic::Element<'a, PodcastMessage> {
    let mut col = widget::Column::new().spacing(8);
    col = col.push(
        widget::Row::new()
            .push(widget::text::title4(fl!("subscriptions")))
            .push(widget::Space::new().width(Length::Fill))
            .push(widget::button::standard(fl!("refresh-all")).on_press(PodcastMessage::RefreshAll))
            .align_y(Alignment::Center),
    );

    if props.podcasts.is_empty() {
        col = col.push(common::empty_state(
            "application-rss+xml-symbolic",
            fl!("no-podcasts"),
            fl!("podcasts-empty-hint"),
        ));
        return col.into();
    }

    let mut list = widget::Column::new().spacing(2);
    for podcast in props.podcasts {
        list = list.push(subscription_row(podcast, props));
    }
    col = col.push(widget::scrollable(widget::container(list).width(Length::Fill)).height(Length::Fill));
    col.into()
}

fn subscription_row<'a>(
    podcast: &'a Podcast,
    props: &PodcastViewProps<'a>,
) -> cosmic::Element<'a, PodcastMessage> {
    if props.pending_unsubscribe == Some(podcast.id) {
        return unsubscribe_confirm_row(podcast);
    }

    let mut caption_parts: Vec<String> = Vec::new();
    if !podcast.author.is_empty() {
        caption_parts.push(podcast.author.clone());
    }
    caption_parts.push(format_last_refreshed(podcast.last_refreshed));
    let info = widget::Column::new()
        .push(common::cell_text(podcast.title.as_str()))
        .push(common::cell_caption(caption_parts.join("  ·  ")))
        .spacing(2);

    let badge: cosmic::Element<'a, PodcastMessage> = if podcast.unplayed_count > 0 {
        widget::container(common::cell_caption(fl!(
            "podcast-unplayed-badge",
            count = podcast.unplayed_count
        )))
        .class(cosmic::theme::Container::Card)
        .padding(6)
        .into()
    } else {
        widget::Space::new().width(0).height(0).into()
    };

    let refreshing = props.refreshing.contains(&podcast.id);
    let refresh_icon = if refreshing { "content-loading-symbolic" } else { "view-refresh-symbolic" };
    let refresh_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name(refresh_icon).size(16))
            .on_press_maybe((!refreshing).then_some(PodcastMessage::RefreshPodcast(podcast.id))),
        widget::text::caption(fl!("refresh-podcast-tooltip")),
        widget::tooltip::Position::Top,
    );
    let remove_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name("edit-delete-symbolic").size(16))
            .class(cosmic::theme::Button::Destructive)
            .on_press(PodcastMessage::StartUnsubscribe(podcast.id)),
        widget::text::caption(fl!("unsubscribe-tooltip")),
        widget::tooltip::Position::Top,
    );

    widget::button::custom(
        widget::Row::new()
            .push(podcast_icon(&podcast.image_url, props.icons, 40))
            .push(common::clipped_cell(info.into()))
            .push(badge)
            .push(refresh_btn)
            .push(remove_btn)
            .spacing(12)
            .align_y(Alignment::Center)
            .padding(8),
    )
    .on_press(PodcastMessage::SelectPodcast(podcast.id))
    .width(Length::Fill)
    .class(list_row_button_class(false))
    .into()
}

/// Replaces a subscription row with an inline unsubscribe confirmation, at
/// the same list position — only the row being confirmed changes shape.
fn unsubscribe_confirm_row<'a>(podcast: &'a Podcast) -> cosmic::Element<'a, PodcastMessage> {
    widget::Row::new()
        .push(common::clipped_cell(
            common::cell_text(fl!("podcast-confirm-unsubscribe", name = podcast.title.clone())).into(),
        ))
        .push(widget::button::standard(fl!("podcast-cancel")).on_press(PodcastMessage::CancelUnsubscribe))
        .push(
            widget::button::standard(fl!("podcast-confirm-unsubscribe-yes"))
                .class(cosmic::theme::Button::Destructive)
                .on_press(PodcastMessage::ConfirmUnsubscribe(podcast.id)),
        )
        .spacing(8)
        .align_y(Alignment::Center)
        .padding(8)
        .into()
}

/// "Discover" tab: search box, then a single fixed results slot (loading /
/// error+retry / empty / list) so the scrollable itself is always present.
fn discover_tab<'a>(props: &PodcastViewProps<'a>) -> cosmic::Element<'a, PodcastMessage> {
    let mut col = widget::Column::new().spacing(8);
    col = col.push(
        widget::Row::new()
            .push(
                widget::text_input(fl!("podcast-search-placeholder"), props.search_query)
                    .on_input(PodcastMessage::SearchChanged)
                    .on_submit(|_| PodcastMessage::SearchSubmit)
                    .width(Length::Fill),
            )
            .push(widget::button::standard(fl!("search")).on_press(PodcastMessage::SearchSubmit))
            .spacing(8)
            .align_y(Alignment::Center),
    );

    let body: cosmic::Element<'a, PodcastMessage> = if props.search_loading {
        common::empty_state("view-refresh-symbolic", fl!("searching"), fl!("podcast-searching-hint"))
    } else if let Some(err) = props.search_error {
        widget::Column::new()
            .spacing(8)
            .align_x(Alignment::Center)
            .push(widget::icon::from_name("dialog-error-symbolic").size(48))
            .push(common::cell_text(err))
            .push(widget::button::standard(fl!("podcast-retry")).on_press(PodcastMessage::RetrySearch))
            .into()
    } else if props.search_results.is_empty() {
        common::empty_state("edit-find-symbolic", fl!("podcast-no-results"), fl!("podcast-no-results-hint"))
    } else {
        let mut list = widget::Column::new().spacing(2);
        for result in props.search_results {
            let subscribed = props.podcasts.iter().any(|p| p.feed_url == result.feed_url);
            list = list.push(discover_result_row(result, props.icons, subscribed));
        }
        widget::container(list).width(Length::Fill).into()
    };
    col = col.push(widget::scrollable(body).height(Length::Fill).width(Length::Fill));
    col.into()
}

fn discover_result_row<'a>(
    result: &'a PodcastSearchResult,
    icons: &'a HashMap<String, widget::icon::Handle>,
    subscribed: bool,
) -> cosmic::Element<'a, PodcastMessage> {
    let info = widget::Column::new()
        .push(common::cell_text(result.title.as_str()))
        .push(common::cell_caption(result.author.as_str()))
        .spacing(2);

    let action: cosmic::Element<'a, PodcastMessage> = if subscribed {
        widget::button::standard(fl!("podcast-subscribed-badge"))
            .class(cosmic::theme::Button::Text)
            .into()
    } else {
        widget::button::suggested(fl!("subscribe"))
            .on_press(PodcastMessage::SubscribeFromSearch(result.feed_url.clone()))
            .into()
    };

    widget::container(
        widget::Row::new()
            .push(podcast_icon(&result.image, icons, 40))
            .push(common::clipped_cell(info.into()))
            .push(action)
            .spacing(12)
            .align_y(Alignment::Center)
            .padding(8),
    )
    .width(Length::Fill)
    .into()
}

/// Render a subscribed show's detail: header (art, title, author,
/// description, refresh/unsubscribe), episode filter chips + text filter,
/// then the episode list.
fn podcast_detail_view<'a>(
    podcast: &'a Podcast,
    props: &PodcastViewProps<'a>,
) -> cosmic::Element<'a, PodcastMessage> {
    let header = detail_header(podcast, props);

    let filters_row = widget::Row::new()
        .push(filter_chip(fl!("podcast-filter-all"), props.episode_filter == EpisodeFilter::All, EpisodeFilter::All))
        .push(filter_chip(
            fl!("podcast-filter-unplayed"),
            props.episode_filter == EpisodeFilter::Unplayed,
            EpisodeFilter::Unplayed,
        ))
        .push(filter_chip(
            fl!("podcast-filter-downloaded"),
            props.episode_filter == EpisodeFilter::Downloaded,
            EpisodeFilter::Downloaded,
        ))
        .push(
            widget::text_input(fl!("podcast-episode-filter-placeholder"), props.episode_text_filter)
                .on_input(PodcastMessage::EpisodeTextFilterChanged)
                .width(Length::FillPortion(2)),
        )
        .spacing(6)
        .align_y(Alignment::Center);

    let filtered: Vec<&Episode> = props
        .episodes
        .iter()
        .filter(|ep| episode_matches(ep, props.episode_filter, props.episode_text_filter))
        .collect();

    let mut episode_list = widget::Column::new().spacing(2);
    if props.episodes.is_empty() {
        episode_list = episode_list.push(common::empty_state(
            "application-rss+xml-symbolic",
            fl!("no-episodes"),
            fl!("no-episodes-hint"),
        ));
    } else if filtered.is_empty() {
        episode_list = episode_list.push(common::empty_state(
            "edit-find-symbolic",
            fl!("podcast-no-filter-matches"),
            fl!("podcast-no-filter-matches-hint"),
        ));
    } else {
        for episode in filtered {
            let is_current = props.is_episode_playing && props.current_episode_id == Some(episode.id);
            episode_list = episode_list.push(episode_row(episode, props.downloading, is_current, props.is_paused));
        }
    }

    widget::scrollable(
        widget::Column::new()
            .push(header)
            .push(widget::divider::horizontal::default())
            .push(filters_row)
            .push(episode_list)
            .spacing(16)
            .padding(16),
    )
    .height(Length::Fill)
    .into()
}

fn detail_header<'a>(
    podcast: &'a Podcast,
    props: &PodcastViewProps<'a>,
) -> cosmic::Element<'a, PodcastMessage> {
    let back_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
            .on_press(PodcastMessage::BackToList),
        widget::text::caption(fl!("back-to-podcasts")),
        widget::tooltip::Position::Top,
    );

    let mut info = widget::Column::new().push(widget::text::title1(podcast.title.as_str())).spacing(8).width(Length::Fill);
    if !podcast.author.is_empty() {
        info = info.push(common::cell_caption(podcast.author.as_str()));
    }
    if !podcast.description.is_empty() {
        info = info.push(description_block(&podcast.description, props.description_expanded));
    }

    let refreshing = props.refreshing.contains(&podcast.id);
    info = info.push(
        widget::Row::new()
            .push(
                widget::button::standard(fl!("podcast-refresh"))
                    .on_press_maybe((!refreshing).then_some(PodcastMessage::RefreshPodcast(podcast.id))),
            )
            .push(
                widget::button::standard(fl!("podcast-unsubscribe"))
                    .class(cosmic::theme::Button::Destructive)
                    .on_press(PodcastMessage::StartUnsubscribe(podcast.id)),
            )
            .spacing(8),
    );

    let confirm: cosmic::Element<'a, PodcastMessage> = if props.pending_unsubscribe == Some(podcast.id) {
        widget::container(
            widget::Row::new()
                .push(common::clipped_cell(
                    common::cell_text(fl!("podcast-confirm-unsubscribe", name = podcast.title.clone())).into(),
                ))
                .push(widget::button::standard(fl!("podcast-cancel")).on_press(PodcastMessage::CancelUnsubscribe))
                .push(
                    widget::button::standard(fl!("podcast-confirm-unsubscribe-yes"))
                        .class(cosmic::theme::Button::Destructive)
                        .on_press(PodcastMessage::ConfirmUnsubscribe(podcast.id)),
                )
                .spacing(8)
                .align_y(Alignment::Center),
        )
        .class(cosmic::theme::Container::Card)
        .padding(8)
        .width(Length::Fill)
        .into()
    } else {
        widget::Space::new().width(Length::Shrink).height(Length::Fixed(0.0)).into()
    };

    widget::Column::new()
        .push(
            widget::Row::new()
                .push(back_btn)
                .push(podcast_icon(&podcast.image_url, props.icons, 96))
                .push(info)
                .spacing(16)
                .align_y(Alignment::Start),
        )
        .push(confirm)
        .spacing(12)
        .into()
}

/// Clipped, expandable description block: a truncated caption with a
/// "Show more" toggle when collapsed, or the full wrapped body text with a
/// "Show less" toggle when expanded.
fn description_block<'a>(description: &'a str, expanded: bool) -> cosmic::Element<'a, PodcastMessage> {
    const COLLAPSED_MAX_CHARS: usize = 240;
    if expanded {
        widget::Column::new()
            .push(widget::text::body(description))
            .push(
                widget::button::text(fl!("podcast-description-less"))
                    .on_press(PodcastMessage::ToggleDescriptionExpanded),
            )
            .spacing(4)
            .into()
    } else {
        widget::Column::new()
            .push(common::cell_caption(common::truncate_str(description, COLLAPSED_MAX_CHARS)))
            .push(
                widget::button::text(fl!("podcast-description-more"))
                    .on_press(PodcastMessage::ToggleDescriptionExpanded),
            )
            .spacing(4)
            .into()
    }
}

fn filter_chip<'a>(label: String, selected: bool, target: EpisodeFilter) -> cosmic::Element<'a, PodcastMessage> {
    let btn = widget::button::text(label).on_press(PodcastMessage::EpisodeFilterSelected(target));
    if selected { btn.class(cosmic::theme::Button::Suggested) } else { btn.class(cosmic::theme::Button::Standard) }
        .into()
}

fn episode_row<'a>(
    episode: &'a Episode,
    downloading: &HashSet<i64>,
    is_current: bool,
    is_paused: bool,
) -> cosmic::Element<'a, PodcastMessage> {
    let date = format_pub_date(episode.pub_date);
    let mut info = widget::Column::new()
        .push(common::cell_text(episode.title.as_str()))
        .push(common::cell_caption(date))
        .spacing(2);

    if should_show_resume(episode) {
        let resumed_at = common::format_duration_coarse((episode.position_ms / 1000).max(0) as u64);
        info = info.push(common::cell_caption(fl!("resume-at", position = resumed_at)));
    }
    info = info.push(
        widget::progress_bar::determinate_linear(progress_fraction(episode)).width(Length::Fixed(120.0)),
    );

    let played_icon = if episode.played { "emblem-ok-symbolic" } else { "emblem-default-symbolic" };
    let played_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name(played_icon).size(16))
            .on_press(PodcastMessage::TogglePlayed(episode.id)),
        widget::text::caption(fl!("mark-played-tooltip")),
        widget::tooltip::Position::Top,
    );

    let download_control: cosmic::Element<'a, PodcastMessage> = if !episode.downloaded_path.is_empty() {
        widget::Row::new()
            .push(widget::icon::from_name("emblem-downloads-symbolic").size(16))
            .push(widget::tooltip(
                widget::button::icon(widget::icon::from_name("user-trash-symbolic").size(16))
                    .class(cosmic::theme::Button::Destructive)
                    .on_press(PodcastMessage::DeleteDownload(episode.id)),
                widget::text::caption(fl!("delete-download-tooltip")),
                widget::tooltip::Position::Top,
            ))
            .spacing(4)
            .align_y(Alignment::Center)
            .into()
    } else if downloading.contains(&episode.id) {
        widget::button::icon(widget::icon::from_name("content-loading-symbolic").size(16))
            .on_press_maybe(None::<PodcastMessage>)
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

    let play_icon = if is_current && !is_paused { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" };
    let play_tooltip = if is_current {
        fl!("pause")
    } else if should_show_resume(episode) {
        fl!("podcast-resume-tooltip")
    } else {
        fl!("podcast-play-tooltip")
    };
    let play_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name(play_icon).size(20))
            .on_press(PodcastMessage::PlayEpisode(episode.id)),
        widget::text::caption(play_tooltip),
        widget::tooltip::Position::Top,
    );

    widget::container(
        widget::Row::new()
            .push(play_btn)
            .push(common::clipped_cell(info.into()))
            .push(common::duration_cell(episode.duration_secs.max(0) as u64))
            .push(played_btn)
            .push(download_control)
            .spacing(8)
            .width(Length::Fill)
            .align_y(Alignment::Center)
            .padding(4),
    )
    .class(if is_current { cosmic::theme::Container::Card } else { cosmic::theme::Container::default() })
    .width(Length::Fill)
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
    episode.title.to_lowercase().contains(&needle) || episode.description.to_lowercase().contains(&needle)
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

    fn ep(played: bool, position_ms: i64, duration_secs: i64, downloaded_path: &str, title: &str, description: &str) -> Episode {
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
        assert!(!episode_matches(&played_match, EpisodeFilter::Unplayed, "rust"));
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
        assert_eq!(format_epoch_date(1704067200), Some("2024-01-01".to_string()));
    }
}
