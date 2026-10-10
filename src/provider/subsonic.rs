// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! OpenSubsonic/Navidrome music provider.
//!
//! Connects to a Subsonic-compatible server using the `opensubsonic` crate,
//! browses the library via the ID3-based API, and streams audio via HTTP URLs
//! decoded locally by the `LocalBackend`.

use super::{MusicProvider, ProviderError, ProviderType};
use crate::library::{Album, Artist, CoverSource, LyricLine, Lyrics, Playlist, Track};
use opensubsonic::{Auth, Client};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Helper to wrap Subsonic errors into `ProviderError::Io` with a labeled context.
fn subsonic_err<E: std::fmt::Display>(op: &str) -> impl FnOnce(E) -> ProviderError {
    super::wrap_err("Subsonic", op)
}

/// Configuration for connecting to a Subsonic-compatible server.
#[derive(Debug, Clone)]
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

/// Subsonic music provider backed by the `opensubsonic` crate.
///
/// All API calls are async (via the opensubsonic `Client`). The `MusicProvider`
/// trait is synchronous, so we bridge using `runtime.block_on()`.
pub struct SubsonicProvider {
    config: SubsonicConfig,
    /// Shared provider ID for zero-copy assignment to tracks.
    provider_id: Arc<str>,
    client: Client,
    /// Shared HTTP client for cover art fetches (avoids creating a new
    /// connection pool per request).
    http_client: reqwest::Client,
    runtime: tokio::runtime::Handle,
}

impl SubsonicProvider {
    /// Create a new Subsonic provider. The client is constructed immediately
    /// but no network call is made until `ping()` or a browse method is called.
    pub fn new(
        config: SubsonicConfig,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, ProviderError> {
        let auth = Auth::token(&config.username, &config.password);
        let mut client = Client::new(&config.url, auth)
            .map_err(|e| ProviderError::NotConnected(format!("Invalid Subsonic URL: {e}")))?
            .with_client_name("aulos");

        if config.accept_invalid_certs {
            client = client
                .with_danger_accept_invalid_certs()
                .map_err(|e| ProviderError::NotConnected(format!("TLS config error: {e}")))?;
        }

        let provider_id: Arc<str> = Arc::from(config.id.as_str());
        let http_client = reqwest::Client::new();
        Ok(Self {
            config,
            provider_id,
            client,
            http_client,
            runtime,
        })
    }

    /// Validate connectivity by sending a `ping` request.
    pub async fn ping(&self) -> Result<(), ProviderError> {
        self.client
            .ping()
            .await
            .map_err(|e| ProviderError::NotConnected(format!("Subsonic ping failed: {e}")))
    }

    /// Run an async block on the tokio runtime (bridging sync MusicProvider to async).
    fn block_on<F, T>(&self, future: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.runtime.block_on(future)
    }

    /// Convert an opensubsonic `Child` (song) to our `Track` model.
    ///
    /// The `source_uri` is set to the full authenticated stream URL so that
    /// `resolve_track_source()` in the player can create an `HttpStream`
    /// without needing access to the provider/client.
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
        let genre = child.genre.clone().unwrap_or_default();
        let track_number = child.track.unwrap_or(0) as u32;
        let disc_number = child.disc_number.unwrap_or(1) as u32;
        let year = child.year.unwrap_or(0) as u32;
        let duration = Duration::from_secs(child.duration.unwrap_or(0) as u64);
        let bitrate = child.bit_rate.unwrap_or(0) as u32;
        let sample_rate = child.sampling_rate.unwrap_or(0) as u32;

        // Pre-build the authenticated stream URL so the player can use it
        // directly without needing access to the Subsonic client.
        // Pass transcoding parameters from config if configured.
        let mut stream_opts = opensubsonic::StreamOptions::new();
        if let Some(b) = self.config.transcoding_max_bitrate {
            stream_opts = stream_opts.max_bit_rate(b as i32);
        }
        if let Some(f) = self.config.transcoding_format.as_deref() {
            stream_opts = stream_opts.format(f);
        }
        let source_uri = self
            .client
            .stream_url(&child.id, &stream_opts)
            .map(|url| url.to_string())
            .unwrap_or_else(|_| child.id.clone());

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
            provider_id: Arc::clone(&self.provider_id),
            source_uri,
            is_favorite,
            rating,
            rg_track_gain,
            rg_album_gain,
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
        let mut albums = Vec::with_capacity(count);

        for album_id3 in &album_list {
            let album_detail = self
                .client
                .get_album(&album_id3.id)
                .await
                .map_err(subsonic_err("getAlbum"))?;

            let mut tracks: Vec<Track> = album_detail
                .song
                .iter()
                .map(|s| self.child_to_track(s))
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
            .map_err(subsonic_err("getAlbumList2"))
    }

    /// Scrobble a track (mark as played) on the Subsonic server.
    ///
    /// The `song_id` is the Subsonic song ID (not the stream URL).
    /// Call this when a track reaches 50% or 4 minutes of playback.
    pub fn scrobble(&self, song_id: &str) {
        let id = song_id.to_string();
        let client = self.client.clone();
        self.runtime.spawn(async move {
            if let Err(e) = client.scrobble(&id, None, Some(true)).await {
                tracing::warn!("Subsonic scrobble failed for '{id}': {e}");
            } else {
                tracing::debug!("Scrobbled song '{id}'");
            }
        });
    }

    /// Send a "now playing" notification to the Subsonic server.
    pub fn now_playing(&self, song_id: &str) {
        let id = song_id.to_string();
        let client = self.client.clone();
        self.runtime.spawn(async move {
            if let Err(e) = client.scrobble(&id, None, Some(false)).await {
                tracing::debug!("Subsonic now-playing failed for '{id}': {e}");
            }
        });
    }
}

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
    fn browse_albums(&self) -> Result<Vec<Album>, ProviderError> {
        self.block_on(async {
            let mut all_albums = Vec::new();
            let mut offset = 0;
            let page_size = 500;

            loop {
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
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn browse_artists(&self) -> Result<Vec<Artist>, ProviderError> {
        self.block_on(async {
            let artists_id3 = self
                .client
                .get_artists(None)
                .await
                .map_err(subsonic_err("getArtists"))?;

            let mut artists = Vec::new();

            for index in &artists_id3.index {
                for artist_id3 in &index.artist {
                    // Get artist details with album list.
                    let artist_detail = self
                        .client
                        .get_artist(&artist_id3.id)
                        .await
                        .map_err(subsonic_err("getArtist"))?;

                    let mut artist_albums = Vec::new();

                    for album_id3 in &artist_detail.album {
                        // Fetch full album (with songs).
                        let album_detail = self
                            .client
                            .get_album(&album_id3.id)
                            .await
                            .map_err(subsonic_err("getAlbum"))?;

                        let mut tracks: Vec<Track> = album_detail
                            .song
                            .iter()
                            .map(|s| self.child_to_track(s))
                            .collect();
                        Track::sort_by_disc_and_track(&mut tracks);

                        let cover_source = self.cover_source_from_id(&album_detail.cover_art);

                        let mut album =
                            Album::from_tracks(album_detail.name.clone(), tracks, cover_source);
                        album.artist = artist_id3.name.clone();
                        album.year = album_detail.year.unwrap_or(0) as u32;
                        album.refresh_derived();
                        artist_albums.push(album);
                    }

                    artist_albums.sort_by_key(|a| a.year);

                    artists.push(Artist::new(artist_id3.name.clone(), artist_albums));
                }
            }

            artists.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(artists)
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn browse_tracks(&self) -> Result<Vec<Track>, ProviderError> {
        self.block_on(async {
            let mut all_tracks = Vec::new();
            let page_size = 500;
            let mut offset = 0;

            loop {
                let album_list = self.fetch_album_list(page_size, offset).await?;

                let count = album_list.len();
                if count == 0 {
                    break;
                }

                for album_id3 in &album_list {
                    let album_detail = self
                        .client
                        .get_album(&album_id3.id)
                        .await
                        .map_err(subsonic_err("getAlbum"))?;

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
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn search(&self, query: &str) -> Result<Vec<Track>, ProviderError> {
        let query_owned = query.to_string();
        self.block_on(async {
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
                .map_err(subsonic_err("search3"))?;

            let tracks: Vec<Track> = results
                .song
                .iter()
                .map(|s| self.child_to_track(s))
                .collect();
            Ok(tracks)
        })
    }

    #[tracing::instrument(skip(self, album), level = "debug")]
    fn get_cover_art(&self, album: &Album) -> Result<Option<Vec<u8>>, ProviderError> {
        // The cover_source should already contain a Url from browse_albums().
        // Fetch the image bytes for display.
        match &album.cover_source {
            Some(CoverSource::Url(url)) => {
                let http = self.http_client.clone();
                self.block_on(async {
                    let resp = http
                        .get(url)
                        .send()
                        .await
                        .map_err(subsonic_err("cover art fetch"))?;
                    let bytes = resp.bytes().await.map_err(subsonic_err("cover art read"))?;
                    Ok(Some(bytes.to_vec()))
                })
            }
            _ => {
                // Try to get cover art from the first track's album ID.
                if let Some(first_track) = album.tracks.first() {
                    self.block_on(async {
                        // The source_uri is the song ID; we can try using it as the cover art ID.
                        match self
                            .client
                            .get_cover_art(&first_track.source_uri, Some(300))
                            .await
                        {
                            Ok(bytes) => Ok(Some(bytes.to_vec())),
                            Err(e) => {
                                tracing::warn!("Subsonic cover art for '{}': {e}", album.name);
                                Ok(None)
                            }
                        }
                    })
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn get_lyrics(&self, track: &Track) -> Result<Option<Lyrics>, ProviderError> {
        // Extract the Subsonic song ID from the stream URL path.
        let song_id = track.path.to_string_lossy();
        self.block_on(async {
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
                    tracing::debug!("getLyricsBySongId not available: {e}");
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
                    tracing::debug!("getLyrics not available: {e}");
                }
            }

            Ok(None)
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn sync_library(&self) -> Result<usize, ProviderError> {
        // Subsonic servers manage their own library scanning.
        // We can trigger a scan via startScan, but the actual library
        // data is fetched on demand via browse_*() methods.
        self.block_on(async {
            match self.client.start_scan().await {
                Ok(_) => tracing::info!("Triggered Subsonic library scan"),
                Err(e) => tracing::warn!("Subsonic startScan: {e}"),
            }
            Ok(0)
        })
    }

    // --- Playlists ---

    #[tracing::instrument(skip(self), level = "debug")]
    fn list_playlists(&self) -> Result<Vec<Playlist>, ProviderError> {
        self.block_on(async {
            let playlists = self
                .client
                .get_playlists(None)
                .await
                .map_err(subsonic_err("getPlaylists"))?;

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
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn get_playlist(&self, id: &str) -> Result<Playlist, ProviderError> {
        let id_owned = id.to_string();
        self.block_on(async {
            let p = self
                .client
                .get_playlist(&id_owned)
                .await
                .map_err(subsonic_err("getPlaylist"))?;

            let tracks: Vec<Track> = p.entry.iter().map(|s| self.child_to_track(s)).collect();
            let total_duration = tracks.iter().map(|t| t.duration).sum();

            Ok(Playlist {
                id: p.id,
                name: p.name,
                track_count: tracks.len() as u32,
                total_duration,
                tracks,
            })
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn create_playlist(&self, name: &str) -> Result<Playlist, ProviderError> {
        let name_owned = name.to_string();
        self.block_on(async {
            let p = self
                .client
                .create_playlist(None, Some(&name_owned), &[])
                .await
                .map_err(subsonic_err("createPlaylist"))?;

            Ok(Playlist {
                id: p.id,
                name: p.name,
                tracks: Vec::new(),
                track_count: 0,
                total_duration: Duration::ZERO,
            })
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn delete_playlist(&self, id: &str) -> Result<(), ProviderError> {
        let id_owned = id.to_string();
        self.block_on(async {
            self.client
                .delete_playlist(&id_owned)
                .await
                .map_err(subsonic_err("deletePlaylist"))
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn rename_playlist(&self, id: &str, new_name: &str) -> Result<(), ProviderError> {
        let id_owned = id.to_string();
        let name_owned = new_name.to_string();
        self.block_on(async {
            self.client
                .update_playlist(
                    &id_owned,
                    &opensubsonic::UpdatePlaylistOptions::new().name(name_owned.as_str()),
                )
                .await
                .map_err(subsonic_err("updatePlaylist"))
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn add_to_playlist(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> Result<(), ProviderError> {
        let pid = playlist_id.to_string();
        let ids: Vec<String> = track_ids.to_vec();
        self.block_on(async {
            self.client
                .update_playlist(
                    &pid,
                    &opensubsonic::UpdatePlaylistOptions::new().add_songs(ids.iter().cloned()),
                )
                .await
                .map_err(subsonic_err("updatePlaylist"))
        })
    }

    // --- Favorites and ratings ---

    #[tracing::instrument(skip(self), level = "debug")]
    fn toggle_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let id = track_id.to_string();
        self.block_on(async {
            // Check if already starred by fetching the song.
            let song = self
                .client
                .get_song(&id)
                .await
                .map_err(subsonic_err("getSong"))?;

            if song.starred.is_some() {
                // Currently starred → unstar.
                self.client
                    .unstar(&[id.as_str()], &[], &[])
                    .await
                    .map_err(subsonic_err("unstar"))?;
                Ok(false)
            } else {
                // Not starred → star.
                self.client
                    .star(&[id.as_str()], &[], &[])
                    .await
                    .map_err(subsonic_err("star"))?;
                Ok(true)
            }
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn is_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        let id = track_id.to_string();
        self.block_on(async {
            let song = self
                .client
                .get_song(&id)
                .await
                .map_err(subsonic_err("getSong"))?;
            Ok(song.starred.is_some())
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn set_rating(&self, track_id: &str, rating: u8) -> Result<(), ProviderError> {
        let id = track_id.to_string();
        self.block_on(async {
            self.client
                .set_rating(&id, rating as i32)
                .await
                .map_err(subsonic_err("setRating"))
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn get_rating(&self, track_id: &str) -> Result<Option<u8>, ProviderError> {
        let id = track_id.to_string();
        self.block_on(async {
            let song = self
                .client
                .get_song(&id)
                .await
                .map_err(subsonic_err("getSong"))?;
            Ok(song
                .user_rating
                .and_then(|r| if r > 0 { Some(r as u8) } else { None }))
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn list_favorites(&self) -> Result<Vec<Track>, ProviderError> {
        self.block_on(async {
            let starred = self
                .client
                .get_starred2(None)
                .await
                .map_err(subsonic_err("getStarred2"))?;

            let tracks: Vec<Track> = starred
                .song
                .iter()
                .map(|s| self.child_to_track(s))
                .collect();
            Ok(tracks)
        })
    }

    // --- Genres ---

    #[tracing::instrument(skip(self), level = "debug")]
    fn list_genres(&self) -> Result<Vec<String>, ProviderError> {
        self.block_on(async {
            let genres = self
                .client
                .get_genres()
                .await
                .map_err(subsonic_err("getGenres"))?;

            let mut names: Vec<String> = genres.into_iter().map(|g| g.name).collect();
            names.sort();
            Ok(names)
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn get_tracks_by_genre(&self, genre: &str) -> Result<Vec<Track>, ProviderError> {
        let genre_owned = genre.to_string();
        self.block_on(async {
            let songs = self
                .client
                .get_songs_by_genre(
                    &genre_owned,
                    &opensubsonic::SongsByGenreOptions::new().count(500),
                )
                .await
                .map_err(subsonic_err("getSongsByGenre"))?;

            let tracks: Vec<Track> = songs.iter().map(|s| self.child_to_track(s)).collect();
            Ok(tracks)
        })
    }

    // --- Artist metadata ---

    /// Serves the server's own artist bio/image (`getArtistInfo2` and the
    /// artist's cover art / `artistImageUrl`) regardless of Aulos's
    /// "fetch artist images and info online" setting — that toggle only
    /// gates Aulos's own Deezer/Wikipedia lookup for Local/MPD, which
    /// would be redundant (and pointlessly slower) next to data the
    /// Subsonic/Navidrome server already curates.
    #[tracing::instrument(skip(self), level = "debug")]
    fn get_artist_info(
        &self,
        name: &str,
    ) -> Result<Option<crate::library::ArtistInfoResult>, ProviderError> {
        self.block_on(async {
            let indexes = self
                .client
                .get_artists(None)
                .await
                .map_err(subsonic_err("getArtists"))?;

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
                .map_err(subsonic_err("getArtist"))?;

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

            let image_bytes = match image_url {
                Some(url) => match self.http_client.get(&url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        resp.bytes().await.ok().map(|b| b.to_vec())
                    }
                    _ => None,
                },
                None => None,
            };

            if bio.is_none() && image_bytes.is_none() {
                return Ok(None);
            }

            Ok(Some(crate::library::ArtistInfoResult { bio, image_bytes }))
        })
    }
}
