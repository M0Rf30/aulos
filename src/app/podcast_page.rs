// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Podcasts page controller.
//!
//! Maps the `Message::Podcast` UI events (`update_podcast`) onto
//! [`aulos_core::session::PodcastsState`], which owns the workflows and the
//! open show, and keeps what is UI-only: the add/search form inputs, the
//! show's filters and the notice texts. The async results are session
//! events (`Message::Session`); icons are fetched by the session. The view
//! (`crate::views::podcasts`) stays pure -- this is the only place that
//! touches `AppModel`'s `podcast_*` fields.

use super::tasks::session_effect;
use super::{AppModel, Message};
use crate::views::podcasts::PodcastMessage;
use aulos_core::fl;
use aulos_core::online::UrlError;
use aulos_core::online::podcast::EpisodeFilter;
use aulos_core::session::{EpisodePlayback, PodcastNotice};
use cosmic::prelude::*;

/// The toast text of a podcast notice.
pub(super) fn notice_text(notice: PodcastNotice) -> String {
    match notice {
        PodcastNotice::Subscribed { name } => fl!("toast-podcast-subscribed", name = name),
        PodcastNotice::SubscribeFailed { reason } => {
            fl!("toast-podcast-subscribe-failed", reason = reason)
        }
        PodcastNotice::RefreshFailed { reason } => {
            fl!("toast-podcast-refresh-failed", reason = reason)
        }
        PodcastNotice::UnsubscribeFailed { reason } => {
            fl!("toast-podcast-unsubscribe-failed", reason = reason)
        }
        PodcastNotice::DownloadFailed { reason } => {
            fl!("toast-episode-download-failed", reason = reason)
        }
    }
}

impl AppModel {
    /// Load subscribed podcasts from the online store.
    pub(super) fn load_podcasts(&self) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.podcasts.load())
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
            PodcastMessage::RefreshPodcast(id) => session_effect(self.session.podcasts.refresh(id)),
            PodcastMessage::RefreshAll => session_effect(self.session.podcasts.refresh_all()),
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
            PodcastMessage::SearchSubmit | PodcastMessage::RetrySearch => {
                let query = self.podcast_search_query.trim().to_string();
                session_effect(self.session.podcasts.search(query))
            }
            PodcastMessage::SubscribeFromSearch(feed_url) => self.submit_add_podcast(feed_url),

            PodcastMessage::BackToList => {
                self.session.podcasts.close_show();
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
            PodcastMessage::TogglePlayed(id) => {
                session_effect(self.session.podcasts.toggle_played(id))
            }
            PodcastMessage::Download(id) => session_effect(self.session.podcasts.download(id)),
            PodcastMessage::DeleteDownload(id) => {
                session_effect(self.session.podcasts.delete_download(id))
            }
        }
    }

    /// A podcast was subscribed: the add form has done its job.
    pub(super) fn reset_podcast_form(&mut self) {
        self.podcast_add_url.clear();
        self.podcast_add_open = false;
        self.podcast_add_error = None;
    }

    fn select_podcast(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        self.podcast_episode_filter = EpisodeFilter::default();
        self.podcast_episode_text_filter.clear();
        self.podcast_description_expanded = false;
        self.pending_unsubscribe_podcast = None;
        session_effect(self.session.podcasts.select(id))
    }

    /// Shared by add-by-URL and Subscribe-from-Discover: a rejected URL
    /// shows its error in the add form, otherwise the subscription starts.
    fn submit_add_podcast(&mut self, feed_url: String) -> Task<cosmic::Action<Message>> {
        match self.session.podcasts.subscribe(feed_url) {
            Ok(subscribe) => {
                self.podcast_add_error = None;
                session_effect(subscribe)
            }
            Err(UrlError::Empty) => {
                self.podcast_add_error = Some(fl!("podcast-add-url-required"));
                Task::none()
            }
            Err(UrlError::NotHttp) => {
                self.podcast_add_error = Some(fl!("podcast-add-url-invalid"));
                Task::none()
            }
        }
    }

    fn unsubscribe_podcast(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        self.pending_unsubscribe_podcast = None;
        let Some(unsubscribe) = self.session.podcasts.unsubscribe(id) else {
            return Task::none();
        };
        session_effect(unsubscribe)
    }

    /// Play (or, if already the current episode, toggle play/pause) an
    /// episode, resuming from its saved position.
    fn play_episode(&mut self, episode_id: i64) -> Task<cosmic::Action<Message>> {
        let decision = self
            .session
            .podcasts
            .episode_playback(episode_id, self.session.playback.current_track.as_ref());
        match decision {
            None => Task::none(),
            Some(EpisodePlayback::Toggle) => {
                if let Some(player) = &mut self.session.playback.player {
                    let _ = player.toggle_playback();
                }
                Task::none()
            }
            Some(EpisodePlayback::Play {
                episode_id,
                track,
                resume,
            }) => {
                let play_task = self.play_track_list(vec![*track], 0);
                self.session.podcasts.start_episode(episode_id, resume);
                if let Some(resume_at) = resume {
                    if let Some(player) = &mut self.session.playback.player {
                        let _ = player.seek(resume_at);
                    }
                    self.session.playback.position = resume_at;
                }
                play_task
            }
        }
    }
}
