// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Internet radio directory search (radio-browser.info) and PLS/M3U
//! playlist resolution for Shoutcast/Icecast stations that publish a
//! playlist file instead of a direct stream URL, plus the blocking
//! saved-station operations over the [`OnlineStore`] (a UI runs them on a
//! worker thread).

use super::store::{NewStation, OnlineStore, RadioStation};
use serde::Deserialize;

/// A radio-browser.info directory search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationSearchResult {
    /// Radio-browser's own stable station id. Used as the row key for
    /// Discover results instead of a list position, since a slow query
    /// finishing late or a re-sort must never make an in-flight
    /// Play/Save click land on the wrong row.
    pub stationuuid: String,
    pub name: String,
    pub url: String,
    pub homepage: String,
    pub favicon: String,
    pub tags: String,
    pub codec: String,
    pub bitrate: u32,
    pub country: String,
    pub countrycode: String,
    pub language: String,
    pub votes: i64,
    pub clickcount: i64,
}

#[derive(Debug, Deserialize)]
struct StationRaw {
    #[serde(default)]
    stationuuid: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    url_resolved: String,
    #[serde(default)]
    homepage: String,
    #[serde(default)]
    favicon: String,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    codec: String,
    #[serde(default)]
    bitrate: u32,
    #[serde(default)]
    country: String,
    #[serde(default)]
    countrycode: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    votes: i64,
    #[serde(default)]
    clickcount: i64,
}

/// Sort order for [`search_stations`], mirroring radio-browser.info's
/// `order=` query parameter. Numeric orders are sent with `reverse=true`
/// so they read "highest/most-popular first"; `Name` stays ascending
/// (A→Z) — reversing it would list stations Z→A.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    Clickcount,
    Votes,
    Name,
    Bitrate,
}

impl SortOrder {
    /// Every order the Discover sort dropdown offers, in display order.
    pub const ALL: [SortOrder; 4] = [
        SortOrder::Clickcount,
        SortOrder::Votes,
        SortOrder::Name,
        SortOrder::Bitrate,
    ];

    fn as_param(self) -> &'static str {
        match self {
            SortOrder::Clickcount => "clickcount",
            SortOrder::Votes => "votes",
            SortOrder::Name => "name",
            SortOrder::Bitrate => "bitrate",
        }
    }

    /// Localised dropdown label.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            SortOrder::Clickcount => crate::fl!("radio-sort-most-played"),
            SortOrder::Votes => crate::fl!("radio-sort-most-voted"),
            SortOrder::Name => crate::fl!("radio-sort-name"),
            SortOrder::Bitrate => crate::fl!("radio-sort-bitrate"),
        }
    }

    /// Position in [`SortOrder::ALL`] (the dropdown's selected index).
    #[must_use]
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|o| *o == self).unwrap_or(0)
    }

    /// Whether radio-browser should reverse its natural ascending order.
    fn descending(self) -> bool {
        !matches!(self, SortOrder::Name)
    }
}

/// Which of the two Radio tabs is shown. Kept in `AppModel` for the
/// session (not persisted to disk) so navigating away and back preserves
/// the tab the user was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RadioTab {
    #[default]
    MyStations,
    Discover,
}

/// A one-shot Discover preset that bypasses the free-text/tag/country
/// query and fetches a globally ranked list instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverPreset {
    Popular,
    TopVoted,
}

/// Quick-tag chips offered above the Discover results. A fixed, curated
/// set rather than a fetched tag cloud -- covers common genres without a
/// network round trip just to populate the chip row.
pub const QUICK_TAGS: [&str; 9] = [
    "Pop",
    "Rock",
    "Jazz",
    "Classical",
    "Electronic",
    "News",
    "Talk",
    "Ambient",
    "Lo-fi",
];

/// First `n` non-empty comma-separated tags.
#[must_use]
pub fn tag_list(tags: &str, n: usize) -> Vec<String> {
    tags.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .take(n)
        .map(|t| crate::format::truncate_str(t, 18))
        .collect()
}

/// Parameters for a directory search against `/json/stations/search`.
///
/// `name`/`tag`/`countrycode` left empty are omitted from the request
/// entirely rather than sent as `name=` -- an empty value narrows radio-
/// browser's search to stations with a literally empty field, which is
/// never what an empty UI field means.
#[derive(Debug, Clone, Default)]
pub struct StationQuery {
    pub name: String,
    pub tag: String,
    pub countrycode: String,
    pub order: SortOrder,
    pub limit: u32,
}

/// Cap on the radio-browser.info JSON response body. The directory's own
/// `limit=`/count params bound normal responses to well under this, so
/// hitting the cap means a hostile or broken server, not a legitimate
/// large result set.
const MAX_JSON_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// Build the `/json/stations/search` request URL for `query`. Split out
/// from [`search_stations`] so the query-string assembly (which params get
/// included/omitted) is unit-testable without a network round trip.
fn build_search_url(query: &StationQuery) -> String {
    let mut url = format!(
        "https://all.api.radio-browser.info/json/stations/search?hidebroken=true&reverse={}&limit={}&order={}",
        query.order.descending(),
        query.limit.max(1),
        query.order.as_param()
    );
    if !query.name.trim().is_empty() {
        url.push_str("&name=");
        url.push_str(&urlencoding::encode(query.name.trim()));
    }
    if !query.tag.trim().is_empty() {
        url.push_str("&tag=");
        url.push_str(&urlencoding::encode(query.tag.trim()));
    }
    if !query.countrycode.trim().is_empty() {
        url.push_str("&countrycode=");
        url.push_str(&urlencoding::encode(query.countrycode.trim()));
    }
    url
}

/// Search the radio-browser.info station directory by name/tag/country,
/// sorted by `query.order` (most-popular/matching first).
pub fn search_stations(
    client: &reqwest::blocking::Client,
    query: &StationQuery,
) -> Result<Vec<StationSearchResult>, String> {
    fetch_stations(client, &build_search_url(query))
}

/// Fetch the globally most-clicked stations, for a Discover "Popular"
/// preset that needs no search terms.
pub fn top_click_stations(
    client: &reqwest::blocking::Client,
    limit: u32,
) -> Result<Vec<StationSearchResult>, String> {
    fetch_stations(client, &top_click_url(limit))
}

/// Fetch the globally top-voted stations, for a Discover "Top voted" preset.
pub fn top_vote_stations(
    client: &reqwest::blocking::Client,
    limit: u32,
) -> Result<Vec<StationSearchResult>, String> {
    fetch_stations(client, &top_vote_url(limit))
}

/// How many results a Discover search/preset fetch asks for.
pub const DISCOVER_LIMIT: u32 = 50;

/// Searches the directory with the shared HTTP client (see
/// [`search_stations`]).
pub fn search(query: &StationQuery) -> Result<Vec<StationSearchResult>, String> {
    search_stations(&super::HTTP_CLIENT, query)
}

/// The Discover "Popular" preset: the [`DISCOVER_LIMIT`] most-clicked stations.
pub fn popular() -> Result<Vec<StationSearchResult>, String> {
    top_click_stations(&super::HTTP_CLIENT, DISCOVER_LIMIT)
}

/// The Discover "Top voted" preset: the [`DISCOVER_LIMIT`] top-voted stations.
pub fn top_voted() -> Result<Vec<StationSearchResult>, String> {
    top_vote_stations(&super::HTTP_CLIENT, DISCOVER_LIMIT)
}

/// Resolves a station URL to a playable stream with the shared HTTP client,
/// following a `.pls`/`.m3u`/`.m3u8` playlist if needed (see
/// [`resolve_stream_url`]).
pub fn resolve_stream(url: &str) -> Result<String, String> {
    resolve_stream_url(&super::HTTP_CLIENT, url)
}

/// Derives a two-letter upper-case country code from the `LC_ALL`/`LANG`
/// locale (e.g. `en_US.UTF-8` -> `US`), or an empty string when it can't
/// be determined. Read from the process environment -- never a network
/// lookup.
#[must_use]
pub fn locale_country_code() -> String {
    for key in ["LC_ALL", "LANG"] {
        if let Ok(v) = std::env::var(key)
            && let Some(region) = v.split('.').next().and_then(|l| l.split('_').nth(1))
            && region.len() == 2
            && region.chars().all(|c| c.is_ascii_alphabetic())
        {
            return region.to_ascii_uppercase();
        }
    }
    String::new()
}

/// Loads the saved stations from the online store; a store error is logged
/// and reads as "no stations" so the page can still render.
#[must_use]
pub fn load_saved() -> Vec<RadioStation> {
    OnlineStore::open_default()
        .and_then(|store| store.list_radio_stations())
        .unwrap_or_else(|e| {
            tracing::warn!("list_radio_stations failed: {e}");
            Vec::new()
        })
}

/// Codec and bitrate a stream announces in its response headers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamFormat {
    /// Upper-case codec name ("MP3", "AAC", …); empty when unknown.
    pub codec: String,
    /// Bitrate in kbit/s (`icy-br`); `0` when unknown.
    pub bitrate_kbps: u32,
}

impl StreamFormat {
    /// Whether neither the codec nor the bitrate is known.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        self.codec.is_empty() && self.bitrate_kbps == 0
    }

    /// Reads the ICY bitrate headers (`icy-br` and friends) and maps the
    /// `Content-Type` to a codec name.
    #[must_use]
    pub fn from_headers(headers: &reqwest::header::HeaderMap) -> Self {
        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        Self {
            codec: codec_from_content_type(content_type).to_owned(),
            bitrate_kbps: icy_metadata::IcyHeaders::parse_from_headers(headers)
                .bitrate()
                .unwrap_or(0),
        }
    }
}

/// Short stream-format label for a station row: "AAC 128", "FLAC" or "128"
/// (codec and/or kbit/s); empty when neither is known.
#[must_use]
pub fn format_label(codec: &str, bitrate_kbps: u32) -> String {
    let mut label = codec.to_uppercase();
    if bitrate_kbps > 0 {
        if !label.is_empty() {
            label.push(' ');
        }
        label.push_str(&bitrate_kbps.to_string());
    }
    label
}

/// Codec name for a stream `Content-Type`; empty when it is not a known
/// audio type.
fn codec_from_content_type(content_type: &str) -> &'static str {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "audio/mpeg" | "audio/mp3" => "MP3",
        "audio/aacp" => "AAC+",
        "audio/aac" | "audio/x-aac" | "audio/mp4" | "audio/x-m4a" => "AAC",
        "audio/ogg" | "application/ogg" => "OGG",
        "audio/opus" => "OPUS",
        "audio/flac" | "audio/x-flac" => "FLAC",
        _ => "",
    }
}

/// How long [`probe_stream_format`] waits for a station's response headers.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Opens `url` just long enough to read the response headers and reports
/// the stream's announced codec and bitrate (the body is never read). A
/// `.pls`/`.m3u` playlist is resolved first.
pub fn probe_stream_format(url: &str) -> Result<StreamFormat, String> {
    let stream_url = resolve_stream(url)?;
    let response = super::HTTP_CLIENT
        .get(&stream_url)
        .timeout(PROBE_TIMEOUT)
        .send()
        .map_err(|e| format!("Stream probe failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Stream probe returned HTTP {}", response.status()));
    }
    Ok(StreamFormat::from_headers(response.headers()))
}

/// Saves a station (or refreshes it when its stream URL is already saved).
/// A station without a known codec/bitrate (added by URL) is probed first
/// (best effort: an unreachable stream is saved without a format).
/// Returns the row id and whether the station already existed.
pub fn save_station(mut station: NewStation) -> Result<(i64, bool), String> {
    if station.codec.is_empty() && station.bitrate_kbps == 0 {
        match probe_stream_format(&station.stream_url) {
            Ok(format) => {
                station.codec = format.codec;
                station.bitrate_kbps = format.bitrate_kbps;
            }
            Err(e) => tracing::debug!("radio format probe skipped: {e}"),
        }
    }
    OnlineStore::open_default().and_then(|store| store.add_radio_station(&station))
}

/// Learns the format of the saved station whose stream URL is `key` from a
/// probe, when the store does not know it yet. Returns the reloaded list
/// when the store changed, `None` when there was nothing to learn.
pub fn learn_station_format(key: &str) -> Result<Option<Vec<RadioStation>>, String> {
    let store = OnlineStore::open_default()?;
    let known = store
        .list_radio_stations()?
        .into_iter()
        .any(|s| s.stream_url == key && !(s.codec.is_empty() && s.bitrate_kbps == 0));
    if known {
        return Ok(None);
    }
    let format = probe_stream_format(key)?;
    if format.is_unknown()
        || !store.set_radio_station_format(key, &format.codec, format.bitrate_kbps)?
    {
        return Ok(None);
    }
    store.list_radio_stations().map(Some)
}

/// Removes a saved station and returns the remaining list.
pub fn remove_saved(id: i64) -> Result<Vec<RadioStation>, String> {
    let store = OnlineStore::open_default()?;
    store.remove_radio_station(id)?;
    store.list_radio_stations()
}

/// Renames a saved station and returns the updated list.
pub fn rename_saved(id: i64, name: &str) -> Result<Vec<RadioStation>, String> {
    let store = OnlineStore::open_default()?;
    store.rename_radio_station(id, name)?;
    store.list_radio_stations()
}

fn top_click_url(limit: u32) -> String {
    format!(
        "https://all.api.radio-browser.info/json/stations/topclick/{}?hidebroken=true",
        limit.max(1)
    )
}

fn top_vote_url(limit: u32) -> String {
    format!(
        "https://all.api.radio-browser.info/json/stations/topvote/{}?hidebroken=true",
        limit.max(1)
    )
}

fn fetch_stations(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<Vec<StationSearchResult>, String> {
    let response = client
        .get(url)
        .header("User-Agent", "aulos/0.1")
        .send()
        .map_err(|e| format!("Radio search failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("Radio search returned HTTP {}", response.status()));
    }
    let body = super::read_capped_body(response, MAX_JSON_RESPONSE_BYTES)?;
    let raw: Vec<StationRaw> =
        serde_json::from_slice(&body).map_err(|e| format!("Radio response parse failed: {e}"))?;
    Ok(map_station_results(raw))
}

fn map_station_results(raw: Vec<StationRaw>) -> Vec<StationSearchResult> {
    raw.into_iter()
        .map(|s| {
            let url = if s.url_resolved.is_empty() {
                s.url
            } else {
                s.url_resolved
            };
            StationSearchResult {
                stationuuid: s.stationuuid,
                name: s.name,
                url,
                homepage: s.homepage,
                favicon: s.favicon,
                tags: s.tags,
                codec: s.codec,
                bitrate: s.bitrate,
                country: s.country,
                countrycode: s.countrycode,
                language: s.language,
                votes: s.votes,
                clickcount: s.clickcount,
            }
        })
        .filter(|s| !s.url.is_empty())
        .collect()
}

/// Playlist container format, detected by file extension or content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistFormat {
    Pls,
    M3u,
}

/// Detect the playlist format of `url` from its extension, falling back to
/// `content_type` (e.g. from a `Content-Type` response header) when the
/// extension is inconclusive. Returns `None` when neither indicates a
/// playlist container, meaning `url` is presumably already a direct stream.
pub fn sniff_playlist_format(url: &str, content_type: Option<&str>) -> Option<PlaylistFormat> {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    if path.ends_with(".pls") {
        return Some(PlaylistFormat::Pls);
    }
    if path.ends_with(".m3u") || path.ends_with(".m3u8") {
        return Some(PlaylistFormat::M3u);
    }
    let ct = content_type?.to_ascii_lowercase();
    if ct.contains("scpls") {
        Some(PlaylistFormat::Pls)
    } else if ct.contains("mpegurl") {
        Some(PlaylistFormat::M3u)
    } else {
        None
    }
}

/// Extract the lowest-numbered `FileN=` entry from a PLS playlist body.
pub fn parse_pls(body: &str) -> Option<String> {
    let mut best: Option<(u32, String)> = None;
    for line in body.lines() {
        let line = line.trim();
        let Some(eq_idx) = line.find('=') else {
            continue;
        };
        let key = &line[..eq_idx];
        // `key.get(..4)`/`key.get(4..)` (not byte-slicing) so a key
        // containing multi-byte UTF-8 within its first 4 bytes — from a
        // malformed/hostile playlist — can never land mid-character and
        // panic; it just fails to match "file" and the line is skipped.
        let Some(prefix) = key.get(..4) else { continue };
        if !prefix.eq_ignore_ascii_case("file") {
            continue;
        }
        let Some(suffix) = key.get(4..) else { continue };
        let Ok(n) = suffix.parse::<u32>() else {
            continue;
        };
        let value = line[eq_idx + 1..].trim();
        if value.is_empty() {
            continue;
        }
        if best.as_ref().is_none_or(|(best_n, _)| n < *best_n) {
            best = Some((n, value.to_string()));
        }
    }
    best.map(|(_, v)| v)
}

/// Extract the first non-comment, non-blank line from an M3U playlist body.
pub fn parse_m3u(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
}

/// Parse a playlist body of the given format into its first stream URL.
pub fn parse_playlist(body: &str, format: PlaylistFormat) -> Option<String> {
    match format {
        PlaylistFormat::Pls => parse_pls(body),
        PlaylistFormat::M3u => parse_m3u(body),
    }
}

/// Maximum number of chained playlist fetches `resolve_stream_url` will
/// follow. Some Shoutcast/Icecast directories publish a playlist whose
/// first (lowest-numbered/first-listed) entry is itself another playlist —
/// e.g. a `.pls` wrapping a mirrored `.m3u` — so one fetch-and-parse pass
/// doesn't always land on a direct stream. The cap bounds the number of
/// network round-trips so a pathological or self-referential chain can
/// never recurse indefinitely or block the caller forever.
const MAX_PLAYLIST_HOPS: u32 = 3;

/// Resolve a station URL to a directly playable stream URL. If `url` looks
/// like a `.pls`/`.m3u`/`.m3u8` playlist (by extension, or by the response's
/// `Content-Type` once fetched), fetches it and extracts the first stream
/// entry; otherwise returns `url` unchanged. Some directories chain
/// playlists (the extracted entry is itself another playlist), so this
/// repeats the fetch-and-extract step for up to `MAX_PLAYLIST_HOPS` hops,
/// stopping as soon as a hop's result no longer looks like a playlist. If
/// the chain is still unresolved at the cap, that's a clear error rather
/// than silently handing back a playlist URL as if it were a direct stream.
pub fn resolve_stream_url(client: &reqwest::blocking::Client, url: &str) -> Result<String, String> {
    /// Playlist bodies (.pls/.m3u) are always small hand-written text
    /// files; a hostile or broken server returning an unbounded body must
    /// not be read into memory in full.
    const MAX_PLAYLIST_RESPONSE_BYTES: u64 = 1024 * 1024;

    let mut current = url.to_string();
    for _ in 0..MAX_PLAYLIST_HOPS {
        let Some(extension_hint) = sniff_playlist_format(&current, None) else {
            return Ok(current);
        };
        let response = client
            .get(&current)
            .send()
            .map_err(|e| format!("Failed to fetch playlist: {e}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "Failed to fetch playlist: HTTP {}",
                response.status()
            ));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let format =
            sniff_playlist_format(&current, content_type.as_deref()).unwrap_or(extension_hint);
        let bytes = super::read_capped_body(response, MAX_PLAYLIST_RESPONSE_BYTES)?;
        let body =
            String::from_utf8(bytes).map_err(|e| format!("Playlist was not valid UTF-8: {e}"))?;
        current = parse_playlist(&body, format)
            .ok_or_else(|| "Playlist contained no stream URL".to_string())?;
    }
    if sniff_playlist_format(&current, None).is_some() {
        Err(format!(
            "Playlist resolution exceeded {MAX_PLAYLIST_HOPS} hops without reaching a stream URL"
        ))
    } else {
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_playlist_format_from_extension() {
        assert_eq!(
            sniff_playlist_format("https://x.example/station.pls", None),
            Some(PlaylistFormat::Pls)
        );
        assert_eq!(
            sniff_playlist_format("https://x.example/station.m3u8?x=1", None),
            Some(PlaylistFormat::M3u)
        );
        assert_eq!(sniff_playlist_format("https://x.example/live", None), None);
    }

    #[test]
    fn sniff_playlist_format_from_content_type() {
        assert_eq!(
            sniff_playlist_format(
                "https://x.example/live",
                Some("audio/x-scpls; charset=utf-8")
            ),
            Some(PlaylistFormat::Pls)
        );
        assert_eq!(
            sniff_playlist_format("https://x.example/live", Some("audio/x-mpegurl")),
            Some(PlaylistFormat::M3u)
        );
        assert_eq!(
            sniff_playlist_format("https://x.example/live", Some("audio/mpeg")),
            None
        );
    }

    #[test]
    fn parse_pls_picks_lowest_numbered_file_entry() {
        let body = "[playlist]\nNumberOfEntries=2\nFile2=https://x.example/b.mp3\nFile1=https://x.example/a.mp3\nTitle1=A\nVersion=2\n";
        assert_eq!(parse_pls(body), Some("https://x.example/a.mp3".to_string()));
    }

    #[test]
    fn parse_pls_is_case_insensitive_and_ignores_other_keys() {
        let body = "[Playlist]\nfile1=https://x.example/a.mp3\nLength1=-1\n";
        assert_eq!(parse_pls(body), Some("https://x.example/a.mp3".to_string()));
    }

    #[test]
    fn parse_pls_returns_none_without_file_entries() {
        assert_eq!(parse_pls("[playlist]\nNumberOfEntries=0\n"), None);
    }

    #[test]
    fn parse_pls_ignores_multibyte_key_without_panicking() {
        // A hostile/malformed playlist could put multi-byte UTF-8 right
        // before the first 4 bytes of a key; byte-slicing at index 4 would
        // land mid-character and panic. This must just skip the line.
        let body = "fïle1=https://x.example/a.mp3\nFile1=https://x.example/b.mp3\n";
        assert_eq!(parse_pls(body), Some("https://x.example/b.mp3".to_string()));
    }

    #[test]
    fn parse_m3u_skips_comments_and_blank_lines() {
        let body = "#EXTM3U\n#EXTINF:-1,Station Name\n\nhttps://x.example/stream.mp3\n";
        assert_eq!(
            parse_m3u(body),
            Some("https://x.example/stream.mp3".to_string())
        );
    }

    #[test]
    fn parse_m3u_returns_none_for_comments_only() {
        assert_eq!(parse_m3u("#EXTM3U\n#EXTINF:-1,Nothing\n"), None);
    }

    #[test]
    fn radio_browser_json_maps_url_resolved_with_fallback() {
        const BODY: &str = r#"[
            {"name": "Resolved Station", "url": "http://x.example/orig", "url_resolved": "http://x.example/resolved",
             "homepage": "http://x.example", "favicon": "http://x.example/favicon.ico",
             "tags": "jazz,chill", "codec": "MP3", "bitrate": 128},
            {"name": "Unresolved Station", "url": "http://y.example/live", "url_resolved": "",
             "tags": "", "codec": "AAC", "bitrate": 64},
            {"name": "No URL At All", "url": "", "url_resolved": ""}
        ]"#;
        let raw: Vec<StationRaw> = serde_json::from_str(BODY).unwrap();
        let results = map_station_results(raw);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "http://x.example/resolved");
        assert_eq!(results[0].bitrate, 128);
        assert_eq!(results[1].url, "http://y.example/live");
        assert_eq!(results[1].codec, "AAC");
    }

    #[test]
    fn radio_browser_json_maps_full_extended_field_set() {
        const BODY: &str = r#"[
            {"stationuuid": "abc-123", "name": "Full Station", "url": "http://x.example/orig",
             "url_resolved": "http://x.example/resolved", "homepage": "http://x.example",
             "favicon": "http://x.example/favicon.ico", "tags": "jazz,chill", "codec": "MP3",
             "bitrate": 128, "country": "Italy", "countrycode": "IT", "language": "italian",
             "votes": 42, "clickcount": 999}
        ]"#;
        let raw: Vec<StationRaw> = serde_json::from_str(BODY).unwrap();
        let results = map_station_results(raw);
        assert_eq!(results.len(), 1);
        let s = &results[0];
        assert_eq!(s.stationuuid, "abc-123");
        assert_eq!(s.country, "Italy");
        assert_eq!(s.countrycode, "IT");
        assert_eq!(s.language, "italian");
        assert_eq!(s.votes, 42);
        assert_eq!(s.clickcount, 999);
    }

    #[test]
    fn radio_browser_json_defaults_missing_extended_fields() {
        // Older/partial responses that omit the newer fields entirely
        // must still parse, defaulting to empty/zero rather than failing.
        const BODY: &str = r#"[{"name": "Bare Station", "url": "http://x.example/live"}]"#;
        let raw: Vec<StationRaw> = serde_json::from_str(BODY).unwrap();
        let results = map_station_results(raw);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].stationuuid, "");
        assert_eq!(results[0].votes, 0);
        assert_eq!(results[0].clickcount, 0);
    }

    #[test]
    fn build_search_url_omits_empty_params_but_always_has_order_and_limit() {
        let query = StationQuery {
            limit: 25,
            order: SortOrder::Votes,
            ..Default::default()
        };
        let url = build_search_url(&query);
        assert!(url.contains("hidebroken=true"));
        assert!(url.contains("reverse=true"));
        assert!(url.contains("limit=25"));
        assert!(url.contains("order=votes"));
        assert!(!url.contains("name="));
        assert!(!url.contains("tag="));
        assert!(!url.contains("countrycode="));
    }

    #[test]
    fn build_search_url_includes_name_tag_and_countrycode_when_set() {
        let query = StationQuery {
            name: "Radio Paradise".to_string(),
            tag: "chill out".to_string(),
            countrycode: "US".to_string(),
            order: SortOrder::Name,
            limit: 10,
        };
        let url = build_search_url(&query);
        assert!(url.contains("name=Radio%20Paradise"));
        assert!(url.contains("tag=chill%20out"));
        assert!(url.contains("countrycode=US"));
        assert!(url.contains("order=name"));
        // Name sorts A→Z; only numeric orders are reversed.
        assert!(url.contains("reverse=false"));
    }

    #[test]
    fn build_search_url_clamps_zero_limit_to_one() {
        let query = StationQuery {
            limit: 0,
            ..Default::default()
        };
        assert!(build_search_url(&query).contains("limit=1"));
    }

    #[test]
    fn top_click_and_top_vote_urls_hit_the_expected_global_endpoints() {
        assert_eq!(
            top_click_url(50),
            "https://all.api.radio-browser.info/json/stations/topclick/50?hidebroken=true"
        );
        assert_eq!(
            top_vote_url(50),
            "https://all.api.radio-browser.info/json/stations/topvote/50?hidebroken=true"
        );
        // Never request a zero-sized list.
        assert!(top_click_url(0).contains("/topclick/1?"));
    }

    #[test]
    fn stream_format_reads_icy_bitrate_and_content_type() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "audio/mpeg".parse().unwrap());
        headers.insert("icy-br", "128".parse().unwrap());
        assert_eq!(
            StreamFormat::from_headers(&headers),
            StreamFormat {
                codec: "MP3".into(),
                bitrate_kbps: 128
            }
        );

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "audio/aacp; charset=x".parse().unwrap());
        let format = StreamFormat::from_headers(&headers);
        assert_eq!(format.codec, "AAC+");
        assert_eq!(format.bitrate_kbps, 0);
        assert!(!format.is_unknown());
    }

    #[test]
    fn stream_format_is_unknown_without_audio_headers() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-type", "text/html".parse().unwrap());
        assert!(StreamFormat::from_headers(&headers).is_unknown());
        assert_eq!(format_label("aac", 128), "AAC 128");
        assert_eq!(format_label("FLAC", 0), "FLAC");
        assert_eq!(format_label("", 96), "96");
        assert_eq!(format_label("", 0), "");
        assert_eq!(codec_from_content_type("audio/ogg"), "OGG");
        assert_eq!(codec_from_content_type("Audio/FLAC"), "FLAC");
    }
}
