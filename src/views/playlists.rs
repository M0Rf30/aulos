// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Playlists view - list of playlists with create/delete/rename actions,
//! and a detail view showing tracks in a selected playlist.

use crate::fl;
use crate::library::{Playlist, Track};
use crate::views::common;
use crate::views::list_row_button_class;
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
    /// Insert every track in this playlist right after the currently
    /// playing one.
    PlayNext(Vec<Track>),
    /// Append every track in this playlist to the end of the queue.
    AddToQueue(Vec<Track>),
}

/// Render the playlist list view.
pub fn playlist_list_view<'a>(
    playlists: &'a [Playlist],
    new_playlist_name: &'a str,
) -> cosmic::Element<'a, PlaylistMessage> {
    let mut col = widget::Column::new().spacing(12).padding(16);

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
            .push(common::cell_text(playlist.name.as_str()))
            .push(common::cell_caption(format!(
                "{}  -  {}",
                playlist_track_count_label(track_count),
                common::format_duration_coarse(playlist.total_duration.as_secs())
            )))
            .spacing(2);

        let delete_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("edit-delete-symbolic").size(16))
                .class(cosmic::theme::Button::Destructive)
                .on_press(PlaylistMessage::DeletePlaylist(index)),
            widget::text::caption(fl!("delete-playlist-tooltip")),
            widget::tooltip::Position::Top,
        );

        let playlist_icon: cosmic::Element<'_, PlaylistMessage> =
            widget::icon::from_name("playlist-symbolic").size(40).into();

        let row = widget::button::custom(
            widget::Row::new()
                .push(playlist_icon)
                .push(common::clipped_cell(info.into()))
                .push(delete_btn)
                .spacing(12)
                .align_y(Alignment::Center)
                .padding(8),
        )
        .on_press(PlaylistMessage::SelectPlaylist(index))
        .width(Length::Fill)
        .class(list_row_button_class(false));

        list = list.push(row);
    }

    col = col
        .push(widget::scrollable(widget::container(list).width(Length::Fill)).height(Length::Fill));

    col.into()
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
                                .on_press(PlaylistMessage::PlayNext(playlist.tracks.clone())),
                        )
                        .push(
                            widget::button::standard(fl!("queue-add"))
                                .on_press(PlaylistMessage::AddToQueue(playlist.tracks.clone())),
                        )
                        .spacing(8),
                )
                .spacing(8)
                .width(Length::Fill),
        )
        .spacing(16)
        .align_y(Alignment::Center);

    let mut track_list = widget::Column::new().spacing(2);

    for (track_idx, track) in playlist.tracks.iter().enumerate() {
        let remove_btn = widget::tooltip(
            widget::button::icon(widget::icon::from_name("list-remove-symbolic").size(16))
                .on_press(PlaylistMessage::RemoveTrack(playlist_index, track_idx)),
            widget::text::caption(fl!("remove-from-playlist")),
            widget::tooltip::Position::Top,
        );

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
                .push(common::duration_cell(track.duration.as_secs()))
                .push(remove_btn)
                .spacing(8)
                .width(Length::Fill)
                .align_y(Alignment::Center)
                .padding(4),
        )
        .on_press(PlaylistMessage::PlayTrack(playlist_index, track_idx))
        .width(Length::Fill)
        .class(list_row_button_class(false));

        track_list = track_list.push(row);
    }

    if playlist.tracks.is_empty() {
        track_list = track_list.push(common::empty_state(
            "playlist-symbolic",
            fl!("playlist-empty"),
            fl!("playlist-empty-hint"),
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

/// Localized "N tracks" label used in playlist row/header captions.
fn playlist_track_count_label(count: u32) -> String {
    if count == 1 {
        fl!("playlist-track-count-one", count = count.to_string())
    } else {
        fl!("playlist-track-count-other", count = count.to_string())
    }
}
