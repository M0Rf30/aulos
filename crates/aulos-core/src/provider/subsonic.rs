// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! OpenSubsonic/Navidrome music provider.
//!
//! Connects to a Subsonic-compatible server using the `opensubsonic` crate,
//! browses the library via the ID3-based API, and streams audio via HTTP URLs
//! decoded locally by the `LocalBackend`.

use super::{MusicProvider, ProviderError, ProviderType};
use crate::library::scan_progress::ScanProgress;
use crate::library::{Album, Artist, CoverSource, LyricLine, Lyrics, Playlist, Track, TrackSource};
use opensubsonic::{Auth, Client};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

/// Concurrent `getAlbum` requests per page.
const ALBUM_FETCH_CONCURRENCY: usize = 8;

/// Request timeout of the Subsonic HTTP clients, so a hung server fails the
/// sync instead of leaving it running forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// What a failed Subsonic request means to the user, in one place for the
/// provider, the connection test and the browser's stream fetch.
///
/// The server's own words are kept for API errors (wrong credentials, ...)
/// and HTTP statuses. A request that got no answer at all is, in a browser,
/// mostly a CORS refusal or a down server, which the page cannot tell apart,
/// so it says both ([`crate::http::no_answer_message`], which also catches
/// mixed content); natively it is reported as it came, labeled with the
/// operation.
fn describe_error(url: &str, op: &str, error: &opensubsonic::Error) -> String {
    use opensubsonic::Error;
    match error {
        Error::Api(api) => format!("{api}"),
        Error::Http(e) => match e.status() {
            Some(status) => crate::http::status_message(
                &crate::http::host_of(url),
                status.as_u16(),
                status.canonical_reason().unwrap_or_default(),
            ),
            None if cfg!(target_arch = "wasm32") && !e.is_decode() && !e.is_body() => {
                crate::http::no_answer_message(url)
            }
            // reqwest's Display appends the request URL (u, t, s).
            None => {
                let text = e.to_string();
                let text = text.split(" for url (").next().unwrap_or(&text);
                format!("Subsonic {op}: {text}")
            }
        },
        Error::Parse(body) => {
            let head: String = body.chars().take(PARSE_ERROR_CHARS).collect();
            let ellipsis = if head.len() < body.len() { "…" } else { "" };
            format!("Subsonic {op}: Parse error: {head}{ellipsis}")
        }
        other => format!("Subsonic {op}: {other}"),
    }
}

/// Most `getAlbumList2` pages [`MusicProvider::browse_albums`] and
/// `browse_tracks` read (500 albums each): a server that ignores `offset`
/// and answers every page in full would otherwise never end the listing.
const MAX_LIST_PAGES: usize = 400;

/// Largest cover or artist image read from a server: a hostile or broken
/// server must not be able to fill memory with one body.
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// Characters of a response body kept in a parse error shown to the user.
const PARSE_ERROR_CHARS: usize = 80;

/// Whether the server turned the credentials down: a Subsonic API error 40
/// (wrong username or password), 41 (token auth unsupported), 42/43 (auth
/// mechanism) or 44 (invalid API key), or an HTTP 401/403 from a proxy or
/// the server in front of it.
fn is_auth_rejection(error: &opensubsonic::Error) -> bool {
    use opensubsonic::Error;
    match error {
        Error::Api(api) => (40..=44).contains(&api.code),
        Error::Http(e) => e.status().is_some_and(|s| matches!(s.as_u16(), 401 | 403)),
        _ => false,
    }
}

/// The user-facing message of `error` (see [`describe_error`]) as the
/// typed failure: [`ProviderError::Unauthorized`] for a credential
/// rejection, `other` of the message for everything else.
fn classify(
    url: &str,
    op: &str,
    error: &opensubsonic::Error,
    other: fn(String) -> ProviderError,
) -> ProviderError {
    let message = describe_error(url, op, error);
    if is_auth_rejection(error) {
        ProviderError::Unauthorized(message)
    } else {
        other(message)
    }
}

/// Checks that a server answers `ping` with the given credentials: the
/// settings card's "Test connection".
///
/// # Errors
/// [`ProviderError::Unauthorized`] when the server rejects the credentials;
/// otherwise [`ProviderError::Other`] carrying the classified message (see
/// [`describe_error`]).
pub async fn test_connection(
    url: &str,
    username: &str,
    password: &str,
    accept_invalid_certs: bool,
) -> Result<(), ProviderError> {
    crate::http::check_mixed_content(url).map_err(ProviderError::Other)?;
    let client = build_client(url, username, password, accept_invalid_certs)
        .map_err(|e| ProviderError::Other(format!("Client: {e}")))?;
    client
        .ping()
        .await
        .map_err(|e| classify(url, "ping", &e, ProviderError::Other))
}

/// The HTTP client of the API and cover requests, with a request timeout.
/// `accept_invalid_certs` only exists natively: a browser decides on
/// certificates itself and the setting cannot apply.
fn http_client(accept_invalid_certs: bool) -> reqwest::Client {
    #[cfg(not(target_arch = "wasm32"))]
    if accept_invalid_certs {
        return crate::http::async_builder()
            .timeout(REQUEST_TIMEOUT)
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap_or_else(|_| crate::http::async_client());
    }
    #[cfg(target_arch = "wasm32")]
    if accept_invalid_certs {
        tracing::warn!("accept_invalid_certs has no effect in the browser");
    }
    crate::http::client(REQUEST_TIMEOUT).unwrap_or_else(|_| crate::http::async_client())
}

/// The API client for a server.
fn build_client(
    url: &str,
    username: &str,
    password: &str,
    accept_invalid_certs: bool,
) -> Result<Client, opensubsonic::Error> {
    Ok(Client::new(url, Auth::token(username, password))?
        .with_client_name("aulos")
        .with_http_client(http_client(accept_invalid_certs)))
}

/// The album's OpenSubsonic release types (`"Album"`, `"EP"`, `"Live"`, ...)
/// plus `"Compilation"` when the server flags it, in the `; `-separated form
/// the library's release-type tag uses. Empty when the server reports none.
fn release_type_tag(album: &opensubsonic::data::AlbumWithSongsId3) -> String {
    let mut types = album.release_types.clone().unwrap_or_default();
    if album.is_compilation == Some(true)
        && !types.iter().any(|t| t.eq_ignore_ascii_case("compilation"))
    {
        types.push("Compilation".to_owned());
    }
    types.join("; ")
}

/// The audio suffix of a Subsonic song (`"flac"`, `"m4a"`), lowercase without
/// the dot: the server's `suffix`, else the suffix of its file `path`, else
/// the one a well-known `contentType` implies; empty when none says.
fn remote_suffix(child: &opensubsonic::data::Child) -> String {
    let stated = child
        .suffix
        .as_deref()
        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    stated
        .or_else(|| {
            child
                .path
                .as_deref()
                .map(crate::library::suffix_of)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            let mime = child.content_type.as_deref()?.to_ascii_lowercase();
            let suffix = match mime.split(';').next()?.trim() {
                "audio/flac" | "audio/x-flac" => "flac",
                "audio/mpeg" | "audio/mp3" => "mp3",
                "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
                "audio/aac" | "audio/aacp" => "aac",
                "audio/ogg" | "application/ogg" => "ogg",
                "audio/opus" => "opus",
                "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
                "audio/aiff" | "audio/x-aiff" => "aiff",
                "audio/x-ms-wma" => "wma",
                "audio/x-ape" => "ape",
                "audio/x-wavpack" => "wv",
                _ => return None,
            };
            Some(suffix.to_owned())
        })
        .unwrap_or_default()
}

/// Unix seconds of a server timestamp (ISO 8601, e.g. `created` and
/// `starred`); `0` when absent or unparseable.
fn iso_secs(iso: Option<&str>) -> i64 {
    iso.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or(0, |dt| dt.timestamp())
}

/// Configuration for connecting to a Subsonic-compatible server.
#[derive(Clone)]
pub struct SubsonicConfig {
    /// Unique provider ID (e.g., "subsonic-home").
    pub id: String,
    /// Human-readable name (e.g., "Navidrome").
    pub name: String,
    /// Server base URL (e.g., "https://music.example.com").
    pub url: String,
    /// Subsonic username.
    pub username: String,
    /// Password for authentication.
    pub password: String,
    /// Accept invalid TLS certificates (self-signed, Tailscale, etc.).
    pub accept_invalid_certs: bool,
    /// Maximum bitrate for transcoding (None = original quality).
    pub transcoding_max_bitrate: Option<u32>,
    /// Transcoding format (None = original format, e.g., "mp3", "ogg", "opus").
    pub transcoding_format: Option<String>,
}

impl fmt::Debug for SubsonicConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubsonicConfig")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("accept_invalid_certs", &self.accept_invalid_certs)
            .field("transcoding_max_bitrate", &self.transcoding_max_bitrate)
            .field("transcoding_format", &self.transcoding_format)
            .finish()
    }
}

impl Default for SubsonicConfig {
    fn default() -> Self {
        Self {
            id: "subsonic".to_string(),
            name: "Subsonic Server".to_string(),
            url: "https://music.example.com".to_string(),
            username: String::new(),
            password: String::new(),
            accept_invalid_certs: false,
            transcoding_max_bitrate: None,
            transcoding_format: None,
        }
    }
}

impl From<crate::config::SubsonicConfigEntry> for SubsonicConfig {
    fn from(entry: crate::config::SubsonicConfigEntry) -> Self {
        // Task 85/86: Retrieve password from keyring when password_in_keyring is set.
        let password = if entry.password_in_keyring {
            match crate::credentials::retrieve_password(&entry.id) {
                Ok(Some(pw)) => pw,
                Ok(None) => {
                    // Task 86: password_in_keyring is true but no keyring entry found.
                    tracing::error!(
                        "Subsonic provider '{}': password marked as stored in keyring but not found. \
                         Please re-enter the password in provider settings.",
                        entry.id
                    );
                    String::new()
                }
                Err(e) => {
                    tracing::error!(
                        "Subsonic provider '{}': failed to retrieve password from keyring: {e}",
                        entry.id
                    );
                    String::new()
                }
            }
        } else {
            entry.password.unwrap_or_default()
        };

        Self {
            id: entry.id,
            name: entry.name,
            url: entry.url,
            username: entry.username,
            password,
            accept_invalid_certs: entry.accept_invalid_certs,
            transcoding_max_bitrate: entry.transcoding_max_bitrate,
            transcoding_format: entry.transcoding_format,
        }
    }
}

/// Signs a provider's stream URLs when a track is played. A track holds only
/// the song id (in `path`): the signed URL carries the user name and a login
/// token that stays valid as long as the password does, so it is never stored
/// in a [`Track`], the library database or a queue.
struct StreamSigner {
    client: Client,
    options: opensubsonic::StreamOptions,
    /// The provider's "accept invalid certificates" option, which the player
    /// needs to open the stream of a self-signed server.
    accept_invalid_certs: bool,
}

/// The live provider's signer by provider id. Weak: a replaced or removed
/// provider takes its signer (and credentials) with it.
static SIGNERS: LazyLock<Mutex<HashMap<String, Weak<StreamSigner>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The `source_uri` of a Subsonic track: a stable identity for the song that
/// holds no credential (the player signs the real URL, see [`stream_url`]).
fn track_locator(provider_id: &str, song_id: &str) -> String {
    format!("subsonic://{provider_id}/{song_id}")
}

/// What the player streams for a Subsonic `track`: the authenticated URL,
/// signed now by its provider, with the provider's certificate option. With
/// no such provider connected it is the track's own `source_uri` and the
/// default certificate checks.
#[must_use]
pub fn stream_source(track: &Track) -> TrackSource {
    let signer = SIGNERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&*track.provider_id)
        .and_then(Weak::upgrade);
    let Some(signer) = signer else {
        return TrackSource::HttpStream {
            url: track.source_uri.clone(),
            accept_invalid_certs: false,
        };
    };
    let url = signer
        .client
        .stream_url(&track.path.to_string_lossy(), &signer.options)
        .map_or_else(|_| track.source_uri.clone(), |url| url.to_string());
    TrackSource::HttpStream {
        url,
        accept_invalid_certs: signer.accept_invalid_certs,
    }
}

/// Subsonic music provider backed by the `opensubsonic` crate.
///
/// All API calls are async (via the opensubsonic `Client`), on whatever
/// executor polls them: tokio natively, the browser's event loop on wasm.
pub struct SubsonicProvider {
    config: SubsonicConfig,
    /// Shared provider ID for zero-copy assignment to tracks.
    provider_id: Arc<str>,
    client: Client,
    /// Registered under the provider id for [`stream_url`].
    _signer: Arc<StreamSigner>,
    /// Shared HTTP client for cover art fetches (avoids creating a new
    /// connection pool per request).
    http_client: reqwest::Client,
}

impl SubsonicProvider {
    /// Create a new Subsonic provider. The client is constructed immediately
    /// but no network call is made until `ping()` or a browse method is called.
    pub fn new(config: SubsonicConfig) -> Result<Self, ProviderError> {
        let client = build_client(
            &config.url,
            &config.username,
            &config.password,
            config.accept_invalid_certs,
        )
        .map_err(|e| ProviderError::NotConnected(format!("Invalid Subsonic URL: {e}")))?;

        let provider_id: Arc<str> = Arc::from(config.id.as_str());
        let http_client = http_client(config.accept_invalid_certs);
        // Transcoding parameters from the config, applied to every stream.
        let mut options = opensubsonic::StreamOptions::new();
        if let Some(b) = config.transcoding_max_bitrate {
            options = options.max_bit_rate(b as i32);
        }
        if let Some(f) = config.transcoding_format.as_deref() {
            options = options.format(f);
        }
        let signer = Arc::new(StreamSigner {
            client: client.clone(),
            options,
            accept_invalid_certs: config.accept_invalid_certs,
        });
        {
            let mut signers = SIGNERS.lock().unwrap_or_else(|e| e.into_inner());
            signers.retain(|_, signer| signer.strong_count() > 0);
            signers.insert(config.id.clone(), Arc::downgrade(&signer));
        }
        Ok(Self {
            config,
            provider_id,
            client,
            _signer: signer,
            http_client,
        })
    }

    /// Maps a failed request of operation `op` to the error shown to the
    /// user.
    fn err(&self, op: &'static str) -> impl FnOnce(opensubsonic::Error) -> ProviderError + '_ {
        move |e| classify(&self.config.url, op, &e, ProviderError::Io)
    }

    /// Validate connectivity by sending a `ping` request.
    pub async fn ping(&self) -> Result<(), ProviderError> {
        crate::http::check_mixed_content(&self.config.url).map_err(ProviderError::NotConnected)?;
        self.client
            .ping()
            .await
            .map_err(|e| classify(&self.config.url, "ping", &e, ProviderError::NotConnected))
    }

    /// Convert an opensubsonic `Child` (song) to our `Track` model.
    ///
    /// The song id goes in `path`; `source_uri` is a credential-free locator.
    /// The player asks [`stream_url`] for the signed URL when it plays the
    /// track.
    fn child_to_track(&self, child: &opensubsonic::data::Child) -> Track {
        let title = child.title.clone();
        let artist = child
            .artist
            .clone()
            .unwrap_or_else(|| "Unknown Artist".to_string());
        let album_artist = child
            .display_album_artist
            .clone()
            .or_else(|| {
                child
                    .album_artists
                    .as_ref()
                    .and_then(|a| a.first())
                    .map(|a| a.name.clone())
            })
            .unwrap_or_else(|| artist.clone());
        let album = child
            .album
            .clone()
            .unwrap_or_else(|| "Unknown Album".to_string());
        // Servers that only fill the OpenSubsonic `genres` list leave `genre` unset.
        let genre = child
            .genre
            .clone()
            .filter(|g| !g.is_empty())
            .or_else(|| {
                child
                    .genres
                    .as_ref()
                    .and_then(|g| g.first())
                    .map(|g| g.name.clone())
            })
            .unwrap_or_default();
        let track_number = child.track.unwrap_or(0) as u32;
        let disc_number = child.disc_number.unwrap_or(1) as u32;
        let year = child.year.unwrap_or(0) as u32;
        let duration = Duration::from_secs(child.duration.unwrap_or(0) as u64);
        let bitrate = child.bit_rate.unwrap_or(0) as u32;
        let sample_rate = child.sampling_rate.unwrap_or(0) as u32;
        let suffix = remote_suffix(child);
        let bit_depth =
            super::server_bit_depth(&suffix, child.bit_depth, child.bit_rate).unwrap_or(0);

        let source_uri = track_locator(&self.provider_id, &child.id);

        let is_favorite = child.starred.is_some();
        let rating = child
            .user_rating
            .and_then(|r| if r > 0 { Some(r as u8) } else { None });

        let rg_track_gain = child
            .replay_gain
            .as_ref()
            .and_then(|rg| rg.track_gain.map(|g| g as f32));
        let rg_album_gain = child
            .replay_gain
            .as_ref()
            .and_then(|rg| rg.album_gain.map(|g| g as f32));

        Track {
            id: 0,
            path: PathBuf::from(&child.id),
            title,
            artist,
            album_artist,
            album,
            genre,
            track_number,
            disc_number,
            year,
            duration,
            bitrate,
            sample_rate,
            bit_depth,
            provider_id: Arc::clone(&self.provider_id),
            source_uri,
            is_favorite,
            rating,
            rg_track_gain,
            rg_album_gain,
            release_type: String::new(),
            suffix,
            added_at: iso_secs(child.created.as_deref()),
            favorited_at: iso_secs(child.starred.as_deref()),
        }
    }

    /// Fetch a single page of albums with full details (songs, cover art).
    ///
    /// Returns `(albums, has_more)`. Each page fetches up to `page_size` album
    /// stubs from `getAlbumList2`, then fetches full details (with songs) for each.
    /// This is the building block for incremental library loading.
    pub async fn browse_albums_page(
        &self,
        offset: i32,
        page_size: i32,
    ) -> Result<(Vec<Album>, bool), ProviderError> {
        let album_list = self.fetch_album_list(page_size, offset).await?;

        let count = album_list.len();
        let details = self
            .fetch_album_details(album_list.iter().map(|a| a.id.clone()).collect())
            .await?;

        let mut albums = Vec::with_capacity(count);
        for album_detail in details {
            let release_type = release_type_tag(&album_detail);
            let mut tracks: Vec<Track> = album_detail
                .song
                .iter()
                .map(|s| {
                    let mut track = self.child_to_track(s);
                    track.release_type.clone_from(&release_type);
                    track
                })
                .collect();
            Track::sort_by_disc_and_track(&mut tracks);

            let cover_source = self.cover_source_from_id(&album_detail.cover_art);

            let mut album = Album::from_tracks(album_detail.name.clone(), tracks, cover_source);
            // Override artist from album metadata (more reliable than first track).
            if let Some(artist) = &album_detail.artist {
                album.artist = artist.clone();
            }
            album.year = album_detail.year.unwrap_or(0) as u32;
            album.refresh_derived();
            albums.push(album);
        }

        let has_more = count >= page_size as usize;
        Ok((albums, has_more))
    }

    /// `getAlbum` (songs included) for each of `ids`, in order. `buffered`
    /// keeps the server's order while overlapping the round trips (one
    /// serial request per album made a first sync cost hundreds of RTTs).
    async fn fetch_album_details(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<opensubsonic::data::AlbumWithSongsId3>, ProviderError> {
        use futures::StreamExt;
        let this = self;
        futures::stream::iter(ids)
            .map(|id| async move {
                this.client
                    .get_album(&id)
                    .await
                    .map_err(this.err("getAlbum"))
            })
            .buffered(ALBUM_FETCH_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect()
    }

    /// The server's library-change marker (`getIndexes` `lastModified`,
    /// the last scan time on Navidrome and OpenSubsonic servers); `None`
    /// when the server does not report one or the request fails.
    pub async fn library_marker(&self) -> Option<i64> {
        self.client
            .get_indexes(None, None)
            .await
            .ok()
            .and_then(|indexes| indexes.last_modified)
    }

    /// Build a `CoverSource` from a Subsonic cover art ID.
    fn cover_source_from_id(&self, cover_id: &Option<String>) -> Option<CoverSource> {
        cover_id.as_ref().and_then(|id| {
            self.client
                .cover_art_url(id, Some(300))
                .ok()
                .map(|url| CoverSource::Url(url.to_string()))
        })
    }

    /// Fetch a page of album stubs from `getAlbumList2`.
    async fn fetch_album_list(
        &self,
        page_size: i32,
        offset: i32,
    ) -> Result<Vec<opensubsonic::data::AlbumId3>, ProviderError> {
        self.client
            .get_album_list2(
                opensubsonic::AlbumListType::AlphabeticalByName,
                &opensubsonic::AlbumListOptions::new()
                    .size(page_size)
                    .offset(offset),
            )
            .await
            .map_err(self.err("getAlbumList2"))
    }

    /// Scrobble a track (mark as played) on the Subsonic server; failures
    /// are only logged.
    ///
    /// The `song_id` is the Subsonic song ID (not the stream URL).
    /// Call this when a track reaches 50% or 4 minutes of playback.
    pub async fn scrobble(&self, song_id: &str) {
        match self.client.scrobble(song_id, None, Some(true)).await {
            Ok(_) => tracing::debug!("Scrobbled song '{song_id}'"),
            Err(e) => tracing::warn!(
                "Subsonic scrobble failed for '{song_id}': {}",
                describe_error(&self.config.url, "scrobble", &e)
            ),
        }
    }

    /// Send a "now playing" notification to the Subsonic server; failures
    /// are only logged.
    pub async fn now_playing(&self, song_id: &str) {
        if let Err(e) = self.client.scrobble(song_id, None, Some(false)).await {
            tracing::debug!(
                "Subsonic now-playing failed for '{song_id}': {}",
                describe_error(&self.config.url, "now playing", &e)
            );
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl MusicProvider for SubsonicProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn name(&self) -> &str {
        &self.config.name
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Subsonic
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn browse_albums(&self) -> Result<Vec<Album>, ProviderError> {
        let mut all_albums = Vec::new();
        let mut offset = 0;
        let page_size = 500;

        for _ in 0..MAX_LIST_PAGES {
            let (batch, has_more) = self.browse_albums_page(offset, page_size).await?;
            if batch.is_empty() {
                break;
            }
            let count = batch.len();
            all_albums.extend(batch);
            if !has_more {
                break;
            }
            offset += count as i32;
        }

        all_albums.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(all_albums)
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn browse_artists(&self) -> Result<Vec<Artist>, ProviderError> {
        let artists_id3 = self
            .client
            .get_artists(None)
            .await
            .map_err(self.err("getArtists"))?;

        let mut artists = Vec::new();

        for index in &artists_id3.index {
            for artist_id3 in &index.artist {
                // Get artist details with album list.
                let artist_detail = self
                    .client
                    .get_artist(&artist_id3.id)
                    .await
                    .map_err(self.err("getArtist"))?;

                let mut artist_albums = Vec::new();

                // Full albums (with songs), fetched concurrently.
                let details = self
                    .fetch_album_details(artist_detail.album.iter().map(|a| a.id.clone()).collect())
                    .await?;
                for album_detail in details {
                    let release_type = release_type_tag(&album_detail);
                    let mut tracks: Vec<Track> = album_detail
                        .song
                        .iter()
                        .map(|s| {
                            let mut track = self.child_to_track(s);
                            track.release_type.clone_from(&release_type);
                            track
                        })
                        .collect();
                    Track::sort_by_disc_and_track(&mut tracks);

                    let cover_source = self.cover_source_from_id(&album_detail.cover_art);

                    let mut album =
                        Album::from_tracks(album_detail.name.clone(), tracks, cover_source);
                    album.artist = artist_id3.name.clone();
                    album.year = album_detail.year.unwrap_or(0) as u32;
                    album.refresh_derived();
                    artist_albums.push(Arc::new(album));
                }

                artist_albums.sort_by_key(|a| a.year);

                artists.push(Artist::new(artist_id3.name.clone(), artist_albums));
            }
        }

        artists.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(artists)
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn browse_tracks(&self) -> Result<Vec<Track>, ProviderError> {
        let mut all_tracks = Vec::new();
        let page_size = 500;
        let mut offset = 0;

        for _ in 0..MAX_LIST_PAGES {
            let album_list = self.fetch_album_list(page_size, offset).await?;

            let count = album_list.len();
            if count == 0 {
                break;
            }

            let details = self
                .fetch_album_details(album_list.iter().map(|a| a.id.clone()).collect())
                .await?;
            for album_detail in &details {
                for song in &album_detail.song {
                    all_tracks.push(self.child_to_track(song));
                }
            }

            if count < page_size as usize {
                break;
            }
            offset += page_size;
        }

        all_tracks.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(all_tracks)
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn search(&self, query: &str) -> Result<Vec<Track>, ProviderError> {
        let query_owned = query.to_string();
        let results = self
            .client
            .search3(
                &query_owned,
                &opensubsonic::Search3Options::new()
                    .artist_count(0) // no artists
                    .album_count(0) // no albums
                    .song_count(50), // up to 50 songs
            )
            .await
            .map_err(self.err("search3"))?;

        let tracks: Vec<Track> = results
            .song
            .iter()
            .map(|s| self.child_to_track(s))
            .collect();
        Ok(tracks)
    }

    #[tracing::instrument(skip(self, album), level = "debug")]
    async fn get_cover_art(&self, album: &Album) -> Result<Option<Vec<u8>>, ProviderError> {
        // The cover_source should already contain a Url from browse_albums().
        // Fetch the image bytes for display.
        match &album.cover_source {
            Some(CoverSource::Url(url)) => {
                let resp = self
                    .http_client
                    .get(url)
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(|e| self.err("cover art fetch")(e.without_url().into()))?;
                let bytes = crate::online::read_capped_response(resp, MAX_IMAGE_BYTES)
                    .await
                    .map_err(|e| ProviderError::Io(format!("Subsonic cover art read: {e}")))?;
                Ok(Some(bytes))
            }
            _ => {
                // Try to get cover art from the first track's album ID.
                if let Some(first_track) = album.tracks.first() {
                    // A Subsonic song id is stored in `path`; it doubles as a cover art id.
                    match self
                        .client
                        .get_cover_art(&first_track.path.to_string_lossy(), Some(300))
                        .await
                    {
                        Ok(bytes) => Ok(Some(bytes.to_vec())),
                        Err(e) => {
                            tracing::warn!(
                                "Subsonic cover art for '{}': {}",
                                album.name,
                                describe_error(&self.config.url, "getCoverArt", &e)
                            );
                            Ok(None)
                        }
                    }
                } else {
                    Ok(None)
                }
            }
        }
    }

    async fn get_lyrics(&self, track: &Track) -> Result<Option<Lyrics>, ProviderError> {
        // Extract the Subsonic song ID from the stream URL path.
        let song_id = track.path.to_string_lossy();
        // Try getLyricsBySongId first (OpenSubsonic extension).
        match self.client.get_lyrics_by_song_id(&song_id, None).await {
            Ok(lyrics_list) => {
                if let Some(structured) = lyrics_list.structured_lyrics.first() {
                    if structured.synced {
                        // Build synced lyrics from timestamps.
                        let offset_ms = structured.offset.unwrap_or(0.0);
                        let lines: Vec<LyricLine> = structured
                            .line
                            .iter()
                            .filter_map(|l| {
                                l.start.map(|start| LyricLine {
                                    timestamp_ms: (start + offset_ms).max(0.0) as u64,
                                    text: l.value.clone(),
                                })
                            })
                            .collect();
                        if !lines.is_empty() {
                            return Ok(Some(Lyrics::Synced(lines)));
                        }
                    }
                    // Unsynced structured lyrics — join lines.
                    let text: String = structured
                        .line
                        .iter()
                        .map(|l| l.value.as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        return Ok(Some(Lyrics::Unsynced(text)));
                    }
                }
            }
            Err(e) => {
                tracing::debug!(
                    "getLyricsBySongId not available: {}",
                    describe_error(&self.config.url, "getLyricsBySongId", &e)
                );
            }
        }

        // Fallback: legacy getLyrics with artist + title.
        match self
            .client
            .get_lyrics(Some(&track.artist), Some(&track.title))
            .await
        {
            Ok(lyrics) => {
                if let Some(text) = lyrics.value
                    && !text.is_empty()
                {
                    return Ok(Some(Lyrics::Unsynced(text)));
                }
            }
            Err(e) => {
                tracing::debug!(
                    "getLyrics not available: {}",
                    describe_error(&self.config.url, "getLyrics", &e)
                );
            }
        }

        Ok(None)
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn sync_library(&self, _progress: Arc<ScanProgress>) -> Result<usize, ProviderError> {
        // Subsonic servers manage their own library scanning.
        // We can trigger a scan via startScan, but the actual library
        // data is fetched on demand via browse_*() methods.
        match self.client.start_scan().await {
            Ok(_) => tracing::info!("Triggered Subsonic library scan"),
            Err(e) => tracing::warn!(
                "Subsonic startScan: {}",
                describe_error(&self.config.url, "startScan", &e)
            ),
        }
        Ok(0)
    }

    // --- Playlists ---

    #[tracing::instrument(skip(self), level = "debug")]
    async fn list_playlists(&self) -> Result<Vec<Playlist>, ProviderError> {
        let playlists = self
            .client
            .get_playlists(None)
            .await
            .map_err(self.err("getPlaylists"))?;

        Ok(playlists
            .into_iter()
            .map(|p| Playlist {
                id: p.id,
                name: p.name,
                tracks: Vec::new(),
                track_count: p.song_count.unwrap_or(0) as u32,
                total_duration: Duration::from_secs(p.duration.unwrap_or(0) as u64),
            })
            .collect())
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn get_playlist(&self, id: &str) -> Result<Playlist, ProviderError> {
        let id_owned = id.to_string();
        let p = self
            .client
            .get_playlist(&id_owned)
            .await
            .map_err(self.err("getPlaylist"))?;

        let tracks: Vec<Track> = p.entry.iter().map(|s| self.child_to_track(s)).collect();
        let total_duration = tracks.iter().map(|t| t.duration).sum();

        Ok(Playlist {
            id: p.id,
            name: p.name,
            track_count: tracks.len() as u32,
            total_duration,
            tracks,
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn create_playlist(&self, name: &str) -> Result<Playlist, ProviderError> {
        let name_owned = name.to_string();
        let p = self
            .client
            .create_playlist(None, Some(&name_owned), &[])
            .await
            .map_err(self.err("createPlaylist"))?;

        Ok(Playlist {
            id: p.id,
            name: p.name,
            tracks: Vec::new(),
            track_count: 0,
            total_duration: Duration::ZERO,
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn delete_playlist(&self, id: &str) -> Result<(), ProviderError> {
        let id_owned = id.to_string();
        self.client
            .delete_playlist(&id_owned)
            .await
            .map_err(self.err("deletePlaylist"))
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn rename_playlist(&self, id: &str, new_name: &str) -> Result<(), ProviderError> {
        let id_owned = id.to_string();
        let name_owned = new_name.to_string();
        self.client
            .update_playlist(
                &id_owned,
                &opensubsonic::UpdatePlaylistOptions::new().name(name_owned.as_str()),
            )
            .await
            .map_err(self.err("updatePlaylist"))
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn add_to_playlist(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> Result<(), ProviderError> {
        let pid = playlist_id.to_string();
        let ids: Vec<String> = track_ids.to_vec();
        self.client
            .update_playlist(
                &pid,
                &opensubsonic::UpdatePlaylistOptions::new().add_songs(ids.iter().cloned()),
            )
            .await
            .map_err(self.err("updatePlaylist"))
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn remove_from_playlist(
        &self,
        playlist_id: &str,
        index: usize,
    ) -> Result<(), ProviderError> {
        let pid = playlist_id.to_string();
        let index = i32::try_from(index)
            .map_err(|_| ProviderError::Other(format!("Playlist index {index} out of range")))?;
        self.client
            .update_playlist(
                &pid,
                &opensubsonic::UpdatePlaylistOptions::new().remove_index(index),
            )
            .await
            .map_err(self.err("updatePlaylist"))
    }

    // --- Favorites and ratings ---

    #[tracing::instrument(skip(self), level = "debug")]
    async fn toggle_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let id = track_id.to_string();
        // Check if already starred by fetching the song.
        let song = self
            .client
            .get_song(&id)
            .await
            .map_err(self.err("getSong"))?;

        if song.starred.is_some() {
            // Currently starred → unstar.
            self.client
                .unstar(&[id.as_str()], &[], &[])
                .await
                .map_err(self.err("unstar"))?;
            Ok(false)
        } else {
            // Not starred → star.
            self.client
                .star(&[id.as_str()], &[], &[])
                .await
                .map_err(self.err("star"))?;
            Ok(true)
        }
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn is_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let id = track_id.to_string();
        let song = self
            .client
            .get_song(&id)
            .await
            .map_err(self.err("getSong"))?;
        Ok(song.starred.is_some())
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn set_rating(&self, track_id: &str, rating: u8) -> Result<(), ProviderError> {
        let id = track_id.to_string();
        self.client
            .set_rating(&id, rating as i32)
            .await
            .map_err(self.err("setRating"))
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn get_rating(&self, track_id: &str) -> Result<Option<u8>, ProviderError> {
        let id = track_id.to_string();
        let song = self
            .client
            .get_song(&id)
            .await
            .map_err(self.err("getSong"))?;
        Ok(song
            .user_rating
            .and_then(|r| if r > 0 { Some(r as u8) } else { None }))
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn list_favorites(&self) -> Result<Vec<Track>, ProviderError> {
        let starred = self
            .client
            .get_starred2(None)
            .await
            .map_err(self.err("getStarred2"))?;

        let tracks: Vec<Track> = starred
            .song
            .iter()
            .map(|s| self.child_to_track(s))
            .collect();
        Ok(tracks)
    }

    // --- Genres ---

    #[tracing::instrument(skip(self), level = "debug")]
    async fn list_genres(&self) -> Result<Vec<String>, ProviderError> {
        let genres = self
            .client
            .get_genres()
            .await
            .map_err(self.err("getGenres"))?;

        let mut names: Vec<String> = genres.into_iter().map(|g| g.name).collect();
        names.sort();
        Ok(names)
    }

    #[tracing::instrument(skip(self), level = "debug")]
    async fn get_tracks_by_genre(&self, genre: &str) -> Result<Vec<Track>, ProviderError> {
        let genre_owned = genre.to_string();
        let songs = self
            .client
            .get_songs_by_genre(
                &genre_owned,
                &opensubsonic::SongsByGenreOptions::new().count(500),
            )
            .await
            .map_err(self.err("getSongsByGenre"))?;

        let tracks: Vec<Track> = songs.iter().map(|s| self.child_to_track(s)).collect();
        Ok(tracks)
    }

    // --- Artist metadata ---

    /// Serves the server's own artist bio/image (`getArtistInfo2` and the
    /// artist's cover art / `artistImageUrl`) regardless of Aulos's
    /// "fetch artist images and info online" setting — that toggle only
    /// gates Aulos's own Deezer/Wikipedia lookup for Local/MPD, which
    /// would be redundant (and pointlessly slower) next to data the
    /// Subsonic/Navidrome server already curates.
    #[tracing::instrument(skip(self), level = "debug")]
    async fn get_artist_info(
        &self,
        name: &str,
    ) -> Result<Option<crate::library::ArtistInfoResult>, ProviderError> {
        let indexes = self
            .client
            .get_artists(None)
            .await
            .map_err(self.err("getArtists"))?;

        let Some(artist_ref) = indexes
            .index
            .iter()
            .flat_map(|idx| idx.artist.iter())
            .find(|a| a.name.eq_ignore_ascii_case(name))
        else {
            return Ok(None);
        };
        let artist_id = artist_ref.id.clone();

        let detail = self
            .client
            .get_artist(&artist_id)
            .await
            .map_err(self.err("getArtist"))?;

        let bio = self
            .client
            .get_artist_info2(&artist_id, None, None)
            .await
            .ok()
            .and_then(|info| info.biography)
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty());

        // Prefer the server's own cover art (an authenticated URL,
        // already signed by the client, matching `get_cover_art`'s
        // `CoverSource::Url` handling above) over the external
        // `artistImageUrl`, which may be stale or absent on servers
        // that don't scrape one.
        let image_url = self
            .cover_source_from_id(&detail.cover_art)
            .and_then(|src| match src {
                CoverSource::Url(u) => Some(u),
                CoverSource::LocalFile(_) | CoverSource::MpdAlbumArt(_) => None,
            })
            .or_else(|| detail.artist_image_url.clone());

        // The URL may come from the server's own metadata (`artistImageUrl`):
        // fetch only http(s), and never more than an image's worth of bytes.
        let image_bytes = match image_url.filter(|u| crate::online::validate_http_url(u).is_ok()) {
            Some(url) => match self.http_client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    crate::online::read_capped_response(resp, MAX_IMAGE_BYTES)
                        .await
                        .ok()
                }
                _ => None,
            },
            None => None,
        };

        if bio.is_none() && image_bytes.is_none() {
            return Ok(None);
        }

        Ok(Some(crate::library::ArtistInfoResult { bio, image_bytes }))
    }

    #[tracing::instrument(skip(self, track), level = "debug")]
    async fn track_tech_info(
        &self,
        track: &Track,
    ) -> Result<Option<crate::library::tech_info::TechInfo>, ProviderError> {
        // The song ID lives in `track.path` (see `child_to_track`);
        // `source_uri` is only an identity locator, not the song id.
        let song_id = track.path.to_string_lossy().to_string();
        let song = self
            .client
            .get_song(&song_id)
            .await
            .map_err(self.err("getSong"))?;
        Ok(Some(song_to_tech_info(&song)))
    }
}

/// Convert a `getSong` response into the Properties drawer's display model.
///
/// Servers report zero for values they could not determine, so every
/// numeric field is kept only when positive — the drawer then shows an em
/// dash instead of a misleading `0`. Bit depth is dropped for lossy formats
/// (it is meaningless there and the drawer shows "n/a" instead).
fn song_to_tech_info(song: &opensubsonic::data::Child) -> crate::library::tech_info::TechInfo {
    use crate::library::tech_info::{ReplayGain, TechInfo};

    let positive = |v: Option<i32>| v.filter(|v| *v > 0).map(|v| v as u32);
    let suffix = remote_suffix(song);
    let suffix = suffix.as_str();
    let lossless = super::lossless_from_suffix(suffix);
    let bitrate = positive(song.bit_rate);
    let channels = positive(song.channel_count);
    let bits = super::server_bit_depth(suffix, song.bit_depth, song.bit_rate);
    let gain = |v: Option<f64>| v.map(|g| format!("{g:.2} dB"));
    let peak = |v: Option<f64>| v.map(|p| format!("{p:.6}"));
    let replay_gain = song
        .replay_gain
        .as_ref()
        .map(|rg| ReplayGain {
            track_gain: gain(rg.track_gain),
            track_peak: peak(rg.track_peak),
            album_gain: gain(rg.album_gain),
            album_peak: peak(rg.album_peak),
        })
        .unwrap_or_default();

    TechInfo {
        container: super::container_from_suffix(suffix),
        codec_short: (!suffix.is_empty()).then(|| suffix.to_ascii_lowercase()),
        sample_rate: positive(song.sampling_rate),
        bits_per_sample: bits,
        lossless,
        channels,
        channel_layout: channels.and_then(super::channel_layout_name),
        duration: song
            .duration
            .filter(|d| *d > 0)
            .map(|d| Duration::from_secs(d as u64)),
        file_size: song.size.filter(|s| *s > 0).map(|s| s as u64),
        bitrate_kbps: bitrate,
        nominal_bitrate_kbps: bitrate,
        replay_gain,
        date: song.year.filter(|y| *y > 0).map(|y| y.to_string()),
        mime_type: song.content_type.clone().filter(|m| !m.is_empty()),
        remote_path: song.path.clone().filter(|p| !p.is_empty()),
        composer: song.display_composer.clone().filter(|c| !c.is_empty()),
        ..TechInfo::default()
    }
}

#[cfg(test)]
mod release_type_tests {
    use super::release_type_tag;
    use opensubsonic::data::AlbumWithSongsId3;

    fn album(extra: &str) -> AlbumWithSongsId3 {
        serde_json::from_str(&format!(
            r#"{{"id":"a","name":"n","songCount":0,"duration":0,"created":"2020-01-01T00:00:00Z"{extra}}}"#
        ))
        .expect("valid album JSON")
    }

    #[test]
    fn release_types_and_compilation_flag_become_the_tag() {
        assert_eq!(release_type_tag(&album("")), "");
        assert_eq!(
            release_type_tag(&album(r#","releaseTypes":["Album","Live"]"#)),
            "Album; Live"
        );
        assert_eq!(
            release_type_tag(&album(r#","releaseTypes":["Album"],"isCompilation":true"#)),
            "Album; Compilation"
        );
        assert_eq!(
            release_type_tag(&album(
                r#","releaseTypes":["compilation"],"isCompilation":true"#
            )),
            "compilation"
        );
    }
}

#[cfg(test)]
mod suffix_tests {
    use super::remote_suffix;
    use opensubsonic::data::Child;

    fn song(extra: &str) -> Child {
        serde_json::from_str(&format!(r#"{{"id":"1","title":"t"{extra}}}"#)).expect("valid Child")
    }

    #[test]
    fn suffix_comes_from_the_server_then_the_path_then_the_content_type() {
        assert_eq!(remote_suffix(&song(r#","suffix":".FLAC""#)), "flac");
        assert_eq!(
            remote_suffix(&song(r#","suffix":"","path":"A/B/c.M4a""#)),
            "m4a"
        );
        assert_eq!(
            remote_suffix(&song(r#","contentType":"audio/mpeg; charset=x""#)),
            "mp3"
        );
        assert_eq!(remote_suffix(&song(r#","contentType":"video/x""#)), "");
        assert_eq!(remote_suffix(&song("")), "");
    }
}

#[cfg(test)]
mod timestamp_tests {
    use super::iso_secs;

    #[test]
    fn server_timestamps_become_unix_seconds() {
        assert_eq!(iso_secs(Some("2024-05-01T12:00:00.000Z")), 1_714_564_800);
        assert_eq!(iso_secs(Some("2024-05-01T14:00:00+02:00")), 1_714_564_800);
        assert_eq!(iso_secs(Some("garbage")), 0);
        assert_eq!(iso_secs(None), 0);
    }
}

#[cfg(test)]
mod tech_info_tests {
    use super::song_to_tech_info;
    use opensubsonic::data::Child;

    fn song(json: &str) -> Child {
        serde_json::from_str(json).expect("valid Child JSON")
    }

    #[test]
    fn lossless_song_keeps_bit_depth_and_server_fields() {
        let info = song_to_tech_info(&song(
            r#"{"id":"1","title":"t","suffix":"flac","contentType":"audio/flac",
                "bitRate":900,"bitDepth":24,"samplingRate":96000,"channelCount":2,
                "size":1000,"duration":10,"path":"A/B/c.flac","year":1999}"#,
        ));
        assert_eq!(info.container.as_deref(), Some("FLAC"));
        assert_eq!(info.bits_per_sample, Some(24));
        assert_eq!(info.sample_rate, Some(96_000));
        assert_eq!(info.channel_layout.as_deref(), Some("Stereo"));
        assert_eq!(info.bitrate_kbps, Some(900));
        assert_eq!(info.remote_path.as_deref(), Some("A/B/c.flac"));
        assert_eq!(info.date.as_deref(), Some("1999"));
    }

    #[test]
    fn zero_and_missing_values_become_none_and_lossy_drops_bit_depth() {
        let info = song_to_tech_info(&song(
            r#"{"id":"1","title":"t","suffix":"mp3","bitRate":0,"bitDepth":16,
                "samplingRate":0,"size":0,"duration":0}"#,
        ));
        assert_eq!(info.lossless, Some(false));
        assert_eq!(info.bits_per_sample, None);
        assert_eq!(info.bitrate_kbps, None);
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.file_size, None);
        assert_eq!(info.duration, None);
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;
    use opensubsonic::{Error, SubsonicApiError};

    fn api(code: i32) -> Error {
        Error::Api(SubsonicApiError {
            code,
            message: "nope".into(),
            help_url: None,
        })
    }

    #[test]
    fn credential_api_codes_map_to_unauthorized() {
        for code in [40, 41, 42, 43, 44] {
            let error = classify("http://h", "ping", &api(code), ProviderError::Io);
            assert!(matches!(error, ProviderError::Unauthorized(_)), "{code}");
        }
    }

    #[test]
    fn other_api_codes_keep_the_callers_error() {
        for code in [0, 10, 20, 30, 50, 60, 70] {
            let error = classify("http://h", "ping", &api(code), ProviderError::Io);
            assert!(matches!(error, ProviderError::Io(_)), "{code}");
        }
        let parse = Error::Parse("x".into());
        let error = classify("http://h", "ping", &parse, ProviderError::NotConnected);
        assert!(matches!(error, ProviderError::NotConnected(_)));
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod connection_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves `answers` (status line, JSON body) to successive requests.
    async fn serve(answers: Vec<(&'static str, &'static str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            for (status, body) in answers {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    const PONG: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#;
    const WRONG_PASSWORD: &str = r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#;

    #[tokio::test]
    async fn a_server_that_answers_ping_connects() {
        let url = serve(vec![("200 OK", PONG)]).await;
        assert_eq!(test_connection(&url, "u", "p", false).await, Ok(()));
    }

    #[tokio::test]
    async fn api_errors_keep_the_servers_own_message() {
        let url = serve(vec![("200 OK", WRONG_PASSWORD)]).await;
        let error = test_connection(&url, "u", "bad", false)
            .await
            .expect_err("rejected");
        assert!(matches!(error, ProviderError::Unauthorized(_)), "{error:?}");
        assert!(
            error.to_string().contains("Wrong username or password"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_http_401_is_a_credential_rejection() {
        let url = serve(vec![("401 Unauthorized", "")]).await;
        let error = test_connection(&url, "u", "p", false)
            .await
            .expect_err("unauthorized");
        assert!(matches!(error, ProviderError::Unauthorized(_)), "{error:?}");
    }

    #[tokio::test]
    async fn http_status_errors_name_the_host_and_status() {
        let url = serve(vec![("503 Service Unavailable", "")]).await;
        let error = test_connection(&url, "u", "p", false)
            .await
            .expect_err("unavailable");
        assert!(matches!(error, ProviderError::Other(_)), "{error:?}");
        let error = error.to_string();
        assert!(error.contains("503"), "{error}");
        assert!(error.contains("127.0.0.1"), "{error}");
    }

    #[tokio::test]
    async fn transport_errors_do_not_leak_the_request_credentials() {
        // A scheme reqwest rejects fails the request without any network.
        let failure = reqwest::Client::new()
            .get("ftp://127.0.0.1/rest/ping?u=alice&t=secret&s=salt")
            .send()
            .await
            .expect_err("unsupported scheme");
        let message = describe_error(
            "http://127.0.0.1",
            "ping",
            &opensubsonic::Error::Http(failure),
        );
        assert!(!message.contains("secret"), "{message}");
        assert!(!message.contains("salt"), "{message}");
    }
}

#[cfg(test)]
mod stream_url_tests {
    use super::*;
    use opensubsonic::data::Child;

    fn provider(id: &str) -> SubsonicProvider {
        SubsonicProvider::new(SubsonicConfig {
            id: id.to_owned(),
            url: "https://music.example.com".to_owned(),
            username: "alice".to_owned(),
            password: "hunter2".to_owned(),
            transcoding_format: Some("mp3".to_owned()),
            ..SubsonicConfig::default()
        })
        .expect("valid config")
    }

    fn song() -> Child {
        serde_json::from_str(r#"{"id":"song-7","title":"t"}"#).expect("valid Child")
    }

    fn http_stream(track: &Track) -> (String, bool) {
        match stream_source(track) {
            TrackSource::HttpStream {
                url,
                accept_invalid_certs,
            } => (url, accept_invalid_certs),
            other => panic!("not an HTTP stream: {other:?}"),
        }
    }

    #[test]
    fn tracks_hold_no_credential_and_the_player_signs_at_playback() {
        let provider = provider("subsonic-signing-test");
        let track = provider.child_to_track(&song());
        assert_eq!(track.path, PathBuf::from("song-7"));
        assert_eq!(track.source_uri, "subsonic://subsonic-signing-test/song-7");
        assert!(!track.source_uri.contains("alice"));
        let (url, accept_invalid_certs) = http_stream(&track);
        assert!(!accept_invalid_certs);
        assert!(
            url.starts_with("https://music.example.com/rest/stream"),
            "{url}"
        );
        assert!(url.contains("id=song-7"), "{url}");
        assert!(url.contains("u=alice"), "{url}");
        assert!(url.contains("format=mp3"), "{url}");
        assert!(!url.contains("hunter2"), "{url}");
    }

    #[test]
    fn without_a_live_provider_the_stored_uri_is_used() {
        let mut track = provider("subsonic-gone-test").child_to_track(&song());
        track.provider_id = Arc::from("subsonic-never-registered");
        track.source_uri = "https://old.example.com/rest/stream?id=x".to_owned();
        assert_eq!(http_stream(&track), (track.source_uri.clone(), false));
    }

    #[test]
    fn the_stream_carries_the_providers_certificate_option() {
        let provider = SubsonicProvider::new(SubsonicConfig {
            id: "subsonic-self-signed-test".to_owned(),
            username: "alice".to_owned(),
            password: "hunter2".to_owned(),
            accept_invalid_certs: true,
            ..SubsonicConfig::default()
        })
        .expect("valid config");
        let track = provider.child_to_track(&song());
        let (url, accept_invalid_certs) = http_stream(&track);
        assert!(url.contains("id=song-7"), "{url}");
        assert!(accept_invalid_certs);
    }

    #[test]
    fn debug_output_hides_the_password() {
        let text = format!(
            "{:?}",
            SubsonicConfig {
                password: "hunter2".to_owned(),
                ..SubsonicConfig::default()
            }
        );
        assert!(!text.contains("hunter2"), "{text}");
    }
}
