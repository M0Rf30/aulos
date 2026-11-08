// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The HTTP transport `tagcore` (MusicBrainz, AcoustID, Cover Art Archive)
//! sends its requests through: the shared blocking [`HTTP_CLIENT`] (stall
//! and connect timeouts, platform TLS roots) with aulos's `User-Agent`,
//! which MusicBrainz requires and blocks generic ones without. Native only:
//! a browser has no blocking requests, so there tagcore reports that no
//! transport is configured.

use std::io::Read;
use std::sync::Arc;

use tagcore::http::{Headers, MAX_BODY, Response, Transport};

use super::HTTP_CLIENT;

const USER_AGENT: &str = concat!(
    "Aulos/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/M0Rf30/aulos )"
);

struct ReqwestTransport;

impl ReqwestTransport {
    fn finish(request: reqwest::blocking::RequestBuilder) -> tagcore::Result<Response> {
        let response = request
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .send()
            .map_err(|e| tagcore::Error::Http(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(|v| v.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let mut body = Vec::new();
        response
            .take(MAX_BODY + 1)
            .read_to_end(&mut body)
            .map_err(|e| tagcore::Error::Http(format!("Failed to read response body: {e}")))?;
        if body.len() as u64 > MAX_BODY {
            return Err(tagcore::Error::Http(format!(
                "Response exceeded {MAX_BODY} byte limit"
            )));
        }
        Ok(Response {
            status,
            content_type,
            body,
        })
    }
}

impl Transport for ReqwestTransport {
    fn get(&self, url: &str, headers: Headers<'_>) -> tagcore::Result<Response> {
        let mut request = HTTP_CLIENT.get(url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        Self::finish(request)
    }

    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> tagcore::Result<Response> {
        let body = form
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        Self::finish(
            HTTP_CLIENT
                .post(url)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(body),
        )
    }
}

/// Makes tagcore send its requests through the shared client. Call once at
/// startup, before the first tagging job.
pub fn install() {
    tagcore::http::set_transport(Arc::new(ReqwestTransport));
}
