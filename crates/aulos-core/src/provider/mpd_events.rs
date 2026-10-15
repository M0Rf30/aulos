// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! What an MPD server reports, as plain data: the polled status, the
//! connection events and the idle (subsystem change) notifications with the
//! action each one calls for. Portable: the streams that produce them live in
//! `mpd_streams` (native only, they need an MPD client).

use crate::library::Track;
use crate::player::PlaybackState;
use std::time::Duration;

/// A polled MPD status (position, duration, state, volume).
#[derive(Debug, Clone)]
pub struct MpdStatusUpdate {
    pub position: Duration,
    pub duration: Duration,
    pub state: PlaybackState,
    /// `None` when MPD has no mixer (volume unknown).
    pub volume: Option<f32>,
    /// `Some` only on the tick where MPD's current song identity changed
    /// (including the first tick after the stream starts) — most ticks
    /// carry `None`.
    pub song: Option<Track>,
}

/// What the idle stream reports about one MPD provider.
#[derive(Debug, Clone)]
pub enum MpdEvent {
    /// Both the idle and the command connection are established.
    Connected(String),
    /// A connection attempt failed (`provider_id`, error text); the stream
    /// retries on its own after a back-off.
    ConnectionFailed(String, String),
    /// `provider_id`, plus which MPD subsystem changed.
    Idle(String, IdleSubsystem),
}

/// Mirrors the subset of `mpd_client::client::Subsystem` variants Aulos's
/// idle handler distinguishes between. Kept separate from the upstream
/// type (rather than embedding it directly in a UI message) because
/// messages must be `Clone` and `Subsystem::Other` wraps a `Box<str>` —
/// collapsing everything Aulos doesn't act on to a single `Other` unit
/// variant is all [`idle_action`] needs and keeps the conversion trivial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleSubsystem {
    /// The song database changed (files added/removed/rescanned).
    Database,
    /// A database `update`/`rescan` job finished.
    Update,
    /// A stored playlist was created, deleted, or modified.
    StoredPlaylist,
    /// Everything else (player/mixer/options/queue/output/partition/...):
    /// already covered by the status-poll stream, so the idle
    /// handler takes no action for these.
    Other,
}

/// What an MPD idle event should trigger in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleAction {
    /// Reload the whole library (tracks/albums/artists/covers).
    ReloadLibrary,
    /// Reload only the stored-playlists list.
    ReloadPlaylists,
    /// Nothing to do.
    None,
}

/// Pure mapping from an idle subsystem notification to the action the UI
/// should take.
#[must_use]
pub fn idle_action(subsystem: IdleSubsystem) -> IdleAction {
    match subsystem {
        IdleSubsystem::Database => IdleAction::ReloadLibrary,
        // `update` fires once a `update`/`rescan` command's background job
        // completes; treat it the same as `database` since content may
        // have changed without MPD necessarily also emitting a separate
        // `database` event for it.
        IdleSubsystem::Update => IdleAction::ReloadLibrary,
        IdleSubsystem::StoredPlaylist => IdleAction::ReloadPlaylists,
        IdleSubsystem::Other => IdleAction::None,
    }
}

#[cfg(test)]
mod idle_action_tests {
    use super::{IdleAction, IdleSubsystem, idle_action};

    #[test]
    fn database_reloads_library() {
        assert_eq!(
            idle_action(IdleSubsystem::Database),
            IdleAction::ReloadLibrary
        );
    }

    #[test]
    fn update_reloads_library() {
        assert_eq!(
            idle_action(IdleSubsystem::Update),
            IdleAction::ReloadLibrary
        );
    }

    #[test]
    fn stored_playlist_reloads_playlists_only() {
        assert_eq!(
            idle_action(IdleSubsystem::StoredPlaylist),
            IdleAction::ReloadPlaylists
        );
    }

    #[test]
    fn other_subsystems_do_nothing() {
        assert_eq!(idle_action(IdleSubsystem::Other), IdleAction::None);
    }
}
