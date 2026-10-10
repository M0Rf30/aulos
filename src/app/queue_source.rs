// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! "Play next" / "Add to queue" actions that name their tracks by index.
//!
//! Detail views used to clone the whole `Vec<Track>` into every button's
//! message on every `view()`. They now send a [`QueueSource`]; the tracks
//! are resolved here, once, when the button is actually pressed.

use super::AppModel;
use crate::library::Track;

/// Which tracks a queue action refers to. `track: None` means every track
/// of the entity, `Some(i)` the single track at index `i` within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueSource {
    /// An album in `all_albums`.
    Album { album: usize, track: Option<usize> },
    /// An album of an artist in `all_artists`.
    ArtistAlbum {
        artist: usize,
        album: usize,
        track: Option<usize>,
    },
    /// The tracks of the genre currently shown in the genre detail view.
    Genre { track: Option<usize> },
    /// A playlist in `playlists`.
    Playlist {
        playlist: usize,
        track: Option<usize>,
    },
}

/// All of `items` (`None`) or just the one at the given index; empty when
/// the index is out of range (stale message after the data changed).
fn select<T: Clone>(items: &[T], index: Option<usize>) -> Vec<T> {
    match index {
        None => items.to_vec(),
        Some(i) => items.get(i).cloned().into_iter().collect(),
    }
}

impl AppModel {
    /// Resolve `source` to the tracks it refers to.
    pub(super) fn queue_source_tracks(&self, source: QueueSource) -> Vec<Track> {
        match source {
            QueueSource::Album { album, track } => self
                .all_albums
                .get(album)
                .map(|a| select(&a.tracks, track))
                .unwrap_or_default(),
            QueueSource::ArtistAlbum {
                artist,
                album,
                track,
            } => self
                .all_artists
                .get(artist)
                .and_then(|a| a.albums.get(album))
                .map(|a| select(&a.tracks, track))
                .unwrap_or_default(),
            QueueSource::Genre { track } => select(&self.genre_tracks, track),
            QueueSource::Playlist { playlist, track } => self
                .playlists
                .get(playlist)
                .map(|p| select(&p.tracks, track))
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::select;

    #[test]
    fn select_none_is_everything() {
        assert_eq!(select(&[1, 2, 3], None), vec![1, 2, 3]);
    }

    #[test]
    fn select_some_is_a_single_item() {
        assert_eq!(select(&[1, 2, 3], Some(1)), vec![2]);
    }

    #[test]
    fn select_out_of_range_is_empty() {
        assert!(select(&[1, 2, 3], Some(7)).is_empty());
        assert!(select::<i32>(&[], None).is_empty());
    }
}
