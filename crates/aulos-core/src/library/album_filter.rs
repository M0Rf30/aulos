// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Filter state of the Albums page: kind (All / Albums / Compilations) and
//! release decade. The state is tiny and the predicate pure; the derived
//! data (visible album positions, decades present) is memoized in
//! [`AlbumFilterCache`] and recomputed only when the albums or the chip
//! selection change, not on every UI frame.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::Album;
use super::compilations::decade_of;

/// Which kind of album to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlbumKind {
    #[default]
    All,
    /// Everything that is not a compilation.
    Albums,
    /// "Various Artists" compilations only.
    Compilations,
}

/// Current chip selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlbumFilter {
    pub kind: AlbumKind,
    /// `Some(1990)` selects the 1990s.
    pub decade: Option<u32>,
}

impl AlbumFilter {
    /// Whether any chip narrows the album list.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.kind != AlbumKind::All || self.decade.is_some()
    }

    /// Selects which kind of album to list.
    pub fn set_kind(&mut self, kind: AlbumKind) {
        self.kind = kind;
    }

    /// Selects a decade (`Some(1990)` for the 1990s); `None` clears it.
    pub fn set_decade(&mut self, decade: Option<u32>) {
        self.decade = decade;
    }

    /// Whether `album` passes both the kind and the decade filter.
    #[must_use]
    pub fn matches(&self, album: &Album) -> bool {
        let kind_ok = match self.kind {
            AlbumKind::All => true,
            AlbumKind::Albums => !album.is_compilation(),
            AlbumKind::Compilations => album.is_compilation(),
        };
        kind_ok && self.decade.is_none_or(|d| decade_of(album.year) == Some(d))
    }

    /// Indices into `albums` of the albums passing the filter, in order.
    #[must_use]
    pub fn visible_indices(&self, albums: &[Album]) -> Vec<usize> {
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
pub fn decades_present(albums: &[Album]) -> Vec<u32> {
    let mut decades: Vec<u32> = albums.iter().filter_map(|a| decade_of(a.year)).collect();
    decades.sort_unstable();
    decades.dedup();
    decades
}

/// Memoized derived data of the Albums page: positions of the albums that
/// pass the chip filters and the decades present.
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
    /// Number of recomputations (for tests).
    computed: usize,
}

impl AlbumFilterCache {
    /// Force the next [`AlbumFilterCache::get`] to recompute.
    pub fn invalidate(&self) {
        self.stale.set(true);
    }

    /// `(visible positions, decades present)` for `albums` under `filter`.
    pub fn get(
        &self,
        epoch: u64,
        filter: &AlbumFilter,
        albums: &[Album],
    ) -> (Rc<Vec<usize>>, Rc<Vec<u32>>) {
        let key = (epoch, *filter, albums.len());
        let mut entry = self.inner.borrow_mut();
        if self.stale.replace(false) || entry.key != Some(key) {
            entry.visible = Rc::new(filter.visible_indices(albums));
            entry.decades = Rc::new(decades_present(albums));
            entry.key = Some(key);
            entry.computed += 1;
        }
        (Rc::clone(&entry.visible), Rc::clone(&entry.decades))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(artist: &str, year: u32) -> Album {
        Album::new("A".into(), artist.into(), year, Vec::new(), None)
    }

    #[test]
    fn kind_filter() {
        let albums = vec![album("Band", 1994), album("Various Artists", 1999)];
        let mut f = AlbumFilter::default();
        assert!(!f.is_active());
        assert_eq!(f.visible_indices(&albums), vec![0, 1]);
        f.set_kind(AlbumKind::Compilations);
        assert_eq!(f.visible_indices(&albums), vec![1]);
        f.set_kind(AlbumKind::Albums);
        assert_eq!(f.visible_indices(&albums), vec![0]);
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

        let (visible, decades) = cache.get(1, &f, &albums);
        assert_eq!(*visible, vec![0, 1]);
        assert_eq!(*decades, vec![1980, 1990]);
        assert_eq!(cache.inner.borrow().computed, 1);

        // Same inputs: served from the cache.
        cache.get(1, &f, &albums);
        assert_eq!(cache.inner.borrow().computed, 1);

        // Filter change.
        f.set_decade(Some(1990));
        let (visible, _) = cache.get(1, &f, &albums);
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
