// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The in-memory library a UI browses: every track/album/artist/genre of the
//! active provider, the generation counters that invalidate derived caches
//! and in-flight async results, the non-destructive reload staging buffer,
//! and the header-search filter results.
//!
//! The album covers live here too ([`CoverStore`]); UI-side caches that hang
//! off the library (image handles built from them, folder tree, navigation)
//! are *not* here; a UI keeps those next to a [`LibraryState`] and drives
//! them from the same events.

use super::album_sort::{sort_albums, sort_artists, sort_genres};
use super::compilations::is_various_artists;
use super::covers::CoverStore;
use super::scan_progress::ScanProgress;
use super::search::{SearchIndex, contains_all, is_latest_generation, query_tokens};
use super::top_tracks::ArtistTop;
use super::{Album, Artist, Playlist, Track};
use crate::config::Config;
use crate::online::scrobble::import::normalize_text;
use std::collections::HashMap;
use std::sync::Arc;

/// Column the Songs list is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Title,
    Artist,
    Album,
    Duration,
}

/// Case- and whitespace-insensitive name comparison used for every
/// "same artist / same album" lookup.
#[must_use]
pub fn names_match(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Translates a position in a search-filtered list back to its index in
/// the corresponding unfiltered library vector. Passthrough when `map` is
/// `None` (search inactive or the query is empty).
#[must_use]
fn unfilter_index(map: Option<&[usize]>, i: usize) -> usize {
    map.map_or(i, |m| m[i])
}

/// Pure staleness check for an async library reload result.
///
/// `true` when the result's `(generation, provider_id)` no longer matches
/// the currently active reload — i.e. it was superseded by a later
/// reload/scan or a provider switch — and must be discarded without
/// mutating library data or the scanning flag.
fn reload_result_is_stale(
    current_generation: u64,
    current_provider_id: &str,
    result_generation: u64,
    result_provider_id: &str,
) -> bool {
    result_generation != current_generation || result_provider_id != current_provider_id
}

/// The leading artist of a collaboration credit, if the credit has one.
fn primary_artist(name: &str) -> Option<&str> {
    const SEPARATORS: [&str; 8] = [
        " feat. ",
        " feat ",
        " ft. ",
        " featuring ",
        " & ",
        ", ",
        "; ",
        " / ",
    ];
    // ASCII-only lowering keeps byte offsets valid for slicing `name`.
    let lower = name.to_ascii_lowercase();
    SEPARATORS
        .iter()
        .filter_map(|sep| lower.find(sep))
        .min()
        .map(|pos| name[..pos].trim())
        .filter(|s| !s.is_empty())
}

/// Buffers a non-destructive library refresh: batches accumulate here
/// while the previously loaded library stays fully visible, so switching
/// pages, opening a menu, etc. never sees an empty view mid reload.
/// [`LibraryState::complete_reload`] swaps this in atomically once the
/// whole reload finishes.
struct ReloadStaging {
    generation: u64,
    tracks: Vec<Arc<Track>>,
    albums: Vec<Arc<Album>>,
}

/// Search-filtered copies of the library lists, plus the index maps that
/// translate a filtered position back to its index in the corresponding
/// unfiltered vector (so index-carrying view messages resolve against the
/// real data).
///
/// Cached rather than built fresh inside `view()` because view functions
/// borrow `&'a [T]` slices that must live as long as `&self`.
#[derive(Default)]
pub struct SearchFilter {
    /// Whether the lists below hold the results of a non-empty query (the
    /// query of the last refresh, not necessarily the latest typed one).
    active: bool,
    albums: Vec<Arc<Album>>,
    album_map: Vec<usize>,
    artists: Vec<Artist>,
    artist_map: Vec<usize>,
    tracks: Vec<Arc<Track>>,
    track_map: Vec<usize>,
    playlists: Vec<Playlist>,
    playlist_map: Vec<usize>,
    genres: Vec<String>,
    genre_map: Vec<usize>,
    /// Whether the Artists view must read [`Self::artists`] (query active,
    /// or compilations hidden and at least one such artist exists).
    artists_filtered: bool,
}

impl SearchFilter {
    fn clear(&mut self) {
        self.active = false;
        self.albums.clear();
        self.album_map.clear();
        self.artists.clear();
        self.artist_map.clear();
        self.tracks.clear();
        self.track_map.clear();
        self.playlists.clear();
        self.playlist_map.clear();
        self.genres.clear();
        self.genre_map.clear();
    }
}

/// Copy-on-write edits of shared tracks and albums, remembering the edited
/// copy of each original so every holder ends up on the same allocation.
#[derive(Default)]
struct SharedEdits {
    tracks: HashMap<*const Track, Arc<Track>>,
    albums: HashMap<*const Album, Arc<Album>>,
}

impl SharedEdits {
    fn edit_track(&mut self, track: &mut Arc<Track>, id: &str, f: &mut impl FnMut(&mut Track)) {
        if !track.has_key(id) {
            return;
        }
        let original = Arc::as_ptr(track);
        if let Some(edited) = self.tracks.get(&original) {
            *track = Arc::clone(edited);
            return;
        }
        f(Arc::make_mut(track));
        self.tracks.insert(original, Arc::clone(track));
    }

    fn edit_album(&mut self, album: &mut Arc<Album>, id: &str, f: &mut impl FnMut(&mut Track)) {
        let original = Arc::as_ptr(album);
        if let Some(edited) = self.albums.get(&original) {
            *album = Arc::clone(edited);
            return;
        }
        if !album.tracks.iter().any(|t| t.has_key(id)) {
            return;
        }
        for track in &mut Arc::make_mut(album).tracks {
            self.edit_track(track, id, f);
        }
        self.albums.insert(original, Arc::clone(album));
    }
}

/// Library data plus the bookkeeping derived from it.
#[derive(Default)]
pub struct LibraryState {
    pub tracks: Vec<Arc<Track>>,
    pub albums: Vec<Arc<Album>>,
    pub artists: Vec<Artist>,
    /// All distinct genres from the active provider.
    pub genres: Vec<String>,
    /// The genre row open in the genre detail view, if any (set by
    /// `Session::load_genre_tracks`; a UI clears it when leaving the view).
    /// Loaded tracks for any other genre are dropped.
    pub selected_genre: Option<usize>,
    /// Tracks of the genre currently shown in the genre detail view.
    pub genre_tracks: Vec<Track>,
    /// Bumped whenever `tracks`/`albums`/`artists` change (load, batch,
    /// sort, artist rebuild); invalidates derived caches.
    generation: u64,
    /// Counts in-place track edits (favourite toggles); see
    /// [`Self::favorites_epoch`].
    track_edits: u64,
    /// Monotonically increasing counter for library reloads/scans. Bumped
    /// before every new reload/scan so in-flight async results tagged with
    /// an older generation (or a different provider id) can be detected
    /// and ignored as stale.
    reload_generation: u64,
    /// Staged data for a non-destructive library refresh in flight. `None`
    /// when no reload is running, or the running reload is a first/empty-
    /// library load that instead populates the visible lists progressively.
    reload_staging: Option<ReloadStaging>,
    /// Pre-lowercased search keys (rebuilt per `generation`).
    search_index: SearchIndex,
    /// Current header-search query (every word must occur, ignoring case,
    /// diacritics and punctuation, in the active page's fields); see [`Self::search_query`].
    search_query: String,
    /// Latest debounce generation for search-query changes.
    search_debounce_gen: u64,
    /// Bumped on every [`Self::refresh_search`], i.e. whenever the lists of
    /// the `visible_*` accessors may have changed.
    filter_generation: u64,
    /// Results of the current search; see [`SearchFilter`].
    filtered: SearchFilter,
    /// Album cover thumbnails.
    pub covers: CoverStore,
    /// Whether a library scan/reload is in flight.
    pub scanning: bool,
    /// A watcher batch changed tracks while a scan was running: reload
    /// when that scan completes.
    pub reload_after_scan: bool,
    /// The ranked top tracks of the artist page last loaded.
    pub artist_top: Option<ArtistTop>,
    /// How far the running scan is; shared with the scanning thread.
    pub scan_progress: Arc<ScanProgress>,
    /// [`Self::generation`] the UI's folder tree was last built for.
    pub folder_tree_gen: Option<u64>,
    /// Whether any configured music dir exists (cached; avoids `stat` per frame).
    pub music_dirs_present: bool,
}

impl LibraryState {
    /// Generation of the library lists; changes whenever they do.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Bumps the library reload generation counter, invalidating any
    /// in-flight async result tagged with an older generation. Call this
    /// once at the start of every new reload/scan that should supersede
    /// earlier work.
    pub fn begin_reload_generation(&mut self) -> u64 {
        self.reload_generation += 1;
        self.reload_generation
    }

    /// True when an async library result tagged with `generation`/
    /// `provider_id` is stale — superseded by a later reload/scan or a
    /// provider switch (`active_provider_id` is the provider now active) —
    /// and must be ignored without mutating state.
    #[must_use]
    pub fn is_stale_reload(
        &self,
        active_provider_id: &str,
        generation: u64,
        provider_id: &str,
    ) -> bool {
        reload_result_is_stale(
            self.reload_generation,
            active_provider_id,
            generation,
            provider_id,
        )
    }

    /// Prepare for an incremental (batched) reload tagged `generation`.
    ///
    /// With nothing loaded yet there is nothing on screen to flash away
    /// from, so batches populate the visible lists progressively and the
    /// artist index is reset (returns `false`). Otherwise the loaded
    /// library stays fully visible and batches accumulate in a staging
    /// buffer that [`Self::complete_reload`] swaps in atomically (returns
    /// `true`; the UI should stage its own per-reload data the same way).
    pub fn begin_incremental_reload(&mut self, generation: u64) -> bool {
        if self.tracks.is_empty() && self.albums.is_empty() {
            self.reload_staging = None;
            self.artists.clear();
            false
        } else {
            self.reload_staging = Some(ReloadStaging {
                generation,
                tracks: Vec::new(),
                albums: Vec::new(),
            });
            true
        }
    }

    /// Whether a non-destructive refresh tagged `generation` is staging.
    #[must_use]
    pub fn is_staging(&self, generation: u64) -> bool {
        self.reload_staging
            .as_ref()
            .is_some_and(|s| s.generation == generation)
    }

    /// Accumulate a batch into the staging buffer (no-op without one).
    pub fn stage_albums(&mut self, albums: Vec<Arc<Album>>) {
        if let Some(staging) = self.reload_staging.as_mut() {
            for album in &albums {
                staging.tracks.extend(album.tracks.iter().cloned());
            }
            staging.albums.extend(albums);
        }
    }

    /// Append a batch straight into the visible lists (the progressive
    /// first-load path) and fold it into the artist index.
    ///
    /// Returns whether the search filter must be refreshed: a query is
    /// active, the visible artists are a filtered copy (which the batch would
    /// leave stale), or compilations are hidden and the batch brought one.
    pub fn append_batch(&mut self, albums: &[Arc<Album>], config: &Config) -> bool {
        for album in albums {
            self.tracks.extend(album.tracks.iter().cloned());
            self.albums.push(Arc::clone(album));
        }
        self.merge_artists_from_batch(albums, config);
        !self.search_query.trim().is_empty()
            || self.filtered.artists_filtered
            || (!config.show_compilations_in_artists
                && albums.iter().any(|a| is_various_artists(&a.artist)))
    }

    /// Replace the visible lists wholesale (single-shot reload) and rebuild
    /// the artist index from the new albums.
    pub fn replace_loaded(
        &mut self,
        tracks: Vec<Arc<Track>>,
        albums: Vec<Arc<Album>>,
        config: &Config,
    ) {
        self.tracks = tracks;
        self.albums = albums;
        sort_albums(&mut self.albums, config.albums_sort);
        self.rebuild_artists(config);
    }

    /// Apply an incremental scan's result in place: `updated` replace the
    /// tracks at the same path, `removed` paths disappear. Returns `false`
    /// without changing anything when that is not possible (a track that is
    /// not loaded yet, a retag that moves a track to another album, or an
    /// album that would become empty); the caller reloads instead.
    pub fn patch_tracks(&mut self, updated: &[Track], removed: &[String], config: &Config) -> bool {
        use std::collections::{HashMap, HashSet};
        let upd: HashMap<&str, Arc<Track>> = updated
            .iter()
            .filter_map(|t| t.path.to_str().map(|p| (p, Arc::new(t.clone()))))
            .collect();
        let rem: HashSet<&str> = removed.iter().map(String::as_str).collect();
        if upd.len() != updated.len() {
            return false;
        }

        // Validate before touching anything.
        let (mut found_upd, mut found_rem) = (0, 0);
        for album in &self.albums {
            let mut kept = album.tracks.len();
            for track in &album.tracks {
                let Some(path) = track.path.to_str() else {
                    continue;
                };
                if let Some(new) = upd.get(path) {
                    if new.album != track.album || new.album_artist != track.album_artist {
                        return false;
                    }
                    found_upd += 1;
                } else if rem.contains(path) {
                    found_rem += 1;
                    kept -= 1;
                }
            }
            if kept == 0 {
                return false;
            }
        }
        if found_upd != upd.len() || found_rem != rem.len() {
            return false;
        }

        let patch = |tracks: &mut Vec<Arc<Track>>| {
            tracks.retain(|t| t.path.to_str().is_none_or(|p| !rem.contains(p)));
            for track in tracks.iter_mut() {
                if let Some(new) = track.path.to_str().and_then(|p| upd.get(p)) {
                    *track = Arc::clone(new);
                }
            }
        };
        for album in &mut self.albums {
            let touched = album.tracks.iter().any(|t| {
                t.path
                    .to_str()
                    .is_some_and(|p| upd.contains_key(p) || rem.contains(p))
            });
            if touched {
                let album = Arc::make_mut(album);
                patch(&mut album.tracks);
                album.tracks.sort_by(|a, b| Track::disc_track_order(a, b));
                album.refresh_derived();
            }
        }
        patch(&mut self.tracks);
        self.rebuild_artists(config);
        self.sort_browse(config);
        self.generation += 1;
        true
    }

    /// Drop the staged refresh of `generation` without swapping it in.
    pub fn cancel_reload(&mut self, generation: u64) {
        if self
            .reload_staging
            .as_ref()
            .is_some_and(|s| s.generation == generation)
        {
            self.reload_staging = None;
        }
    }

    /// Finish an incremental reload tagged `generation`: swap the staged
    /// refresh in (if one was running) and run the final sort.
    ///
    /// Returns `true` when a staged refresh was swapped in, so the UI can
    /// swap its own staged data.
    pub fn complete_reload(&mut self, generation: u64, config: &Config) -> bool {
        let swapped = if let Some(staging) = self.reload_staging.take()
            && staging.generation == generation
        {
            // The previously displayed library was never cleared, so there
            // is no empty-view flash between old and new data.
            self.tracks = staging.tracks;
            self.albums = staging.albums;
            self.rebuild_artists(config);
            true
        } else {
            false
        };

        self.tracks.sort_by(|a, b| a.title.cmp(&b.title));
        self.sort_browse(config);
        self.generation += 1;
        swapped
    }

    /// Drop the track/album/artist lists (provider switch). The generation
    /// is deliberately left alone: a reload follows immediately.
    pub fn clear_collections(&mut self) {
        self.tracks.clear();
        self.albums.clear();
        self.artists.clear();
        self.artist_top = None;
    }

    /// Incrementally merge new albums into `artists`, splitting each
    /// album's primary-artist tag into individual collaborators when
    /// `split_artist_tags` is enabled — one album can then contribute to
    /// several `Artist` entries so browsing by any collaborator finds it.
    /// Albums keep a single primary attribution (`album.artist`, set by
    /// the provider) for the Albums view; only the artist index widens.
    /// Only processes the `new_albums` slice (the batch that just arrived),
    /// appending to existing artists or creating new ones. Avatars aren't
    /// generated here at all — UIs build a placeholder on the fly, and real
    /// photos are fetched lazily.
    fn merge_artists_from_batch(&mut self, new_albums: &[Arc<Album>], config: &Config) {
        self.generation += 1;
        // Build an index over the current artists list for O(1) lookup.
        let mut index: HashMap<String, usize> = self
            .artists
            .iter()
            .enumerate()
            .map(|(i, a)| (a.name.clone(), i))
            .collect();

        let split_enabled = config.split_artist_tags;
        let delimiters = &config.artist_tag_delimiters;

        for album in new_albums {
            let names: Vec<String> = if split_enabled {
                super::artist_tags::split(&album.artist, delimiters)
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            } else {
                vec![album.artist.clone()]
            };

            for name in names {
                if let Some(&idx) = index.get(&name) {
                    self.artists[idx].push_album(Arc::clone(album));
                } else {
                    let idx = self.artists.len();
                    index.insert(name.clone(), idx);
                    self.artists
                        .push(Artist::new(name.clone(), vec![Arc::clone(album)]));
                }
            }
        }
    }

    /// Rebuild `artists` from scratch by re-running the batch-merge
    /// aggregation over every album currently in `albums`. This is the
    /// single aggregation path — used whenever `split_artist_tags` or
    /// `artist_tag_delimiters` changes, so the artist index picks up the
    /// new split rules immediately, without a rescan.
    pub fn rebuild_artists(&mut self, config: &Config) {
        self.artists.clear();
        let albums = std::mem::take(&mut self.albums);
        self.merge_artists_from_batch(&albums, config);
        sort_artists(&mut self.artists, config.artists_sort);
        self.albums = albums;
    }

    /// Order the albums, artists and genres as the configured sort orders
    /// say. Callers that change the lists' order after the fact bump the
    /// generation and refresh the search results themselves.
    pub fn sort_browse(&mut self, config: &Config) {
        sort_albums(&mut self.albums, config.albums_sort);
        sort_artists(&mut self.artists, config.artists_sort);
        for artist in &mut self.artists {
            sort_albums(&mut artist.albums, config.albums_sort);
        }
        sort_genres(&mut self.genres, &self.tracks, config.genres_sort);
    }

    /// [`Self::sort_browse`] after a sort order changed: also invalidates
    /// everything derived from the lists' order.
    pub fn resort(&mut self, config: &Config) {
        self.sort_browse(config);
        self.generation += 1;
    }

    pub fn sort_tracks(&mut self, field: SortField) {
        self.generation += 1;
        match field {
            SortField::Title => self.tracks.sort_by(|a, b| a.title.cmp(&b.title)),
            SortField::Artist => self.tracks.sort_by(|a, b| a.artist.cmp(&b.artist)),
            SortField::Album => self.tracks.sort_by(|a, b| a.album.cmp(&b.album)),
            SortField::Duration => self.tracks.sort_by_key(|a| a.duration),
        }
    }

    /// Recomputes the search-filtered caches from `search_query`.
    ///
    /// Called whenever the query changes and whenever the underlying library
    /// data is reloaded or re-sorted, so the filtered caches — and the index
    /// maps that translate a filtered position back to its index in the
    /// corresponding unfiltered vector — never go stale. When the query is
    /// empty the caches are cleared; views then read directly from the
    /// unfiltered vectors. `playlists` are the caller's playlists (filtered
    /// by name); `hide_compilations` hides "Various Artists" from the
    /// Artists view through the same cache.
    pub fn refresh_search(&mut self, playlists: &[Playlist], hide_compilations: bool) {
        self.filter_generation += 1;
        let tokens = query_tokens(&self.search_query);
        let filtered = &mut self.filtered;
        filtered.clear();

        if self.search_query.trim().is_empty() {
            // Only materialise a filtered artist copy when something is
            // actually hidden; otherwise the view reads `artists`.
            filtered.artists_filtered = false;
            if hide_compilations && self.artists.iter().any(|a| is_various_artists(&a.name)) {
                filtered.artists_filtered = true;
                for (i, artist) in self.artists.iter().enumerate() {
                    if !is_various_artists(&artist.name) {
                        filtered.artists.push(artist.clone());
                        filtered.artist_map.push(i);
                    }
                }
            }
            return;
        }
        filtered.active = true;
        filtered.artists_filtered = true;

        self.search_index
            .ensure(self.generation, &self.albums, &self.artists, &self.tracks);

        for (i, album) in self.albums.iter().enumerate() {
            if self.search_index.album_matches(i, &tokens) {
                filtered.albums.push(Arc::clone(album));
                filtered.album_map.push(i);
            }
        }

        for (i, artist) in self.artists.iter().enumerate() {
            if self.search_index.artist_matches(i, &tokens)
                && !(hide_compilations && is_various_artists(&artist.name))
            {
                filtered.artists.push(artist.clone());
                filtered.artist_map.push(i);
            }
        }

        for (i, track) in self.tracks.iter().enumerate() {
            if self.search_index.track_matches(i, &tokens) {
                filtered.tracks.push(Arc::clone(track));
                filtered.track_map.push(i);
            }
        }

        for (i, playlist) in playlists.iter().enumerate() {
            if contains_all(&normalize_text(&playlist.name), &tokens) {
                filtered.playlists.push(playlist.clone());
                filtered.playlist_map.push(i);
            }
        }

        for (i, genre) in self.genres.iter().enumerate() {
            if contains_all(&normalize_text(genre), &tokens) {
                filtered.genres.push(genre.clone());
                filtered.genre_map.push(i);
            }
        }
    }

    /// The header-search query as typed.
    #[must_use]
    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    /// Stores a new query and supersedes any pending debounce. Returns the
    /// debounce generation to schedule a refresh for, or `None` for an empty
    /// query, which the caller must refresh at once (clearing is cheap and
    /// should feel instant).
    pub(crate) fn begin_search(&mut self, query: String) -> Option<u64> {
        self.search_query = query;
        self.search_debounce_gen += 1;
        (!self.search_query.trim().is_empty()).then_some(self.search_debounce_gen)
    }

    /// Whether the debounce for `generation` is still the latest one.
    #[must_use]
    pub(crate) fn is_latest_search(&self, generation: u64) -> bool {
        is_latest_generation(generation, self.search_debounce_gen)
    }

    /// Changes whenever the lists the `visible_*` accessors return may have
    /// changed (library data, search results or in-place track edits such as
    /// a favourite toggle); a cache key for views memoising something derived
    /// from them. Both low counters only grow, so their sum is unique per state.
    #[must_use]
    pub fn visible_epoch(&self) -> u64 {
        let low = self.filter_generation.wrapping_add(self.track_edits);
        (self.generation << 32) | (low & 0xffff_ffff)
    }

    /// Bumped by every in-place track edit ([`Self::for_each_track`], e.g. a
    /// favourite toggle), which leaves [`Self::generation`] alone; already
    /// folded into [`Self::visible_epoch`].
    #[must_use]
    pub fn favorites_epoch(&self) -> u64 {
        self.track_edits
    }

    /// The albums to show: the search results, or all of them.
    #[must_use]
    pub fn visible_albums(&self) -> &[Arc<Album>] {
        if self.filtered.active {
            &self.filtered.albums
        } else {
            &self.albums
        }
    }

    /// Index in [`Self::albums`] of position `i` of [`Self::visible_albums`].
    #[must_use]
    pub fn album_index(&self, i: usize) -> usize {
        unfilter_index(
            self.filtered
                .active
                .then_some(self.filtered.album_map.as_slice()),
            i,
        )
    }

    /// The tracks to show: the search results, or all of them.
    #[must_use]
    pub fn visible_tracks(&self) -> &[Arc<Track>] {
        if self.filtered.active {
            &self.filtered.tracks
        } else {
            &self.tracks
        }
    }

    /// Index in [`Self::tracks`] of position `i` of [`Self::visible_tracks`].
    #[must_use]
    pub fn track_index(&self, i: usize) -> usize {
        unfilter_index(
            self.filtered
                .active
                .then_some(self.filtered.track_map.as_slice()),
            i,
        )
    }

    /// The artists to show: the search results and/or without compilations
    /// (when hidden), or all of them.
    #[must_use]
    pub fn visible_artists(&self) -> &[Artist] {
        if self.filtered.artists_filtered {
            &self.filtered.artists
        } else {
            &self.artists
        }
    }

    /// Index in [`Self::artists`] of position `i` of
    /// [`Self::visible_artists`].
    #[must_use]
    pub fn artist_index(&self, i: usize) -> usize {
        unfilter_index(
            self.filtered
                .artists_filtered
                .then_some(self.filtered.artist_map.as_slice()),
            i,
        )
    }

    /// The genres to show: the search results, or all of them.
    #[must_use]
    pub fn visible_genres(&self) -> &[String] {
        if self.filtered.active {
            &self.filtered.genres
        } else {
            &self.genres
        }
    }

    /// Index in [`Self::genres`] of position `i` of [`Self::visible_genres`].
    #[must_use]
    pub fn genre_index(&self, i: usize) -> usize {
        unfilter_index(
            self.filtered
                .active
                .then_some(self.filtered.genre_map.as_slice()),
            i,
        )
    }

    /// The playlists to show: the search results, or `all` (the session's
    /// playlists, which this state does not hold).
    #[must_use]
    pub fn visible_playlists<'a>(&'a self, all: &'a [Playlist]) -> &'a [Playlist] {
        if self.filtered.active {
            &self.filtered.playlists
        } else {
            all
        }
    }

    /// Index in the session's playlists of position `i` of
    /// [`Self::visible_playlists`].
    #[must_use]
    pub fn playlist_index(&self, i: usize) -> usize {
        unfilter_index(
            self.filtered
                .active
                .then_some(self.filtered.playlist_map.as_slice()),
            i,
        )
    }

    /// Index of the artist named `name`, falling back to the primary artist
    /// of a collaboration credit ("A feat. B", "A & B", "A, B"…).
    #[must_use]
    pub fn find_artist(&self, name: &str) -> Option<usize> {
        let name = name.trim();
        let exact = |n: &str| {
            self.artists
                .iter()
                .position(|a| a.name.trim().eq_ignore_ascii_case(n))
        };
        exact(name).or_else(|| primary_artist(name).and_then(exact))
    }

    /// Index of the album with this exact `(artist, name)` identity.
    #[must_use]
    pub fn find_album_exact(&self, artist: &str, album: &str) -> Option<usize> {
        self.albums
            .iter()
            .position(|a| names_match(&a.name, album) && names_match(&a.artist, artist))
    }

    /// Index of the album `(artist, name)`, falling back to any album of
    /// that name when the artist credit differs.
    #[must_use]
    pub fn find_album(&self, artist: &str, album: &str) -> Option<usize> {
        self.find_album_exact(artist, album)
            .or_else(|| self.albums.iter().position(|a| names_match(&a.name, album)))
    }

    /// Lightweight album clone (cover source + first track) for the album
    /// whose cover key is built from `artist` and `album_name`.
    #[must_use]
    pub fn cover_hint_for(&self, artist: &str, album_name: &str) -> Option<Album> {
        self.albums
            .iter()
            .find(|a| a.artist == artist && a.name == album_name)
            .map(|a| a.cover_hint())
    }

    /// Every library track whose genre matches `genre`, ignoring case.
    #[must_use]
    pub fn tracks_in_genre(&self, genre: &str) -> Vec<Track> {
        self.tracks
            .iter()
            .filter(|t| t.genre.eq_ignore_ascii_case(genre))
            .map(|t| Track::clone(t))
            .collect()
    }

    /// Apply `f` to the track with `track_id` wherever the library holds
    /// it (flat list, albums, artists' albums and the search results).
    ///
    /// The holders share one allocation per track and album, so each is
    /// copied on write once and the copy is shared again by every holder.
    pub fn for_each_track(&mut self, track_id: &str, mut f: impl FnMut(&mut Track)) {
        self.track_edits += 1;
        let mut memo = SharedEdits::default();
        for album in self.albums.iter_mut().chain(&mut self.filtered.albums) {
            memo.edit_album(album, track_id, &mut f);
        }
        let artists = self.artists.iter_mut().chain(&mut self.filtered.artists);
        for album in artists.flat_map(|artist| artist.albums.iter_mut()) {
            memo.edit_album(album, track_id, &mut f);
        }
        for track in self.tracks.iter_mut().chain(&mut self.filtered.tracks) {
            memo.edit_track(track, track_id, &mut f);
        }
    }

    /// Current favourite state of a track, if known locally. `current` (the
    /// playing track) wins over the library copy.
    #[must_use]
    pub fn favorite_state(&self, current: Option<&Track>, track_id: &str) -> Option<bool> {
        if let Some(ct) = current
            && ct.has_key(track_id)
        {
            return Some(ct.is_favorite);
        }
        self.tracks
            .iter()
            .find(|t| t.has_key(track_id))
            .map(|t| t.is_favorite)
    }

    /// Current rating of a track, if known locally; see
    /// [`Self::favorite_state`].
    #[must_use]
    pub fn rating_state(&self, current: Option<&Track>, track_id: &str) -> Option<u8> {
        if let Some(ct) = current
            && ct.has_key(track_id)
        {
            return ct.rating;
        }
        self.tracks
            .iter()
            .find(|t| t.has_key(track_id))
            .and_then(|t| t.rating)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::track;

    fn local_track(id: i64, title: &str, album: &str) -> Track {
        let mut t = track(id);
        t.provider_id = "local".into();
        t.path = format!("/m/{album}/{title}.flac").into();
        t.title = title.into();
        t.album = album.into();
        t.album_artist = "Artist".into();
        t
    }

    fn lib_with(tracks: Vec<Track>) -> LibraryState {
        let mut lib = LibraryState::default();
        let tracks: Vec<Arc<Track>> = tracks.into_iter().map(Arc::new).collect();
        let mut by_album: Vec<Album> = Vec::new();
        for t in &tracks {
            match by_album.iter_mut().find(|a| a.name == t.album) {
                Some(a) => a.tracks.push(Arc::clone(t)),
                None => {
                    let mut album =
                        Album::new(t.album.clone(), t.album_artist.clone(), 0, Vec::new(), None);
                    album.tracks.push(Arc::clone(t));
                    by_album.push(album);
                }
            }
        }
        let albums = by_album.into_iter().map(Arc::new).collect();
        lib.replace_loaded(tracks, albums, &Config::default());
        lib
    }

    #[test]
    fn patch_replaces_and_removes_tracks_in_place() {
        let mut lib = lib_with(vec![
            local_track(1, "a", "X"),
            local_track(2, "b", "X"),
            local_track(3, "c", "Y"),
        ]);
        let mut edited = local_track(1, "a2", "X");
        edited.path = lib.tracks[0].path.clone();
        let gone = lib.tracks[1].path.to_string_lossy().into_owned();
        assert!(lib.patch_tracks(&[edited], &[gone], &Config::default()));
        assert_eq!(lib.tracks.len(), 2);
        assert!(lib.tracks.iter().any(|t| t.title == "a2"));
        let x = lib.albums.iter().find(|a| a.name == "X").unwrap();
        assert_eq!(x.tracks.len(), 1);
    }

    #[test]
    fn toggling_a_favourite_changes_the_cache_key() {
        let mut lib = lib_with(vec![local_track(1, "a", "X")]);
        let key = lib.tracks[0].key();
        let before = (lib.visible_epoch(), lib.favorites_epoch());
        lib.for_each_track(&key, |t| t.is_favorite = true);
        assert_ne!(before, (lib.visible_epoch(), lib.favorites_epoch()));
        assert!(lib.tracks[0].is_favorite);
        assert!(lib.albums[0].tracks[0].is_favorite);
    }

    #[test]
    fn patch_refuses_what_needs_a_reload() {
        let mut lib = lib_with(vec![local_track(1, "a", "X"), local_track(2, "c", "Y")]);
        let cfg = Config::default();
        // A track that is not loaded yet (new file).
        assert!(!lib.patch_tracks(&[local_track(9, "new", "X")], &[], &cfg));
        // A retag that moves the track to another album.
        let mut moved = local_track(1, "a", "Z");
        moved.path = lib.tracks[0].path.clone();
        assert!(!lib.patch_tracks(&[moved], &[], &cfg));
        // Removing an album's last track.
        let only = lib.tracks[1].path.to_string_lossy().into_owned();
        assert!(!lib.patch_tracks(&[], &[only], &cfg));
        assert_eq!(lib.tracks.len(), 2);
    }

    #[test]
    fn primary_artist_splits_common_credits() {
        assert_eq!(
            primary_artist("Daft Punk feat. Pharrell"),
            Some("Daft Punk")
        );
        assert_eq!(primary_artist("Simon & Garfunkel"), Some("Simon"));
        assert_eq!(primary_artist("A, B; C"), Some("A"));
        assert_eq!(primary_artist("Solo"), None);
    }

    #[test]
    fn matching_generation_and_provider_is_not_stale() {
        assert!(!reload_result_is_stale(3, "mpd-home", 3, "mpd-home"));
    }

    #[test]
    fn result_from_a_superseded_reload_is_stale() {
        // A second reload bumped the generation before this result arrived.
        assert!(reload_result_is_stale(3, "mpd-home", 2, "mpd-home"));
    }

    #[test]
    fn result_from_a_different_provider_is_stale_even_at_current_generation() {
        // The generation counter alone can't catch a switch back to the
        // same numeric generation on a different provider, so identity
        // must be checked independently.
        assert!(reload_result_is_stale(3, "mpd-home", 3, "subsonic-server"));
    }

    #[test]
    fn begin_reload_generation_supersedes_earlier_results() {
        let mut lib = LibraryState::default();
        let first = lib.begin_reload_generation();
        assert!(!lib.is_stale_reload("local", first, "local"));
        let second = lib.begin_reload_generation();
        assert!(lib.is_stale_reload("local", first, "local"));
        assert!(!lib.is_stale_reload("local", second, "local"));
    }

    #[test]
    fn find_album_prefers_exact_identity_then_falls_back_to_name() {
        let lib = LibraryState {
            albums: vec![
                Arc::new(Album::new(
                    "Hits".into(),
                    "Alpha".into(),
                    1990,
                    Vec::new(),
                    None,
                )),
                Arc::new(Album::new(
                    "Hits".into(),
                    "Beta".into(),
                    1991,
                    Vec::new(),
                    None,
                )),
            ],
            ..LibraryState::default()
        };
        assert_eq!(lib.find_album(" beta ", "hits"), Some(1));
        assert_eq!(lib.find_album_exact("Gamma", "Hits"), None);
        assert_eq!(lib.find_album("Gamma", "Hits"), Some(0));
    }

    #[test]
    fn find_artist_falls_back_to_the_primary_credit() {
        let lib = LibraryState {
            artists: vec![Artist::new("Daft Punk".into(), Vec::new())],
            ..LibraryState::default()
        };
        assert_eq!(lib.find_artist("daft punk feat. Pharrell"), Some(0));
        assert_eq!(lib.find_artist("Nobody"), None);
    }

    #[test]
    fn unfilter_index_passes_through_without_a_map() {
        assert_eq!(unfilter_index(None, 4), 4);
        assert_eq!(unfilter_index(Some(&[7, 9]), 1), 9);
    }
}
