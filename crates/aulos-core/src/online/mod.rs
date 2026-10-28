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

/// How long one wait on an episode download may stall: for the response
/// headers and for each single read of the body.
const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Shared blocking HTTP client for episode downloads, which stream to a
/// file on a worker thread (desktop and Android only). Built once so
/// requests reuse the connection pool and TLS session cache. The blocking
/// client's timeout is not a total deadline: it bounds the wait for the
/// response headers and every `Read::read` of the body separately, so a
/// long episode can finish while a peer that stops sending fails after
/// [`DOWNLOAD_STALL_TIMEOUT`] instead of blocking the worker (and its
/// cancel check) forever. Connecting is bounded too, and TCP keepalive drops
/// a peer that vanished. The body size is capped by the download loop.
pub static HTTP_CLIENT: LazyLock<crate::http::blocking::Client> = LazyLock::new(|| {
    crate::http::blocking_builder()
        .timeout(DOWNLOAD_STALL_TIMEOUT)
        .connect_timeout(Duration::from_secs(10))
        .tcp_keepalive(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| crate::http::blocking_client())
});

/// Shared async HTTP client for the radio directory, podcast search and feed
/// fetches, stream playlists and icons. These run as plain futures so the
/// browser (which has no blocking requests) can make them too.
pub fn http() -> &'static reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        crate::http::client(Duration::from_secs(30)).unwrap_or_else(|_| crate::http::async_client())
    });
    &CLIENT
}

/// Blocks the calling thread for `d`, for rate limiting before a blocking
/// request. A browser has no thread to block (`std::thread::sleep` panics
/// there), and its blocking requests fail immediately, so it does not wait.
pub(crate) fn block_for(d: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::sleep(d);
    #[cfg(target_arch = "wasm32")]
    let _ = d;
}

/// What a failed request to `url` means to the user. Natively the error is
/// reported as it came (without its URL, which may carry a token), labeled
/// with `what`; in a browser a request that got no answer is mostly a CORS
/// refusal or a down server, which the page cannot tell apart, so it says
/// both (see [`crate::http::no_answer_message`]).
pub(crate) fn request_error(what: &str, url: &str, error: reqwest::Error) -> String {
    if cfg!(target_arch = "wasm32") && error.status().is_none() && !error.is_decode() {
        crate::http::no_answer_message(url)
    } else {
        format!("{what}: {}", error.without_url())
    }
}

/// Largest icon / artwork body [`fetch_icon`] accepts.
const ICON_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Fetches a podcast artwork / station favicon URL and decodes it into RGBA
/// pixels at most [`ONLINE_ICON_MAX_DIM`](crate::library::CoverArt::ONLINE_ICON_MAX_DIM)
/// on a side. The fetch is async; the decode is CPU work and runs on the
/// blocking pool, off the async reactor.
pub async fn fetch_icon(url: String) -> Option<(u32, u32, Vec<u8>)> {
    let response = http().get(&url).send().await.ok()?;
    let bytes = read_capped_response(response, ICON_MAX_BYTES).await.ok()?;
    crate::task::spawn_blocking(move || {
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

/// [`read_capped_body`] for an async response: natively it reads chunk by
/// chunk and stops as soon as `max_bytes` is exceeded, so a server that never
/// ends its body cannot fill memory. A browser's `fetch` hands over the body
/// whole, so there the size is checked afterwards.
pub(crate) async fn read_capped_response(
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))] mut response: reqwest::Response,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    let too_big = || format!("Response exceeded {max_bytes} byte limit");
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut buf = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("Failed to read response body: {e}"))?
        {
            if (buf.len() + chunk.len()) as u64 > max_bytes {
                return Err(too_big());
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    }
    #[cfg(target_arch = "wasm32")]
    {
        if response.content_length().is_some_and(|n| n > max_bytes) {
            return Err(too_big());
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read response body: {e}"))?;
        if body.len() as u64 > max_bytes {
            return Err(too_big());
        }
        Ok(body.to_vec())
    }
}

/// Largest JSON/text API response read by [`read_capped_text`] callers.
pub(crate) const API_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// [`read_capped_body`] decoded as (lossy) UTF-8 text.
pub(crate) fn read_capped_text(
    response: crate::http::blocking::Response,
    max_bytes: u64,
) -> Result<String, String> {
    let bytes = read_capped_body(response, max_bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Reads a blocking HTTP response body into memory, capped at `max_bytes`.
/// Every response in this module comes from an untrusted remote server
/// (radio directories, podcast feeds, ICY streams); without a cap a
/// hostile or merely broken server can hand back an unbounded body and
/// exhaust memory. Returns an error instead of the body once the cap is
/// exceeded.
pub(crate) fn read_capped_body(
    response: crate::http::blocking::Response,
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
