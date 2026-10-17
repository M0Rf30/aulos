// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Filter state of the Albums page: release kind (Albums / EPs / Singles /
//! Compilations / Live), release decade, genre and favourites. The state is
//! small and the predicate pure; the derived data (visible album positions,
//! decades and genres present) is memoized in [`AlbumFilterCache`] and
//! recomputed only when the albums or the chip selection change, not on
//! every UI frame.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::Album;
use super::compilations::decade_of;
use super::release_kind::ReleaseKind;
use std::sync::Arc;

/// Current chip selection. The default selects everything.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AlbumFilter {
    /// Only releases of this kind; `None` lists every kind.
    pub kind: Option<ReleaseKind>,
    /// `Some(1990)` selects the 1990s.
    pub decade: Option<u32>,
    /// Only albums with this genre among their tracks (case-insensitive).
    pub genre: Option<String>,
    /// Only albums with at least one favourite track.
    pub favourites: bool,
}

impl AlbumFilter {
    /// Whether any chip narrows the album list.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.kind.is_some() || self.decade.is_some() || self.genre.is_some() || self.favourites
    }

    /// Selects which kind of release to list; `None` lists every kind.
    pub fn set_kind(&mut self, kind: Option<ReleaseKind>) {
        self.kind = kind;
    }

    /// Selects a decade (`Some(1990)` for the 1990s); `None` clears it.
    pub fn set_decade(&mut self, decade: Option<u32>) {
        self.decade = decade;
    }

    /// Selects a genre; `None` clears it.
    pub fn set_genre(&mut self, genre: Option<String>) {
        self.genre = genre;
    }

    /// Restrict the list to albums with favourite tracks, or not.
    pub fn set_favourites(&mut self, favourites: bool) {
        self.favourites = favourites;
    }

    /// Whether `album` passes every active chip.
    #[must_use]
    pub fn matches(&self, album: &Album) -> bool {
        self.kind.is_none_or(|k| album.kind == k)
            && self.decade.is_none_or(|d| decade_of(album.year) == Some(d))
            && (!self.favourites || album.tracks.iter().any(|t| t.is_favorite))
            && self.genre.as_deref().is_none_or(|g| {
                album
                    .genres()
                    .iter()
                    .any(|have| have.eq_ignore_ascii_case(g))
            })
    }

    /// Indices into `albums` of the albums passing the filter, in order.
    #[must_use]
    pub fn visible_indices(&self, albums: &[Arc<Album>]) -> Vec<usize> {
        albums
            .iter()
            .enumerate()
            .filter(|(_, a)| self.matches(a))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Distinct decades (ascending) present among `albums`.
#[must_use]
pub fn decades_present(albums: &[Arc<Album>]) -> Vec<u32> {
    let mut decades: Vec<u32> = albums.iter().filter_map(|a| decade_of(a.year)).collect();
    decades.sort_unstable();
    decades.dedup();
    decades
}

/// Distinct genres (sorted, case-insensitively unique) present among
/// `albums`.
#[must_use]
pub fn genres_present(albums: &[Arc<Album>]) -> Vec<String> {
    let mut genres: Vec<String> = albums.iter().flat_map(|a| a.genres()).collect();
    genres.sort_by_key(|g| g.to_lowercase());
    genres.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    genres
}

/// `(visible positions, decades present, genres present)`.
pub type FilterView = (Rc<Vec<usize>>, Rc<Vec<u32>>, Rc<Vec<String>>);

/// Memoized derived data of the Albums page: positions of the albums that
/// pass the chip filters, and the decades and genres present.
///
/// UI `view()` functions only have `&self`, hence the interior mutability.
/// The entry is recomputed when the caller's `epoch` (bumped whenever the
/// albums slice handed to the view may have changed), the filter or the
/// slice length differ from the cached ones, or after
/// [`AlbumFilterCache::invalidate`].
#[derive(Debug, Default)]
pub struct AlbumFilterCache {
    stale: Cell<bool>,
    inner: RefCell<CacheEntry>,
}

#[derive(Debug, Default)]
struct CacheEntry {
    key: Option<(u64, AlbumFilter, usize)>,
    visible: Rc<Vec<usize>>,
    decades: Rc<Vec<u32>>,
    genres: Rc<Vec<String>>,
    /// Number of recomputations (for tests).
    computed: usize,
}

impl AlbumFilterCache {
    /// Force the next [`AlbumFilterCache::get`] to recompute.
    pub fn invalidate(&self) {
        self.stale.set(true);
    }

    /// `(visible positions, decades present, genres present)` for `albums`
    /// under `filter`.
    pub fn get(&self, epoch: u64, filter: &AlbumFilter, albums: &[Arc<Album>]) -> FilterView {
        let key = (epoch, filter.clone(), albums.len());
        let mut entry = self.inner.borrow_mut();
        if self.stale.replace(false) || entry.key.as_ref() != Some(&key) {
            entry.visible = Rc::new(filter.visible_indices(albums));
            entry.decades = Rc::new(decades_present(albums));
            entry.genres = Rc::new(genres_present(albums));
            entry.key = Some(key);
            entry.computed += 1;
        }
        (
            Rc::clone(&entry.visible),
            Rc::clone(&entry.decades),
            Rc::clone(&entry.genres),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(artist: &str, year: u32) -> Arc<Album> {
        Arc::new(Album::new(
            "A".into(),
            artist.into(),
            year,
            Vec::new(),
            None,
        ))
    }

    /// An album of `n` ten-minute tracks tagged `release_type` and `genre`.
    fn tagged(name: &str, release_type: &str, genre: &str, favourite: bool) -> Arc<Album> {
        let tracks = (1..=12)
            .map(|i| crate::library::Track {
                id: i,
                path: std::path::PathBuf::new(),
                title: String::new(),
                artist: "Band".into(),
                album_artist: "Band".into(),
                album: name.into(),
                genre: genre.into(),
                track_number: i as u32,
                disc_number: 1,
                year: 2001,
                duration: std::time::Duration::from_secs(300),
                bitrate: 0,
                sample_rate: 0,
                bit_depth: 0,
                provider_id: std::sync::Arc::from("local"),
                source_uri: String::new(),
                is_favorite: favourite && i == 1,
                rating: None,
                rg_track_gain: None,
                rg_album_gain: None,
                release_type: release_type.into(),
                suffix: String::new(),
                added_at: 0,
                favorited_at: 0,
            })
            .collect();
        Arc::new(Album::new(name.into(), "Band".into(), 2001, tracks, None))
    }

    #[test]
    fn kind_filter() {
        let albums = vec![
            tagged("Studio", "album", "Rock", false),
            tagged("Gig", "album; live", "Rock", false),
            tagged("Mini", "ep", "Rock", false),
            tagged("Hit", "single", "Rock", false),
            album("Various Artists", 1999),
        ];
        let mut f = AlbumFilter::default();
        assert!(!f.is_active());
        assert_eq!(f.visible_indices(&albums), vec![0, 1, 2, 3, 4]);
        f.set_kind(Some(ReleaseKind::Compilation));
        assert!(f.is_active());
        assert_eq!(f.visible_indices(&albums), vec![4]);
        f.set_kind(Some(ReleaseKind::Album));
        assert_eq!(f.visible_indices(&albums), vec![0]);
        f.set_kind(Some(ReleaseKind::Live));
        assert_eq!(f.visible_indices(&albums), vec![1]);
        f.set_kind(Some(ReleaseKind::Ep));
        assert_eq!(f.visible_indices(&albums), vec![2]);
        f.set_kind(Some(ReleaseKind::Single));
        assert_eq!(f.visible_indices(&albums), vec![3]);
    }

    #[test]
    fn genre_and_favourite_filters_combine() {
        let albums = vec![
            tagged("A", "album", "Rock; Pop", true),
            tagged("B", "album", "Jazz", true),
            tagged("C", "album", "rock", false),
        ];
        let mut f = AlbumFilter::default();
        f.set_genre(Some("Rock".into()));
        assert_eq!(f.visible_indices(&albums), vec![0, 2]);
        f.set_favourites(true);
        assert_eq!(f.visible_indices(&albums), vec![0]);
        f.set_genre(None);
        assert_eq!(f.visible_indices(&albums), vec![0, 1]);
        assert_eq!(genres_present(&albums), vec!["Jazz", "Pop", "Rock"]);
    }

    #[test]
    fn decade_filter_and_listing() {
        let albums = vec![
            album("A", 1994),
            album("B", 1989),
            album("C", 0),
            album("D", 1991),
        ];
        assert_eq!(decades_present(&albums), vec![1980, 1990]);
        let mut f = AlbumFilter::default();
        f.set_decade(Some(1990));
        assert!(f.is_active());
        assert_eq!(f.visible_indices(&albums), vec![0, 3]);
        f.set_decade(None);
        assert_eq!(f.visible_indices(&albums).len(), 4);
    }

    #[test]
    fn cache_recomputes_only_on_epoch_filter_len_or_invalidate() {
        let albums = vec![album("A", 1994), album("B", 1989)];
        let cache = AlbumFilterCache::default();
        let mut f = AlbumFilter::default();

        let (visible, decades, _) = cache.get(1, &f, &albums);
        assert_eq!(*visible, vec![0, 1]);
        assert_eq!(*decades, vec![1980, 1990]);
        assert_eq!(cache.inner.borrow().computed, 1);

        // Same inputs: served from the cache.
        cache.get(1, &f, &albums);
        assert_eq!(cache.inner.borrow().computed, 1);

        // Filter change.
        f.set_decade(Some(1990));
        let (visible, _, _) = cache.get(1, &f, &albums);
        assert_eq!(*visible, vec![0]);
        assert_eq!(cache.inner.borrow().computed, 2);

        // New epoch (library/search changed).
        cache.get(2, &f, &albums);
        assert_eq!(cache.inner.borrow().computed, 3);

        // Different slice length.
        cache.get(2, &f, &albums[..1]);
        assert_eq!(cache.inner.borrow().computed, 4);

        // Explicit invalidation.
        cache.invalidate();
        cache.get(2, &f, &albums[..1]);
        assert_eq!(cache.inner.borrow().computed, 5);
        cache.get(2, &f, &albums[..1]);
        assert_eq!(cache.inner.borrow().computed, 5);
    }
}
