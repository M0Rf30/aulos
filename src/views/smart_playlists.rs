// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Dynamic playlist list + rules editor view.

use crate::fl;
use crate::library::Track;
use crate::library::smart_playlist::{
    MatchMode, OrderField, Rule, RuleField, RuleOp, SmartPlaylist,
};
use crate::views::common;
use crate::views::list_row_button_class;
use cosmic::iced::core::Color;
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;

/// Messages from the smart playlists view (list, detail, and rules editor).
#[derive(Debug, Clone)]
pub enum SmartPlaylistMessage {
    // -- List view --
    /// Resolve and play a saved smart playlist's tracks, by list index.
    Play(usize),
    /// Open the rules editor for an existing smart playlist, by list index.
    Edit(usize),
    /// Delete a saved smart playlist, by list index.
    Delete(usize),
    /// Open the rules editor for a brand-new smart playlist.
    New,
    /// Select a saved smart playlist to view its resolved tracks.
    Select(usize),
    /// Return to the list from the detail or editor view.
    BackToList,

    // -- Detail view --
    /// Play a track from the currently viewed smart playlist's resolved list.
    PlayTrack(usize),
    /// Toggle favorite status for a track shown in the detail view.
    ToggleFavorite(String),
    /// Set the rating (1-5, 0 clears) for a track shown in the detail view.
    SetRating(String, u8),

    // -- Rules editor --
    EditorNameChanged(String),
    /// Match-mode dropdown changed (index into `[MatchMode::All, MatchMode::Any]`).
    EditorMatchModeChanged(usize),
    EditorAddRule,
    EditorRemoveRule(usize),
    /// A rule's field dropdown changed (rule index, index into `RuleField::ALL`).
    EditorRuleFieldChanged(usize, usize),
    /// A rule's operator dropdown changed (rule index, index into `RuleOp::ALL`).
    EditorRuleOpChanged(usize, usize),
    EditorRuleValueChanged(usize, String),
    /// The `Between` upper-bound value changed, by rule index.
    EditorRuleValue2Changed(usize, String),
    /// Order-by dropdown changed (index into `OrderField::ALL`).
    EditorOrderByChanged(usize),
    EditorOrderDescToggled(bool),
    /// Whether a row limit applies at all.
    EditorLimitToggled(bool),
    EditorLimitChanged(String),
    /// Persist the in-progress smart playlist (create if new, else update).
    EditorSave,
    /// Discard the in-progress edit and return to the list.
    EditorCancel,

    // -- Async results, dispatched from app.rs after off-thread DB work --
    /// Saved smart playlists (with resolved track counts) loaded from the DB.
    Loaded(Vec<SmartPlaylist>),
    /// Resolved tracks for the currently selected smart playlist (detail view).
    TracksLoaded(Vec<Track>),
    /// Resolved tracks ready for immediate playback (bypasses the detail view).
    PlayResolved(Vec<Track>),
}

/// In-progress edit state for the rules editor: the playlist being built,
/// plus live validation errors recomputed after every field change.
#[derive(Debug, Clone)]
pub struct EditorState {
    /// The smart playlist under edit. `id == 0` means "not yet saved" —
    /// `EditorSave` inserts a new row instead of updating one.
    pub playlist: SmartPlaylist,
    /// Whether a row limit applies. `playlist.limit` only holds a value
    /// while this is true; toggling off keeps `limit_input` so re-enabling
    /// restores the last-typed number.
    pub limit_enabled: bool,
    /// Raw text of the limit field, so a momentarily empty/invalid edit
    /// doesn't collapse `playlist.limit` before the user finishes typing.
    pub limit_input: String,
    /// Validation errors from the last edit; non-empty disables Save.
    pub errors: Vec<String>,
}

impl EditorState {
    /// Start editing a brand-new, empty smart playlist.
    pub fn new() -> Self {
        let playlist = SmartPlaylist {
            id: 0,
            name: String::new(),
            rules: Vec::new(),
            match_mode: MatchMode::All,
            order_by: OrderField::Title,
            order_desc: false,
            limit: None,
            track_count: 0,
        };
        let mut state = Self {
            playlist,
            limit_enabled: false,
            limit_input: String::new(),
            errors: Vec::new(),
        };
        state.revalidate();
        state
    }

    /// Start editing a previously saved smart playlist.
    pub fn from_existing(playlist: SmartPlaylist) -> Self {
        let limit_enabled = playlist.limit.is_some();
        let limit_input = playlist.limit.map(|l| l.to_string()).unwrap_or_default();
        let mut state = Self {
            playlist,
            limit_enabled,
            limit_input,
            errors: Vec::new(),
        };
        state.revalidate();
        state
    }

    fn revalidate(&mut self) {
        self.errors = self.playlist.validate().err().unwrap_or_default();
    }

    pub fn set_name(&mut self, name: String) {
        self.playlist.name = name;
        self.revalidate();
    }

    pub fn set_match_mode(&mut self, index: usize) {
        self.playlist.match_mode = if index == 1 {
            MatchMode::Any
        } else {
            MatchMode::All
        };
        self.revalidate();
    }

    pub fn add_rule(&mut self) {
        self.playlist.rules.push(Rule {
            field: RuleField::Title,
            op: RuleOp::Contains,
            value: String::new(),
            value2: String::new(),
        });
        self.revalidate();
    }

    pub fn remove_rule(&mut self, index: usize) {
        if index < self.playlist.rules.len() {
            self.playlist.rules.remove(index);
            self.revalidate();
        }
    }

    pub fn set_rule_field(&mut self, index: usize, field_index: usize) {
        if let (Some(rule), Some(field)) = (
            self.playlist.rules.get_mut(index),
            RuleField::ALL.get(field_index),
        ) {
            rule.field = *field;
        }
        self.revalidate();
    }

    pub fn set_rule_op(&mut self, index: usize, op_index: usize) {
        if let (Some(rule), Some(op)) = (
            self.playlist.rules.get_mut(index),
            RuleOp::ALL.get(op_index),
        ) {
            rule.op = *op;
        }
        self.revalidate();
    }

    pub fn set_rule_value(&mut self, index: usize, value: String) {
        if let Some(rule) = self.playlist.rules.get_mut(index) {
            rule.value = value;
        }
        self.revalidate();
    }

    pub fn set_rule_value2(&mut self, index: usize, value: String) {
        if let Some(rule) = self.playlist.rules.get_mut(index) {
            rule.value2 = value;
        }
        self.revalidate();
    }

    pub fn set_order_by(&mut self, index: usize) {
        if let Some(field) = OrderField::ALL.get(index) {
            self.playlist.order_by = *field;
        }
        self.revalidate();
    }

    pub fn set_order_desc(&mut self, desc: bool) {
        self.playlist.order_desc = desc;
        self.revalidate();
    }

    pub fn set_limit_enabled(&mut self, enabled: bool) {
        self.limit_enabled = enabled;
        self.sync_limit();
    }

    pub fn set_limit_input(&mut self, input: String) {
        self.limit_input = input;
        self.sync_limit();
    }

    fn sync_limit(&mut self) {
        self.playlist.limit = if self.limit_enabled {
            self.limit_input.trim().parse::<u32>().ok()
        } else {
            None
        };
        self.revalidate();
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

/// Localized "N tracks" label used in list rows and the detail header.
fn smart_playlist_track_count_label(count: usize) -> String {
    if count == 1 {
        fl!("smart-playlist-track-count-one", count = count.to_string())
    } else {
        fl!(
            "smart-playlist-track-count-other",
            count = count.to_string()
        )
    }
}

fn field_label(field: RuleField) -> String {
    match field {
        RuleField::Title => fl!("smart-playlist-field-title"),
        RuleField::Artist => fl!("smart-playlist-field-artist"),
        RuleField::AlbumArtist => fl!("smart-playlist-field-album-artist"),
        RuleField::Album => fl!("smart-playlist-field-album"),
        RuleField::Genre => fl!("smart-playlist-field-genre"),
        RuleField::Year => fl!("smart-playlist-field-year"),
        RuleField::Rating => fl!("smart-playlist-field-rating"),
        RuleField::Favorite => fl!("smart-playlist-field-favorite"),
        RuleField::DurationSecs => fl!("smart-playlist-field-duration"),
        RuleField::Bitrate => fl!("smart-playlist-field-bitrate"),
        RuleField::SampleRate => fl!("smart-playlist-field-sample-rate"),
    }
}

fn op_label(op: RuleOp) -> String {
    match op {
        RuleOp::Is => fl!("smart-playlist-op-is"),
        RuleOp::IsNot => fl!("smart-playlist-op-is-not"),
        RuleOp::Contains => fl!("smart-playlist-op-contains"),
        RuleOp::NotContains => fl!("smart-playlist-op-not-contains"),
        RuleOp::StartsWith => fl!("smart-playlist-op-starts-with"),
        RuleOp::EndsWith => fl!("smart-playlist-op-ends-with"),
        RuleOp::GreaterThan => fl!("smart-playlist-op-greater-than"),
        RuleOp::LessThan => fl!("smart-playlist-op-less-than"),
        RuleOp::Between => fl!("smart-playlist-op-between"),
    }
}

fn order_label(field: OrderField) -> String {
    match field {
        OrderField::Title => fl!("smart-playlist-order-title"),
        OrderField::Artist => fl!("smart-playlist-order-artist"),
        OrderField::Album => fl!("smart-playlist-order-album"),
        OrderField::Year => fl!("smart-playlist-order-year"),
        OrderField::Rating => fl!("smart-playlist-order-rating"),
        OrderField::DurationSecs => fl!("smart-playlist-order-duration"),
        OrderField::Random => fl!("smart-playlist-order-random"),
        OrderField::RecentlyAdded => fl!("smart-playlist-order-recently-added"),
    }
}

/// Render the saved smart playlists list.
pub fn smart_playlists_view(
    playlists: &[SmartPlaylist],
) -> cosmic::Element<'_, SmartPlaylistMessage> {
    let mut col = widget::Column::new().spacing(12).padding(16);

    let header = widget::Row::new()
        .push(widget::Space::new().width(Length::Fill))
        .push(
            widget::button::suggested(fl!("new-smart-playlist"))
                .on_press(SmartPlaylistMessage::New),
        )
        .align_y(Alignment::Center);

    col = col.push(header);
    col = col.push(widget::divider::horizontal::default());

    if playlists.is_empty() {
        col = col.push(common::empty_state(
            "starred-symbolic",
            fl!("no-smart-playlists"),
            fl!("smart-playlists-empty-hint"),
        ));
        return col.into();
    }

    let mut list = widget::Column::new().spacing(2);

    for (index, playlist) in playlists.iter().enumerate() {
        let info = widget::Column::new()
            .push(common::cell_text(playlist.name.as_str()))
            .push(common::cell_caption(smart_playlist_track_count_label(
                playlist.track_count,
            )))
            .spacing(2);

        let play_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("media-playback-start-symbolic").size(16))
                .on_press(SmartPlaylistMessage::Play(index)),
            widget::text::caption(fl!("play-smart-playlist-tooltip")),
            widget::tooltip::Position::Top,
        );
        let edit_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("document-edit-symbolic").size(16))
                .on_press(SmartPlaylistMessage::Edit(index)),
            widget::text::caption(fl!("edit-smart-playlist-tooltip")),
            widget::tooltip::Position::Top,
        );
        let delete_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("edit-delete-symbolic").size(16))
                .class(cosmic::theme::Button::Destructive)
                .on_press(SmartPlaylistMessage::Delete(index)),
            widget::text::caption(fl!("delete-smart-playlist-tooltip")),
            widget::tooltip::Position::Top,
        );

        let icon: cosmic::Element<'_, SmartPlaylistMessage> =
            widget::icon::from_name("starred-symbolic").size(40).into();

        let row = widget::button::custom(
            widget::Row::new()
                .push(icon)
                .push(common::clipped_cell(info.into()))
                .push(play_btn)
                .push(edit_btn)
                .push(delete_btn)
                .spacing(12)
                .align_y(Alignment::Center)
                .padding(8),
        )
        .on_press(SmartPlaylistMessage::Select(index))
        .width(Length::Fill)
        .class(list_row_button_class(false));

        list = list.push(row);
    }

    col = col
        .push(widget::scrollable(widget::container(list).width(Length::Fill)).height(Length::Fill));

    col.into()
}

/// Render the detail view for a selected smart playlist: its resolved tracks.
pub fn smart_playlist_detail_view<'a>(
    playlist: &'a SmartPlaylist,
    playlist_index: usize,
    tracks: &'a [Track],
    now_playing_id: Option<i64>,
) -> cosmic::Element<'a, SmartPlaylistMessage> {
    let detail_icon: cosmic::Element<'_, SmartPlaylistMessage> =
        widget::icon::from_name("starred-symbolic").size(80).into();

    let header = widget::Row::new()
        .push(widget::tooltip(
            widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                .on_press(SmartPlaylistMessage::BackToList),
            widget::text::caption(fl!("back-to-smart-playlists")),
            widget::tooltip::Position::Top,
        ))
        .push(detail_icon)
        .push(
            widget::Column::new()
                .push(common::clipped_cell(
                    widget::text::title1(playlist.name.as_str())
                        .wrapping(Wrapping::None)
                        .into(),
                ))
                .push(common::clipped_cell(
                    common::cell_caption(smart_playlist_track_count_label(tracks.len())).into(),
                ))
                .push(
                    widget::button::suggested(fl!("play-all"))
                        .on_press(SmartPlaylistMessage::Play(playlist_index)),
                )
                .spacing(8)
                .width(Length::Fill),
        )
        .spacing(16)
        .align_y(Alignment::Center);

    let mut track_list = widget::Column::new().spacing(2);

    for (track_idx, track) in tracks.iter().enumerate() {
        let is_playing = now_playing_id == Some(track.id);
        let track_id = track.id.to_string();
        let rating_track_id = track_id.clone();

        let title_col = widget::container(common::clipped_cell(
            common::cell_text(track.title.as_str()).into(),
        ))
        .width(Length::FillPortion(4));
        let artist_col = widget::container(common::clipped_cell(
            common::cell_text(track.artist.as_str()).into(),
        ))
        .width(Length::FillPortion(3));

        let row = widget::button::custom(
            widget::Row::new()
                .push(common::cell_text((track_idx + 1).to_string()).width(40))
                .push(title_col)
                .push(artist_col)
                .push(common::favorite_button(
                    track.is_favorite,
                    SmartPlaylistMessage::ToggleFavorite(track_id.clone()),
                ))
                .push(common::star_rating(track.rating, move |r| {
                    SmartPlaylistMessage::SetRating(rating_track_id.clone(), r)
                }))
                .push(common::duration_cell(track.duration.as_secs()))
                .spacing(8)
                .width(Length::Fill)
                .align_y(Alignment::Center)
                .padding(4),
        )
        .on_press(SmartPlaylistMessage::PlayTrack(track_idx))
        .width(Length::Fill)
        .class(list_row_button_class(is_playing));

        track_list = track_list.push(row);
    }

    if tracks.is_empty() {
        track_list = track_list.push(common::empty_state(
            "starred-symbolic",
            fl!("smart-playlist-empty"),
            fl!("smart-playlist-empty-hint"),
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

/// Render one rule row: field/operator dropdowns, value input(s), remove button.
fn rule_row(idx: usize, rule: &Rule) -> cosmic::Element<'_, SmartPlaylistMessage> {
    let field_index = RuleField::ALL
        .iter()
        .position(|f| *f == rule.field)
        .unwrap_or(0);
    let op_index = RuleOp::ALL.iter().position(|o| *o == rule.op).unwrap_or(0);

    let field_dropdown = widget::dropdown(
        RuleField::ALL
            .iter()
            .map(|f| field_label(*f))
            .collect::<Vec<_>>(),
        Some(field_index),
        move |i| SmartPlaylistMessage::EditorRuleFieldChanged(idx, i),
    );

    let op_dropdown = widget::dropdown(
        RuleOp::ALL.iter().map(|o| op_label(*o)).collect::<Vec<_>>(),
        Some(op_index),
        move |i| SmartPlaylistMessage::EditorRuleOpChanged(idx, i),
    );

    let value_input =
        widget::text_input(fl!("smart-playlist-value-placeholder"), rule.value.as_str())
            .on_input(move |v| SmartPlaylistMessage::EditorRuleValueChanged(idx, v))
            .width(Length::FillPortion(2));

    let mut row = widget::Row::new()
        .push(field_dropdown)
        .push(op_dropdown)
        .push(value_input)
        .spacing(8)
        .align_y(Alignment::Center);

    if rule.op == RuleOp::Between {
        row = row.push(
            widget::text_input(
                fl!("smart-playlist-value2-placeholder"),
                rule.value2.as_str(),
            )
            .on_input(move |v| SmartPlaylistMessage::EditorRuleValue2Changed(idx, v))
            .width(Length::FillPortion(2)),
        );
    }

    row = row.push(widget::tooltip(
        widget::button::icon(widget::icon::from_name("list-remove-symbolic").size(16))
            .on_press(SmartPlaylistMessage::EditorRemoveRule(idx)),
        widget::text::caption(fl!("remove-rule-tooltip")),
        widget::tooltip::Position::Top,
    ));

    row.into()
}

/// Render the rules editor for the in-progress smart playlist.
pub fn editor_view(state: &EditorState) -> cosmic::Element<'_, SmartPlaylistMessage> {
    let title = if state.playlist.id == 0 {
        fl!("new-smart-playlist")
    } else {
        fl!("edit-smart-playlist")
    };

    let name_row = widget::text_input(
        fl!("smart-playlist-name-placeholder"),
        state.playlist.name.as_str(),
    )
    .on_input(SmartPlaylistMessage::EditorNameChanged)
    .width(Length::Fill);

    let match_mode_index = match state.playlist.match_mode {
        MatchMode::All => 0,
        MatchMode::Any => 1,
    };
    let match_row = widget::Row::new()
        .push(common::cell_text(fl!("smart-playlist-match")))
        .push(widget::dropdown(
            vec![
                fl!("smart-playlist-match-all"),
                fl!("smart-playlist-match-any"),
            ],
            Some(match_mode_index),
            SmartPlaylistMessage::EditorMatchModeChanged,
        ))
        .spacing(8)
        .align_y(Alignment::Center);

    let mut rules_col = widget::Column::new().spacing(8);
    for (idx, rule) in state.playlist.rules.iter().enumerate() {
        rules_col = rules_col.push(rule_row(idx, rule));
    }

    let add_rule_btn =
        widget::button::standard(fl!("add-rule")).on_press(SmartPlaylistMessage::EditorAddRule);

    let order_field_index = OrderField::ALL
        .iter()
        .position(|f| *f == state.playlist.order_by)
        .unwrap_or(0);
    let order_row = widget::Row::new()
        .push(common::cell_text(fl!("smart-playlist-order-by")))
        .push(widget::dropdown(
            OrderField::ALL
                .iter()
                .map(|f| order_label(*f))
                .collect::<Vec<_>>(),
            Some(order_field_index),
            SmartPlaylistMessage::EditorOrderByChanged,
        ))
        .push(
            widget::checkbox(state.playlist.order_desc)
                .label(fl!("smart-playlist-order-desc"))
                .on_toggle(SmartPlaylistMessage::EditorOrderDescToggled),
        )
        .spacing(8)
        .align_y(Alignment::Center);

    let limit_row = widget::Row::new()
        .push(
            widget::checkbox(state.limit_enabled)
                .label(fl!("smart-playlist-limit"))
                .on_toggle(SmartPlaylistMessage::EditorLimitToggled),
        )
        .push(
            widget::text_input(
                fl!("smart-playlist-limit-placeholder"),
                state.limit_input.as_str(),
            )
            .on_input(SmartPlaylistMessage::EditorLimitChanged)
            .width(Length::Fixed(100.0)),
        )
        .spacing(8)
        .align_y(Alignment::Center);

    let mut body = widget::Column::new()
        .push(widget::text::title2(title))
        .push(name_row)
        .push(match_row)
        .push(widget::text::body(fl!("smart-playlist-rules-heading")))
        .push(rules_col)
        .push(add_rule_btn)
        .push(order_row)
        .push(limit_row)
        .spacing(12);

    if !state.errors.is_empty() {
        let mut errors_col = widget::Column::new().spacing(2);
        for error in &state.errors {
            errors_col = errors_col.push(
                widget::text::caption(error.as_str())
                    .class(cosmic::theme::Text::Color(Color::from_rgb(0.9, 0.2, 0.2))),
            );
        }
        body = body.push(errors_col);
    }

    let can_save = state.errors.is_empty();
    let action_row = widget::Row::new()
        .push(
            widget::button::suggested(fl!("save"))
                .on_press_maybe(can_save.then_some(SmartPlaylistMessage::EditorSave)),
        )
        .push(
            widget::button::standard(fl!("smart-playlist-cancel-edit"))
                .on_press(SmartPlaylistMessage::EditorCancel),
        )
        .spacing(8);

    body = body.push(action_row);

    widget::scrollable(body.padding(16))
        .height(Length::Fill)
        .into()
}
