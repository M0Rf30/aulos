// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! App-side logic for the Home page: loading the shelves, resolving card
//! clicks, and recording plays into the library's play history.

use super::{AppModel, ContextPage, Message, Page};
use crate::library::history::{
    AlbumRef, DecadeCount, HomeData, LOCAL_PROVIDER, SHELF_LEN, exclude_albums,
};
use crate::library::{Album, LibraryDb, Track};
use crate::online::scrobble::Service;
use crate::online::scrobble::controller::is_connected;
use crate::online::similar::{self, SuggestParams};
use crate::player::PlaybackState;
use crate::views::Route;
use crate::views::home::{DecadeView, HomeMessage, album_route};
use cosmic::Application;
use cosmic::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// How long loaded Home shelves are reused before a Home visit reloads them
/// anyway (the "this month" windows are relative to the wall clock).
const HOME_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// What the currently loaded `HomeData` was computed for. A Home visit only
/// reloads when this no longer matches (a play was recorded, the provider or
/// the suggestions toggle changed) or the data aged out; library changes and
/// explicit refreshes always reload via `load_home` directly.
#[derive(Debug, Default)]
pub(super) struct HomeCache {
    /// Bumped whenever a play is written to the history.
    play_gen: u64,
    loaded: Option<HomeCacheKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomeCacheKey {
    provider: String,
    suggestions: bool,
    play_gen: u64,
    loaded_at: std::time::Instant,
}

impl HomeCache {
    /// A play was recorded: loaded shelves are out of date.
    fn invalidate(&mut self) {
        self.play_gen += 1;
    }

    /// Record that shelves for `provider` were just loaded.
    fn mark_loaded(&mut self, provider: &str, suggestions: bool, now: std::time::Instant) {
        self.loaded = Some(HomeCacheKey {
            provider: provider.to_string(),
            suggestions,
            play_gen: self.play_gen,
            loaded_at: now,
        });
    }

    /// Whether the loaded shelves can be shown again as they are.
    fn is_fresh(&self, provider: &str, suggestions: bool, now: std::time::Instant) -> bool {
        self.loaded.as_ref().is_some_and(|k| {
            k.provider == provider
                && k.suggestions == suggestions
                && k.play_gen == self.play_gen
                && now.saturating_duration_since(k.loaded_at) < HOME_CACHE_TTL
        })
    }
}

impl AppModel {
    /// Whether the active provider's library lives in the library database
    /// (and therefore has play history).
    fn home_uses_database(&self) -> bool {
        self.registry.active_id() == LOCAL_PROVIDER
    }

    /// Switch the sidebar to `page` (no-op if it isn't in the nav).
    fn open_page(&mut self, page: Page) -> Task<cosmic::Action<Message>> {
        let target = self
            .nav
            .iter()
            .find(|&id| self.nav.data::<Page>(id) == Some(&page));
        match target {
            Some(id) => self.select_nav(id),
            None => Task::none(),
        }
    }
    /// Handle every `Message::Home`.
    pub(super) fn update_home(&mut self, msg: HomeMessage) -> Task<cosmic::Action<Message>> {
        match msg {
            HomeMessage::OpenAlbum { artist, name } => {
                return self.navigate(album_route(&artist, &name));
            }
            HomeMessage::OpenArtist(name) => return self.navigate(Route::Artist(name)),
            HomeMessage::PlayAlbum { artist, name } => {
                if let Some(album) = find_album(&self.all_albums, &artist, &name) {
                    let tracks = album.tracks.clone();
                    return self.play_track_list(tracks, 0);
                }
            }
            HomeMessage::AddMusicDir => return self.update(Message::AddMusicDir),
            HomeMessage::ConnectServer => {
                return self.update(Message::ToggleContextPage(ContextPage::Providers));
            }
            HomeMessage::OpenSettings => {
                return self.update(Message::ToggleContextPage(ContextPage::Settings));
            }
            HomeMessage::BrowseRadio => return self.open_page(Page::Radio),
            HomeMessage::DiscoverPodcasts => return self.open_page(Page::Podcasts),
            HomeMessage::PlayArtist(name) => {
                let tracks = artist_tracks(&self.all_albums, &name);
                if !tracks.is_empty() {
                    return self.play_track_list(tracks, 0);
                }
            }
            HomeMessage::Refresh => return self.load_home(false),
            HomeMessage::ShuffleRandom => return self.reroll_random_picks(),
            HomeMessage::OpenDecade(decade) => {
                self.home.decade = Some(DecadeView {
                    decade,
                    albums: None,
                });
                return self.load_decade(decade);
            }
            HomeMessage::CloseDecade => self.home.decade = None,
            HomeMessage::PlayDecade => {
                let tracks: Vec<Track> = self
                    .home
                    .decade
                    .as_ref()
                    .and_then(|view| view.albums.as_ref())
                    .map(|albums| {
                        albums
                            .iter()
                            .filter_map(|a| find_album(&self.all_albums, &a.artist, &a.name))
                            .flat_map(|album| album.tracks.iter().cloned())
                            .collect()
                    })
                    .unwrap_or_default();
                if !tracks.is_empty() {
                    return self.play_shuffled(tracks);
                }
            }
            HomeMessage::Loaded {
                request,
                keep_picks,
                data,
            } => {
                if request == self.home.request {
                    let mut data = *data;
                    if keep_picks && let Some(old) = &self.home.data {
                        if !old.random.is_empty() {
                            data.random = old.random.clone();
                        }
                        if !old.rediscover.is_empty() && !data.rediscover.is_empty() {
                            data.rediscover = old.rediscover.clone();
                        }
                    }
                    self.home.data = Some(data);
                    self.home_cache.mark_loaded(
                        self.registry.active_id(),
                        self.suggestions_enabled(),
                        std::time::Instant::now(),
                    );
                    return self.load_similar();
                }
            }
            HomeMessage::RandomLoaded { request, albums } => {
                if request == self.home.request
                    && let Some(data) = &mut self.home.data
                {
                    data.random = albums;
                }
            }
            HomeMessage::DecadeLoaded { decade, albums } => {
                if let Some(view) = &mut self.home.decade
                    && view.decade == decade
                {
                    view.albums = Some(albums);
                }
            }
            HomeMessage::OpenUrl(url) => {
                if url.starts_with("https://") {
                    return self.update(Message::LaunchUrl(url));
                }
            }
            HomeMessage::SimilarShelf { request, shelf } => {
                if request == self.home.request {
                    if self.home.similar_request != request {
                        self.home.similar.clear();
                        self.home.similar_request = request;
                    }
                    self.home.similar.push(*shelf);
                }
            }
            HomeMessage::SimilarDone { request, count } => {
                // Nothing came back (offline, no taste data): show no shelves
                // rather than the previous visit's.
                if request == self.home.request
                    && count == 0
                    && self.home.similar_request != request
                {
                    self.home.similar.clear();
                    self.home.similar_request = request;
                }
            }
            HomeMessage::PlayRecorded => self.home_cache.invalidate(),
            HomeMessage::Scrolled(area, offset) => self.extras.grid_scroll.set(area, offset),
        }
        Task::none()
    }

    /// Whether "Because you listened to …" shelves should be loaded: the
    /// setting is on (default) and a scrobbling service is connected.
    fn suggestions_enabled(&self) -> bool {
        self.config.home_online_suggestions
            && Service::ALL.iter().any(|s| is_connected(&self.config, *s))
    }

    /// (Re)load the similar-artist shelves in the background, one shelf at a
    /// time as they become available. Cancels a lookup still in flight.
    pub(super) fn load_similar(&mut self) -> Task<cosmic::Action<Message>> {
        self.home.similar_cancel.store(true, Ordering::Relaxed);
        if !self.suggestions_enabled() || !self.home_uses_database() {
            self.home.similar.clear();
            return Task::none();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.home.similar_cancel = Arc::clone(&cancel);
        let params = SuggestParams {
            db_path: super::online_db_path(),
            cache_path: similar::cache_path(),
            lastfm_key: self.config.scrobble_lastfm_api_key.clone(),
            now: super::now_epoch(),
        };
        cosmic::task::stream(similar_stream(params, self.home.request, cancel))
    }

    /// (Re)load every Home shelf. Library-database providers query the play
    /// history on a blocking thread; remote providers get a history-less
    /// Home built from the albums already in memory.
    pub(super) fn load_home(&mut self, keep_picks: bool) -> Task<cosmic::Action<Message>> {
        self.home.request += 1;
        let request = self.home.request;

        if !self.home_uses_database() {
            self.home.data = Some(home_from_albums(&self.all_albums));
            return Task::none();
        }

        let db_path = super::online_db_path();
        cosmic::task::future(async move {
            let data = tokio::task::spawn_blocking(move || {
                LibraryDb::open(&db_path)
                    .and_then(|db| db.home_data(LOCAL_PROVIDER, super::now_epoch()))
            })
            .await;
            let data = match data {
                Ok(Ok(data)) => data,
                Ok(Err(e)) => {
                    tracing::warn!("Home data failed to load: {e}");
                    HomeData::default()
                }
                Err(e) => {
                    tracing::warn!("Home data task failed: {e}");
                    HomeData::default()
                }
            };
            cosmic::Action::App(Message::Home(HomeMessage::Loaded {
                request,
                keep_picks,
                data: Box::new(data),
            }))
        })
    }

    /// Called when the Home page is opened from the sidebar (or returned
    /// to): leave any decade grid and refresh the shelves, keeping the
    /// random picks stable.
    pub(super) fn enter_home(&mut self) -> Task<cosmic::Action<Message>> {
        self.home.decade = None;
        // Database-backed shelves are reused while nothing they depend on
        // changed (no play recorded, same provider, within the TTL): the
        // library-load and import paths call `load_home` directly, so those
        // still refresh.
        if self.home_uses_database()
            && self.home.data.is_some()
            && self.home_cache.is_fresh(
                self.registry.active_id(),
                self.suggestions_enabled(),
                std::time::Instant::now(),
            )
        {
            return Task::none();
        }
        self.load_home(true)
    }

    /// Re-roll only the "Random picks" shelf.
    fn reroll_random_picks(&mut self) -> Task<cosmic::Action<Message>> {
        let exclude: Vec<AlbumRef> = self
            .home
            .data
            .as_ref()
            .map(|d| d.rediscover.clone())
            .unwrap_or_default();

        if !self.home_uses_database() {
            let mut picks = random_album_refs(&self.all_albums, SHELF_LEN as usize * 2);
            exclude_albums(&mut picks, &exclude, SHELF_LEN as usize);
            if let Some(data) = &mut self.home.data {
                data.random = picks;
            }
            return Task::none();
        }

        let request = self.home.request;
        let db_path = super::online_db_path();
        cosmic::task::future(async move {
            let picks = tokio::task::spawn_blocking(move || {
                LibraryDb::open(&db_path)
                    .and_then(|db| db.random_albums(LOCAL_PROVIDER, SHELF_LEN * 2))
            })
            .await;
            let mut albums = match picks {
                Ok(Ok(albums)) => albums,
                Ok(Err(e)) => {
                    tracing::warn!("Random picks failed to load: {e}");
                    Vec::new()
                }
                Err(e) => {
                    tracing::warn!("Random picks task failed: {e}");
                    Vec::new()
                }
            };
            exclude_albums(&mut albums, &exclude, SHELF_LEN as usize);
            cosmic::Action::App(Message::Home(HomeMessage::RandomLoaded { request, albums }))
        })
    }

    /// Load the albums of one decade for the decade grid.
    fn load_decade(&mut self, decade: u32) -> Task<cosmic::Action<Message>> {
        if !self.home_uses_database() {
            if let Some(view) = &mut self.home.decade {
                view.albums = Some(decade_albums(&self.all_albums, decade));
            }
            return Task::none();
        }

        let db_path = super::online_db_path();
        cosmic::task::future(async move {
            let albums = tokio::task::spawn_blocking(move || {
                LibraryDb::open(&db_path).and_then(|db| db.albums_by_decade(LOCAL_PROVIDER, decade))
            })
            .await;
            let albums = match albums {
                Ok(Ok(albums)) => albums,
                Ok(Err(e)) => {
                    tracing::warn!("Decade albums failed to load: {e}");
                    Vec::new()
                }
                Err(e) => {
                    tracing::warn!("Decade albums task failed: {e}");
                    Vec::new()
                }
            };
            cosmic::Action::App(Message::Home(HomeMessage::DecadeLoaded { decade, albums }))
        })
    }

    /// Turn shuffle on and start `tracks` at a random position, like the
    /// genre page's shuffle.
    fn play_shuffled(&mut self, tracks: Vec<Track>) -> Task<cosmic::Action<Message>> {
        if !self.config.shuffle {
            self.config.shuffle = true;
            if let Some(player) = &mut self.player {
                player.set_shuffle(true);
            }
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0);
        let start = nanos % tracks.len().max(1);
        self.play_track_list(tracks, start)
    }

    /// Feed the playback tick into the play-history tracker; once the
    /// current track has been listened to for half its length (or four
    /// minutes), write one play to the library database.
    ///
    /// Only database-backed tracks are counted — remote tracks (MPD,
    /// Subsonic), radio and podcasts have no stable library id.
    pub(super) fn track_play_history(&mut self) -> Task<cosmic::Action<Message>> {
        let Some(track) = &self.current_track else {
            self.home.tracked_track = None;
            return Task::none();
        };
        if track.id <= 0 || &*track.provider_id != LOCAL_PROVIDER {
            return Task::none();
        }
        let (id, duration) = (track.id, track.duration);

        if self.home.tracked_track != Some(id) {
            self.home.tracker.reset();
            self.home.tracked_track = Some(id);
        }
        let playing = self
            .player
            .as_ref()
            .is_some_and(|p| p.state() == PlaybackState::Playing);

        if !self
            .home
            .tracker
            .observe(self.playback_position, duration, playing)
        {
            return Task::none();
        }

        let db_path = super::online_db_path();
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                LibraryDb::open(&db_path).and_then(|db| db.record_play(id, super::now_epoch()))
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("Failed to record play: {e}"),
                Err(e) => tracing::warn!("Record-play task failed: {e}"),
            }
            cosmic::Action::App(Message::Home(HomeMessage::PlayRecorded))
        })
    }
}

/// Compute the similar-artist shelves on the blocking pool, streaming each
/// shelf as a `HomeMessage` and finishing with `SimilarDone`.
fn similar_stream(
    params: SuggestParams,
    request: u64,
    cancel: Arc<AtomicBool>,
) -> impl cosmic::iced::futures::Stream<Item = cosmic::Action<Message>> {
    use cosmic::iced::futures::SinkExt;
    cosmic::iced::stream::channel(
        8,
        move |mut out: cosmic::iced::futures::channel::mpsc::Sender<cosmic::Action<Message>>| async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<HomeMessage>();
            let worker = tokio::task::spawn_blocking(move || {
                let count = {
                    let mut on_shelf = |shelf| {
                        let _ = tx.send(HomeMessage::SimilarShelf {
                            request,
                            shelf: Box::new(shelf),
                        });
                    };
                    match similar::compute_shelves(&params, &cancel, &mut on_shelf) {
                        Ok(count) => count,
                        Err(e) => {
                            tracing::warn!("similar-artist shelves failed: {e}");
                            0
                        }
                    }
                };
                let _ = tx.send(HomeMessage::SimilarDone { request, count });
            });
            while let Some(msg) = rx.recv().await {
                if out
                    .send(cosmic::Action::App(Message::Home(msg)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            let _ = worker.await;
        },
    )
}

fn eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The album with this `(artist, name)` identity.
fn find_album<'a>(albums: &'a [Album], artist: &str, name: &str) -> Option<&'a Album> {
    albums
        .iter()
        .find(|a| eq(&a.name, name) && eq(&a.artist, artist))
}

/// Every track of every album credited to `artist`, album by album.
fn artist_tracks(albums: &[Album], artist: &str) -> Vec<Track> {
    albums
        .iter()
        .filter(|a| eq(&a.artist, artist))
        .flat_map(|a| a.tracks.iter().cloned())
        .collect()
}

/// The first year of `year`'s decade, if it is a plausible release year.
fn decade_of(year: u32) -> Option<u32> {
    (year >= 1900).then_some(year / 10 * 10)
}

fn album_ref(album: &Album) -> AlbumRef {
    AlbumRef {
        artist: album.artist.clone(),
        name: album.name.clone(),
        year: album.year,
        plays: 0,
        timestamp: 0,
    }
}

/// Fisher–Yates shuffle driven by a time-seeded xorshift: good enough for
/// "random picks", and avoids a dependency.
fn shuffled<T>(mut items: Vec<T>) -> Vec<T> {
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    for i in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(i, (state % (i as u64 + 1)) as usize);
    }
    items
}

fn random_album_refs(albums: &[Album], limit: usize) -> Vec<AlbumRef> {
    let candidates: Vec<&Album> = albums.iter().filter(|a| !a.name.is_empty()).collect();
    shuffled(candidates)
        .into_iter()
        .take(limit)
        .map(album_ref)
        .collect()
}

/// Albums released in the decade starting at `decade`, by year then artist.
fn decade_albums(albums: &[Album], decade: u32) -> Vec<AlbumRef> {
    let mut found: Vec<AlbumRef> = albums
        .iter()
        .filter(|a| !a.name.is_empty() && decade_of(a.year) == Some(decade))
        .map(album_ref)
        .collect();
    found.sort_by(|a, b| {
        a.year
            .cmp(&b.year)
            .then_with(|| a.artist.cmp(&b.artist))
            .then_with(|| a.name.cmp(&b.name))
    });
    found
}

/// A history-less Home for providers that don't live in the library
/// database: random picks and decades from the albums in memory.
fn home_from_albums(albums: &[Album]) -> HomeData {
    let mut decades: Vec<DecadeCount> = Vec::new();
    for album in albums.iter().filter(|a| !a.name.is_empty()) {
        if let Some(decade) = decade_of(album.year) {
            match decades.iter_mut().find(|d| d.decade == decade) {
                Some(entry) => entry.albums += 1,
                None => decades.push(DecadeCount { decade, albums: 1 }),
            }
        }
    }
    decades.sort_by_key(|d| d.decade);

    HomeData {
        random: random_album_refs(albums, SHELF_LEN as usize),
        decades,
        total_albums: albums.len() as u32,
        history_available: false,
        ..HomeData::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn track(album: &str, artist: &str, n: u32) -> Track {
        Track {
            id: 0,
            path: format!("/m/{artist}/{album}/{n}").into(),
            title: format!("t{n}"),
            artist: artist.into(),
            album_artist: artist.into(),
            album: album.into(),
            genre: String::new(),
            track_number: n,
            disc_number: 1,
            year: 0,
            duration: Duration::from_secs(100),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("subsonic"),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    fn album(name: &str, artist: &str, year: u32, tracks: u32) -> Album {
        Album::new(
            name.into(),
            artist.into(),
            year,
            (1..=tracks).map(|n| track(name, artist, n)).collect(),
            None,
        )
    }

    fn library() -> Vec<Album> {
        vec![
            album("A", "Alpha", 1975, 2),
            album("B", "Beta", 1984, 3),
            album("C", "Beta", 1989, 1),
            album("D", "Delta", 0, 1),
            album("", "Loose", 1990, 1),
        ]
    }

    #[test]
    fn decade_of_rejects_implausible_years() {
        assert_eq!(decade_of(1984), Some(1980));
        assert_eq!(decade_of(2000), Some(2000));
        assert_eq!(decade_of(0), None);
        assert_eq!(decade_of(1899), None);
    }

    #[test]
    fn find_album_matches_case_and_whitespace_insensitively() {
        let lib = library();
        assert!(find_album(&lib, " beta ", "b").is_some());
        assert!(find_album(&lib, "Alpha", "B").is_none());
    }

    #[test]
    fn artist_tracks_collects_every_album_in_order() {
        let tracks = artist_tracks(&library(), "Beta");
        assert_eq!(tracks.len(), 4);
        assert_eq!(tracks[0].album, "B");
        assert_eq!(tracks[3].album, "C");
        assert!(artist_tracks(&library(), "Nobody").is_empty());
    }

    #[test]
    fn decade_albums_filters_and_sorts() {
        let eighties = decade_albums(&library(), 1980);
        let names: Vec<_> = eighties.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["B", "C"]);
        assert!(decade_albums(&library(), 2000).is_empty());
        // Untitled albums never appear.
        assert!(decade_albums(&library(), 1990).is_empty());
    }

    #[test]
    fn shuffled_is_a_permutation() {
        let mut out = shuffled((0..50).collect::<Vec<u32>>());
        out.sort_unstable();
        assert_eq!(out, (0..50).collect::<Vec<u32>>());
        assert!(shuffled(Vec::<u32>::new()).is_empty());
    }

    #[test]
    fn fallback_home_has_random_picks_and_decades_but_no_history() {
        let home = home_from_albums(&library());
        assert!(!home.history_available);
        assert_eq!(home.total_albums, 5);
        assert_eq!(home.random.len(), 4, "untitled album is skipped");
        assert_eq!(
            home.decades,
            [
                DecadeCount {
                    decade: 1970,
                    albums: 1
                },
                DecadeCount {
                    decade: 1980,
                    albums: 2
                },
            ]
        );
        assert!(home.recently_played.is_empty() && home.recently_added.is_empty());
    }

    #[test]
    fn home_cache_is_fresh_until_play_provider_toggle_or_ttl() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut cache = HomeCache::default();
        assert!(!cache.is_fresh("local", true, t0), "nothing loaded yet");

        cache.mark_loaded("local", true, t0);
        assert!(cache.is_fresh("local", true, t0 + Duration::from_secs(60)));
        assert!(!cache.is_fresh("mpd", true, t0), "other provider");
        assert!(!cache.is_fresh("local", false, t0), "suggestions toggled");
        assert!(
            !cache.is_fresh("local", true, t0 + HOME_CACHE_TTL + Duration::from_secs(1)),
            "aged out"
        );

        cache.invalidate();
        assert!(!cache.is_fresh("local", true, t0), "play recorded");
        cache.mark_loaded("local", true, t0);
        assert!(cache.is_fresh("local", true, t0));
    }
}
