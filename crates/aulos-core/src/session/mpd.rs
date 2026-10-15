// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The MPD commands the transport sends, behind one seam: natively they go
//! through `mpd_client`, in the browser there is no MPD client yet, so
//! [`Client`] and [`Provider`] are uninhabited and no command can be built.

use super::playback::PlaybackEvent;
use crate::config::ReplayGainMode;
use crate::task::Effect;
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
pub(super) use native::*;
#[cfg(target_arch = "wasm32")]
pub(super) use web::*;

/// Wraps an asynchronous MPD command as an effect; failures are reported
/// through [`PlaybackEvent::MpdCommand`].
#[cfg(not(target_arch = "wasm32"))]
fn command(
    command: impl Future<Output = Result<(), String>> + Send + 'static,
) -> Effect<PlaybackEvent> {
    Effect::future(async move { PlaybackEvent::MpdCommand(command.await) })
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use crate::provider::mpd::MpdProvider;
    use std::sync::Arc;

    /// A handle to the MPD server's command connection.
    pub(in crate::session) type Client = mpd_client::Client;
    /// An MPD provider, for the commands that go through it.
    pub(in crate::session) type Provider = Arc<MpdProvider>;

    pub(in crate::session) fn seek(client: Client, target: Duration) -> Effect<PlaybackEvent> {
        command(async move {
            client
                .command(mpd_client::commands::Seek(
                    mpd_client::commands::SeekMode::Absolute(target),
                ))
                .await
                .map_err(|e| format!("MPD seek: {e}"))
        })
    }

    pub(in crate::session) fn set_pause(client: Client, paused: bool) -> Effect<PlaybackEvent> {
        command(async move {
            client
                .command(mpd_client::commands::SetPause(paused))
                .await
                .map_err(|e| format!("MPD set_pause: {e}"))
        })
    }

    pub(in crate::session) fn stop(client: Client) -> Effect<PlaybackEvent> {
        command(async move {
            client
                .command(mpd_client::commands::Stop)
                .await
                .map_err(|e| format!("MPD stop: {e}"))
        })
    }

    /// `level` is 0..=100.
    pub(in crate::session) fn set_volume(client: Client, level: u8) -> Effect<PlaybackEvent> {
        command(async move {
            client
                .command(mpd_client::commands::SetVolume(level))
                .await
                .map_err(|e| format!("MPD set_volume: {e}"))
        })
    }

    /// Sends MPD one `ClearQueue + Add + Play` command list for `uri`.
    ///
    /// Aulos drives MPD one track at a time from its own [`crate::player`]
    /// queue — shuffle/repeat live entirely in that queue, never in MPD
    /// itself. Forcing `random`/`repeat`/`single` off on every dispatch
    /// guards against a stale `repeat 1` (set by an external MPD client,
    /// or left over from before Aulos started driving this server): with a
    /// single-song MPD queue, `repeat 1` would otherwise loop that one
    /// song forever and `is_finished()` would never fire, silently
    /// freezing playback on the current track.
    ///
    /// Sent as one command list: one round trip, and no other client can
    /// interleave commands between `clear` and `play`.
    pub(in crate::session) fn play(client: Client, uri: String) -> Effect<PlaybackEvent> {
        use mpd_client::commands::{
            Add, ClearQueue, Play, SetRandom, SetRepeat, SetSingle, SingleMode,
        };
        command(async move {
            client
                .command_list((
                    ClearQueue,
                    Add::uri(&uri),
                    SetRandom(false),
                    SetRepeat(false),
                    SetSingle(SingleMode::Disabled),
                    Play::current(),
                ))
                .await
                .map_err(|e| format!("MPD play {uri}: {e}"))?;
            Ok(())
        })
    }

    pub(in crate::session) fn set_crossfade(
        provider: Provider,
        seconds: u64,
    ) -> Effect<PlaybackEvent> {
        command(async move {
            provider
                .send_crossfade(seconds)
                .map_err(|e| format!("crossfade: {e}"))
        })
    }

    pub(in crate::session) fn set_replay_gain_mode(
        provider: Provider,
        mode: ReplayGainMode,
    ) -> Effect<PlaybackEvent> {
        use mpd_client::commands::ReplayGainMode as MpdReplayGainMode;
        let mode = match mode {
            ReplayGainMode::Off => MpdReplayGainMode::Off,
            ReplayGainMode::Track => MpdReplayGainMode::Track,
            ReplayGainMode::Album => MpdReplayGainMode::Album,
            ReplayGainMode::Auto => MpdReplayGainMode::Auto,
        };
        command(async move {
            provider
                .send_replay_gain_mode(mode)
                .map_err(|e| format!("replay_gain_mode: {e}"))
        })
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;
    use std::convert::Infallible;

    pub(in crate::session) type Client = Infallible;
    pub(in crate::session) type Provider = Infallible;

    pub(in crate::session) fn seek(client: Client, _target: Duration) -> Effect<PlaybackEvent> {
        match client {}
    }

    pub(in crate::session) fn set_pause(client: Client, _paused: bool) -> Effect<PlaybackEvent> {
        match client {}
    }

    pub(in crate::session) fn stop(client: Client) -> Effect<PlaybackEvent> {
        match client {}
    }

    pub(in crate::session) fn set_volume(client: Client, _level: u8) -> Effect<PlaybackEvent> {
        match client {}
    }

    pub(in crate::session) fn play(client: Client, _uri: String) -> Effect<PlaybackEvent> {
        match client {}
    }

    pub(in crate::session) fn set_crossfade(
        provider: Provider,
        _seconds: u64,
    ) -> Effect<PlaybackEvent> {
        match provider {}
    }

    pub(in crate::session) fn set_replay_gain_mode(
        provider: Provider,
        _mode: ReplayGainMode,
    ) -> Effect<PlaybackEvent> {
        match provider {}
    }
}
