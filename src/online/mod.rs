// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Online audio hub: podcast subscriptions (via `feed-rs`) and internet
//! radio directory search/playback (Shoutcast/Icecast).

pub mod podcast;
pub mod radio;
pub mod scrobble;
pub mod store;

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
