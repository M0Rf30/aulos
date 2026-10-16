// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! MusicBrainz client for the tag editor: release search, release and
//! recording lookups, Picard-style tag mapping, file-to-track matching,
//! Cover Art Archive downloads and "add release" seeding.
//!
//! Every function here is blocking (a shared `reqwest::blocking` client,
//! like the scrobblers); call them from a worker thread. All web-service
//! calls go through one process-wide throttle of one request per second —
//! MusicBrainz's documented limit for anonymous clients — so concurrent
//! callers queue instead of getting `503`d.

mod cover;
mod matching;
mod model;
mod seed;
mod tags;

use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use web_time::Instant;

pub use cover::fetch_front_cover;
pub use matching::{FileForMatch, MIN_MATCH_SCORE, TrackMatch, match_files, title_similarity};
pub use model::{
    ArtistRef, CoverArtArchive, CreditName, Label, LabelInfo, Medium, Recording, Release,
    ReleaseEvent, ReleaseGroup, TextRepresentation, Track, credit_artist_ids, credit_artist_names,
    credit_sort_text, credit_text,
};
pub use seed::{seed_fields, seed_html, seed_release_add};

use model::{ErrorResponse, SearchResponse, non_empty};

const API_BASE: &str = "https://musicbrainz.org/ws/2";
const SITE_BASE: &str = "https://musicbrainz.org";
const MIN_REQUEST_GAP: Duration = Duration::from_secs(1);
/// A release with a few hundred tracks and credits is ~1 MB of JSON.
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
/// Releases returned per search; the web service's default is 25.
const SEARCH_LIMIT: u32 = 25;

/// Shared blocking client. MusicBrainz asks every application to send a
/// descriptive `User-Agent` with a contact URL and blocks generic ones.
pub(crate) fn http_client() -> &'static crate::http::blocking::Client {
    static CLIENT: LazyLock<crate::http::blocking::Client> = LazyLock::new(|| {
        crate::http::blocking_builder()
            .user_agent(format!(
                "Aulos/{} ( https://github.com/M0Rf30/aulos )",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("MusicBrainz HTTP client config is static and valid")
    });
    &CLIENT
}

/// Blocks until at least [`MIN_REQUEST_GAP`] has passed since the previous
/// web-service request. The lock is held while sleeping on purpose: that
/// is what serialises concurrent callers.
fn throttle() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = *last {
        let elapsed = prev.elapsed();
        if elapsed < MIN_REQUEST_GAP {
            std::thread::sleep(MIN_REQUEST_GAP - elapsed);
        }
    }
    *last = Some(Instant::now());
}

/// `GET`s a web-service URL (JSON), returning the body or MusicBrainz's own
/// error message.
fn get_json(url: &str) -> Result<String, String> {
    throttle();
    let response = http_client()
        .get(url)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("MusicBrainz request failed: {e}"))?;
    let status = response.status();
    let body = crate::online::read_capped_body(response, MAX_RESPONSE_BYTES)?;
    let text = String::from_utf8_lossy(&body);
    if status.is_success() {
        return Ok(text.into_owned());
    }
    let detail = serde_json::from_str::<ErrorResponse>(&text)
        .ok()
        .and_then(|e| e.error)
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("request failed")
                .to_owned()
        });
    if status.as_u16() == 503 {
        return Err(format!(
            "MusicBrainz is rate limiting or unavailable (HTTP 503): {detail}"
        ));
    }
    Err(format!(
        "MusicBrainz error (HTTP {}): {detail}",
        status.as_u16()
    ))
}

/// Whether `s` has the shape of an MBID (8-4-4-4-12 hex digits). Checked
/// before an MBID is spliced into a URL path.
#[must_use]
pub fn is_mbid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn require_mbid(what: &str, s: &str) -> Result<(), String> {
    if is_mbid(s) {
        Ok(())
    } else {
        Err(format!("'{s}' is not a valid MusicBrainz {what} id"))
    }
}

/// Escapes Lucene query syntax (`+ - & | ! ( ) { } [ ] ^ " ~ * ? : \ /`)
/// so user text is matched literally.
#[must_use]
pub fn escape_lucene(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if "+-&|!(){}[]^\"~*?:\\/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// What to search releases for. Every non-empty field narrows the search
/// (they are ANDed), so leave out whatever is unreliable — for example the
/// track count of an incomplete folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseQuery {
    pub artist: String,
    pub album: String,
    /// Total tracks over all media.
    pub track_count: Option<u32>,
    pub barcode: Option<String>,
    pub catalog_number: Option<String>,
}

impl ReleaseQuery {
    /// The Lucene query string, or `None` when nothing is searchable.
    #[must_use]
    pub fn lucene(&self) -> Option<String> {
        fn words(s: &str) -> String {
            s.split_whitespace()
                .map(escape_lucene)
                .collect::<Vec<_>>()
                .join(" ")
        }
        let mut clauses = Vec::new();
        let album = words(&self.album);
        if !album.is_empty() {
            clauses.push(format!("release:({album})"));
        }
        let artist = words(&self.artist);
        if !artist.is_empty() {
            clauses.push(format!("artist:({artist})"));
        }
        if let Some(n) = self.track_count.filter(|&n| n > 0) {
            clauses.push(format!("tracks:{n}"));
        }
        if let Some(b) = non_empty(self.barcode.as_deref()) {
            let digits: String = b.chars().filter(char::is_ascii_alphanumeric).collect();
            if !digits.is_empty() {
                clauses.push(format!("barcode:{digits}"));
            }
        }
        if let Some(c) = non_empty(self.catalog_number.as_deref()) {
            clauses.push(format!("catno:\"{}\"", escape_lucene(c)));
        }
        (!clauses.is_empty()).then(|| clauses.join(" AND "))
    }
}

/// One search hit, flattened for display: empty strings mean "unknown".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseCandidate {
    pub id: String,
    pub title: String,
    /// Credited release artist.
    pub artist: String,
    pub date: String,
    pub country: String,
    pub label: String,
    pub catalog_number: String,
    pub barcode: String,
    /// Media formats, e.g. `2×CD` or `CD + DVD-Video`.
    pub formats: String,
    pub track_count: u32,
    /// Number of media (discs).
    pub disc_count: u32,
    pub status: String,
    pub primary_type: String,
    pub secondary_types: Vec<String>,
    pub disambiguation: String,
    /// Search relevance, 0-100 (0 when the release didn't come from a search).
    pub score: u32,
}

impl ReleaseCandidate {
    /// Flattens a (possibly partial) release.
    #[must_use]
    pub fn from_release(r: &Release) -> Self {
        let label = r
            .label_info
            .iter()
            .filter_map(|l| l.label.as_ref())
            .map(|l| l.name.as_str())
            .filter(|n| !n.is_empty())
            .fold(Vec::<&str>::new(), |mut v, n| {
                if !v.contains(&n) {
                    v.push(n);
                }
                v
            })
            .join(", ");
        let catalog_number = r
            .label_info
            .iter()
            .filter_map(|l| non_empty(l.catalog_number.as_deref()))
            .fold(Vec::<&str>::new(), |mut v, n| {
                if !v.contains(&n) {
                    v.push(n);
                }
                v
            })
            .join(", ");
        let group = r.release_group.as_ref();
        Self {
            id: r.id.clone(),
            title: r.title.clone(),
            artist: credit_text(&r.artist_credit),
            date: r.date_text().unwrap_or_default().to_owned(),
            country: r.countries().join(", "),
            label,
            catalog_number,
            barcode: non_empty(r.barcode.as_deref())
                .unwrap_or_default()
                .to_owned(),
            formats: formats_summary(&r.media),
            track_count: u32::try_from(r.track_count()).unwrap_or(u32::MAX),
            disc_count: u32::try_from(r.media.len()).unwrap_or(u32::MAX),
            status: r.status.clone().unwrap_or_default(),
            primary_type: group
                .and_then(|g| g.primary_type.clone())
                .unwrap_or_default(),
            secondary_types: group.map(|g| g.secondary_types.clone()).unwrap_or_default(),
            disambiguation: r.disambiguation.clone().unwrap_or_default(),
            score: r.score.unwrap_or(0),
        }
    }
}

/// `2×CD`, `CD + DVD-Video`, `12" Vinyl`: media formats in order of first
/// appearance with their counts.
fn formats_summary(media: &[Medium]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for m in media {
        let f = non_empty(m.format.as_deref()).unwrap_or("[unknown]");
        match counts.iter_mut().find(|(name, _)| *name == f) {
            Some((_, n)) => *n += 1,
            None => counts.push((f, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(f, n)| {
            if n > 1 {
                format!("{n}×{f}")
            } else {
                f.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Searches releases, best match first.
pub fn search_releases(query: &ReleaseQuery) -> Result<Vec<ReleaseCandidate>, String> {
    let lucene = query.lucene().ok_or_else(|| {
        "Nothing to search for: give an artist, album, barcode or catalog number".to_owned()
    })?;
    let url = format!(
        "{API_BASE}/release/?query={}&fmt=json&limit={SEARCH_LIMIT}",
        urlencoding::encode(&lucene)
    );
    parse_search(&get_json(&url)?)
}

fn parse_search(json: &str) -> Result<Vec<ReleaseCandidate>, String> {
    let response: SearchResponse = serde_json::from_str(json)
        .map_err(|e| format!("Unexpected MusicBrainz search response: {e}"))?;
    Ok(response
        .releases
        .iter()
        .map(ReleaseCandidate::from_release)
        .collect())
}

/// Fetches a release with everything the tag mapping needs: media and
/// tracks, recordings with ISRCs, artist credits, labels and the release
/// group.
pub fn release(mbid: &str) -> Result<Release, String> {
    require_mbid("release", mbid)?;
    let url = format!(
        "{API_BASE}/release/{mbid}?inc=artist-credits+labels+recordings+release-groups+media+isrcs&fmt=json"
    );
    parse_release(&get_json(&url)?)
}

fn parse_release(json: &str) -> Result<Release, String> {
    serde_json::from_str(json).map_err(|e| format!("Unexpected MusicBrainz release response: {e}"))
}

/// Fetches a recording with its artist credit, ISRCs and the releases it
/// appears on (what an AcoustID hit needs to resolve to a release).
pub fn recording_lookup(mbid: &str) -> Result<Recording, String> {
    require_mbid("recording", mbid)?;
    let url = format!(
        "{API_BASE}/recording/{mbid}?inc=artist-credits+releases+release-groups+isrcs&fmt=json"
    );
    parse_recording(&get_json(&url)?)
}

fn parse_recording(json: &str) -> Result<Recording, String> {
    serde_json::from_str(json)
        .map_err(|e| format!("Unexpected MusicBrainz recording response: {e}"))
}

impl Recording {
    /// The releases this recording appears on, flattened for display.
    #[must_use]
    pub fn release_candidates(&self) -> Vec<ReleaseCandidate> {
        self.releases
            .iter()
            .map(ReleaseCandidate::from_release)
            .collect()
    }
}

/// `https://musicbrainz.org/release/<mbid>`.
#[must_use]
pub fn release_url(mbid: &str) -> String {
    format!("{SITE_BASE}/release/{mbid}")
}

/// `https://musicbrainz.org/recording/<mbid>`.
#[must_use]
pub fn recording_url(mbid: &str) -> String {
    format!("{SITE_BASE}/recording/{mbid}")
}

/// `https://musicbrainz.org/artist/<mbid>`.
#[must_use]
pub fn artist_url(mbid: &str) -> String {
    format!("{SITE_BASE}/artist/{mbid}")
}

/// `https://musicbrainz.org/release-group/<mbid>`.
#[must_use]
pub fn release_group_url(mbid: &str) -> String {
    format!("{SITE_BASE}/release-group/{mbid}")
}

/// Opens a recording's MusicBrainz page in the default browser.
pub fn open_recording_page(mbid: &str) -> Result<(), String> {
    require_mbid("recording", mbid)?;
    crate::desktop::browser::open_in_browser(&recording_url(mbid))
}

/// Opens a release's MusicBrainz page in the default browser.
pub fn open_release_page(mbid: &str) -> Result<(), String> {
    require_mbid("release", mbid)?;
    crate::desktop::browser::open_in_browser(&release_url(mbid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tagio::{TagSet, keys};

    pub(super) fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/musicbrainz")
                .join(name),
        )
        .unwrap()
    }

    pub(super) fn fixture_release() -> Release {
        parse_release(&fixture("release_lookup.json")).unwrap()
    }

    #[test]
    fn escapes_lucene_syntax() {
        assert_eq!(escape_lucene("AC/DC"), "AC\\/DC");
        assert_eq!(
            escape_lucene("a+b-c && d || !e"),
            "a\\+b\\-c \\&\\& d \\|\\| \\!e"
        );
        assert_eq!(
            escape_lucene("(x)[y]{z}^\"~*?:\\"),
            "\\(x\\)\\[y\\]\\{z\\}\\^\\\"\\~\\*\\?\\:\\\\"
        );
        assert_eq!(escape_lucene("plain Ünï"), "plain Ünï");
    }

    #[test]
    fn builds_release_queries() {
        let q = ReleaseQuery {
            artist: "AC/DC".into(),
            album: "Back  in Black (Deluxe)".into(),
            track_count: Some(10),
            barcode: Some(" 5099 7480 ".into()),
            catalog_number: Some("EPC \"84\"".into()),
        };
        assert_eq!(
            q.lucene().unwrap(),
            "release:(Back in Black \\(Deluxe\\)) AND artist:(AC\\/DC) AND tracks:10 \
             AND barcode:50997480 AND catno:\"EPC \\\"84\\\"\""
        );
        let only_album = ReleaseQuery {
            album: "II".into(),
            ..Default::default()
        };
        assert_eq!(only_album.lucene().unwrap(), "release:(II)");
        assert!(ReleaseQuery::default().lucene().is_none());
        assert!(
            ReleaseQuery {
                track_count: Some(0),
                ..Default::default()
            }
            .lucene()
            .is_none()
        );
    }

    #[test]
    fn validates_mbids() {
        assert!(is_mbid("00398749-1816-4c98-8a39-377627740365"));
        assert!(!is_mbid("00398749-1816-4c98-8a39-37762774036"));
        assert!(!is_mbid("../../etc/passwd"));
        assert!(release("not-an-id").is_err());
        assert!(recording_lookup("x").is_err());
    }

    #[test]
    fn parses_search_results() {
        let hits = parse_search(&fixture("release_search.json")).unwrap();
        assert_eq!(hits.len(), 5);
        let first = &hits[0];
        assert_eq!(first.id, "9efefd2b-463f-41f0-bfcf-88eba093f3ca");
        assert_eq!(first.title, "II");
        assert_eq!(first.artist, "Moderat");
        assert_eq!(first.date, "2014-01-23");
        assert_eq!(first.country, "DE");
        assert_eq!(first.label, "Monkeytown Records");
        assert_eq!(first.catalog_number, "MTR040CD/DVD");
        assert_eq!(first.barcode, "817231011527");
        assert_eq!(first.formats, "2×CD + DVD-Video");
        assert_eq!(first.track_count, 36);
        assert_eq!(first.disc_count, 3);
        assert_eq!(first.status, "Official");
        assert_eq!(first.primary_type, "Album");
        assert!(first.secondary_types.is_empty());
        assert_eq!(first.score, 100);
        assert_eq!(first.disambiguation, "limited tour edition");
        assert_eq!(hits[2].track_count, 11);
    }

    #[test]
    fn rejects_unexpected_search_payload() {
        assert!(parse_search("[1,2]").unwrap_err().contains("Unexpected"));
        assert!(parse_search("{\"releases\":null}").unwrap().is_empty());
    }

    #[test]
    fn parses_full_release() {
        let r = fixture_release();
        assert_eq!(r.id, "00398749-1816-4c98-8a39-377627740365");
        assert_eq!(r.media.len(), 2);
        assert_eq!(r.media[0].tracks.len(), 5);
        assert_eq!(r.media[1].tracks.len(), 6);
        assert_eq!(r.track_count(), 11);
        let t = &r.media[0].tracks[0];
        assert_eq!(t.number.as_deref(), Some("A1"));
        assert_eq!(t.position, 1);
        assert_eq!(t.recording.isrcs, ["DEOE81310017"]);
        assert_eq!(t.length_ms(), Some(96_000));
        assert_eq!(r.countries(), ["DE"]);
        assert_eq!(r.date_text(), Some("2013-08-02"));
    }

    #[test]
    fn parses_recording_lookup() {
        let rec = parse_recording(&fixture("recording_lookup.json")).unwrap();
        assert_eq!(rec.id, "c35eb744-c525-44d0-ad21-b6af3e6caad2");
        assert_eq!(rec.title, "The Mark (interlude)");
        assert_eq!(rec.isrcs, ["DEOE81310017"]);
        assert_eq!(rec.releases.len(), 4);
        let cands = rec.release_candidates();
        assert_eq!(cands.len(), 4);
        assert!(cands.iter().all(|c| !c.id.is_empty() && c.title == "II"));
    }

    #[test]
    fn summarises_formats() {
        let m = |f: Option<&str>| Medium {
            format: f.map(str::to_owned),
            ..Default::default()
        };
        assert_eq!(formats_summary(&[m(Some("CD"))]), "CD");
        assert_eq!(
            formats_summary(&[m(Some("CD")), m(Some("DVD-Video")), m(Some("CD"))]),
            "2×CD + DVD-Video"
        );
        assert_eq!(formats_summary(&[m(None)]), "[unknown]");
        assert_eq!(formats_summary(&[]), "");
    }

    #[test]
    fn recording_tags_cover_the_basics() {
        let rec = parse_recording(&fixture("recording_lookup.json")).unwrap();
        let t: TagSet = rec.tags();
        assert_eq!(t.get(keys::TITLE), Some("The Mark (interlude)"));
        assert_eq!(t.get(keys::ARTIST), Some("Moderat"));
        assert_eq!(t.get(keys::ARTISTSORT), Some("Moderat"));
        assert_eq!(
            t.get(keys::MUSICBRAINZ_RECORDINGID),
            Some("c35eb744-c525-44d0-ad21-b6af3e6caad2")
        );
        assert_eq!(t.get(keys::ISRC), Some("DEOE81310017"));
    }

    #[test]
    fn url_builders() {
        assert_eq!(release_url("x"), "https://musicbrainz.org/release/x");
        assert_eq!(recording_url("x"), "https://musicbrainz.org/recording/x");
        assert!(open_recording_page("nope").is_err());
    }
}
