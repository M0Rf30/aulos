// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Two-way sync between Aulos' favorites and Last.fm's loved tracks: the
//! pure matching step that decides what to favorite locally and what to
//! love remotely. Applying the plan (toggling favorites, sending loves) is
//! the caller's job.

use super::source_allowed;
use crate::library::Track;
use std::collections::HashSet;
use std::sync::Arc;

/// Upper bound on favorites pushed to Last.fm by a single sync.
pub const MAX_BULK_LOVES: usize = 500;

/// Case-insensitive `(artist, title)` identity used to match library tracks
/// against Last.fm's loved list.
fn loved_key(artist: &str, title: &str) -> String {
    format!(
        "{}\u{1}{}",
        artist.trim().to_lowercase(),
        title.trim().to_lowercase()
    )
}

/// What a loved-track sync has to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LovedSyncPlan {
    /// Ids of library tracks that are loved remotely but not yet favorites.
    pub to_favorite: Vec<String>,
    /// `(artist, title)` of favorites the remote service lacks, capped at
    /// [`MAX_BULK_LOVES`].
    pub to_love: Vec<(String, String)>,
}

/// Matches `tracks` against the remote `loved` `(artist, title)` list.
/// Tracks whose provider is not scrobble-eligible are ignored.
#[must_use]
pub fn plan_sync(tracks: &[Arc<Track>], loved: &[(String, String)]) -> LovedSyncPlan {
    let remote: HashSet<String> = loved.iter().map(|(a, t)| loved_key(a, t)).collect();
    let mut plan = LovedSyncPlan::default();
    let mut seen: HashSet<String> = HashSet::new();
    for t in tracks {
        if !source_allowed(&t.provider_id, false) {
            continue;
        }
        let key = loved_key(&t.artist, &t.title);
        if remote.contains(&key) {
            if !t.is_favorite {
                plan.to_favorite.push(t.key());
            }
        } else if t.is_favorite
            && plan.to_love.len() < MAX_BULK_LOVES
            && !t.artist.trim().is_empty()
            && seen.insert(key)
        {
            plan.to_love.push((t.artist.clone(), t.title.clone()));
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn track(id: i64, artist: &str, title: &str, favorite: bool) -> Arc<Track> {
        Arc::new(Track {
            id,
            path: PathBuf::new(),
            title: title.to_string(),
            artist: artist.to_string(),
            album_artist: String::new(),
            album: String::new(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 0,
            duration: Duration::from_secs(200),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("local"),
            source_uri: String::new(),
            is_favorite: favorite,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
            bit_depth: 0,
            release_type: String::new(),
            suffix: String::new(),
            added_at: 0,
            favorited_at: 0,
        })
    }

    #[test]
    fn loved_key_ignores_case_and_padding() {
        assert_eq!(
            loved_key(" Björk ", "Hunter"),
            loved_key("björk", " hunter")
        );
        assert_ne!(loved_key("a", "b"), loved_key("b", "a"));
    }

    #[test]
    fn plan_merges_both_directions() {
        let tracks = [
            track(1, "Björk", "Hunter", false),  // loved remotely -> favorite
            track(2, "Boards", "Roygbiv", true), // favorite, not loved -> love
            track(3, "Boards", "Roygbiv", true), // duplicate key -> love once
            track(4, "Air", "Sexy Boy", true),   // loved and already favorite
            track(5, "", "Untitled", true),      // no artist -> never pushed
        ];
        let loved = vec![
            ("björk".to_string(), " hunter".to_string()),
            ("Air".to_string(), "Sexy Boy".to_string()),
        ];
        let plan = plan_sync(&tracks, &loved);
        assert_eq!(plan.to_favorite, vec!["1".to_string()]);
        assert_eq!(
            plan.to_love,
            vec![("Boards".to_string(), "Roygbiv".to_string())]
        );
    }

    #[test]
    fn plan_caps_bulk_loves() {
        let tracks: Vec<Arc<Track>> = (0..(MAX_BULK_LOVES as i64 + 10))
            .map(|i| track(i, "Artist", &format!("Song {i}"), true))
            .collect();
        let plan = plan_sync(&tracks, &[]);
        assert_eq!(plan.to_love.len(), MAX_BULK_LOVES);
    }
}
