// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! HTTP Range-based seekable reader.
//!
//! Wraps an HTTP URL so that it implements `Read + Seek`. Playback starts
//! from the beginning of the response body (streaming), and seeking is
//! achieved by dropping the current connection and re-requesting with an
//! appropriate `Range: bytes=N-` header.
//!
//! This allows symphonia to decode audio from a remote server with
//! full seek support, as long as the server advertises `Accept-Ranges: bytes`
//! (which Navidrome does for raw / cached-transcoded files).

use std::io::{self, Read, Seek, SeekFrom};

/// A seekable reader backed by HTTP Range requests.
///
/// - `Read::read()` reads from the current HTTP response body.
/// - `Seek::seek()` drops the current connection and opens a new one at
///   the target byte offset via `Range: bytes=N-`.
pub struct HttpRangeReader {
    url: String,
    client: reqwest::blocking::Client,
    response: Option<reqwest::blocking::Response>,
    /// Current byte position in the stream.
    position: u64,
    /// Total content length (from initial HEAD or first GET response).
    content_length: u64,
}

impl HttpRangeReader {
    /// Create a new reader for the given URL.
    ///
    /// Makes an initial GET request to start streaming and reads
    /// `Content-Length` from the response. Returns an error if the
    /// request fails or the server doesn't provide Content-Length.
    pub fn new(url: String, client: Option<reqwest::blocking::Client>) -> Result<Self, String> {
        let client = client.unwrap_or_default();

        // Start streaming from byte 0.
        let response = client
            .get(&url)
            .send()
            .map_err(|e| format!("HTTP request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("HTTP request returned an error status: {e}"))?;

        let content_length = response.content_length().unwrap_or(0);

        if content_length == 0 {
            tracing::warn!("HTTP stream has unknown or zero Content-Length, seeking may not work");
        }

        tracing::info!(
            "HttpRangeReader: opened {} ({} bytes)",
            url.split('?').next().unwrap_or(&url),
            content_length,
        );

        Ok(Self {
            url,
            client,
            response: Some(response),
            position: 0,
            content_length,
        })
    }

    /// Total content length in bytes (0 if unknown).
    pub fn content_length(&self) -> u64 {
        self.content_length
    }

    /// Open a new HTTP connection starting at `byte_offset`.
    #[tracing::instrument(skip(self), level = "debug")]
    fn open_at(&mut self, byte_offset: u64) -> io::Result<()> {
        // Drop the old response (closes the connection).
        self.response = None;

        let response = self
            .client
            .get(&self.url)
            .header("Range", format!("bytes={byte_offset}-"))
            .send()
            .map_err(|e| io::Error::other(format!("HTTP Range request failed: {e}")))?;

        if byte_offset > 0 {
            if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                return Err(io::Error::other(format!(
                    "HTTP Range request returned {} instead of 206 Partial Content",
                    response.status()
                )));
            }

            let range_start = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("bytes "))
                .and_then(|value| value.split_once('-'))
                .and_then(|(start, _)| start.parse::<u64>().ok());

            if range_start != Some(byte_offset) {
                return Err(io::Error::other(format!(
                    "HTTP Range response Content-Range does not start at byte {byte_offset}"
                )));
            }
        } else if !response.status().is_success() {
            return Err(io::Error::other(format!(
                "HTTP Range request returned {}",
                response.status()
            )));
        }

        self.response = Some(response);
        self.position = byte_offset;
        Ok(())
    }
}

impl Read for HttpRangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let response = self
            .response
            .as_mut()
            .ok_or_else(|| io::Error::other("no active HTTP response"))?;

        let n = response.read(buf)?;
        self.position += n as u64;
        Ok(n)
    }
}

impl Seek for HttpRangeReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(n) => {
                if n >= 0 {
                    self.position.saturating_add(n as u64)
                } else {
                    self.position.saturating_sub((-n) as u64)
                }
            }
            SeekFrom::End(n) => {
                if self.content_length == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "SeekFrom::End not supported without Content-Length",
                    ));
                }
                if n >= 0 {
                    self.content_length.saturating_add(n as u64)
                } else {
                    self.content_length.saturating_sub((-n) as u64)
                }
            }
        };

        // Clamp to content bounds.
        let target = if self.content_length > 0 {
            target.min(self.content_length)
        } else {
            target
        };

        // If we're already at the target, no need to reconnect.
        if target == self.position {
            return Ok(self.position);
        }

        // Small forward seeks (< 256KB) can be done by discarding bytes
        // instead of making a new HTTP request — much faster for symphonia's
        // typical small seeks during format probing.
        let forward_delta = target.saturating_sub(self.position);
        if target > self.position
            && forward_delta < 256 * 1024
            && let Some(ref mut response) = self.response
        {
            let (skipped, error) = {
                let mut skipped = 0;
                let mut remaining = forward_delta;
                let mut skip_buf = [0u8; 8192];
                let error = loop {
                    let to_read = remaining.min(skip_buf.len() as u64) as usize;
                    match response.read(&mut skip_buf[..to_read]) {
                        Ok(0) => {
                            break Some(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "HTTP response ended before forward seek completed",
                            ));
                        }
                        Ok(n) => {
                            skipped += n as u64;
                            remaining -= n as u64;
                            if remaining == 0 {
                                break None;
                            }
                        }
                        Err(error) => break Some(error),
                    }
                };
                (skipped, error)
            };
            self.position += skipped;
            if let Some(error) = error {
                return Err(error);
            }
            return Ok(self.position);
        }

        self.open_at(target)?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn server(responses: Vec<String>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 1024];
                let _ = stream.read(&mut request);
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        format!("http://{address}/track")
    }

    fn response(status: &str, headers: &[(&str, &str)], body: &str) -> String {
        let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        response.push_str(body);
        response
    }

    const LARGE_OFFSET: u64 = 256 * 1024;
    const LARGE_LENGTH: u64 = LARGE_OFFSET + 1;

    #[test]
    fn rejects_initial_error_status() {
        let url = server(vec![response(
            "404 Not Found",
            &[("Content-Length", "0")],
            "",
        )]);

        assert!(HttpRangeReader::new(url, None).is_err());
    }

    #[test]
    fn rejects_range_response_that_ignores_range_header() {
        let url = server(vec![
            response(
                "200 OK",
                &[("Content-Length", &LARGE_LENGTH.to_string())],
                "",
            ),
            response("200 OK", &[("Content-Length", "0")], ""),
        ]);
        let mut reader = HttpRangeReader::new(url, None).unwrap();

        assert!(reader.seek(SeekFrom::Start(LARGE_OFFSET)).is_err());
    }

    #[test]
    fn accepts_matching_partial_content_range() {
        let url = server(vec![
            response(
                "200 OK",
                &[("Content-Length", &LARGE_LENGTH.to_string())],
                "",
            ),
            response(
                "206 Partial Content",
                &[
                    ("Content-Length", "0"),
                    (
                        "Content-Range",
                        &format!("bytes {LARGE_OFFSET}-{LARGE_OFFSET}/{LARGE_LENGTH}"),
                    ),
                ],
                "",
            ),
        ]);
        let mut reader = HttpRangeReader::new(url, None).unwrap();

        assert_eq!(
            reader.seek(SeekFrom::Start(LARGE_OFFSET)).unwrap(),
            LARGE_OFFSET
        );
    }

    #[test]
    fn rejects_mismatched_partial_content_range() {
        let url = server(vec![
            response(
                "200 OK",
                &[("Content-Length", &LARGE_LENGTH.to_string())],
                "",
            ),
            response(
                "206 Partial Content",
                &[
                    ("Content-Length", "0"),
                    ("Content-Range", "bytes 7-8/262145"),
                ],
                "",
            ),
        ]);
        let mut reader = HttpRangeReader::new(url, None).unwrap();

        assert!(reader.seek(SeekFrom::Start(LARGE_OFFSET)).is_err());
    }

    #[test]
    fn truncated_forward_seek_keeps_consumed_position() {
        let url = server(vec![response(
            "200 OK",
            &[("Transfer-Encoding", "chunked")],
            "3\r\nabc\r\n0\r\n\r\n",
        )]);
        let mut reader = HttpRangeReader::new(url, None).unwrap();

        let error = reader.seek(SeekFrom::Start(5)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(reader.position, 3);
    }
}
