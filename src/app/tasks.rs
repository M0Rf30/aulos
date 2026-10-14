// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::Message;
use aulos_core::library::artist_info::ArtistInfoOutcome;
use aulos_core::session::EqualizerEvent;
use aulos_core::task::Effect;
use cosmic::prelude::*;

/// Runs a backend [`Effect`] as an iced task.
pub(super) fn effect<M: Send + 'static>(effect: Effect<M>) -> Task<M> {
    match effect.into_stream() {
        Some(stream) => Task::stream(stream),
        None => Task::none(),
    }
}

/// Runs an AutoEQ download, routing its result to `Message::AutoEQEvent`.
pub(super) fn autoeq_task(autoeq: Effect<EqualizerEvent>) -> Task<cosmic::Action<Message>> {
    effect(autoeq).map(|event| cosmic::Action::App(Message::AutoEQEvent(event)))
}

/// Runs artist-info fetches, routing each outcome to
/// `Message::ArtistInfoLoaded`.
pub(super) fn artist_info_task(fetch: Effect<ArtistInfoOutcome>) -> Task<cosmic::Action<Message>> {
    effect(fetch).map(|outcome| cosmic::Action::App(Message::ArtistInfoLoaded(outcome)))
}
