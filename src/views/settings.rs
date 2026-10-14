// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Settings view — library folders, playback defaults, and quick links to
//! the Equalizer/Providers drawers and the About dialog.

use crate::views::common;
use aulos_core::config::ReplayGainMode;
use aulos_core::fl;
use aulos_core::settings_search::{SearchQuery, keywords};
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use std::path::PathBuf;

/// Messages emitted by the settings view.
///
/// All variants map onto existing `Message` variants in `app.rs` — this
/// page reuses the flows already owned by the music-dir picker, the
/// playback engine, and the context drawers rather than introducing new
/// mutations.
#[derive(Debug, Clone)]
pub enum SettingsMessage {
    /// Launch the XDG portal directory picker to add a music folder.
    AddMusicDir,
    /// Remove a music directory by index.
    RemoveMusicDir(usize),
    /// Crossfade duration changed (seconds, 0 = disabled).
    SetCrossfade(f32),
    /// Replay gain mode changed.
    SetReplayGainMode(ReplayGainMode),
    /// Playback volume changed.
    SetVolume(f32),
    /// Open the Equalizer context drawer.
    OpenEqualizer,
    /// Open the Providers context drawer.
    OpenProviders,
    /// Open the About dialog.
    OpenAbout,
    /// Toggle multi-artist tag splitting on/off.
    SetSplitArtistTags(bool),
    /// Live text of the delimiter list editor, before submit.
    EditArtistTagDelimiters(String),
    /// Commit the edited delimiter text (parsed on the `" | "` separator).
    SubmitArtistTagDelimiters(String),
    /// Reset the delimiter list to the built-in defaults.
    ResetArtistTagDelimiters,
    /// Toggle the experimental local file converter feature on/off.
    SetExperimentalConverter(bool),
    /// Toggle fetching artist images/biography from online sources
    /// (Local/MPD mode only — ignored in Subsonic mode).
    SetFetchArtistInfo(bool),
    /// Grid card size multiplier changed (`common::GRID_SCALE_RANGE`).
    SetGridScale(f32),
    /// Toggle showing the "Various Artists" compilations entry in the
    /// Artists view.
    SetShowCompilationsInArtists(bool),
    /// Toggle relative paths in exported M3U playlists.
    SetM3uRelativePaths(bool),
    /// A message from the Scrobbling block (see `aulos_core::online::scrobble`).
    Scrobble(aulos_core::online::scrobble::ScrobbleMessage),
    /// A message from the playback extras sections (fades, auto-play,
    /// party mode, desktop integration) — see `crate::app::playback_extras`.
    PlaybackExtras(crate::app::playback_extras::PlaybackExtrasMessage),
    /// A message from the Startup block (start section / provider) — see
    /// `crate::app::startup`.
    Startup(crate::app::startup::StartupMessage),
    /// The AcoustID application API key text changed (persisted to config).
    SetAcoustidApiKey(String),
    /// Live text of the user's AcoustID API key, before saving.
    EditAcoustidUserKey(String),
    /// Store the user's AcoustID API key in the keyring (empty deletes it).
    SaveAcoustidUserKey,
    /// Toggle preferring the Cover Art Archive front cover.
    SetTaggerUseCaaCover(bool),
    /// Open a web page (the AcoustID key pages).
    OpenUrl(String),
    /// The settings search box changed (empty = show everything).
    Search(String),
}

/// Render the Settings page (shown in the context drawer, which already
/// supplies the outer padding and the scrolling). `search` filters the
/// sections: only those whose text matches every word are shown.
#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    music_dirs: &'a [PathBuf],
    crossfade_secs: f32,
    replay_gain_mode: ReplayGainMode,
    volume: f32,
    split_artist_tags: bool,
    experimental_converter: bool,
    fetch_artist_info: bool,
    artist_tag_delimiters_input: &'a str,
    grid_scale: f32,
    show_compilations_in_artists: bool,
    m3u_relative_paths: bool,
    scrobbling: cosmic::Element<'a, SettingsMessage>,
    playback_extras: cosmic::Element<'a, SettingsMessage>,
    startup: cosmic::Element<'a, SettingsMessage>,
    search: &'a str,
    acoustid_api_key: &'a str,
    acoustid_user_key_input: &'a str,
    acoustid_user_key_saved: Option<bool>,
    tagger_use_caa_cover: bool,
) -> cosmic::Element<'a, SettingsMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let query = SearchQuery::new(search);

    let mut search_box = widget::search_input(fl!("settings-search-placeholder"), search)
        .on_input(SettingsMessage::Search)
        .width(Length::Fill);
    if !search.is_empty() {
        search_box = search_box.on_clear(SettingsMessage::Search(String::new()));
    }

    let sections: Vec<(Vec<String>, cosmic::Element<'a, SettingsMessage>)> = vec![
        (keywords::library(), library_section(music_dirs)),
        (
            keywords::artist_tags(),
            artist_tags_section(split_artist_tags, artist_tag_delimiters_input),
        ),
        (
            keywords::compilations(),
            compilations_section(show_compilations_in_artists),
        ),
        (keywords::playlists(), playlists_section(m3u_relative_paths)),
        (
            keywords::playback(),
            playback_section(crossfade_secs, replay_gain_mode, volume),
        ),
        (keywords::playback_extras(), playback_extras),
        (keywords::startup(), startup),
        (keywords::appearance(), appearance_section(grid_scale)),
        (
            keywords::artist_info(),
            artist_info_section(fetch_artist_info),
        ),
        (crate::views::scrobble::keywords(), scrobbling),
        (
            keywords::tagger(),
            tagger_section(
                acoustid_api_key,
                acoustid_user_key_input,
                acoustid_user_key_saved,
                tagger_use_caa_cover,
            ),
        ),
        (
            keywords::experimental(),
            experimental_section(experimental_converter),
        ),
        (keywords::shortcuts(), shortcuts_section()),
        (keywords::about(), about_section()),
    ];

    let mut page = widget::Column::new()
        .spacing(sp.space_l)
        .width(Length::Fill)
        .push(search_box);
    let mut shown = 0;
    for (words, element) in sections {
        if query.matches(&words) {
            page = page.push(element);
            shown += 1;
        }
    }
    if shown == 0 {
        page = page.push(
            widget::container(widget::text::body(fl!(
                "settings-search-empty",
                query = search
            )))
            .padding(sp.space_m)
            .width(Length::Fill),
        );
    }
    page.into()
}

/// Section header: heading plus a dimmed one-line explanation.
fn section_header<'a>(
    title: impl Into<std::borrow::Cow<'a, str>> + 'a,
    description: impl Into<std::borrow::Cow<'a, str>> + 'a,
) -> cosmic::Element<'a, SettingsMessage> {
    widget::Column::new()
        .push(widget::text::heading(title))
        .push(widget::text::caption(description).class(dim_text()))
        .spacing(2)
        .into()
}

/// Theme-driven dimmed text colour for secondary labels and values.
fn dim_text() -> cosmic::theme::Text {
    cosmic::theme::Text::Color(cosmic::theme::active().cosmic().palette.neutral_7.into())
}

/// A slider setting: title with the current value on the right, and the
/// slider spanning the full row underneath (a narrow drawer has no room for
/// a fixed-width slider beside the label).
fn slider_item<'a>(
    title: String,
    value_label: String,
    slider: impl Into<cosmic::Element<'a, SettingsMessage>>,
) -> cosmic::Element<'a, SettingsMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    widget::Column::new()
        .push(
            widget::Row::new()
                .push(widget::text::body(title))
                .push(widget::space::horizontal())
                .push(widget::text::caption(value_label).class(dim_text()))
                .align_y(Alignment::Center),
        )
        .push(slider)
        .spacing(sp.space_xxs)
        .width(Length::Fill)
        .into()
}

/// Library section: configured music directories, with add/remove.
fn library_section<'a>(music_dirs: &'a [PathBuf]) -> cosmic::Element<'a, SettingsMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let mut section = widget::settings::section().header(section_header(
        fl!("settings-library"),
        fl!("settings-library-description"),
    ));

    if music_dirs.is_empty() {
        section = section.add(
            widget::Column::new()
                .push(widget::text::body(fl!("no-music-dirs")))
                .push(widget::text::caption(fl!("no-music-dirs-hint")).class(dim_text()))
                .spacing(2),
        );
    } else {
        for (i, dir) in music_dirs.iter().enumerate() {
            let children: Vec<cosmic::Element<'a, SettingsMessage>> = vec![
                widget::icon::from_name("folder-symbolic").size(16).into(),
                common::clipped_cell(common::cell_text(dir.to_string_lossy()).into()),
                widget::tooltip(
                    widget::button::icon(widget::icon::from_name("edit-delete-symbolic").size(16))
                        .extra_small()
                        .on_press(SettingsMessage::RemoveMusicDir(i)),
                    widget::text::caption(fl!("remove")),
                    widget::tooltip::Position::Top,
                )
                .into(),
            ];
            section = section.add(widget::settings::item_row(children));
        }
    }

    // The primary call to action only when there is nothing yet; otherwise
    // a secondary button so the list stays the focus.
    let add_label = fl!("add-music-folder");
    let add_button = if music_dirs.is_empty() {
        widget::button::suggested(add_label)
    } else {
        widget::button::standard(add_label)
    }
    .leading_icon(widget::icon::from_name("list-add-symbolic").size(16))
    .on_press(SettingsMessage::AddMusicDir);
    section = section.add(
        widget::Row::new()
            .push(widget::space::horizontal())
            .push(add_button)
            .padding([sp.space_xxs, 0])
            .width(Length::Fill),
    );

    section.into()
}

/// Artist tags section: multi-artist tag splitting toggle and delimiter
/// editor (the editor is inert while splitting is off).
fn artist_tags_section<'a>(
    split_artist_tags: bool,
    artist_tag_delimiters_input: &'a str,
) -> cosmic::Element<'a, SettingsMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;

    let split_item = widget::settings::item::builder(fl!("split-artist-tags"))
        .description(fl!("split-artist-tags-description"))
        .toggler(split_artist_tags, SettingsMessage::SetSplitArtistTags);

    let mut input = widget::text_input(
        fl!("artist-tag-delimiters-placeholder"),
        artist_tag_delimiters_input,
    );
    if split_artist_tags {
        input = input
            .on_input(SettingsMessage::EditArtistTagDelimiters)
            .on_submit_maybe(Some(SettingsMessage::SubmitArtistTagDelimiters));
    }
    let reset = widget::button::text(fl!("reset-to-defaults"))
        .on_press_maybe(split_artist_tags.then_some(SettingsMessage::ResetArtistTagDelimiters));

    let delimiters_item = widget::Column::new()
        .push(widget::text::body(fl!("artist-tag-delimiters")))
        .push(
            widget::text::caption(fl!("artist-tag-delimiters-description"))
                .class(dim_text())
                .wrapping(cosmic::iced::core::text::Wrapping::Word),
        )
        .push(
            widget::Row::new()
                .push(input.width(Length::Fill))
                .push(reset)
                .spacing(sp.space_xs)
                .align_y(Alignment::Center),
        )
        .spacing(sp.space_xxs)
        .width(Length::Fill);

    widget::settings::section()
        .header(section_header(
            fl!("settings-artist-tags"),
            fl!("settings-artist-tags-description"),
        ))
        .add(split_item)
        .add(delimiters_item)
        .into()
}

/// Compilations section: whether "Various Artists" appears in the Artists
/// view (compilations always remain browsable from the Albums page).
fn compilations_section<'a>(show_in_artists: bool) -> cosmic::Element<'a, SettingsMessage> {
    widget::settings::section()
        .header(section_header(
            fl!("settings-compilations"),
            fl!("settings-compilations-description"),
        ))
        .add(
            widget::settings::item::builder(fl!("show-compilations-in-artists"))
                .description(fl!("show-compilations-in-artists-description"))
                .toggler(
                    show_in_artists,
                    SettingsMessage::SetShowCompilationsInArtists,
                ),
        )
        .into()
}

/// Playlists section: M3U export path style.
fn playlists_section<'a>(relative_paths: bool) -> cosmic::Element<'a, SettingsMessage> {
    widget::settings::section()
        .header(section_header(
            fl!("settings-playlists"),
            fl!("settings-playlists-description"),
        ))
        .add(
            widget::settings::item::builder(fl!("m3u-relative-paths"))
                .description(fl!("m3u-relative-paths-description"))
                .toggler(relative_paths, SettingsMessage::SetM3uRelativePaths),
        )
        .into()
}

/// Playback section: crossfade duration, replay gain mode, volume.
fn playback_section<'a>(
    crossfade_secs: f32,
    replay_gain_mode: ReplayGainMode,
    volume: f32,
) -> cosmic::Element<'a, SettingsMessage> {
    let crossfade_label = if crossfade_secs < 0.1 {
        fl!("crossfade-disabled")
    } else {
        fl!("crossfade-seconds", secs = format!("{:.0}", crossfade_secs))
    };
    let crossfade_item = widget::Column::new()
        .push(slider_item(
            fl!("crossfade-duration"),
            crossfade_label,
            widget::slider(0.0..=12.0, crossfade_secs, SettingsMessage::SetCrossfade)
                .step(0.5_f32)
                .width(Length::Fill),
        ))
        .push(widget::text::caption(fl!("crossfade-description")).class(dim_text()))
        .spacing(2);

    let replay_gain_labels = vec![
        fl!("replay-gain-off"),
        fl!("replay-gain-track"),
        fl!("replay-gain-album"),
        fl!("replay-gain-auto"),
    ];
    let replay_gain_selected = ReplayGainMode::ALL
        .iter()
        .position(|mode| *mode == replay_gain_mode);
    let replay_gain_item = widget::settings::item::builder(fl!("replay-gain"))
        .description(fl!("replay-gain-description"))
        .control(widget::dropdown(
            replay_gain_labels,
            replay_gain_selected,
            |i| SettingsMessage::SetReplayGainMode(ReplayGainMode::ALL[i]),
        ));

    let volume_item = slider_item(
        fl!("volume"),
        format!("{:.0}%", volume * 100.0),
        widget::slider(0.0..=1.0, volume, SettingsMessage::SetVolume)
            .step(0.01_f32)
            .width(Length::Fill),
    );

    widget::settings::section()
        .header(section_header(
            fl!("settings-playback"),
            fl!("settings-playback-description"),
        ))
        .add(crossfade_item)
        .add(replay_gain_item)
        .add(volume_item)
        .into()
}

/// Appearance section: grid card size (shared by every card grid; also
/// adjustable from the header zoom slider).
fn appearance_section<'a>(grid_scale: f32) -> cosmic::Element<'a, SettingsMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let range = common::GRID_SCALE_RANGE;
    let is_default = (grid_scale - 1.0).abs() < 0.001;

    let slider_row = widget::Row::new()
        .push(widget::icon::from_name("zoom-out-symbolic").size(16))
        .push(
            widget::slider(range, grid_scale, SettingsMessage::SetGridScale)
                .step(0.05_f32)
                .width(Length::Fill),
        )
        .push(widget::icon::from_name("zoom-in-symbolic").size(16))
        .spacing(sp.space_xs)
        .align_y(Alignment::Center);

    let grid_item = widget::Column::new()
        .push(slider_item(
            fl!("grid-size"),
            format!("{:.0}%", grid_scale * 100.0),
            slider_row,
        ))
        .push(
            widget::Row::new()
                .push(
                    widget::text::caption(fl!("grid-size-description"))
                        .class(dim_text())
                        .width(Length::Fill),
                )
                .push(
                    widget::button::text(fl!("reset-to-defaults")).on_press_maybe(
                        (!is_default).then_some(SettingsMessage::SetGridScale(1.0)),
                    ),
                )
                .spacing(sp.space_xs)
                .align_y(Alignment::Center),
        )
        .spacing(sp.space_xxs);

    widget::settings::section()
        .header(section_header(
            fl!("settings-appearance"),
            fl!("settings-appearance-description"),
        ))
        .add(grid_item)
        .into()
}

/// Tag editor section: AcoustID application key, the user's own AcoustID
/// key (keyring-only; `saved` is `None` while the keyring is being read),
/// and the Cover Art Archive toggle.
fn tagger_section<'a>(
    acoustid_api_key: &'a str,
    user_key_input: &'a str,
    user_key_saved: Option<bool>,
    use_caa_cover: bool,
) -> cosmic::Element<'a, SettingsMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let wrap = cosmic::iced::core::text::Wrapping::Word;

    let app_key_item = widget::Column::new()
        .push(widget::text::body(fl!("settings-tagger-app-key")))
        .push(
            widget::text::caption(fl!("settings-tagger-app-key-description"))
                .class(dim_text())
                .wrapping(wrap),
        )
        .push(
            widget::text_input(fl!("settings-tagger-app-key-placeholder"), acoustid_api_key)
                .on_input(SettingsMessage::SetAcoustidApiKey)
                .width(Length::Fill),
        )
        .push(
            widget::button::link(fl!("settings-tagger-app-key-get"))
                .on_press(SettingsMessage::OpenUrl(
                    aulos_core::tagger::ACOUSTID_NEW_APPLICATION_URL.to_string(),
                ))
                .padding(0),
        )
        .spacing(sp.space_xxs)
        .width(Length::Fill);

    let state = match user_key_saved {
        None => fl!("settings-tagger-user-key-checking"),
        Some(true) => fl!("settings-tagger-user-key-saved"),
        Some(false) => fl!("settings-tagger-user-key-none"),
    };
    let user_key_item = widget::Column::new()
        .push(widget::text::body(fl!("settings-tagger-user-key")))
        .push(
            widget::text::caption(fl!("settings-tagger-user-key-description"))
                .class(dim_text())
                .wrapping(wrap),
        )
        .push(
            widget::Row::new()
                .push(
                    widget::text_input(fl!("settings-tagger-user-key-placeholder"), user_key_input)
                        .on_input(SettingsMessage::EditAcoustidUserKey)
                        .on_submit(|_| SettingsMessage::SaveAcoustidUserKey)
                        .password()
                        .width(Length::Fill),
                )
                .push(
                    widget::button::standard(fl!("settings-tagger-user-key-save"))
                        .on_press(SettingsMessage::SaveAcoustidUserKey),
                )
                .spacing(sp.space_xs)
                .align_y(Alignment::Center),
        )
        .push(widget::text::caption(state).class(dim_text()))
        .push(
            widget::button::link(fl!("settings-tagger-user-key-find"))
                .on_press(SettingsMessage::OpenUrl(
                    aulos_core::tagger::ACOUSTID_USER_KEY_URL.to_string(),
                ))
                .padding(0),
        )
        .spacing(sp.space_xxs)
        .width(Length::Fill);

    let caa_item = widget::settings::item::builder(fl!("settings-tagger-caa-cover"))
        .description(fl!("settings-tagger-caa-cover-description"))
        .toggler(use_caa_cover, SettingsMessage::SetTaggerUseCaaCover);

    widget::settings::section()
        .header(section_header(
            fl!("settings-tagger"),
            fl!("settings-tagger-description"),
        ))
        .add(app_key_item)
        .add(user_key_item)
        .add(caa_item)
        .into()
}

/// Experimental section: opt-in toggle for the local file
/// converter/transcoder/ripper page, disabled by default (see
/// `aulos_core::config::Config::experimental_converter`). Toggling it
/// live-adds/removes the Convert nav entry — see
/// `crate::app::AppModel::set_convert_nav_entry`.
fn experimental_section<'a>(experimental_converter: bool) -> cosmic::Element<'a, SettingsMessage> {
    let item = widget::settings::item::builder(fl!("experimental-converter"))
        .description(fl!("experimental-converter-description"))
        .toggler(
            experimental_converter,
            SettingsMessage::SetExperimentalConverter,
        );

    widget::settings::section()
        .title(fl!("settings-experimental"))
        .add(item)
        .into()
}

/// Artist info section: opt-in toggle for fetching artist images and
/// biography text from online sources when browsing in Local or MPD mode
/// (see `aulos_core::config::Config::fetch_artist_info`). Ignored in Subsonic
/// mode, which always shows the server's own artist info instead.
fn artist_info_section<'a>(fetch_artist_info: bool) -> cosmic::Element<'a, SettingsMessage> {
    let item = widget::settings::item::builder(fl!("fetch-artist-info"))
        .description(fl!("fetch-artist-info-description"))
        .toggler(fetch_artist_info, SettingsMessage::SetFetchArtistInfo);

    widget::settings::section()
        .title(fl!("settings-artist-info"))
        .add(item)
        .into()
}

/// Shortcuts section: whole-row links into the Equalizer/Providers drawers.
fn shortcuts_section<'a>() -> cosmic::Element<'a, SettingsMessage> {
    widget::settings::section()
        .title(fl!("settings-shortcuts"))
        .add(drawer_link_row(
            fl!("equalizer"),
            fl!("settings-equalizer-description"),
            SettingsMessage::OpenEqualizer,
        ))
        .add(drawer_link_row(
            fl!("providers"),
            fl!("settings-providers-description"),
            SettingsMessage::OpenProviders,
        ))
        .into()
}

/// About section: whole-row link into the About dialog.
fn about_section<'a>() -> cosmic::Element<'a, SettingsMessage> {
    widget::settings::section()
        .title(fl!("settings-about"))
        .add(drawer_link_row(
            fl!("about"),
            fl!("settings-about-description"),
            SettingsMessage::OpenAbout,
        ))
        .into()
}

/// A settings row that is entirely clickable, opening a drawer/dialog via
/// `message`, with a description and a trailing chevron hinting at the
/// navigation.
fn drawer_link_row<'a>(
    title: String,
    description: String,
    message: SettingsMessage,
) -> widget::list::ListButton<'a, SettingsMessage> {
    widget::list::button(
        widget::settings::item::builder(title)
            .description(description)
            .control(widget::icon::from_name("go-next-symbolic")),
    )
    .on_press(message)
}
