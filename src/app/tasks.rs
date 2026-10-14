// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::Message;
use aulos_core::online::podcast;
use aulos_core::online::radio;
use aulos_core::online::store::Episode;
use aulos_core::task::Effect;
use cosmic::prelude::*;
use std::path::PathBuf;

/// Runs a backend [`Effect`] as an iced task.
pub(super) fn effect<M: Send + 'static>(effect: Effect<M>) -> Task<M> {
    match effect.into_stream() {
        Some(stream) => Task::stream(stream),
        None => Task::none(),
    }
}

/// Re-fetch a podcast's feed and update its metadata/episodes in the
/// online store, dispatching `PodcastEvent::Refreshed` with the outcome.
pub(super) fn refresh_podcast_task(id: i64, feed_url: String) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let result = tokio::task::spawn_blocking(move || podcast::refresh(id, &feed_url))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        cosmic::Action::App(Message::PodcastEvent(
            super::podcast_page::PodcastEvent::Refreshed { id, result },
        ))
    })
}

/// Download an episode's enclosure for offline playback (see
/// [`podcast::download_episode`]), dispatching `PodcastEvent::Downloaded`
/// with the outcome.
pub(super) fn download_episode_task(episode: Episode) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let episode_id = episode.id;
        let result = tokio::task::spawn_blocking(move || podcast::download_episode(&episode))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        cosmic::Action::App(Message::PodcastEvent(
            super::podcast_page::PodcastEvent::Downloaded { episode_id, result },
        ))
    })
}

/// Resolve a station URL (following a `.pls`/`.m3u`/`.m3u8` playlist if
/// needed) and dispatch `RadioEvent::StreamResolved` with the outcome.
/// `favicon` and `key` are threaded through unresolved: `favicon` so the
/// now-playing strip can show it once playback actually starts, `key`
/// (the pre-resolution stream/result URL) so the "currently playing" row
/// indicator keeps matching even after resolution rewrites the actual
/// stream URL (e.g. a `.pls` playlist resolving to a different host).
pub(super) fn resolve_and_play_radio(
    name: String,
    favicon: String,
    key: String,
    url: String,
) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let result = tokio::task::spawn_blocking(move || radio::resolve_stream(&url))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        cosmic::Action::App(Message::RadioEvent(
            super::radio_page::RadioEvent::StreamResolved {
                name,
                favicon,
                key,
                result,
            },
        ))
    })
}

/// Resolves a track's MPRIS cover-art URL off the update-loop thread —
/// [`aulos_core::mpris::extract_art_url`] parses tags and writes to an on-disk
/// cache synchronously — then dispatches `Message::MprisArtResolved` so
/// `publish_mpris` can republish with the real URL once it resolves.
pub(super) fn resolve_mpris_art_task(
    track_id: i64,
    path: PathBuf,
) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let art_url = tokio::task::spawn_blocking(move || {
            aulos_core::mpris::extract_art_url(track_id, &path)
        })
        .await
        .unwrap_or(None);
        cosmic::Action::App(Message::MprisArtResolved(track_id, art_url))
    })
}
