// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Podcast feed fetching (via `feed-rs`) and the iTunes podcast directory
//! search used by the "Add podcast" search field.

use feed_rs::model::{Entry, Feed};
use serde::Deserialize;

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
pub fn fetch_feed(
    client: &reqwest::blocking::Client,
    feed_url: &str,
) -> Result<(PodcastMeta, Vec<EpisodeMeta>), String> {
    let response = client
        .get(feed_url)
        .send()
        .map_err(|e| format!("Failed to fetch feed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Failed to fetch feed: HTTP {}", response.status()));
    }
    let bytes = super::read_capped_body(response, MAX_FEED_RESPONSE_BYTES)?;
    let feed = feed_rs::parser::parse(&bytes[..]).map_err(|e| format!("Failed to parse feed: {e}"))?;

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
        title: feed.title.as_ref().map(|t| t.content.clone()).unwrap_or_default(),
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
        author: feed.authors.first().map(|p| p.name.clone()).unwrap_or_default(),
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
        .map(|link| (link.href.clone(), link.media_type.clone().unwrap_or_default(), 0))
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
    let title = entry.title.as_ref().map(|t| t.content.clone()).unwrap_or_default();
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
pub fn search_itunes(
    client: &reqwest::blocking::Client,
    query: &str,
) -> Result<Vec<PodcastSearchResult>, String> {
    let url = format!(
        "https://itunes.apple.com/search?media=podcast&term={}",
        urlencoding::encode(query)
    );
    let response = client
        .get(&url)
        .send()
        .map_err(|e| format!("iTunes search failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("iTunes search returned HTTP {}", response.status()));
    }
    let bytes = super::read_capped_body(response, MAX_ITUNES_RESPONSE_BYTES)?;
    let body: ItunesResponse =
        serde_json::from_slice(&bytes).map_err(|e| format!("iTunes response parse failed: {e}"))?;
    Ok(map_itunes_results(body.results))
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(resolve_guid("ep-1-guid", "https://example.com/ep1.mp3"), "ep-1-guid");
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
}
