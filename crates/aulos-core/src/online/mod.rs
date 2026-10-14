// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Online audio hub: podcast subscriptions (via `feed-rs`), internet
//! radio directory search/playback (Shoutcast/Icecast), and the tag
//! editor's MusicBrainz / AcoustID clients.

pub mod acoustid;
pub mod musicbrainz;
pub mod podcast;
pub mod radio;
pub mod scrobble;
pub mod similar;
pub mod store;

use std::sync::LazyLock;
use std::time::Duration;

/// Shared blocking HTTP client for all radio/podcast requests (search,
/// feed fetch, stream resolution, episode downloads). Built once so
/// requests reuse the connection pool and TLS session cache instead of
/// paying a fresh handshake on every call, with an explicit timeout so a
/// stalled server can't hang the blocking task forever.
pub static HTTP_CLIENT: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
});

/// Fetches a podcast artwork / station favicon URL and decodes it into RGBA
/// pixels at most [`ONLINE_ICON_MAX_DIM`](crate::library::CoverArt::ONLINE_ICON_MAX_DIM)
/// on a side. Fetch and decode run on one blocking task: both are IO/CPU
/// work that must stay off the async reactor, and the raw bytes are only
/// ever needed for decoding.
pub async fn fetch_icon(url: String) -> Option<(u32, u32, Vec<u8>)> {
    tokio::task::spawn_blocking(move || {
        let bytes = HTTP_CLIENT.get(&url).send().ok()?.bytes().ok()?;
        crate::library::CoverArt::decode_thumbnail(
            &bytes,
            crate::library::CoverArt::ONLINE_ICON_MAX_DIM,
        )
    })
    .await
    .ok()
    .flatten()
}

/// Why a typed-in feed or stream URL was rejected before any network use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    /// Nothing was entered.
    Empty,
    /// Not an `http://` or `https://` URL.
    NotHttp,
}

/// Checks that `url` (already trimmed) is a non-empty `http(s)` URL.
pub fn validate_http_url(url: &str) -> Result<(), UrlError> {
    if url.is_empty() {
        return Err(UrlError::Empty);
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(UrlError::NotHttp);
    }
    Ok(())
}

/// Reads a blocking HTTP response body into memory, capped at `max_bytes`.
/// Every response in this module comes from an untrusted remote server
/// (radio directories, podcast feeds, ICY streams); without a cap a
/// hostile or merely broken server can hand back an unbounded body and
/// exhaust memory. Returns an error instead of the body once the cap is
/// exceeded.
pub(crate) fn read_capped_body(
    response: reqwest::blocking::Response,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut buf = Vec::new();
    response
        .take(max_bytes + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("Failed to read response body: {e}"))?;
    if buf.len() as u64 > max_bytes {
        return Err(format!("Response exceeded {max_bytes} byte limit"));
    }
    Ok(buf)
}
