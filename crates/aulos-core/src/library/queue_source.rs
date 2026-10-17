// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! "Play next" / "Add to queue" actions that name their tracks by index.
//!
//! Detail views used to clone the whole `Vec<Track>` into every button's
//! message on every `view()`. They now send a [`QueueSource`]; the tracks
//! are resolved here, once, when the button is actually pressed.

use super::state::LibraryState;
use super::{Playlist, Track};
use std::sync::Arc;

/// Which tracks a queue action refers to. `track: None` means every track
/// of the entity, `Some(i)` the single track at index `i` within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueSource {
    /// An album in [`LibraryState::albums`].
    Album { album: usize, track: Option<usize> },
    /// An album of an artist in [`LibraryState::artists`].
    ArtistAlbum {
        artist: usize,
        album: usize,
        track: Option<usize>,
    },
    /// The tracks of the genre currently shown in the genre detail view.
    Genre { track: Option<usize> },
    /// A playlist in the caller's playlist list.
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

/// [`select`] over shared library tracks, cloning only what is selected
/// into owned queue entries.
fn select_shared(items: &[Arc<Track>], index: Option<usize>) -> Vec<Track> {
    match index {
        None => items.iter().map(|t| Track::clone(t)).collect(),
        Some(i) => items.get(i).map(|t| Track::clone(t)).into_iter().collect(),
    }
}

impl QueueSource {
    /// Resolve `self` to the tracks it refers to, against `library` and the
    /// caller's `playlists`.
    #[must_use]
    pub fn resolve(self, library: &LibraryState, playlists: &[Playlist]) -> Vec<Track> {
        match self {
            Self::Album { album, track } => library
                .albums
                .get(album)
                .map(|a| select_shared(&a.tracks, track))
                .unwrap_or_default(),
            Self::ArtistAlbum {
                artist,
                album,
                track,
            } => library
                .artists
                .get(artist)
                .and_then(|a| a.albums.get(album))
                .map(|a| select_shared(&a.tracks, track))
                .unwrap_or_default(),
            Self::Genre { track } => select(&library.genre_tracks, track),
            Self::Playlist { playlist, track } => playlists
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
