// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Optional online artist metadata: images and biographies.
//!
//! Modeled loosely on Navidrome's external-metadata agents
//! (`core/agents`), which try a chain of providers (Last.fm, Spotify,
//! Deezer, MusicBrainz) behind a common `ArtistImageRetriever` /
//! `ArtistBiographyRetriever` interface and cache results in its
//! database with a TTL (`consts.ArtistInfoTimeToLive`, 24h server-side).
//! Aulos is a desktop client hitting keyless public endpoints rather than
//! a server with its own API key vault, so this module is deliberately
//! smaller:
//!
//! - **Images**: Deezer's public `search/artist` endpoint (no API key,
//!   unlike Last.fm/Spotify) — same source Navidrome's own Deezer agent
//!   uses for `GetArtistImages`.
//! - **Biography**: Wikipedia's REST summary endpoint (no API key).
//!   MusicBrainz is a reasonable alternative but doesn't carry prose
//!   bios; Last.fm has good bios but requires a registered API key, so
//!   it's deliberately not required here (see the module doc on
//!   `crate::config::Config::fetch_artist_info`).
//! - **Caching**: a flat JSON index (`artist_info/index.json` under the
//!   data dir) of bio text + a "do we have a cached image" flag, TTL'd
//!   like `crate::autoeq::manager`'s disk cache; images themselves live
//!   as separate files under `artist_info/images/`, decoded lazily.
//!   Negative results (nothing found) are cached too, with a shorter
//!   TTL, so a networked lookup for an artist with no online presence
//!   isn't retried every time the Artists page is opened.
//!
//! In Subsonic/Navidrome mode this module is bypassed entirely: the
//! server's own `getArtistInfo2` / `artistImageUrl` / cover art already
//! surface through `SubsonicProvider::get_artist_info` (see
//! `crate::provider::mod::MusicProvider::get_artist_info`), which reuses
//! this module only for the on-disk cache, not for Deezer/Wikipedia.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a positive (bio and/or image found) cache entry stays fresh
/// before a re-fetch is attempted. Longer than Navidrome's 24h server-side
/// TTL — artist bios/photos change rarely, and this is a desktop client
/// hitting keyless public endpoints, not a server meant to stay in sync
/// with upstream catalogs.
pub const POSITIVE_TTL_SECS: i64 = 30 * 24 * 3600;

/// How long a negative (nothing found) cache entry stays fresh. Short
/// enough that a since-added Wikipedia page or Deezer listing is picked
/// up within a few days, long enough that opening the Artists page
/// repeatedly never re-hits the network for artists confirmed to have no
/// online presence.
pub const NEGATIVE_TTL_SECS: i64 = 3 * 24 * 3600;

/// Cap on a downloaded artist image, matching the spirit of
/// `crate::online::read_capped_body`'s caps elsewhere: a hostile or
/// merely broken server/CDN must not be able to exhaust memory.
const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;

/// Cap on the biography text length kept in the cache/UI. Wikipedia
/// summaries are normally a paragraph or two; this just guards against a
/// pathological extract.
const MAX_BIO_CHARS: usize = 2000;

const USER_AGENT: &str = concat!(
    "Aulos/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/M0Rf30/aulos)"
);

const DEEZER_SEARCH_URL: &str = "https://api.deezer.com/search/artist";
const WIKIPEDIA_SUMMARY_URL: &str = "https://en.wikipedia.org/api/rest_v1/page/summary";

/// One cached artist-info entry, keyed by `cache_key` in the on-disk index.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ArtistInfoEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    /// Whether an image was cached to disk for this entry (see
    /// `ArtistInfoStore::image_path`) — the index itself never embeds
    /// image bytes.
    #[serde(default)]
    pub image_cached: bool,
    /// Unix timestamp (seconds) this entry was last fetched.
    pub fetched_at: i64,
    /// True when the last fetch attempt found neither a bio nor an image.
    #[serde(default)]
    pub negative: bool,
}

/// Whether `entry` is still within its TTL as of `now` (unix seconds).
/// Pure so the refresh policy can be unit tested without any I/O.
pub fn is_fresh(entry: &ArtistInfoEntry, now: i64) -> bool {
    let ttl = if entry.negative {
        NEGATIVE_TTL_SECS
    } else {
        POSITIVE_TTL_SECS
    };
    now.saturating_sub(entry.fetched_at) < ttl
}

/// Normalizes an artist name into a cache-index key: trimmed and
/// lowercased so casing differences in tags don't fragment the cache.
fn normalize(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Cache key for the online-agents (Deezer/Wikipedia) path — shared
/// across every Local/MPD provider, since the result doesn't depend on
/// which library it came from.
pub fn agents_cache_key(name: &str) -> String {
    format!("agents:{}", normalize(name))
}

/// Cache key for the Subsonic path — namespaced per server, since the
/// bio/image genuinely come from that specific server's own agents and
/// may legitimately differ between servers.
pub fn subsonic_cache_key(provider_id: &str, name: &str) -> String {
    format!("subsonic:{provider_id}:{}", normalize(name))
}

/// Result of resolving one artist's info, either from cache or fresh.
#[derive(Debug, Clone)]
pub struct ArtistInfoOutcome {
    pub name: String,
    pub bio: Option<String>,
    /// Decoded `(width, height, rgba_pixels)`, ready for
    /// `widget::image::Handle::from_rgba`.
    pub image: Option<(u32, u32, Vec<u8>)>,
}

impl ArtistInfoOutcome {
    pub fn empty(name: String) -> Self {
        Self {
            name,
            bio: None,
            image: None,
        }
    }
}

/// Disk-backed cache for artist bios/images: a JSON index plus a
/// directory of downloaded image files, both under `data_dir`. Every
/// method is best-effort — a cache read/write failure (missing
/// permissions, a corrupt/truncated file from a crash mid-write) is
/// treated as a miss rather than a fatal error, matching
/// `crate::autoeq::manager`'s disk cache.
pub struct ArtistInfoStore {
    index_path: PathBuf,
    images_dir: PathBuf,
}

impl ArtistInfoStore {
    /// Opens (creating if needed) the store under `data_dir/artist_info`.
    /// Never fails: a directory that can't be created just means every
    /// cache read is a miss and every write is a no-op, which is better
    /// than the whole feature refusing to run because a disk cache
    /// couldn't be set up.
    pub fn open(data_dir: &Path) -> Self {
        let root = data_dir.join("artist_info");
        let images_dir = root.join("images");
        if let Err(e) = std::fs::create_dir_all(&images_dir) {
            tracing::warn!("artist_info cache dir unavailable, caching disabled: {e}");
        }
        Self {
            index_path: root.join("index.json"),
            images_dir,
        }
    }

    fn load_index(&self) -> std::collections::HashMap<String, ArtistInfoEntry> {
        let Ok(content) = std::fs::read_to_string(&self.index_path) else {
            return std::collections::HashMap::new();
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    fn save_index(&self, index: &std::collections::HashMap<String, ArtistInfoEntry>) {
        let Ok(json) = serde_json::to_string(index) else {
            return;
        };
        if let Err(e) = write_cache_atomically(&self.index_path, &json) {
            tracing::warn!("failed to save artist_info cache: {e}");
        }
    }

    /// Path an image for `key` would be stored at (whether or not it
    /// currently exists).
    fn image_path(&self, key: &str) -> PathBuf {
        self.images_dir
            .join(format!("{:x}.img", md5::compute(key.as_bytes())))
    }

    /// A fresh (non-expired) cached entry for `key`, if any.
    pub fn get_fresh(&self, key: &str, now: i64) -> Option<ArtistInfoEntry> {
        let index = self.load_index();
        let entry = index.get(key)?.clone();
        is_fresh(&entry, now).then_some(entry)
    }

    /// Records a fetch outcome (positive or negative) for `key`.
    pub fn put(&self, key: &str, entry: ArtistInfoEntry) {
        let mut index = self.load_index();
        index.insert(key.to_string(), entry);
        self.save_index(&index);
    }

    /// Saves raw (encoded) image bytes for `key` to disk. Best-effort.
    pub fn save_image(&self, key: &str, bytes: &[u8]) {
        if let Err(e) = std::fs::write(self.image_path(key), bytes) {
            tracing::warn!("failed to save artist image to cache: {e}");
        }
    }

    /// Loads and decodes the cached image for `key`, downscaled to a
    /// size generous enough to stay sharp in the largest place an artist
    /// photo is shown (the circular detail-view avatar).
    pub fn read_image(&self, key: &str) -> Option<(u32, u32, Vec<u8>)> {
        let bytes = std::fs::read(self.image_path(key)).ok()?;
        crate::library::CoverArt::decode_thumbnail(&bytes, 640)
    }
}

/// Writes `content` to `path` via a temp-file-then-rename, so a crash or
/// power loss mid-write can never leave a truncated/corrupt cache file in
/// `path`'s place. Mirrors `crate::autoeq::manager::write_cache_atomically`.
fn write_cache_atomically(path: &Path, content: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

/// Truncates on a `char` boundary, appending an ellipsis when `s` exceeds
/// `max_chars`. Local copy of `crate::views::common::truncate_str` — the
/// library layer must not depend on the views layer.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

// ── Deezer (images) ─────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct DeezerSearchResponse {
    #[serde(default)]
    data: Vec<DeezerArtist>,
}

#[derive(serde::Deserialize)]
struct DeezerArtist {
    name: String,
    #[serde(default)]
    picture_xl: String,
    #[serde(default)]
    picture_big: String,
    #[serde(default)]
    picture_medium: String,
    #[serde(default)]
    nb_fan: i64,
}

/// Deezer's shape for an artist with no picture: a generic silhouette
/// served from this path on any CDN host regardless of artist ID.
const DEEZER_PLACEHOLDER_PATH: &str = "/images/artist//";

/// Parses a Deezer `search/artist` response and picks the best image URL
/// for `artist_name`, or `None` if nothing matched closely enough.
///
/// Ranking mirrors Navidrome's Deezer agent
/// (`adapters/deezer/deezer.go: searchArtist`): Deezer's own relevance
/// ranking isn't reliable for homonyms, so this re-ranks by exact name
/// match, then case-insensitive match, then fan count, and rejects the
/// result outright if even the best match isn't at least a
/// case-insensitive equal (avoids attaching a same-genre-but-wrong
/// artist's photo).
pub fn parse_deezer_image_url(json: &str, artist_name: &str) -> Option<String> {
    let resp: DeezerSearchResponse = serde_json::from_str(json).ok()?;
    if resp.data.is_empty() {
        return None;
    }

    let rank = |a: &DeezerArtist| -> i32 {
        if a.name == artist_name {
            2
        } else if a.name.eq_ignore_ascii_case(artist_name) {
            1
        } else {
            0
        }
    };

    let mut data = resp.data;
    data.sort_by(|a, b| rank(b).cmp(&rank(a)).then(b.nb_fan.cmp(&a.nb_fan)));
    let best = &data[0];
    if !best.name.eq_ignore_ascii_case(artist_name) {
        return None;
    }

    [&best.picture_xl, &best.picture_big, &best.picture_medium]
        .into_iter()
        .find(|url| !url.is_empty() && !url.contains(DEEZER_PLACEHOLDER_PATH))
        .cloned()
}

fn fetch_deezer_image_url(client: &reqwest::blocking::Client, artist_name: &str) -> Option<String> {
    let url = format!(
        "{DEEZER_SEARCH_URL}?q={}&limit=5",
        urlencoding::encode(artist_name)
    );
    let resp = client.get(&url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().ok()?;
    parse_deezer_image_url(&body, artist_name)
}

// ── Wikipedia (biography) ───────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct WikipediaSummary {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    extract: String,
}

/// Parses a Wikipedia REST `page/summary` response into a bio string, or
/// `None` for a disambiguation page, a "not found" error body, or an
/// empty extract.
pub fn parse_wikipedia_extract(json: &str) -> Option<String> {
    let resp: WikipediaSummary = serde_json::from_str(json).ok()?;
    if resp.kind == "disambiguation" {
        return None;
    }
    let extract = resp.extract.trim();
    if extract.is_empty() {
        return None;
    }
    Some(truncate_chars(extract, MAX_BIO_CHARS))
}

fn fetch_wikipedia_bio(client: &reqwest::blocking::Client, artist_name: &str) -> Option<String> {
    let url = format!(
        "{WIKIPEDIA_SUMMARY_URL}/{}",
        urlencoding::encode(artist_name)
    );
    let resp = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.text().ok()?;
    parse_wikipedia_extract(&body)
}

/// A dedicated blocking client (distinct from `crate::app::HTTP_CLIENT`)
/// so every request here carries an identifying User-Agent, as public
/// APIs like Wikipedia's ask for.
fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(8))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
}

fn download_image(client: &reqwest::blocking::Client, url: &str) -> Option<Vec<u8>> {
    let resp = client.get(url).send().ok()?;
    if !resp.status().is_success() {
        return None;
    }
    crate::online::read_capped_body(resp, MAX_IMAGE_BYTES).ok()
}

/// Resolves one artist's info via Aulos's own keyless online agents
/// (Deezer for the image, Wikipedia for the bio), consulting/populating
/// `store` first. Blocking — call from `tokio::task::spawn_blocking`,
/// never on the UI thread or an async task's own worker thread.
pub fn resolve_via_agents(store: &ArtistInfoStore, name: &str, now: i64) -> ArtistInfoOutcome {
    let key = agents_cache_key(name);

    if let Some(entry) = store.get_fresh(&key, now) {
        let image = entry.image_cached.then(|| store.read_image(&key)).flatten();
        return ArtistInfoOutcome {
            name: name.to_string(),
            bio: entry.bio,
            image,
        };
    }

    let client = http_client();
    let image_url = fetch_deezer_image_url(&client, name);
    let bio = fetch_wikipedia_bio(&client, name);
    let raw_image = image_url.and_then(|url| download_image(&client, &url));

    let image = raw_image.as_deref().and_then(|bytes| {
        store.save_image(&key, bytes);
        crate::library::CoverArt::decode_thumbnail(bytes, 640)
    });

    let negative = bio.is_none() && image.is_none();
    store.put(
        &key,
        ArtistInfoEntry {
            bio: bio.clone(),
            image_cached: image.is_some(),
            fetched_at: now,
            negative,
        },
    );

    ArtistInfoOutcome {
        name: name.to_string(),
        bio,
        image,
    }
}

/// Resolves one artist's info from a provider-native source (currently
/// only meaningfully implemented for `SubsonicProvider`), consulting/
/// populating `store` first with a server-namespaced key.
pub fn resolve_via_provider(
    store: &ArtistInfoStore,
    provider: &dyn crate::provider::MusicProvider,
    provider_id: &str,
    name: &str,
    now: i64,
) -> ArtistInfoOutcome {
    let key = subsonic_cache_key(provider_id, name);

    if let Some(entry) = store.get_fresh(&key, now) {
        let image = entry.image_cached.then(|| store.read_image(&key)).flatten();
        return ArtistInfoOutcome {
            name: name.to_string(),
            bio: entry.bio,
            image,
        };
    }

    let (bio, raw_image) = match provider.get_artist_info(name) {
        Ok(Some(result)) => (result.bio, result.image_bytes),
        Ok(None) | Err(_) => (None, None),
    };

    let image = raw_image.as_deref().and_then(|bytes| {
        store.save_image(&key, bytes);
        crate::library::CoverArt::decode_thumbnail(bytes, 640)
    });

    let negative = bio.is_none() && image.is_none();
    store.put(
        &key,
        ArtistInfoEntry {
            bio: bio.clone(),
            image_cached: image.is_some(),
            fetched_at: now,
            negative,
        },
    );

    ArtistInfoOutcome {
        name: name.to_string(),
        bio,
        image,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(negative: bool, fetched_at: i64) -> ArtistInfoEntry {
        ArtistInfoEntry {
            bio: Some("bio".into()),
            image_cached: true,
            fetched_at,
            negative,
        }
    }

    #[test]
    fn positive_entry_fresh_within_ttl() {
        let e = entry(false, 1000);
        assert!(is_fresh(&e, 1000 + POSITIVE_TTL_SECS - 1));
        assert!(!is_fresh(&e, 1000 + POSITIVE_TTL_SECS));
    }

    #[test]
    fn negative_entry_uses_shorter_ttl() {
        let e = entry(true, 1000);
        assert!(is_fresh(&e, 1000 + NEGATIVE_TTL_SECS - 1));
        assert!(!is_fresh(&e, 1000 + NEGATIVE_TTL_SECS));
        // Sanity: the negative TTL really is shorter than the positive one.
        const { assert!(NEGATIVE_TTL_SECS < POSITIVE_TTL_SECS) };
    }

    #[test]
    fn cache_keys_normalize_case_and_whitespace() {
        assert_eq!(
            agents_cache_key("Daft Punk"),
            agents_cache_key("  daft punk  ")
        );
        assert_ne!(
            agents_cache_key("Daft Punk"),
            subsonic_cache_key("srv1", "Daft Punk")
        );
        assert_ne!(
            subsonic_cache_key("srv1", "Daft Punk"),
            subsonic_cache_key("srv2", "Daft Punk")
        );
    }

    #[test]
    fn deezer_exact_match_picks_highest_res_non_placeholder() {
        let json = r#"{
            "data": [
                {"name": "Daft Punk", "picture_xl": "", "picture_big": "https://e-cdn-images.dzcdn.net/images/artist//1000x1000-000000-80-0-0.jpg", "picture_medium": "https://cdn/med.jpg", "nb_fan": 1000000}
            ]
        }"#;
        // The "big" URL is Deezer's placeholder shape, so the parser must
        // skip it and fall through to "medium".
        assert_eq!(
            parse_deezer_image_url(json, "Daft Punk"),
            Some("https://cdn/med.jpg".to_string())
        );
    }

    #[test]
    fn deezer_prefers_exact_and_more_popular_match() {
        let json = r#"{
            "data": [
                {"name": "Daft Punk Tribute", "picture_xl": "https://cdn/tribute.jpg", "picture_big": "", "picture_medium": "", "nb_fan": 50},
                {"name": "Daft Punk", "picture_xl": "https://cdn/real.jpg", "picture_big": "", "picture_medium": "", "nb_fan": 900000}
            ]
        }"#;
        assert_eq!(
            parse_deezer_image_url(json, "Daft Punk"),
            Some("https://cdn/real.jpg".to_string())
        );
    }

    #[test]
    fn deezer_rejects_no_close_match() {
        let json = r#"{"data": [{"name": "Someone Else", "picture_xl": "https://cdn/x.jpg", "picture_big": "", "picture_medium": "", "nb_fan": 1}]}"#;
        assert_eq!(parse_deezer_image_url(json, "Daft Punk"), None);
    }

    #[test]
    fn deezer_empty_data_is_none() {
        assert_eq!(parse_deezer_image_url(r#"{"data": []}"#, "Daft Punk"), None);
        assert_eq!(parse_deezer_image_url("not json", "Daft Punk"), None);
    }

    #[test]
    fn wikipedia_extracts_bio_text() {
        let json = r#"{"type": "standard", "extract": "  Daft Punk was a French duo.  "}"#;
        assert_eq!(
            parse_wikipedia_extract(json),
            Some("Daft Punk was a French duo.".to_string())
        );
    }

    #[test]
    fn wikipedia_rejects_disambiguation_and_empty() {
        assert_eq!(
            parse_wikipedia_extract(r#"{"type": "disambiguation", "extract": "stuff"}"#),
            None
        );
        assert_eq!(
            parse_wikipedia_extract(r#"{"type": "standard", "extract": "   "}"#),
            None
        );
        assert_eq!(parse_wikipedia_extract("not json"), None);
    }

    #[test]
    fn truncate_chars_appends_ellipsis_only_when_needed() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("abcdefghij", 5), "abcde…");
    }
}
