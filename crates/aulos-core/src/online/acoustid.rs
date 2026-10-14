// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! AcoustID web service client (<https://acoustid.org/webservice>): look a
//! [`Fingerprint`] up to find its MusicBrainz recordings and releases, and
//! submit fingerprints (linked to a recording MBID or described by
//! metadata) back.
//!
//! Needs an application API key (`client_key`) — AcoustID issues one per
//! application — and, for submissions, the user's own key. Blocking; call
//! from a worker thread.

use serde::{Deserialize, Deserializer};

use super::musicbrainz::{http_client, is_mbid};
use crate::library::fingerprint::Fingerprint;
use crate::library::tagio::{TagSet, keys, model::split_position};

const API_BASE: &str = "https://api.acoustid.org/v2";
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
/// What a lookup asks for: recordings with their release groups, releases
/// and per-medium track positions. `+` is the documented separator (it
/// decodes to a space on the server).
const LOOKUP_META: &str = "recordings+releasegroups+releases+tracks";

/// `https://acoustid.org/track/<id>`.
#[must_use]
pub fn track_url(acoustid: &str) -> String {
    format!("https://acoustid.org/track/{acoustid}")
}

/// Opens an AcoustID's page in the default browser.
pub fn open_track_page(acoustid: &str) -> Result<(), String> {
    super::musicbrainz::open_in_browser(&track_url(acoustid))
}

fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// AcoustID release dates are `{"year":2013,"month":8,"day":2}` with any
/// part optional; flattened to `2013-08-02`, `2013-08` or `2013`.
fn date_text<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Parts {
            year: Option<u32>,
            month: Option<u32>,
            day: Option<u32>,
        },
        Text(String),
    }
    Ok(match Option::<Wire>::deserialize(d)? {
        Some(Wire::Parts {
            year: Some(y),
            month,
            day,
        }) => Some(match (month, day) {
            (Some(m), Some(d)) => format!("{y:04}-{m:02}-{d:02}"),
            (Some(m), None) => format!("{y:04}-{m:02}"),
            _ => format!("{y:04}"),
        }),
        Some(Wire::Text(s)) if !s.is_empty() => Some(s),
        _ => None,
    })
}

/// An id that the API sends as a JSON number or string.
fn flexible_id<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Num(u64),
        Text(String),
    }
    match Wire::deserialize(d)? {
        Wire::Num(n) => Ok(n),
        Wire::Text(s) => s
            .parse()
            .map_err(|_| serde::de::Error::custom(format!("invalid submission id '{s}'"))),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AcoustIdArtist {
    pub id: String,
    pub name: String,
    pub joinphrase: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AcoustIdTrack {
    /// Release-track MBID.
    pub id: String,
    /// 1-based position on the medium.
    pub position: u32,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AcoustIdMedium {
    pub position: u32,
    pub format: Option<String>,
    pub track_count: u32,
    /// The track(s) of this medium that carry the recording.
    #[serde(deserialize_with = "null_default")]
    pub tracks: Vec<AcoustIdTrack>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AcoustIdRelease {
    pub id: String,
    pub title: String,
    pub country: Option<String>,
    #[serde(deserialize_with = "date_text")]
    pub date: Option<String>,
    pub medium_count: u32,
    pub track_count: u32,
    #[serde(deserialize_with = "null_default")]
    pub mediums: Vec<AcoustIdMedium>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct AcoustIdReleaseGroup {
    pub id: String,
    pub title: String,
    /// Primary type (`Album`, `Single`...).
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(rename = "secondarytypes", deserialize_with = "null_default")]
    pub secondary_types: Vec<String>,
    #[serde(deserialize_with = "null_default")]
    pub releases: Vec<AcoustIdRelease>,
}

/// A MusicBrainz recording an AcoustID matched.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AcoustIdRecording {
    /// Recording MBID.
    pub id: String,
    pub title: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    #[serde(deserialize_with = "null_default")]
    pub artists: Vec<AcoustIdArtist>,
    #[serde(rename = "releasegroups", deserialize_with = "null_default")]
    pub release_groups: Vec<AcoustIdReleaseGroup>,
    /// Releases listed directly on the recording (without release groups).
    #[serde(deserialize_with = "null_default")]
    pub releases: Vec<AcoustIdRelease>,
}

impl AcoustIdRecording {
    /// The credited artist text (`"A feat. B"`).
    #[must_use]
    pub fn artist_text(&self) -> String {
        self.artists
            .iter()
            .map(|a| format!("{}{}", a.name, a.joinphrase))
            .collect()
    }

    /// Every release of the recording with the release group it belongs
    /// to (when AcoustID reported one).
    #[must_use]
    pub fn all_releases(&self) -> Vec<(Option<&AcoustIdReleaseGroup>, &AcoustIdRelease)> {
        self.release_groups
            .iter()
            .flat_map(|g| g.releases.iter().map(move |r| (Some(g), r)))
            .chain(self.releases.iter().map(|r| (None, r)))
            .collect()
    }
}

/// One fingerprint match: an AcoustID and the recordings it maps to.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct AcoustIdResult {
    /// The AcoustID (a UUID; see [`track_url`]).
    pub id: String,
    /// Match confidence, 0.0-1.0.
    pub score: f64,
    #[serde(deserialize_with = "null_default")]
    pub recordings: Vec<AcoustIdRecording>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: Option<i64>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    status: Option<String>,
    error: Option<ApiError>,
    #[serde(default, deserialize_with = "null_default")]
    results: Vec<AcoustIdResult>,
    #[serde(default, deserialize_with = "null_default")]
    submissions: Vec<WireSubmission>,
}

#[derive(Debug, Deserialize)]
struct WireSubmission {
    #[serde(deserialize_with = "flexible_id")]
    id: u64,
    #[serde(default)]
    status: String,
    result: Option<WireSubmissionResult>,
}

#[derive(Debug, Deserialize)]
struct WireSubmissionResult {
    id: Option<String>,
}

/// Processing state of a submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionStatus {
    /// AcoustID's submission id, for [`submission_status`].
    pub id: u64,
    /// `pending` until AcoustID has processed it, then `imported`.
    pub status: String,
    /// The AcoustID the fingerprint ended up under, once imported.
    pub acoustid: Option<String>,
}

/// A fingerprint to submit, with the recording it identifies (`mbid`) or,
/// failing that, the metadata describing it.
#[derive(Debug, Clone)]
pub struct Submission {
    pub fingerprint: Fingerprint,
    /// MusicBrainz recording MBID. Preferred; metadata is the fallback
    /// for files MusicBrainz doesn't know yet.
    pub mbid: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    /// File extension / container (`flac`, `mp3`).
    pub file_format: String,
    pub bitrate_kbps: Option<u32>,
}

impl Submission {
    /// Builds a submission from a file's tags.
    #[must_use]
    pub fn from_tags(fingerprint: Fingerprint, tags: &TagSet, mbid: Option<String>) -> Self {
        let number = |key: &str| tags.get(key).and_then(|v| split_position(v).0);
        let year = tags
            .get(keys::DATE)
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse().ok());
        Self {
            fingerprint,
            mbid,
            title: tags.get(keys::TITLE).unwrap_or_default().to_owned(),
            artist: tags.get(keys::ARTIST).unwrap_or_default().to_owned(),
            album: tags.get(keys::ALBUM).unwrap_or_default().to_owned(),
            album_artist: tags.get(keys::ALBUMARTIST).unwrap_or_default().to_owned(),
            year,
            track_number: number(keys::TRACKNUMBER),
            disc_number: number(keys::DISCNUMBER),
            file_format: String::new(),
            bitrate_kbps: None,
        }
    }
}

/// `application/x-www-form-urlencoded` body.
fn encode_form(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn lookup_form(client_key: &str, fp: &Fingerprint) -> String {
    // `meta` is appended verbatim: its `+` separators must not be escaped.
    let mut body = encode_form(&[
        ("client".to_owned(), client_key.to_owned()),
        ("duration".to_owned(), fp.duration_secs.to_string()),
        ("fingerprint".to_owned(), fp.compressed.clone()),
        ("format".to_owned(), "json".to_owned()),
    ]);
    body.push_str("&meta=");
    body.push_str(LOOKUP_META);
    body
}

fn submit_pairs(
    client_key: &str,
    user_key: &str,
    subs: &[Submission],
) -> Result<Vec<(String, String)>, String> {
    let mut pairs: Vec<(String, String)> = vec![
        ("client".into(), client_key.into()),
        ("user".into(), user_key.into()),
        ("clientversion".into(), env!("CARGO_PKG_VERSION").into()),
        ("format".into(), "json".into()),
    ];
    for (i, s) in subs.iter().enumerate() {
        let mut add = |name: &str, value: String| {
            if !value.is_empty() {
                pairs.push((format!("{name}.{i}"), value));
            }
        };
        if s.fingerprint.duration_secs == 0 || s.fingerprint.compressed.is_empty() {
            return Err(format!("Submission {} has an empty fingerprint", i + 1));
        }
        match s.mbid.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
            Some(m) if !is_mbid(m) => {
                return Err(format!(
                    "Submission {}: '{m}' is not a MusicBrainz recording id",
                    i + 1
                ));
            }
            Some(m) => add("mbid", m.to_owned()),
            None if s.title.trim().is_empty() || s.artist.trim().is_empty() => {
                return Err(format!(
                    "Submission {} needs a recording MBID or at least a title and an artist",
                    i + 1
                ));
            }
            None => {}
        }
        add("fingerprint", s.fingerprint.compressed.clone());
        add("duration", s.fingerprint.duration_secs.to_string());
        add("track", s.title.trim().to_owned());
        add("artist", s.artist.trim().to_owned());
        add("album", s.album.trim().to_owned());
        add("albumartist", s.album_artist.trim().to_owned());
        add(
            "year",
            s.year
                .filter(|&y| y > 0)
                .map(|y| y.to_string())
                .unwrap_or_default(),
        );
        add(
            "trackno",
            s.track_number
                .filter(|&n| n > 0)
                .map(|n| n.to_string())
                .unwrap_or_default(),
        );
        add(
            "discno",
            s.disc_number
                .filter(|&n| n > 0)
                .map(|n| n.to_string())
                .unwrap_or_default(),
        );
        add("fileformat", s.file_format.trim().to_owned());
        add(
            "bitrate",
            s.bitrate_kbps
                .filter(|&b| b > 0)
                .map(|b| b.to_string())
                .unwrap_or_default(),
        );
    }
    Ok(pairs)
}

/// Parses an AcoustID reply, turning API-level errors into readable
/// messages. `http_status` is only used when the body isn't AcoustID JSON.
fn parse_envelope(http_status: u16, body: &str) -> Result<Envelope, String> {
    let http_ok = (200..300).contains(&http_status);
    let http_error = || format!("AcoustID request failed (HTTP {http_status})");
    match serde_json::from_str::<Envelope>(body) {
        Ok(env) if env.status.as_deref() == Some("error") || env.error.is_some() => {
            let (code, message) = env.error.map_or((None, None), |e| (e.code, e.message));
            let message = message.unwrap_or_else(|| "unknown error".to_owned());
            Err(match code {
                Some(code) => format!("AcoustID error {code}: {message}"),
                None => format!("AcoustID error: {message}"),
            })
        }
        Ok(_) if !http_ok => Err(http_error()),
        Ok(env) => Ok(env),
        Err(_) if !http_ok => Err(http_error()),
        Err(e) => Err(format!("Unexpected AcoustID response: {e}")),
    }
}

fn send(request: reqwest::blocking::RequestBuilder) -> Result<Envelope, String> {
    let response = request
        .send()
        .map_err(|e| format!("AcoustID request failed: {e}"))?;
    let status = response.status().as_u16();
    let body = super::read_capped_body(response, MAX_RESPONSE_BYTES)?;
    parse_envelope(status, &String::from_utf8_lossy(&body))
}

fn post_form(path: &str, body: String) -> Result<Envelope, String> {
    send(
        http_client()
            .post(format!("{API_BASE}/{path}"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body),
    )
}

/// Looks `fingerprint` up. Results come best score first; an empty list
/// means AcoustID doesn't know the recording.
pub fn lookup(client_key: &str, fingerprint: &Fingerprint) -> Result<Vec<AcoustIdResult>, String> {
    if client_key.trim().is_empty() {
        return Err("No AcoustID application key configured".to_owned());
    }
    if fingerprint.duration_secs == 0 || fingerprint.compressed.is_empty() {
        return Err("Cannot look up an empty fingerprint".to_owned());
    }
    let mut results = post_form("lookup", lookup_form(client_key, fingerprint))?.results;
    results.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(results)
}

fn statuses(env: Envelope) -> Vec<SubmissionStatus> {
    env.submissions
        .into_iter()
        .map(|s| SubmissionStatus {
            id: s.id,
            status: s.status,
            acoustid: s.result.and_then(|r| r.id).filter(|id| !id.is_empty()),
        })
        .collect()
}

/// Submits fingerprints, each linked to a MusicBrainz recording or
/// described by its metadata. `user_key` is the submitting user's AcoustID
/// API key (not the application key). Returns one status per submission,
/// in order.
pub fn submit(
    client_key: &str,
    user_key: &str,
    submissions: &[Submission],
) -> Result<Vec<SubmissionStatus>, String> {
    if client_key.trim().is_empty() {
        return Err("No AcoustID application key configured".to_owned());
    }
    if user_key.trim().is_empty() {
        return Err("No AcoustID user key configured".to_owned());
    }
    if submissions.is_empty() {
        return Ok(Vec::new());
    }
    let pairs = submit_pairs(client_key, user_key, submissions)?;
    Ok(statuses(post_form("submit", encode_form(&pairs))?))
}

/// Checks whether earlier submissions have been processed yet.
pub fn submission_status(client_key: &str, ids: &[u64]) -> Result<Vec<SubmissionStatus>, String> {
    if client_key.trim().is_empty() {
        return Err("No AcoustID application key configured".to_owned());
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut url = format!(
        "{API_BASE}/submission_status?client={}&format=json",
        urlencoding::encode(client_key)
    );
    for id in ids {
        url.push_str(&format!("&id={id}"));
    }
    Ok(statuses(send(http_client().get(url))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/acoustid")
                .join(name),
        )
        .unwrap()
    }

    fn fp() -> Fingerprint {
        Fingerprint {
            duration_secs: 215,
            raw: vec![1, 2, 3],
            compressed: "AQAAEknUJFGSRNGR_UF8Hv-RT1CuS-iT".to_owned(),
        }
    }

    #[test]
    fn parses_a_lookup_with_release_groups() {
        let env = parse_envelope(200, &fixture("lookup_ok.json")).unwrap();
        let mut results = env.results;
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
        assert_eq!(results.len(), 2);
        let best = &results[0];
        assert_eq!(best.id, "9ff43b6a-4f16-427c-93c2-92307ca505e0");
        assert!((best.score - 0.97).abs() < 1e-9);
        assert_eq!(best.recordings.len(), 1);
        let rec = &best.recordings[0];
        assert_eq!(rec.id, "c35eb744-c525-44d0-ad21-b6af3e6caad2");
        assert_eq!(rec.title.as_deref(), Some("The Mark (interlude)"));
        assert_eq!(rec.duration, Some(96.0));
        assert_eq!(rec.artist_text(), "Moderat feat. Someone");
        assert_eq!(rec.artists[0].id, "7754905b-8bf7-48e2-935a-03d566fec464");

        assert_eq!(rec.release_groups.len(), 1);
        let rg = &rec.release_groups[0];
        assert_eq!(rg.title, "II");
        assert_eq!(rg.kind.as_deref(), Some("Album"));
        assert_eq!(rg.secondary_types, ["Compilation"]);
        assert_eq!(rg.releases.len(), 2);
        let rel = &rg.releases[0];
        assert_eq!(rel.id, "00398749-1816-4c98-8a39-377627740365");
        assert_eq!(rel.country.as_deref(), Some("DE"));
        assert_eq!(rel.date.as_deref(), Some("2013-08-02"));
        assert_eq!(rel.medium_count, 2);
        assert_eq!(rel.track_count, 11);
        assert_eq!(rel.mediums[0].position, 1);
        assert_eq!(rel.mediums[0].format.as_deref(), Some("12\" Vinyl"));
        assert_eq!(rel.mediums[0].track_count, 5);
        assert_eq!(rel.mediums[0].tracks[0].position, 1);
        assert_eq!(
            rel.mediums[0].tracks[0].id,
            "665929cd-216d-4ed5-8c08-ac142096bd7a"
        );
        assert_eq!(rg.releases[1].date.as_deref(), Some("2013-08"));

        // Direct releases (no release groups) are listed after the grouped ones.
        let all = rec.all_releases();
        assert_eq!(all.len(), 3);
        assert_eq!(
            all[0].0.map(|g| g.id.as_str()),
            Some("885181a4-9133-485f-bfce-fddc1168a3c1")
        );
        assert!(all[2].0.is_none());
        assert_eq!(all[2].1.date.as_deref(), Some("2014"));

        // A result AcoustID knows without any MusicBrainz link.
        assert!(results[1].recordings.is_empty());
    }

    #[test]
    fn surfaces_api_errors_readably() {
        let err = parse_envelope(400, &fixture("error_invalid_key.json")).unwrap_err();
        assert_eq!(err, "AcoustID error 4: invalid API key");
        // The same message even if the server answered 200.
        assert!(parse_envelope(200, &fixture("error_invalid_key.json")).is_err());
        assert_eq!(
            parse_envelope(502, "<html>bad gateway</html>").unwrap_err(),
            "AcoustID request failed (HTTP 502)"
        );
        assert!(
            parse_envelope(200, "<html>")
                .unwrap_err()
                .contains("Unexpected AcoustID response")
        );
    }

    #[test]
    fn lookup_form_keeps_meta_separators_and_escapes_the_rest() {
        let body = lookup_form("a b&c", &fp());
        assert_eq!(
            body,
            "client=a%20b%26c&duration=215&fingerprint=AQAAEknUJFGSRNGR_UF8Hv-RT1CuS-iT&format=json\
             &meta=recordings+releasegroups+releases+tracks"
        );
    }

    #[test]
    fn lookup_rejects_missing_key_or_fingerprint_without_a_request() {
        assert!(lookup(" ", &fp()).unwrap_err().contains("key"));
        let empty = Fingerprint {
            duration_secs: 0,
            raw: vec![],
            compressed: String::new(),
        };
        assert!(lookup("k", &empty).unwrap_err().contains("empty"));
    }

    #[test]
    fn submit_form_is_indexed_and_urlencoded() {
        let mut tags = TagSet::new();
        tags.set_one(keys::TITLE, "Song & Dance");
        tags.set_one(keys::ARTIST, "Ünï");
        tags.set_one(keys::ALBUM, "Alb");
        tags.set_one(keys::ALBUMARTIST, "Various Artists");
        tags.set_one(keys::DATE, "2013-08-02");
        tags.set_one(keys::TRACKNUMBER, "3/12");
        tags.set_one(keys::DISCNUMBER, "1");
        let mut linked = Submission::from_tags(
            fp(),
            &tags,
            Some("c35eb744-c525-44d0-ad21-b6af3e6caad2".into()),
        );
        linked.file_format = "flac".into();
        linked.bitrate_kbps = Some(900);
        let by_metadata = Submission::from_tags(fp(), &tags, None);

        let pairs = submit_pairs("ck", "uk", &[linked, by_metadata]).unwrap();
        let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("client"), Some("ck"));
        assert_eq!(get("user"), Some("uk"));
        assert_eq!(get("clientversion"), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(get("format"), Some("json"));
        assert_eq!(get("mbid.0"), Some("c35eb744-c525-44d0-ad21-b6af3e6caad2"));
        assert_eq!(
            get("fingerprint.0"),
            Some("AQAAEknUJFGSRNGR_UF8Hv-RT1CuS-iT")
        );
        assert_eq!(get("duration.0"), Some("215"));
        assert_eq!(get("track.0"), Some("Song & Dance"));
        assert_eq!(get("year.0"), Some("2013"));
        assert_eq!(get("trackno.0"), Some("3"));
        assert_eq!(get("discno.0"), Some("1"));
        assert_eq!(get("fileformat.0"), Some("flac"));
        assert_eq!(get("bitrate.0"), Some("900"));
        assert_eq!(get("mbid.1"), None);
        assert_eq!(
            get("fingerprint.1"),
            Some("AQAAEknUJFGSRNGR_UF8Hv-RT1CuS-iT")
        );
        assert_eq!(get("artist.1"), Some("Ünï"));
        assert_eq!(get("fileformat.1"), None, "empty fields are left out");

        let body = encode_form(&pairs);
        assert!(body.starts_with("client=ck&user=uk&clientversion="));
        assert!(body.contains("&track.0=Song%20%26%20Dance"));
        assert!(body.contains("&artist.1=%C3%9Cn%C3%AF"));
    }

    #[test]
    fn submit_validates_before_sending() {
        let bare = Submission::from_tags(fp(), &TagSet::new(), None);
        assert!(
            submit_pairs("c", "u", &[bare])
                .unwrap_err()
                .contains("MBID")
        );
        let bad = Submission::from_tags(fp(), &TagSet::new(), Some("nope".into()));
        assert!(
            submit_pairs("c", "u", &[bad])
                .unwrap_err()
                .contains("not a MusicBrainz recording id")
        );
        assert!(submit("", "u", &[]).is_err());
        assert!(submit("c", " ", &[]).unwrap_err().contains("user key"));
        assert_eq!(submit("c", "u", &[]).unwrap(), vec![]);
    }

    #[test]
    fn parses_submission_replies() {
        let sub = statuses(parse_envelope(200, &fixture("submit_ok.json")).unwrap());
        assert_eq!(
            sub,
            vec![
                SubmissionStatus {
                    id: 1_234_567,
                    status: "pending".into(),
                    acoustid: None
                },
                SubmissionStatus {
                    id: 1_234_568,
                    status: "pending".into(),
                    acoustid: None
                },
            ]
        );
        let status = statuses(parse_envelope(200, &fixture("submission_status_ok.json")).unwrap());
        assert_eq!(status[0].status, "imported");
        assert_eq!(
            status[0].acoustid.as_deref(),
            Some("9ff43b6a-4f16-427c-93c2-92307ca505e0")
        );
        assert_eq!(status[1].status, "pending");
        assert_eq!(status[1].acoustid, None);
        assert_eq!(status[1].id, 1_234_568, "string ids are accepted");
    }
}
