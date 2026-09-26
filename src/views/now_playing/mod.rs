// SPDX-License-Identifier: GPL-3.0

//! Playback controls: a persistent bottom bar and expandable now-playing view.
//!
//! The bottom bar contains: cover art, track info, transport controls, seek bar,
//! volume slider, and utility buttons. Clicking the bar expands into a full
//! now-playing view with large cover art, metadata, and optional visualizer.

pub mod animation;
pub mod blur;
pub mod compact_bar;
pub mod expanded_view;
#[cfg(feature = "visualizer")]
pub mod preset_browser;
#[cfg(feature = "visualizer")]
pub mod visualizer;
#[cfg(feature = "visualizer")]
pub mod viz_shader;

use crate::library::Track;
use std::time::Duration;

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
    ShowLyrics,
    /// Click on bar background — expand to full view.
    ExpandToggle,
    /// Collapse button or Escape — return to compact bar.
    Collapse,
    /// Toggle favorite for the currently playing track (track ID as string).
    ToggleFavorite(String),
    /// Toggle the ProjectM visualizer on/off.
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
    /// Load a specific preset file (browser row click), bypassing the
    /// playlist, with a smooth transition.
    #[cfg(feature = "visualizer")]
    LoadVizPreset(std::path::PathBuf),
    /// Lock/unlock automatic preset transitions.
    #[cfg(feature = "visualizer")]
    SetVizLocked(bool),
    /// Adjust beat-reactivity sensitivity.
    #[cfg(feature = "visualizer")]
    SetVizBeatSensitivity(f32),
}

/// Format a duration as `H:MM:SS` / `M:SS`.
pub fn format_time(d: Duration) -> String {
    super::common::format_duration(d.as_secs())
}

/// Truncate a string to `max_chars`, appending `…` if it exceeds the limit.
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    super::common::truncate_str(s, max_chars)
}

/// A track that should be presented as a live stream rather than a normal,
/// seekable recording: radio (`provider_id == "radio"`) or anything else
/// reporting zero duration. Shared by the compact bar and expanded view so
/// both hide/disable the same controls (seek slider, shuffle, prev/next,
/// repeat) for the same tracks.
pub fn is_live_stream(track: &Track) -> bool {
    &*track.provider_id == "radio" || track.duration.is_zero()
}

#[cfg(test)]
mod tests {
    use super::is_live_stream;
    use crate::library::Track;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn track(provider_id: &str, duration_secs: u64) -> Track {
        Track {
            id: 1,
            path: PathBuf::new(),
            title: String::new(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 0,
            duration: Duration::from_secs(duration_secs),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from(provider_id),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    #[test]
    fn radio_provider_is_live_even_with_a_nonzero_duration() {
        assert!(is_live_stream(&track("radio", 180)));
    }

    #[test]
    fn zero_duration_track_is_live_regardless_of_provider() {
        assert!(is_live_stream(&track("local", 0)));
    }

    #[test]
    fn normal_local_track_is_not_live() {
        assert!(!is_live_stream(&track("local", 180)));
    }
}
