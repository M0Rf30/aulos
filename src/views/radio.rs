// SPDX-License-Identifier: GPL-3.0

//! Radio view — saved internet radio stations ("My stations") and a
//! radio-browser.info directory search ("Discover"), behind a stable
//! two-tab layout.
//!
//! Every reactive slot (add-by-URL card, now-playing strip, Discover
//! results) stays present in the tree at all times; only its *content*
//! changes with state. That's deliberate: iced keeps widget state (scroll
//! offset, text-input focus/cursor) by tree position, so conditionally
//! inserting/removing a sibling ahead of a `scrollable`/`text_input`
//! resets it. Switching tabs is the one place the tree shape does change,
//! since that's an explicit user navigation, not incidental reactive
//! state (like the old Discover button popping in and out ever was).

use crate::fl;
use crate::library::Track;
use crate::online::radio::{SortOrder, StationSearchResult};
use crate::online::store::RadioStation;
use crate::player::PlaybackState;
use crate::views::common;
use crate::views::list_row_button_class;
use cosmic::iced::core::Color;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use std::collections::HashMap;

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
pub const QUICK_TAGS: [&str; 9] =
    ["Pop", "Rock", "Jazz", "Classical", "Electronic", "News", "Talk", "Ambient", "Lo-fi"];

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

fn station_icon<'a, M: 'a + 'static>(
    favicon_url: &str,
    icons: &HashMap<String, widget::icon::Handle>,
    size: u16,
) -> cosmic::Element<'a, M> {
    match icons.get(favicon_url) {
        Some(handle) if !favicon_url.is_empty() => widget::icon::icon(handle.clone()).size(size).into(),
        _ => widget::icon::from_name("network-wireless-symbolic").size(size).into(),
    }
}

/// Render the full radio page: header, add-by-URL card, now-playing
/// strip and the active tab's content.
pub fn radio_view<'a>(props: RadioViewProps<'a>) -> cosmic::Element<'a, RadioMessage> {
    let is_radio_track = props.current_track.is_some_and(|t| &*t.provider_id == "radio");
    let is_live = is_radio_track
        && matches!(props.playback_state, Some(PlaybackState::Playing) | Some(PlaybackState::Paused));
    let current_stream_url = if is_live { Some(props.now_playing_key) } else { None };
    let is_paused = matches!(props.playback_state, Some(PlaybackState::Paused));

    let mut col = widget::Column::new().spacing(12).padding(16);
    col = col.push(header_row(props.tab, props.add_open));
    col = col.push(add_form_card(props.add_open, props.add_name, props.add_url, props.add_error));
    col = col.push(now_playing_strip(
        is_live,
        is_paused,
        props.current_track,
        props.now_playing_favicon,
        props.icons,
    ));
    col = col.push(widget::divider::horizontal::default());

    let content = match props.tab {
        RadioTab::MyStations => my_stations_tab(
            props.stations,
            props.filter,
            props.renaming_id,
            props.rename_input,
            props.icons,
            current_stream_url,
            is_live,
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
        ),
    };
    col = col.push(content);
    col.into()
}

/// Title + tab switch + add-station toggle. Always the same widgets in
/// the same order -- only styles/state change (selected tab, toggle
/// pressed state).
fn header_row<'a>(tab: RadioTab, add_open: bool) -> cosmic::Element<'a, RadioMessage> {
    let add_btn = widget::button::text(fl!("radio-add-station"))
        .on_press(RadioMessage::ToggleAddForm)
        .class(if add_open { cosmic::theme::Button::Suggested } else { cosmic::theme::Button::Standard });
    widget::Row::new()
        .push(widget::text::title3(fl!("radio")))
        .push(widget::Space::new().width(Length::Fill))
        .push(tab_button(fl!("radio-tab-my-stations"), tab == RadioTab::MyStations, RadioTab::MyStations))
        .push(tab_button(fl!("radio-tab-discover"), tab == RadioTab::Discover, RadioTab::Discover))
        .push(add_btn)
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
}

fn tab_button<'a>(label: String, selected: bool, target: RadioTab) -> cosmic::Element<'a, RadioMessage> {
    let btn = widget::button::text(label).on_press(RadioMessage::TabSelected(target));
    if selected { btn.class(cosmic::theme::Button::Suggested) } else { btn.class(cosmic::theme::Button::Standard) }
        .into()
}

/// The add-by-URL card's slot. Always present; empty (zero-height) when
/// closed, so opening/closing it never shifts anything below it in a way
/// that would surprise -- the toggle button that opens it stays put in
/// the header, this is purely additive underneath.
fn add_form_card<'a>(
    open: bool,
    name: &'a str,
    url: &'a str,
    error: Option<&'a str>,
) -> cosmic::Element<'a, RadioMessage> {
    if !open {
        return widget::Space::new().width(Length::Shrink).height(Length::Fixed(0.0)).into();
    }
    let fields = widget::Row::new()
        .push(
            widget::text_input(fl!("station-name-placeholder"), name)
                .on_input(RadioMessage::AddNameChanged)
                .width(Length::FillPortion(1)),
        )
        .push(
            widget::text_input(fl!("station-url-placeholder"), url)
                .on_input(RadioMessage::AddUrlChanged)
                .on_submit(|_| RadioMessage::SubmitAdd)
                .width(Length::FillPortion(2)),
        )
        .push(widget::button::suggested(fl!("save")).on_press(RadioMessage::SubmitAdd))
        .push(widget::button::standard(fl!("radio-cancel")).on_press(RadioMessage::ToggleAddForm))
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

/// The now-playing strip's permanent slot: live station info + Stop, or a
/// neutral idle hint. `is_live` already folds in playback state, so a
/// stopped station never renders as if it were still playing.
fn now_playing_strip<'a>(
    is_live: bool,
    is_paused: bool,
    current_track: Option<&'a Track>,
    favicon: &'a str,
    icons: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, RadioMessage> {
    let body: cosmic::Element<'a, RadioMessage> = if is_live {
        let title = current_track.map(|t| t.title.as_str()).unwrap_or_default();
        let status = if is_paused { fl!("pause") } else { fl!("now-playing-live") };
        let info = widget::Column::new()
            .push(common::cell_text(title))
            .push(common::cell_caption(status))
            .spacing(2);
        widget::Row::new()
            .push(station_icon(favicon, icons, 40))
            .push(common::clipped_cell(info.into()))
            .push(
                widget::button::standard(fl!("stop"))
                    .class(cosmic::theme::Button::Destructive)
                    .on_press(RadioMessage::Stop),
            )
            .spacing(12)
            .align_y(Alignment::Center)
            .into()
    } else {
        widget::Row::new()
            .push(widget::icon::from_name("network-wireless-symbolic").size(24))
            .push(common::cell_caption(fl!("radio-idle-hint")))
            .spacing(12)
            .align_y(Alignment::Center)
            .into()
    };
    widget::container(body)
        .class(cosmic::theme::Container::Card)
        .padding(12)
        .width(Length::Fill)
        .into()
}

/// "My stations" tab: filter box, then either the empty state or the
/// saved-station list. `is_current` also requires `is_live` so a stopped
/// station's row loses its highlight.
#[allow(clippy::too_many_arguments)]
fn my_stations_tab<'a>(
    stations: &'a [RadioStation],
    filter: &'a str,
    renaming_id: Option<i64>,
    rename_input: &'a str,
    icons: &'a HashMap<String, widget::icon::Handle>,
    current_stream_url: Option<&'a str>,
    is_live: bool,
) -> cosmic::Element<'a, RadioMessage> {
    let mut col = widget::Column::new().spacing(8);
    col = col.push(
        widget::text_input(fl!("radio-filter-placeholder"), filter)
            .on_input(RadioMessage::FilterChanged)
            .width(Length::Fill),
    );

    let filter_lower = filter.trim().to_lowercase();
    let filtered: Vec<&RadioStation> = stations
        .iter()
        .filter(|s| {
            filter_lower.is_empty()
                || s.name.to_lowercase().contains(&filter_lower)
                || s.tags.to_lowercase().contains(&filter_lower)
        })
        .collect();

    if stations.is_empty() {
        let mut empty_col = widget::Column::new()
            .spacing(12)
            .align_x(Alignment::Center)
            .push(common::empty_state(
                "network-wireless-symbolic",
                fl!("no-stations"),
                fl!("stations-empty-hint"),
            ));
        empty_col = empty_col.push(
            widget::Row::new()
                .spacing(8)
                .push(
                    widget::button::standard(fl!("radio-go-discover"))
                        .on_press(RadioMessage::TabSelected(RadioTab::Discover)),
                )
                .push(widget::button::standard(fl!("radio-add-by-url")).on_press(RadioMessage::ToggleAddForm)),
        );
        col = col.push(empty_col);
        return col.into();
    }

    if filtered.is_empty() {
        col = col.push(common::empty_state(
            "edit-find-symbolic",
            fl!("radio-no-filter-matches"),
            fl!("radio-no-filter-matches-hint"),
        ));
        return col.into();
    }

    let mut list = widget::Column::new().spacing(2);
    for station in filtered {
        let is_current = is_live && current_stream_url == Some(station.stream_url.as_str());
        let row = if renaming_id == Some(station.id) {
            rename_row(station.id, rename_input)
        } else {
            saved_station_row(station, icons, is_current)
        };
        list = list.push(row);
    }

    col = col.push(widget::scrollable(widget::container(list).width(Length::Fill)).height(Length::Fill));
    col.into()
}

fn saved_station_row<'a>(
    station: &'a RadioStation,
    icons: &'a HashMap<String, widget::icon::Handle>,
    is_current: bool,
) -> cosmic::Element<'a, RadioMessage> {
    let info = widget::Column::new()
        .push(common::cell_text(station.name.as_str()))
        .push(common::cell_caption(station.tags.as_str()))
        .spacing(2);

    let station_id = station.id;
    let rename_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name("document-edit-symbolic").size(16))
            .on_press(RadioMessage::StartRename(station_id, station.name.clone())),
        widget::text::caption(fl!("radio-rename-tooltip")),
        widget::tooltip::Position::Top,
    );
    let remove_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name("edit-delete-symbolic").size(16))
            .class(cosmic::theme::Button::Destructive)
            .on_press(RadioMessage::RemoveSaved(station_id)),
        widget::text::caption(fl!("remove")),
        widget::tooltip::Position::Top,
    );

    widget::button::custom(
        widget::Row::new()
            .push(station_icon(&station.favicon_url, icons, 32))
            .push(common::clipped_cell(info.into()))
            .push(rename_btn)
            .push(remove_btn)
            .spacing(12)
            .align_y(Alignment::Center)
            .padding(8),
    )
    .on_press(RadioMessage::PlaySaved(station_id))
    .width(Length::Fill)
    .class(list_row_button_class(is_current))
    .into()
}

/// Replaces a saved-station row with an inline rename editor, at the same
/// list position -- only the row being renamed changes shape.
fn rename_row<'a>(id: i64, rename_input: &'a str) -> cosmic::Element<'a, RadioMessage> {
    widget::Row::new()
        .push(
            widget::text_input("", rename_input)
                .on_input(RadioMessage::RenameInputChanged)
                .on_submit(move |_| RadioMessage::CommitRename(id))
                .width(Length::Fill),
        )
        .push(widget::button::suggested(fl!("save")).on_press(RadioMessage::CommitRename(id)))
        .push(widget::button::standard(fl!("radio-cancel")).on_press(RadioMessage::CancelRename))
        .spacing(8)
        .align_y(Alignment::Center)
        .padding(8)
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
) -> cosmic::Element<'a, RadioMessage> {
    let mut col = widget::Column::new().spacing(8);

    let search_row = widget::Row::new()
        .push(
            widget::text_input(fl!("radio-search-placeholder"), query)
                .on_input(RadioMessage::SearchChanged)
                .on_submit(|_| RadioMessage::SearchSubmit)
                .width(Length::Fill),
        )
        .push(widget::button::standard(fl!("search")).on_press(RadioMessage::SearchSubmit))
        .push(widget::dropdown(sort_labels(), Some(sort_index(sort)), RadioMessage::SortSelected))
        .spacing(8)
        .align_y(Alignment::Center);
    col = col.push(search_row);

    let mut chip_elements: Vec<cosmic::Element<'a, RadioMessage>> = Vec::new();
    for name in QUICK_TAGS {
        let selected = tag == Some(name);
        let target = if selected { None } else { Some(name) };
        let btn = widget::button::text(name.to_string()).on_press(RadioMessage::TagSelected(target));
        chip_elements.push(
            if selected { btn.class(cosmic::theme::Button::Suggested) } else { btn.class(cosmic::theme::Button::Standard) }
                .into(),
        );
    }
    if !locale_country.is_empty() {
        let selected = !country.is_empty();
        let label = fl!("radio-only-country", code = locale_country.to_string());
        let btn = widget::button::text(label).on_press(RadioMessage::CountryToggled);
        chip_elements.push(
            if selected { btn.class(cosmic::theme::Button::Suggested) } else { btn.class(cosmic::theme::Button::Standard) }
                .into(),
        );
    }
    col = col.push(widget::flex_row(chip_elements).spacing(6));

    let presets_row = widget::Row::new()
        .push(widget::button::standard(fl!("radio-preset-popular")).on_press(RadioMessage::Preset(DiscoverPreset::Popular)))
        .push(widget::button::standard(fl!("radio-preset-top-voted")).on_press(RadioMessage::Preset(DiscoverPreset::TopVoted)))
        .spacing(8);
    col = col.push(presets_row);

    let body: cosmic::Element<'a, RadioMessage> = if loading {
        common::empty_state("view-refresh-symbolic", fl!("searching"), fl!("radio-searching-hint"))
    } else if let Some(err) = error {
        widget::Column::new()
            .spacing(8)
            .align_x(Alignment::Center)
            .push(widget::icon::from_name("dialog-error-symbolic").size(48))
            .push(common::cell_text(err))
            .push(widget::button::standard(fl!("radio-retry")).on_press(RadioMessage::RetrySearch))
            .into()
    } else if results.is_empty() {
        common::empty_state("edit-find-symbolic", fl!("radio-no-results"), fl!("radio-no-results-hint"))
    } else {
        let mut list = widget::Column::new().spacing(2);
        for result in results {
            let already_saved = saved.iter().any(|s| s.stream_url == result.url);
            let is_current = is_live && current_stream_url == Some(result.url.as_str());
            list = list.push(discover_result_row(result, icons, already_saved, is_current));
        }
        widget::container(list).width(Length::Fill).into()
    };
    col = col.push(widget::scrollable(body).height(Length::Fill).width(Length::Fill));
    col.into()
}

fn discover_result_row<'a>(
    result: &'a StationSearchResult,
    icons: &'a HashMap<String, widget::icon::Handle>,
    already_saved: bool,
    is_current: bool,
) -> cosmic::Element<'a, RadioMessage> {
    let caption = format!(
        "{}  ·  {}  ·  {} kbps  ·  {}",
        if result.country.is_empty() { fl!("radio-unknown-country") } else { result.country.clone() },
        result.codec,
        result.bitrate,
        result.tags
    );
    let info = widget::Column::new()
        .push(common::cell_text(result.name.as_str()))
        .push(common::cell_caption(caption))
        .spacing(2);

    let uuid = result.stationuuid.clone();
    let play_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name("media-playback-start-symbolic").size(16))
            .on_press_maybe(if is_current { None } else { Some(RadioMessage::PlayResult(uuid.clone())) }),
        widget::text::caption(if is_current { fl!("radio-now-playing-badge") } else { fl!("play-station-tooltip") }),
        widget::tooltip::Position::Top,
    );
    let save_btn = if already_saved {
        widget::button::standard(fl!("radio-saved")).class(cosmic::theme::Button::Text)
    } else {
        widget::button::standard(fl!("save")).on_press(RadioMessage::SaveResult(uuid))
    };

    widget::container(
        widget::Row::new()
            .push(station_icon(&result.favicon, icons, 32))
            .push(common::clipped_cell(info.into()))
            .push(play_btn)
            .push(save_btn)
            .spacing(12)
            .align_y(Alignment::Center)
            .padding(8),
    )
    .class(if is_current { cosmic::theme::Container::Card } else { cosmic::theme::Container::default() })
    .width(Length::Fill)
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
