// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(feature = "visualizer")]
use super::VIZ_HUD_HOLD_FRAMES;
use super::{AppModel, ContextPage, MenuAction, Message, Page, SEARCH_INPUT_ID, unfilter_index};
use crate::fl;
use crate::library::{Album, Artist, Track};
use crate::player::PlaybackState;
use crate::views::radio as radio_view;
use crate::views::{
    albums, artists, convert, equalizer, genres, lyrics, now_playing, playlists, podcasts,
    providers, queue, settings, songs,
};
use cosmic::app::context_drawer;
use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget::{self, icon, menu};
#[cfg(feature = "visualizer")]
use std::sync::Arc;
use std::time::Duration;

/// Map lyrics drawer messages (shared by its pinned header and its body).
fn map_lyrics_message(msg: lyrics::LyricsMessage) -> Message {
    match msg {
        lyrics::LyricsMessage::FetchLyrics => Message::FetchLyricsOnline,
        lyrics::LyricsMessage::Close => Message::ToggleContextPage(ContextPage::Lyrics),
    }
}

/// Map queue drawer messages (shared by its pinned header and its rows).
fn map_queue_message(msg: queue::QueueMessage) -> Message {
    match msg {
        queue::QueueMessage::Jump(i) => Message::QueueJump(i),
        queue::QueueMessage::MoveUp(i) => Message::QueueMove {
            from: i,
            to: i.saturating_sub(1),
        },
        queue::QueueMessage::MoveDown(i) => Message::QueueMove { from: i, to: i + 1 },
        queue::QueueMessage::Remove(i) => Message::QueueRemove(i),
        queue::QueueMessage::Clear => Message::QueueClear,
        queue::QueueMessage::Navigate(route) => Message::Navigate(route),
    }
}

impl AppModel {
    /// Header bar start: menu bar.
    pub(super) fn header_start_elements(&self) -> Vec<Element<'_, Message>> {
        // Leading glyph for every menu entry, so the dropdowns read as a
        // scannable list instead of a wall of plain text.
        let glyph = |name: &'static str| Some(icon::from_name(name).handle());
        let menu_bar = menu::bar(vec![
            menu::Tree::with_children(
                menu::root(fl!("file")).apply(Element::from),
                menu::items(
                    &self.key_binds,
                    vec![
                        // `menu::Item::Divider` renders as a solid filled
                        // block (not a thin line) in this pinned libcosmic
                        // revision -- omitted rather than shipping a
                        // visibly-broken separator.
                        menu::Item::Button(
                            fl!("add-music-folder"),
                            glyph("folder-new-symbolic"),
                            MenuAction::AddMusicDir,
                        ),
                        menu::Item::Button(
                            fl!("scan-library"),
                            glyph("view-refresh-symbolic"),
                            MenuAction::ScanLibrary,
                        ),
                        menu::Item::Button(
                            fl!("quit"),
                            glyph("application-exit-symbolic"),
                            MenuAction::Quit,
                        ),
                    ],
                ),
            ),
            menu::Tree::with_children(
                menu::root(fl!("view")).apply(Element::from),
                menu::items(
                    &self.key_binds,
                    vec![
                        menu::Item::Button(
                            fl!("search"),
                            glyph("edit-find-symbolic"),
                            MenuAction::Search,
                        ),
                        menu::Item::Button(
                            fl!("equalizer"),
                            glyph("multimedia-equalizer-symbolic"),
                            MenuAction::Equalizer,
                        ),
                        menu::Item::Button(
                            fl!("providers"),
                            glyph("network-server-symbolic"),
                            MenuAction::Providers,
                        ),
                        menu::Item::Button(
                            fl!("settings"),
                            glyph("preferences-system-symbolic"),
                            MenuAction::Settings,
                        ),
                        menu::Item::Button(
                            fl!("queue"),
                            glyph("media-playlist-consecutive-symbolic"),
                            MenuAction::Queue,
                        ),
                        menu::Item::Button(
                            fl!("about"),
                            glyph("help-about-symbolic"),
                            MenuAction::About,
                        ),
                    ],
                ),
            ),
        ]);

        vec![menu_bar.into()]
    }

    /// Header bar center: library search input, shown when search is
    /// active (playback controls are in the bottom bar). The field fills
    /// the centre slot up to a comfortable reading width so it never looks
    /// stranded or cramped as the window is resized.
    pub(super) fn header_center_elements(&self) -> Vec<Element<'_, Message>> {
        if !self.search_active {
            return vec![];
        }

        let input = widget::search_input(fl!("search-library"), &self.library_search)
            .id(widget::Id::new(SEARCH_INPUT_ID))
            .on_input(Message::LibrarySearchChanged)
            .on_clear(Message::ClearLibrarySearch)
            .width(Length::Fill);

        vec![
            widget::container(input)
                .width(Length::Fill)
                .max_width(420.0)
                .into(),
        ]
    }

    /// Header bar end: library search toggle, plus the provider selector
    /// (shown when multiple providers are configured).
    pub(super) fn header_end_elements(&self) -> Vec<Element<'_, Message>> {
        let mut elements: Vec<Element<'_, Message>> = vec![
            widget::button::icon(icon::from_name("edit-find-symbolic"))
                .selected(self.search_active)
                .tooltip(fl!("search"))
                .on_press(Message::ToggleLibrarySearch)
                .into(),
        ];

        // Card-size zoom, only on pages currently showing a card grid.
        let shows_card_grid = match self.nav.active_data::<Page>() {
            Some(Page::Albums) => {
                self.selected_album.is_none()
                    && self.config.albums_view_mode == crate::config::ViewMode::Grid
            }
            Some(Page::Artists) => {
                self.selected_artist.is_none()
                    && self.config.artists_view_mode == crate::config::ViewMode::Grid
            }
            Some(Page::Genres) => {
                self.selected_genre.is_none()
                    && self.config.genres_view_mode == crate::config::ViewMode::Grid
            }
            Some(Page::Folders | Page::Radio) => true,
            Some(Page::Podcasts) => self.selected_podcast.is_none(),
            _ => false,
        };
        if shows_card_grid {
            let space_xs = cosmic::theme::active().cosmic().spacing.space_xs;
            let zoom = widget::Row::new()
                .push(icon::from_name("zoom-out-symbolic").size(16))
                .push(
                    widget::slider(
                        crate::views::common::GRID_SCALE_RANGE,
                        crate::views::common::grid_scale(),
                        Message::SetGridScale,
                    )
                    .step(0.05_f32)
                    .width(Length::Fixed(120.0)),
                )
                .push(icon::from_name("zoom-in-symbolic").size(16))
                .spacing(space_xs)
                .align_y(Alignment::Center);
            elements.insert(
                0,
                widget::tooltip(
                    zoom,
                    widget::text::caption(fl!("zoom-grid-tooltip")),
                    widget::tooltip::Position::Bottom,
                )
                .into(),
            );
        }

        if self.provider_list.len() > 1 {
            let provider_names: Vec<String> = self
                .provider_list
                .iter()
                .map(|(_, name)| name.clone())
                .collect();

            let dropdown = widget::dropdown(
                provider_names,
                self.active_provider_index,
                Message::SwitchProvider,
            );

            // A leading server glyph makes the selector read as "source",
            // not as an unlabeled floating dropdown next to the search button.
            let selector = widget::Row::new()
                .push(icon::from_name("network-server-symbolic").size(16))
                .push(dropdown)
                .spacing(cosmic::theme::active().cosmic().spacing.space_xs)
                .align_y(Alignment::Center);

            elements.push(selector.into());
        }

        elements
    }

    pub(super) fn context_drawer_page(&self) -> Option<context_drawer::ContextDrawer<'_, Message>> {
        if !self.core.window.show_context {
            return None;
        }

        Some(match self.context_page {
            ContextPage::About => context_drawer::about(
                &self.about,
                |url| Message::LaunchUrl(url.to_string()),
                Message::ToggleContextPage(ContextPage::About),
            ),
            ContextPage::Equalizer => {
                let save_as = self.save_as_name.clone();

                let eq_content = equalizer::equalizer_view(
                    &self.config.equalizer_bands,
                    self.config.equalizer_enabled,
                    self.config.equalizer_preamp,
                    &self.all_presets,
                    self.active_preset_name.as_deref(),
                    self.eq_dirty,
                    &self.save_as_name,
                    &self.autoeq_profiles,
                    self.autoeq_loading,
                    &self.autoeq_search,
                )
                .map(move |msg| match msg {
                    equalizer::EqualizerMessage::SetBand(i, v) => Message::EqSetBand(i, v),
                    equalizer::EqualizerMessage::ToggleEnabled(e) => Message::EqToggle(e),
                    equalizer::EqualizerMessage::SetPreamp(v) => Message::EqSetPreamp(v),
                    equalizer::EqualizerMessage::SelectPreset(name) => {
                        Message::EqSelectPreset(name)
                    }
                    equalizer::EqualizerMessage::SelectAutoEQ(path) => {
                        Message::EqSelectAutoEQ(path)
                    }
                    equalizer::EqualizerMessage::SavePreset => Message::EqSavePreset,
                    equalizer::EqualizerMessage::SaveAsNameChanged(name) => {
                        Message::EqSaveAsNameChanged(name)
                    }
                    equalizer::EqualizerMessage::SavePresetAs => {
                        Message::EqSavePresetAs(save_as.clone())
                    }
                    equalizer::EqualizerMessage::DeletePreset => Message::EqDeletePreset,
                    equalizer::EqualizerMessage::ResetPreset => Message::EqResetPreset,
                    equalizer::EqualizerMessage::FetchAutoEQ => Message::FetchAutoEQIndex,
                    equalizer::EqualizerMessage::AutoEQSearchChanged(query) => {
                        Message::AutoEQSearchChanged(query)
                    }
                });

                context_drawer::context_drawer(
                    eq_content,
                    Message::ToggleContextPage(ContextPage::Equalizer),
                )
                .title(fl!("equalizer"))
            }
            ContextPage::Providers => {
                let providers_content = providers::providers_view(
                    &self.mpd_edit_states,
                    &self.mpd_connection_status,
                    &self.subsonic_edit_states,
                    &self.subsonic_connection_status,
                )
                .map(|msg| match msg {
                    // MPD
                    providers::ProvidersMessage::AddMpd => Message::MpdAddServer,
                    providers::ProvidersMessage::EditName(i, v) => Message::MpdEditName(i, v),
                    providers::ProvidersMessage::EditHost(i, v) => Message::MpdEditHost(i, v),
                    providers::ProvidersMessage::EditPort(i, v) => Message::MpdEditPort(i, v),
                    providers::ProvidersMessage::EditPassword(i, v) => {
                        Message::MpdEditPassword(i, v)
                    }
                    providers::ProvidersMessage::Save(i) => Message::MpdSaveServer(i),
                    providers::ProvidersMessage::Remove(i) => Message::MpdRemoveServer(i),
                    providers::ProvidersMessage::TestConnection(i) => Message::MpdTestConnection(i),
                    // Subsonic
                    providers::ProvidersMessage::AddSubsonic => Message::SubsonicAddServer,
                    providers::ProvidersMessage::SubsonicEditName(i, v) => {
                        Message::SubsonicEditName(i, v)
                    }
                    providers::ProvidersMessage::SubsonicEditUrl(i, v) => {
                        Message::SubsonicEditUrl(i, v)
                    }
                    providers::ProvidersMessage::SubsonicEditUsername(i, v) => {
                        Message::SubsonicEditUsername(i, v)
                    }
                    providers::ProvidersMessage::SubsonicEditPassword(i, v) => {
                        Message::SubsonicEditPassword(i, v)
                    }
                    providers::ProvidersMessage::SubsonicToggleCerts(i, v) => {
                        Message::SubsonicToggleCerts(i, v)
                    }
                    providers::ProvidersMessage::SubsonicSave(i) => Message::SubsonicSaveServer(i),
                    providers::ProvidersMessage::SubsonicRemove(i) => {
                        Message::SubsonicRemoveServer(i)
                    }
                    providers::ProvidersMessage::SubsonicTestConnection(i) => {
                        Message::SubsonicTestConnection(i)
                    }
                    // Transcoding (Task 109)
                    providers::ProvidersMessage::SubsonicTranscodingBitrate(i, br) => {
                        Message::SubsonicTranscodingBitrate(i, br)
                    }
                    providers::ProvidersMessage::SubsonicTranscodingFormat(i, f) => {
                        Message::SubsonicTranscodingFormat(i, f)
                    }
                });

                context_drawer::context_drawer(
                    providers_content,
                    Message::ToggleContextPage(ContextPage::Providers),
                )
                .title(fl!("providers"))
            }
            ContextPage::Settings => {
                let volume = self
                    .player
                    .as_ref()
                    .map(|p| p.volume())
                    .unwrap_or(self.config.volume);

                let settings_content = settings::view(
                    &self.config.music_dirs,
                    self.config.crossfade_duration_secs,
                    self.config.replay_gain_mode,
                    volume,
                    self.config.split_artist_tags,
                    self.config.experimental_converter,
                    self.config.fetch_artist_info,
                    &self.artist_tag_delimiters_input,
                    self.config.grid_scale,
                )
                .map(|msg| match msg {
                    settings::SettingsMessage::AddMusicDir => Message::AddMusicDir,
                    settings::SettingsMessage::RemoveMusicDir(i) => Message::RemoveMusicDir(i),
                    settings::SettingsMessage::SetCrossfade(v) => Message::SetCrossfade(v),
                    settings::SettingsMessage::SetReplayGainMode(m) => {
                        Message::SetReplayGainMode(m)
                    }
                    settings::SettingsMessage::SetVolume(v) => Message::SetVolume(v),
                    settings::SettingsMessage::SetGridScale(v) => Message::SetGridScale(v),
                    settings::SettingsMessage::OpenEqualizer => {
                        Message::ToggleContextPage(ContextPage::Equalizer)
                    }
                    settings::SettingsMessage::OpenProviders => {
                        Message::ToggleContextPage(ContextPage::Providers)
                    }
                    settings::SettingsMessage::OpenAbout => {
                        Message::ToggleContextPage(ContextPage::About)
                    }
                    settings::SettingsMessage::SetSplitArtistTags(v) => {
                        Message::SetSplitArtistTags(v)
                    }
                    settings::SettingsMessage::SetFetchArtistInfo(v) => {
                        Message::SetFetchArtistInfo(v)
                    }
                    settings::SettingsMessage::EditArtistTagDelimiters(v) => {
                        Message::ArtistTagDelimitersInputChanged(v)
                    }
                    settings::SettingsMessage::SubmitArtistTagDelimiters(v) => {
                        Message::SubmitArtistTagDelimiters(v)
                    }
                    settings::SettingsMessage::ResetArtistTagDelimiters => {
                        Message::ResetArtistTagDelimiters
                    }
                    settings::SettingsMessage::SetExperimentalConverter(v) => {
                        Message::SetExperimentalConverter(v)
                    }
                });

                context_drawer::context_drawer(
                    settings_content,
                    Message::ToggleContextPage(ContextPage::Settings),
                )
                .title(fl!("settings"))
            }
            ContextPage::Lyrics => {
                let (title, artist) = self
                    .current_track
                    .as_ref()
                    .map(|t| (t.title.as_str(), t.artist.as_str()))
                    .unwrap_or(("", ""));

                let lyrics_content = lyrics::lyrics_view(
                    self.lyrics_text.as_ref(),
                    self.lyrics_loading,
                    self.playback_position,
                    self.accent.as_ref(),
                )
                .map(map_lyrics_message);
                let lyrics_header = lyrics::lyrics_header_view(
                    self.lyrics_text.as_ref(),
                    title,
                    artist,
                    self.playback_position,
                    self.accent.as_ref(),
                )
                .map(map_lyrics_message);

                context_drawer::context_drawer(
                    lyrics_content,
                    Message::ToggleContextPage(ContextPage::Lyrics),
                )
                .title(fl!("lyrics"))
                .header(lyrics_header)
            }
            ContextPage::Queue => {
                let queue_data = self.player.as_ref().map(|p| (p.queue(), p.queue_index()));
                let playback_state = self
                    .player
                    .as_ref()
                    .map(|p| p.state())
                    .unwrap_or(PlaybackState::Stopped);
                let queue_content = queue::queue_view(
                    queue_data,
                    self.current_track.as_ref(),
                    playback_state,
                    &self.cover_images,
                )
                .map(map_queue_message);
                let queue_header = queue::queue_header_view(
                    queue_data,
                    self.current_track.as_ref(),
                    playback_state,
                    &self.cover_images,
                )
                .map(|header| header.map(map_queue_message));

                let drawer = context_drawer::context_drawer(
                    queue_content,
                    Message::ToggleContextPage(ContextPage::Queue),
                )
                .title(fl!("queue"));
                match queue_header {
                    Some(header) => drawer.header(header),
                    None => drawer,
                }
            }
        })
    }

    pub(super) fn view_page(&self) -> Element<'_, Message> {
        let page = self
            .nav
            .active_data::<Page>()
            .cloned()
            .unwrap_or(Page::Albums);
        // Defensive fallback: the Convert nav entry only exists while
        // `experimental_converter` is on (see `AppModel::set_convert_nav_entry`),
        // but guard here too in case `last_view` restoration ever lands on
        // it while the flag is off.
        let page = if page == Page::Convert && !self.config.experimental_converter {
            Page::Albums
        } else {
            page
        };

        let search_query_active = self.search_active && !self.library_search.trim().is_empty();

        let content: Element<'_, Message> = match page {
            Page::Albums => {
                if let Some(album_idx) = self.selected_album {
                    if let Some(album) = self.all_albums.get(album_idx) {
                        albums::album_detail_view(
                            album,
                            album_idx,
                            &self.cover_images,
                            &self.playlists,
                            self.current_track.as_ref().map(|t| t.id),
                            self.detail_hero(&crate::library::CoverArt::album_key(
                                &album.artist,
                                &album.name,
                            )),
                        )
                        .map(Message::from)
                    } else {
                        widget::text("Album not found").into()
                    }
                } else {
                    let (albums_data, album_map): (&[Album], Option<&[usize]>) =
                        if search_query_active {
                            (
                                &self.filtered_albums,
                                Some(self.filtered_album_map.as_slice()),
                            )
                        } else {
                            (&self.all_albums, None)
                        };
                    albums::albums_view(
                        albums_data,
                        &self.cover_images,
                        self.config.albums_view_mode,
                    )
                    .map(move |msg| {
                        Message::from(match msg {
                            albums::AlbumMessage::SelectAlbum(i) => {
                                albums::AlbumMessage::SelectAlbum(unfilter_index(album_map, i))
                            }
                            albums::AlbumMessage::PlayAlbum(i) => {
                                albums::AlbumMessage::PlayAlbum(unfilter_index(album_map, i))
                            }
                            other => other,
                        })
                    })
                }
            }

            Page::Artists => {
                if let Some(artist_idx) = self.selected_artist {
                    if let Some(artist) = self.all_artists.get(artist_idx) {
                        artists::artist_detail_view(
                            artist,
                            artist_idx,
                            &self.artist_photos,
                            self.artist_bios.get(&artist.name).map(String::as_str),
                            self.artist_bio_expanded,
                            &self.cover_images,
                            self.current_track.as_ref().map(|t| t.id),
                            artist.albums.iter().find_map(|a| {
                                self.detail_hero(&crate::library::CoverArt::album_key(
                                    &artist.name,
                                    &a.name,
                                ))
                            }),
                        )
                        .map(Message::from)
                    } else {
                        widget::text("Artist not found").into()
                    }
                } else {
                    let (artists_data, artist_map): (&[Artist], Option<&[usize]>) =
                        if search_query_active {
                            (
                                &self.filtered_artists,
                                Some(self.filtered_artist_map.as_slice()),
                            )
                        } else {
                            (&self.all_artists, None)
                        };
                    artists::artists_view(
                        artists_data,
                        &self.artist_photos,
                        self.config.artists_view_mode,
                    )
                    .map(move |msg| {
                        Message::from(match msg {
                            artists::ArtistMessage::SelectArtist(i) => {
                                artists::ArtistMessage::SelectArtist(unfilter_index(artist_map, i))
                            }
                            artists::ArtistMessage::PlayArtistAlbum(ai, ali) => {
                                artists::ArtistMessage::PlayArtistAlbum(
                                    unfilter_index(artist_map, ai),
                                    ali,
                                )
                            }
                            artists::ArtistMessage::PlayTrack(ai, ali, ti) => {
                                artists::ArtistMessage::PlayTrack(
                                    unfilter_index(artist_map, ai),
                                    ali,
                                    ti,
                                )
                            }
                            other => other,
                        })
                    })
                }
            }

            Page::Songs => {
                let (tracks_data, track_map): (&[Track], Option<&[usize]>) = if search_query_active
                {
                    (
                        &self.filtered_tracks,
                        Some(self.filtered_track_map.as_slice()),
                    )
                } else {
                    (&self.all_tracks, None)
                };
                songs::songs_list_view(
                    tracks_data,
                    self.songs_sort,
                    self.songs_sort_descending,
                    self.favorites_filter,
                    self.genre_filter.as_deref(),
                    &self.playlists,
                    self.current_track.as_ref().map(|t| t.id),
                    self.songs_scroll_offset,
                )
                .map(move |msg| match msg {
                    songs::SongMessage::Navigate(route) => Message::Navigate(route),
                    songs::SongMessage::PlayTrack(i) => {
                        Message::PlayTrackIndex(unfilter_index(track_map, i))
                    }
                    songs::SongMessage::SortBy(f) => Message::SortSongs(f),
                    songs::SongMessage::ToggleFavorite(id) => Message::ToggleFavorite(id),
                    songs::SongMessage::SetRating(id, r) => Message::SetRating(id, r),
                    songs::SongMessage::AddToPlaylist(uri, pid) => Message::AddToPlaylist(uri, pid),
                    songs::SongMessage::ToggleFavoritesFilter => Message::ToggleFavoritesFilter,
                    songs::SongMessage::FilterByGenre(g) => Message::FilterByGenre(g),
                    songs::SongMessage::ClearGenreFilter => Message::FilterByGenre(String::new()),
                    songs::SongMessage::Scrolled(y) => Message::SongsScrolled(y),
                    // `i` indexes `tracks_data` (the slice actually shown,
                    // filtered or not), so resolve it there directly —
                    // `unfilter_index` maps into `all_tracks` instead.
                    songs::SongMessage::PlayNext(i) => {
                        Message::PlayNext(tracks_data.get(i).cloned().into_iter().collect())
                    }
                    songs::SongMessage::AddToQueue(i) => {
                        Message::AddToQueue(tracks_data.get(i).cloned().into_iter().collect())
                    }
                })
            }

            Page::Playlists => {
                if let Some(pl_idx) = self.selected_playlist {
                    if let Some(playlist) = self.playlists.get(pl_idx) {
                        playlists::playlist_detail_view(
                            playlist,
                            pl_idx,
                            &self.rename_playlist_input,
                        )
                        .map(|msg| match msg {
                            playlists::PlaylistMessage::Navigate(route) => Message::Navigate(route),
                            playlists::PlaylistMessage::BackToList => Message::BackToPlaylistList,
                            playlists::PlaylistMessage::PlayPlaylist(i) => Message::PlayPlaylist(i),
                            playlists::PlaylistMessage::PlayTrack(pi, ti) => {
                                Message::PlayPlaylistTrack(pi, ti)
                            }
                            playlists::PlaylistMessage::RemoveTrack(pi, ti) => {
                                Message::RemovePlaylistTrack(pi, ti)
                            }
                            playlists::PlaylistMessage::SelectPlaylist(i) => {
                                Message::SelectPlaylist(i)
                            }
                            playlists::PlaylistMessage::CreatePlaylist(n) => {
                                Message::CreatePlaylist(n)
                            }
                            playlists::PlaylistMessage::DeletePlaylist(i) => {
                                Message::DeletePlaylist(i)
                            }
                            playlists::PlaylistMessage::RenamePlaylist(i, n) => {
                                Message::RenamePlaylist(i, n)
                            }
                            playlists::PlaylistMessage::NewPlaylistNameChanged(n) => {
                                Message::NewPlaylistNameChanged(n)
                            }
                            playlists::PlaylistMessage::RenameInputChanged(i, n) => {
                                Message::RenamePlaylistInput(i, n)
                            }
                            playlists::PlaylistMessage::PlayNext(tracks) => {
                                Message::PlayNext(tracks)
                            }
                            playlists::PlaylistMessage::AddToQueue(tracks) => {
                                Message::AddToQueue(tracks)
                            }
                        })
                    } else {
                        widget::text("Playlist not found").into()
                    }
                } else {
                    let (playlists_data, playlist_map): (
                        &[crate::library::Playlist],
                        Option<&[usize]>,
                    ) = if search_query_active {
                        (
                            &self.filtered_playlists,
                            Some(self.filtered_playlist_map.as_slice()),
                        )
                    } else {
                        (&self.playlists, None)
                    };
                    playlists::playlist_list_view(playlists_data, &self.new_playlist_name).map(
                        move |msg| match msg {
                            playlists::PlaylistMessage::Navigate(route) => Message::Navigate(route),
                            playlists::PlaylistMessage::SelectPlaylist(i) => {
                                Message::SelectPlaylist(unfilter_index(playlist_map, i))
                            }
                            playlists::PlaylistMessage::CreatePlaylist(n) => {
                                Message::CreatePlaylist(n)
                            }
                            playlists::PlaylistMessage::DeletePlaylist(i) => {
                                Message::DeletePlaylist(unfilter_index(playlist_map, i))
                            }
                            playlists::PlaylistMessage::RenamePlaylist(i, n) => {
                                Message::RenamePlaylist(unfilter_index(playlist_map, i), n)
                            }
                            playlists::PlaylistMessage::NewPlaylistNameChanged(n) => {
                                Message::NewPlaylistNameChanged(n)
                            }
                            playlists::PlaylistMessage::RenameInputChanged(i, n) => {
                                Message::RenamePlaylistInput(unfilter_index(playlist_map, i), n)
                            }
                            playlists::PlaylistMessage::BackToList => Message::BackToPlaylistList,
                            playlists::PlaylistMessage::PlayPlaylist(i) => {
                                Message::PlayPlaylist(unfilter_index(playlist_map, i))
                            }
                            playlists::PlaylistMessage::PlayTrack(pi, ti) => {
                                Message::PlayPlaylistTrack(unfilter_index(playlist_map, pi), ti)
                            }
                            playlists::PlaylistMessage::RemoveTrack(pi, ti) => {
                                Message::RemovePlaylistTrack(unfilter_index(playlist_map, pi), ti)
                            }
                            playlists::PlaylistMessage::PlayNext(tracks) => {
                                Message::PlayNext(tracks)
                            }
                            playlists::PlaylistMessage::AddToQueue(tracks) => {
                                Message::AddToQueue(tracks)
                            }
                        },
                    )
                }
            }

            Page::SmartPlaylists => {
                if let Some(editor) = &self.smart_playlist_editor {
                    crate::views::smart_playlists::editor_view(editor).map(Message::SmartPlaylists)
                } else if let Some(idx) = self.selected_smart_playlist {
                    if let Some(playlist) = self.smart_playlists.get(idx) {
                        crate::views::smart_playlists::smart_playlist_detail_view(
                            playlist,
                            idx,
                            &self.smart_playlist_tracks,
                            self.current_track.as_ref().map(|t| t.id),
                        )
                        .map(Message::SmartPlaylists)
                    } else {
                        widget::text("Smart playlist not found").into()
                    }
                } else {
                    crate::views::smart_playlists::smart_playlists_view(&self.smart_playlists)
                        .map(Message::SmartPlaylists)
                }
            }

            Page::Genres => {
                if let Some(genre_idx) = self.selected_genre {
                    if let Some(genre_name) = self.all_genres.get(genre_idx) {
                        genres::genre_detail_view(
                            genre_name,
                            &self.genre_tracks,
                            self.current_track.as_ref().map(|t| t.id),
                        )
                        .map(|msg| match msg {
                            genres::GenreMessage::Navigate(route) => Message::Navigate(route),
                            genres::GenreMessage::BackToGrid => Message::BackToGenreGrid,
                            genres::GenreMessage::PlayTrack(i) => Message::PlayGenreTrack(i),
                            genres::GenreMessage::Shuffle => Message::ShuffleGenre,
                            genres::GenreMessage::PlayNext(t) => Message::PlayNext(t),
                            genres::GenreMessage::AddToQueue(t) => Message::AddToQueue(t),
                            genres::GenreMessage::SelectGenre(i) => Message::SelectGenre(i),
                            genres::GenreMessage::ToggleViewMode => Message::ToggleGenresViewMode,
                        })
                    } else {
                        widget::text("Genre not found").into()
                    }
                } else {
                    let (genres_data, genre_map): (&[String], Option<&[usize]>) =
                        if search_query_active {
                            (
                                &self.filtered_genres,
                                Some(self.filtered_genre_map.as_slice()),
                            )
                        } else {
                            (&self.all_genres, None)
                        };
                    genres::genres_view(genres_data, self.config.genres_view_mode).map(move |msg| {
                        match msg {
                            genres::GenreMessage::Navigate(route) => Message::Navigate(route),
                            genres::GenreMessage::SelectGenre(i) => {
                                Message::SelectGenre(unfilter_index(genre_map, i))
                            }
                            genres::GenreMessage::BackToGrid => Message::BackToGenreGrid,
                            genres::GenreMessage::PlayTrack(i) => Message::PlayGenreTrack(i),
                            genres::GenreMessage::Shuffle => Message::ShuffleGenre,
                            genres::GenreMessage::PlayNext(t) => Message::PlayNext(t),
                            genres::GenreMessage::AddToQueue(t) => Message::AddToQueue(t),
                            genres::GenreMessage::ToggleViewMode => Message::ToggleGenresViewMode,
                        }
                    })
                }
            }

            Page::Folders => crate::views::folders::folder_view(
                &self.folder_state,
                &self.all_tracks,
                self.current_track.as_ref(),
                &self.cover_images,
            )
            .map(Message::Folders),

            Page::Podcasts => {
                let is_podcast_track = self
                    .current_track
                    .as_ref()
                    .is_some_and(|t| &*t.provider_id == "podcast");
                let playback_state = self.player.as_ref().map(|p| p.state());
                let is_episode_playing = is_podcast_track
                    && matches!(
                        playback_state,
                        Some(PlaybackState::Playing) | Some(PlaybackState::Paused)
                    );
                let is_paused = matches!(playback_state, Some(PlaybackState::Paused));
                let selected = self
                    .selected_podcast
                    .and_then(|id| self.podcasts.iter().find(|p| p.id == id));
                let props = podcasts::PodcastViewProps {
                    podcasts: &self.podcasts,
                    tab: self.podcast_tab,
                    add_open: self.podcast_add_open,
                    add_url: &self.podcast_add_url,
                    add_error: self.podcast_add_error.as_deref(),
                    refreshing: &self.refreshing_podcasts,
                    pending_unsubscribe: self.pending_unsubscribe_podcast,
                    search_query: &self.podcast_search_query,
                    search_results: &self.podcast_search_results,
                    search_loading: self.podcast_search_loading,
                    search_error: self.podcast_search_error.as_deref(),
                    selected,
                    episodes: &self.podcast_episodes,
                    episode_filter: self.podcast_episode_filter,
                    episode_text_filter: &self.podcast_episode_text_filter,
                    description_expanded: self.podcast_description_expanded,
                    downloading: &self.downloading_episodes,
                    current_episode_id: self.current_podcast_episode_id,
                    is_episode_playing,
                    is_paused,
                    icons: &self.online_icons,
                };
                podcasts::podcast_view(props).map(Message::Podcast)
            }

            Page::Radio => {
                let props = radio_view::RadioViewProps {
                    stations: &self.radio_stations,
                    tab: self.radio_tab,
                    filter: &self.radio_filter,
                    add_open: self.radio_add_open,
                    add_name: &self.radio_add_name,
                    add_url: &self.radio_add_url,
                    add_error: self.radio_add_error.as_deref(),
                    renaming_id: self.radio_renaming_id,
                    rename_input: &self.radio_rename_input,
                    search_query: &self.radio_search_query,
                    search_tag: self.radio_search_tag,
                    search_country: &self.radio_search_country,
                    locale_country: &self.radio_locale_country,
                    sort: self.radio_search_sort,
                    results: &self.radio_search_results,
                    results_loading: self.radio_search_loading,
                    results_error: self.radio_search_error.as_deref(),
                    icons: &self.online_icons,
                    current_track: self.current_track.as_ref(),
                    playback_state: self.player.as_ref().map(|p| p.state()),
                    now_playing_favicon: &self.radio_now_playing_favicon,
                    now_playing_key: &self.radio_now_playing_key,
                };
                radio_view::radio_view(props).map(|msg| match msg {
                    radio_view::RadioMessage::Stop => Message::Stop,
                    other => Message::Radio(other),
                })
            }

            Page::Convert => {
                let out_dir = self.convert_out_dir();
                convert::convert_view(convert::ConvertViewProps {
                    jobs: &self.convert_jobs,
                    out_dir: &out_dir,
                    format: self.config.convert_format,
                    sample_rate: self.config.convert_sample_rate,
                    dir_error: self.convert_dir_error.as_deref(),
                    flac_options: self.config.flac_options,
                    lossy_options: self.config.lossy_options,
                    ffmpeg_available: crate::convert::ffmpeg::cached(),
                })
                .map(Message::Convert)
            }
        };

        // Build bottom playback bar
        let state = self
            .player
            .as_ref()
            .map(|p| p.state())
            .unwrap_or(PlaybackState::Stopped);
        let duration = self
            .current_track
            .as_ref()
            .map(|t| t.duration)
            .unwrap_or(Duration::ZERO);
        let volume = self
            .player
            .as_ref()
            .map(|p| p.volume())
            .unwrap_or(self.config.volume);
        let current_cover_key = self.current_track.as_ref().map(|track| {
            // Use album_artist to match how albums store cover art.
            // Falls back to track.artist when album_artist is empty.
            let artist = if track.album_artist.is_empty() {
                &track.artist
            } else {
                &track.album_artist
            };
            crate::library::CoverArt::album_key(artist, &track.album)
        });
        let current_cover = current_cover_key
            .as_ref()
            .and_then(|key| self.cover_images.get(key));

        // Whether the Up Next queue drawer is the currently open context
        // page — drives the toggle button's `selected` state in both
        // now-playing views.
        let is_queue_open =
            self.context_page == ContextPage::Queue && self.core.window.show_context;
        // Separately decoded, higher-resolution cover for the expanded
        // now-playing view (see `AppModel::maybe_update_blurred_cover`) --
        // `cover_images` only holds grid-thumbnail-sized handles now, which
        // would look soft blown up to the expanded view's much larger art
        // area. Only used while it belongs to the current track's album;
        // falls back to the grid thumbnail until the larger decode
        // completes (e.g. right after a track change).
        let current_cover_large = self
            .current_cover_large
            .as_ref()
            .filter(|(key, _)| current_cover_key.as_ref() == Some(key))
            .map(|(_, handle)| handle)
            .or(current_cover);

        // Helper closure to map NowPlayingMessage to Message
        let map_now_playing_msg = |msg| match msg {
            now_playing::NowPlayingMessage::Navigate(route) => Message::Navigate(route),
            now_playing::NowPlayingMessage::TogglePlayback => Message::TogglePlayback,
            now_playing::NowPlayingMessage::Next => Message::NextTrack,
            now_playing::NowPlayingMessage::Previous => Message::PreviousTrack,
            now_playing::NowPlayingMessage::SeekPreview(v) => Message::SeekPreview(v),
            now_playing::NowPlayingMessage::SeekCommit => Message::SeekCommit,
            now_playing::NowPlayingMessage::SetVolume(v) => Message::SetVolume(v),
            now_playing::NowPlayingMessage::VolumeCommit => Message::VolumeCommit,
            now_playing::NowPlayingMessage::ToggleShuffle => Message::ToggleShuffle,
            now_playing::NowPlayingMessage::CycleRepeat => Message::CycleRepeat,
            now_playing::NowPlayingMessage::ShowLyrics => Message::ShowLyrics,
            now_playing::NowPlayingMessage::ExpandToggle => Message::ExpandNowPlaying,
            now_playing::NowPlayingMessage::Collapse => Message::CollapseNowPlaying,
            now_playing::NowPlayingMessage::ToggleFavorite(id) => Message::ToggleFavorite(id),
            now_playing::NowPlayingMessage::Stop => Message::Stop,
            now_playing::NowPlayingMessage::ToggleQueue => {
                Message::ToggleContextPage(ContextPage::Queue)
            }
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::ToggleVisualizer => Message::ToggleVisualizer,
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::NextPreset => Message::NextVisualizerPreset,
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::ToggleVizFullscreen => {
                Message::ToggleVisualizerFullscreen
            }
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::VizHudPointerEnter => Message::VizHudPointerEnter,
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::VizHudPointerExit => Message::VizHudPointerExit,
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::TogglePresetBrowser => Message::TogglePresetBrowser,
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::PresetSearchInput(query) => {
                Message::PresetSearchInput(query)
            }
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::LoadVizPreset(path) => Message::LoadVizPreset(path),
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::SetVizLocked(locked) => Message::SetVizLocked(locked),
            #[cfg(feature = "visualizer")]
            now_playing::NowPlayingMessage::SetVizBeatSensitivity(v) => {
                Message::SetVizBeatSensitivity(v)
            }
        };

        let bar = now_playing::compact_bar::playback_bar(
            self.current_track.as_ref(),
            state,
            self.playback_position,
            duration,
            volume,
            self.config.shuffle,
            self.config.repeat_mode,
            current_cover,
            self.seeking_preview,
            self.blurred_cover.as_ref(),
            self.accent.as_ref(),
            is_queue_open,
        )
        .map(map_now_playing_msg);

        // Main layout: library content + optional scanning indicator +
        // bottom playback bar, always mounted (so scroll positions survive
        // an expand/collapse), with the expanded now-playing view sliding
        // up over it as a sheet while `expand_progress > 0`. Both layers
        // animate themselves at draw time — see `now_playing::sheet`.
        let mut layout_col = widget::Column::new().push(
            widget::container(content)
                .width(Length::Fill)
                .height(Length::Fill),
        );

        // Always push a slot for the scanning indicator, varying only
        // its content -- conditionally pushing/removing this sibling
        // before `bar` (a stateful widget tree) would shift `bar`'s
        // position in the column, resetting/flashing its state (see
        // module-level notes on iced's tree-position-keyed widget
        // state).
        let scanning_indicator: Element<'_, Message> = if self.library_scanning {
            use cosmic::cosmic_theme::palette::WithAlpha;
            let space_xs = cosmic::theme::active().cosmic().spacing.space_xs;
            let pill = widget::container(
                widget::Row::new()
                    .push(widget::indeterminate_circular().size(14.0).bar_height(2.0))
                    .push(widget::text::caption(fl!("scanning-library")))
                    .spacing(space_xs)
                    .align_y(Alignment::Center),
            )
            .padding([4, 12])
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                let accent = cosmic.accent_color();
                cosmic::iced::widget::container::Style {
                    background: Some(cosmic::iced::Background::Color(
                        accent.with_alpha(0.14).into(),
                    )),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border: cosmic::iced::Border {
                        radius: cosmic.corner_radii.radius_xl.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }));
            widget::container(pill)
                .padding([space_xs, 0])
                .width(Length::Fill)
                .align_x(cosmic::iced::alignment::Horizontal::Center)
                .into()
        } else {
            widget::container(
                widget::Space::new()
                    .width(Length::Fill)
                    .height(Length::Fixed(0.0)),
            )
            .width(Length::Fill)
            .into()
        };
        layout_col = layout_col.push(scanning_indicator);
        layout_col = layout_col.push(bar);

        let sheet_open = self
            .expand_target
            .map_or(self.expand_progress > 0.0, |t| t > 0.5);
        let sheet_transitioning = self.expand_target.is_some();
        let mut stack = cosmic::iced::widget::Stack::new()
            .width(Length::Fill)
            .height(Length::Fill)
            .push(now_playing::sheet::underlay(
                layout_col,
                sheet_open,
                sheet_transitioning,
            ));

        if self.expand_progress > 0.0 {
            #[cfg(feature = "visualizer")]
            let viz_hud_visible = self.viz_hud_pointer_over
                || self.viz_hud_idle_frames < VIZ_HUD_HOLD_FRAMES
                || self.viz_browser_open;
            let expanded = now_playing::expanded_view::expanded_now_playing(
                self.current_track.as_ref(),
                state,
                self.playback_position,
                duration,
                volume,
                self.config.shuffle,
                self.config.repeat_mode,
                current_cover_large,
                self.blurred_cover.as_ref(),
                self.accent.as_ref(),
                is_queue_open,
                self.seeking_preview,
                self.expand_progress,
                self.lyrics_overlay_active,
                self.lyrics_text.as_ref(),
                self.lyrics_loading,
                #[cfg(feature = "visualizer")]
                self.visualizer_active,
                #[cfg(feature = "visualizer")]
                Arc::clone(&self.viz_frame_buf),
                #[cfg(feature = "visualizer")]
                self.viz_metadata_opacity,
                #[cfg(feature = "visualizer")]
                self.viz_fullscreen,
                #[cfg(feature = "visualizer")]
                viz_hud_visible,
                #[cfg(feature = "visualizer")]
                self.viz_browser_open,
                #[cfg(feature = "visualizer")]
                &self.viz_preset_entries,
                #[cfg(feature = "visualizer")]
                &self.viz_preset_search,
                #[cfg(feature = "visualizer")]
                self.viz_locked,
                #[cfg(feature = "visualizer")]
                self.viz_beat_sensitivity,
                #[cfg(feature = "visualizer")]
                self.viz_current_preset_name.as_deref(),
            )
            .map(map_now_playing_msg);

            stack = stack.push(now_playing::sheet::sheet(
                widget::container(expanded)
                    .width(Length::Fill)
                    .height(Length::Fill),
                sheet_open,
                sheet_transitioning,
            ));
        }

        let layout: Element<'_, Message> = stack.into();

        // WindowBackground pins the app surface to background.base color and
        // sets icon_color/text_color to background.on so all child widgets
        // inherit the correct foreground regardless of maximize state or
        // compositor behavior (which may otherwise paint a transparent/white
        // surface behind the content area).
        let background = widget::container(layout)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::WindowBackground);

        widget::toaster(&self.toasts, background)
    }
}
