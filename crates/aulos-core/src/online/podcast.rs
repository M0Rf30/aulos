// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Podcast feed fetching (via `feed-rs`) and the iTunes podcast directory
//! search used by the "Add podcast" search field, plus the blocking
//! subscribe / refresh / unsubscribe / download operations that combine them
//! with the [`OnlineStore`]. Network calls are async (so they run in the
//! browser too) and store access runs on the blocking pool; only
//! [`download_episode`] blocks and is native-only in practice.

use super::store::{Episode, OnlineStore, Podcast};
use feed_rs::model::{Entry, Feed};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};

/// Podcast-level metadata extracted from a feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodcastMeta {
    pub title: String,
    pub description: String,
    pub image_url: String,
    pub author: String,
}

/// A single episode extracted from a feed entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeMeta {
    pub guid: String,
    pub title: String,
    pub enclosure_url: String,
    pub mime: String,
    pub duration_secs: i64,
    pub pub_date: i64,
    pub description: String,
}

/// Cap on a fetched podcast feed body. Legitimate feeds (even
/// long-running shows with thousands of episodes) are well under this; a
/// hostile or broken server serving an unbounded body must not be read
/// into memory in full.
const MAX_FEED_RESPONSE_BYTES: u64 = 20 * 1024 * 1024;

/// Cap on the number of episodes kept from a single feed fetch, as a
/// second line of defense against a pathological feed with an enormous
/// entry count (bounding memory and the size of the follow-up DB upsert).
const MAX_EPISODES_PER_FEED: usize = 5_000;

/// Fetch and parse a podcast feed, returning its metadata and episodes.
pub async fn fetch_feed(
    client: &reqwest::Client,
    feed_url: &str,
) -> Result<(PodcastMeta, Vec<EpisodeMeta>), String> {
    let response = client
        .get(feed_url)
        .send()
        .await
        .map_err(|e| super::request_error("Failed to fetch feed", feed_url, e))?;
    if !response.status().is_success() {
        return Err(format!("Failed to fetch feed: HTTP {}", response.status()));
    }
    let bytes = super::read_capped_response(response, MAX_FEED_RESPONSE_BYTES).await?;
    let feed =
        feed_rs::parser::parse(&bytes[..]).map_err(|e| format!("Failed to parse feed: {e}"))?;

    let meta = feed_to_podcast_meta(&feed);
    let episodes = feed
        .entries
        .iter()
        .filter_map(entry_to_episode_meta)
        .take(MAX_EPISODES_PER_FEED)
        .collect();
    Ok((meta, episodes))
}

/// Map a parsed [`Feed`] to podcast-level metadata.
pub fn feed_to_podcast_meta(feed: &Feed) -> PodcastMeta {
    PodcastMeta {
        title: feed
            .title
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_default(),
        description: feed
            .description
            .as_ref()
            .map(|t| t.content.clone())
            .unwrap_or_default(),
        image_url: feed
            .logo
            .as_ref()
            .map(|i| i.uri.clone())
            .or_else(|| feed.icon.as_ref().map(|i| i.uri.clone()))
            .unwrap_or_default(),
        author: feed
            .authors
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_default(),
    }
}

/// Locate the audio enclosure for an entry: first from a MediaRSS/itunes
/// `media:content` object (how feed-rs represents RSS2 `<enclosure>` and
/// `<itunes:duration>`), falling back to an Atom `<link rel="enclosure">`.
/// Returns `(url, mime, duration_secs)`.
fn find_enclosure(entry: &Entry) -> Option<(String, String, i64)> {
    for media in &entry.media {
        for content in &media.content {
            if let Some(url) = &content.url {
                let mime = content
                    .content_type
                    .as_ref()
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                // `try_into` + clamp instead of a raw `as` cast: an
                // absurd/hostile `<itunes:duration>` value near u64::MAX
                // would otherwise wrap around to a negative duration.
                let duration = media
                    .duration
                    .map(|d| d.as_secs().try_into().unwrap_or(i64::MAX))
                    .unwrap_or(0);
                return Some((url.to_string(), mime, duration));
            }
        }
    }
    entry
        .links
        .iter()
        .find(|link| link.rel.as_deref() == Some("enclosure"))
        .map(|link| {
            (
                link.href.clone(),
                link.media_type.clone().unwrap_or_default(),
                0,
            )
        })
}

/// `entry.id` when present, otherwise the enclosure URL. Defensive only:
/// feed-rs itself always synthesizes a non-empty entry id (a hash of the
/// first link/title, or a UUID) even when the source feed has no explicit
/// `<guid>`, so this fallback should never actually trigger in practice.
fn resolve_guid(entry_id: &str, enclosure_url: &str) -> String {
    if entry_id.is_empty() {
        enclosure_url.to_string()
    } else {
        entry_id.to_string()
    }
}

/// Map a feed [`Entry`] to episode metadata. Returns `None` when the entry
/// has no usable audio enclosure.
pub fn entry_to_episode_meta(entry: &Entry) -> Option<EpisodeMeta> {
    let (enclosure_url, mime, duration_secs) = find_enclosure(entry)?;
    if enclosure_url.is_empty() {
        return None;
    }
    let guid = resolve_guid(&entry.id, &enclosure_url);
    let title = entry
        .title
        .as_ref()
        .map(|t| t.content.clone())
        .unwrap_or_default();
    let description = entry
        .summary
        .as_ref()
        .map(|t| t.content.clone())
        .or_else(|| entry.content.as_ref().and_then(|c| c.body.clone()))
        .unwrap_or_default();
    let pub_date = entry
        .published
        .or(entry.updated)
        .map(|d| d.timestamp())
        .unwrap_or(0);

    Some(EpisodeMeta {
        guid,
        title,
        enclosure_url,
        mime,
        duration_secs,
        pub_date,
        description,
    })
}

/// A podcast directory search result from the iTunes Search API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodcastSearchResult {
    pub title: String,
    pub feed_url: String,
    pub image: String,
    pub author: String,
}

#[derive(Debug, Deserialize)]
struct ItunesResponse {
    #[serde(default)]
    results: Vec<ItunesResultRaw>,
}

#[derive(Debug, Deserialize)]
struct ItunesResultRaw {
    #[serde(default, rename = "collectionName")]
    collection_name: String,
    #[serde(default, rename = "feedUrl")]
    feed_url: Option<String>,
    #[serde(default, rename = "artworkUrl600")]
    artwork_url_600: Option<String>,
    #[serde(default, rename = "artworkUrl100")]
    artwork_url_100: Option<String>,
    #[serde(default, rename = "artistName")]
    artist_name: String,
}

/// Cap on the iTunes Search API JSON response.
const MAX_ITUNES_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// Search the iTunes podcast directory. Results without a `feedUrl` are
/// skipped since they can't be subscribed to.
pub async fn search_itunes(
    client: &reqwest::Client,
    query: &str,
) -> Result<Vec<PodcastSearchResult>, String> {
    let url = format!(
        "https://itunes.apple.com/search?media=podcast&term={}",
        urlencoding::encode(query)
    );
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| super::request_error("iTunes search failed", &url, e))?;
    if !response.status().is_success() {
        return Err(format!("iTunes search returned HTTP {}", response.status()));
    }
    let bytes = super::read_capped_response(response, MAX_ITUNES_RESPONSE_BYTES).await?;
    let body: ItunesResponse =
        serde_json::from_slice(&bytes).map_err(|e| format!("iTunes response parse failed: {e}"))?;
    Ok(map_itunes_results(body.results))
}

/// Searches the iTunes podcast directory with the shared HTTP client.
pub async fn search(query: &str) -> Result<Vec<PodcastSearchResult>, String> {
    search_itunes(super::http(), query).await
}

/// Loads the subscribed podcasts from the online store. A store error is
/// logged and reads as "no podcasts" so a broken database never blocks the
/// page from rendering.
#[must_use]
pub fn load_subscriptions() -> Vec<Podcast> {
    OnlineStore::open_default()
        .and_then(|store| store.list_podcasts())
        .unwrap_or_else(|e| {
            tracing::warn!("list_podcasts failed: {e}");
            Vec::new()
        })
}

/// Loads a podcast's episodes from the online store; errors are logged and
/// read as "no episodes" (see [`load_subscriptions`]).
#[must_use]
pub fn load_episodes(podcast_id: i64) -> Vec<Episode> {
    OnlineStore::open_default()
        .and_then(|store| store.list_episodes(podcast_id))
        .unwrap_or_else(|e| {
            tracing::warn!("list_episodes failed: {e}");
            Vec::new()
        })
}

/// Persists an episode's playback position and played marker.
pub fn save_episode_position(
    episode_id: i64,
    position_ms: i64,
    played: bool,
) -> Result<(), String> {
    OnlineStore::open_default()
        .and_then(|store| store.save_episode_position(episode_id, position_ms, played))
}

/// Subscribes to the podcast at `feed_url`: fetches and parses the feed,
/// then upserts the show and its episodes into the online store. Returns
/// the show's title for the confirmation toast.
pub async fn subscribe(feed_url: &str) -> Result<String, String> {
    let (meta, episodes) = fetch_feed(super::http(), feed_url).await?;
    let feed_url = feed_url.to_owned();
    OnlineStore::run(move |store| {
        let id = store.add_podcast(&feed_url, &meta)?;
        store.upsert_episodes(id, &episodes)?;
        store.touch_podcast_refresh(id, &meta, crate::unix_now())?;
        Ok(meta.title)
    })
    .await
}

/// Re-fetches a subscribed podcast's feed and updates its metadata and
/// episodes in the online store.
pub async fn refresh(id: i64, feed_url: &str) -> Result<(), String> {
    let (meta, episodes) = fetch_feed(super::http(), feed_url).await?;
    OnlineStore::run(move |store| {
        store.touch_podcast_refresh(id, &meta, crate::unix_now())?;
        store.upsert_episodes(id, &episodes)?;
        Ok(())
    })
    .await
}

/// Unsubscribes from a podcast: deletes its downloaded episode files, then
/// the podcast itself (its episodes cascade away in the store). Files in the
/// download directory are matched by episode id, so a transfer still in
/// flight (not yet recorded in the store) leaves nothing behind either; one
/// that finishes after the removal is deleted by [`download_episode`].
pub fn unsubscribe(id: i64) -> Result<(), String> {
    let store = OnlineStore::open_default()?;
    let episodes = store.list_episodes(id)?;
    for episode in &episodes {
        if !episode.downloaded_path.is_empty() {
            let _ = std::fs::remove_file(&episode.downloaded_path);
        }
    }
    store.remove_podcast(id)?;
    let ids: Vec<i64> = episodes.iter().map(|e| e.id).collect();
    remove_download_files(&crate::paths::data_dir().join("podcast_downloads"), &ids);
    Ok(())
}

/// Removes every `<episode id>.<ext>` file of `episode_ids` from `dir`.
fn remove_download_files(dir: &std::path::Path, episode_ids: &[i64]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_episode_file = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<i64>().ok())
            .is_some_and(|id| episode_ids.contains(&id));
        if is_episode_file {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Infers the file extension for a downloaded episode from its MIME type,
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

/// Why [`download_episode`] did not produce a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// The caller's cancel flag was raised mid-transfer; the partial file is
    /// gone.
    Cancelled,
    Failed(String),
}

/// Hard cap on one episode download (2 GiB).
const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Smallest number of bytes between two progress reports.
const PROGRESS_STEP_BYTES: u64 = 128 * 1024;

/// Downloads an episode's enclosure to `<data dir>/podcast_downloads` for
/// offline playback and records the resulting path in the online store. A
/// partial file is removed on failure or cancellation. `on_progress` gets
/// `(bytes received, total bytes if the server announced them)` after every
/// [`PROGRESS_STEP_BYTES`]; raising `cancel` stops the transfer at the next
/// chunk. Returns the downloaded file's path.
pub fn download_episode(
    episode: &Episode,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(u64, Option<u64>),
) -> Result<String, DownloadError> {
    let dir = crate::paths::data_dir().join("podcast_downloads");
    std::fs::create_dir_all(&dir)
        .map_err(|e| DownloadError::Failed(format!("Create download dir error: {e}")))?;
    let ext = episode_file_extension(&episode.mime, &episode.enclosure_url);
    let dest = dir.join(format!("{}.{ext}", episode.id));

    let write_result = (|| -> Result<(), DownloadError> {
        let mut response = super::HTTP_CLIENT
            .get(&episode.enclosure_url)
            .send()
            .map_err(|e| {
                DownloadError::Failed(format!("Download request error: {}", e.without_url()))
            })?
            .error_for_status()
            .map_err(|e| {
                DownloadError::Failed(format!("Download response error: {}", e.without_url()))
            })?;
        let total = response.content_length().filter(|&n| n > 0);
        let mut file = std::fs::File::create(&dest)
            .map_err(|e| DownloadError::Failed(format!("Create download file error: {e}")))?;
        copy_with_progress(&mut response, &mut file, total, cancel, &mut on_progress)
    })();

    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&dest);
        return Err(e);
    }

    let path = dest.to_string_lossy().into_owned();
    let recorded = OnlineStore::open_default()
        .and_then(|store| store.set_episode_downloaded_path(episode.id, &path));
    match recorded {
        Ok(true) => Ok(path),
        Ok(false) => {
            // The show was unsubscribed mid-transfer: nothing refers to the file.
            let _ = std::fs::remove_file(&dest);
            Err(DownloadError::Failed(
                "Download discarded: the episode is no longer subscribed".to_owned(),
            ))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&dest);
            Err(DownloadError::Failed(e))
        }
    }
}

/// Copies `reader` to `writer` in chunks, reporting progress and honouring
/// `cancel` between chunks.
fn copy_with_progress(
    reader: &mut impl std::io::Read,
    writer: &mut impl std::io::Write,
    total: Option<u64>,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(u64, Option<u64>),
) -> Result<(), DownloadError> {
    let mut buf = [0u8; 32 * 1024];
    let mut received = 0u64;
    let mut reported = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| DownloadError::Failed(format!("Read download error: {e}")))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .map_err(|e| DownloadError::Failed(format!("Write download error: {e}")))?;
        received += n as u64;
        if received > MAX_DOWNLOAD_BYTES {
            return Err(DownloadError::Failed(format!(
                "Download exceeded the {MAX_DOWNLOAD_BYTES} byte limit"
            )));
        }
        if received - reported >= PROGRESS_STEP_BYTES {
            reported = received;
            on_progress(received, total);
        }
    }
    on_progress(received, total.or(Some(received)));
    Ok(())
}

/// Deletes a downloaded episode file (if still present) and clears its
/// recorded path in the online store.
pub fn delete_download(episode_id: i64, path: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    OnlineStore::open_default()?
        .set_episode_downloaded_path(episode_id, "")
        .map(|_| ())
}

fn map_itunes_results(results: Vec<ItunesResultRaw>) -> Vec<PodcastSearchResult> {
    results
        .into_iter()
        .filter_map(|r| {
            let feed_url = r.feed_url?;
            Some(PodcastSearchResult {
                title: r.collection_name,
                feed_url,
                image: r.artwork_url_600.or(r.artwork_url_100).unwrap_or_default(),
                author: r.artist_name,
            })
        })
        .collect()
}

/// Which of the two Podcasts tabs is shown. Kept in `AppModel` for the
/// session (not persisted to disk).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PodcastTab {
    #[default]
    Subscriptions,
    Discover,
}

/// Episode-list category filter, applied alongside the free-text filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EpisodeFilter {
    #[default]
    All,
    Unplayed,
    Downloaded,
}

impl Episode {
    /// Whether the episode passes the category filter and the free-text
    /// filter (case-insensitive match on title or description).
    #[must_use]
    pub fn matches(&self, filter: EpisodeFilter, text: &str) -> bool {
        let category_ok = match filter {
            EpisodeFilter::All => true,
            EpisodeFilter::Unplayed => !self.played,
            EpisodeFilter::Downloaded => !self.downloaded_path.is_empty(),
        };
        if !category_ok {
            return false;
        }
        let needle = text.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        self.title.to_lowercase().contains(&needle)
            || self.description.to_lowercase().contains(&needle)
    }

    /// Progress fraction (0.0-1.0) for the playback progress bar.
    /// Fully played episodes always report 1.0 regardless of the last saved
    /// position; an unknown/zero duration (missing feed metadata) always
    /// reports 0.0 rather than dividing by zero.
    #[must_use]
    pub fn progress_fraction(&self) -> f32 {
        if self.played {
            return 1.0;
        }
        if self.duration_secs <= 0 || self.position_ms <= 0 {
            return 0.0;
        }
        let duration_ms = self.duration_secs as f64 * 1000.0;
        ((self.position_ms as f64) / duration_ms).clamp(0.0, 1.0) as f32
    }

    /// Whether the episode has a meaningful saved position to resume from
    /// (partway through, not already marked played).
    #[must_use]
    pub fn should_show_resume(&self) -> bool {
        !self.played && self.position_ms > 0
    }

    /// `pub_date` (epoch seconds) as `YYYY-MM-DD`, or an empty string when unset.
    #[must_use]
    pub fn formatted_pub_date(&self) -> String {
        format_epoch_date(self.pub_date).unwrap_or_default()
    }
}

/// Pick the episode the hero's primary button acts on: the most recent
/// partly-played one ("Continue listening"), else the newest episode
/// ("Play latest").
#[must_use]
pub fn primary_episode(episodes: &[Episode]) -> Option<(&Episode, bool)> {
    episodes
        .iter()
        .find(|e| e.should_show_resume())
        .map(|e| (e, true))
        .or_else(|| episodes.first().map(|e| (e, false)))
}

/// How often a show publishes, derived from its episode dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    Daily,
    TwiceWeekly,
    Weekly,
    Biweekly,
    Monthly,
}

/// How many of the newest episodes [`Cadence::from_pub_dates`] looks at.
pub const CADENCE_SAMPLE: usize = 12;

impl Cadence {
    /// The cadence of a show whose episodes were published at `pub_dates`
    /// (epoch seconds, any order, `0` = unknown and ignored): the median gap
    /// between the newest [`CADENCE_SAMPLE`] episodes, snapped to the
    /// nearest regular schedule. `None` with fewer than three dated
    /// episodes or an irregular schedule.
    #[must_use]
    pub fn from_pub_dates(pub_dates: &[i64]) -> Option<Self> {
        const DAY: f64 = 86_400.0;
        let mut dates: Vec<i64> = pub_dates.iter().copied().filter(|&d| d > 0).collect();
        dates.sort_unstable_by(|a, b| b.cmp(a));
        dates.truncate(CADENCE_SAMPLE);
        if dates.len() < 3 {
            return None;
        }
        let mut gaps: Vec<i64> = dates.windows(2).map(|w| w[0] - w[1]).collect();
        gaps.sort_unstable();
        let mid = gaps.len() / 2;
        let median = if gaps.len() % 2 == 1 {
            gaps[mid] as f64
        } else {
            (gaps[mid - 1] + gaps[mid]) as f64 / 2.0
        } / DAY;
        match median {
            d if d < 0.5 => None,
            d if d <= 1.5 => Some(Self::Daily),
            d if (2.0..=4.5).contains(&d) => Some(Self::TwiceWeekly),
            d if (5.5..=9.0).contains(&d) => Some(Self::Weekly),
            d if (11.0..=17.0).contains(&d) => Some(Self::Biweekly),
            d if (26.0..=35.0).contains(&d) => Some(Self::Monthly),
            _ => None,
        }
    }
}

/// A subscribed show's `last_refreshed` (epoch seconds), formatted as a
/// localized "Updated <date>" caption, or "Never updated" when it hasn't
/// been refreshed yet.
#[must_use]
pub fn format_last_refreshed(epoch_secs: i64) -> String {
    match format_epoch_date(epoch_secs) {
        Some(date) => crate::fl!("podcast-updated-at", when = date),
        None => crate::fl!("podcast-never-updated"),
    }
}

/// Epoch seconds to `YYYY-MM-DD`; `None` for a non-positive/unset epoch.
fn format_epoch_date(epoch_secs: i64) -> Option<String> {
    if epoch_secs <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(epoch_secs, 0).map(|dt| dt.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(
        played: bool,
        position_ms: i64,
        duration_secs: i64,
        downloaded_path: &str,
        title: &str,
        description: &str,
    ) -> Episode {
        Episode {
            id: 1,
            podcast_id: 1,
            guid: "guid".to_string(),
            title: title.to_string(),
            enclosure_url: String::new(),
            mime: String::new(),
            duration_secs,
            pub_date: 0,
            description: description.to_string(),
            position_ms,
            played,
            downloaded_path: downloaded_path.to_string(),
        }
    }

    #[test]
    fn progress_fraction_is_zero_for_untouched_episode() {
        assert_eq!(ep(false, 0, 600, "", "t", "").progress_fraction(), 0.0);
    }

    #[test]
    fn progress_fraction_reflects_partial_position() {
        let e = ep(false, 150_000, 600, "", "t", ""); // 150s of 600s = 25%
        assert!((e.progress_fraction() - 0.25).abs() < 0.001);
    }

    #[test]
    fn progress_fraction_clamps_when_position_exceeds_duration() {
        let e = ep(false, 900_000, 600, "", "t", "");
        assert_eq!(e.progress_fraction(), 1.0);
    }

    #[test]
    fn progress_fraction_is_one_when_marked_played_even_with_no_position() {
        assert_eq!(ep(true, 0, 600, "", "t", "").progress_fraction(), 1.0);
    }

    #[test]
    fn progress_fraction_is_zero_for_unknown_duration() {
        assert_eq!(ep(false, 1000, 0, "", "t", "").progress_fraction(), 0.0);
    }

    #[test]
    fn should_show_resume_true_only_when_partway_and_not_played() {
        assert!(ep(false, 1000, 600, "", "t", "").should_show_resume());
        assert!(!ep(true, 1000, 600, "", "t", "").should_show_resume());
        assert!(!ep(false, 0, 600, "", "t", "").should_show_resume());
    }

    #[test]
    fn matches_filters_by_category() {
        let unplayed = ep(false, 0, 600, "", "Title", "desc");
        let played = ep(true, 0, 600, "", "Title", "desc");
        let downloaded = ep(false, 0, 600, "/tmp/x.mp3", "Title", "desc");
        assert!(unplayed.matches(EpisodeFilter::All, ""));
        assert!(unplayed.matches(EpisodeFilter::Unplayed, ""));
        assert!(!played.matches(EpisodeFilter::Unplayed, ""));
        assert!(downloaded.matches(EpisodeFilter::Downloaded, ""));
        assert!(!unplayed.matches(EpisodeFilter::Downloaded, ""));
    }

    #[test]
    fn matches_filters_by_text_case_insensitively() {
        let e = ep(false, 0, 600, "", "Rust Talk Show", "About programming");
        assert!(e.matches(EpisodeFilter::All, "rust"));
        assert!(e.matches(EpisodeFilter::All, "PROGRAMMING"));
        assert!(!e.matches(EpisodeFilter::All, "python"));
    }

    #[test]
    fn matches_combines_category_and_text_filters() {
        let played_match = ep(true, 0, 600, "", "Rust Talk", "");
        assert!(!played_match.matches(EpisodeFilter::Unplayed, "rust"));
    }

    #[test]
    fn formatted_pub_date_formats_epoch_seconds_or_empty() {
        let mut e = ep(false, 0, 600, "", "t", "");
        e.pub_date = 1704067200;
        assert_eq!(e.formatted_pub_date(), "2024-01-01");
        e.pub_date = 0;
        assert_eq!(e.formatted_pub_date(), "");
        e.pub_date = -10;
        assert_eq!(e.formatted_pub_date(), "");
    }

    #[test]
    fn format_epoch_date_reports_none_when_unset_or_negative() {
        assert_eq!(format_epoch_date(0), None);
        assert_eq!(format_epoch_date(-5), None);
        assert_eq!(
            format_epoch_date(1704067200),
            Some("2024-01-01".to_string())
        );
    }

    const FEED_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd">
  <channel>
    <title>Test Podcast</title>
    <description>A show about tests</description>
    <image><url>https://example.com/art.png</url></image>
    <itunes:author>Test Author</itunes:author>
    <item>
      <title>Episode One</title>
      <guid>ep-1-guid</guid>
      <pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate>
      <description>First episode</description>
      <enclosure url="https://example.com/ep1.mp3" type="audio/mpeg" length="123456" />
      <itunes:duration>01:02:03</itunes:duration>
    </item>
    <item>
      <title>Episode Two (no guid)</title>
      <pubDate>Tue, 02 Jan 2024 00:00:00 GMT</pubDate>
      <enclosure url="https://example.com/ep2.mp3" type="audio/mpeg" length="654321" />
    </item>
  </channel>
</rss>"#;

    #[test]
    fn feed_metadata_maps_title_description_image_and_author() {
        let feed = feed_rs::parser::parse(FEED_XML.as_bytes()).unwrap();
        let meta = feed_to_podcast_meta(&feed);
        assert_eq!(meta.title, "Test Podcast");
        assert_eq!(meta.description, "A show about tests");
        assert_eq!(meta.image_url, "https://example.com/art.png");
        assert_eq!(meta.author, "Test Author");
    }

    #[test]
    fn entry_maps_enclosure_guid_and_itunes_duration() {
        let feed = feed_rs::parser::parse(FEED_XML.as_bytes()).unwrap();
        let ep = entry_to_episode_meta(&feed.entries[0]).expect("enclosure present");
        assert_eq!(ep.guid, "ep-1-guid");
        assert_eq!(ep.title, "Episode One");
        assert_eq!(ep.enclosure_url, "https://example.com/ep1.mp3");
        assert_eq!(ep.mime, "audio/mpeg");
        // 01:02:03 -> 3723 seconds.
        assert_eq!(ep.duration_secs, 3723);
        assert_eq!(ep.pub_date, 1704067200);
        assert_eq!(ep.description, "First episode");
    }

    #[test]
    fn resolve_guid_falls_back_to_enclosure_url_when_id_is_empty() {
        assert_eq!(
            resolve_guid("", "https://example.com/ep2.mp3"),
            "https://example.com/ep2.mp3"
        );
        assert_eq!(
            resolve_guid("ep-1-guid", "https://example.com/ep1.mp3"),
            "ep-1-guid"
        );
    }

    #[test]
    fn entry_without_explicit_guid_still_gets_a_nonempty_id() {
        // feed-rs synthesizes an id when `<guid>` is absent (see
        // `resolve_guid`'s doc comment) — assert that behavior end to end.
        let feed = feed_rs::parser::parse(FEED_XML.as_bytes()).unwrap();
        let ep = entry_to_episode_meta(&feed.entries[1]).expect("enclosure present");
        assert!(!ep.guid.is_empty());
        assert_eq!(ep.duration_secs, 0);
    }

    #[test]
    fn episode_extension_prefers_mime_then_url_then_mp3() {
        assert_eq!(
            episode_file_extension("audio/x-m4a", "https://x/a.mp3"),
            "m4a"
        );
        assert_eq!(
            episode_file_extension("application/octet-stream", "https://x/a.ogg?token=1#t"),
            "ogg"
        );
        assert_eq!(episode_file_extension("", "https://x/stream"), "mp3");
    }

    #[test]
    fn itunes_results_without_feed_url_are_skipped() {
        const BODY: &str = r#"{
            "resultCount": 2,
            "results": [
                {"collectionName": "Has Feed", "feedUrl": "https://feed.example/rss",
                 "artworkUrl600": "https://example.com/600.png", "artistName": "Alice"},
                {"collectionName": "No Feed", "artistName": "Bob"}
            ]
        }"#;
        let parsed: ItunesResponse = serde_json::from_str(BODY).unwrap();
        let results = map_itunes_results(parsed.results);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Has Feed");
        assert_eq!(results[0].feed_url, "https://feed.example/rss");
        assert_eq!(results[0].image, "https://example.com/600.png");
        assert_eq!(results[0].author, "Alice");
    }

    fn dates_every(days: f64, n: usize) -> Vec<i64> {
        (0..n)
            .map(|i| 1_700_000_000 + (i as f64 * days * 86_400.0) as i64)
            .collect()
    }

    #[test]
    fn cadence_snaps_to_regular_schedules() {
        assert_eq!(
            Cadence::from_pub_dates(&dates_every(1.0, 10)),
            Some(Cadence::Daily)
        );
        assert_eq!(
            Cadence::from_pub_dates(&dates_every(3.5, 10)),
            Some(Cadence::TwiceWeekly)
        );
        assert_eq!(
            Cadence::from_pub_dates(&dates_every(7.0, 10)),
            Some(Cadence::Weekly)
        );
        assert_eq!(
            Cadence::from_pub_dates(&dates_every(14.0, 6)),
            Some(Cadence::Biweekly)
        );
        assert_eq!(
            Cadence::from_pub_dates(&dates_every(30.0, 6)),
            Some(Cadence::Monthly)
        );
    }

    #[test]
    fn cadence_tolerates_a_missed_week_and_any_order() {
        // Weekly with one skipped week: the median gap is still seven days.
        let mut dates = dates_every(7.0, 8);
        dates.remove(3);
        dates.reverse();
        assert_eq!(Cadence::from_pub_dates(&dates), Some(Cadence::Weekly));
    }

    #[test]
    fn cadence_needs_enough_dated_regular_episodes() {
        assert_eq!(Cadence::from_pub_dates(&dates_every(7.0, 2)), None);
        assert_eq!(Cadence::from_pub_dates(&[0, 0, 0, 0]), None);
        assert_eq!(Cadence::from_pub_dates(&dates_every(60.0, 6)), None);
    }

    #[test]
    fn cadence_only_samples_the_newest_episodes() {
        // Old monthly archive, recent weekly releases.
        let mut dates = dates_every(30.0, 20);
        let newest = dates[19];
        dates.extend((1..=12).map(|i| newest + i * 7 * 86_400));
        assert_eq!(Cadence::from_pub_dates(&dates), Some(Cadence::Weekly));
    }

    #[test]
    fn copy_reports_progress_and_honours_cancel() {
        let data = vec![7u8; 300 * 1024];
        let mut out = Vec::new();
        let mut seen = Vec::new();
        let cancel = AtomicBool::new(false);
        copy_with_progress(
            &mut &data[..],
            &mut out,
            Some(data.len() as u64),
            &cancel,
            &mut |got, total| seen.push((got, total)),
        )
        .unwrap();
        assert_eq!(out.len(), data.len());
        let last = *seen.last().unwrap();
        assert_eq!(last, (data.len() as u64, Some(data.len() as u64)));
        assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0));

        cancel.store(true, Ordering::Relaxed);
        let result = copy_with_progress(
            &mut &data[..],
            &mut Vec::new(),
            None,
            &cancel,
            &mut |_, _| {},
        );
        assert_eq!(result, Err(DownloadError::Cancelled));
    }

    #[test]
    fn unsubscribe_cleanup_removes_only_the_shows_episode_files() {
        let dir = std::env::temp_dir().join(format!("aulos-dl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["1.mp3", "2.m4a", "3.mp3", "notes.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        remove_download_files(&dir, &[1, 2]);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(left, ["3.mp3", "notes.txt"]);
    }
}
