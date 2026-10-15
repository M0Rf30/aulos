// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Blocking HTTP for the worker threads of the backend (radio, podcasts,
//! scrobbling, artist info, stream playback).
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

/// An async client whose requests give up after `timeout`. The browser
/// manages `fetch` timeouts itself, so there it is not applied.
///
/// # Errors
/// When the client cannot be built.
pub fn client(timeout: std::time::Duration) -> reqwest::Result<reqwest::Client> {
    let builder = reqwest::Client::builder();
    #[cfg(not(target_arch = "wasm32"))]
    let builder = builder.timeout(timeout);
    #[cfg(target_arch = "wasm32")]
    let _ = timeout;
    builder.build()
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
