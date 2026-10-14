// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::Message;
use aulos_core::session::SessionEvent;
use aulos_core::task::Effect;
use cosmic::prelude::*;

/// Runs a backend [`Effect`] as an iced task.
pub(super) fn effect<M: Send + 'static>(effect: Effect<M>) -> Task<M> {
    match effect.into_stream() {
        Some(stream) => Task::stream(stream),
        None => Task::none(),
    }
}

/// Runs an [`Effect`] of any domain's event type, routing every event to
/// `Message::Session`.
pub(super) fn session_effect<E: Into<SessionEvent> + Send + 'static>(
    session_effect: Effect<E>,
) -> Task<cosmic::Action<Message>> {
    effect(session_effect.into_session()).map(|event| cosmic::Action::App(Message::Session(event)))
}
