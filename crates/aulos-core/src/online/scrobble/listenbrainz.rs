// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! ListenBrainz backend (`/1/submit-listens`, user-token auth).

use super::{
    CLIENT_NAME, ScrobbleError, ScrobbleTrack, Scrobbler, Service, http_client, net_error,
};
use serde_json::{Value, json};

pub const DEFAULT_BASE_URL: &str = "https://api.listenbrainz.org";

/// ListenBrainz accepts up to 1000 listens per request; stay well below.
pub const MAX_BATCH: usize = 100;

pub struct ListenBrainz {
    base_url: String,
    token: String,
}

impl ListenBrainz {
    pub fn new(token: String) -> Self {
        Self::with_base_url(DEFAULT_BASE_URL.to_string(), token)
    }

    pub fn with_base_url(base_url: String, token: String) -> Self {
        Self { base_url, token }
    }

    fn submit_payload(&self, body: &Value) -> Result<(), ScrobbleError> {
        let resp = http_client()
            .post(format!("{}/1/submit-listens", self.base_url))
            .header("Authorization", format!("Token {}", self.token))
            .json(body)
            .send()
            .map_err(net_error)?;
        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        classify_response(status, &text)
    }
}

/// `track_metadata` object for one listen.
pub fn track_metadata(t: &ScrobbleTrack) -> Value {
    let mut info = json!({
        "media_player": CLIENT_NAME,
        "submission_client": CLIENT_NAME,
        "submission_client_version": env!("CARGO_PKG_VERSION"),
    });
    if t.duration_secs > 0 {
        info["duration_ms"] = json!(u64::from(t.duration_secs) * 1000);
    }
    if t.track_number > 0 {
        info["tracknumber"] = json!(t.track_number);
    }
    let mut meta = json!({
        "artist_name": t.artist,
        "track_name": t.title,
        "additional_info": info,
    });
    if !t.album.is_empty() {
        meta["release_name"] = json!(t.album);
    }
    meta
}

/// Request body for a `playing_now` submission (no `listened_at`).
pub fn playing_now_payload(t: &ScrobbleTrack) -> Value {
    json!({
        "listen_type": "playing_now",
        "payload": [{ "track_metadata": track_metadata(t) }],
    })
}

/// Request body for finished listens: `single` for one, `import` for a batch.
pub fn listens_payload(tracks: &[ScrobbleTrack]) -> Value {
    let listen_type = if tracks.len() == 1 {
        "single"
    } else {
        "import"
    };
    let payload: Vec<Value> = tracks
        .iter()
        .map(|t| {
            json!({
                "listened_at": t.timestamp,
                "track_metadata": track_metadata(t),
            })
        })
        .collect();
    json!({ "listen_type": listen_type, "payload": payload })
}

/// Map a ListenBrainz HTTP response to a retry policy.
pub fn classify_response(status: u16, body: &str) -> Result<(), ScrobbleError> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| format!("HTTP {status}"));
    match status {
        401 | 403 => Err(ScrobbleError::Auth(detail)),
        429 | 500..=599 => Err(ScrobbleError::Transient(detail)),
        400..=499 => Err(ScrobbleError::Rejected(detail)),
        _ => Err(ScrobbleError::Transient(detail)),
    }
}

/// Validate a user token; returns the ListenBrainz user name.
pub fn validate_token(base_url: &str, token: &str) -> Result<String, ScrobbleError> {
    let resp = http_client()
        .get(format!("{base_url}/1/validate-token"))
        .header("Authorization", format!("Token {token}"))
        .send()
        .map_err(net_error)?;
    let status = resp.status().as_u16();
    let body = resp.text().map_err(net_error)?;
    parse_validate_response(status, &body)
}

pub fn parse_validate_response(status: u16, body: &str) -> Result<String, ScrobbleError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|_| ScrobbleError::Transient(format!("unexpected response (HTTP {status})")))?;
    if v.get("valid").and_then(Value::as_bool) == Some(true) {
        return v
            .get("user_name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ScrobbleError::Rejected("no user name in response".into()));
    }
    if status == 429 || status >= 500 {
        return Err(ScrobbleError::Transient(format!("HTTP {status}")));
    }
    Err(ScrobbleError::Auth("invalid token".into()))
}

impl Scrobbler for ListenBrainz {
    fn service(&self) -> Service {
        Service::ListenBrainz
    }

    fn now_playing(&self, track: &ScrobbleTrack) -> Result<(), ScrobbleError> {
        self.submit_payload(&playing_now_payload(track))
    }

    fn submit(&self, tracks: &[ScrobbleTrack]) -> Result<(), ScrobbleError> {
        if tracks.is_empty() {
            return Ok(());
        }
        self.submit_payload(&listens_payload(tracks))
    }

    fn max_batch(&self) -> usize {
        MAX_BATCH
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(title: &str, ts: i64) -> ScrobbleTrack {
        ScrobbleTrack {
            artist: "Artist".into(),
            title: title.into(),
            album: "Album".into(),
            album_artist: String::new(),
            duration_secs: 200,
            track_number: 2,
            timestamp: ts,
        }
    }

    #[test]
    fn single_listen_payload() {
        let p = listens_payload(&[t("Song", 1_700_000_000)]);
        assert_eq!(p["listen_type"], "single");
        let l = &p["payload"][0];
        assert_eq!(l["listened_at"], 1_700_000_000);
        assert_eq!(l["track_metadata"]["artist_name"], "Artist");
        assert_eq!(l["track_metadata"]["track_name"], "Song");
        assert_eq!(l["track_metadata"]["release_name"], "Album");
        assert_eq!(
            l["track_metadata"]["additional_info"]["duration_ms"],
            200_000
        );
        assert_eq!(
            l["track_metadata"]["additional_info"]["media_player"],
            "Aulos"
        );
    }

    #[test]
    fn batch_uses_import() {
        let p = listens_payload(&[t("A", 1), t("B", 2)]);
        assert_eq!(p["listen_type"], "import");
        assert_eq!(p["payload"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn playing_now_has_no_timestamp() {
        let p = playing_now_payload(&t("Song", 5));
        assert_eq!(p["listen_type"], "playing_now");
        assert!(p["payload"][0].get("listened_at").is_none());
    }

    #[test]
    fn empty_album_is_omitted() {
        let mut tr = t("Song", 1);
        tr.album.clear();
        assert!(track_metadata(&tr).get("release_name").is_none());
    }

    #[test]
    fn response_classification() {
        assert!(classify_response(200, "{}").is_ok());
        assert!(matches!(
            classify_response(401, r#"{"error":"Invalid authorization token."}"#),
            Err(ScrobbleError::Auth(_))
        ));
        assert!(matches!(
            classify_response(429, ""),
            Err(ScrobbleError::Transient(_))
        ));
        assert!(matches!(
            classify_response(503, ""),
            Err(ScrobbleError::Transient(_))
        ));
        assert!(matches!(
            classify_response(400, r#"{"error":"bad payload"}"#),
            Err(ScrobbleError::Rejected(m)) if m == "bad payload"
        ));
    }

    #[test]
    fn token_validation_parsing() {
        assert_eq!(
            parse_validate_response(
                200,
                r#"{"code":200,"message":"Token valid.","valid":true,"user_name":"alice"}"#
            ),
            Ok("alice".to_string())
        );
        assert!(matches!(
            parse_validate_response(200, r#"{"code":200,"valid":false}"#),
            Err(ScrobbleError::Auth(_))
        ));
    }
}
