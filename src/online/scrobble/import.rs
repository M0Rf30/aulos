// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Import a user's listening history from ListenBrainz / Last.fm / Libre.fm
//! into the library's `play_history` table.
//!
//! The pipeline is deliberately split so that every interesting part is
//! testable without a network:
//!
//! - **sources** ([`ListenSource`]): paged, newest-first fetchers —
//!   [`ListenBrainzSource`] (`GET /1/user/{user}/listens`, `max_ts` paging,
//!   1000 per page, `X-RateLimit-*` honoured) and [`AudioscrobblerSource`]
//!   (`user.getRecentTracks`, 200 per page, paced below Last.fm's
//!   ~5 requests/s);
//! - **matching** ([`TrackIndex`]): listens are matched to library tracks by
//!   normalised artist + title (case, diacritics, punctuation, `feat.` and
//!   remaster annotations ignored; album as tiebreaker). Matching by
//!   recording MBID is not attempted: the library does not store MBIDs;
//! - **merging** ([`run_import`]): matched listens are written through
//!   `LibraryDb::import_plays`, which is idempotent.

use super::audioscrobbler::{self, Endpoint, encode_form};
use super::{ScrobbleError, Service, http_client, keys, net_error};
use crate::fl;
use crate::library::LibraryDb;
use crate::library::history::{ImportPlay, ImportTrack, LOCAL_PROVIDER};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Listens requested per ListenBrainz page (the API's maximum).
pub const LB_PAGE_SIZE: u32 = 1000;
/// Listens requested per Last.fm page (the API's maximum).
pub const LASTFM_PAGE_SIZE: u32 = 200;
/// Pause between Last.fm requests (keeps us at ≤ 4 requests/s).
const LASTFM_PACE: Duration = Duration::from_millis(250);
/// Attempts per page before the import gives up.
const MAX_ATTEMPTS: u32 = 4;
/// Longest single wait we accept from a rate-limit header.
const MAX_RATE_WAIT: Duration = Duration::from_secs(60);

/// Path of the library database (must match `app::online_db_path`).
pub fn library_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aulos")
        .join("library.db")
}

// ---------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------

/// One listen reported by a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    pub artist: String,
    pub title: String,
    pub album: String,
    /// Unix seconds.
    pub timestamp: i64,
}

/// Running counters of an import.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportProgress {
    /// Listens read from the service so far.
    pub fetched: u64,
    /// Listens that matched a library track.
    pub matched: u64,
    /// Listens without a counterpart in the library.
    pub unmatched: u64,
    /// Plays actually added to the history (`matched` minus those already
    /// present).
    pub imported: u64,
    /// Total listens the service reports, when it says so.
    pub total: Option<u64>,
}

impl ImportProgress {
    /// Matched listens that were already in the history.
    pub fn skipped(&self) -> u64 {
        self.matched.saturating_sub(self.imported)
    }

    /// Completion in `0.0..=1.0`, when the total is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.fetched as f32 / total as f32).clamp(0.0, 1.0))
    }
}

/// How an import ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    pub progress: ImportProgress,
    pub cancelled: bool,
    /// Set when fetching failed part-way; what was read before is kept.
    pub error: Option<String>,
}

// ---------------------------------------------------------------------
// Normalisation and matching
// ---------------------------------------------------------------------

/// Append the ASCII fold of `c` (diacritics and a few ligatures removed).
fn fold_char(c: char, out: &mut String) {
    match c {
        // Combining marks (decomposed input).
        '\u{300}'..='\u{36f}' => {}
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => out.push('a'),
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => out.push('c'),
        'ď' | 'đ' | 'ð' => out.push('d'),
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => out.push('e'),
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => out.push('g'),
        'ĥ' | 'ħ' => out.push('h'),
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => out.push('i'),
        'ĵ' => out.push('j'),
        'ķ' => out.push('k'),
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => out.push('l'),
        'ñ' | 'ń' | 'ņ' | 'ň' => out.push('n'),
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => out.push('o'),
        'ŕ' | 'ŗ' | 'ř' => out.push('r'),
        'ś' | 'ŝ' | 'ş' | 'š' => out.push('s'),
        'ţ' | 'ť' | 'ŧ' => out.push('t'),
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => out.push('u'),
        'ŵ' => out.push('w'),
        'ý' | 'ÿ' | 'ŷ' => out.push('y'),
        'ź' | 'ż' | 'ž' => out.push('z'),
        'ß' => out.push_str("ss"),
        'æ' => out.push_str("ae"),
        'œ' => out.push_str("oe"),
        'þ' => out.push_str("th"),
        _ => out.push(c),
    }
}

/// Lower-case, fold diacritics, turn `&` into `and`, drop punctuation and
/// collapse whitespace: `"Beyoncé — Halo!"` → `"beyonce halo"`.
pub fn normalize_text(s: &str) -> String {
    let mut folded = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '&' {
            folded.push_str(" and ");
            continue;
        }
        for lc in c.to_lowercase() {
            fold_char(lc, &mut folded);
        }
    }
    let mut out = String::with_capacity(folded.len());
    let mut pending_space = false;
    for c in folded.chars() {
        if c.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        } else if c == '\'' || c == '\u{2019}' {
            // "don't" → "dont" rather than "don t".
        } else {
            pending_space = true;
        }
    }
    out
}

static BRACKET_ANNOTATION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"\s*[\(\[]\s*(?:(?:feat|ft|featuring|with)\b\.?[^\)\]]*|[^\)\]]*remaster[^\)\]]*)[\)\]]",
    )
    .expect("valid regex")
});
static TAIL_FEAT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\s+(?:feat|ft|featuring)\b\.?(?:\s.*)?$").expect("valid regex")
});
static TAIL_REMASTER: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\s+[-–—]\s+[^-–—]*remaster[^-–—]*$").expect("valid regex")
});
static ANY_BRACKET: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\s*[\(\[][^\)\]]*[\)\]]").expect("valid regex"));

/// Remove `(feat. X)`, `[ft. X]`, ` featuring X` and remaster markers.
fn strip_annotations(s: &str) -> String {
    let lower = s.to_lowercase();
    let a = BRACKET_ANNOTATION.replace_all(&lower, "");
    let b = TAIL_REMASTER.replace(&a, "");
    let c = TAIL_FEAT.replace(&b, "");
    c.into_owned()
}

/// Identity key of a track title.
pub fn title_key(title: &str) -> String {
    let stripped = strip_annotations(title);
    let key = normalize_text(&stripped);
    if key.is_empty() {
        normalize_text(title)
    } else {
        key
    }
}

/// Identity key of an album name (all bracketed suffixes ignored, so
/// "Album (Deluxe Edition)" equals "Album").
pub fn album_key(album: &str) -> String {
    let stripped = ANY_BRACKET.replace_all(album, "");
    let key = normalize_text(&stripped);
    if key.is_empty() {
        normalize_text(album)
    } else {
        key
    }
}

/// Normalised artist name without a leading article.
fn artist_key(name: &str) -> String {
    let key = normalize_text(name);
    match key.strip_prefix("the ") {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => key,
    }
}

/// Candidate identity keys of an artist credit, most specific first: the
/// whole credit (featured artists removed), then its first credited artist
/// (`"A & B"`, `"A, B"`, `"A; B"`, `"A x B"` → `"A"`).
pub fn artist_keys(credit: &str) -> Vec<String> {
    let stripped = strip_annotations(credit);
    let mut keys = Vec::new();
    let full = artist_key(&stripped);
    if !full.is_empty() {
        keys.push(full);
    }
    let first = stripped
        .split([';', ',', '/'])
        .next()
        .unwrap_or(&stripped)
        .split(" & ")
        .next()
        .unwrap_or("")
        .split(" and ")
        .next()
        .unwrap_or("")
        .split(" x ")
        .next()
        .unwrap_or("");
    let first = artist_key(first);
    if !first.is_empty() && !keys.contains(&first) {
        keys.push(first);
    }
    if keys.is_empty() {
        let raw = artist_key(credit);
        if !raw.is_empty() {
            keys.push(raw);
        }
    }
    keys
}

struct IndexEntry {
    id: i64,
    album: String,
    duration_secs: u32,
}

/// A library track a listen was matched to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Matched {
    pub track_id: i64,
    pub duration_secs: u32,
}

/// Lookup of library tracks by normalised artist + title.
pub struct TrackIndex {
    entries: Vec<IndexEntry>,
    by_key: HashMap<(String, String), Vec<usize>>,
}

impl TrackIndex {
    pub fn new(tracks: &[ImportTrack]) -> Self {
        let mut entries = Vec::with_capacity(tracks.len());
        let mut by_key: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for t in tracks {
            let title = title_key(&t.title);
            if title.is_empty() {
                continue;
            }
            let idx = entries.len();
            entries.push(IndexEntry {
                id: t.id,
                album: album_key(&t.album),
                duration_secs: t.duration_secs,
            });
            let mut seen: Vec<String> = Vec::new();
            for credit in [&t.artist, &t.album_artist] {
                for key in artist_keys(credit) {
                    if !seen.contains(&key) {
                        by_key
                            .entry((key.clone(), title.clone()))
                            .or_default()
                            .push(idx);
                        seen.push(key);
                    }
                }
            }
        }
        Self { entries, by_key }
    }

    /// The library track for a listen, preferring the one on the same album
    /// when several share artist and title.
    pub fn find(&self, artist: &str, title: &str, album: &str) -> Option<Matched> {
        let title = title_key(title);
        if title.is_empty() {
            return None;
        }
        let album = album_key(album);
        for artist in artist_keys(artist) {
            let Some(candidates) = self.by_key.get(&(artist, title.clone())) else {
                continue;
            };
            let same_album = (!album.is_empty())
                .then(|| {
                    candidates
                        .iter()
                        .copied()
                        .find(|&i| self.entries[i].album == album)
                })
                .flatten();
            let pick = same_album.or_else(|| candidates.first().copied())?;
            let e = &self.entries[pick];
            return Some(Matched {
                track_id: e.id,
                duration_secs: e.duration_secs,
            });
        }
        None
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Window within which an existing play of the same track counts as the
/// same listen (see `LibraryDb::import_plays`): the track's length,
/// clamped to 2–15 minutes.
pub fn dedupe_window_secs(duration_secs: u32) -> i64 {
    i64::from(duration_secs).clamp(120, 900)
}

// ---------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------

/// A paged, newest-first stream of listens.
pub trait ListenSource {
    /// Total number of listens, if the service can tell cheaply.
    fn total(&mut self) -> Option<u64> {
        None
    }

    /// Next page, or `None` once the history is exhausted.
    fn next_page(&mut self, cancel: &AtomicBool) -> Result<Option<Vec<Listen>>, String>;
}

/// Sleep `d`, waking early when `cancel` is set. Returns `false` if
/// cancelled.
fn sleep_cancellable(d: Duration, cancel: &AtomicBool) -> bool {
    let step = Duration::from_millis(100);
    let mut left = d;
    while !left.is_zero() {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let nap = left.min(step);
        std::thread::sleep(nap);
        left -= nap;
    }
    !cancel.load(Ordering::Relaxed)
}

/// How long to wait before the next ListenBrainz request, from the
/// `X-RateLimit-Remaining` / `X-RateLimit-Reset-In` headers: nothing while
/// requests remain, the reset time once the budget is used up.
pub fn rate_limit_delay(remaining: Option<i64>, reset_in: Option<u64>) -> Option<Duration> {
    match (remaining, reset_in) {
        (Some(r), Some(reset)) if r <= 1 => {
            Some(Duration::from_secs(reset.saturating_add(1)).min(MAX_RATE_WAIT))
        }
        _ => None,
    }
}

fn header_num<T: std::str::FromStr>(resp: &reqwest::blocking::Response, name: &str) -> Option<T> {
    resp.headers().get(name)?.to_str().ok()?.trim().parse().ok()
}

/// ListenBrainz listens of one user.
pub struct ListenBrainzSource {
    base_url: String,
    user: String,
    token: Option<String>,
    /// `listened_at` upper bound (exclusive) of the next page.
    max_ts: Option<i64>,
    done: bool,
}

impl ListenBrainzSource {
    pub fn new(base_url: &str, user: &str, token: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            user: user.to_string(),
            token: token.filter(|t| !t.is_empty()),
            max_ts: None,
            done: false,
        }
    }

    fn user_url(&self, suffix: &str) -> String {
        format!(
            "{}/1/user/{}/{suffix}",
            self.base_url,
            urlencoding::encode(&self.user)
        )
    }

    /// GET with retries, honouring `Retry`/rate-limit headers.
    fn get(&self, url: &str, cancel: &AtomicBool) -> Result<Value, String> {
        let mut last_error = String::from("request failed");
        for attempt in 0..MAX_ATTEMPTS {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let mut req = http_client().get(url);
            if let Some(t) = &self.token {
                req = req.header("Authorization", format!("Token {t}"));
            }
            let resp = match req.send() {
                Ok(r) => r,
                Err(e) => {
                    last_error = net_error(e).to_string();
                    if !sleep_cancellable(Duration::from_secs(2 * u64::from(attempt + 1)), cancel) {
                        return Err("cancelled".into());
                    }
                    continue;
                }
            };
            let status = resp.status().as_u16();
            let delay = rate_limit_delay(
                header_num(&resp, "X-RateLimit-Remaining"),
                header_num(&resp, "X-RateLimit-Reset-In"),
            );
            let reset_in: Option<u64> = header_num(&resp, "X-RateLimit-Reset-In");
            let body = resp.text().map_err(|e| net_error(e).to_string())?;
            match status {
                200 => {
                    let v = serde_json::from_str(&body)
                        .map_err(|_| "unexpected response from ListenBrainz".to_string())?;
                    if let Some(d) = delay {
                        sleep_cancellable(d, cancel);
                    }
                    return Ok(v);
                }
                404 => return Err(format!("ListenBrainz user '{}' not found", self.user)),
                401 | 403 => return Err("ListenBrainz refused the request (token?)".into()),
                429 => {
                    last_error = "rate limited by ListenBrainz".into();
                    let wait = Duration::from_secs(reset_in.unwrap_or(5).saturating_add(1))
                        .min(MAX_RATE_WAIT);
                    if !sleep_cancellable(wait, cancel) {
                        return Err("cancelled".into());
                    }
                }
                _ => {
                    last_error = format!("ListenBrainz HTTP {status}");
                    if !sleep_cancellable(Duration::from_secs(2 * u64::from(attempt + 1)), cancel) {
                        return Err("cancelled".into());
                    }
                }
            }
        }
        Err(last_error)
    }
}

/// Listens of a `GET /1/user/{user}/listens` response, plus the oldest
/// `listened_at` seen and the user's overall oldest listen (when given).
pub fn parse_lb_listens(v: &Value) -> (Vec<Listen>, Option<i64>, Option<i64>) {
    let payload = v.get("payload");
    let mut oldest: Option<i64> = None;
    let mut listens = Vec::new();
    if let Some(items) = payload
        .and_then(|p| p.get("listens"))
        .and_then(Value::as_array)
    {
        for item in items {
            let Some(ts) = item.get("listened_at").and_then(Value::as_i64) else {
                continue;
            };
            oldest = Some(oldest.map_or(ts, |o| o.min(ts)));
            let meta = item.get("track_metadata");
            let text = |key: &str| {
                meta.and_then(|m| m.get(key))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let (artist, title) = (text("artist_name"), text("track_name"));
            if artist.is_empty() || title.is_empty() {
                continue;
            }
            listens.push(Listen {
                artist,
                title,
                album: text("release_name"),
                timestamp: ts,
            });
        }
    }
    let user_oldest = payload
        .and_then(|p| p.get("oldest_listen_ts"))
        .and_then(Value::as_i64);
    (listens, oldest, user_oldest)
}

impl ListenSource for ListenBrainzSource {
    fn total(&mut self) -> Option<u64> {
        let cancel = AtomicBool::new(false);
        let v = self.get(&self.user_url("listen-count"), &cancel).ok()?;
        v.get("payload")?.get("count")?.as_u64()
    }

    fn next_page(&mut self, cancel: &AtomicBool) -> Result<Option<Vec<Listen>>, String> {
        if self.done {
            return Ok(None);
        }
        let mut query = format!("count={LB_PAGE_SIZE}");
        if let Some(ts) = self.max_ts {
            query.push_str(&format!("&max_ts={ts}"));
        }
        let v = self.get(&self.user_url(&format!("listens?{query}")), cancel)?;
        let (listens, oldest, user_oldest) = parse_lb_listens(&v);
        match oldest {
            None => {
                self.done = true;
                return Ok(None);
            }
            Some(o) => {
                // `max_ts` is exclusive: the next page starts below `o`.
                if self.max_ts.is_some_and(|m| o >= m) || user_oldest.is_some_and(|u| o <= u) {
                    self.done = true;
                }
                self.max_ts = Some(o);
            }
        }
        Ok(Some(listens))
    }
}

/// `user.getRecentTracks` of a Last.fm-compatible service.
pub struct AudioscrobblerSource {
    endpoint: Endpoint,
    api_key: String,
    user: String,
    /// Next page to fetch (1-based).
    page: u32,
    total_pages: Option<u32>,
    total: Option<u64>,
    first_request: bool,
}

impl AudioscrobblerSource {
    pub fn new(endpoint: Endpoint, api_key: &str, user: &str) -> Self {
        Self {
            endpoint,
            api_key: api_key.to_string(),
            user: user.to_string(),
            page: 1,
            total_pages: None,
            total: None,
            first_request: true,
        }
    }

    fn fetch(&mut self, cancel: &AtomicBool) -> Result<Value, String> {
        let url = format!(
            "{}?{}",
            self.endpoint.url,
            encode_form(&[
                ("method".to_string(), "user.getRecentTracks".to_string()),
                ("user".to_string(), self.user.clone()),
                ("api_key".to_string(), self.api_key.clone()),
                ("format".to_string(), "json".to_string()),
                ("limit".to_string(), LASTFM_PAGE_SIZE.to_string()),
                ("page".to_string(), self.page.to_string()),
            ])
        );
        let mut last_error = String::from("request failed");
        for attempt in 0..MAX_ATTEMPTS {
            if !self.first_request && !sleep_cancellable(LASTFM_PACE, cancel) {
                return Err("cancelled".into());
            }
            self.first_request = false;
            let resp = match http_client().get(&url).send() {
                Ok(r) => r,
                Err(e) => {
                    last_error = net_error(e).to_string();
                    if !sleep_cancellable(Duration::from_secs(2 * u64::from(attempt + 1)), cancel) {
                        return Err("cancelled".into());
                    }
                    continue;
                }
            };
            let status = resp.status().as_u16();
            let body = resp.text().map_err(|e| net_error(e).to_string())?;
            match audioscrobbler::parse_response(status, &body) {
                Ok(v) => return Ok(v),
                Err(ScrobbleError::Transient(m)) => {
                    last_error = m;
                    if !sleep_cancellable(Duration::from_secs(2 * u64::from(attempt + 1)), cancel) {
                        return Err("cancelled".into());
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err(last_error)
    }
}

/// Listens, total page count and total listen count of a
/// `user.getRecentTracks` response. The "now playing" entry (no date) is
/// skipped; a single-track page arrives as a bare object.
pub fn parse_recent_tracks(v: &Value) -> (Vec<Listen>, u32, Option<u64>) {
    let recent = v.get("recenttracks");
    let attr = recent.and_then(|r| r.get("@attr"));
    let num = |key: &str| -> Option<u64> {
        let n = attr?.get(key)?;
        n.as_u64().or_else(|| n.as_str()?.parse().ok())
    };
    let total_pages = num("totalPages").unwrap_or(1) as u32;
    let total = num("total");
    let tracks = match recent.and_then(|r| r.get("track")) {
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        _ => Vec::new(),
    };
    let text = |v: Option<&Value>| -> String {
        v.and_then(|x| x.get("#text").or_else(|| x.get("name")))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let listens = tracks
        .iter()
        .filter_map(|t| {
            let ts = t
                .get("date")
                .and_then(|d| d.get("uts"))
                .and_then(|u| u.as_i64().or_else(|| u.as_str()?.parse().ok()))?;
            let title = t.get("name")?.as_str()?.to_string();
            let artist = text(t.get("artist"));
            (!artist.is_empty() && !title.is_empty()).then(|| Listen {
                artist,
                title,
                album: text(t.get("album")),
                timestamp: ts,
            })
        })
        .collect();
    (listens, total_pages, total)
}

impl ListenSource for AudioscrobblerSource {
    fn total(&mut self) -> Option<u64> {
        // Learned from the first page; the driver asks again afterwards.
        self.total
    }

    fn next_page(&mut self, cancel: &AtomicBool) -> Result<Option<Vec<Listen>>, String> {
        if self.total_pages.is_some_and(|n| self.page > n) {
            return Ok(None);
        }
        let v = self.fetch(cancel)?;
        let (listens, total_pages, total) = parse_recent_tracks(&v);
        self.total_pages = Some(total_pages);
        self.total = total;
        self.page += 1;
        Ok(Some(listens))
    }
}

// ---------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------

/// Read `source` page by page and merge the listens that match library
/// tracks into the play history. Pages already merged stay merged when a
/// later page fails or the import is cancelled. `report` is called after
/// every page.
pub fn run_import(
    source: &mut dyn ListenSource,
    db: &LibraryDb,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(ImportProgress),
) -> Result<ImportOutcome, String> {
    let index = TrackIndex::new(&db.import_tracks(LOCAL_PROVIDER)?);
    let snapshot = db.max_play_id()?;
    let mut progress = ImportProgress {
        total: source.total(),
        ..ImportProgress::default()
    };
    report(progress);

    let mut outcome = ImportOutcome {
        progress,
        cancelled: false,
        error: None,
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            break;
        }
        let page = match source.next_page(cancel) {
            Ok(Some(page)) => page,
            Ok(None) => break,
            Err(_) if cancel.load(Ordering::Relaxed) => {
                outcome.cancelled = true;
                break;
            }
            Err(e) => {
                outcome.error = Some(e);
                break;
            }
        };
        if progress.total.is_none() {
            progress.total = source.total();
        }
        let mut plays = Vec::with_capacity(page.len());
        for listen in &page {
            progress.fetched += 1;
            if listen.timestamp <= 0 {
                progress.unmatched += 1;
                continue;
            }
            match index.find(&listen.artist, &listen.title, &listen.album) {
                Some(m) => plays.push(ImportPlay {
                    track_id: m.track_id,
                    played_at: listen.timestamp,
                    window_secs: dedupe_window_secs(m.duration_secs),
                }),
                None => progress.unmatched += 1,
            }
        }
        progress.matched += plays.len() as u64;
        progress.imported += u64::from(db.import_plays(&plays, snapshot)?);
        report(progress);
    }
    outcome.progress = progress;
    Ok(outcome)
}

/// Everything an import needs, gathered on the UI thread.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub service: Service,
    pub user: String,
    /// Last.fm API key (ignored for the other services).
    pub lastfm_api_key: String,
    pub db_path: PathBuf,
}

/// Build the source for `req` (reads the ListenBrainz token from the
/// keyring; it is optional for public profiles).
fn source_for(req: &ImportRequest) -> Result<Box<dyn ListenSource>, String> {
    match req.service {
        Service::ListenBrainz => {
            let token = crate::credentials::retrieve_password(&keys::listenbrainz_token())
                .ok()
                .flatten();
            Ok(Box::new(ListenBrainzSource::new(
                super::listenbrainz::DEFAULT_BASE_URL,
                &req.user,
                token,
            )))
        }
        service => {
            let endpoint = Endpoint::for_service(service).ok_or("unsupported service")?;
            let (key, _) = audioscrobbler::api_credentials(service, &req.lastfm_api_key, "");
            if key.is_empty() {
                return Err(fl!("scrobble-error-no-keys"));
            }
            Ok(Box::new(AudioscrobblerSource::new(
                endpoint, &key, &req.user,
            )))
        }
    }
}

/// Blocking entry point used by the controller's background task.
pub fn run_request(
    req: &ImportRequest,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(ImportProgress),
) -> Result<ImportOutcome, String> {
    let mut source = source_for(req)?;
    let db = LibraryDb::open(&req.db_path)?;
    run_import(source.as_mut(), &db, cancel, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn track(id: i64, artist: &str, title: &str, album: &str) -> ImportTrack {
        ImportTrack {
            id,
            title: title.into(),
            artist: artist.into(),
            album_artist: artist.into(),
            album: album.into(),
            duration_secs: 200,
        }
    }

    #[test]
    fn normalisation_ignores_case_diacritics_and_punctuation() {
        assert_eq!(normalize_text("Beyoncé — Halo!"), "beyonce halo");
        assert_eq!(normalize_text("Sigur Rós"), "sigur ros");
        assert_eq!(normalize_text("Mötley Crüe"), normalize_text("MOTLEY CRUE"));
        assert_eq!(normalize_text("Don't Stop"), "dont stop");
        assert_eq!(normalize_text("Simon & Garfunkel"), "simon and garfunkel");
        assert_eq!(normalize_text("Æon Flux"), "aeon flux");
        // Decomposed input (e + combining acute) folds the same way.
        assert_eq!(normalize_text("Cafe\u{301}"), "cafe");
        assert_eq!(normalize_text("  A   B  "), "a b");
    }

    #[test]
    fn titles_ignore_feat_and_remaster_annotations() {
        let base = title_key("Hello");
        assert_eq!(title_key("Hello (feat. Someone)"), base);
        assert_eq!(title_key("Hello [ft. Someone Else]"), base);
        assert_eq!(title_key("Hello feat. Someone"), base);
        assert_eq!(title_key("Hello (Remastered 2011)"), base);
        assert_eq!(title_key("Hello - 2011 Remaster"), base);
        assert_eq!(title_key("Hello (with Friend)"), base);
        // A live version is a different recording.
        assert_ne!(title_key("Hello (Live)"), base);
        // Never reduces a title to nothing.
        assert!(!title_key("(feat. X)").is_empty());
    }

    #[test]
    fn artist_keys_cover_featured_and_joined_credits() {
        assert_eq!(artist_keys("The Beatles"), ["beatles"]);
        assert_eq!(artist_keys("Beyoncé feat. Jay-Z"), ["beyonce"]);
        assert_eq!(artist_keys("Hall & Oates"), ["hall and oates", "hall"]);
        assert_eq!(artist_keys("A; B"), ["a b", "a"]);
        assert!(artist_keys("   ").is_empty());
    }

    #[test]
    fn album_key_ignores_edition_suffix() {
        assert_eq!(
            album_key("OK Computer (Deluxe Edition)"),
            album_key("ok computer")
        );
    }

    #[test]
    fn index_matches_loosely_and_prefers_the_same_album() {
        let index = TrackIndex::new(&[
            track(1, "Björk", "Hunter", "Homogenic"),
            track(2, "Björk", "Hunter", "Greatest Hits"),
            track(3, "Hall & Oates", "Maneater", "H2O"),
            track(4, "Beyoncé", "Halo", "I Am"),
        ]);
        assert_eq!(index.len(), 4);
        let id = |a, t, al| index.find(a, t, al).map(|m| m.track_id);
        assert_eq!(id("bjork", "hunter", "Greatest Hits"), Some(2));
        assert_eq!(id("BJÖRK", "Hunter!", "homogenic"), Some(1));
        // No album → first candidate; unknown album → first candidate too.
        assert_eq!(id("Björk", "Hunter", ""), Some(1));
        assert_eq!(id("Björk", "Hunter", "Other"), Some(1));
        // First credited artist and featured artists.
        assert_eq!(id("Hall", "Maneater", ""), Some(3));
        assert_eq!(id("Beyonce feat. Jay-Z", "Halo (Remastered)", ""), Some(4));
        // Different artist or title: no match.
        assert_eq!(id("Hall & Oates", "Halo", ""), None);
        assert_eq!(id("Nobody", "Hunter", ""), None);
        assert_eq!(id("Björk", "", ""), None);
    }

    #[test]
    fn dedupe_window_is_clamped() {
        assert_eq!(dedupe_window_secs(0), 120);
        assert_eq!(dedupe_window_secs(200), 200);
        assert_eq!(dedupe_window_secs(5000), 900);
    }

    #[test]
    fn rate_limit_waits_only_when_budget_is_spent() {
        assert_eq!(rate_limit_delay(Some(20), Some(8)), None);
        assert_eq!(rate_limit_delay(None, None), None);
        assert_eq!(
            rate_limit_delay(Some(0), Some(8)),
            Some(Duration::from_secs(9))
        );
        assert_eq!(rate_limit_delay(Some(1), Some(3600)), Some(MAX_RATE_WAIT));
    }

    #[test]
    fn parses_listenbrainz_listens() {
        let v = json!({"payload": {
        "count": 3, "oldest_listen_ts": 100,
        "listens": [
            {"listened_at": 300, "track_metadata": {
                "artist_name": "A", "track_name": "T", "release_name": "R"}},
            {"listened_at": 200, "track_metadata": {"artist_name": "B", "track_name": "U"}},
            {"listened_at": 150, "track_metadata": {"artist_name": "", "track_name": "V"}},
        ]}});
        let (listens, oldest, user_oldest) = parse_lb_listens(&v);
        assert_eq!(listens.len(), 2, "listen without artist is dropped");
        assert_eq!(listens[0].album, "R");
        assert_eq!(listens[1].album, "");
        assert_eq!(oldest, Some(150));
        assert_eq!(user_oldest, Some(100));
        assert_eq!(parse_lb_listens(&json!({})).1, None);
    }

    #[test]
    fn parses_recent_tracks_skipping_now_playing() {
        let v = json!({"recenttracks": {
        "@attr": {"page": "1", "totalPages": "7", "total": "1234"},
        "track": [
            {"name": "Live", "artist": {"#text": "A"}, "album": {"#text": "X"},
             "@attr": {"nowplaying": "true"}},
            {"name": "Done", "artist": {"#text": "B"}, "album": {"#text": ""},
             "date": {"uts": "1700000000", "#text": "14 Nov 2023"}},
        ]}});
        let (listens, pages, total) = parse_recent_tracks(&v);
        assert_eq!((pages, total), (7, Some(1234)));
        assert_eq!(listens.len(), 1);
        assert_eq!(listens[0].timestamp, 1_700_000_000);
        assert_eq!(listens[0].artist, "B");

        // A single track arrives as a bare object.
        let single = json!({"recenttracks": {"track": {
            "name": "Solo", "artist": {"#text": "C"}, "date": {"uts": "5"}}}});
        let (listens, pages, total) = parse_recent_tracks(&single);
        assert_eq!((listens.len(), pages, total), (1, 1, None));
    }

    /// In-memory source: fixed pages, no network.
    struct Pages(Vec<Vec<Listen>>);

    impl ListenSource for Pages {
        fn total(&mut self) -> Option<u64> {
            Some(self.0.iter().map(|p| p.len() as u64).sum())
        }

        fn next_page(&mut self, _: &AtomicBool) -> Result<Option<Vec<Listen>>, String> {
            Ok((!self.0.is_empty()).then(|| self.0.remove(0)))
        }
    }

    fn listen(artist: &str, title: &str, ts: i64) -> Listen {
        Listen {
            artist: artist.into(),
            title: title.into(),
            album: String::new(),
            timestamp: ts,
        }
    }

    fn library() -> (LibraryDb, i64) {
        use crate::library::Track;
        use std::sync::Arc;
        let db = LibraryDb::open_memory().unwrap();
        let t = Track {
            id: 0,
            path: "/m/hunter.flac".into(),
            title: "Hunter".into(),
            artist: "Björk".into(),
            album_artist: "Björk".into(),
            album: "Homogenic".into(),
            genre: String::new(),
            track_number: 1,
            disc_number: 1,
            year: 1997,
            duration: Duration::from_secs(255),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("local"),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        };
        db.upsert_track(&t, 1).unwrap();
        let id = db.import_tracks("local").unwrap()[0].id;
        (db, id)
    }

    fn pages() -> Pages {
        Pages(vec![
            vec![
                listen("bjork", "hunter", 5_000),
                listen("Nobody", "Song", 4_000),
            ],
            vec![
                listen("Björk", "Hunter (Remastered)", 1_000),
                listen("Björk", "Hunter", 0),
            ],
        ])
    }

    #[test]
    fn import_counts_and_is_idempotent() {
        let (db, id) = library();
        let cancel = AtomicBool::new(false);
        let mut reports = Vec::new();
        let out = run_import(&mut pages(), &db, &cancel, &mut |p| reports.push(p)).unwrap();
        assert!(!out.cancelled && out.error.is_none());
        let p = out.progress;
        assert_eq!(
            (p.fetched, p.matched, p.unmatched, p.imported, p.total),
            (4, 2, 2, 2, Some(4))
        );
        assert_eq!(p.skipped(), 0);
        assert_eq!(db.play_count(id).unwrap(), 2);
        assert_eq!(reports.len(), 3, "initial report + one per page");
        assert_eq!(reports.last().unwrap().fraction(), Some(1.0));

        // Re-import: everything matched is already there.
        let out = run_import(&mut pages(), &db, &cancel, &mut |_| {}).unwrap();
        assert_eq!((out.progress.matched, out.progress.imported), (2, 0));
        assert_eq!(out.progress.skipped(), 2);
        assert_eq!(db.play_count(id).unwrap(), 2);
    }

    #[test]
    fn cancelled_import_stops_before_reading() {
        let (db, id) = library();
        let cancel = AtomicBool::new(true);
        let out = run_import(&mut pages(), &db, &cancel, &mut |_| {}).unwrap();
        assert!(out.cancelled);
        assert_eq!(out.progress.fetched, 0);
        assert_eq!(db.play_count(id).unwrap(), 0);
    }

    #[test]
    fn source_failure_keeps_earlier_pages() {
        struct Flaky(u8);
        impl ListenSource for Flaky {
            fn next_page(&mut self, _: &AtomicBool) -> Result<Option<Vec<Listen>>, String> {
                self.0 += 1;
                match self.0 {
                    1 => Ok(Some(vec![listen("Björk", "Hunter", 9_000)])),
                    _ => Err("boom".into()),
                }
            }
        }
        let (db, id) = library();
        let out = run_import(&mut Flaky(0), &db, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(out.error.as_deref(), Some("boom"));
        assert_eq!(out.progress.imported, 1);
        assert_eq!(db.play_count(id).unwrap(), 1);
    }
}
