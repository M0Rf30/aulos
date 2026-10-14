// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Dynamic playlist list + rules editor view.

use crate::views::common;
use crate::views::list_row_button_class;
use crate::views::track_row;
use aulos_core::fl;
use aulos_core::library::Track;
use aulos_core::library::smart_playlist::{
    EditorState, MatchMode, OrderField, Rule, RuleField, RuleOp, SmartPlaylist,
};
use cosmic::iced::alignment::Horizontal;
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
    /// Export a saved smart playlist's current tracks as M3U, by list index.
    Export(usize),
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

    /// Jump to another view (artist page).
    Navigate(crate::views::Route),
    /// Open the Properties drawer for this track.
    ShowProperties(Track),
    /// Open the tag editor on this (local) track.
    Tag(Track, aulos_core::tagger::TagAction),
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

/// Render the saved smart playlists list.
pub fn smart_playlists_view(
    playlists: &[SmartPlaylist],
) -> cosmic::Element<'_, SmartPlaylistMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut col = widget::Column::new().spacing(spacing.space_s).padding([
        spacing.space_m,
        spacing.space_m + 16,
        0,
        spacing.space_m,
    ]);

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
            .push(common::cell_text(playlist.name.as_str()).font(cosmic::font::semibold()))
            .push(secondary_caption(playlist.rule_summary()))
            .spacing(2);

        let play_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("media-playback-start-symbolic").size(16))
                .on_press(SmartPlaylistMessage::Play(index)),
            widget::text::caption(fl!("play-smart-playlist-tooltip")),
            widget::tooltip::Position::Top,
        );
        let export_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("document-save-symbolic").size(16))
                .on_press(SmartPlaylistMessage::Export(index)),
            widget::text::caption(fl!("export-m3u-tooltip")),
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
                .on_press(SmartPlaylistMessage::Delete(index)),
            widget::text::caption(fl!("delete-smart-playlist-tooltip")),
            widget::tooltip::Position::Top,
        );

        let art: cosmic::Element<'_, SmartPlaylistMessage> =
            common::list_art_icon(None, 52, "starred-symbolic");

        let row = widget::button::custom(
            widget::Row::new()
                .push(art)
                .push(
                    widget::container(common::clipped_cell(info.into()))
                        .width(Length::FillPortion(5)),
                )
                .push(
                    widget::container(secondary_caption(smart_playlist_track_count_label(
                        playlist.track_count,
                    )))
                    .width(LIST_TRACKS_WIDTH)
                    .align_x(Horizontal::Right),
                )
                .push(play_btn)
                .push(export_btn)
                .push(edit_btn)
                .push(delete_btn)
                .spacing(16)
                .height(Length::Fill)
                .align_y(Alignment::Center)
                .padding([0, 12]),
        )
        .on_press(SmartPlaylistMessage::Select(index))
        .width(Length::Fill)
        .height(Length::Fixed(LIST_ROW_HEIGHT))
        .padding(0)
        .class(list_row_button_class(false));

        list = list.push(row);
    }

    let spacing = cosmic::theme::active().cosmic().spacing;
    col = col.push(
        widget::scrollable(
            widget::container(list)
                .padding([0, spacing.space_m + 16, spacing.space_m, 0])
                .width(Length::Fill),
        )
        .height(Length::Fill),
    );

    col.into()
}

/// List row geometry shared with the Albums list.
const LIST_ROW_HEIGHT: f32 = 68.0;
const LIST_TRACKS_WIDTH: f32 = 90.0;

/// Caption text dimmed to the theme's secondary colour.
fn secondary_caption<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Custom(|theme| {
        cosmic::iced::widget::text::Style {
            color: Some(theme.cosmic().palette.neutral_7.into()),
            ..Default::default()
        }
    }))
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
                .push(
                    widget::button::standard(fl!("export-m3u"))
                        .on_press(SmartPlaylistMessage::Export(playlist_index)),
                )
                .spacing(8)
                .width(Length::Fill),
        )
        .spacing(16)
        .align_y(Alignment::Center);

    let track_list: cosmic::Element<'a, SmartPlaylistMessage> = if tracks.is_empty() {
        common::empty_state(
            "starred-symbolic",
            fl!("smart-playlist-empty"),
            fl!("smart-playlist-empty-hint"),
        )
    } else {
        let columns = track_row::Columns {
            artist: true,
            album: true,
            favorite: true,
            rating: true,
            quality: true,
            ..Default::default()
        };
        track_row::width_aware(tracks.len(), true, move |width| {
            let columns = columns.responsive(width);
            track_row::rows_column(
                Some(track_row::Header::new(columns)),
                tracks.iter().enumerate().map(|(track_idx, track)| {
                    let track_id = track.id.to_string();
                    let rating_track_id = track_id.clone();
                    track_row::TrackRow::new(
                        track,
                        (track_idx + 1).to_string(),
                        now_playing_id == Some(track.id),
                        columns,
                        SmartPlaylistMessage::PlayTrack(track_idx),
                    )
                    .with_navigate(SmartPlaylistMessage::Navigate)
                    .with_favorite(SmartPlaylistMessage::ToggleFavorite(track_id))
                    .with_rating(move |r| {
                        SmartPlaylistMessage::SetRating(rating_track_id.clone(), r)
                    })
                    .with_properties(SmartPlaylistMessage::ShowProperties(track.clone()))
                    .with_tagging(SmartPlaylistMessage::Tag)
                    .with_artist_subtitle(!columns.artist)
                    .view()
                }),
            )
        })
    };

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
        RuleField::ALL.iter().map(|f| f.label()).collect::<Vec<_>>(),
        Some(field_index),
        move |i| SmartPlaylistMessage::EditorRuleFieldChanged(idx, i),
    );

    let op_dropdown = widget::dropdown(
        RuleOp::ALL.iter().map(|o| o.label()).collect::<Vec<_>>(),
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
                .map(|f| f.label())
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
