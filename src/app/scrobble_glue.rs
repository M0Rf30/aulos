// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! App-side glue for multi-service scrobbling: settings messages, favorite
//! → Last.fm "love" mirroring and the loved-track sync.

use super::{AppModel, Message};
use crate::fl;
use crate::online::scrobble::import::ImportOutcome;
use crate::online::scrobble::{ScrobbleMessage, Service};
use cosmic::prelude::*;
use std::collections::HashSet;

/// Upper bound on favorites pushed to Last.fm by a single sync.
const MAX_BULK_LOVES: usize = 500;

/// Case-insensitive `(artist, title)` identity used to match library tracks
/// against Last.fm's loved list.
fn loved_key(artist: &str, title: &str) -> String {
    format!(
        "{}\u{1}{}",
        artist.trim().to_lowercase(),
        title.trim().to_lowercase()
    )
}

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
        let (task, changed) = self.scrobble.update(msg, &mut self.config);
        if changed {
            self.save_config();
        }
        let mut tasks = vec![task.map(|m| cosmic::Action::App(Message::Scrobble(m)))];
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
            .all_tracks
            .iter()
            .find(|t| t.id.to_string() == track_id)
            .or(self
                .current_track
                .as_ref()
                .filter(|t| t.id.to_string() == track_id))
            .cloned();
        if let Some(track) = track {
            self.scrobble.on_favorite_changed(track_id, &track, loved);
        }
    }

    /// Two-way sync with Last.fm's loved tracks: loved tracks missing from
    /// Aulos' favorites become favorites; favorites Last.fm lacks are loved.
    fn apply_loved_tracks(&mut self, loved: &[(String, String)]) -> Task<cosmic::Action<Message>> {
        let remote: HashSet<String> = loved.iter().map(|(a, t)| loved_key(a, t)).collect();
        let mut to_favorite: Vec<String> = Vec::new();
        let mut to_love: Vec<(String, String)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for t in &self.all_tracks {
            if !crate::online::scrobble::source_allowed(&t.provider_id, false) {
                continue;
            }
            let key = loved_key(&t.artist, &t.title);
            if remote.contains(&key) {
                if !t.is_favorite {
                    to_favorite.push(t.id.to_string());
                }
            } else if t.is_favorite
                && to_love.len() < MAX_BULK_LOVES
                && !t.artist.trim().is_empty()
                && seen.insert(key)
            {
                to_love.push((t.artist.clone(), t.title.clone()));
            }
        }

        let added = to_favorite.len();
        let pushed = to_love.len();
        for (artist, title) in to_love {
            self.scrobble.love_bulk(artist, title);
        }
        let mut tasks = Vec::new();
        for id in to_favorite {
            // These toggles come from Last.fm itself; don't echo them back.
            self.scrobble.suppress_next_love(id.clone());
            tasks.push(self.handle_message(Message::ToggleFavorite(id)));
        }
        self.scrobble.finish_sync(fl!(
            "scrobble-sync-done",
            added = added.to_string(),
            pushed = pushed.to_string()
        ));
        Task::batch(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::loved_key;

    #[test]
    fn loved_key_ignores_case_and_padding() {
        assert_eq!(
            loved_key(" Björk ", "Hunter"),
            loved_key("björk", " hunter")
        );
        assert_ne!(loved_key("a", "b"), loved_key("b", "a"));
    }
}
