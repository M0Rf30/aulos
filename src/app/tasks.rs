// SPDX-License-Identifier: GPL-3.0

use super::{HTTP_CLIENT, Message, now_epoch, open_online_store};
use crate::online::podcast;
use crate::online::radio;
use crate::online::store::Episode;
use cosmic::prelude::*;
use std::path::PathBuf;

/// Re-fetch a podcast's feed and update its metadata/episodes in the
/// online store, dispatching `PodcastEvent::Refreshed` with the outcome.
pub(super) fn refresh_podcast_task(id: i64, feed_url: String) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let result = tokio::task::spawn_blocking(move || {
            let client = HTTP_CLIENT.clone();
            let (meta, episodes) = podcast::fetch_feed(&client, &feed_url)?;
            let store = open_online_store()?;
            store.touch_podcast_refresh(id, &meta, now_epoch())?;
            store.upsert_episodes(id, &episodes)?;
            Ok(())
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        cosmic::Action::App(Message::PodcastEvent(super::podcast_page::PodcastEvent::Refreshed {
            id,
            result,
        }))
    })
}

/// Infer the file extension for a downloaded episode from its MIME type,
/// falling back to the enclosure URL's own extension, then `mp3`.
fn episode_file_extension(mime: &str, enclosure_url: &str) -> String {
    match mime {
        "audio/mpeg" => return "mp3".to_string(),
        "audio/mp4" | "audio/x-m4a" => return "m4a".to_string(),
        "audio/ogg" => return "ogg".to_string(),
        "audio/flac" => return "flac".to_string(),
        "audio/wav" => return "wav".to_string(),
        _ => {}
    }
    let path_part = enclosure_url
        .split(['?', '#'])
        .next()
        .unwrap_or(enclosure_url);
    std::path::Path::new(path_part)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "mp3".to_string())
}

/// Download an episode's enclosure to `dirs::data_dir()/lyra/podcast_downloads`
/// for offline playback, persisting the resulting path via
/// `OnlineStore::set_episode_downloaded_path` and dispatching
/// `PodcastEvent::Downloaded` with the outcome. Mirrors `refresh_podcast_task`'s
/// blocking-task idiom.
pub(super) fn download_episode_task(episode: Episode) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let episode_id = episode.id;
        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let dir = dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("lyra")
                .join("podcast_downloads");
            std::fs::create_dir_all(&dir).map_err(|e| format!("Create download dir error: {e}"))?;
            let ext = episode_file_extension(&episode.mime, &episode.enclosure_url);
            let dest = dir.join(format!("{episode_id}.{ext}"));

            let write_result = (|| -> Result<(), String> {
                let client = HTTP_CLIENT.clone();
                let mut response = client
                    .get(&episode.enclosure_url)
                    .send()
                    .map_err(|e| format!("Download request error: {e}"))?
                    .error_for_status()
                    .map_err(|e| format!("Download response error: {e}"))?;
                let mut file = std::fs::File::create(&dest)
                    .map_err(|e| format!("Create download file error: {e}"))?;
                std::io::copy(&mut response, &mut file)
                    .map_err(|e| format!("Write download error: {e}"))?;
                Ok(())
            })();

            if let Err(e) = write_result {
                let _ = std::fs::remove_file(&dest);
                return Err(e);
            }

            let path = dest.to_string_lossy().into_owned();
            open_online_store()?.set_episode_downloaded_path(episode_id, &path)?;
            Ok(path)
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        cosmic::Action::App(Message::PodcastEvent(super::podcast_page::PodcastEvent::Downloaded {
            episode_id,
            result,
        }))
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
        let result = tokio::task::spawn_blocking(move || {
            let client = HTTP_CLIENT.clone();
            radio::resolve_stream_url(&client, &url)
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        cosmic::Action::App(Message::RadioEvent(super::radio_page::RadioEvent::StreamResolved {
            name,
            favicon,
            key,
            result,
        }))
    })
}

/// Resolves a track's MPRIS cover-art URL off the update-loop thread —
/// [`crate::mpris::extract_art_url`] parses tags and writes to an on-disk
/// cache synchronously — then dispatches `Message::MprisArtResolved` so
/// `publish_mpris` can republish with the real URL once it resolves.
pub(super) fn resolve_mpris_art_task(track_id: i64, path: PathBuf) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let art_url =
            tokio::task::spawn_blocking(move || crate::mpris::extract_art_url(track_id, &path))
                .await
                .unwrap_or(None);
        cosmic::Action::App(Message::MprisArtResolved(track_id, art_url))
    })
}
