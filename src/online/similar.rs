// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! "Because you listened to …": similar-artist suggestions for the Home page.
//!
//! - **Seeds** ([`pick_seeds`]): the artists played most in the last 90
//!   days, topped up from all-time plays, favorites and finally random
//!   library artists, so a brand-new library still gets shelves.
//! - **Providers** ([`SimilarArtists`], tried in order by [`Chain`]):
//!   Last.fm `artist.getSimilar` (only with the user's own API key) and
//!   ListenBrainz Labs `similar-artists` (no key; needs the artist's
//!   MusicBrainz id, looked up by name). When neither knows the artist the
//!   library itself is used: [`local_similar`] ranks library artists by
//!   shared genres and decade.
//! - **Cache** ([`SimilarCache`]): results live in
//!   `data_dir/aulos/similar_artists.json` for [`CACHE_TTL_SECS`] (7 days);
//!   an expired entry is still used when the network is unreachable.
//! - **Shelves** ([`build_shelf`]): similar artists present in the library
//!   first, the rest as "discover" entries linking to their Last.fm /
//!   MusicBrainz page.
//!
//! Everything blocks; [`compute_shelves`] is meant for a blocking thread.
//! Offline simply yields no shelves.

use crate::library::LibraryDb;
use crate::library::history::{ArtistProfile, ArtistRef, LOCAL_PROVIDER};
use crate::online::scrobble::import::{artist_keys, normalize_text};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a fetched result stays fresh.
pub const CACHE_TTL_SECS: i64 = 7 * 24 * 3600;
/// Cache entries older than this many TTLs are dropped on save.
const PRUNE_AFTER_TTLS: i64 = 4;
/// Window of "recent" plays used to pick seeds.
pub const SEED_WINDOW_SECS: i64 = 90 * 24 * 3600;
/// Seed artists (= shelves) on Home.
pub const SEED_COUNT: usize = 4;
/// Cards per shelf.
pub const SHELF_LEN: usize = 20;
/// Similar artists requested per seed.
const FETCH_LIMIT: usize = 40;

const LASTFM_URL: &str = "https://ws.audioscrobbler.com/2.0/";
const MUSICBRAINZ_URL: &str = "https://musicbrainz.org/ws/2/artist";
const LB_LABS_URL: &str = "https://labs.api.listenbrainz.org/similar-artists/json";
/// Labs algorithm: session-based similarity over the last 9000 days.
const LB_ALGORITHM: &str =
    "session_based_days_9000_session_300_contribution_5_threshold_15_limit_50_skip_30";

/// Path of the on-disk cache.
pub fn cache_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aulos")
        .join("similar_artists.json")
}

// ---------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------

/// An artist reported as similar to a seed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimilarArtist {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    /// Web page of the artist, when the provider gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Why a provider could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimilarError {
    /// This provider cannot answer (not configured, rejected the key, …).
    Unavailable,
    /// The service could not be reached or is rate-limiting us.
    Network(String),
}

/// A source of similar artists.
pub trait SimilarArtists: Send + Sync {
    fn id(&self) -> &'static str;

    /// Up to `limit` artists similar to `artist`, most similar first. An
    /// artist the service does not know yields `Ok(vec![])`.
    fn similar(&self, artist: &str, limit: usize) -> Result<Vec<SimilarArtist>, SimilarError>;
}

/// Providers tried in order; the first non-empty answer wins.
pub struct Chain {
    providers: Vec<Box<dyn SimilarArtists>>,
}

impl Chain {
    pub fn new(providers: Vec<Box<dyn SimilarArtists>>) -> Self {
        Self { providers }
    }

    /// The default chain: Last.fm when `lastfm_key` is set, then ListenBrainz.
    pub fn online(lastfm_key: &str) -> Self {
        let mut providers: Vec<Box<dyn SimilarArtists>> = Vec::new();
        if !lastfm_key.trim().is_empty() {
            providers.push(Box::new(LastFmSimilar::new(lastfm_key.trim())));
        }
        providers.push(Box::new(ListenBrainzSimilar));
        Self::new(providers)
    }

    /// `Ok(vec![])` when every reachable provider returned nothing,
    /// `Err(Network)` when nobody answered and one of them was unreachable.
    pub fn similar(
        &self,
        artist: &str,
        limit: usize,
    ) -> Result<(String, Vec<SimilarArtist>), SimilarError> {
        let mut network_error = None;
        for p in &self.providers {
            match p.similar(artist, limit) {
                Ok(found) if !found.is_empty() => return Ok((p.id().to_string(), found)),
                Ok(_) | Err(SimilarError::Unavailable) => {}
                Err(SimilarError::Network(e)) => network_error = Some(e),
            }
        }
        match network_error {
            Some(e) => Err(SimilarError::Network(e)),
            None => Ok((String::new(), Vec::new())),
        }
    }
}

// ---------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheEntry {
    /// Unix seconds.
    pub fetched_at: i64,
    /// Provider id that produced the entry.
    #[serde(default)]
    pub source: String,
    pub artists: Vec<SimilarArtist>,
}

/// Disk cache of similar-artist lookups, keyed by normalised seed name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SimilarCache {
    #[serde(default)]
    entries: HashMap<String, CacheEntry>,
}

/// Cache key of an artist name.
pub fn cache_key(artist: &str) -> String {
    normalize_text(artist)
}

impl SimilarCache {
    /// Load the cache; a missing or corrupt file is an empty cache.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Write the cache atomically (temp file + rename), pruning very old
    /// entries first.
    pub fn save(&mut self, path: &Path, now: i64) -> std::io::Result<()> {
        self.prune(now);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// The entry for `artist` if it is younger than [`CACHE_TTL_SECS`].
    pub fn fresh(&self, artist: &str, now: i64) -> Option<&CacheEntry> {
        self.entries
            .get(&cache_key(artist))
            .filter(|e| now - e.fetched_at < CACHE_TTL_SECS && e.fetched_at <= now + 3600)
    }

    /// The entry for `artist` regardless of age.
    pub fn stale(&self, artist: &str) -> Option<&CacheEntry> {
        self.entries.get(&cache_key(artist))
    }

    pub fn insert(&mut self, artist: &str, now: i64, source: &str, artists: Vec<SimilarArtist>) {
        self.entries.insert(
            cache_key(artist),
            CacheEntry {
                fetched_at: now,
                source: source.to_string(),
                artists,
            },
        );
    }

    /// Drop entries that are far past their TTL.
    pub fn prune(&mut self, now: i64) {
        self.entries
            .retain(|_, e| now - e.fetched_at < CACHE_TTL_SECS * PRUNE_AFTER_TTLS);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Similar artists of `seed`: from the fresh cache, else from `chain`
/// (storing the answer), else — when nothing is reachable — from an expired
/// cache entry. `Err` only when nothing at all is known and the network is
/// unreachable.
pub fn resolve(
    cache: &mut SimilarCache,
    chain: &Chain,
    seed: &str,
    now: i64,
) -> Result<Vec<SimilarArtist>, SimilarError> {
    if let Some(entry) = cache.fresh(seed, now) {
        return Ok(entry.artists.clone());
    }
    match chain.similar(seed, FETCH_LIMIT) {
        Ok((source, found)) => {
            cache.insert(seed, now, &source, found.clone());
            Ok(found)
        }
        Err(e) => cache
            .stale(seed)
            .map(|entry| entry.artists.clone())
            .ok_or(e),
    }
}

// ---------------------------------------------------------------------
// Seeds
// ---------------------------------------------------------------------

/// Names that say nothing about taste.
fn is_generic_artist(name: &str) -> bool {
    matches!(
        normalize_text(name).as_str(),
        "" | "various artists" | "various" | "va" | "unknown artist" | "unknown" | "soundtrack"
    )
}

/// Seed artists: recent top artists first, then all-time top, favorites and
/// random library artists, without duplicates, up to `count`.
pub fn pick_seeds(
    recent: &[ArtistRef],
    all_time: &[ArtistRef],
    favorites: &[String],
    random: &[String],
    count: usize,
) -> Vec<String> {
    let candidates = recent
        .iter()
        .map(|a| a.name.as_str())
        .chain(all_time.iter().map(|a| a.name.as_str()))
        .chain(favorites.iter().map(String::as_str))
        .chain(random.iter().map(String::as_str));
    let mut seen = HashSet::new();
    let mut seeds = Vec::new();
    for name in candidates {
        if seeds.len() >= count {
            break;
        }
        if is_generic_artist(name) {
            continue;
        }
        if seen.insert(cache_key(name)) {
            seeds.push(name.to_string());
        }
    }
    seeds
}

// ---------------------------------------------------------------------
// Shelves
// ---------------------------------------------------------------------

/// Library artist names, for telling owned artists from unknown ones.
#[derive(Debug, Clone, Default)]
pub struct LibraryArtists {
    by_key: HashMap<String, String>,
}

impl LibraryArtists {
    pub fn new(names: &[String]) -> Self {
        let mut by_key = HashMap::new();
        for name in names {
            if let Some(key) = artist_keys(name).into_iter().next() {
                by_key.entry(key).or_insert_with(|| name.clone());
            }
        }
        Self { by_key }
    }

    /// The library's spelling of `name`, if the artist is in the library.
    pub fn find(&self, name: &str) -> Option<&str> {
        let key = artist_keys(name).into_iter().next()?;
        self.by_key.get(&key).map(String::as_str)
    }
}

/// An artist that is not in the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverArtist {
    pub name: String,
    /// Web page to open (https).
    pub url: String,
    /// Where `url` leads, for the tooltip ("Last.fm", "MusicBrainz").
    pub site: &'static str,
}

/// One "Because you listened to …" shelf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimilarShelf {
    pub seed: String,
    /// Similar artists present in the library (library spelling).
    pub in_library: Vec<String>,
    /// Similar artists not in the library.
    pub discover: Vec<DiscoverArtist>,
}

impl SimilarShelf {
    pub fn is_empty(&self) -> bool {
        self.in_library.is_empty() && self.discover.is_empty()
    }
}

/// Page of `artist` to open for a "discover" card, and its site label.
pub fn discover_link(artist: &SimilarArtist) -> (String, &'static str) {
    if let Some(url) = artist.url.as_deref().filter(|u| u.starts_with("https://")) {
        let site = if url.contains("musicbrainz.org") {
            "MusicBrainz"
        } else if url.contains("last.fm") {
            "Last.fm"
        } else {
            "the web"
        };
        return (url.to_string(), site);
    }
    if let Some(mbid) = artist.mbid.as_deref().filter(|m| !m.is_empty()) {
        return (
            format!(
                "https://musicbrainz.org/artist/{}",
                urlencoding::encode(mbid)
            ),
            "MusicBrainz",
        );
    }
    (
        format!(
            "https://www.last.fm/music/{}",
            urlencoding::encode(&artist.name)
        ),
        "Last.fm",
    )
}

/// Split `similar` into a shelf: library artists first, then the others,
/// each in the providers' order, at most `max_total` cards. The seed itself
/// and duplicates are dropped.
pub fn build_shelf(
    seed: &str,
    similar: &[SimilarArtist],
    library: &LibraryArtists,
    max_total: usize,
) -> SimilarShelf {
    let seed_key = cache_key(seed);
    let mut seen = HashSet::from([seed_key]);
    let mut in_library = Vec::new();
    let mut discover = Vec::new();
    for artist in similar {
        let key = cache_key(&artist.name);
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        match library.find(&artist.name) {
            Some(own) => {
                // The library spelling may itself be the seed.
                if cache_key(own) != cache_key(seed) {
                    in_library.push(own.to_string());
                }
            }
            None => {
                let (url, site) = discover_link(artist);
                discover.push(DiscoverArtist {
                    name: artist.name.clone(),
                    url,
                    site,
                });
            }
        }
    }
    in_library.truncate(max_total);
    discover.truncate(max_total.saturating_sub(in_library.len()));
    SimilarShelf {
        seed: seed.to_string(),
        in_library,
        discover,
    }
}

/// Library artists resembling `seed` by shared genres (and, as a bonus,
/// decade). Used when no online provider knows the artist. Artists without
/// any genre in common are never returned.
pub fn local_similar(seed: &str, profiles: &[ArtistProfile], limit: usize) -> Vec<SimilarArtist> {
    let seed_key = cache_key(seed);
    let Some(me) = profiles.iter().find(|p| cache_key(&p.name) == seed_key) else {
        return Vec::new();
    };
    if me.genres.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(f32, &ArtistProfile)> = profiles
        .iter()
        .filter(|p| cache_key(&p.name) != seed_key)
        .filter_map(|p| {
            let shared = p.genres.iter().filter(|g| me.genres.contains(g)).count();
            if shared == 0 {
                return None;
            }
            let union = me.genres.len() + p.genres.len() - shared;
            let mut score = shared as f32 / union as f32;
            if me.year >= 1900 && p.year >= 1900 {
                let gap = (i64::from(me.year) / 10 - i64::from(p.year) / 10).abs();
                score += match gap {
                    0 => 0.3,
                    1 => 0.15,
                    _ => 0.0,
                };
            }
            Some((score, p))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.name.cmp(&b.1.name))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, p)| SimilarArtist {
            name: p.name.clone(),
            mbid: None,
            url: None,
        })
        .collect()
}

// ---------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------

/// Enforces a minimum gap between requests to one host.
struct Throttle {
    gap: Duration,
    last: Mutex<Option<Instant>>,
}

impl Throttle {
    const fn new(gap: Duration) -> Self {
        Self {
            gap,
            last: Mutex::new(None),
        }
    }

    fn wait(&self) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = *last {
            let since = t.elapsed();
            if since < self.gap {
                std::thread::sleep(self.gap - since);
            }
        }
        *last = Some(Instant::now());
    }
}

/// Last.fm allows ~5 requests/s per key: stay at 4.
static LASTFM_THROTTLE: Throttle = Throttle::new(Duration::from_millis(250));
/// MusicBrainz asks for at most one request per second.
static MUSICBRAINZ_THROTTLE: Throttle = Throttle::new(Duration::from_millis(1100));

fn get_json(url: &str) -> Result<Value, SimilarError> {
    let resp = crate::online::scrobble::http_client()
        .get(url)
        .send()
        .map_err(|e| SimilarError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .map_err(|e| SimilarError::Network(e.to_string()))?;
    match status {
        200..=299 => serde_json::from_str(&body)
            .map_err(|_| SimilarError::Network("unexpected response".into())),
        // Rate limited or server trouble: try again another day.
        429 | 500..=599 => Err(SimilarError::Network(format!("HTTP {status}"))),
        // Lookup refused (unknown artist, bad request): no answer.
        _ => Err(SimilarError::Unavailable),
    }
}

/// Last.fm `artist.getSimilar`.
pub struct LastFmSimilar {
    api_key: String,
}

impl LastFmSimilar {
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
        }
    }
}

/// Artists of an `artist.getSimilar` response. A Last.fm error object (for
/// example "artist not found") means no answer, not a failure.
pub fn parse_lastfm_similar(v: &Value) -> Vec<SimilarArtist> {
    let artists = match v.get("similarartists").and_then(|s| s.get("artist")) {
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        _ => return Vec::new(),
    };
    artists
        .iter()
        .filter_map(|a| {
            let name = a.get("name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let text = |key: &str| {
                a.get(key)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            Some(SimilarArtist {
                name: name.to_string(),
                mbid: text("mbid"),
                url: text("url"),
            })
        })
        .collect()
}

impl SimilarArtists for LastFmSimilar {
    fn id(&self) -> &'static str {
        "lastfm"
    }

    fn similar(&self, artist: &str, limit: usize) -> Result<Vec<SimilarArtist>, SimilarError> {
        LASTFM_THROTTLE.wait();
        let url = format!(
            "{LASTFM_URL}?method=artist.getsimilar&artist={}&api_key={}&format=json&autocorrect=1&limit={limit}",
            urlencoding::encode(artist),
            urlencoding::encode(&self.api_key),
        );
        let resp = crate::online::scrobble::http_client()
            .get(&url)
            .send()
            .map_err(|e| SimilarError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .map_err(|e| SimilarError::Network(e.to_string()))?;
        use crate::online::scrobble::ScrobbleError;
        match crate::online::scrobble::audioscrobbler::parse_response(status, &body) {
            Ok(v) => Ok(parse_lastfm_similar(&v)),
            Err(ScrobbleError::Transient(e)) => Err(SimilarError::Network(e)),
            // Invalid / suspended key: this provider is out of the game.
            Err(ScrobbleError::Auth(_)) => Err(SimilarError::Unavailable),
            // "Artist not found" and friends.
            Err(ScrobbleError::Rejected(_)) => Ok(Vec::new()),
        }
    }
}

/// ListenBrainz Labs `similar-artists` (keyless), after resolving the
/// artist's MusicBrainz id by name.
pub struct ListenBrainzSimilar;

/// MusicBrainz id of the artist search result that is `seed`.
pub fn parse_musicbrainz_artist(v: &Value, seed: &str) -> Option<String> {
    let want = cache_key(seed);
    v.get("artists")?
        .as_array()?
        .iter()
        .find(|a| {
            a.get("score").and_then(Value::as_i64).unwrap_or(0) >= 90
                && a.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| cache_key(n) == want)
        })
        .and_then(|a| a.get("id")?.as_str())
        .map(str::to_string)
}

/// Artists of a Labs `similar-artists` response (an array), without
/// duplicates.
pub fn parse_labs_similar(v: &Value, seed: &str) -> Vec<SimilarArtist> {
    let seed_key = cache_key(seed);
    let mut seen = HashSet::new();
    v.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = item.get("name")?.as_str()?.trim();
                    let mbid = item
                        .get("artist_mbid")
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                        .map(str::to_string);
                    let key = cache_key(name);
                    if key.is_empty() || key == seed_key || !seen.insert(key) {
                        return None;
                    }
                    Some(SimilarArtist {
                        name: name.to_string(),
                        url: mbid
                            .as_ref()
                            .map(|m| format!("https://musicbrainz.org/artist/{m}")),
                        mbid,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl SimilarArtists for ListenBrainzSimilar {
    fn id(&self) -> &'static str {
        "listenbrainz"
    }

    fn similar(&self, artist: &str, limit: usize) -> Result<Vec<SimilarArtist>, SimilarError> {
        MUSICBRAINZ_THROTTLE.wait();
        let query = format!("artist:\"{}\"", artist.replace('"', " "));
        let url = format!(
            "{MUSICBRAINZ_URL}?query={}&fmt=json&limit=5",
            urlencoding::encode(&query)
        );
        let Some(mbid) = parse_musicbrainz_artist(&get_json(&url)?, artist) else {
            return Ok(Vec::new());
        };
        let url = format!(
            "{LB_LABS_URL}?artist_mbids={}&algorithm={LB_ALGORITHM}",
            urlencoding::encode(&mbid)
        );
        let mut found = parse_labs_similar(&get_json(&url)?, artist);
        found.truncate(limit);
        Ok(found)
    }
}

// ---------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------

/// Inputs of [`compute_shelves`].
#[derive(Debug, Clone)]
pub struct SuggestParams {
    pub db_path: PathBuf,
    pub cache_path: PathBuf,
    /// The user's Last.fm API key; empty = ListenBrainz only.
    pub lastfm_key: String,
    /// Unix seconds.
    pub now: i64,
}

/// Compute the Home shelves, calling `on_shelf` for each as soon as it is
/// ready. Stops early when `cancel` is set or the network is unreachable
/// (then nothing more is shown). Returns the number of shelves produced.
pub fn compute_shelves(
    params: &SuggestParams,
    cancel: &AtomicBool,
    on_shelf: &mut dyn FnMut(SimilarShelf),
) -> Result<usize, String> {
    let db = LibraryDb::open(&params.db_path)?;
    let recent = db.most_played_artists(LOCAL_PROVIDER, params.now - SEED_WINDOW_SECS, 10)?;
    let all_time = db.most_played_artists(LOCAL_PROVIDER, 0, 10)?;
    let favorites = db.favorite_artists(LOCAL_PROVIDER, 10)?;
    let random = db.random_artists(LOCAL_PROVIDER, 10)?;
    let seeds = pick_seeds(&recent, &all_time, &favorites, &random, SEED_COUNT);
    if seeds.is_empty() {
        return Ok(0);
    }

    let library = LibraryArtists::new(&db.library_artists(LOCAL_PROVIDER)?);
    let chain = Chain::online(&params.lastfm_key);
    let mut cache = SimilarCache::load(&params.cache_path);
    let mut profiles: Option<Vec<ArtistProfile>> = None;
    let mut produced = 0;

    for seed in &seeds {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let similar = match resolve(&mut cache, &chain, seed, params.now) {
            Ok(found) if !found.is_empty() => found,
            Ok(_) => {
                // Nobody online knows the artist: derive it from the library.
                if profiles.is_none() {
                    profiles = Some(db.artist_profiles(LOCAL_PROVIDER)?);
                }
                local_similar(seed, profiles.as_deref().unwrap_or(&[]), SHELF_LEN)
            }
            // Offline: show nothing, and don't hammer the network.
            Err(_) => break,
        };
        let shelf = build_shelf(seed, &similar, &library, SHELF_LEN);
        if !shelf.is_empty() {
            produced += 1;
            on_shelf(shelf);
        }
    }
    if let Err(e) = cache.save(&params.cache_path, params.now) {
        tracing::debug!("could not save similar-artist cache: {e}");
    }
    Ok(produced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 24 * 3600;

    fn sim(name: &str) -> SimilarArtist {
        SimilarArtist {
            name: name.into(),
            mbid: None,
            url: None,
        }
    }

    fn artist_ref(name: &str, plays: u32) -> ArtistRef {
        ArtistRef {
            name: name.into(),
            plays,
        }
    }

    // ---- cache ----------------------------------------------------

    #[test]
    fn cache_entries_expire_after_seven_days() {
        let mut cache = SimilarCache::default();
        cache.insert("Radiohead", NOW, "lastfm", vec![sim("Muse")]);
        // Key is normalised: case and diacritics don't matter.
        assert!(cache.fresh("RADIOHEAD", NOW + 6 * DAY).is_some());
        assert!(cache.fresh("radiohead", NOW + 7 * DAY - 1).is_some());
        assert!(cache.fresh("radiohead", NOW + 7 * DAY).is_none());
        // Expired entries remain available as a stale fallback.
        assert!(cache.stale("radiohead").is_some());
        assert!(cache.fresh("someone else", NOW).is_none());
    }

    #[test]
    fn cache_prunes_very_old_entries_and_round_trips() {
        let mut cache = SimilarCache::default();
        cache.insert("old", NOW - 40 * DAY, "x", vec![]);
        cache.insert("new", NOW - DAY, "x", vec![sim("A")]);
        let path = std::env::temp_dir().join(format!("aulos-similar-{}.json", std::process::id()));
        cache.save(&path, NOW).unwrap();
        let loaded = SimilarCache::load(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.fresh("new", NOW).unwrap().artists, vec![sim("A")]);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(SimilarCache::load(&path).is_empty(), "corrupt file = empty");
        let _ = std::fs::remove_file(&path);
        assert!(SimilarCache::load(&path).is_empty(), "missing file = empty");
    }

    // ---- providers / chain / resolve -------------------------------

    struct Fake {
        id: &'static str,
        answer: Result<Vec<SimilarArtist>, SimilarError>,
        calls: Arc<AtomicUsize>,
    }

    impl SimilarArtists for Fake {
        fn id(&self) -> &'static str {
            self.id
        }

        fn similar(&self, _: &str, _: usize) -> Result<Vec<SimilarArtist>, SimilarError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    fn fake(
        id: &'static str,
        answer: Result<Vec<SimilarArtist>, SimilarError>,
    ) -> (Box<dyn SimilarArtists>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Box::new(Fake {
                id,
                answer,
                calls: Arc::clone(&calls),
            }),
            calls,
        )
    }

    #[test]
    fn chain_falls_through_to_the_first_useful_provider() {
        let (a, a_calls) = fake("a", Err(SimilarError::Unavailable));
        let (b, _) = fake("b", Ok(vec![]));
        let (c, c_calls) = fake("c", Ok(vec![sim("X")]));
        let (d, d_calls) = fake("d", Ok(vec![sim("Y")]));
        let chain = Chain::new(vec![a, b, c, d]);
        let (source, found) = chain.similar("seed", 10).unwrap();
        assert_eq!((source.as_str(), found), ("c", vec![sim("X")]));
        assert_eq!(a_calls.load(Ordering::SeqCst), 1);
        assert_eq!(c_calls.load(Ordering::SeqCst), 1);
        assert_eq!(d_calls.load(Ordering::SeqCst), 0, "stops at first answer");
    }

    #[test]
    fn chain_reports_network_failure_only_when_nothing_answered() {
        let (a, _) = fake("a", Err(SimilarError::Network("down".into())));
        let (b, _) = fake("b", Ok(vec![]));
        assert_eq!(
            Chain::new(vec![a, b]).similar("s", 5),
            Err(SimilarError::Network("down".into()))
        );
        let (a, _) = fake("a", Err(SimilarError::Network("down".into())));
        let (b, _) = fake("b", Ok(vec![sim("Z")]));
        assert!(Chain::new(vec![a, b]).similar("s", 5).is_ok());
        let (a, _) = fake("a", Err(SimilarError::Unavailable));
        assert_eq!(
            Chain::new(vec![a]).similar("s", 5),
            Ok((String::new(), vec![]))
        );
    }

    #[test]
    fn resolve_uses_fresh_cache_without_calling_providers() {
        let (p, calls) = fake("p", Ok(vec![sim("New")]));
        let chain = Chain::new(vec![p]);
        let mut cache = SimilarCache::default();
        cache.insert("Seed", NOW - DAY, "p", vec![sim("Cached")]);
        let got = resolve(&mut cache, &chain, "seed", NOW).unwrap();
        assert_eq!(got, vec![sim("Cached")]);
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        // After the TTL the provider is asked again and the cache refreshed.
        let got = resolve(&mut cache, &chain, "seed", NOW + 8 * DAY).unwrap();
        assert_eq!(got, vec![sim("New")]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.fresh("seed", NOW + 8 * DAY).is_some());
    }

    #[test]
    fn resolve_falls_back_to_stale_entry_when_offline() {
        let (p, _) = fake("p", Err(SimilarError::Network("offline".into())));
        let chain = Chain::new(vec![p]);
        let mut cache = SimilarCache::default();
        cache.insert("Seed", NOW - 30 * DAY, "p", vec![sim("Old")]);
        assert_eq!(
            resolve(&mut cache, &chain, "Seed", NOW).unwrap(),
            vec![sim("Old")]
        );
        // Nothing cached and offline: an error, and nothing is cached.
        assert!(resolve(&mut cache, &chain, "Other", NOW).is_err());
        assert!(cache.stale("Other").is_none());
    }

    #[test]
    fn empty_answers_are_cached_too() {
        let (p, calls) = fake("p", Ok(vec![]));
        let chain = Chain::new(vec![p]);
        let mut cache = SimilarCache::default();
        assert!(
            resolve(&mut cache, &chain, "Nobody", NOW)
                .unwrap()
                .is_empty()
        );
        assert!(
            resolve(&mut cache, &chain, "Nobody", NOW + DAY)
                .unwrap()
                .is_empty()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // ---- seeds ----------------------------------------------------

    #[test]
    fn seeds_prefer_recent_plays_then_top_up() {
        let recent = [artist_ref("Alpha", 9), artist_ref("Beta", 5)];
        let all = [
            artist_ref("Gamma", 100),
            artist_ref("alpha", 90),
            artist_ref("Delta", 80),
            artist_ref("Epsilon", 70),
        ];
        let favs = vec!["Zeta".to_string()];
        let seeds = pick_seeds(&recent, &all, &favs, &[], 4);
        assert_eq!(seeds, ["Alpha", "Beta", "Gamma", "Delta"]);
        assert_eq!(pick_seeds(&recent, &all, &favs, &[], 10).len(), 6);
    }

    #[test]
    fn seeds_fall_back_to_favorites_then_random() {
        let favs = vec!["Fav".to_string()];
        let random = vec!["Rand1".to_string(), "Rand2".to_string()];
        assert_eq!(
            pick_seeds(&[], &[], &favs, &random, 3),
            ["Fav", "Rand1", "Rand2"]
        );
        assert_eq!(pick_seeds(&[], &[], &[], &random, 1), ["Rand1"]);
        assert!(pick_seeds(&[], &[], &[], &[], 4).is_empty());
    }

    #[test]
    fn seeds_skip_generic_artists() {
        let recent = [
            artist_ref("Various Artists", 50),
            artist_ref("Unknown Artist", 40),
            artist_ref("Real", 3),
        ];
        assert_eq!(pick_seeds(&recent, &[], &[], &[], 3), ["Real"]);
    }

    // ---- shelves --------------------------------------------------

    #[test]
    fn shelf_lists_library_artists_first_and_discover_rest() {
        let library = LibraryArtists::new(&[
            "Muse".to_string(),
            "The National".to_string(),
            "Radiohead".to_string(),
        ]);
        let similar = vec![
            sim("Nirvana"),
            sim("Muse"),
            sim("National"),  // article-insensitive match
            sim("radiohead"), // the seed itself
            sim("Muse"),      // duplicate
            SimilarArtist {
                name: "Blur".into(),
                mbid: Some("abc".into()),
                url: None,
            },
        ];
        let shelf = build_shelf("Radiohead", &similar, &library, 20);
        assert_eq!(shelf.seed, "Radiohead");
        assert_eq!(shelf.in_library, ["Muse", "The National"]);
        let names: Vec<_> = shelf.discover.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Nirvana", "Blur"]);
        assert_eq!(shelf.discover[1].url, "https://musicbrainz.org/artist/abc");
        assert_eq!(shelf.discover[1].site, "MusicBrainz");
        assert_eq!(shelf.discover[0].url, "https://www.last.fm/music/Nirvana");
    }

    #[test]
    fn shelf_respects_the_card_cap() {
        let library = LibraryArtists::new(&["A".to_string(), "B".to_string()]);
        let similar: Vec<_> = ["A", "B", "C", "D", "E"].iter().map(|n| sim(n)).collect();
        let shelf = build_shelf("Seed", &similar, &library, 3);
        assert_eq!(shelf.in_library.len() + shelf.discover.len(), 3);
        assert_eq!(shelf.in_library, ["A", "B"]);
        assert_eq!(shelf.discover.len(), 1);
        assert!(build_shelf("Seed", &[], &library, 3).is_empty());
    }

    #[test]
    fn discover_links_only_accept_https() {
        let with_url = |u: &str| SimilarArtist {
            name: "X Y".into(),
            mbid: None,
            url: Some(u.into()),
        };
        assert_eq!(
            discover_link(&with_url("https://www.last.fm/music/X+Y")),
            ("https://www.last.fm/music/X+Y".to_string(), "Last.fm")
        );
        let (url, _) = discover_link(&with_url("javascript:alert(1)"));
        assert_eq!(url, "https://www.last.fm/music/X%20Y");
        let (url, _) = discover_link(&with_url("file:///etc/passwd"));
        assert!(url.starts_with("https://"));
    }

    #[test]
    fn local_similarity_ranks_shared_genres_and_decade() {
        let p = |name: &str, genres: &[&str], year: u32| ArtistProfile {
            name: name.into(),
            genres: genres.iter().map(|g| g.to_string()).collect(),
            year,
        };
        let profiles = [
            p("Seed", &["rock", "indie"], 1995),
            p("Same", &["rock", "indie"], 1996),
            p("Partial", &["rock"], 1995),
            p("PartialOld", &["rock"], 1965),
            p("Jazzer", &["jazz"], 1995),
            p("NoGenre", &[], 1995),
        ];
        let got = local_similar("seed", &profiles, 10);
        let names: Vec<_> = got.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Same", "Partial", "PartialOld"]);
        assert_eq!(local_similar("seed", &profiles, 1).len(), 1);
        assert!(local_similar("NoGenre", &profiles, 10).is_empty());
        assert!(local_similar("Missing", &profiles, 10).is_empty());
    }

    // ---- parsers --------------------------------------------------

    #[test]
    fn parses_lastfm_similar_including_single_and_error_shapes() {
        let v = json!({"similarartists": {"artist": [
            {"name": "Muse", "mbid": "m1", "url": "https://www.last.fm/music/Muse", "match": "1"},
            {"name": "  ", "url": "x"},
            {"name": "Blur", "mbid": ""},
        ]}});
        let got = parse_lastfm_similar(&v);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].mbid.as_deref(), Some("m1"));
        assert_eq!(got[1].mbid, None);
        let single = json!({"similarartists": {"artist": {"name": "Solo"}}});
        assert_eq!(parse_lastfm_similar(&single).len(), 1);
        assert!(parse_lastfm_similar(&json!({"error": 6, "message": "nope"})).is_empty());
    }

    #[test]
    fn parses_musicbrainz_search_by_exact_name_and_score() {
        let v = json!({"artists": [
            {"id": "wrong", "name": "Radiohead Tribute", "score": 100},
            {"id": "low", "name": "Radiohead", "score": 40},
            {"id": "right", "name": "Radiohead", "score": 100},
        ]});
        assert_eq!(
            parse_musicbrainz_artist(&v, "radiohead").as_deref(),
            Some("right")
        );
        assert_eq!(parse_musicbrainz_artist(&v, "Nobody"), None);
        assert_eq!(parse_musicbrainz_artist(&json!({}), "x"), None);
    }

    #[test]
    fn parses_labs_similar_dropping_seed_and_duplicates() {
        let v = json!([
            {"artist_mbid": "1", "name": "Nirvana", "score": 9},
            {"artist_mbid": "2", "name": "Radiohead", "score": 9},
            {"artist_mbid": "3", "name": "nirvana", "score": 8},
            {"artist_mbid": "4", "name": "Muse", "score": 7},
        ]);
        let got = parse_labs_similar(&v, "Radiohead");
        let names: Vec<_> = got.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Nirvana", "Muse"]);
        assert_eq!(
            got[0].url.as_deref(),
            Some("https://musicbrainz.org/artist/1")
        );
        assert!(parse_labs_similar(&json!({"error": "x"}), "s").is_empty());
    }
}
