// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Playlists view - list of playlists with create/delete/rename actions,
//! and a detail view showing tracks in a selected playlist.

use crate::fl;
use crate::library::Playlist;
use crate::views::common;
use crate::views::list_row_button_class;
use crate::views::track_row;
use cosmic::iced::alignment::Horizontal;
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;

/// Messages from the playlists view.
#[derive(Debug, Clone)]
pub enum PlaylistMessage {
    /// User selected a playlist to view its tracks.
    SelectPlaylist(usize),
    /// Go back to the playlist list from detail view.
    BackToList,
    /// User wants to create a new playlist (carries the name).
    CreatePlaylist(String),
    /// User wants to delete a playlist by index.
    DeletePlaylist(usize),
    /// User wants to rename a playlist (index, new name).
    RenamePlaylist(usize, String),
    /// Play all tracks in a playlist.
    PlayPlaylist(usize),
    /// Play a specific track within a playlist detail view.
    PlayTrack(usize, usize),
    /// Remove a track from a playlist (playlist index, track index).
    RemoveTrack(usize, usize),
    /// The new-playlist name input changed.
    NewPlaylistNameChanged(String),
    /// The rename input changed (playlist index, new text).
    RenameInputChanged(usize, String),
    /// Insert tracks right after the currently playing one: track `.1` of
    /// playlist `.0`, or every track when `None`. Indices only; `update`
    /// resolves them, so `view()` never clones track lists.
    PlayNext(usize, Option<usize>),
    /// Append tracks (see `PlayNext`) to the end of the queue.
    AddToQueue(usize, Option<usize>),
    /// Jump to another view (artist page).
    Navigate(crate::views::Route),
    /// Export the playlist at this index as an M3U file.
    Export(usize),
    /// Pick M3U/M3U8/PLS files to import as new playlists.
    ImportFiles,
}

/// Render the playlist list view.
pub fn playlist_list_view<'a>(
    playlists: &'a [Playlist],
    new_playlist_name: &'a str,
) -> cosmic::Element<'a, PlaylistMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut col = widget::Column::new().spacing(spacing.space_s).padding([
        spacing.space_m,
        spacing.space_m + 16,
        0,
        spacing.space_m,
    ]);

    // Create playlist row
    let create_row = widget::Row::new()
        .push(
            widget::container(
                widget::text_input(fl!("new-playlist-placeholder"), new_playlist_name)
                    .on_input(PlaylistMessage::NewPlaylistNameChanged)
                    .on_submit_maybe(if new_playlist_name.is_empty() {
                        None
                    } else {
                        Some(PlaylistMessage::CreatePlaylist)
                    })
                    .width(Length::Fill),
            )
            .width(Length::Fill)
            .max_width(420.0),
        )
        .push(
            widget::button::suggested(fl!("create-playlist")).on_press_maybe(
                if new_playlist_name.is_empty() {
                    None
                } else {
                    Some(PlaylistMessage::CreatePlaylist(
                        new_playlist_name.to_string(),
                    ))
                },
            ),
        )
        .push(
            widget::button::standard(fl!("import-m3u"))
                .leading_icon(widget::icon::from_name("document-open-symbolic").size(16))
                .on_press(PlaylistMessage::ImportFiles),
        )
        .spacing(8)
        .align_y(Alignment::Center);

    col = col.push(create_row);
    col = col.push(widget::divider::horizontal::default());

    if playlists.is_empty() {
        col = col.push(common::empty_state(
            "playlist-symbolic",
            fl!("no-playlists"),
            fl!("playlists-empty-hint"),
        ));

        return col.into();
    }

    let mut list = widget::Column::new().spacing(2);

    for (index, playlist) in playlists.iter().enumerate() {
        let track_count = playlist.track_count;
        let info = widget::Column::new()
            .push(common::cell_text(playlist.name.as_str()).font(cosmic::font::semibold()))
            .push(secondary_caption(format!(
                "{}  -  {}",
                playlist_track_count_label(track_count),
                common::format_duration_coarse(playlist.total_duration.as_secs())
            )))
            .spacing(2);

        let art: cosmic::Element<'_, PlaylistMessage> =
            common::list_art_icon(None, 52, "playlist-symbolic");

        let play_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("media-playback-start-symbolic").size(16))
                .on_press_maybe((track_count > 0).then_some(PlaylistMessage::PlayPlaylist(index))),
            widget::text::caption(fl!("play-all")),
            widget::tooltip::Position::Top,
        );
        let export_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("document-save-symbolic").size(16))
                .on_press_maybe((track_count > 0).then_some(PlaylistMessage::Export(index))),
            widget::text::caption(fl!("export-m3u-tooltip")),
            widget::tooltip::Position::Top,
        );
        let delete_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("edit-delete-symbolic").size(16))
                .on_press(PlaylistMessage::DeletePlaylist(index)),
            widget::text::caption(fl!("delete-playlist-tooltip")),
            widget::tooltip::Position::Top,
        );

        let row = widget::button::custom(
            widget::Row::new()
                .push(art)
                .push(
                    widget::container(common::clipped_cell(info.into()))
                        .width(Length::FillPortion(5)),
                )
                .push(
                    widget::container(secondary_caption(playlist_track_count_label(track_count)))
                        .width(LIST_TRACKS_WIDTH)
                        .align_x(Horizontal::Right),
                )
                .push(
                    widget::container(secondary_caption(common::format_duration_coarse(
                        playlist.total_duration.as_secs(),
                    )))
                    .width(LIST_DURATION_WIDTH)
                    .align_x(Horizontal::Right),
                )
                .push(play_btn)
                .push(export_btn)
                .push(delete_btn)
                .spacing(16)
                .height(Length::Fill)
                .align_y(Alignment::Center)
                .padding([0, 12]),
        )
        .on_press(PlaylistMessage::SelectPlaylist(index))
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

/// List row geometry shared with the Albums list: fixed height and
/// right-hand metadata column widths so columns line up.
const LIST_ROW_HEIGHT: f32 = 68.0;
const LIST_TRACKS_WIDTH: f32 = 90.0;
const LIST_DURATION_WIDTH: f32 = 64.0;

/// Caption text dimmed to the theme's secondary colour.
fn secondary_caption<'a>(content: impl Into<std::borrow::Cow<'a, str>> + 'a) -> common::Text<'a> {
    common::cell_caption(content).class(cosmic::theme::Text::Custom(|theme| {
        cosmic::iced::widget::text::Style {
            color: Some(theme.cosmic().palette.neutral_7.into()),
            ..Default::default()
        }
    }))
}

pub fn playlist_detail_view<'a>(
    playlist: &'a Playlist,
    playlist_index: usize,
    edit_name: &'a str,
) -> cosmic::Element<'a, PlaylistMessage> {
    let detail_icon: cosmic::Element<'_, PlaylistMessage> =
        widget::icon::from_name("playlist-symbolic").size(80).into();

    let can_rename = !edit_name.trim().is_empty() && edit_name != playlist.name.as_str();
    let rename_row = widget::Row::new()
        .push(
            widget::container(
                widget::text_input(fl!("playlist-name-placeholder"), edit_name)
                    .on_input(move |text| PlaylistMessage::RenameInputChanged(playlist_index, text))
                    .on_submit_maybe(if can_rename {
                        Some(move |text: String| {
                            PlaylistMessage::RenamePlaylist(playlist_index, text)
                        })
                    } else {
                        None
                    })
                    .width(Length::Fill),
            )
            .width(Length::Fill)
            .max_width(420.0),
        )
        .push(
            widget::button::standard(fl!("rename-playlist")).on_press_maybe(if can_rename {
                Some(PlaylistMessage::RenamePlaylist(
                    playlist_index,
                    edit_name.to_string(),
                ))
            } else {
                None
            }),
        )
        .spacing(8)
        .align_y(Alignment::Center);

    let track_count = playlist.track_count;
    let header = widget::Row::new()
        .push(widget::tooltip(
            widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                .on_press(PlaylistMessage::BackToList),
            widget::text::caption(fl!("back-to-playlists")),
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
                    common::cell_caption(format!(
                        "{}  -  {}",
                        playlist_track_count_label(track_count),
                        common::format_duration_coarse(playlist.total_duration.as_secs())
                    ))
                    .into(),
                ))
                .push(rename_row)
                .push(
                    widget::button::suggested(fl!("play-all"))
                        .on_press(PlaylistMessage::PlayPlaylist(playlist_index)),
                )
                .push(
                    widget::Row::new()
                        .push(
                            widget::button::standard(fl!("queue-play-next"))
                                .on_press(PlaylistMessage::PlayNext(playlist_index, None)),
                        )
                        .push(
                            widget::button::standard(fl!("queue-add"))
                                .on_press(PlaylistMessage::AddToQueue(playlist_index, None)),
                        )
                        .spacing(8),
                )
                .push(
                    widget::button::standard(fl!("export-m3u"))
                        .leading_icon(widget::icon::from_name("document-save-symbolic").size(16))
                        .on_press_maybe(
                            (track_count > 0).then_some(PlaylistMessage::Export(playlist_index)),
                        ),
                )
                .spacing(8)
                .width(Length::Fill),
        )
        .spacing(16)
        .align_y(Alignment::Center);

    let track_list: cosmic::Element<'a, PlaylistMessage> = if playlist.tracks.is_empty() {
        common::empty_state(
            "playlist-symbolic",
            fl!("playlist-empty"),
            fl!("playlist-empty-hint"),
        )
    } else {
        let columns = track_row::Columns {
            artist: true,
            album: true,
            queue_actions: true,
            trailing: true,
            ..Default::default()
        };
        track_row::width_aware(playlist.tracks.len(), true, move |width| {
            let columns = columns.responsive(width);
            track_row::rows_column(
                Some(track_row::Header::new(columns)),
                playlist
                    .tracks
                    .iter()
                    .enumerate()
                    .map(|(track_idx, track)| {
                        track_row::TrackRow::new(
                            track,
                            (track_idx + 1).to_string(),
                            false,
                            columns,
                            PlaylistMessage::PlayTrack(playlist_index, track_idx),
                        )
                        .with_navigate(PlaylistMessage::Navigate)
                        .with_queue_actions(
                            PlaylistMessage::PlayNext(playlist_index, Some(track_idx)),
                            PlaylistMessage::AddToQueue(playlist_index, Some(track_idx)),
                        )
                        .with_trailing_action(
                            "list-remove-symbolic",
                            fl!("remove-from-playlist"),
                            PlaylistMessage::RemoveTrack(playlist_index, track_idx),
                        )
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

/// Localized "N tracks" label used in playlist row/header captions.
fn playlist_track_count_label(count: u32) -> String {
    if count == 1 {
        fl!("playlist-track-count-one", count = count.to_string())
    } else {
        fl!("playlist-track-count-other", count = count.to_string())
    }
}
