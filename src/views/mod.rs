// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

pub mod album_filters;
pub mod albums;
pub mod artists;
pub mod common;
pub mod convert;
pub mod equalizer;
pub mod folders;
pub mod genres;
pub mod home;
pub mod lyrics;
pub mod lyrics_follow;
pub mod mini_player;
pub mod now_playing;
pub mod playlists;
pub mod podcasts;
pub mod providers;
pub mod queue;
pub mod radio;
pub mod settings;
pub mod smart_playlists;
pub mod songs;
pub mod track_row;

use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::core::Background;
use cosmic::widget::button::Style as ButtonStyle;

/// A cross-view navigation target. Any view can emit one (e.g. clicking an
/// artist name inside an album) and the app resolves it to the right page
/// and detail selection, remembering where the user came from so "Back"
/// returns there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Artist(String),
    Album { artist: String, album: String },
    Genre(String),
}

impl Route {
    /// The album page a track belongs to (keyed by album artist, falling
    /// back to the track artist, as albums are grouped).
    pub fn album_of(track: &crate::library::Track) -> Self {
        let artist = if track.album_artist.is_empty() {
            &track.artist
        } else {
            &track.album_artist
        };
        Route::Album {
            artist: artist.clone(),
            album: track.album.clone(),
        }
    }
}

pub fn card_button_class() -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| {
            let cosmic = theme.cosmic();
            ButtonStyle {
                background: None,
                text_color: Some(cosmic.background(false).component.on.into()),
                icon_color: Some(cosmic.background(false).component.on.into()),
                border_radius: cosmic.corner_radii.radius_m.into(),
                ..ButtonStyle::new()
            }
        }),
        hovered: Box::new(|_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.background(false).component;
            ButtonStyle {
                background: Some(Background::Color(comp.hover.into())),
                text_color: Some(comp.on.into()),
                icon_color: Some(comp.on.into()),
                border_radius: cosmic.corner_radii.radius_m.into(),
                ..ButtonStyle::new()
            }
        }),
        pressed: Box::new(|_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.background(false).component;
            ButtonStyle {
                background: Some(Background::Color(comp.pressed.into())),
                text_color: Some(comp.on.into()),
                icon_color: Some(comp.on.into()),
                border_radius: cosmic.corner_radii.radius_m.into(),
                ..ButtonStyle::new()
            }
        }),
        disabled: Box::new(|theme| {
            let cosmic = theme.cosmic();
            ButtonStyle {
                background: None,
                text_color: Some(cosmic.background(false).component.on_disabled.into()),
                icon_color: Some(cosmic.background(false).component.on_disabled.into()),
                border_radius: cosmic.corner_radii.radius_m.into(),
                ..ButtonStyle::new()
            }
        }),
    }
}

/// Button class for interactive list rows across library views.
///
/// Replaces `cosmic::theme::Button::Text` on list rows: unselected rows keep
/// the default on-surface text/icon color (not accent blue) so ordinary rows
/// read as plain text, while the row that is actually playing gets an
/// accent-tinted label on a subtle accent-alpha background — unmistakable in
/// both light and dark themes.
pub fn list_row_button_class(selected: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.12).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: None,
                    text_color: Some(cosmic.background(false).component.on.into()),
                    icon_color: Some(cosmic.background(false).component.on.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        hovered: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.background(false).component;
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.18).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: Some(Background::Color(comp.hover.into())),
                    text_color: Some(comp.on.into()),
                    icon_color: Some(comp.on.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        pressed: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            let comp = &cosmic.background(false).component;
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.24).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: Some(Background::Color(comp.pressed.into())),
                    text_color: Some(comp.on.into()),
                    icon_color: Some(comp.on.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        disabled: Box::new(|theme| {
            let cosmic = theme.cosmic();
            ButtonStyle {
                background: None,
                text_color: Some(cosmic.background(false).component.on_disabled.into()),
                icon_color: Some(cosmic.background(false).component.on_disabled.into()),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
    }
}
