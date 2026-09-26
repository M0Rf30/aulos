// SPDX-License-Identifier: GPL-3.0

//! Podcasts page controller.
//!
//! Owns every piece of update logic for the Podcasts section: the
//! `Message::Podcast` UI-event dispatch (`update_podcast`) and the
//! `Message::PodcastEvent` async-result dispatch (`handle_podcast_event`),
//! plus the private helpers/tasks that back them. The view
//! (`crate::views::podcasts`) stays pure -- this is the only place that
//! touches the online store, the podcast feed/iTunes HTTP client, or
//! `AppModel`'s `podcast_*` fields.

use super::tasks::{download_episode_task, refresh_podcast_task};
use super::{AppModel, HTTP_CLIENT, Message, now_epoch, open_online_store};
use crate::fl;
use crate::library::Track;
use crate::online::podcast::{self, PodcastSearchResult};
use crate::online::store::{Episode, Podcast};
use crate::views::podcasts::{EpisodeFilter, PodcastMessage};
use cosmic::prelude::*;
use cosmic::widget;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Async results for the podcasts page: search/subscribe/refresh/download
/// outcomes.
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
    /// `Ok` carries the subscribed show's title, for the confirmation
    /// toast.
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
}

impl AppModel {
    /// Load subscribed podcasts from the online store.
    pub(super) fn load_podcasts(&self) -> Task<cosmic::Action<Message>> {
        cosmic::task::future(async move {
            let podcasts = tokio::task::spawn_blocking(|| {
                open_online_store().and_then(|store| store.list_podcasts()).unwrap_or_else(|e| {
                    tracing::warn!("list_podcasts failed: {e}");
                    Vec::new()
                })
            })
            .await
            .unwrap_or_default();
            cosmic::Action::App(Message::PodcastEvent(PodcastEvent::PodcastsLoaded(podcasts)))
        })
    }

    /// Load a podcast's episodes from the online store.
    pub(super) fn load_podcast_episodes(&self, podcast_id: i64) -> Task<cosmic::Action<Message>> {
        cosmic::task::future(async move {
            let episodes = tokio::task::spawn_blocking(move || {
                open_online_store().and_then(|store| store.list_episodes(podcast_id)).unwrap_or_else(|e| {
                    tracing::warn!("list_episodes failed: {e}");
                    Vec::new()
                })
            })
            .await
            .unwrap_or_default();
            cosmic::Action::App(Message::PodcastEvent(PodcastEvent::EpisodesLoaded { podcast_id, episodes }))
        })
    }

    /// Persist podcast episode playback progress. Fire-and-forget: a
    /// transient DB error is logged, not surfaced as a toast, since it
    /// shouldn't interrupt playback.
    pub(super) fn save_podcast_position(
        &self,
        episode_id: i64,
        position_ms: i64,
        played: bool,
    ) -> Task<cosmic::Action<Message>> {
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                open_online_store()
                    .and_then(|store| store.save_episode_position(episode_id, position_ms, played))
            })
            .await;
            if let Ok(Err(e)) = result {
                tracing::warn!("Failed to save podcast position: {e}");
            }
            // No-op message — this write is fire-and-forget, matching
            // `dispatch_mpd`'s convention for tasks nothing depends on.
            cosmic::Action::App(Message::PlaybackTick)
        })
    }

    /// Handle a UI event from the podcasts view.
    pub(super) fn update_podcast(&mut self, msg: PodcastMessage) -> Task<cosmic::Action<Message>> {
        match msg {
            PodcastMessage::TabSelected(tab) => {
                self.podcast_tab = tab;
                Task::none()
            }
            PodcastMessage::ToggleAddForm => {
                self.podcast_add_open = !self.podcast_add_open;
                if !self.podcast_add_open {
                    self.podcast_add_url.clear();
                    self.podcast_add_error = None;
                }
                Task::none()
            }
            PodcastMessage::AddUrlChanged(v) => {
                self.podcast_add_url = v;
                self.podcast_add_error = None;
                Task::none()
            }
            PodcastMessage::SubmitAdd => {
                let feed_url = self.podcast_add_url.trim().to_string();
                self.submit_add_podcast(feed_url)
            }
            PodcastMessage::SelectPodcast(id) => self.select_podcast(id),
            PodcastMessage::RefreshPodcast(id) => self.refresh_podcast(id),
            PodcastMessage::RefreshAll => self.refresh_all_podcasts(),
            PodcastMessage::StartUnsubscribe(id) => {
                self.pending_unsubscribe_podcast = Some(id);
                Task::none()
            }
            PodcastMessage::CancelUnsubscribe => {
                self.pending_unsubscribe_podcast = None;
                Task::none()
            }
            PodcastMessage::ConfirmUnsubscribe(id) => self.unsubscribe_podcast(id),

            PodcastMessage::SearchChanged(q) => {
                self.podcast_search_query = q;
                Task::none()
            }
            PodcastMessage::SearchSubmit => self.dispatch_podcast_search(),
            PodcastMessage::RetrySearch => self.dispatch_podcast_search(),
            PodcastMessage::SubscribeFromSearch(feed_url) => self.submit_add_podcast(feed_url),

            PodcastMessage::BackToList => {
                self.selected_podcast = None;
                self.podcast_episodes.clear();
                self.podcast_episode_filter = EpisodeFilter::default();
                self.podcast_episode_text_filter.clear();
                self.podcast_description_expanded = false;
                self.pending_unsubscribe_podcast = None;
                Task::none()
            }
            PodcastMessage::ToggleDescriptionExpanded => {
                self.podcast_description_expanded = !self.podcast_description_expanded;
                Task::none()
            }
            PodcastMessage::EpisodeFilterSelected(filter) => {
                self.podcast_episode_filter = filter;
                Task::none()
            }
            PodcastMessage::EpisodeTextFilterChanged(text) => {
                self.podcast_episode_text_filter = text;
                Task::none()
            }
            PodcastMessage::PlayEpisode(id) => self.play_episode(id),
            PodcastMessage::TogglePlayed(id) => self.toggle_episode_played(id),
            PodcastMessage::Download(id) => self.download_episode(id),
            PodcastMessage::DeleteDownload(id) => self.delete_episode_download(id),
        }
    }

    /// Handle an async result dispatched by one of the podcasts page's
    /// tasks.
    pub(super) fn handle_podcast_event(&mut self, event: PodcastEvent) -> Task<cosmic::Action<Message>> {
        match event {
            PodcastEvent::PodcastsLoaded(podcasts) => {
                let icon_urls: Vec<String> =
                    podcasts.iter().map(|p| p.image_url.clone()).filter(|u| !u.is_empty()).collect();
                self.podcasts = podcasts;
                self.load_online_icons(icon_urls)
            }
            PodcastEvent::EpisodesLoaded { podcast_id, episodes } => {
                if self.selected_podcast == Some(podcast_id) {
                    self.podcast_episodes = episodes;
                }
                Task::none()
            }
            PodcastEvent::SearchResults { generation, result } => {
                if generation != self.podcast_search_generation {
                    // Superseded by a newer request; drop silently.
                    return Task::none();
                }
                self.podcast_search_loading = false;
                match result {
                    Ok(results) => {
                        let icon_urls: Vec<String> =
                            results.iter().map(|r| r.image.clone()).filter(|u| !u.is_empty()).collect();
                        self.podcast_search_results = results;
                        self.podcast_search_error = None;
                        self.load_online_icons(icon_urls)
                    }
                    Err(e) => {
                        self.podcast_search_results.clear();
                        self.podcast_search_error = Some(e);
                        Task::none()
                    }
                }
            }
            PodcastEvent::Subscribed { result } => match result {
                Ok(title) => {
                    self.podcast_add_url.clear();
                    self.podcast_add_open = false;
                    self.podcast_add_error = None;
                    self.podcast_search_results.clear();
                    let toast =
                        self.push_toast(widget::toaster::Toast::new(fl!("toast-podcast-subscribed", name = title)));
                    Task::batch([self.load_podcasts(), toast])
                }
                Err(e) => {
                    self.push_toast(widget::toaster::Toast::new(fl!("toast-podcast-subscribe-failed", reason = e)))
                }
            },
            PodcastEvent::Refreshed { id, result } => {
                self.refreshing_podcasts.remove(&id);
                match result {
                    Ok(()) => {
                        let reload_task = self.load_podcasts();
                        let episodes_task = if self.selected_podcast == Some(id) {
                            self.load_podcast_episodes(id)
                        } else {
                            Task::none()
                        };
                        Task::batch([reload_task, episodes_task])
                    }
                    Err(e) => {
                        self.push_toast(widget::toaster::Toast::new(fl!("toast-podcast-refresh-failed", reason = e)))
                    }
                }
            }
            PodcastEvent::Unsubscribed { result } => match result {
                Ok(()) => self.load_podcasts(),
                Err(e) => self
                    .push_toast(widget::toaster::Toast::new(fl!("toast-podcast-unsubscribe-failed", reason = e))),
            },
            PodcastEvent::Downloaded { episode_id, result } => {
                self.downloading_episodes.remove(&episode_id);
                match result {
                    Ok(path) => {
                        if let Some(ep) = self.podcast_episodes.iter_mut().find(|e| e.id == episode_id) {
                            ep.downloaded_path = path;
                        }
                        Task::none()
                    }
                    Err(e) => {
                        self.push_toast(widget::toaster::Toast::new(fl!("toast-episode-download-failed", reason = e)))
                    }
                }
            }
            PodcastEvent::DownloadDeleted { episode_id, result } => {
                match result {
                    Ok(()) => {
                        if let Some(ep) = self.podcast_episodes.iter_mut().find(|e| e.id == episode_id) {
                            ep.downloaded_path = String::new();
                        }
                    }
                    Err(e) => tracing::error!("Failed to clear episode download: {e}"),
                }
                Task::none()
            }
        }
    }

    fn select_podcast(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        self.selected_podcast = Some(id);
        self.podcast_episodes.clear();
        self.podcast_episode_filter = EpisodeFilter::default();
        self.podcast_episode_text_filter.clear();
        self.podcast_description_expanded = false;
        self.pending_unsubscribe_podcast = None;
        self.load_podcast_episodes(id)
    }

    /// Shared by add-by-URL and Subscribe-from-Discover: validates the
    /// feed URL, fetches + parses the feed, and upserts the podcast and
    /// its episodes into the online store.
    fn submit_add_podcast(&mut self, feed_url: String) -> Task<cosmic::Action<Message>> {
        if feed_url.is_empty() {
            self.podcast_add_error = Some(fl!("podcast-add-url-required"));
            return Task::none();
        }
        if !(feed_url.starts_with("http://") || feed_url.starts_with("https://")) {
            self.podcast_add_error = Some(fl!("podcast-add-url-invalid"));
            return Task::none();
        }
        self.podcast_add_error = None;
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
                let client = HTTP_CLIENT.clone();
                let (meta, episodes) = podcast::fetch_feed(&client, &feed_url)?;
                let store = open_online_store()?;
                let id = store.add_podcast(&feed_url, &meta)?;
                store.upsert_episodes(id, &episodes)?;
                store.touch_podcast_refresh(id, &meta, now_epoch())?;
                Ok(meta.title)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::PodcastEvent(PodcastEvent::Subscribed { result }))
        })
    }

    fn refresh_podcast(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        let Some(podcast) = self.podcasts.iter().find(|p| p.id == id) else {
            return Task::none();
        };
        if !self.refreshing_podcasts.insert(id) {
            // Already refreshing — ignore the duplicate request.
            return Task::none();
        }
        refresh_podcast_task(id, podcast.feed_url.clone())
    }

    fn refresh_all_podcasts(&mut self) -> Task<cosmic::Action<Message>> {
        let ids: Vec<i64> = self.podcasts.iter().map(|p| p.id).collect();
        Task::batch(ids.into_iter().map(|id| self.refresh_podcast(id)).collect::<Vec<_>>())
    }

    /// Unsubscribe from a podcast: drop it from the in-memory list right
    /// away (so the row disappears immediately) and delete it (plus its
    /// downloaded episode files) from the store. Deliberately not
    /// undo-able: unlike a saved radio station, a podcast's per-episode
    /// playback positions and downloads would be permanently lost by the
    /// cascade delete, so the view asks for confirmation up front instead
    /// (see `PodcastMessage::StartUnsubscribe`/`ConfirmUnsubscribe`).
    fn unsubscribe_podcast(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        self.pending_unsubscribe_podcast = None;
        if !self.podcasts.iter().any(|p| p.id == id) {
            return Task::none();
        }
        self.podcasts.retain(|p| p.id != id);
        if self.selected_podcast == Some(id) {
            self.selected_podcast = None;
            self.podcast_episodes.clear();
        }
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                let store = open_online_store()?;
                let episodes = store.list_episodes(id)?;
                for episode in episodes {
                    if !episode.downloaded_path.is_empty() {
                        let _ = std::fs::remove_file(&episode.downloaded_path);
                    }
                }
                store.remove_podcast(id)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::PodcastEvent(PodcastEvent::Unsubscribed { result }))
        })
    }

    fn dispatch_podcast_search(&mut self) -> Task<cosmic::Action<Message>> {
        let query = self.podcast_search_query.trim().to_string();
        if query.is_empty() {
            return Task::none();
        }
        self.podcast_search_generation += 1;
        let generation = self.podcast_search_generation;
        self.podcast_search_loading = true;
        self.podcast_search_error = None;
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                let client = HTTP_CLIENT.clone();
                podcast::search_itunes(&client, &query)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::PodcastEvent(PodcastEvent::SearchResults { generation, result }))
        })
    }

    /// Play (or, if already the current episode, toggle play/pause) an
    /// episode. Resumes from its saved position unless it was already
    /// marked fully played (a finished episode restarts from the top).
    fn play_episode(&mut self, episode_id: i64) -> Task<cosmic::Action<Message>> {
        let already_current = self.current_podcast_episode_id == Some(episode_id)
            && self.current_track.as_ref().is_some_and(|t| &*t.provider_id == "podcast");
        if already_current {
            if let Some(player) = &mut self.player {
                let _ = player.toggle_playback();
            }
            return Task::none();
        }

        let Some(podcast_id) = self.selected_podcast else {
            return Task::none();
        };
        let Some(episode) = self.podcast_episodes.iter().find(|e| e.id == episode_id).cloned() else {
            return Task::none();
        };
        let Some(show) = self.podcasts.iter().find(|p| p.id == podcast_id) else {
            return Task::none();
        };

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

        let play_task = self.play_track_list(vec![track], 0);
        self.current_podcast_episode_id = Some(episode.id);
        self.last_saved_podcast_position_secs = 0;
        if episode.position_ms > 0 && !episode.played {
            let resume_at = Duration::from_millis(episode.position_ms as u64);
            if let Some(player) = &mut self.player {
                let _ = player.seek(resume_at);
            }
            self.playback_position = resume_at;
            self.last_saved_podcast_position_secs = resume_at.as_secs();
        }
        play_task
    }

    /// Toggle an episode's played marker. Updates the in-memory copy
    /// immediately (instant UI feedback) and persists in the background.
    fn toggle_episode_played(&mut self, episode_id: i64) -> Task<cosmic::Action<Message>> {
        let Some(episode) = self.podcast_episodes.iter_mut().find(|e| e.id == episode_id) else {
            return Task::none();
        };
        episode.played = !episode.played;
        let new_played = episode.played;
        let position_ms = episode.position_ms;
        self.save_podcast_position(episode_id, position_ms, new_played)
    }

    fn download_episode(&mut self, episode_id: i64) -> Task<cosmic::Action<Message>> {
        let Some(episode) = self.podcast_episodes.iter().find(|e| e.id == episode_id).cloned() else {
            return Task::none();
        };
        if !self.downloading_episodes.insert(episode.id) {
            // Already downloading — ignore the duplicate request.
            return Task::none();
        }
        download_episode_task(episode)
    }

    fn delete_episode_download(&mut self, episode_id: i64) -> Task<cosmic::Action<Message>> {
        let Some(episode) = self.podcast_episodes.iter().find(|e| e.id == episode_id).cloned() else {
            return Task::none();
        };
        if episode.downloaded_path.is_empty() {
            return Task::none();
        }
        let path = episode.downloaded_path.clone();
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                let _ = std::fs::remove_file(&path);
                open_online_store()?.set_episode_downloaded_path(episode_id, "")
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::PodcastEvent(PodcastEvent::DownloadDeleted { episode_id, result }))
        })
    }
}
