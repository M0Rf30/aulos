// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use crate::library::Track;
use crate::online::radio::{
    self, DiscoverPreset, StationQuery, StationSearchResult, locale_country_code,
};
use crate::online::store::{NewStation, RadioStation};
use crate::online::{UrlError, validate_http_url};
use crate::task::Effect;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Async results of the radio workflows; feed them to
/// [`RadioState::apply_event`].
#[derive(Debug, Clone)]
pub enum RadioEvent {
    StationsLoaded(Vec<RadioStation>),
    /// `generation` pairs with the request that kicked it off, so a slow,
    /// since-superseded query's results (or error) can be recognized and
    /// dropped instead of overwriting a newer one.
    SearchResults {
        generation: u64,
        result: Result<Vec<StationSearchResult>, String>,
    },
    Added {
        name: String,
        already_existed: bool,
        result: Result<i64, String>,
    },
    StreamResolved {
        name: String,
        favicon: String,
        key: String,
        result: Result<String, String>,
    },
    /// The codec/bitrate of a saved station was read from its stream
    /// headers; `Ok(Some)` carries the reloaded list, `Ok(None)` means
    /// nothing changed.
    FormatLearned(Result<Option<Vec<RadioStation>>, String>),
}

/// Something the user should be told about after applying a [`RadioEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RadioNotice {
    /// Saved; the add form can be reset.
    Saved {
        name: String,
    },
    /// The station was already saved; the add form can be reset.
    AlreadySaved {
        name: String,
    },
    SaveFailed {
        reason: String,
    },
    PlayFailed {
        reason: String,
    },
}

/// What a UI must do after [`RadioState::apply_event`].
#[must_use]
pub struct RadioUpdate {
    /// Follow-up work (reloads); its events go back into `apply_event`.
    pub effect: Effect<RadioEvent>,
    /// Image URLs the UI should fetch icons for.
    pub icon_urls: Vec<String>,
    pub notice: Option<RadioNotice>,
    /// The resolved stream, ready to be queued and played (as a one-track
    /// list, from index 0).
    pub play: Option<Track>,
}

impl RadioUpdate {
    fn none() -> Self {
        Self {
            effect: Effect::none(),
            icon_urls: Vec::new(),
            notice: None,
            play: None,
        }
    }

    fn effect(effect: Effect<RadioEvent>) -> Self {
        Self {
            effect,
            ..Self::none()
        }
    }

    fn notice(notice: RadioNotice) -> Self {
        Self {
            notice: Some(notice),
            ..Self::none()
        }
    }
}

/// Saved stations, directory search and the identity of the playing station.
pub struct RadioState {
    pub stations: Vec<RadioStation>,
    pub search_results: Vec<StationSearchResult>,
    pub search_loading: bool,
    /// Error from the last search/preset fetch, shown in the results slot
    /// with a Retry action instead of only logging/toasting.
    pub search_error: Option<String>,
    /// Bumped on every new search/preset dispatch so a slow, since-
    /// superseded request's result can be recognized and dropped.
    pub search_generation: u64,
    /// Country code derived once from the process locale at startup (see
    /// [`locale_country_code`]); empty when it can't be determined.
    pub locale_country: String,
    /// Favicon URL and pre-resolution stream/result URL ("key") of the
    /// station currently loaded into the player, captured when playback
    /// started. `current_track.source_uri` alone can't serve as the
    /// "currently playing" identity: stream-URL resolution (following a
    /// `.pls`/`.m3u` playlist) can rewrite it to a different URL than the
    /// one the saved/search row was keyed by.
    pub now_playing_favicon: String,
    pub now_playing_key: String,
}

impl RadioState {
    pub(super) fn new() -> Self {
        Self {
            stations: Vec::new(),
            search_results: Vec::new(),
            search_loading: false,
            search_error: None,
            search_generation: 0,
            locale_country: locale_country_code(),
            now_playing_favicon: String::new(),
            now_playing_key: String::new(),
        }
    }

    /// Loads the saved stations from the online store.
    pub fn load(&self) -> Effect<RadioEvent> {
        Self::load_saved()
    }

    fn load_saved() -> Effect<RadioEvent> {
        Effect::future(async move {
            let stations = tokio::task::spawn_blocking(radio::load_saved)
                .await
                .unwrap_or_default();
            RadioEvent::StationsLoaded(stations)
        })
    }

    /// Add-by-URL: validates `url` (already trimmed) and saves the station,
    /// named `name` (already trimmed) or, when blank, after its URL.
    ///
    /// # Errors
    /// The URL is empty or not `http(s)`; nothing was started.
    pub fn add_by_url(&self, name: String, url: String) -> Result<Effect<RadioEvent>, UrlError> {
        validate_http_url(&url)?;
        let display_name = if name.is_empty() { url.clone() } else { name };
        Ok(Self::save(NewStation {
            name: display_name,
            stream_url: url,
            ..NewStation::default()
        }))
    }

    /// Re-saves a station exactly as it was (Undo of a removal).
    pub fn restore(&self, station: RadioStation) -> Effect<RadioEvent> {
        Self::save(station.into())
    }

    /// Saves the directory search result `uuid`. A no-op for an unknown one.
    pub fn save_search_result(&self, uuid: &str) -> Effect<RadioEvent> {
        let Some(result) = self
            .search_results
            .iter()
            .find(|r| r.stationuuid == uuid)
            .cloned()
        else {
            return Effect::none();
        };
        Self::save(NewStation {
            name: result.name,
            stream_url: result.url,
            homepage: result.homepage,
            favicon_url: result.favicon,
            tags: result.tags,
            codec: result.codec,
            bitrate_kbps: result.bitrate,
        })
    }

    /// Runs the blocking DB upsert and reports `RadioEvent::Added` with the
    /// outcome (including whether the station already existed).
    fn save(station: NewStation) -> Effect<RadioEvent> {
        Effect::future(async move {
            let event_name = station.name.clone();
            let outcome = tokio::task::spawn_blocking(move || radio::save_station(station))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let (result, already_existed) = match outcome {
                Ok((id, existed)) => (Ok(id), existed),
                Err(e) => (Err(e), false),
            };
            RadioEvent::Added {
                name: event_name,
                already_existed,
                result,
            }
        })
    }

    /// Starts playing the saved station `id`: resolves its stream URL (a
    /// `.pls`/`.m3u` playlist is followed) and reports
    /// `RadioEvent::StreamResolved`. A no-op for an unknown id.
    pub fn play_saved(&self, id: i64) -> Effect<RadioEvent> {
        let Some(station) = self.stations.iter().find(|s| s.id == id) else {
            return Effect::none();
        };
        Self::resolve(
            station.name.clone(),
            station.favicon_url.clone(),
            station.stream_url.clone(),
        )
    }

    /// Starts playing the directory search result `uuid` (see
    /// [`Self::play_saved`]). A no-op for an unknown one.
    pub fn play_search_result(&self, uuid: &str) -> Effect<RadioEvent> {
        let Some(result) = self.search_results.iter().find(|r| r.stationuuid == uuid) else {
            return Effect::none();
        };
        Self::resolve(
            result.name.clone(),
            result.favicon.clone(),
            result.url.clone(),
        )
    }

    /// Resolves a station URL and reports `RadioEvent::StreamResolved`.
    /// `favicon` and `key` are threaded through unresolved: `favicon` so the
    /// now-playing strip can show it once playback actually starts, `key`
    /// (the pre-resolution stream/result URL, also the URL to resolve) so
    /// the "currently playing" row indicator keeps matching even after
    /// resolution rewrites the actual stream URL.
    fn resolve(name: String, favicon: String, key: String) -> Effect<RadioEvent> {
        let url = key.clone();
        Effect::blocking(
            move || radio::resolve_stream(&url),
            move |result| RadioEvent::StreamResolved {
                name,
                favicon,
                key,
                result,
            },
        )
    }

    /// Removes a saved station: drops it from the in-memory list right away
    /// (so the row disappears immediately) and deletes it from the store.
    /// Returns the removed station (for an Undo offer, see
    /// [`Self::restore`]) and the reload of the remaining list, or `None`
    /// for an unknown id.
    pub fn remove(&mut self, id: i64) -> Option<(RadioStation, Effect<RadioEvent>)> {
        let station = self.stations.iter().find(|s| s.id == id).cloned()?;
        self.stations.retain(|s| s.id != id);
        let effect = Effect::future(async move {
            let stations = tokio::task::spawn_blocking(move || radio::remove_saved(id))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
                .unwrap_or_else(|e| {
                    tracing::error!("Failed to remove radio station: {e}");
                    Vec::new()
                });
            RadioEvent::StationsLoaded(stations)
        });
        Some((station, effect))
    }

    /// Renames a saved station to `new_name` (already trimmed); a no-op for
    /// an empty name.
    pub fn rename(&self, id: i64, new_name: String) -> Effect<RadioEvent> {
        if new_name.is_empty() {
            return Effect::none();
        }
        Effect::future(async move {
            let stations = tokio::task::spawn_blocking(move || radio::rename_saved(id, &new_name))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
                .unwrap_or_else(|e| {
                    tracing::error!("Failed to rename radio station: {e}");
                    Vec::new()
                });
            RadioEvent::StationsLoaded(stations)
        })
    }

    /// Searches the station directory. Supersedes any search still in flight.
    pub fn search(&mut self, query: StationQuery) -> Effect<RadioEvent> {
        let generation = self.begin_search();
        Effect::blocking(
            move || radio::search(&query),
            move |result| RadioEvent::SearchResults { generation, result },
        )
    }

    /// Fetches a Discover preset list. Supersedes any search still in flight.
    pub fn preset(&mut self, preset: DiscoverPreset) -> Effect<RadioEvent> {
        let generation = self.begin_search();
        Effect::blocking(
            move || match preset {
                DiscoverPreset::Popular => radio::popular(),
                DiscoverPreset::TopVoted => radio::top_voted(),
            },
            move |result| RadioEvent::SearchResults { generation, result },
        )
    }

    fn begin_search(&mut self) -> u64 {
        self.search_generation += 1;
        self.search_loading = true;
        self.search_error = None;
        self.search_generation
    }

    /// Applies an async result.
    pub fn apply_event(&mut self, event: RadioEvent) -> RadioUpdate {
        match event {
            RadioEvent::StationsLoaded(stations) => {
                let icon_urls = non_empty(stations.iter().map(|s| &s.favicon_url));
                self.stations = stations;
                RadioUpdate {
                    icon_urls,
                    ..RadioUpdate::none()
                }
            }
            RadioEvent::SearchResults { generation, result } => {
                if generation != self.search_generation {
                    // Superseded by a newer request; drop silently.
                    return RadioUpdate::none();
                }
                self.search_loading = false;
                match result {
                    Ok(results) => {
                        let icon_urls = non_empty(results.iter().map(|r| &r.favicon));
                        self.search_results = results;
                        self.search_error = None;
                        RadioUpdate {
                            icon_urls,
                            ..RadioUpdate::none()
                        }
                    }
                    Err(e) => {
                        self.search_results.clear();
                        self.search_error = Some(e);
                        RadioUpdate::none()
                    }
                }
            }
            RadioEvent::Added {
                name,
                already_existed,
                result,
            } => match result {
                Ok(_) => RadioUpdate {
                    notice: Some(if already_existed {
                        RadioNotice::AlreadySaved { name }
                    } else {
                        RadioNotice::Saved { name }
                    }),
                    ..RadioUpdate::effect(self.load())
                },
                Err(reason) => RadioUpdate::notice(RadioNotice::SaveFailed { reason }),
            },
            RadioEvent::StreamResolved {
                name,
                favicon,
                key,
                result,
            } => match result {
                Ok(resolved_url) => {
                    self.now_playing_favicon = favicon.clone();
                    self.now_playing_key = key;
                    let icon_urls = if favicon.is_empty() {
                        Vec::new()
                    } else {
                        vec![favicon]
                    };
                    let learn = self
                        .stations
                        .iter()
                        .find(|s| s.stream_url == self.now_playing_key)
                        .filter(|s| s.codec.is_empty() && s.bitrate_kbps == 0)
                        .map(|s| Self::learn_format(s.stream_url.clone()))
                        .unwrap_or_else(Effect::none);
                    RadioUpdate {
                        effect: learn,
                        icon_urls,
                        play: Some(stream_track(name, resolved_url)),
                        ..RadioUpdate::none()
                    }
                }
                Err(reason) => RadioUpdate::notice(RadioNotice::PlayFailed { reason }),
            },
            RadioEvent::FormatLearned(result) => match result {
                Ok(Some(stations)) => self.apply_event(RadioEvent::StationsLoaded(stations)),
                Ok(None) => RadioUpdate::none(),
                Err(e) => {
                    tracing::debug!("radio format probe failed: {e}");
                    RadioUpdate::none()
                }
            },
        }
    }

    /// Reads the codec/bitrate of the saved station `key` from its stream
    /// headers, for stations saved without them.
    fn learn_format(key: String) -> Effect<RadioEvent> {
        Effect::blocking(
            move || radio::learn_station_format(&key),
            RadioEvent::FormatLearned,
        )
    }
}

/// The player-queue entry for a resolved radio stream.
fn stream_track(name: String, stream_url: String) -> Track {
    Track {
        id: -1,
        path: PathBuf::new(),
        title: name,
        artist: String::new(),
        album_artist: String::new(),
        album: String::new(),
        genre: String::new(),
        track_number: 0,
        disc_number: 0,
        year: 0,
        duration: Duration::ZERO,
        bitrate: 0,
        sample_rate: 0,
        provider_id: Arc::from("radio"),
        source_uri: stream_url,
        is_favorite: false,
        rating: None,
        rg_track_gain: None,
        rg_album_gain: None,
        bit_depth: 0,
        release_type: String::new(),
        added_at: 0,
        favorited_at: 0,
    }
}

fn non_empty<'a>(urls: impl Iterator<Item = &'a String>) -> Vec<String> {
    urls.filter(|u| !u.is_empty()).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_search_generation_is_dropped() {
        let mut state = RadioState::new();
        let _ = state.preset(DiscoverPreset::Popular);
        let stale = state.search_generation - 1;
        let _ = state.preset(DiscoverPreset::TopVoted);
        let update = state.apply_event(RadioEvent::SearchResults {
            generation: stale,
            result: Err("late".into()),
        });
        assert!(state.search_loading);
        assert!(state.search_error.is_none());
        assert!(update.notice.is_none());
    }

    #[test]
    fn resolved_stream_records_identity_and_builds_the_track() {
        let mut state = RadioState::new();
        let update = state.apply_event(RadioEvent::StreamResolved {
            name: "FM".into(),
            favicon: "http://x/i.png".into(),
            key: "http://x/list.pls".into(),
            result: Ok("http://real/stream".into()),
        });
        assert_eq!(state.now_playing_key, "http://x/list.pls");
        assert_eq!(update.icon_urls, vec!["http://x/i.png".to_string()]);
        let track = update.play.expect("track to play");
        assert_eq!(&*track.provider_id, "radio");
        assert_eq!(track.source_uri, "http://real/stream");
    }

    fn saved(url: &str, codec: &str, bitrate_kbps: u32) -> RadioStation {
        RadioStation {
            id: 1,
            name: "FM".into(),
            stream_url: url.into(),
            homepage: String::new(),
            favicon_url: String::new(),
            tags: String::new(),
            codec: codec.into(),
            bitrate_kbps,
        }
    }

    fn resolve(state: &mut RadioState, key: &str) -> RadioUpdate {
        state.apply_event(RadioEvent::StreamResolved {
            name: "FM".into(),
            favicon: String::new(),
            key: key.into(),
            result: Ok("http://real/stream".into()),
        })
    }

    #[test]
    fn playing_a_saved_station_without_format_learns_it() {
        let mut state = RadioState::new();
        state.stations = vec![saved("http://x/a", "", 0)];
        assert!(!resolve(&mut state, "http://x/a").effect.is_none());
        // Not a saved station, or one whose format is already known.
        assert!(resolve(&mut state, "http://x/other").effect.is_none());
        state.stations = vec![saved("http://x/a", "MP3", 0)];
        assert!(resolve(&mut state, "http://x/a").effect.is_none());
    }

    #[test]
    fn learned_format_replaces_the_saved_list() {
        let mut state = RadioState::new();
        state.stations = vec![saved("http://x/a", "", 0)];
        let _ = state.apply_event(RadioEvent::FormatLearned(Ok(Some(vec![saved(
            "http://x/a",
            "AAC",
            64,
        )]))));
        assert_eq!(state.stations[0].codec, "AAC");
        let _ = state.apply_event(RadioEvent::FormatLearned(Ok(None)));
        assert_eq!(state.stations[0].bitrate_kbps, 64);
    }
}
