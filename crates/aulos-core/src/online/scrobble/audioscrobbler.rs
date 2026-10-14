// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Audioscrobbler 2.0 backend, shared by Last.fm and Libre.fm.
//!
//! Auth uses the desktop flow: `auth.getToken` → the user authorises the
//! token in a browser → `auth.getSession` yields a permanent session key.

use super::sign::api_sig;
use super::{ScrobbleError, ScrobbleTrack, Scrobbler, Service, http_client, net_error};
use serde_json::Value;

pub const LASTFM_ENDPOINT: &str = "https://ws.audioscrobbler.com/2.0/";
pub const LIBREFM_ENDPOINT: &str = "https://libre.fm/2.0/";

/// Libre.fm accepts any 32-character key, so a fixed placeholder is used.
pub const LIBREFM_API_KEY: &str = "a05b1c9d3e7f4a62b8d0c5e1f9a3b7d4";
/// Libre.fm signs with this value as the shared secret.
pub const LIBREFM_API_SECRET: &str = "a05b1c9d3e7f4a62b8d0c5e1f9a3b7d4";

/// Maximum listens per `track.scrobble` call.
pub const MAX_BATCH: usize = 50;

/// Static description of an Audioscrobbler-compatible service.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub service: Service,
    pub url: &'static str,
    pub auth_url: &'static str,
}

impl Endpoint {
    pub fn for_service(service: Service) -> Option<Self> {
        match service {
            Service::LastFm => Some(Self {
                service,
                url: LASTFM_ENDPOINT,
                auth_url: "https://www.last.fm/api/auth/",
            }),
            Service::LibreFm => Some(Self {
                service,
                url: LIBREFM_ENDPOINT,
                auth_url: "https://libre.fm/api/auth/",
            }),
            Service::ListenBrainz => None,
        }
    }
}

/// API credentials for a service. Libre.fm uses fixed placeholders.
pub fn api_credentials(
    service: Service,
    lastfm_key: &str,
    lastfm_secret: &str,
) -> (String, String) {
    match service {
        Service::LibreFm => (LIBREFM_API_KEY.to_string(), LIBREFM_API_SECRET.to_string()),
        _ => (lastfm_key.to_string(), lastfm_secret.to_string()),
    }
}

/// URL the user must visit to authorise `token`.
pub fn auth_page_url(endpoint: &Endpoint, api_key: &str, token: &str) -> String {
    format!(
        "{}?api_key={}&token={}",
        endpoint.auth_url,
        urlencoding::encode(api_key),
        urlencoding::encode(token)
    )
}

pub struct Audioscrobbler {
    endpoint: Endpoint,
    api_key: String,
    api_secret: String,
    session_key: String,
}

impl Audioscrobbler {
    pub fn new(
        endpoint: Endpoint,
        api_key: String,
        api_secret: String,
        session_key: String,
    ) -> Self {
        Self {
            endpoint,
            api_key,
            api_secret,
            session_key,
        }
    }

    /// Signed POST; `params` must not yet contain `api_key`/`api_sig`/`format`.
    fn call(&self, method: &str, params: Vec<(String, String)>) -> Result<Value, ScrobbleError> {
        let sk = (!self.session_key.is_empty()).then_some(self.session_key.as_str());
        post(
            &self.endpoint,
            &self.api_key,
            &self.api_secret,
            method,
            params,
            sk,
        )
    }
}

/// Perform a signed Audioscrobbler call.
fn post(
    endpoint: &Endpoint,
    api_key: &str,
    api_secret: &str,
    method: &str,
    params: Vec<(String, String)>,
    session_key: Option<&str>,
) -> Result<Value, ScrobbleError> {
    if api_key.is_empty() || api_secret.is_empty() {
        return Err(ScrobbleError::Auth(format!(
            "{} API key/secret not configured",
            endpoint.service.display_name()
        )));
    }
    let form = signed_form(method, api_key, api_secret, session_key, params);
    let resp = http_client()
        .post(endpoint.url)
        .header(
            "Content-Type",
            "application/x-www-form-urlencoded; charset=utf-8",
        )
        .body(encode_form(&form))
        .send()
        .map_err(net_error)?;
    let status = resp.status().as_u16();
    let body = resp.text().map_err(net_error)?;
    parse_response(status, &body)
}

/// `application/x-www-form-urlencoded` encoding of `params`.
pub fn encode_form(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Assemble the final form body: params + method + api_key (+ sk), signed,
/// then `format=json` appended (it is deliberately not part of the signature).
pub fn signed_form(
    method: &str,
    api_key: &str,
    api_secret: &str,
    session_key: Option<&str>,
    mut params: Vec<(String, String)>,
) -> Vec<(String, String)> {
    params.push(("method".into(), method.into()));
    params.push(("api_key".into(), api_key.into()));
    if let Some(sk) = session_key {
        params.push(("sk".into(), sk.into()));
    }
    let sig = api_sig(&params, api_secret);
    params.push(("api_sig".into(), sig));
    params.push(("format".into(), "json".into()));
    params
}

/// Map an Audioscrobbler error code to a retry policy.
pub fn classify_error_code(code: i64, message: &str) -> ScrobbleError {
    let msg = format!("{message} (code {code})");
    match code {
        // Auth failed, invalid session key, invalid API key, token problems,
        // suspended API key, login required.
        4 | 9 | 10 | 14 | 15 | 17 | 26 => ScrobbleError::Auth(msg),
        // Operation failed, service offline / temporarily unavailable, rate limit.
        8 | 11 | 16 | 29 => ScrobbleError::Transient(msg),
        _ => ScrobbleError::Rejected(msg),
    }
}

/// Interpret an HTTP response from an Audioscrobbler endpoint.
pub fn parse_response(status: u16, body: &str) -> Result<Value, ScrobbleError> {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(code) = v.get("error").and_then(Value::as_i64) {
            let message = v.get("message").and_then(Value::as_str).unwrap_or("error");
            return Err(classify_error_code(code, message));
        }
        if (200..300).contains(&status) {
            return Ok(v);
        }
    } else if (200..300).contains(&status) {
        // Some GNU FM deployments ignore `format=json` and answer in XML.
        if body.contains("status=\"ok\"") {
            return Ok(Value::Null);
        }
        if let Some(code) = xml_error_code(body) {
            return Err(classify_error_code(code, "request failed"));
        }
    }
    if status == 429 || status >= 500 {
        return Err(ScrobbleError::Transient(format!("HTTP {status}")));
    }
    if status == 401 || status == 403 {
        return Err(ScrobbleError::Auth(format!("HTTP {status}")));
    }
    Err(ScrobbleError::Transient(format!(
        "unexpected response (HTTP {status})"
    )))
}

fn xml_error_code(body: &str) -> Option<i64> {
    let idx = body.find("<error code=\"")? + "<error code=\"".len();
    let rest = &body[idx..];
    rest[..rest.find('"')?].parse().ok()
}

/// Parameters of a `track.updateNowPlaying` call.
pub fn now_playing_params(t: &ScrobbleTrack) -> Vec<(String, String)> {
    let mut p = vec![
        ("artist".to_string(), t.artist.clone()),
        ("track".to_string(), t.title.clone()),
    ];
    push_optional(&mut p, "", t);
    if t.duration_secs > 0 {
        p.push(("duration".into(), t.duration_secs.to_string()));
    }
    p
}

/// Parameters of a batched `track.scrobble` call (`name[i]` style).
pub fn scrobble_params(tracks: &[ScrobbleTrack]) -> Vec<(String, String)> {
    let mut p = Vec::new();
    for (i, t) in tracks.iter().enumerate() {
        let k = |name: &str| format!("{name}[{i}]");
        p.push((k("artist"), t.artist.clone()));
        p.push((k("track"), t.title.clone()));
        p.push((k("timestamp"), t.timestamp.to_string()));
        if !t.album.is_empty() {
            p.push((k("album"), t.album.clone()));
        }
        if !t.album_artist.is_empty() && t.album_artist != t.artist {
            p.push((k("albumArtist"), t.album_artist.clone()));
        }
        if t.track_number > 0 {
            p.push((k("trackNumber"), t.track_number.to_string()));
        }
        if t.duration_secs > 0 {
            p.push((k("duration"), t.duration_secs.to_string()));
        }
    }
    p
}

fn push_optional(p: &mut Vec<(String, String)>, suffix: &str, t: &ScrobbleTrack) {
    let k = |name: &str| format!("{name}{suffix}");
    if !t.album.is_empty() {
        p.push((k("album"), t.album.clone()));
    }
    if !t.album_artist.is_empty() && t.album_artist != t.artist {
        p.push((k("albumArtist"), t.album_artist.clone()));
    }
    if t.track_number > 0 {
        p.push((k("trackNumber"), t.track_number.to_string()));
    }
}

impl Scrobbler for Audioscrobbler {
    fn service(&self) -> Service {
        self.endpoint.service
    }

    fn now_playing(&self, track: &ScrobbleTrack) -> Result<(), ScrobbleError> {
        self.call("track.updateNowPlaying", now_playing_params(track))
            .map(|_| ())
    }

    fn submit(&self, tracks: &[ScrobbleTrack]) -> Result<(), ScrobbleError> {
        if tracks.is_empty() {
            return Ok(());
        }
        self.call("track.scrobble", scrobble_params(tracks))
            .map(|_| ())
    }

    fn max_batch(&self) -> usize {
        MAX_BATCH
    }

    fn supports_love(&self) -> bool {
        true
    }

    fn set_loved(&self, artist: &str, title: &str, loved: bool) -> Result<(), ScrobbleError> {
        let method = if loved { "track.love" } else { "track.unlove" };
        self.call(
            method,
            vec![
                ("artist".into(), artist.into()),
                ("track".into(), title.into()),
            ],
        )
        .map(|_| ())
    }
}

/// Step 1 of the auth flow: fetch a request token.
pub fn get_token(
    endpoint: &Endpoint,
    api_key: &str,
    api_secret: &str,
) -> Result<String, ScrobbleError> {
    let v = post(
        endpoint,
        api_key,
        api_secret,
        "auth.getToken",
        Vec::new(),
        None,
    )?;
    v.get("token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ScrobbleError::Rejected("no token in response".into()))
}

/// Step 3 of the auth flow: exchange the authorised token for
/// `(user name, session key)`.
pub fn get_session(
    endpoint: &Endpoint,
    api_key: &str,
    api_secret: &str,
    token: &str,
) -> Result<(String, String), ScrobbleError> {
    let v = post(
        endpoint,
        api_key,
        api_secret,
        "auth.getSession",
        vec![("token".into(), token.into())],
        None,
    )?;
    let session = v.get("session");
    let name = session.and_then(|s| s.get("name")).and_then(Value::as_str);
    let key = session.and_then(|s| s.get("key")).and_then(Value::as_str);
    match (name, key) {
        (Some(n), Some(k)) => Ok((n.to_string(), k.to_string())),
        _ => Err(ScrobbleError::Rejected("no session in response".into())),
    }
}

/// Fetch the `(artist, title)` pairs a Last.fm user has loved (unsigned,
/// public call; at most `max_pages` pages of 1000).
pub fn get_loved_tracks(
    endpoint: &Endpoint,
    api_key: &str,
    user: &str,
    max_pages: u32,
) -> Result<Vec<(String, String)>, ScrobbleError> {
    let mut out = Vec::new();
    for page in 1..=max_pages {
        let url = format!(
            "{}?{}",
            endpoint.url,
            encode_form(&[
                ("method".to_string(), "user.getLovedTracks".to_string()),
                ("user".to_string(), user.to_string()),
                ("api_key".to_string(), api_key.to_string()),
                ("format".to_string(), "json".to_string()),
                ("limit".to_string(), "1000".to_string()),
                ("page".to_string(), page.to_string()),
            ])
        );
        let resp = http_client().get(url).send().map_err(net_error)?;
        let status = resp.status().as_u16();
        let body = resp.text().map_err(net_error)?;
        let v = parse_response(status, &body)?;
        let (pairs, total_pages) = parse_loved_tracks(&v);
        out.extend(pairs);
        if page >= total_pages {
            break;
        }
    }
    Ok(out)
}

/// Extract `(artist, title)` pairs and the total page count from a
/// `user.getLovedTracks` response. Tolerates the single-track case, where
/// the JSON "array" is a bare object.
pub fn parse_loved_tracks(v: &Value) -> (Vec<(String, String)>, u32) {
    let loved = v.get("lovedtracks");
    let total_pages = loved
        .and_then(|l| l.get("@attr"))
        .and_then(|a| a.get("totalPages"))
        .and_then(|t| t.as_str().and_then(|s| s.parse().ok()).or(t.as_u64()))
        .unwrap_or(1) as u32;
    let tracks = match loved.and_then(|l| l.get("track")) {
        Some(Value::Array(a)) => a.clone(),
        Some(o @ Value::Object(_)) => vec![o.clone()],
        _ => Vec::new(),
    };
    let pairs = tracks
        .iter()
        .filter_map(|t| {
            let title = t.get("name")?.as_str()?;
            let artist = t.get("artist")?.get("name")?.as_str()?;
            Some((artist.to_string(), title.to_string()))
        })
        .collect();
    (pairs, total_pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> ScrobbleTrack {
        ScrobbleTrack {
            artist: "Björk".into(),
            title: "Hunter".into(),
            album: "Homogenic".into(),
            album_artist: "Björk".into(),
            duration_secs: 255,
            track_number: 3,
            timestamp: 1_700_000_000,
        }
    }

    fn get<'a>(form: &'a [(String, String)], k: &str) -> Option<&'a str> {
        form.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn signed_form_signs_everything_but_format() {
        let form = signed_form(
            "track.love",
            "KEY",
            "SECRET",
            Some("SK"),
            vec![("artist".into(), "A".into()), ("track".into(), "T".into())],
        );
        let expected = format!(
            "{:x}",
            md5::compute(b"api_keyKEYartistAmethodtrack.loveskSKtrackTSECRET")
        );
        assert_eq!(get(&form, "api_sig"), Some(expected.as_str()));
        assert_eq!(get(&form, "format"), Some("json"));
        assert_eq!(get(&form, "sk"), Some("SK"));
    }

    #[test]
    fn scrobble_batch_is_indexed() {
        let mut second = t();
        second.title = "Joga".into();
        second.album_artist = String::new();
        let p = scrobble_params(&[t(), second]);
        assert_eq!(get(&p, "artist[0]"), Some("Björk"));
        assert_eq!(get(&p, "track[1]"), Some("Joga"));
        assert_eq!(get(&p, "timestamp[1]"), Some("1700000000"));
        assert_eq!(get(&p, "trackNumber[0]"), Some("3"));
        // album artist equal to artist / empty is omitted
        assert_eq!(get(&p, "albumArtist[0]"), None);
        assert_eq!(get(&p, "albumArtist[1]"), None);
    }

    #[test]
    fn now_playing_has_duration_but_no_timestamp() {
        let p = now_playing_params(&t());
        assert_eq!(get(&p, "duration"), Some("255"));
        assert_eq!(get(&p, "timestamp"), None);
        assert_eq!(get(&p, "album"), Some("Homogenic"));
    }

    #[test]
    fn error_codes_map_to_policies() {
        assert!(matches!(
            classify_error_code(9, "x"),
            ScrobbleError::Auth(_)
        ));
        assert!(matches!(
            classify_error_code(26, "x"),
            ScrobbleError::Auth(_)
        ));
        assert!(matches!(
            classify_error_code(29, "x"),
            ScrobbleError::Transient(_)
        ));
        assert!(matches!(
            classify_error_code(16, "x"),
            ScrobbleError::Transient(_)
        ));
        assert!(matches!(
            classify_error_code(6, "x"),
            ScrobbleError::Rejected(_)
        ));
    }

    #[test]
    fn response_parsing() {
        assert!(parse_response(200, r#"{"scrobbles":{"@attr":{"accepted":1}}}"#).is_ok());
        assert!(matches!(
            parse_response(403, r#"{"error":9,"message":"Invalid session key"}"#),
            Err(ScrobbleError::Auth(_))
        ));
        assert!(matches!(
            parse_response(503, "<html>down</html>"),
            Err(ScrobbleError::Transient(_))
        ));
        assert!(parse_response(200, r#"<lfm status="ok"></lfm>"#).is_ok());
        assert!(matches!(
            parse_response(
                200,
                r#"<lfm status="failed"><error code="9">bad</error></lfm>"#
            ),
            Err(ScrobbleError::Auth(_)) | Err(ScrobbleError::Transient(_))
        ));
    }

    #[test]
    fn loved_tracks_parsing_handles_single_object() {
        let many: Value = serde_json::from_str(
            r#"{"lovedtracks":{"track":[{"name":"A","artist":{"name":"X"}},{"name":"B","artist":{"name":"Y"}}],"@attr":{"totalPages":"2"}}}"#,
        )
        .unwrap();
        let (pairs, pages) = parse_loved_tracks(&many);
        assert_eq!(
            pairs,
            vec![("X".into(), "A".into()), ("Y".into(), "B".into())]
        );
        assert_eq!(pages, 2);

        let one: Value = serde_json::from_str(
            r#"{"lovedtracks":{"track":{"name":"A","artist":{"name":"X"}},"@attr":{"totalPages":"1"}}}"#,
        )
        .unwrap();
        assert_eq!(parse_loved_tracks(&one).0, vec![("X".into(), "A".into())]);
    }

    #[test]
    fn form_encoding_escapes_reserved_characters() {
        let body = encode_form(&[
            ("artist[0]".into(), "Simon & Garfunkel".into()),
            ("track[0]".into(), "50% = ü".into()),
        ]);
        assert_eq!(
            body,
            "artist%5B0%5D=Simon%20%26%20Garfunkel&track%5B0%5D=50%25%20%3D%20%C3%BC"
        );
    }

    #[test]
    fn auth_url_encodes_params() {
        let ep = Endpoint::for_service(Service::LastFm).unwrap();
        assert_eq!(
            auth_page_url(&ep, "k e", "tok"),
            "https://www.last.fm/api/auth/?api_key=k%20e&token=tok"
        );
    }

    #[test]
    fn librefm_uses_fixed_credentials() {
        let (k, s) = api_credentials(Service::LibreFm, "mine", "mysecret");
        assert_eq!(k, LIBREFM_API_KEY);
        assert_eq!(s, LIBREFM_API_SECRET);
        assert_eq!(k.len(), 32);
        let (k, s) = api_credentials(Service::LastFm, "mine", "mysecret");
        assert_eq!((k.as_str(), s.as_str()), ("mine", "mysecret"));
    }
}
