// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! MPD server music provider.
//!
//! Connects to an MPD server using the `mpd_client` crate, browses its
//! library, controls remote playback, and receives real-time idle events.

use super::{MusicProvider, ProviderError, ProviderType};
use crate::library::{Album, Artist, CoverSource, Playlist, Track};
use mpd_client::Client;
use mpd_client::client::{ConnectWithPasswordError, ConnectionEvents};
use mpd_client::commands::{
    self, AddToPlaylist, ClearPlaylist, Command, Crossfade, CurrentSong, DeletePlaylist,
    GetPlaylist, GetPlaylists, List, ReplayGainMode, SaveQueueAsPlaylist, SetRandom, SetRepeat,
    SetReplayGainMode, SetSingle, SingleMode, Status, StickerDelete, StickerFind, StickerGet,
    StickerSet, Update,
};
use mpd_client::protocol::command::Command as RawCommand;
use mpd_client::protocol::response::Frame;
use mpd_client::responses;
use mpd_client::tag::Tag;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// Filter-expression construction. `mpd_client::filter::Filter` escaped
/// values incorrectly before elomatreb/mpd_client#27 (`"` broke the command,
/// `\` was dropped); this module predates that fix and is kept because it
/// also feeds [`FilterQuery`] (`search`, which the crate has no typed
/// command for).
///
/// Values get *filter-level* escaping here (`\` -> `\\`, then `"` -> `\"`);
/// `mpd_protocol::command::Command::argument` then applies the normal
/// *argument-level* escaping. See the `filter_expr` tests for the exact wire
/// bytes.
mod filter_expr {
    /// Filter-level escaping for a value embedded in a filter expression.
    /// Order matters: backslashes must be doubled before quotes are
    /// escaped, or an escaped quote's backslash would itself be escaped.
    pub(super) fn escape_value(value: &str) -> String {
        value.replace('\\', "\\\\").replace('"', "\\\"")
    }

    /// `(Tag == "value")`
    pub(super) fn eq(tag: &str, value: &str) -> String {
        format!("({tag} == \"{}\")", escape_value(value))
    }

    /// `(Tag contains "value")`
    pub(super) fn contains(tag: &str, value: &str) -> String {
        format!("({tag} contains \"{}\")", escape_value(value))
    }

    /// `((a) AND (b) AND ...)`. `parts` are expected to already be
    /// parenthesized (e.g. produced by [`eq`] or [`contains`]).
    pub(super) fn and(parts: &[String]) -> String {
        format!("({})", parts.join(" AND "))
    }
}

/// `find <filterexpr>` / `search <filterexpr>` with an expression built by
/// [`filter_expr`]. `search` is the case-insensitive variant; `mpd_client`
/// has no typed command for it.
struct FilterQuery {
    command: &'static str,
    expr: String,
}

impl FilterQuery {
    fn find(expr: String) -> Self {
        Self {
            command: "find",
            expr,
        }
    }

    fn search(expr: String) -> Self {
        Self {
            command: "search",
            expr,
        }
    }
}

impl Command for FilterQuery {
    type Response = Vec<responses::Song>;

    fn command(&self) -> RawCommand {
        RawCommand::new(self.command).argument(self.expr.as_str())
    }

    fn response(self, frame: Frame) -> Result<Self::Response, responses::TypedResponseError> {
        // `responses::Song` is `#[non_exhaustive]` with a crate-private
        // constructor, so it cannot be built outside `mpd_client`. Reuse
        // `ListAllIn`'s public `Command::response` impl instead, which
        // parses a song list frame identically (both call
        // `responses::Song::from_frame_multi` internally) — only its
        // (unused) `command()` differs.
        commands::ListAllIn::root().response(frame)
    }
}

/// `commands`: the commands this connection may run. Used to detect
/// sticker support — a server without a sticker database omits `sticker`
/// here, while `sticker get` itself fails with "sticker database is
/// disabled", not "unknown command".
struct AvailableCommands;

impl Command for AvailableCommands {
    type Response = Vec<String>;

    fn command(&self) -> RawCommand {
        RawCommand::new("commands")
    }

    fn response(self, frame: Frame) -> Result<Self::Response, responses::TypedResponseError> {
        Ok(frame
            .into_iter()
            .filter(|(key, _)| &**key == "command")
            .map(|(_, value)| value)
            .collect())
    }
}

/// `getvol` (MPD 0.23): `Some(volume)`, or `None` when MPD has no mixer.
/// `Status::volume` reports `0` in both "muted" and "no mixer" cases, so
/// this disambiguates a `0`.
pub struct GetVolume;

impl Command for GetVolume {
    type Response = Option<u8>;

    fn command(&self) -> RawCommand {
        RawCommand::new("getvol")
    }

    fn response(self, frame: Frame) -> Result<Self::Response, responses::TypedResponseError> {
        frame
            .into_iter()
            .find(|(key, _)| &**key == "volume")
            .map(|(_, value)| {
                value
                    .parse()
                    .map_err(|_| responses::TypedResponseError::invalid_value("volume", value))
            })
            .transpose()
    }
}

/// `list Album <filterexpr>` sent with a filter expression built by
/// [`filter_expr`] — replacement for `mpd_client::commands::List::filter`
/// + `mpd_client::filter::Filter` (see `filter_expr` module docs).
struct ListAlbumFiltered(String);

impl Command for ListAlbumFiltered {
    type Response = Vec<String>;

    fn command(&self) -> RawCommand {
        RawCommand::new("list")
            .argument("Album")
            .argument(self.0.as_str())
    }

    fn response(self, frame: Frame) -> Result<Self::Response, responses::TypedResponseError> {
        let mut out = Vec::with_capacity(frame.fields_len());
        for (key, value) in frame {
            if &*key != "Album" {
                return Err(responses::TypedResponseError::unexpected_field(
                    "Album",
                    key.as_ref(),
                ));
            }
            out.push(value);
        }
        Ok(out)
    }
}

/// Helper to wrap MPD errors into `ProviderError::Io` with a labeled context.
fn mpd_err<E: std::fmt::Display>(op: &str) -> impl FnOnce(E) -> ProviderError {
    super::wrap_err("MPD", op)
}

/// Configuration for connecting to an MPD server.
#[derive(Debug, Clone)]
pub struct MpdConfig {
    /// Unique provider ID (e.g., "mpd-home").
    pub id: String,
    /// Human-readable name (e.g., "Home MPD Server").
    pub name: String,
    /// MPD server hostname.
    pub host: String,
    /// MPD server port (default: 6600).
    pub port: u16,
    /// Optional password for authentication.
    pub password: Option<String>,
}

impl Default for MpdConfig {
    fn default() -> Self {
        Self {
            id: "mpd".to_string(),
            name: "MPD Server".to_string(),
            host: "localhost".to_string(),
            port: 6600,
            password: None,
        }
    }
}

impl From<crate::config::MpdConfigEntry> for MpdConfig {
    fn from(entry: crate::config::MpdConfigEntry) -> Self {
        // Task 85/86: Retrieve password from keyring when password_in_keyring is set.
        let password = if entry.password_in_keyring {
            match crate::credentials::retrieve_password(&entry.id) {
                Ok(Some(pw)) => Some(pw),
                Ok(None) => {
                    // Task 86: password_in_keyring is true but no keyring entry found.
                    tracing::error!(
                        "MPD provider '{}': password marked as stored in keyring but not found. \
                         Please re-enter the password in provider settings.",
                        entry.id
                    );
                    None
                }
                Err(e) => {
                    tracing::error!(
                        "MPD provider '{}': failed to retrieve password from keyring: {e}",
                        entry.id
                    );
                    None
                }
            }
        } else {
            entry.password
        };

        Self {
            id: entry.id,
            name: entry.name,
            host: entry.host,
            port: entry.port,
            password,
        }
    }
}

/// MPD music provider backed by the `mpd_client` crate.
///
/// Uses **two separate TCP connections** to the same MPD server:
/// - **Idle connection**: owned by the COSMIC subscription, used solely
///   for receiving idle events via `ConnectionEvents`. This connection
///   stays in MPD's "idle" mode permanently.
/// - **Command connection**: stored in `self.client`, used for all
///   browse/search/playback commands. This connection is never in idle
///   mode, so commands execute without conflicting with idle events.
///
/// This dual-connection approach avoids the `invalid message` protocol
/// errors caused by multiplexing idle and command traffic on a single
/// connection.
pub struct MpdProvider {
    config: MpdConfig,
    /// Shared provider ID for zero-copy assignment to tracks.
    provider_id: Arc<str>,
    /// Command connection — used for browse, search, playback, etc.
    client: Arc<Mutex<Option<Client>>>,
    runtime: tokio::runtime::Handle,
    /// Whether MPD stickers are supported (detected on connect).
    stickers_supported: AtomicBool,
}

impl MpdProvider {
    /// Create a new MPD provider. Does NOT connect yet — call `connect_idle()` first.
    pub fn new(config: MpdConfig, runtime: tokio::runtime::Handle) -> Self {
        let provider_id: Arc<str> = Arc::from(config.id.as_str());
        Self {
            config,
            provider_id,
            client: Arc::new(Mutex::new(None)),
            runtime,
            stickers_supported: AtomicBool::new(false),
        }
    }

    /// Helper: open a fresh TCP+MPD connection, returning `(Client, ConnectionEvents)`.
    async fn open_connection(&self) -> Result<(Client, ConnectionEvents), ProviderError> {
        let addr = format!("{}:{}", self.config.host, self.config.port);
        let stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| ProviderError::NotConnected(format!("TCP connect to {addr}: {e}")))?;

        let (client, events) =
            Client::connect_with_password_opt(stream, self.config.password.as_deref())
                .await
                .map_err(|e| match e {
                    ConnectWithPasswordError::IncorrectPassword => {
                        ProviderError::NotConnected("Incorrect MPD password".into())
                    }
                    ConnectWithPasswordError::ProtocolError(pe) => {
                        ProviderError::NotConnected(format!("MPD protocol error: {pe}"))
                    }
                })?;

        tracing::info!(
            "Connected to MPD at {} (protocol {})",
            addr,
            client.protocol_version()
        );

        Ok((client, events))
    }

    /// Establish the **idle connection** and return its event stream.
    ///
    /// This connection is used only by the COSMIC subscription for
    /// receiving idle events. It does NOT store the client in `self.client`
    /// — that is done by `connect_command()`.
    ///
    /// Returns `(Client, ConnectionEvents)` — the caller **must** keep the
    /// `Client` alive for the duration of the idle loop, because dropping it
    /// signals the background task to exit (which closes `ConnectionEvents`).
    pub async fn connect_idle(&self) -> Result<(Client, ConnectionEvents), ProviderError> {
        self.open_connection().await
    }

    /// Establish the **command connection** and store it for browse/search/playback.
    ///
    /// Must be called after `connect_idle()` succeeds. The `ConnectionEvents`
    /// from this connection is intentionally dropped — we don't need idle
    /// events from the command connection.
    ///
    /// Also probes sticker support via `commands`.
    pub async fn connect_command(&self) -> Result<(), ProviderError> {
        let (client, _events) = self.open_connection().await?;

        // Sticker support: `commands` lists `sticker` only when MPD has a
        // sticker database. Older servers without `commands` permission are
        // treated as unsupported.
        let stickers_ok = match client.command(AvailableCommands).await {
            Ok(cmds) => cmds.iter().any(|c| c == "sticker"),
            Err(e) => {
                tracing::warn!("MPD `commands` failed, assuming no stickers: {e}");
                false
            }
        };
        self.stickers_supported
            .store(stickers_ok, Ordering::Relaxed);
        tracing::info!("MPD sticker support: {stickers_ok}");

        let mut guard = self.client.lock().await;
        *guard = Some(client);
        Ok(())
    }

    /// Disconnect the command connection (e.g. before reconnecting).
    pub async fn disconnect(&self) {
        let mut guard = self.client.lock().await;
        *guard = None;
    }

    /// Get a clone of the command client, or error if not connected.
    async fn get_client(&self) -> Result<Client, ProviderError> {
        let guard = self.client.lock().await;
        guard
            .clone()
            .ok_or_else(|| ProviderError::NotConnected("Not connected to MPD".into()))
    }

    /// Get the `Client` for use in the `MpdBackend` (public for player module).
    pub fn client_clone(&self) -> Option<Client> {
        self.runtime.block_on(async {
            let guard = self.client.lock().await;
            guard.clone()
        })
    }

    /// Get a clone of the tokio runtime handle (for MpdBackend creation).
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.clone()
    }

    /// Shared provider id `Arc`, for building `Track`s outside `MpdProvider`
    /// methods — used by the MPD status-poll subscription to convert a
    /// `CurrentSong` response without holding a full `MpdProvider` handle.
    pub(crate) fn provider_id_arc(&self) -> Arc<str> {
        Arc::clone(&self.provider_id)
    }

    /// The configured server hostname (or unix-socket path). Used by the
    /// visualizer's MPD PipeWire capture subscription to decide whether
    /// this server can plausibly be found on the local PipeWire graph at
    /// all — see `player::pw_capture::is_local_mpd_host`.
    #[cfg(feature = "visualizer")]
    pub(crate) fn host(&self) -> &str {
        &self.config.host
    }

    /// Run an async block on the tokio runtime (bridging sync MusicProvider to async mpd_client).
    fn block_on<F, T>(&self, future: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.runtime.block_on(future)
    }

    /// Get the full list of unique album names from MPD.
    ///
    /// This is a single fast command (`list album`). The returned names
    /// can then be chunked and passed to [`browse_albums_batch`] for
    /// incremental loading.
    pub async fn list_album_names(&self) -> Result<Vec<String>, ProviderError> {
        let client = self.get_client().await?;
        let album_list = client
            .command(List::new(Tag::Album))
            .await
            .map_err(mpd_err("list album"))?;

        Ok(album_list
            .into_iter()
            .filter(|name| !name.is_empty())
            .collect())
    }

    /// Fetch full album details (with songs) for a batch of album names.
    ///
    /// Returns a `Vec<Album>` built by issuing one `find album "<name>"`
    /// per album name. This is the building block for incremental library
    /// loading — call with a chunk of 20-50 album names at a time.
    pub async fn browse_albums_batch(
        &self,
        album_names: &[String],
    ) -> Result<Vec<Album>, ProviderError> {
        let client = self.get_client().await?;
        let mut albums = Vec::with_capacity(album_names.len());

        for album_name in album_names {
            let expr = filter_expr::eq("Album", album_name);
            let songs = match client.command(FilterQuery::find(expr)).await {
                Ok(songs) => songs,
                Err(e) => {
                    tracing::warn!("MPD find failed for album {album_name:?}: {e}");
                    continue;
                }
            };

            if songs.is_empty() {
                continue;
            }

            // `find album` matches purely on title: same-titled albums by
            // different artists (e.g. "Greatest Hits") would otherwise be
            // merged into one Album mixing unrelated tracks. Split by album
            // artist first, same disambiguation `browse_artists()` uses.
            let mut by_artist: HashMap<String, Vec<Track>> = HashMap::new();
            for song in &songs {
                let track = song_to_track(&self.provider_id, song);
                by_artist
                    .entry(track.album_artist.clone())
                    .or_default()
                    .push(track);
            }

            for (_, mut tracks) in by_artist {
                Track::sort_by_disc_and_track(&mut tracks);
                let cover_source = tracks
                    .first()
                    .map(|t| CoverSource::MpdAlbumArt(t.source_uri.clone()));
                albums.push(Album::from_tracks(album_name.clone(), tracks, cover_source));
            }
        }

        Ok(albums)
    }

    /// Query MPD status (async).
    pub async fn status_async(&self) -> Result<responses::Status, ProviderError> {
        let client = self.get_client().await?;
        client.command(Status).await.map_err(mpd_err("status"))
    }

    /// Query current song (async).
    pub async fn current_song_async(
        &self,
    ) -> Result<Option<responses::SongInQueue>, ProviderError> {
        let client = self.get_client().await?;
        client
            .command(CurrentSong)
            .await
            .map_err(mpd_err("currentsong"))
    }

    // --- MPD state control methods (Tasks 56-59) ---

    /// Send `random 0/1` to MPD.
    pub fn send_random(&self, enabled: bool) -> Result<(), ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;
            client
                .command(SetRandom(enabled))
                .await
                .map_err(mpd_err("random"))
        })
    }

    /// Send `repeat 0/1` to MPD.
    pub fn send_repeat(&self, enabled: bool) -> Result<(), ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;
            client
                .command(SetRepeat(enabled))
                .await
                .map_err(mpd_err("repeat"))
        })
    }

    /// Send `single 0/1/oneshot` to MPD.
    pub fn send_single(&self, mode: SingleMode) -> Result<(), ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;
            client
                .command(SetSingle(mode))
                .await
                .map_err(mpd_err("single"))
        })
    }

    /// Send `crossfade <seconds>` to MPD.
    pub fn send_crossfade(&self, seconds: u64) -> Result<(), ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;
            client
                .command(Crossfade(Duration::from_secs(seconds)))
                .await
                .map_err(mpd_err("crossfade"))
        })
    }

    /// Send `replay_gain_mode` to MPD.
    pub fn send_replay_gain_mode(&self, mode: ReplayGainMode) -> Result<(), ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;
            client
                .command(SetReplayGainMode(mode))
                .await
                .map_err(mpd_err("replay_gain_mode"))
        })
    }

    /// Read MPD status (sync). Returns the parsed status for reading
    /// `random`, `repeat`, `single`, `crossfade`, etc.
    pub fn status(&self) -> Result<responses::Status, ProviderError> {
        self.block_on(self.status_async())
    }

    /// Whether MPD sticker support was detected on connect.
    fn stickers_supported(&self) -> bool {
        self.stickers_supported.load(Ordering::Relaxed)
    }

    /// Error to return from favorite/rating mutations when stickers aren't supported.
    fn require_stickers(&self) -> Result<(), ProviderError> {
        if self.stickers_supported() {
            Ok(())
        } else {
            Err(ProviderError::NotSupported(
                "MPD stickers not enabled".into(),
            ))
        }
    }
}

/// Convert an `mpd_client` `Song` to our `Track` model.
///
/// A free function (not an `MpdProvider` method) so the MPD status-poll
/// subscription (`app::subscriptions::mpd_status_stream`) can build a
/// `Track` from a `CurrentSong` response using only the active provider's
/// id, without needing a full `MpdProvider` handle.
pub(crate) fn song_to_track(provider_id: &Arc<str>, song: &responses::Song) -> Track {
    let title = song.title().unwrap_or("Unknown Title").to_string();
    let artist = song
        .artists()
        .first()
        .cloned()
        .unwrap_or_else(|| "Unknown Artist".to_string());
    let album_artist = song
        .album_artists()
        .first()
        .cloned()
        .unwrap_or_else(|| artist.clone());
    let album = song.album().unwrap_or("Unknown Album").to_string();
    let genre = song
        .tags
        .get(&Tag::Genre)
        .and_then(|v| v.first())
        .cloned()
        .unwrap_or_default();
    let (disc_num, track_num) = song.number();
    let year = song
        .tags
        .get(&Tag::Date)
        .and_then(|v| v.first())
        .and_then(|s| s.get(..4))
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let duration = song.duration.unwrap_or(Duration::ZERO);
    let uri = song.url.clone();

    Track {
        id: 0,
        path: PathBuf::from(&uri),
        title,
        artist,
        album_artist,
        album,
        genre,
        track_number: track_num as u32,
        disc_number: disc_num as u32,
        year,
        duration,
        bitrate: 0,
        sample_rate: 0,
        provider_id: Arc::clone(provider_id),
        source_uri: uri,
        is_favorite: false,
        rating: None,
        rg_track_gain: None,
        rg_album_gain: None,
    }
}

impl MusicProvider for MpdProvider {
    fn id(&self) -> &str {
        &self.config.id
    }

    fn name(&self) -> &str {
        &self.config.name
    }

    fn provider_type(&self) -> ProviderType {
        ProviderType::Mpd
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn browse_albums(&self) -> Result<Vec<Album>, ProviderError> {
        self.block_on(async {
            let names = self.list_album_names().await?;
            let mut albums = self.browse_albums_batch(&names).await?;
            albums.sort_by(|a, b| a.name.cmp(&b.name).then(a.artist.cmp(&b.artist)));
            Ok(albums)
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn browse_artists(&self) -> Result<Vec<Artist>, ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;

            let artist_list = client
                .command(List::new(Tag::AlbumArtist))
                .await
                .map_err(mpd_err("list albumartist"))?;

            let artist_names: Vec<String> = artist_list.into_iter().collect();
            let mut artists = Vec::new();

            for artist_name in &artist_names {
                if artist_name.is_empty() {
                    continue;
                }

                // Get albums for this artist
                let expr = filter_expr::eq("AlbumArtist", artist_name);
                let album_list = client
                    .command(ListAlbumFiltered(expr))
                    .await
                    .map_err(mpd_err("list album"))?;

                let album_names: Vec<String> = album_list.into_iter().collect();
                let mut artist_albums = Vec::new();

                for album_name in &album_names {
                    if album_name.is_empty() {
                        continue;
                    }

                    let expr = filter_expr::and(&[
                        filter_expr::eq("Album", album_name),
                        filter_expr::eq("AlbumArtist", artist_name),
                    ]);
                    let songs = client
                        .command(FilterQuery::find(expr))
                        .await
                        .map_err(mpd_err("find"))?;

                    let mut tracks: Vec<Track> = songs
                        .iter()
                        .map(|s| song_to_track(&self.provider_id, s))
                        .collect();
                    Track::sort_by_disc_and_track(&mut tracks);

                    let cover_source = tracks
                        .first()
                        .map(|t| CoverSource::MpdAlbumArt(t.source_uri.clone()));

                    let mut album = Album::from_tracks(album_name.clone(), tracks, cover_source);
                    album.artist = artist_name.clone();
                    album.refresh_derived();
                    artist_albums.push(album);
                }

                artist_albums.sort_by_key(|a| a.year);

                artists.push(Artist::new(artist_name.clone(), artist_albums));
            }

            artists.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(artists)
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn browse_tracks(&self) -> Result<Vec<Track>, ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;

            // Use listallinfo to get all songs from the root
            let songs = client
                .command(commands::ListAllIn::root())
                .await
                .map_err(mpd_err("listallinfo"))?;

            let mut tracks: Vec<Track> = songs
                .iter()
                .map(|s| song_to_track(&self.provider_id, s))
                .collect();
            tracks.sort_by(|a, b| a.title.cmp(&b.title));
            Ok(tracks)
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn search(&self, query: &str) -> Result<Vec<Track>, ProviderError> {
        let query_owned = query.to_string();
        self.block_on(async {
            let client = self.get_client().await?;

            // `any contains` searches every tag; `search` (unlike `find`)
            // matches case-insensitively.
            let expr = filter_expr::contains("any", &query_owned);
            let songs = client
                .command(FilterQuery::search(expr))
                .await
                .map_err(mpd_err("search"))?;

            let tracks: Vec<Track> = songs
                .iter()
                .map(|s| song_to_track(&self.provider_id, s))
                .collect();
            Ok(tracks)
        })
    }

    #[tracing::instrument(skip(self, album), level = "debug")]
    fn get_cover_art(&self, album: &Album) -> Result<Option<Vec<u8>>, ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;

            let uri = match album.tracks.first() {
                Some(t) => &t.source_uri,
                None => return Ok(None),
            };

            match client.album_art(uri).await {
                Ok(Some((data, _mime))) => Ok(Some(data.to_vec())),
                Ok(None) => Ok(None),
                Err(e) => {
                    tracing::warn!("Failed to get MPD album art for {uri}: {e}");
                    Ok(None)
                }
            }
        })
    }

    fn get_lyrics(&self, _track: &Track) -> Result<Option<crate::library::Lyrics>, ProviderError> {
        // MPD doesn't serve lyrics; the app falls back to LRCLIB.
        Ok(None)
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn sync_library(&self) -> Result<usize, ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;
            client
                .command(Update::new())
                .await
                .map_err(mpd_err("update"))?;
            Ok(0)
        })
    }

    // --- Playlist methods (Tasks 47-51) ---

    #[tracing::instrument(skip(self), level = "debug")]
    fn list_playlists(&self) -> Result<Vec<Playlist>, ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;
            let playlists = client
                .command(GetPlaylists)
                .await
                .map_err(mpd_err("listplaylists"))?;

            Ok(playlists
                .into_iter()
                .map(|p| Playlist {
                    id: p.name.clone(),
                    name: p.name,
                    tracks: Vec::new(),
                    track_count: 0,
                    total_duration: Duration::ZERO,
                })
                .collect())
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn get_playlist(&self, id: &str) -> Result<Playlist, ProviderError> {
        let id_owned = id.to_string();
        self.block_on(async {
            let client = self.get_client().await?;
            let songs = client
                .command(GetPlaylist(&id_owned))
                .await
                .map_err(mpd_err("listplaylistinfo"))?;

            let tracks: Vec<Track> = songs
                .iter()
                .map(|s| song_to_track(&self.provider_id, s))
                .collect();
            let total_duration = tracks.iter().map(|t| t.duration).sum();
            let track_count = tracks.len() as u32;

            Ok(Playlist {
                id: id_owned.clone(),
                name: id_owned,
                tracks,
                track_count,
                total_duration,
            })
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn create_playlist(&self, name: &str) -> Result<Playlist, ProviderError> {
        let name_owned = name.to_string();
        self.block_on(async {
            let client = self.get_client().await?;
            // MPD's `playlistclear` only clears an *existing* stored
            // playlist — it does not reliably create one. `save` creates
            // the playlist (as a snapshot of the live queue, which is left
            // untouched), then `playlistclear` wipes that snapshot so the
            // stored playlist ends up genuinely empty regardless of what
            // was in the queue. If the clear fails, best-effort delete the
            // playlist `save` just created so we don't leave a non-empty
            // one behind.
            client
                .command(SaveQueueAsPlaylist(&name_owned))
                .await
                .map_err(mpd_err("save"))?;

            if let Err(e) = client.command(ClearPlaylist(&name_owned)).await {
                let _ = client.command(DeletePlaylist(&name_owned)).await;
                return Err(mpd_err("playlistclear")(e));
            }

            Ok(Playlist {
                id: name_owned.clone(),
                name: name_owned,
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
            let client = self.get_client().await?;
            client
                .command(DeletePlaylist(&id_owned))
                .await
                .map_err(mpd_err("rm"))
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn rename_playlist(&self, id: &str, new_name: &str) -> Result<(), ProviderError> {
        let id_owned = id.to_string();
        let new_name_owned = new_name.to_string();
        self.block_on(async {
            let client = self.get_client().await?;
            client
                .command(commands::RenamePlaylist::new(&id_owned, &new_name_owned))
                .await
                .map_err(mpd_err("rename"))
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn add_to_playlist(
        &self,
        playlist_id: &str,
        track_ids: &[String],
    ) -> Result<(), ProviderError> {
        let playlist_owned = playlist_id.to_string();
        let uris: Vec<String> = track_ids.to_vec();
        self.block_on(async {
            let client = self.get_client().await?;
            for uri in &uris {
                client
                    .command(AddToPlaylist::new(&playlist_owned, uri))
                    .await
                    .map_err(mpd_err("playlistadd"))?;
            }
            Ok(())
        })
    }

    // --- Favorites and ratings via stickers (Tasks 52-53) ---

    fn toggle_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        self.require_stickers()?;
        let uri = track_id.to_string();
        self.block_on(async {
            let client = self.get_client().await?;
            // Check current favorite state
            let is_fav = match client.command(StickerGet::new(&uri, "favorite")).await {
                Ok(s) => s.value == "1",
                Err(_) => false,
            };

            if is_fav {
                // Remove favorite sticker
                let _ = client.command(StickerDelete::new(&uri, "favorite")).await;
                Ok(false)
            } else {
                client
                    .command(StickerSet::new(&uri, "favorite", "1"))
                    .await
                    .map_err(mpd_err("sticker set favorite"))?;
                Ok(true)
            }
        })
    }

    fn is_favorite(&self, track_id: &str) -> Result<bool, ProviderError> {
        if !self.stickers_supported() {
            return Ok(false);
        }
        let uri = track_id.to_string();
        self.block_on(async {
            let client = self.get_client().await?;
            match client.command(StickerGet::new(&uri, "favorite")).await {
                Ok(s) => Ok(s.value == "1"),
                Err(_) => Ok(false),
            }
        })
    }

    fn set_rating(&self, track_id: &str, rating: u8) -> Result<(), ProviderError> {
        self.require_stickers()?;
        let uri = track_id.to_string();
        self.block_on(async {
            let client = self.get_client().await?;
            if rating == 0 {
                // Clear rating
                let _ = client.command(StickerDelete::new(&uri, "rating")).await;
                Ok(())
            } else {
                let val = rating.min(5).to_string();
                client
                    .command(StickerSet::new(&uri, "rating", &val))
                    .await
                    .map_err(mpd_err("sticker set rating"))
            }
        })
    }

    fn get_rating(&self, track_id: &str) -> Result<Option<u8>, ProviderError> {
        if !self.stickers_supported() {
            return Ok(None);
        }
        let uri = track_id.to_string();
        self.block_on(async {
            let client = self.get_client().await?;
            match client.command(StickerGet::new(&uri, "rating")).await {
                Ok(s) => Ok(s.value.parse::<u8>().ok()),
                Err(_) => Ok(None),
            }
        })
    }

    fn list_favorites(&self) -> Result<Vec<Track>, ProviderError> {
        self.require_stickers()?;
        self.block_on(async {
            let client = self.get_client().await?;
            // Find all songs with favorite=1 sticker
            let results = client
                .command(StickerFind::new("", "favorite").where_eq("1"))
                .await
                .map_err(mpd_err("sticker find favorite"))?;

            let mut uris: Vec<&String> = results.value.keys().collect();
            uris.sort();
            let mut tracks = Vec::with_capacity(uris.len());
            for uri in uris {
                // Look up each song's metadata
                let expr = filter_expr::eq("file", uri.as_str());
                if let Ok(songs) = client.command(FilterQuery::find(expr)).await {
                    for song in &songs {
                        let mut track = song_to_track(&self.provider_id, song);
                        track.is_favorite = true;
                        tracks.push(track);
                    }
                }
            }
            Ok(tracks)
        })
    }

    // --- Genre methods (Tasks 54-55) ---

    #[tracing::instrument(skip(self), level = "debug")]
    fn list_genres(&self) -> Result<Vec<String>, ProviderError> {
        self.block_on(async {
            let client = self.get_client().await?;
            let genre_list = client
                .command(List::new(Tag::Genre))
                .await
                .map_err(mpd_err("list genre"))?;

            Ok(genre_list.into_iter().filter(|g| !g.is_empty()).collect())
        })
    }

    #[tracing::instrument(skip(self), level = "debug")]
    fn get_tracks_by_genre(&self, genre: &str) -> Result<Vec<Track>, ProviderError> {
        let genre_owned = genre.to_string();
        self.block_on(async {
            let client = self.get_client().await?;
            let expr = filter_expr::eq("Genre", &genre_owned);
            let songs = client
                .command(FilterQuery::find(expr))
                .await
                .map_err(mpd_err("find genre"))?;

            let tracks: Vec<Track> = songs
                .iter()
                .map(|s| song_to_track(&self.provider_id, s))
                .collect();
            Ok(tracks)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex as PlMutex;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// Minimal fake MPD server tracking stored playlists and a pre-seeded
    /// live queue. Every command line received is forwarded on `tx` so the
    /// test can assert exactly what was sent to the server. `playlistclear`
    /// on a playlist that doesn't exist yet is rejected with an `ACK`, just
    /// like real MPD — it only clears an *existing* stored playlist.
    fn spawn_fake_mpd(
        queue: Vec<String>,
    ) -> (u16, mpsc::Receiver<String>, Arc<PlMutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake MPD listener");
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        let live_queue = Arc::new(PlMutex::new(queue));
        let live_queue_clone = Arc::clone(&live_queue);

        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept fake MPD connection");
            let mut writer = stream.try_clone().expect("clone fake MPD stream");
            let mut reader = BufReader::new(stream);
            writer.write_all(b"OK MPD 0.23.5\n").unwrap();

            let mut playlists: HashMap<String, Vec<String>> = HashMap::new();
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let cmd = line.trim_end().to_string();
                if cmd.is_empty() {
                    continue;
                }
                let _ = tx.send(cmd.clone());

                if cmd == "idle" {
                    // `idle` blocks until `noidle` cancels it; defer the
                    // single combined response to that point.
                    continue;
                } else if cmd == "noidle" {
                    writer.write_all(b"OK\n").unwrap();
                } else if let Some(name) = cmd.strip_prefix("playlistclear ") {
                    if playlists.contains_key(name) {
                        playlists.insert(name.to_string(), Vec::new());
                        writer.write_all(b"OK\n").unwrap();
                    } else {
                        writer
                            .write_all(b"ACK [50@0] {playlistclear} No such playlist\n")
                            .unwrap();
                    }
                } else if let Some(name) = cmd.strip_prefix("save ") {
                    // `save` snapshots the live queue into a new stored
                    // playlist; the live queue itself is left untouched.
                    playlists.insert(name.to_string(), live_queue_clone.lock().clone());
                    writer.write_all(b"OK\n").unwrap();
                } else if let Some(name) = cmd.strip_prefix("rm ") {
                    playlists.remove(name);
                    writer.write_all(b"OK\n").unwrap();
                } else if let Some(name) = cmd.strip_prefix("listplaylistinfo ") {
                    for uri in playlists.get(name).cloned().unwrap_or_default() {
                        writer
                            .write_all(format!("file: {uri}\n").as_bytes())
                            .unwrap();
                    }
                    writer.write_all(b"OK\n").unwrap();
                } else if cmd.starts_with("sticker get") {
                    writer
                        .write_all(b"ACK [5@0] {} unknown command \"sticker\"\n")
                        .unwrap();
                } else {
                    writer.write_all(b"OK\n").unwrap();
                }
            }
        });

        (port, rx, live_queue)
    }

    #[test]
    fn create_playlist_does_not_copy_a_non_empty_live_queue() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let seed_queue = vec!["track-a.mp3".to_string(), "track-b.mp3".to_string()];
        let (port, commands, live_queue) = spawn_fake_mpd(seed_queue.clone());

        let provider = MpdProvider::new(
            MpdConfig {
                id: "mpd-test".into(),
                name: "Test".into(),
                host: "127.0.0.1".into(),
                port,
                password: None,
            },
            runtime.handle().clone(),
        );

        runtime
            .block_on(provider.connect_command())
            .expect("connect to fake MPD");

        let playlist = provider
            .create_playlist("new-playlist")
            .expect("create_playlist should succeed");
        assert_eq!(playlist.track_count, 0);
        assert!(playlist.tracks.is_empty());

        // The server requires `save` (create) before `playlistclear` (wipe)
        // will succeed on a brand-new playlist, so seeing this succeed at
        // all proves both commands were sent in that order.
        let mut saw_save = false;
        let mut saw_playlistclear = false;
        while let Ok(cmd) = commands.try_recv() {
            if cmd == "save new-playlist" {
                saw_save = true;
            }
            if cmd == "playlistclear new-playlist" {
                assert!(saw_save, "playlistclear must be sent after save: {cmd}");
                saw_playlistclear = true;
            }
        }
        assert!(saw_save, "expected a save command");
        assert!(saw_playlistclear, "expected a playlistclear command");

        // The stored playlist ends up genuinely empty...
        let fetched = provider
            .get_playlist("new-playlist")
            .expect("get_playlist should succeed");
        assert!(fetched.tracks.is_empty());
        assert_eq!(fetched.track_count, 0);

        // ...and the live queue was never touched.
        assert_eq!(*live_queue.lock(), seed_queue);
    }

    #[test]
    fn escape_value_leaves_plain_values_untouched() {
        assert_eq!(filter_expr::escape_value("Clic"), "Clic");
    }

    #[test]
    fn escape_value_escapes_embedded_quotes() {
        // Filter-level escaping only: `"` -> `\"`. The upstream bug this
        // replaces instead emitted `\\"` (backslash, backslash, quote) for
        // this exact input, which MPD 0.24 rejects.
        assert_eq!(filter_expr::escape_value("\"Clic\""), "\\\"Clic\\\"");
    }

    #[test]
    fn escape_value_doubles_embedded_backslashes() {
        assert_eq!(filter_expr::escape_value("back\\slash"), "back\\\\slash");
    }

    #[test]
    fn filter_expr_eq_renders_expected_text() {
        assert_eq!(filter_expr::eq("Album", "Clic"), "(Album == \"Clic\")");
        assert_eq!(
            filter_expr::eq("Album", "\"Clic\""),
            "(Album == \"\\\"Clic\\\"\")"
        );
    }

    #[test]
    fn filter_expr_contains_renders_expected_text() {
        assert_eq!(
            filter_expr::contains("any", "foo bar"),
            "(any contains \"foo bar\")"
        );
    }

    #[test]
    fn filter_expr_and_renders_expected_text() {
        let expr = filter_expr::and(&[
            filter_expr::eq("Album", "A"),
            filter_expr::eq("AlbumArtist", "B"),
        ]);
        assert_eq!(expr, "((Album == \"A\") AND (AlbumArtist == \"B\"))");
    }

    #[test]
    fn filter_query_commands_match_raw_commands_built_the_same_way() {
        // `Command::command` must hand the whole expression to
        // `RawCommand::argument` as a single plain argument (letting the
        // protocol layer's own argument escaping apply) rather than
        // re-escaping it or embedding it unescaped.
        let expr = filter_expr::eq("Album", "\"Clic\"");
        let find = FilterQuery::find(expr.clone()).command();
        assert_eq!(find, RawCommand::new("find").argument(expr.as_str()));
        let search = FilterQuery::search(expr.clone()).command();
        assert_eq!(search, RawCommand::new("search").argument(expr.as_str()));
    }

    #[test]
    fn list_album_filtered_command_sends_tag_then_filter_expression() {
        let expr = filter_expr::eq("AlbumArtist", "Foo");
        let actual = ListAlbumFiltered(expr.clone()).command();
        let expected = RawCommand::new("list")
            .argument("Album")
            .argument(expr.as_str());
        assert_eq!(actual, expected);
    }

    /// Regression test for the exact bug this replaces: a literal `"` in
    /// an album name (e.g. the real album titled `"Clic"`) previously
    /// produced wire bytes MPD 0.24 rejected with `ACK [5@0] {} Space
    /// expected after closing '"'`. This asserts the *exact* bytes sent on
    /// the wire, captured from a real TCP connection to a fake MPD server,
    /// match the form verified live against MPD 0.24 to return the
    /// album's songs successfully.
    #[test]
    fn find_expr_sends_the_mpd_accepted_wire_form_for_a_quoted_album_name() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (port, commands, _live_queue) = spawn_fake_mpd(Vec::new());

        let provider = MpdProvider::new(
            MpdConfig {
                id: "mpd-test".into(),
                name: "Test".into(),
                host: "127.0.0.1".into(),
                port,
                password: None,
            },
            runtime.handle().clone(),
        );

        runtime
            .block_on(provider.connect_command())
            .expect("connect to fake MPD");

        // The fake server ACKs the unrelated sticker probe and replies
        // "OK" with no songs to everything else, so this returns an empty
        // batch — only the wire bytes sent for the `find` command matter.
        let _ = runtime.block_on(provider.browse_albums_batch(&["\"Clic\"".to_string()]));

        let find_cmd = std::iter::from_fn(|| commands.try_recv().ok())
            .find(|cmd| cmd.starts_with("find "))
            .expect("expected a find command to have been sent");

        assert_eq!(find_cmd, "find \"(Album == \\\"\\\\\\\"Clic\\\\\\\"\\\")\"");
    }
}
