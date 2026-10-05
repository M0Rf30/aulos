// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! MPRIS2 D-Bus media player interface.
//!
//! Exposes `org.mpris.MediaPlayer2` and `org.mpris.MediaPlayer2.Player` on the
//! session bus (via the `mpris-server` crate) so Lyra responds to desktop
//! media keys and shows up in the COSMIC/GNOME Shell media applet.
//!
//! This module owns nothing about playback itself: [`mpris_stream`] spawns
//! the D-Bus server and yields [`MprisEvent`]s into `app.rs`'s subscription
//! system exactly like the existing `mpd_idle_stream`/`playback_tick_stream`
//! streams. The D-Bus interface implementation only ever reads a shared,
//! last-published [`MprisSnapshot`] and forwards every method call as an
//! [`MprisCommand`] back to `Message::Mpris`, where `app.rs` maps it onto
//! the real playback messages/helpers it already has.
//!
//! API surface verified against the vendored `mpris-server-0.10.0` sources
//! (`~/.cargo/registry/src/*/mpris-server-0.10.0/src/`): `RootInterface` and
//! `PlayerInterface` are defined by the `define_iface!` macro in `lib.rs`
//! (lines 1136-1142) with `#[trait_variant::make(Send + Sync)]`, making them
//! plain `async fn` traits usable from a `tokio`-backed, multi-threaded
//! executor. `Server::new(bus_name_suffix, imp)` is defined in `server.rs`
//! (`impl<T> Server<T> where T: PlayerInterface + 'static`, around line 399)
//! and only requires `T: PlayerInterface` (which itself requires
//! `RootInterface`); it does not require `TrackListInterface` or
//! `PlaylistsInterface`, both of which Lyra does not implement.
//! `Server::properties_changed`/`Server::emit(Signal::Seeked { .. })` are
//! defined in the same file around lines 438-504.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use futures_util::{SinkExt, Stream};
use mpris_server::zbus::fdo;
use mpris_server::{
    LoopStatus as MprisLoopStatus, Metadata, PlaybackRate, PlaybackStatus, PlayerInterface,
    RootInterface, Server, Signal, Time, TrackId, Volume, zbus::Result as ZbusResult,
};
use parking_lot::Mutex;

/// A command originating from an MPRIS client (media keys, shell applet,
/// `playerctl`, …), forwarded to `app.rs` for handling.
#[derive(Debug, Clone, PartialEq)]
pub enum MprisCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Relative seek, in microseconds (negative seeks backward).
    Seek(i64),
    /// Absolute seek target, in microseconds.
    SetPosition(i64),
    SetVolume(f64),
    Shuffle(bool),
    Loop(LoopMode),
    Raise,
    Quit,
    /// `OpenUri` request from an MPRIS client. Only `file://` URIs are
    /// actually playable; see `Message::OpenFiles`/`file_uri_to_path` in
    /// `app.rs`/`lib.rs`.
    OpenUri(String),
}

/// Repeat/loop mode, mirroring `mpris_server::LoopStatus` without leaking
/// that crate's type into `app.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopMode {
    #[default]
    None,
    Track,
    Playlist,
}

/// Coarse playback state, mirroring `mpris_server::PlaybackStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MprisStatus {
    #[default]
    Stopped,
    Playing,
    Paused,
}

/// Everything the D-Bus side needs to answer property getters. Rebuilt by
/// `AppModel::publish_mpris` on every relevant state change (and, cheaply,
/// on every playback tick — [`MprisHandle::publish`] diffs internally).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MprisSnapshot {
    pub status: MprisStatus,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub track_id: i64,
    pub length_us: i64,
    pub position_us: i64,
    pub art_url: Option<String>,
    pub volume: f64,
    pub shuffle: bool,
    pub loop_mode: LoopMode,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_seek: bool,
    pub can_play: bool,
}

/// Item yielded by [`mpris_stream`]: first a one-shot `Ready` carrying the
/// handle the app should hold onto, then a `Command` per incoming D-Bus
/// method call.
#[derive(Debug, Clone)]
pub enum MprisEvent {
    Ready(MprisHandle),
    Command(MprisCommand),
}

/// A pending set of D-Bus signals to emit, computed synchronously by
/// [`MprisHandle::publish`] and handed off to the server task.
struct Update {
    properties: Vec<mpris_server::Property>,
    seeked: Option<Time>,
}

/// State shared between the app-facing [`MprisHandle`] and the D-Bus-facing
/// [`DbusPlayer`] implementation.
struct SharedState {
    snapshot: Mutex<MprisSnapshot>,
    updates: cosmic::iced::futures::channel::mpsc::UnboundedSender<Update>,
    /// Per-track `file://` cover art URL cache. Lyra keeps cover art as
    /// BLOBs in its library database rather than as loose files on disk, but
    /// MPRIS clients need an actual file to read pixels from — see
    /// [`MprisHandle::cached_art_url`]/[`MprisHandle::cache_art_url`].
    art_cache: Mutex<HashMap<i64, Option<String>>>,
}

/// Handle held by `AppModel` to push fresh playback state into the running
/// MPRIS D-Bus server.
#[derive(Clone)]
pub struct MprisHandle {
    shared: Arc<SharedState>,
}

impl std::fmt::Debug for MprisHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MprisHandle").finish()
    }
}

impl MprisHandle {
    /// Stores `snapshot` and, only if it actually differs from the
    /// previously published one, emits the corresponding
    /// `PropertiesChanged`/`Seeked` D-Bus signals. Cheap no-op when nothing
    /// changed, so callers may call this unconditionally on every UI tick.
    pub fn publish(&self, snapshot: MprisSnapshot) {
        use mpris_server::Property;

        let previous = {
            let mut guard = self.shared.snapshot.lock();
            if *guard == snapshot {
                return;
            }
            std::mem::replace(&mut *guard, snapshot.clone())
        };

        let mut properties = Vec::new();
        if previous.status != snapshot.status {
            properties.push(Property::PlaybackStatus(map_status(snapshot.status)));
        }
        if previous.shuffle != snapshot.shuffle {
            properties.push(Property::Shuffle(snapshot.shuffle));
        }
        if previous.loop_mode != snapshot.loop_mode {
            properties.push(Property::LoopStatus(map_loop(snapshot.loop_mode)));
        }
        if previous.volume != snapshot.volume {
            properties.push(Property::Volume(snapshot.volume));
        }
        if previous.can_go_next != snapshot.can_go_next {
            properties.push(Property::CanGoNext(snapshot.can_go_next));
        }
        if previous.can_go_previous != snapshot.can_go_previous {
            properties.push(Property::CanGoPrevious(snapshot.can_go_previous));
        }
        if previous.can_seek != snapshot.can_seek {
            properties.push(Property::CanSeek(snapshot.can_seek));
        }
        if previous.can_play != snapshot.can_play {
            properties.push(Property::CanPlay(snapshot.can_play));
            properties.push(Property::CanPause(snapshot.can_play));
        }
        let metadata_changed = previous.track_id != snapshot.track_id
            || previous.title != snapshot.title
            || previous.artist != snapshot.artist
            || previous.album != snapshot.album
            || previous.album_artist != snapshot.album_artist
            || previous.genre != snapshot.genre
            || previous.length_us != snapshot.length_us
            || previous.art_url != snapshot.art_url;
        if metadata_changed {
            properties.push(Property::Metadata(build_metadata(&snapshot)));
        }

        let seeked = seek_signal(&previous, &snapshot);
        if properties.is_empty() && seeked.is_none() {
            return;
        }

        // The receiving end lives inside the D-Bus server task; if the
        // session bus never came up (or the app is shutting down) this
        // send is simply dropped.
        let _ = self
            .shared
            .updates
            .unbounded_send(Update { properties, seeked });
    }

    /// Returns the cached cover-art URL for `track_id`, if resolution has
    /// already completed. `Some(None)` means resolution finished but the
    /// track has no embedded art; `None` means it hasn't been resolved yet,
    /// and the caller should resolve it off-thread (see [`extract_art_url`])
    /// and record the outcome via [`Self::cache_art_url`].
    pub fn cached_art_url(&self, track_id: i64) -> Option<Option<String>> {
        self.shared.art_cache.lock().get(&track_id).cloned()
    }

    /// Records a resolved cover-art URL for `track_id`, populating the
    /// cache read by [`Self::cached_art_url`].
    pub fn cache_art_url(&self, track_id: i64, url: Option<String>) {
        self.shared.art_cache.lock().insert(track_id, url);
    }
}

/// Extracts embedded cover art for `path` and writes it to a small on-disk
/// cache under the user's cache directory, returning a `file://` URL to it.
/// This is the only disk-writing cover art cache in Lyra (the library's own
/// cache stores album art as database BLOBs, which D-Bus clients can't read
/// directly) and is scoped entirely to MPRIS's needs.
pub(crate) fn extract_art_url(track_id: i64, path: &Path) -> Option<String> {
    let bytes = crate::library::CoverArt::extract_from_file(path)?;
    let ext = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else {
        "jpg"
    };
    let dir = dirs::cache_dir()?.join("lyra").join("mpris");
    std::fs::create_dir_all(&dir).ok()?;
    let file_path = dir.join(format!("{track_id}.{ext}"));
    if !file_path.exists() {
        std::fs::write(&file_path, &bytes).ok()?;
    }
    let encoded_path = file_path
        .to_string_lossy()
        .split('/')
        .map(urlencoding::encode)
        .collect::<Vec<_>>()
        .join("/");
    Some(format!("file://{encoded_path}"))
}

/// Decides whether a `Seeked` signal is warranted for the transition from
/// `previous` to `next`. The MPRIS spec leaves the exact trigger condition
/// to implementations ("the track position has changed in a way that is
/// inconsistent with the current playing state"); we treat any position
/// jump beyond normal tick cadence while playing, or any position change at
/// all while paused/stopped, as an explicit seek.
fn seek_signal(previous: &MprisSnapshot, next: &MprisSnapshot) -> Option<Time> {
    if previous.track_id != next.track_id {
        // A new track starting somewhere other than the beginning (e.g.
        // resuming a podcast episode) is worth announcing explicitly.
        return (next.position_us > 0).then(|| Time::from_micros(next.position_us));
    }

    let delta = next.position_us - previous.position_us;
    if previous.status == MprisStatus::Playing && next.status == MprisStatus::Playing {
        // Generous slack for tick cadence (300-500ms) and rounding.
        const TOLERANCE_US: i64 = 1_500_000;
        if !(0..=TOLERANCE_US).contains(&delta) {
            return Some(Time::from_micros(next.position_us));
        }
        return None;
    }

    (delta != 0).then(|| Time::from_micros(next.position_us))
}

fn map_status(status: MprisStatus) -> PlaybackStatus {
    match status {
        MprisStatus::Playing => PlaybackStatus::Playing,
        MprisStatus::Paused => PlaybackStatus::Paused,
        MprisStatus::Stopped => PlaybackStatus::Stopped,
    }
}

fn map_loop(mode: LoopMode) -> MprisLoopStatus {
    match mode {
        LoopMode::None => MprisLoopStatus::None,
        LoopMode::Track => MprisLoopStatus::Track,
        LoopMode::Playlist => MprisLoopStatus::Playlist,
    }
}

fn map_loop_from_mpris(status: MprisLoopStatus) -> LoopMode {
    match status {
        MprisLoopStatus::None => LoopMode::None,
        MprisLoopStatus::Track => LoopMode::Track,
        MprisLoopStatus::Playlist => LoopMode::Playlist,
    }
}

/// Builds a D-Bus object path identifying a track. Falls back to
/// `TrackId::NO_TRACK` for the "nothing playing" (id `<= 0`) case.
fn track_id_for(id: i64) -> TrackId {
    if id <= 0 {
        return TrackId::NO_TRACK;
    }
    TrackId::try_from(format!("/io/github/m0rf30/Lyra/Track/{id}")).unwrap_or(TrackId::NO_TRACK)
}

fn build_metadata(snapshot: &MprisSnapshot) -> Metadata {
    let mut builder = Metadata::builder()
        .trackid(track_id_for(snapshot.track_id))
        .length(Time::from_micros(snapshot.length_us))
        .title(snapshot.title.clone())
        .album(snapshot.album.clone());

    if !snapshot.artist.is_empty() {
        builder = builder.artist([snapshot.artist.clone()]);
    }
    if !snapshot.album_artist.is_empty() {
        builder = builder.album_artist([snapshot.album_artist.clone()]);
    }
    if !snapshot.genre.is_empty() {
        builder = builder.genre([snapshot.genre.clone()]);
    }
    if let Some(art_url) = &snapshot.art_url {
        builder = builder.art_url(art_url.clone());
    }
    builder.build()
}

/// Spawns the MPRIS D-Bus server and turns it into a `Subscription`-friendly
/// stream, matching the shape of the existing `mpd_idle_stream`/
/// `playback_tick_stream` streams in `app.rs`. Yields exactly one
/// `MprisEvent::Ready` first, then an `MprisEvent::Command` per D-Bus call.
///
/// If no session bus is available, logs a `warn` and ends the stream
/// quietly — Lyra keeps running without media-key/applet integration.
pub fn mpris_stream() -> impl Stream<Item = MprisEvent> {
    cosmic::iced::stream::channel(
        16,
        move |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<MprisEvent>| async move {
            let (updates_tx, mut updates_rx) = cosmic::iced::futures::channel::mpsc::unbounded();
            let shared = Arc::new(SharedState {
                snapshot: Mutex::new(MprisSnapshot::default()),
                updates: updates_tx,
                art_cache: Mutex::new(HashMap::new()),
            });

            let imp = DbusPlayer {
                shared: Arc::clone(&shared),
                commands: emitter.clone(),
            };

            let server = match Server::new("Lyra", imp).await {
                Ok(server) => server,
                Err(err) => {
                    tracing::warn!("MPRIS: could not start D-Bus server (no session bus?): {err}");
                    return;
                }
            };

            if emitter
                .send(MprisEvent::Ready(MprisHandle { shared }))
                .await
                .is_err()
            {
                return;
            }

            use futures_util::StreamExt;
            while let Some(update) = updates_rx.next().await {
                if !update.properties.is_empty()
                    && let Err(err) = server.properties_changed(update.properties).await
                {
                    tracing::warn!("MPRIS: failed to emit PropertiesChanged: {err}");
                }
                if let Some(position) = update.seeked
                    && let Err(err) = server.emit(Signal::Seeked { position }).await
                {
                    tracing::warn!("MPRIS: failed to emit Seeked: {err}");
                }
            }
        },
    )
}

/// The D-Bus-facing implementation of `org.mpris.MediaPlayer2` and
/// `org.mpris.MediaPlayer2.Player`. Every getter reads `shared.snapshot`;
/// every method/setter forwards an [`MprisCommand`] through `commands` and
/// returns immediately — all actual playback logic lives in `app.rs`.
struct DbusPlayer {
    shared: Arc<SharedState>,
    commands: cosmic::iced::futures::channel::mpsc::Sender<MprisEvent>,
}

impl DbusPlayer {
    async fn forward(&self, command: MprisCommand) {
        let mut tx = self.commands.clone();
        let _ = tx.send(MprisEvent::Command(command)).await;
    }

    fn snapshot(&self) -> MprisSnapshot {
        self.shared.snapshot.lock().clone()
    }
}

impl RootInterface for DbusPlayer {
    async fn raise(&self) -> fdo::Result<()> {
        self.forward(MprisCommand::Raise).await;
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        self.forward(MprisCommand::Quit).await;
        Ok(())
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn set_fullscreen(&self, _fullscreen: bool) -> ZbusResult<()> {
        // CanSetFullscreen is false; Lyra has no fullscreen video surface.
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok("Lyra".to_owned())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok("io.github.m0rf30.Lyra".to_owned())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        // Only `file://` is wired up end-to-end (`OpenUri` ->
        // `Message::OpenFiles` -> ad-hoc tag read off disk); advertising
        // http(s) here would be a lie, since Lyra has no ad-hoc
        // network-stream playback path outside its library/queue model.
        Ok(vec!["file".to_owned()])
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        // Mirrors resources/io.github.m0rf30.Lyra.desktop's `MimeType=`
        // list, itself verified against
        // `player::engine::decoder::SUPPORTED_EXTENSIONS`.
        Ok(vec![
            "audio/mpeg".to_owned(),
            "audio/flac".to_owned(),
            "audio/ogg".to_owned(),
            "audio/x-vorbis+ogg".to_owned(),
            "audio/x-opus+ogg".to_owned(),
            "audio/x-wav".to_owned(),
            "audio/x-aiff".to_owned(),
            "audio/mp4".to_owned(),
            "audio/aac".to_owned(),
            "audio/x-ape".to_owned(),
            "audio/x-wavpack".to_owned(),
            "audio/x-musepack".to_owned(),
            "audio/x-dsf".to_owned(),
            "audio/x-dff".to_owned(),
            "audio/webm".to_owned(),
            "audio/x-matroska".to_owned(),
        ])
    }
}

impl PlayerInterface for DbusPlayer {
    async fn next(&self) -> fdo::Result<()> {
        self.forward(MprisCommand::Next).await;
        Ok(())
    }

    async fn previous(&self) -> fdo::Result<()> {
        self.forward(MprisCommand::Previous).await;
        Ok(())
    }

    async fn pause(&self) -> fdo::Result<()> {
        self.forward(MprisCommand::Pause).await;
        Ok(())
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        self.forward(MprisCommand::PlayPause).await;
        Ok(())
    }

    async fn stop(&self) -> fdo::Result<()> {
        self.forward(MprisCommand::Stop).await;
        Ok(())
    }

    async fn play(&self) -> fdo::Result<()> {
        self.forward(MprisCommand::Play).await;
        Ok(())
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        self.forward(MprisCommand::Seek(offset.as_micros())).await;
        Ok(())
    }

    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        // Per spec: ignore stale calls that don't target the current track.
        if track_id_for(self.snapshot().track_id) != track_id {
            return Ok(());
        }
        self.forward(MprisCommand::SetPosition(position.as_micros()))
            .await;
        Ok(())
    }

    async fn open_uri(&self, uri: String) -> fdo::Result<()> {
        self.forward(MprisCommand::OpenUri(uri)).await;
        Ok(())
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(map_status(self.snapshot().status))
    }

    async fn loop_status(&self) -> fdo::Result<MprisLoopStatus> {
        Ok(map_loop(self.snapshot().loop_mode))
    }

    async fn set_loop_status(&self, loop_status: MprisLoopStatus) -> ZbusResult<()> {
        self.forward(MprisCommand::Loop(map_loop_from_mpris(loop_status)))
            .await;
        Ok(())
    }

    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn set_rate(&self, _rate: PlaybackRate) -> ZbusResult<()> {
        // Lyra always plays at normal speed; MinimumRate/MaximumRate are
        // both pinned to 1.0 below, so clients shouldn't attempt this.
        Ok(())
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().shuffle)
    }

    async fn set_shuffle(&self, shuffle: bool) -> ZbusResult<()> {
        self.forward(MprisCommand::Shuffle(shuffle)).await;
        Ok(())
    }

    async fn metadata(&self) -> fdo::Result<Metadata> {
        Ok(build_metadata(&self.snapshot()))
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(self.snapshot().volume)
    }

    async fn set_volume(&self, volume: Volume) -> ZbusResult<()> {
        self.forward(MprisCommand::SetVolume(volume)).await;
        Ok(())
    }

    async fn position(&self) -> fdo::Result<Time> {
        Ok(Time::from_micros(self.snapshot().position_us))
    }

    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0)
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_go_next)
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_go_previous)
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_play)
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_play)
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_seek)
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }
}
