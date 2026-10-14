// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Playback controls: a persistent bottom bar and expandable now-playing view.
//!
//! The bottom bar contains: cover art, track info, transport controls, seek bar,
//! volume slider, and utility buttons. Clicking the bar expands into a full
//! now-playing view with large cover art, metadata, and optional visualizer.

pub mod compact_bar;
pub mod expanded_view;
#[cfg(feature = "visualizer")]
pub mod preset_browser;
pub mod seek_bar;
pub mod sheet;
#[cfg(feature = "visualizer")]
pub mod visualizer;
#[cfg(feature = "visualizer")]
pub mod viz_shader;

use aulos_core::library::Track;

/// Messages from the now-playing controls.
#[derive(Debug, Clone)]
pub enum NowPlayingMessage {
    TogglePlayback,
    Next,
    Previous,
    /// Continuous update during slider drag (visual feedback only).
    SeekPreview(f32),
    /// Emitted on mouse release — performs the actual backend seek.
    SeekCommit,
    SetVolume(f32),
    /// Emitted on mouse release / discrete step — persists the level.
    VolumeCommit,
    ToggleShuffle,
    CycleRepeat,
    /// Stop playback entirely (transport button / `Stop` shortcut). Maps
    /// to `Message::Stop`, which keeps the queue for a later resume.
    Stop,
    /// Toggle the "Up Next" queue drawer (`ContextPage::Queue`).
    ToggleQueue,
    /// Toggle "stop after this track" (`PlaybackExtrasMessage::ToggleStopAfter`).
    ToggleStopAfter,
    ShowLyrics,
    /// Open the Properties drawer for the playing track
    /// (`Message::ShowTrackProperties`). Opens a drawer only: it never
    /// touches the expand/collapse sheet or its animation.
    ShowProperties,
    /// Click on bar background — expand to full view.
    ExpandToggle,
    /// Collapse button or Escape — return to compact bar.
    Collapse,
    /// Toggle favorite for the currently playing track (track ID as string).
    ToggleFavorite(String),
    /// Toggle the lactea visualizer on/off.
    #[cfg(feature = "visualizer")]
    ToggleVisualizer,
    /// Cycle to the next visualizer preset.
    #[cfg(feature = "visualizer")]
    NextPreset,
    /// Double-click on visualizer background — toggle fullscreen.
    #[cfg(feature = "visualizer")]
    ToggleVizFullscreen,
    /// Cursor entered the fullscreen HUD control card — keep it visible
    /// regardless of mouse-idle time while the pointer is over it.
    #[cfg(feature = "visualizer")]
    VizHudPointerEnter,
    /// Cursor left the fullscreen HUD control card — auto-hide idle
    /// counting resumes.
    #[cfg(feature = "visualizer")]
    VizHudPointerExit,
    /// Toggle the preset browser overlay on/off.
    #[cfg(feature = "visualizer")]
    TogglePresetBrowser,
    /// Preset browser search query changed.
    #[cfg(feature = "visualizer")]
    PresetSearchInput(String),
    /// Load a specific preset file (browser row click), outside the
    /// shuffled rotation, with a smooth transition.
    #[cfg(feature = "visualizer")]
    LoadVizPreset(std::path::PathBuf),
    /// The preset browser list scrolled to this vertical offset (px).
    #[cfg(feature = "visualizer")]
    PresetListScrolled(f32),
    /// Lock/unlock automatic preset transitions.
    #[cfg(feature = "visualizer")]
    SetVizLocked(bool),
    /// Adjust beat-reactivity sensitivity.
    #[cfg(feature = "visualizer")]
    SetVizBeatSensitivity(f32),
    /// Jump to the current track's artist or album page.
    Navigate(crate::views::Route),
}

/// "Artist — Album [— year]" line for the playing track, where the artist
/// and album are links to their library pages. `small` uses caption-size
/// text (compact bar); `backdrop` draws in that fixed colour instead of
/// theme colours (expanded view over blurred art). Streams and podcasts,
/// which have no library pages, render as plain text.
pub fn artist_album_line<'a>(
    track: &'a Track,
    small: bool,
    backdrop: Option<cosmic::iced::Color>,
    separator: &'static str,
) -> cosmic::Element<'a, NowPlayingMessage> {
    use crate::views::{Route, common};
    use cosmic::iced::core::text::Wrapping;
    use cosmic::widget;

    let text = move |s: String| -> common::Text<'a> {
        let t = if small {
            widget::text::caption(s)
        } else {
            widget::text::body(s)
        }
        .wrapping(Wrapping::None);
        match backdrop {
            Some(color) => t.class(cosmic::theme::Text::Color(color)),
            None => t,
        }
    };
    let linkable = !track.is_live_stream() && &*track.provider_id != "podcast";
    let part = move |label: &str, route: Route| -> cosmic::Element<'a, NowPlayingMessage> {
        let label = label.to_string();
        if !linkable {
            return text(label).into();
        }
        let msg = NowPlayingMessage::Navigate(route);
        let plain = if small {
            widget::text::caption(label)
        } else {
            widget::text::body(label)
        }
        .wrapping(Wrapping::None);
        match backdrop {
            Some(color) => common::link_on(plain, color, msg),
            None => common::link(plain, true, msg),
        }
    };

    let mut parts: Vec<cosmic::Element<'a, NowPlayingMessage>> = Vec::new();
    if !track.artist.is_empty() {
        parts.push(part(&track.artist, Route::Artist(track.artist.clone())));
    }
    if !track.album.is_empty() {
        parts.push(part(&track.album, Route::album_of(track)));
    }
    if track.year > 0 && !small {
        parts.push(text(track.year.to_string()).into());
    }

    let mut row = widget::Row::new().align_y(cosmic::iced::Alignment::Center);
    for (i, element) in parts.into_iter().enumerate() {
        if i > 0 {
            row = row.push(text(separator.to_string()));
        }
        row = row.push(element);
    }
    row.into()
}
