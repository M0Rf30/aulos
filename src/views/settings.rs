// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Settings view — library folders, playback defaults, and quick links to
//! the Equalizer/Providers drawers and the About dialog.

use crate::config::ReplayGainMode;
use crate::fl;
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
}

/// All replay gain modes, in the order shown in the dropdown.
const REPLAY_GAIN_MODES: [ReplayGainMode; 4] = [
    ReplayGainMode::Off,
    ReplayGainMode::Track,
    ReplayGainMode::Album,
    ReplayGainMode::Auto,
];

/// Render the Settings page.
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
) -> cosmic::Element<'a, SettingsMessage> {
    let col = widget::Column::new()
        .spacing(24)
        .push(library_section(
            music_dirs,
            split_artist_tags,
            artist_tag_delimiters_input,
        ))
        .push(playback_section(crossfade_secs, replay_gain_mode, volume))
        .push(artist_info_section(fetch_artist_info))
        .push(experimental_section(experimental_converter))
        .push(shortcuts_section())
        .push(about_section());

    widget::scrollable(widget::container(col).width(Length::Fill).padding(24))
        .height(Length::Fill)
        .into()
}

/// Library section: configured music directories, with add/remove, plus
/// the multi-artist-tag-splitting toggle and delimiter editor.
fn library_section<'a>(
    music_dirs: &'a [PathBuf],
    split_artist_tags: bool,
    artist_tag_delimiters_input: &'a str,
) -> cosmic::Element<'a, SettingsMessage> {
    let mut section = widget::settings::section().title(fl!("settings-library"));

    if music_dirs.is_empty() {
        section = section.add(widget::text::body(fl!("no-music-dirs")));
    } else {
        for (i, dir) in music_dirs.iter().enumerate() {
            section = section.add(widget::settings::item(
                dir.to_string_lossy(),
                widget::button::destructive(fl!("remove"))
                    .on_press(SettingsMessage::RemoveMusicDir(i)),
            ));
        }
    }

    section = section
        .add(widget::button::text(fl!("add-music-folder")).on_press(SettingsMessage::AddMusicDir));

    let split_item = widget::settings::item::builder(fl!("split-artist-tags"))
        .description(fl!("split-artist-tags-description"))
        .control(widget::toggler(split_artist_tags).on_toggle(SettingsMessage::SetSplitArtistTags));
    section = section.add(split_item);

    let delimiters_row = widget::Row::new()
        .push(
            widget::text_input(
                fl!("artist-tag-delimiters-placeholder"),
                artist_tag_delimiters_input,
            )
            .on_input(SettingsMessage::EditArtistTagDelimiters)
            .on_submit_maybe(Some(SettingsMessage::SubmitArtistTagDelimiters)),
        )
        .push(
            widget::button::text(fl!("reset-to-defaults"))
                .on_press(SettingsMessage::ResetArtistTagDelimiters),
        )
        .spacing(8)
        .align_y(Alignment::Center);

    let delimiters_item = widget::settings::item::builder(fl!("artist-tag-delimiters"))
        .description(fl!("artist-tag-delimiters-description"))
        .control(delimiters_row);
    section = section.add(delimiters_item);

    section.into()
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
    let crossfade_item = widget::settings::item::builder(fl!("crossfade-duration"))
        .description(crossfade_label)
        .control(
            widget::slider(0.0..=12.0, crossfade_secs, SettingsMessage::SetCrossfade)
                .step(0.5_f32)
                .width(Length::Fixed(200.0)),
        );

    let replay_gain_labels = vec![
        fl!("replay-gain-off"),
        fl!("replay-gain-track"),
        fl!("replay-gain-album"),
        fl!("replay-gain-auto"),
    ];
    let replay_gain_selected = REPLAY_GAIN_MODES
        .iter()
        .position(|mode| *mode == replay_gain_mode);
    let replay_gain_item = widget::settings::item::builder(fl!("replay-gain")).control(
        widget::dropdown(replay_gain_labels, replay_gain_selected, |i| {
            SettingsMessage::SetReplayGainMode(REPLAY_GAIN_MODES[i])
        }),
    );

    let volume_item = widget::settings::item::builder(fl!("volume")).control(
        widget::slider(0.0..=1.0, volume, SettingsMessage::SetVolume)
            .step(0.01_f32)
            .width(Length::Fixed(200.0)),
    );

    widget::settings::section()
        .title(fl!("settings-playback"))
        .add(crossfade_item)
        .add(replay_gain_item)
        .add(volume_item)
        .into()
}

/// Experimental section: opt-in toggle for the local file
/// converter/transcoder/ripper page, disabled by default (see
/// `crate::config::Config::experimental_converter`). Toggling it
/// live-adds/removes the Convert nav entry — see
/// `crate::app::AppModel::set_convert_nav_entry`.
fn experimental_section<'a>(experimental_converter: bool) -> cosmic::Element<'a, SettingsMessage> {
    let item = widget::settings::item::builder(fl!("experimental-converter"))
        .description(fl!("experimental-converter-description"))
        .control(
            widget::toggler(experimental_converter)
                .on_toggle(SettingsMessage::SetExperimentalConverter),
        );

    widget::settings::section()
        .title(fl!("settings-experimental"))
        .add(item)
        .into()
}

/// Artist info section: opt-in toggle for fetching artist images and
/// biography text from online sources when browsing in Local or MPD mode
/// (see `crate::config::Config::fetch_artist_info`). Ignored in Subsonic
/// mode, which always shows the server's own artist info instead.
fn artist_info_section<'a>(fetch_artist_info: bool) -> cosmic::Element<'a, SettingsMessage> {
    let item = widget::settings::item::builder(fl!("fetch-artist-info"))
        .description(fl!("fetch-artist-info-description"))
        .control(
            widget::toggler(fetch_artist_info).on_toggle(SettingsMessage::SetFetchArtistInfo),
        );

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
            SettingsMessage::OpenEqualizer,
        ))
        .add(drawer_link_row(
            fl!("providers"),
            SettingsMessage::OpenProviders,
        ))
        .into()
}

/// About section: whole-row link into the About dialog.
fn about_section<'a>() -> cosmic::Element<'a, SettingsMessage> {
    widget::settings::section()
        .title(fl!("settings-about"))
        .add(drawer_link_row(fl!("about"), SettingsMessage::OpenAbout))
        .into()
}

/// A settings row that is entirely clickable, opening a drawer/dialog via
/// `message`, with a trailing chevron hinting at the navigation.
fn drawer_link_row<'a>(
    title: impl Into<std::borrow::Cow<'a, str>> + 'a,
    message: SettingsMessage,
) -> widget::list::ListButton<'a, SettingsMessage> {
    widget::list::button(widget::settings::item(
        title,
        widget::icon::from_name("go-next-symbolic"),
    ))
    .on_press(message)
}
