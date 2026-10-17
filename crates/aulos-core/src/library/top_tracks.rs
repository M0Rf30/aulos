// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Ranking of an artist's "Top tracks": most played first, then by rating
//! and favourites, then in library order.

use std::collections::HashMap;
use std::sync::Arc;

use super::Track;

/// How many tracks the artist page's "Top tracks" block shows.
pub const TOP_TRACKS_LEN: usize = 5;

/// An artist's ranked top tracks, as loaded for the artist page.
#[derive(Debug, Clone)]
pub struct ArtistTop {
    pub artist: String,
    pub tracks: Vec<Track>,
}

/// The `limit` best of `tracks`: most recorded plays (`plays`, keyed by
/// track id) first; ties and unplayed tracks fall back to the rating, then
/// favourites, then the given order (the sort is stable).
#[must_use]
pub fn rank_top_tracks(
    tracks: &[Arc<Track>],
    plays: &HashMap<i64, u32>,
    limit: usize,
) -> Vec<Track> {
    let mut ranked: Vec<&Track> = tracks.iter().map(Arc::as_ref).collect();
    ranked.sort_by(|a, b| {
        let played = |t: &Track| plays.get(&t.id).copied().unwrap_or(0);
        played(b)
            .cmp(&played(a))
            .then(b.rating.unwrap_or(0).cmp(&a.rating.unwrap_or(0)))
            .then(b.is_favorite.cmp(&a.is_favorite))
    });
    ranked.into_iter().take(limit).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: i64, rating: Option<u8>, favorite: bool) -> Track {
        Track {
            id,
            path: std::path::PathBuf::new(),
            title: format!("t{id}"),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 0,
            duration: std::time::Duration::ZERO,
            bitrate: 0,
            sample_rate: 0,
            bit_depth: 0,
            provider_id: std::sync::Arc::from("local"),
            source_uri: String::new(),
            is_favorite: favorite,
            rating,
            rg_track_gain: None,
            rg_album_gain: None,
            release_type: String::new(),
            suffix: String::new(),
            added_at: 0,
            favorited_at: 0,
        }
    }

    fn ids(tracks: &[Track]) -> Vec<i64> {
        tracks.iter().map(|t| t.id).collect()
    }

    #[test]
    fn plays_beat_rating_and_ties_fall_back_to_rating_then_favourite() {
        let tracks = vec![
            Arc::new(track(1, Some(5), false)),
            Arc::new(track(2, None, false)),
            Arc::new(track(3, Some(3), true)),
            Arc::new(track(4, None, true)),
            Arc::new(track(5, Some(3), false)),
        ];
        let plays = HashMap::from([(2, 9), (4, 9), (5, 1)]);
        // 2 and 4 tie on plays (4 is a favourite but 2 is first: same
        // rating None, favourite wins), then 5 (1 play), then unplayed by
        // rating: 1 (5 stars), 3 (3 stars).
        assert_eq!(ids(&rank_top_tracks(&tracks, &plays, 10)), [4, 2, 5, 1, 3]);
        assert_eq!(ids(&rank_top_tracks(&tracks, &plays, 2)), [4, 2]);
    }

    #[test]
    fn without_any_plays_rating_and_favourites_rank() {
        let tracks = vec![
            Arc::new(track(1, None, false)),
            Arc::new(track(2, Some(4), false)),
            Arc::new(track(3, None, true)),
        ];
        assert_eq!(
            ids(&rank_top_tracks(&tracks, &HashMap::new(), 5)),
            [2, 3, 1]
        );
    }
}
