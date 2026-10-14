// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! App-side glue for multi-service scrobbling: settings messages, favorite
//! → Last.fm "love" mirroring and the loved-track sync.

use super::{AppModel, Message};
use aulos_core::fl;
use aulos_core::online::scrobble::import::ImportOutcome;
use aulos_core::online::scrobble::loved::{LovedSyncPlan, plan_sync};
use aulos_core::online::scrobble::{ScrobbleMessage, Service};
use cosmic::prelude::*;

impl AppModel {
    pub(super) fn handle_scrobble_message(
        &mut self,
        msg: ScrobbleMessage,
    ) -> Task<cosmic::Action<Message>> {
        if let ScrobbleMessage::LovedFetched(Ok(loved)) = msg {
            return self.apply_loved_tracks(&loved);
        }
        let import_done = match &msg {
            ScrobbleMessage::ImportDone(service, res) => Some((*service, res.clone())),
            _ => None,
        };
        // Messages after which the Home suggestions may have to appear,
        // vanish or change (a service connected/disconnected, the toggle).
        let affects_suggestions = matches!(
            msg,
            ScrobbleMessage::SetOnlineSuggestions(_)
                | ScrobbleMessage::LbValidated(_)
                | ScrobbleMessage::AuthCompleted(..)
                | ScrobbleMessage::Disconnect(_)
                | ScrobbleMessage::SaveLastFmKeys
        );
        let (effect, changed) = self
            .session
            .playback
            .scrobble
            .update(msg, &mut self.session.config.config);
        if changed {
            self.session.config.save();
        }
        let mut tasks =
            vec![super::tasks::effect(effect).map(|m| cosmic::Action::App(Message::Scrobble(m)))];
        if changed && affects_suggestions {
            tasks.push(self.load_similar());
        }
        if let Some((service, res)) = import_done {
            tasks.push(self.finish_history_import(service, res));
        }
        Task::batch(tasks)
    }

    /// A history import ended: toast the result and, if plays were added,
    /// reload Home so its shelves reflect them.
    fn finish_history_import(
        &mut self,
        service: Service,
        res: Result<ImportOutcome, String>,
    ) -> Task<cosmic::Action<Message>> {
        let name = service.display_name().to_string();
        let (text, imported) = match res {
            Err(message) => (
                fl!("toast-import-failed", service = name, message = message),
                0,
            ),
            Ok(out) => {
                let imported = out.progress.imported;
                let text = match (&out.error, out.cancelled) {
                    (Some(message), _) => {
                        fl!(
                            "toast-import-failed",
                            service = name,
                            message = message.clone()
                        )
                    }
                    (None, true) => fl!("toast-import-cancelled", service = name),
                    (None, false) => fl!(
                        "toast-import-done",
                        service = name,
                        imported = imported.to_string()
                    ),
                };
                (text, imported)
            }
        };
        let toast = self.push_toast(cosmic::widget::toaster::Toast::new(text));
        if imported > 0 {
            Task::batch([toast, self.load_home(true)])
        } else {
            toast
        }
    }

    /// A track was (un)favorited in Aulos: mirror it to Last.fm if enabled.
    pub(super) fn scrobble_favorite_changed(&mut self, track_id: &str, loved: bool) {
        let track = self
            .session
            .library
            .tracks
            .iter()
            .find(|t| t.id.to_string() == track_id)
            .or(self
                .session
                .playback
                .current_track
                .as_ref()
                .filter(|t| t.id.to_string() == track_id))
            .cloned();
        if let Some(track) = track {
            self.session
                .playback
                .scrobble
                .on_favorite_changed(track_id, &track, loved);
        }
    }

    /// Two-way sync with Last.fm's loved tracks: loved tracks missing from
    /// Aulos' favorites become favorites; favorites Last.fm lacks are loved.
    fn apply_loved_tracks(&mut self, loved: &[(String, String)]) -> Task<cosmic::Action<Message>> {
        let LovedSyncPlan {
            to_favorite,
            to_love,
        } = plan_sync(&self.session.library.tracks, loved);

        let added = to_favorite.len();
        let pushed = to_love.len();
        for (artist, title) in to_love {
            self.session.playback.scrobble.love_bulk(artist, title);
        }
        let mut tasks = Vec::new();
        for id in to_favorite {
            // These toggles come from Last.fm itself; don't echo them back.
            self.session
                .playback
                .scrobble
                .suppress_next_love(id.clone());
            tasks.push(self.handle_message(Message::ToggleFavorite(id)));
        }
        self.session.playback.scrobble.finish_sync(fl!(
            "scrobble-sync-done",
            added = added.to_string(),
            pushed = pushed.to_string()
        ));
        Task::batch(tasks)
    }
}
