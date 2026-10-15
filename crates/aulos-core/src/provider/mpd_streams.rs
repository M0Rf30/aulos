// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Event streams for a connected MPD server: the status poll and the idle
//! (subsystem change) listener.
//!
//! Both are boxed streams of the core event types of [`super::mpd_events`]. A
//! UI does not start them by hand: `Session::background_streams` hands them
//! out under a stable `BackgroundKey` that identifies the logical stream, so a
//! UI that dedupes subscriptions by key keeps one running stream per provider
//! and never restarts it when a client handle is replaced.

use super::MusicProvider;
use super::mpd::{GetVolume, MpdProvider, song_to_track};
use super::mpd_events::{IdleSubsystem, MpdEvent, MpdStatusUpdate};
use crate::player::PlaybackState;
use crate::task::channel_stream;
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;

/// Everything about an MPD status that is worth redrawing for. The poll
/// stream only emits an [`MpdStatusUpdate`] when this differs from the
/// previously emitted one: while MPD is stopped or paused nothing changes, so
/// nothing is sent and the UI is not rebuilt; while playing `elapsed` moves
/// every poll, which is exactly the position-bar update that is needed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct StatusSig {
    state: PlaybackState,
    song_id: Option<mpd_client::commands::SongId>,
    volume: Option<u8>,
    duration: Duration,
    elapsed: Duration,
}

/// Whether `current` must be forwarded to the UI given the last forwarded
/// signature (`None` = nothing emitted yet, always emit the first one).
fn status_changed(previous: Option<&StatusSig>, current: &StatusSig) -> bool {
    previous != Some(current)
}

/// How long the status loop sleeps before polling again: only while playing
/// (position bar) is a short interval needed. Stopped/paused states are woken
/// by MPD idle events (see [`status_waker`]); the long interval is merely a
/// safety net in case the idle connection is down.
fn status_poll_delay(state: PlaybackState) -> Duration {
    const PLAYING_INTERVAL: Duration = Duration::from_millis(300);
    const SAFETY_INTERVAL: Duration = Duration::from_secs(15);
    if state == PlaybackState::Playing {
        PLAYING_INTERVAL
    } else {
        SAFETY_INTERVAL
    }
}

/// Per-provider wake-up for the status stream: [`idle_stream`] pokes it on
/// every MPD subsystem event (player/mixer/options/queue/...), so a stopped or
/// paused server is re-polled exactly when something happened instead of on a
/// timer. `Notify` stores one permit, so a poke that lands while the stream is
/// mid-poll is not lost.
fn status_waker(provider_id: &str) -> Arc<tokio::sync::Notify> {
    static WAKERS: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Notify>>>,
    > = std::sync::LazyLock::new(Default::default);
    let mut map = WAKERS.lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(map.entry(provider_id.to_string()).or_default())
}

/// MPD: poll status from the server and forward it only when it changed (see
/// [`StatusSig`]). Polls every 300ms while playing (keeps the position bar
/// smooth); while stopped/paused it sleeps until an idle event wakes it (with
/// a slow safety poll). The current song is fetched only when its identity
/// changes, so steady state stays at exactly one `Status` command per tick.
pub fn status_stream(
    client: mpd_client::Client,
    provider_id: Arc<str>,
) -> BoxStream<'static, MpdStatusUpdate> {
    let waker = status_waker(&provider_id);
    channel_stream(1, |mut emitter| async move {
        let mut last_song_id: Option<mpd_client::commands::SongId> = None;
        let mut last_sent: Option<StatusSig> = None;
        loop {
            let delay = match client.command(mpd_client::commands::Status).await {
                Ok(status) => {
                    let state = match status.state {
                        mpd_client::responses::PlayState::Playing => PlaybackState::Playing,
                        mpd_client::responses::PlayState::Paused => PlaybackState::Paused,
                        mpd_client::responses::PlayState::Stopped => PlaybackState::Stopped,
                    };

                    // `Status::volume` is 0 both when muted and when MPD
                    // has no mixer; only then ask `getvol` (0.23+), which
                    // returns nothing without a mixer. Pre-0.23 servers
                    // reject `getvol`: keep the 0.
                    let volume = if status.volume == 0 {
                        client.command(GetVolume).await.unwrap_or(Some(0))
                    } else {
                        Some(status.volume)
                    };

                    let current_id = status.current_song.map(|(_, id)| id);
                    let sig = StatusSig {
                        state,
                        song_id: current_id,
                        volume,
                        duration: status.duration.unwrap_or(Duration::ZERO),
                        elapsed: status.elapsed.unwrap_or(Duration::ZERO),
                    };

                    if status_changed(last_sent.as_ref(), &sig) {
                        // Fetch the current song only when its identity
                        // changed (including the first tick) — an
                        // unchanged song never needs a second command.
                        // While MPD is stopped there is nothing to adopt,
                        // so forget the last id: the next transition into
                        // Playing/Paused then re-fetches instead of seeing
                        // an unchanged id and staying silent forever.
                        let song = if state == PlaybackState::Stopped {
                            last_song_id = None;
                            None
                        } else if current_id == last_song_id {
                            None
                        } else {
                            last_song_id = current_id;
                            match client.command(mpd_client::commands::CurrentSong).await {
                                Ok(Some(song_in_queue)) => {
                                    Some(song_to_track(&provider_id, &song_in_queue.song))
                                }
                                Ok(None) => None,
                                Err(e) => {
                                    tracing::warn!("MPD current-song fetch failed: {e}");
                                    None
                                }
                            }
                        };

                        last_sent = Some(sig);
                        _ = emitter
                            .send(MpdStatusUpdate {
                                position: sig.elapsed,
                                duration: sig.duration,
                                state,
                                volume: volume.map(|v| f32::from(v) / 100.0),
                                song,
                            })
                            .await;
                    }

                    status_poll_delay(state)
                }
                Err(e) => {
                    tracing::warn!("MPD status poll failed: {e}");
                    // Forget what was sent so the first good status after
                    // an outage is always forwarded.
                    last_sent = None;
                    Duration::from_secs(1)
                }
            };

            tokio::select! {
                () = crate::task::sleep(delay) => {}
                () = waker.notified() => {}
            }
        }
    })
    .boxed()
}

/// MPD idle event stream — one per configured MPD provider.
///
/// Each stream opens TWO separate TCP connections to the MPD server:
/// 1. Idle connection — stays in idle mode, streams events via ConnectionEvents
/// 2. Command connection — stored in MpdProvider for browse/search/playback
///
/// This avoids protocol conflicts between idle mode and command execution.
pub fn idle_stream(provider: Arc<MpdProvider>) -> BoxStream<'static, MpdEvent> {
    channel_stream(4, move |mut emitter| async move {
        loop {
            let pid = provider.id().to_string();

            // Step 1: Establish the idle connection
            // We must keep `_idle_client` alive — dropping it would
            // close the background task and end the `events` stream.
            let idle_result = provider.connect_idle().await;
            let (_idle_client, mut events) = match idle_result {
                Ok(pair) => pair,
                Err(e) => {
                    _ = emitter
                        .send(MpdEvent::ConnectionFailed(pid, e.to_string()))
                        .await;
                    crate::task::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            // Step 2: Establish the command connection
            if let Err(e) = provider.connect_command().await {
                tracing::error!("MPD provider '{pid}' command connection failed: {e}");
                _ = emitter
                    .send(MpdEvent::ConnectionFailed(pid, e.to_string()))
                    .await;
                crate::task::sleep(Duration::from_secs(5)).await;
                continue;
            }

            // Both connections established
            _ = emitter.send(MpdEvent::Connected(pid.clone())).await;
            status_waker(&pid).notify_one();

            // Loop on idle events until the connection drops or closes.
            // Each event carries which MPD subsystem changed; only
            // `Database`/`Update` (library content) and
            // `StoredPlaylist` need a reload here — everything else
            // (player/mixer/options/queue/output/...) is already
            // covered by the status-poll stream, so forwarding
            // every idle wakeup as a blanket "reload the library"
            // trigger (the previous behavior) reset/flashed the views
            // on every pause, volume change, etc.
            while let Some(event) = events.next().await {
                match event {
                    mpd_client::client::ConnectionEvent::SubsystemChange(subsystem) => {
                        // Any subsystem change (player/mixer/options/...)
                        // may alter the status: re-poll now rather than
                        // on a timer (the status stream sleeps while MPD
                        // is stopped/paused).
                        status_waker(&pid).notify_one();
                        let idle_subsystem = IdleSubsystem::from(&subsystem);
                        _ = emitter
                            .send(MpdEvent::Idle(pid.clone(), idle_subsystem))
                            .await;
                    }
                    mpd_client::client::ConnectionEvent::ConnectionClosed(err) => {
                        tracing::warn!("MPD provider '{pid}' idle connection closed: {err}");
                        break;
                    }
                }
            }

            // Idle stream ended — connection lost. Disconnect command too.
            tracing::warn!("MPD provider '{pid}' connection lost, reconnecting...");
            provider.disconnect().await;

            // Backoff before reconnect
            crate::task::sleep(Duration::from_secs(5)).await;
        }
    })
    .boxed()
}

impl From<&mpd_client::client::Subsystem> for IdleSubsystem {
    fn from(subsystem: &mpd_client::client::Subsystem) -> Self {
        match subsystem {
            mpd_client::client::Subsystem::Database => IdleSubsystem::Database,
            mpd_client::client::Subsystem::Update => IdleSubsystem::Update,
            mpd_client::client::Subsystem::StoredPlaylist => IdleSubsystem::StoredPlaylist,
            _ => IdleSubsystem::Other,
        }
    }
}

#[cfg(test)]
mod status_poll_tests {
    use super::*;
    use mpd_client::commands::SongId;

    fn sig(state: PlaybackState, elapsed_ms: u64) -> StatusSig {
        StatusSig {
            state,
            song_id: Some(SongId(1)),
            volume: Some(50),
            duration: Duration::from_secs(200),
            elapsed: Duration::from_millis(elapsed_ms),
        }
    }

    #[test]
    fn first_status_is_always_forwarded() {
        assert!(status_changed(None, &sig(PlaybackState::Stopped, 0)));
    }

    #[test]
    fn identical_stopped_or_paused_status_is_suppressed() {
        let s = sig(PlaybackState::Stopped, 0);
        assert!(!status_changed(Some(&s), &s));
        let p = sig(PlaybackState::Paused, 12_000);
        assert!(!status_changed(Some(&p), &p));
    }

    #[test]
    fn moving_position_state_song_or_volume_is_forwarded() {
        let base = sig(PlaybackState::Playing, 1_000);
        assert!(status_changed(
            Some(&base),
            &sig(PlaybackState::Playing, 1_300)
        ));
        assert!(status_changed(
            Some(&base),
            &sig(PlaybackState::Paused, 1_000)
        ));
        let mut other_song = base;
        other_song.song_id = Some(SongId(2));
        assert!(status_changed(Some(&base), &other_song));
        let mut louder = base;
        louder.volume = Some(60);
        assert!(status_changed(Some(&base), &louder));
    }

    #[test]
    fn only_playing_polls_on_a_short_interval() {
        assert!(status_poll_delay(PlaybackState::Playing) <= Duration::from_millis(500));
        assert!(status_poll_delay(PlaybackState::Paused) >= Duration::from_secs(10));
        assert!(status_poll_delay(PlaybackState::Stopped) >= Duration::from_secs(10));
    }
}
