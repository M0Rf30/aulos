// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! HTTP clients of the backend: the blocking ones for the worker threads
//! (scrobbling, artist info, stream playback, episode downloads) and the
//! async ones (Subsonic, radio and podcast directories and feeds, icons).
//!
//! Natively this is `reqwest::blocking`. A browser has no blocking requests
//! (a synchronous `fetch` would freeze the page, and there is no thread to
//! park), so on wasm the same surface exists but every request fails with
//! [`Error`] until the callers are ported to async `fetch`. The response
//! types cannot be constructed there, which the type system checks:
//! nothing that reads a body can ever run.

#[cfg(not(target_arch = "wasm32"))]
pub use reqwest::Error;
#[cfg(not(target_arch = "wasm32"))]
pub use reqwest::blocking;

#[cfg(target_arch = "wasm32")]
pub use web::{Error, blocking};

/// The Mozilla root set, bundled. On Android reqwest's default verifier
/// (`rustls-platform-verifier`) calls into a Java class the NativeActivity
/// APK does not ship and panics ("Expect rustls-platform-verifier to be
/// initialized") on the first TLS client it builds, so there every client
/// trusts exactly these roots. A server with a private CA needs the
/// "accept invalid certificates" source setting.
#[cfg(target_os = "android")]
fn android_roots() -> Vec<reqwest::Certificate> {
    static ROOTS: std::sync::LazyLock<Vec<reqwest::Certificate>> = std::sync::LazyLock::new(|| {
        webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter_map(|der| reqwest::Certificate::from_der(der).ok())
            .collect()
    });
    ROOTS.clone()
}

/// The async client builder every request in the app starts from, so the
/// per-platform TLS setup lives in one place (see `android_roots`).
pub fn async_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    #[cfg(target_os = "android")]
    let builder = builder.tls_certs_only(android_roots());
    builder
}

/// The blocking counterpart of [`async_builder`].
pub fn blocking_builder() -> blocking::ClientBuilder {
    let builder = blocking::Client::builder();
    #[cfg(target_os = "android")]
    let builder = builder.tls_certs_only(android_roots());
    builder
}

/// A ready async client with default settings.
pub fn async_client() -> reqwest::Client {
    async_builder().build().unwrap_or_default()
}

/// A ready blocking client with default settings.
pub fn blocking_client() -> blocking::Client {
    blocking_builder().build().unwrap_or_default()
}

/// An async client whose requests give up after `timeout`. The browser
/// manages `fetch` timeouts itself, so there it is not applied.
///
/// # Errors
/// When the client cannot be built.
pub fn client(timeout: std::time::Duration) -> reqwest::Result<reqwest::Client> {
    let builder = async_builder();
    #[cfg(not(target_arch = "wasm32"))]
    let builder = builder.timeout(timeout);
    #[cfg(target_arch = "wasm32")]
    let _ = timeout;
    builder.build()
}

/// The host of `url` for error messages (`the server` when it has none).
pub fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "the server".to_string())
}

/// Whether a page served over `page_protocol` (`"https:"`, as
/// `window.location.protocol` spells it) cannot fetch `url`: browsers block
/// an `http://` request made from an `https://` page ("mixed content"),
/// except to loopback addresses, which they treat as secure.
pub fn is_mixed_content(page_protocol: &str, url: &str) -> bool {
    if page_protocol != "https:" {
        return false;
    }
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            let domain = domain.to_ascii_lowercase();
            domain != "localhost" && !domain.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => !ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => !ip.is_loopback(),
        None => false,
    }
}

/// Why a browser refuses to fetch an `http://` server from an `https://`
/// page.
pub fn mixed_content_message(host: &str) -> String {
    format!(
        "{host} is served over plain http:// but this page is https://, so the browser blocks \
         the request (mixed content). Use an https:// address for the server."
    )
}

/// What a request that never got an answer means in a browser: it cannot
/// tell a server that is down from one that does not allow this page to read
/// it, so the message names both.
pub fn unreachable_message(host: &str) -> String {
    format!(
        "Could not reach {host}: the server is down, unreachable, or does not allow requests \
         from this site (CORS: it must send Access-Control-Allow-Origin and \
         Access-Control-Allow-Headers and answer OPTIONS preflights, see \
         https://developer.mozilla.org/docs/Web/HTTP/Guides/CORS)"
    )
}

/// A server that answered with an HTTP error status.
pub fn status_message(host: &str, status: u16, reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        format!("{host} answered HTTP {status}")
    } else {
        format!("{host} answered HTTP {status} {reason}")
    }
}

/// The explanation of a request to `url` that got no answer at all: mixed
/// content when this page cannot fetch it (see [`check_mixed_content`]),
/// otherwise [`unreachable_message`].
pub fn no_answer_message(url: &str) -> String {
    check_mixed_content(url)
        .err()
        .unwrap_or_else(|| unreachable_message(&host_of(url)))
}

/// Fails with [`mixed_content_message`] when this page cannot fetch `url`
/// (see [`is_mixed_content`]); always `Ok` natively.
///
/// # Errors
/// The message to show to the user.
pub fn check_mixed_content(url: &str) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let protocol = web_sys::window()
            .and_then(|w| w.location().protocol().ok())
            .unwrap_or_default();
        if is_mixed_content(&protocol, url) {
            return Err(mixed_content_message(&host_of(url)));
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = url;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_from_an_https_page_is_mixed_content() {
        assert!(is_mixed_content("https:", "http://music.example.com/"));
        assert!(is_mixed_content("https:", "http://192.168.1.10:4533"));
        assert!(is_mixed_content("https:", "HTTP://Music.Example.com"));
    }

    #[test]
    fn https_servers_and_http_pages_are_not() {
        assert!(!is_mixed_content("https:", "https://music.example.com"));
        assert!(!is_mixed_content("http:", "http://music.example.com"));
        assert!(!is_mixed_content("file:", "http://music.example.com"));
    }

    #[test]
    fn loopback_is_secure_for_browsers() {
        assert!(!is_mixed_content("https:", "http://localhost:4533"));
        assert!(!is_mixed_content("https:", "http://127.0.0.1:4533"));
        assert!(!is_mixed_content("https:", "http://[::1]:4533"));
        assert!(!is_mixed_content("https:", "http://app.localhost"));
    }

    #[test]
    fn unparseable_urls_are_left_to_the_client() {
        assert!(!is_mixed_content("https:", "not a url"));
    }

    #[test]
    fn messages_name_the_host_and_the_remedy() {
        assert_eq!(
            host_of("https://music.example.com:4533/rest"),
            "music.example.com"
        );
        assert_eq!(host_of("garbage"), "the server");
        assert!(mixed_content_message("nas.lan").contains("https://"));
        let unreachable = unreachable_message("nas.lan");
        assert!(unreachable.starts_with("Could not reach nas.lan:"));
        assert!(unreachable.contains("CORS"));
        assert!(unreachable.contains("Access-Control-Allow-Origin"));
        assert!(unreachable.contains("OPTIONS"));
        assert_eq!(
            status_message("nas.lan", 404, "Not Found"),
            "nas.lan answered HTTP 404 Not Found"
        );
        assert_eq!(
            status_message("nas.lan", 502, " "),
            "nas.lan answered HTTP 502"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_builds_never_see_mixed_content() {
        assert_eq!(check_mixed_content("http://music.example.com"), Ok(()));
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use std::convert::Infallible;
    use std::time::Duration;

    /// A blocking request could not be made.
    #[derive(Debug)]
    pub struct Error;

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("blocking network requests are not available in the browser")
        }
    }

    impl std::error::Error for Error {}

    impl Error {
        pub fn is_timeout(&self) -> bool {
            false
        }

        pub fn status(&self) -> Option<reqwest::StatusCode> {
            None
        }
    }

    pub mod blocking {
        use super::{Duration, Error, Infallible};

        #[derive(Clone, Debug)]
        pub struct Client;

        #[derive(Debug)]
        pub struct ClientBuilder;

        pub struct RequestBuilder;

        /// Never constructed: [`RequestBuilder::send`] always fails.
        pub struct Response(Infallible);

        impl Client {
            pub fn new() -> Self {
                Self
            }

            pub fn builder() -> ClientBuilder {
                ClientBuilder
            }

            pub fn get(&self, _url: impl AsRef<str>) -> RequestBuilder {
                RequestBuilder
            }

            pub fn post(&self, _url: impl AsRef<str>) -> RequestBuilder {
                RequestBuilder
            }

            pub fn head(&self, _url: impl AsRef<str>) -> RequestBuilder {
                RequestBuilder
            }
        }

        impl Default for Client {
            fn default() -> Self {
                Self
            }
        }

        impl ClientBuilder {
            pub fn user_agent(self, _agent: impl AsRef<str>) -> Self {
                self
            }

            pub fn timeout(self, _timeout: Duration) -> Self {
                self
            }

            pub fn connect_timeout(self, _timeout: Duration) -> Self {
                self
            }

            pub fn build(self) -> Result<Client, Error> {
                Ok(Client)
            }
        }

        impl RequestBuilder {
            pub fn header(self, _name: impl AsRef<str>, _value: impl AsRef<str>) -> Self {
                self
            }

            pub fn timeout(self, _timeout: Duration) -> Self {
                self
            }

            pub fn body(self, _body: impl Into<Vec<u8>>) -> Self {
                self
            }

            pub fn json<T: serde::Serialize + ?Sized>(self, _body: &T) -> Self {
                self
            }

            pub fn send(self) -> Result<Response, Error> {
                Err(Error)
            }
        }

        impl Response {
            pub fn status(&self) -> reqwest::StatusCode {
                match self.0 {}
            }

            pub fn headers(&self) -> &reqwest::header::HeaderMap {
                match self.0 {}
            }

            pub fn content_length(&self) -> Option<u64> {
                match self.0 {}
            }

            pub fn error_for_status(self) -> Result<Self, Error> {
                match self.0 {}
            }

            pub fn text(self) -> Result<String, Error> {
                match self.0 {}
            }

            pub fn bytes(self) -> Result<bytes::Bytes, Error> {
                match self.0 {}
            }
        }

        impl std::io::Read for Response {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0 {}
            }
        }
    }
}
