// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use crate::library::Track;
use crate::online::podcast::{self, DownloadError, PodcastSearchResult};
use crate::online::store::{Episode, Podcast};
use crate::online::{UrlError, validate_http_url};
use crate::task::{Effect, channel_stream};
use futures::SinkExt;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Bytes received so far by a running download (`total` is `None` when
    /// the server did not announce a length).
    DownloadProgress {
        episode_id: i64,
        received: u64,
        total: Option<u64>,
    },
    /// The download was cancelled with [`PodcastsState::cancel_download`];
    /// its partial file is gone.
    DownloadCancelled {
        episode_id: i64,
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

/// A running episode download.
#[derive(Debug, Clone)]
pub struct DownloadState {
    pub received: u64,
    /// The enclosure's length, when the server announced it.
    pub total: Option<u64>,
    cancel: Arc<AtomicBool>,
}

impl DownloadState {
    /// Completed share in `0.0..=1.0`; `None` while the length is unknown.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.received as f64 / total as f64).clamp(0.0, 1.0) as f32)
    }

    /// Whether a cancel was requested and the transfer is winding down.
    #[must_use]
    pub fn is_cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// A subscription whose last refresh failed.
#[derive(Debug, Clone, Copy)]
pub struct RefreshFailure<'a> {
    pub podcast: &'a Podcast,
    pub reason: &'a str,
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
    /// Bumped on every mutation of [`Self::episodes`], so views can key
    /// derived caches on it instead of hashing the list each frame.
    pub episodes_generation: u64,
    /// Podcast ids currently being refreshed.
    pub refreshing: HashSet<i64>,
    /// Why each subscription's last refresh failed, by podcast id; cleared
    /// by a successful refresh or [`Self::dismiss_refresh_errors`].
    pub refresh_errors: HashMap<i64, String>,
    /// Episodes currently being downloaded for offline playback, by id.
    pub downloading: HashMap<i64, DownloadState>,
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
            let podcasts = crate::task::spawn_blocking(podcast::load_subscriptions)
                .await
                .unwrap_or_default();
            PodcastEvent::PodcastsLoaded(podcasts)
        })
    }

    fn load_episodes(podcast_id: i64) -> Effect<PodcastEvent> {
        Effect::future(async move {
            let episodes = crate::task::spawn_blocking(move || podcast::load_episodes(podcast_id))
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
            let result = crate::task::spawn_blocking(move || {
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
        self.episodes_generation += 1;
        Self::load_episodes(id)
    }

    /// Closes the show detail.
    pub fn close_show(&mut self) {
        self.selected = None;
        self.episodes.clear();
        self.episodes_generation += 1;
    }

    /// Shared by add-by-URL and Subscribe-from-Discover: validates the feed
    /// URL (already trimmed), then fetches + parses the feed and upserts the
    /// podcast and its episodes into the online store.
    ///
    /// # Errors
    /// The URL is empty or not `http(s)`; nothing was started.
    pub fn subscribe(&self, feed_url: String) -> Result<Effect<PodcastEvent>, UrlError> {
        validate_http_url(&feed_url)?;
        Ok(Effect::future(async move {
            PodcastEvent::Subscribed {
                result: podcast::subscribe(&feed_url).await,
            }
        }))
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
        Effect::future(async move {
            PodcastEvent::Refreshed {
                id,
                result: podcast::refresh(id, &feed_url).await,
            }
        })
    }

    /// Refreshes every subscription.
    pub fn refresh_all(&mut self) -> Effect<PodcastEvent> {
        let ids: Vec<i64> = self.podcasts.iter().map(|p| p.id).collect();
        Effect::batch(ids.into_iter().map(|id| self.refresh(id)))
    }

    /// Subscriptions whose last refresh failed, in list order.
    #[must_use]
    pub fn refresh_failures(&self) -> Vec<RefreshFailure<'_>> {
        self.podcasts
            .iter()
            .filter_map(|podcast| {
                let reason = self.refresh_errors.get(&podcast.id)?;
                Some(RefreshFailure { podcast, reason })
            })
            .collect()
    }

    /// Refreshes again every subscription whose last refresh failed.
    pub fn retry_failed_refreshes(&mut self) -> Effect<PodcastEvent> {
        let ids: Vec<i64> = self
            .refresh_failures()
            .iter()
            .map(|f| f.podcast.id)
            .collect();
        Effect::batch(ids.into_iter().map(|id| self.refresh(id)))
    }

    /// Forgets the recorded refresh failures (the banner's close button).
    pub fn dismiss_refresh_errors(&mut self) {
        self.refresh_errors.clear();
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
        self.refresh_errors.remove(&id);
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
        Effect::future(async move {
            PodcastEvent::SearchResults {
                generation,
                result: podcast::search(&query).await,
            }
        })
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
            bit_depth: 0,
            release_type: String::new(),
            suffix: crate::library::suffix_of(&episode.downloaded_path),
            added_at: 0,
            favorited_at: 0,
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
        self.episodes_generation += 1;
        self.save_position(episode_id, position_ms, played)
    }

    /// Downloads an episode's enclosure for offline playback. A no-op for an
    /// unknown episode or one already downloading.
    pub fn download(&mut self, episode_id: i64) -> Effect<PodcastEvent> {
        let Some(episode) = self.episodes.iter().find(|e| e.id == episode_id).cloned() else {
            return Effect::none();
        };
        if self.downloading.contains_key(&episode.id) {
            return Effect::none();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.downloading.insert(
            episode.id,
            DownloadState {
                received: 0,
                total: None,
                cancel: Arc::clone(&cancel),
            },
        );
        Effect::stream(channel_stream(8, move |mut events| async move {
            let mut progress = events.clone();
            let outcome = crate::task::spawn_blocking(move || {
                podcast::download_episode(&episode, &cancel, |received, total| {
                    // A full channel only drops an intermediate report.
                    let _ = progress.try_send(PodcastEvent::DownloadProgress {
                        episode_id,
                        received,
                        total,
                    });
                })
            })
            .await
            .unwrap_or_else(|e| Err(DownloadError::Failed(e.to_string())));
            let last = match outcome {
                Ok(path) => PodcastEvent::Downloaded {
                    episode_id,
                    result: Ok(path),
                },
                Err(DownloadError::Cancelled) => PodcastEvent::DownloadCancelled { episode_id },
                Err(DownloadError::Failed(reason)) => PodcastEvent::Downloaded {
                    episode_id,
                    result: Err(reason),
                },
            };
            let _ = events.send(last).await;
        }))
    }

    /// Asks a running download to stop; the entry stays in
    /// [`Self::downloading`] (see [`DownloadState::is_cancelling`]) until
    /// [`PodcastEvent::DownloadCancelled`] arrives. A no-op when `episode_id`
    /// is not downloading.
    pub fn cancel_download(&self, episode_id: i64) {
        if let Some(state) = self.downloading.get(&episode_id) {
            state.cancel.store(true, Ordering::Relaxed);
        }
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
                    self.episodes_generation += 1;
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
                        self.refresh_errors.remove(&id);
                        let episodes = if selected == Some(id) {
                            Self::load_episodes(id)
                        } else {
                            Effect::none()
                        };
                        PodcastUpdate::effect(Effect::batch([self.load(), episodes]))
                    }
                    Err(reason) => {
                        // Unsubscribed while the refresh was in flight: nothing to show.
                        if self.podcasts.iter().any(|p| p.id == id) {
                            self.refresh_errors.insert(id, reason);
                        }
                        PodcastUpdate::none()
                    }
                }
            }
            PodcastEvent::DownloadProgress {
                episode_id,
                received,
                total,
            } => {
                if let Some(state) = self.downloading.get_mut(&episode_id) {
                    state.received = received;
                    state.total = total;
                }
                PodcastUpdate::none()
            }
            PodcastEvent::DownloadCancelled { episode_id } => {
                self.downloading.remove(&episode_id);
                PodcastUpdate::none()
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
                            self.episodes_generation += 1;
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
                            self.episodes_generation += 1;
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
            cadence: None,
        }
    }

    #[test]
    fn download_tracks_progress_and_ends_on_completion() {
        let mut state = PodcastsState {
            selected: Some(1),
            episodes: vec![episode(10, 0, false)],
            ..PodcastsState::default()
        };
        let effect = state.download(10);
        assert!(!effect.is_none());
        // A second request for the same episode is a no-op.
        assert!(state.download(10).is_none());
        assert_eq!(state.downloading[&10].fraction(), None);

        let _ = state.apply_event(PodcastEvent::DownloadProgress {
            episode_id: 10,
            received: 250,
            total: Some(1_000),
        });
        assert_eq!(state.downloading[&10].fraction(), Some(0.25));

        let update = state.apply_event(PodcastEvent::Downloaded {
            episode_id: 10,
            result: Ok("/tmp/ep.mp3".into()),
        });
        assert!(update.notice.is_none());
        assert!(state.downloading.is_empty());
        assert_eq!(state.episodes[0].downloaded_path, "/tmp/ep.mp3");
    }

    #[test]
    fn cancelling_a_download_keeps_it_listed_until_the_task_confirms() {
        let mut state = PodcastsState {
            episodes: vec![episode(10, 0, false)],
            ..PodcastsState::default()
        };
        let _ = state.download(10);
        assert!(!state.downloading[&10].is_cancelling());
        state.cancel_download(10);
        assert!(state.downloading[&10].is_cancelling());
        // Progress after the request must not resurrect or drop the entry.
        let _ = state.apply_event(PodcastEvent::DownloadProgress {
            episode_id: 10,
            received: 5,
            total: None,
        });
        assert!(state.downloading.contains_key(&10));
        let update = state.apply_event(PodcastEvent::DownloadCancelled { episode_id: 10 });
        assert!(update.notice.is_none());
        assert!(state.downloading.is_empty());
        // Progress for an unknown episode is ignored.
        let _ = state.apply_event(PodcastEvent::DownloadProgress {
            episode_id: 10,
            received: 5,
            total: None,
        });
        assert!(state.downloading.is_empty());
    }

    #[test]
    fn failed_refreshes_are_kept_per_show_until_a_refresh_succeeds() {
        let mut state = PodcastsState {
            podcasts: vec![podcast(1), podcast(2)],
            ..PodcastsState::default()
        };
        let update = state.apply_event(PodcastEvent::Refreshed {
            id: 1,
            result: Err("HTTP 503".into()),
        });
        assert!(update.notice.is_none());
        let failures = state.refresh_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(
            (failures[0].podcast.id, failures[0].reason),
            (1, "HTTP 503")
        );

        // Retry re-queues only the failed show.
        assert!(!state.retry_failed_refreshes().is_none());
        assert!(state.refreshing.contains(&1) && !state.refreshing.contains(&2));

        let _ = state.apply_event(PodcastEvent::Refreshed {
            id: 1,
            result: Ok(()),
        });
        assert!(state.refresh_failures().is_empty());

        // A failure for a show that is gone is not recorded.
        let _ = state.apply_event(PodcastEvent::Refreshed {
            id: 9,
            result: Err("late".into()),
        });
        assert!(state.refresh_errors.is_empty());
        state.refresh_errors.insert(2, "x".into());
        state.dismiss_refresh_errors();
        assert!(state.refresh_failures().is_empty());
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

    #[test]
    fn episode_mutations_bump_the_generation() {
        let mut state = PodcastsState {
            selected: Some(1),
            episodes: vec![episode(10, 0, false)],
            ..PodcastsState::default()
        };
        let _ = state.apply_event(PodcastEvent::Downloaded {
            episode_id: 10,
            result: Ok("/tmp/ep.mp3".into()),
        });
        assert_eq!(state.episodes_generation, 1);
        let _ = state.apply_event(PodcastEvent::DownloadDeleted {
            episode_id: 10,
            result: Ok(()),
        });
        assert_eq!(state.episodes_generation, 2);
        // An unknown episode changes nothing.
        let _ = state.apply_event(PodcastEvent::DownloadDeleted {
            episode_id: 99,
            result: Ok(()),
        });
        assert_eq!(state.episodes_generation, 2);
        state.close_show();
        assert_eq!(state.episodes_generation, 3);
    }
}
