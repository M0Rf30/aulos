// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Internet radio directory search (radio-browser.info) and PLS/M3U
//! playlist resolution for Shoutcast/Icecast stations that publish a
//! playlist file instead of a direct stream URL.

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

    /// Whether radio-browser should reverse its natural ascending order.
    fn descending(self) -> bool {
        !matches!(self, SortOrder::Name)
    }
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
}
