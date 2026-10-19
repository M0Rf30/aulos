// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Linux backend: an MPRIS2 D-Bus server on the session bus (via the
//! `mpris-server` crate) so Aulos responds to desktop media keys and shows up
//! in the COSMIC/GNOME Shell/KDE media applets.
//!
//! API surface verified against `mpris-server` 0.10: `RootInterface` and
//! `PlayerInterface` are plain `async fn` traits usable from a
//! `tokio`-backed, multi-threaded executor. `Server::new(bus_name_suffix,
//! imp)` only requires `T: PlayerInterface` (which itself requires
//! `RootInterface`); it does not require `TrackListInterface` or
//! `PlaylistsInterface`, both of which Aulos does not implement.

use std::sync::Arc;

use futures::{SinkExt, Stream, StreamExt};
use mpris_server::zbus::fdo;
use mpris_server::{
    LoopStatus as MprisLoopStatus, Metadata, PlaybackRate, PlaybackStatus, PlayerInterface,
    Property, RootInterface, Server, Signal, Time, TrackId, Volume, zbus::Result as ZbusResult,
};

use super::{
    LoopMode, MprisCommand, MprisEvent, MprisHandle, MprisSnapshot, MprisStatus, NativeWindow,
    SharedState, Update, seek_target,
};

/// The properties that differ between two snapshots, as `PropertiesChanged`
/// entries.
fn changed_properties(previous: &MprisSnapshot, snapshot: &MprisSnapshot) -> Vec<Property> {
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
        properties.push(Property::Metadata(build_metadata(snapshot)));
    }
    properties
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
    TrackId::try_from(format!("/io/github/m0rf30/Aulos/Track/{id}")).unwrap_or(TrackId::NO_TRACK)
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

/// Spawns the MPRIS D-Bus server and turns it into a stream. Yields exactly
/// one `MprisEvent::Ready` first, then an `MprisEvent::Command` per D-Bus
/// call.
///
/// If no session bus is available, logs a `warn` and ends the stream
/// quietly.
pub fn stream(_window: Option<NativeWindow>) -> impl Stream<Item = MprisEvent> {
    crate::task::channel_stream(
        16,
        move |mut emitter: futures::channel::mpsc::Sender<MprisEvent>| async move {
            let (shared, mut updates_rx) = SharedState::new();

            let imp = DbusPlayer {
                shared: Arc::clone(&shared),
                commands: emitter.clone(),
            };

            let server = match Server::new("Aulos", imp).await {
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

            while let Some(Update { previous, next }) = updates_rx.next().await {
                let properties = changed_properties(&previous, &next);
                if !properties.is_empty()
                    && let Err(err) = server.properties_changed(properties).await
                {
                    tracing::warn!("MPRIS: failed to emit PropertiesChanged: {err}");
                }
                if let Some(position) = seek_target(&previous, &next)
                    && let Err(err) = server
                        .emit(Signal::Seeked {
                            position: Time::from_micros(position),
                        })
                        .await
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
    commands: futures::channel::mpsc::Sender<MprisEvent>,
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
        // CanSetFullscreen is false; Aulos has no fullscreen video surface.
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
        Ok("Aulos".to_owned())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok("io.github.m0rf30.Aulos".to_owned())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        // Only `file://` is wired up end-to-end (`OpenUri` ->
        // `Message::OpenFiles` -> ad-hoc tag read off disk); advertising
        // http(s) here would be a lie, since Aulos has no ad-hoc
        // network-stream playback path outside its library/queue model.
        Ok(vec!["file".to_owned()])
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        // Mirrors resources/io.github.m0rf30.Aulos.desktop's `MimeType=`
        // list, itself verified against
        // `player::engine::format_registry::SCAN_EXTENSIONS`.
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
        // Aulos always plays at normal speed; MinimumRate/MaximumRate are
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
