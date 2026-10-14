// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use crate::library::Track;
use crate::online::podcast::{self, PodcastSearchResult};
use crate::online::store::{Episode, Podcast};
use crate::online::{UrlError, validate_http_url};
use crate::task::Effect;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Async results of the podcast workflows; feed them to
/// [`PodcastsState::apply_event`].
#[derive(Debug, Clone)]
pub enum PodcastEvent {
    PodcastsLoaded(Vec<Podcast>),
    EpisodesLoaded {
        podcast_id: i64,
        episodes: Vec<Episode>,
    },
    /// `generation` pairs with the request that kicked it off, so a slow,
    /// since-superseded query's results (or error) can be recognized and
    /// dropped instead of overwriting a newer one.
    SearchResults {
        generation: u64,
        result: Result<Vec<PodcastSearchResult>, String>,
    },
    /// `Ok` carries the subscribed show's title.
    Subscribed {
        result: Result<String, String>,
    },
    Refreshed {
        id: i64,
        result: Result<(), String>,
    },
    Unsubscribed {
        result: Result<(), String>,
    },
    Downloaded {
        episode_id: i64,
        result: Result<String, String>,
    },
    DownloadDeleted {
        episode_id: i64,
        result: Result<(), String>,
    },
    /// An episode position write finished (failures are logged by core);
    /// nothing to apply.
    PositionSaved,
}

/// Something the user should be told about after applying a [`PodcastEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PodcastNotice {
    /// Subscribed to the show titled `name`; the add form can be reset.
    Subscribed {
        name: String,
    },
    SubscribeFailed {
        reason: String,
    },
    RefreshFailed {
        reason: String,
    },
    UnsubscribeFailed {
        reason: String,
    },
    DownloadFailed {
        reason: String,
    },
}

/// What a UI must do after [`PodcastsState::apply_event`].
#[must_use]
pub struct PodcastUpdate {
    /// Follow-up work (reloads); its events go back into `apply_event`.
    pub effect: Effect<PodcastEvent>,
    /// Image URLs the UI should fetch icons for.
    pub icon_urls: Vec<String>,
    pub notice: Option<PodcastNotice>,
}

impl PodcastUpdate {
    fn none() -> Self {
        Self {
            effect: Effect::none(),
            icon_urls: Vec::new(),
            notice: None,
        }
    }

    fn effect(effect: Effect<PodcastEvent>) -> Self {
        Self {
            effect,
            ..Self::none()
        }
    }

    fn notice(notice: PodcastNotice) -> Self {
        Self {
            notice: Some(notice),
            ..Self::none()
        }
    }
}

/// How [`PodcastsState::episode_playback`] wants an episode started.
#[derive(Debug, Clone)]
pub enum EpisodePlayback {
    /// The episode is already the current track: toggle play/pause.
    Toggle,
    /// Play `track`, then seek to `resume` (a finished episode restarts from
    /// the top, so `resume` is `None` for it) and call
    /// [`PodcastsState::start_episode`].
    Play {
        episode_id: i64,
        track: Box<Track>,
        resume: Option<Duration>,
    },
}

/// Subscriptions, the open show's episodes, directory search and playback
/// bookkeeping.
#[derive(Default)]
pub struct PodcastsState {
    pub podcasts: Vec<Podcast>,
    /// The show open in the detail view (see [`Self::select`] /
    /// [`Self::close_show`]).
    pub selected: Option<i64>,
    /// Episodes of the show open in the detail view.
    pub episodes: Vec<Episode>,
    /// Podcast ids currently being refreshed.
    pub refreshing: HashSet<i64>,
    /// Episode ids currently being downloaded for offline playback.
    pub downloading: HashSet<i64>,
    /// The episode id currently playing, if the current track came from a
    /// podcast subscription — used to persist playback position.
    pub current_episode_id: Option<i64>,
    /// Last whole-second position persisted for `current_episode_id`, so the
    /// tick handler only writes to the DB roughly every 5 seconds.
    pub last_saved_position_secs: u64,
    pub search_results: Vec<PodcastSearchResult>,
    pub search_loading: bool,
    /// Error from the last search, shown in the results slot with a Retry
    /// action instead of only logging/toasting.
    pub search_error: Option<String>,
    /// Bumped on every new search dispatch so a slow, since-superseded
    /// request's result can be recognized and dropped.
    pub search_generation: u64,
}

impl PodcastsState {
    /// Loads the subscribed podcasts from the online store.
    pub fn load(&self) -> Effect<PodcastEvent> {
        Self::load_subscriptions()
    }

    fn load_subscriptions() -> Effect<PodcastEvent> {
        Effect::future(async move {
            let podcasts = tokio::task::spawn_blocking(podcast::load_subscriptions)
                .await
                .unwrap_or_default();
            PodcastEvent::PodcastsLoaded(podcasts)
        })
    }

    fn load_episodes(podcast_id: i64) -> Effect<PodcastEvent> {
        Effect::future(async move {
            let episodes = tokio::task::spawn_blocking(move || podcast::load_episodes(podcast_id))
                .await
                .unwrap_or_default();
            PodcastEvent::EpisodesLoaded {
                podcast_id,
                episodes,
            }
        })
    }

    /// Persists podcast episode playback progress. Fire-and-forget: a
    /// transient DB error is logged, not surfaced, since it shouldn't
    /// interrupt playback.
    pub fn save_position(
        &self,
        episode_id: i64,
        position_ms: i64,
        played: bool,
    ) -> Effect<PodcastEvent> {
        Effect::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                podcast::save_episode_position(episode_id, position_ms, played)
            })
            .await;
            if let Ok(Err(e)) = result {
                tracing::warn!("Failed to save podcast position: {e}");
            }
            PodcastEvent::PositionSaved
        })
    }

    /// Opens a show's detail: drops the previous show's episodes and loads
    /// this one's.
    pub fn select(&mut self, id: i64) -> Effect<PodcastEvent> {
        self.selected = Some(id);
        self.episodes.clear();
        Self::load_episodes(id)
    }

    /// Closes the show detail.
    pub fn close_show(&mut self) {
        self.selected = None;
        self.episodes.clear();
    }

    /// Shared by add-by-URL and Subscribe-from-Discover: validates the feed
    /// URL (already trimmed), then fetches + parses the feed and upserts the
    /// podcast and its episodes into the online store.
    ///
    /// # Errors
    /// The URL is empty or not `http(s)`; nothing was started.
    pub fn subscribe(&self, feed_url: String) -> Result<Effect<PodcastEvent>, UrlError> {
        validate_http_url(&feed_url)?;
        Ok(Effect::blocking(
            move || podcast::subscribe(&feed_url),
            |result| PodcastEvent::Subscribed { result },
        ))
    }

    /// Re-fetches a podcast's feed. A no-op for an unknown id or one that is
    /// already refreshing.
    pub fn refresh(&mut self, id: i64) -> Effect<PodcastEvent> {
        let Some(podcast) = self.podcasts.iter().find(|p| p.id == id) else {
            return Effect::none();
        };
        if !self.refreshing.insert(id) {
            return Effect::none();
        }
        let feed_url = podcast.feed_url.clone();
        Effect::blocking(
            move || podcast::refresh(id, &feed_url),
            move |result| PodcastEvent::Refreshed { id, result },
        )
    }

    /// Refreshes every subscription.
    pub fn refresh_all(&mut self) -> Effect<PodcastEvent> {
        let ids: Vec<i64> = self.podcasts.iter().map(|p| p.id).collect();
        Effect::batch(ids.into_iter().map(|id| self.refresh(id)))
    }

    /// Unsubscribes from a podcast: drops it from the in-memory list right
    /// away (so the row disappears immediately) and deletes it (plus its
    /// downloaded episode files) from the store. Deliberately not undo-able:
    /// a podcast's per-episode playback positions and downloads would be
    /// permanently lost by the cascade delete, so a UI should confirm first.
    ///
    /// When `id` is the show open in the detail view it is closed (see
    /// [`Self::close_show`]). Returns `None` for an unknown id.
    pub fn unsubscribe(&mut self, id: i64) -> Option<Effect<PodcastEvent>> {
        if !self.podcasts.iter().any(|p| p.id == id) {
            return None;
        }
        self.podcasts.retain(|p| p.id != id);
        if self.selected == Some(id) {
            self.close_show();
        }
        Some(Effect::blocking(
            move || podcast::unsubscribe(id),
            |result| PodcastEvent::Unsubscribed { result },
        ))
    }

    /// Searches the podcast directory for `query` (already trimmed); a no-op
    /// for an empty one. Supersedes any search still in flight.
    pub fn search(&mut self, query: String) -> Effect<PodcastEvent> {
        if query.is_empty() {
            return Effect::none();
        }
        self.search_generation += 1;
        let generation = self.search_generation;
        self.search_loading = true;
        self.search_error = None;
        Effect::blocking(
            move || podcast::search(&query),
            move |result| PodcastEvent::SearchResults { generation, result },
        )
    }

    /// Decides how to start `episode_id` of the open show: toggle if it is
    /// already the current track, else the track to play. `None` when the
    /// episode or its show cannot be found.
    pub fn episode_playback(
        &self,
        episode_id: i64,
        current_track: Option<&Track>,
    ) -> Option<EpisodePlayback> {
        let already_current = self.current_episode_id == Some(episode_id)
            && current_track.is_some_and(|t| &*t.provider_id == "podcast");
        if already_current {
            return Some(EpisodePlayback::Toggle);
        }

        let podcast_id = self.selected?;
        let episode = self.episodes.iter().find(|e| e.id == episode_id)?;
        let show = self.podcasts.iter().find(|p| p.id == podcast_id)?;

        let track = Track {
            id: -1,
            path: if episode.downloaded_path.is_empty() {
                PathBuf::new()
            } else {
                PathBuf::from(&episode.downloaded_path)
            },
            title: episode.title.clone(),
            artist: show.title.clone(),
            album_artist: show.title.clone(),
            album: show.title.clone(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 0,
            duration: Duration::from_secs(episode.duration_secs.max(0) as u64),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("podcast"),
            source_uri: episode.enclosure_url.clone(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        };
        let resume = (episode.position_ms > 0 && !episode.played)
            .then(|| Duration::from_millis(episode.position_ms as u64));
        Some(EpisodePlayback::Play {
            episode_id: episode.id,
            track: Box::new(track),
            resume,
        })
    }

    /// Records that `episode_id` just started playing (resuming at `resume`,
    /// if any) so its position is persisted from here on.
    pub fn start_episode(&mut self, episode_id: i64, resume: Option<Duration>) {
        self.current_episode_id = Some(episode_id);
        self.last_saved_position_secs = resume.map_or(0, |r| r.as_secs());
    }

    /// Toggles an episode's played marker: updates the in-memory copy
    /// immediately (instant UI feedback) and persists in the background.
    pub fn toggle_played(&mut self, episode_id: i64) -> Effect<PodcastEvent> {
        let Some(episode) = self.episodes.iter_mut().find(|e| e.id == episode_id) else {
            return Effect::none();
        };
        episode.played = !episode.played;
        let (played, position_ms) = (episode.played, episode.position_ms);
        self.save_position(episode_id, position_ms, played)
    }

    /// Downloads an episode's enclosure for offline playback. A no-op for an
    /// unknown episode or one already downloading.
    pub fn download(&mut self, episode_id: i64) -> Effect<PodcastEvent> {
        let Some(episode) = self.episodes.iter().find(|e| e.id == episode_id).cloned() else {
            return Effect::none();
        };
        if !self.downloading.insert(episode.id) {
            return Effect::none();
        }
        Effect::blocking(
            move || podcast::download_episode(&episode),
            move |result| PodcastEvent::Downloaded { episode_id, result },
        )
    }

    /// Deletes an episode's downloaded file. A no-op when nothing is
    /// downloaded.
    pub fn delete_download(&self, episode_id: i64) -> Effect<PodcastEvent> {
        let Some(episode) = self.episodes.iter().find(|e| e.id == episode_id) else {
            return Effect::none();
        };
        if episode.downloaded_path.is_empty() {
            return Effect::none();
        }
        let path = episode.downloaded_path.clone();
        Effect::blocking(
            move || podcast::delete_download(episode_id, &path),
            move |result| PodcastEvent::DownloadDeleted { episode_id, result },
        )
    }

    /// Applies an async result (episode results for any show but the open
    /// one are dropped).
    pub fn apply_event(&mut self, event: PodcastEvent) -> PodcastUpdate {
        let selected = self.selected;
        match event {
            PodcastEvent::PodcastsLoaded(podcasts) => {
                let icon_urls = non_empty(podcasts.iter().map(|p| &p.image_url));
                self.podcasts = podcasts;
                PodcastUpdate {
                    icon_urls,
                    ..PodcastUpdate::none()
                }
            }
            PodcastEvent::EpisodesLoaded {
                podcast_id,
                episodes,
            } => {
                if selected == Some(podcast_id) {
                    self.episodes = episodes;
                }
                PodcastUpdate::none()
            }
            PodcastEvent::SearchResults { generation, result } => {
                if generation != self.search_generation {
                    // Superseded by a newer request; drop silently.
                    return PodcastUpdate::none();
                }
                self.search_loading = false;
                match result {
                    Ok(results) => {
                        let icon_urls = non_empty(results.iter().map(|r| &r.image));
                        self.search_results = results;
                        self.search_error = None;
                        PodcastUpdate {
                            icon_urls,
                            ..PodcastUpdate::none()
                        }
                    }
                    Err(e) => {
                        self.search_results.clear();
                        self.search_error = Some(e);
                        PodcastUpdate::none()
                    }
                }
            }
            PodcastEvent::Subscribed { result } => match result {
                Ok(name) => {
                    self.search_results.clear();
                    PodcastUpdate {
                        notice: Some(PodcastNotice::Subscribed { name }),
                        ..PodcastUpdate::effect(self.load())
                    }
                }
                Err(reason) => PodcastUpdate::notice(PodcastNotice::SubscribeFailed { reason }),
            },
            PodcastEvent::Refreshed { id, result } => {
                self.refreshing.remove(&id);
                match result {
                    Ok(()) => {
                        let episodes = if selected == Some(id) {
                            Self::load_episodes(id)
                        } else {
                            Effect::none()
                        };
                        PodcastUpdate::effect(Effect::batch([self.load(), episodes]))
                    }
                    Err(reason) => PodcastUpdate::notice(PodcastNotice::RefreshFailed { reason }),
                }
            }
            PodcastEvent::Unsubscribed { result } => match result {
                Ok(()) => PodcastUpdate::effect(self.load()),
                Err(reason) => PodcastUpdate::notice(PodcastNotice::UnsubscribeFailed { reason }),
            },
            PodcastEvent::Downloaded { episode_id, result } => {
                self.downloading.remove(&episode_id);
                match result {
                    Ok(path) => {
                        if let Some(ep) = self.episodes.iter_mut().find(|e| e.id == episode_id) {
                            ep.downloaded_path = path;
                        }
                        PodcastUpdate::none()
                    }
                    Err(reason) => PodcastUpdate::notice(PodcastNotice::DownloadFailed { reason }),
                }
            }
            PodcastEvent::DownloadDeleted { episode_id, result } => {
                match result {
                    Ok(()) => {
                        if let Some(ep) = self.episodes.iter_mut().find(|e| e.id == episode_id) {
                            ep.downloaded_path = String::new();
                        }
                    }
                    Err(e) => tracing::error!("Failed to clear episode download: {e}"),
                }
                PodcastUpdate::none()
            }
            PodcastEvent::PositionSaved => PodcastUpdate::none(),
        }
    }
}

fn non_empty<'a>(urls: impl Iterator<Item = &'a String>) -> Vec<String> {
    urls.filter(|u| !u.is_empty()).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn episode(id: i64, position_ms: i64, played: bool) -> Episode {
        Episode {
            id,
            podcast_id: 1,
            guid: String::new(),
            title: String::new(),
            enclosure_url: String::new(),
            mime: String::new(),
            duration_secs: 0,
            pub_date: 0,
            description: String::new(),
            position_ms,
            played,
            downloaded_path: String::new(),
        }
    }

    fn podcast(id: i64) -> Podcast {
        Podcast {
            id,
            feed_url: String::new(),
            title: String::new(),
            description: String::new(),
            image_url: String::new(),
            author: String::new(),
            last_refreshed: 0,
            unplayed_count: 0,
        }
    }

    #[test]
    fn stale_search_generation_is_dropped() {
        let mut state = PodcastsState {
            search_generation: 2,
            search_loading: true,
            ..Default::default()
        };
        let update = state.apply_event(PodcastEvent::SearchResults {
            generation: 1,
            result: Err("late".into()),
        });
        assert!(state.search_loading);
        assert!(state.search_error.is_none());
        assert!(update.effect.is_none());
    }

    #[test]
    fn episodes_for_a_deselected_show_are_dropped() {
        let mut state = PodcastsState::default();
        let event = || PodcastEvent::EpisodesLoaded {
            podcast_id: 7,
            episodes: vec![episode(1, 0, false)],
        };
        state.selected = Some(8);
        let _ = state.apply_event(event());
        assert!(state.episodes.is_empty());
        state.selected = Some(7);
        let _ = state.apply_event(event());
        assert_eq!(state.episodes.len(), 1);
    }

    #[test]
    fn finished_episode_restarts_from_the_top() {
        let mut state = PodcastsState {
            podcasts: vec![podcast(1)],
            selected: Some(1),
            episodes: vec![episode(10, 5_000, true), episode(11, 5_000, false)],
            ..PodcastsState::default()
        };
        let Some(EpisodePlayback::Play { resume, .. }) = state.episode_playback(10, None) else {
            panic!("expected Play");
        };
        assert_eq!(resume, None);
        let Some(EpisodePlayback::Play { resume, .. }) = state.episode_playback(11, None) else {
            panic!("expected Play");
        };
        assert_eq!(resume, Some(Duration::from_millis(5_000)));
        state.start_episode(11, resume);
        assert_eq!(state.last_saved_position_secs, 5);
    }
}
