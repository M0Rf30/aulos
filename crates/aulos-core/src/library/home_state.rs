// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! UI-independent state of the Home page: the loaded shelves, the open
//! decade grid, the listening-time tracker and the "Because you listened
//! to …" shelves, plus the small facts and text helpers a Home UI needs.
//! The shelf data itself and the builders live in [`super::history`] and
//! [`super::home`].

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::history::{AlbumRef, HomeData, PlayTracker, ResumePoint};
use super::home::HomeCache;
use crate::fl;
use crate::online::similar::SimilarShelf;

/// The decade grid currently open on the Home page.
#[derive(Debug, Clone)]
pub struct DecadeView {
    pub decade: u32,
    /// `None` while the albums are still loading.
    pub albums: Option<Vec<AlbumRef>>,
}

/// Home page state, owned by the app.
#[derive(Debug, Default)]
pub struct HomeState {
    /// Loaded shelves; `None` until the first load completes.
    pub data: Option<HomeData>,
    /// Bumped on every load so a slow, superseded result is dropped.
    pub request: u64,
    /// The decade grid, when one is open.
    pub decade: Option<DecadeView>,
    /// Listening-time accumulator for the current track.
    pub tracker: PlayTracker,
    /// Track id `tracker` is currently following.
    pub tracked_track: Option<i64>,
    /// "Because you listened to …" shelves, loaded after the rest.
    pub similar: Vec<SimilarShelf>,
    /// The Home load `similar` belongs to.
    pub similar_request: u64,
    /// Stops the in-flight similar-artist lookup when a new one starts.
    pub similar_cancel: Arc<AtomicBool>,
    /// What the loaded shelves are valid for (see [`HomeCache`]).
    pub cache: HomeCache,
}

/// Cheap, per-frame facts about the library the Home view needs.
#[derive(Debug, Clone, Copy)]
pub struct HomeContext {
    /// Albums in the active provider's library.
    pub library_albums: usize,
    /// Whether a library scan/load is running.
    pub scanning: bool,
    /// Whether any music folder is configured (marks step 1 done).
    pub has_music_dirs: bool,
    /// Whether any MPD/Subsonic server is configured (marks step 2 done).
    pub has_servers: bool,
    /// Scroll offset of the decade grid (virtualization).
    pub decade_scroll: f32,
}

/// Which greeting suits an hour of the day (0–23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Greeting {
    Morning,
    Afternoon,
    Evening,
}

impl Greeting {
    /// The greeting for a local hour of the day (0–23).
    #[must_use]
    pub fn for_hour(hour: u32) -> Self {
        match hour {
            5..=11 => Self::Morning,
            12..=17 => Self::Afternoon,
            _ => Self::Evening,
        }
    }

    /// The localized greeting text.
    #[must_use]
    pub fn text(self) -> String {
        match self {
            Self::Morning => fl!("home-greeting-morning"),
            Self::Afternoon => fl!("home-greeting-afternoon"),
            Self::Evening => fl!("home-greeting-evening"),
        }
    }
}

impl Greeting {
    /// The greeting addressed to `name` ("Good evening, Sam"), or the bare
    /// [`Self::text`] when there is no name.
    #[must_use]
    pub fn text_for(self, name: &str) -> String {
        let name = name.trim();
        if name.is_empty() {
            self.text()
        } else {
            fl!("home-greeting-named", greeting = self.text(), name = name)
        }
    }
}

impl ResumePoint {
    /// Localized "You were 6 tracks into Low Tide Tapes."
    #[must_use]
    pub fn label(&self) -> String {
        fl!(
            "home-resume",
            count = i64::try_from(self.tracks_in).unwrap_or(i64::MAX),
            album = self.album.as_str()
        )
    }
}

/// Label for a decade chip/title, e.g. `1980` → `"1980s"`.
#[must_use]
pub fn decade_label(decade: u32) -> String {
    format!("{decade}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_follows_the_hour() {
        assert_eq!(Greeting::for_hour(4), Greeting::Evening);
        assert_eq!(Greeting::for_hour(5), Greeting::Morning);
        assert_eq!(Greeting::for_hour(11), Greeting::Morning);
        assert_eq!(Greeting::for_hour(12), Greeting::Afternoon);
        assert_eq!(Greeting::for_hour(17), Greeting::Afternoon);
        assert_eq!(Greeting::for_hour(18), Greeting::Evening);
        assert_eq!(Greeting::for_hour(23), Greeting::Evening);
        assert_eq!(Greeting::for_hour(0), Greeting::Evening);
    }

    #[test]
    fn greeting_names_the_user() {
        assert_eq!(Greeting::Evening.text_for("Sam"), "Good evening, Sam");
        assert_eq!(Greeting::Morning.text_for("  "), "Good morning");
        assert_eq!(Greeting::Afternoon.text_for(""), "Good afternoon");
    }

    #[test]
    fn resume_label_counts_tracks() {
        let at = |n| ResumePoint {
            artist: "Band".into(),
            album: "Low Tide Tapes".into(),
            tracks_in: n,
            total: 10,
        };
        assert_eq!(at(6).label(), "You were 6 tracks into Low Tide Tapes.");
        assert_eq!(at(1).label(), "You were 1 track into Low Tide Tapes.");
    }

    #[test]
    fn decade_labels() {
        assert_eq!(decade_label(1980), "1980s");
        assert_eq!(decade_label(2020), "2020s");
    }
}
