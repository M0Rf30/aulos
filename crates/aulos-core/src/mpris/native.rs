// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Windows and macOS backend, on the `souvlaki` crate: System Media Transport
//! Controls on Windows (the media-key overlay and lock screen), the Now
//! Playing center and remote commands on macOS.
//!
//! The controls live on a thread of their own: creating them and pushing
//! metadata does blocking OS and file I/O, and the update loop must not run
//! on the UI or an async worker thread. Platforms extrapolate the position
//! of a playing track, so a position is only pushed on a state change or a
//! seek (see [`super::seek_target`]).

use std::time::Duration;

use futures::{SinkExt, Stream, StreamExt};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

use super::{
    MprisCommand, MprisEvent, MprisHandle, MprisSnapshot, MprisStatus, NativeWindow, SharedState,
    Update, seek_target,
};

/// The step of a transport "seek forward/backward" button with no amount.
const SEEK_STEP: Duration = Duration::from_secs(5);

/// The command a media-control event stands for.
fn command(event: MediaControlEvent) -> MprisCommand {
    let signed = |direction: SeekDirection, amount: Duration| {
        let micros = i64::try_from(amount.as_micros()).unwrap_or(i64::MAX);
        match direction {
            SeekDirection::Forward => micros,
            SeekDirection::Backward => -micros,
        }
    };
    match event {
        MediaControlEvent::Play => MprisCommand::Play,
        MediaControlEvent::Pause => MprisCommand::Pause,
        MediaControlEvent::Toggle => MprisCommand::PlayPause,
        MediaControlEvent::Next => MprisCommand::Next,
        MediaControlEvent::Previous => MprisCommand::Previous,
        MediaControlEvent::Stop => MprisCommand::Stop,
        MediaControlEvent::Seek(direction) => MprisCommand::Seek(signed(direction, SEEK_STEP)),
        MediaControlEvent::SeekBy(direction, amount) => {
            MprisCommand::Seek(signed(direction, amount))
        }
        MediaControlEvent::SetPosition(MediaPosition(position)) => {
            MprisCommand::SetPosition(i64::try_from(position.as_micros()).unwrap_or(i64::MAX))
        }
        MediaControlEvent::SetVolume(volume) => MprisCommand::SetVolume(volume),
        MediaControlEvent::OpenUri(uri) => MprisCommand::OpenUri(uri),
        MediaControlEvent::Raise => MprisCommand::Raise,
        MediaControlEvent::Quit => MprisCommand::Quit,
    }
}

/// The cover URL in the form `souvlaki` wants on this OS. Windows loads a
/// `file://` URL by stripping the scheme and opening what is left as a
/// path, so it needs the native path after `file://`; macOS parses a real
/// URL.
fn cover_url(art_url: &str) -> Option<String> {
    if cfg!(windows) {
        let path = crate::file_uri_to_path(art_url)?;
        Some(format!("file://{}", path.display()))
    } else {
        Some(art_url.to_owned())
    }
}

fn position(snapshot: &MprisSnapshot) -> Option<MediaPosition> {
    Some(MediaPosition(Duration::from_micros(
        u64::try_from(snapshot.position_us).unwrap_or(0),
    )))
}

fn track_changed(previous: &MprisSnapshot, next: &MprisSnapshot) -> bool {
    previous.track_id != next.track_id
        || previous.title != next.title
        || previous.artist != next.artist
        || previous.album != next.album
        || previous.length_us != next.length_us
        || previous.art_url != next.art_url
}

fn metadata<'a>(snapshot: &'a MprisSnapshot, cover_url: Option<&'a str>) -> MediaMetadata<'a> {
    MediaMetadata {
        title: Some(&snapshot.title),
        artist: Some(&snapshot.artist),
        album: Some(&snapshot.album),
        cover_url,
        duration: u64::try_from(snapshot.length_us)
            .ok()
            .filter(|&us| us > 0)
            .map(Duration::from_micros),
    }
}

/// Pushes what changed between the two snapshots of `update` to the OS.
fn push_update(controls: &mut MediaControls, update: &Update) {
    let (previous, next) = (&update.previous, &update.next);
    let metadata_changed = track_changed(previous, next);
    if metadata_changed {
        let cover = next.art_url.as_deref().and_then(cover_url);
        if let Err(e) = controls.set_metadata(metadata(next, cover.as_deref())) {
            // A cover the OS cannot load must not cost the title.
            tracing::debug!("media controls: metadata with cover failed ({e}); retrying without");
            if let Err(e) = controls.set_metadata(metadata(next, None)) {
                tracing::debug!("media controls: metadata failed: {e}");
            }
        }
    }
    // Setting metadata resets the position on macOS, so a new track also
    // re-publishes the playback state.
    if metadata_changed || previous.status != next.status || seek_target(previous, next).is_some() {
        let playback = match next.status {
            MprisStatus::Playing => MediaPlayback::Playing {
                progress: position(next),
            },
            MprisStatus::Paused => MediaPlayback::Paused {
                progress: position(next),
            },
            MprisStatus::Stopped => MediaPlayback::Stopped,
        };
        if let Err(e) = controls.set_playback(playback) {
            tracing::debug!("media controls: playback state failed: {e}");
        }
    }
}

/// Creates the controls and forwards their events as commands.
fn open(
    window: Option<NativeWindow>,
    commands: futures::channel::mpsc::Sender<MprisEvent>,
) -> Result<MediaControls, String> {
    let hwnd = match window {
        Some(NativeWindow(hwnd)) => Some(hwnd as *mut std::ffi::c_void),
        None if cfg!(windows) => return Err("no native window to attach to".to_owned()),
        None => None,
    };
    let mut controls = MediaControls::new(PlatformConfig {
        display_name: "Aulos",
        dbus_name: "aulos",
        hwnd,
    })
    .map_err(|e| format!("{e:?}"))?;
    controls
        .attach(move |event| {
            let _ = commands
                .clone()
                .try_send(MprisEvent::Command(command(event)));
        })
        .map_err(|e| format!("{e:?}"))?;
    Ok(controls)
}

/// Starts the media controls and yields exactly one `MprisEvent::Ready`
/// first, then an `MprisEvent::Command` per button press.
///
/// If the OS refuses them, logs a `warn` and ends the stream quietly.
pub fn stream(window: Option<NativeWindow>) -> impl Stream<Item = MprisEvent> {
    crate::task::channel_stream(
        16,
        move |mut emitter: futures::channel::mpsc::Sender<MprisEvent>| async move {
            let (shared, mut updates_rx) = SharedState::new();
            let (ready_tx, ready_rx) = futures::channel::oneshot::channel();
            let commands = emitter.clone();
            let spawned = std::thread::Builder::new()
                .name("aulos-media-controls".into())
                .spawn(move || {
                    let mut controls = match open(window, commands) {
                        Ok(controls) => {
                            let _ = ready_tx.send(Ok(()));
                            controls
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };
                    // Ends when the handle (and with it the sender) is dropped.
                    futures::executor::block_on(async {
                        while let Some(update) = updates_rx.next().await {
                            push_update(&mut controls, &update);
                        }
                    });
                });
            if let Err(e) = spawned {
                tracing::warn!("media controls: cannot spawn their thread: {e}");
                return;
            }
            match ready_rx.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!("media controls: unavailable: {e}");
                    return;
                }
                Err(_) => return,
            }
            let _ = emitter
                .send(MprisEvent::Ready(MprisHandle { shared }))
                .await;
        },
    )
}
