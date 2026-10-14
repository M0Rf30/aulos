// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Radio page controller.
//!
//! Maps the `Message::Radio` UI events (`update_radio`) onto
//! [`aulos_core::session::RadioState`], which owns the workflows, and keeps
//! what is UI-only: the add/rename/search form inputs, the notice texts and
//! the focus tasks. The async results are session events
//! (`Message::Session`); icons and starting the played station are done by
//! the session. The view (`crate::views::radio`) stays pure -- this
//! is the only place that touches `AppModel`'s `radio_*` fields.

use super::tasks::session_effect;
use super::{AppModel, Message};
use crate::views::radio::RadioMessage;
use aulos_core::fl;
use aulos_core::online::UrlError;
use aulos_core::online::radio::{self, SortOrder, StationQuery};
use aulos_core::session::RadioNotice;
use cosmic::prelude::*;
use cosmic::widget;

/// The toast text of a radio notice.
pub(super) fn notice_text(notice: RadioNotice) -> String {
    match notice {
        RadioNotice::Saved { name } => fl!("toast-radio-saved", name = name),
        RadioNotice::AlreadySaved { name } => fl!("toast-radio-already-saved", name = name),
        RadioNotice::SaveFailed { reason } => fl!("toast-radio-save-failed", reason = reason),
        RadioNotice::PlayFailed { reason } => fl!("toast-radio-play-failed", reason = reason),
    }
}

impl AppModel {
    /// Load saved radio stations from the online store.
    pub(super) fn load_radio_stations(&self) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.radio.load())
    }

    /// Handle a UI event from the radio view.
    pub(super) fn update_radio(&mut self, msg: RadioMessage) -> Task<cosmic::Action<Message>> {
        match msg {
            RadioMessage::TabSelected(tab) => {
                self.radio_tab = tab;
                Task::none()
            }
            RadioMessage::FilterChanged(text) => {
                self.radio_filter = text;
                Task::none()
            }
            RadioMessage::ToggleAddForm => {
                self.radio_add_open = !self.radio_add_open;
                if !self.radio_add_open {
                    self.radio_add_name.clear();
                    self.radio_add_url.clear();
                    self.radio_add_error = None;
                    return Task::none();
                }
                widget::text_input::focus(widget::Id::new(crate::views::radio::ADD_NAME_INPUT_ID))
                    .map(cosmic::Action::App)
            }
            RadioMessage::AddNameChanged(v) => {
                self.radio_add_name = v;
                Task::none()
            }
            RadioMessage::AddUrlChanged(v) => {
                self.radio_add_url = v;
                self.radio_add_error = None;
                Task::none()
            }
            RadioMessage::SubmitAdd => self.submit_add_station(),
            RadioMessage::PlaySaved(id) => session_effect(self.session.radio.play_saved(id)),
            RadioMessage::RemoveSaved(id) => self.remove_saved_station(id),
            RadioMessage::UndoRemove(station) => {
                session_effect(self.session.radio.restore(station))
            }
            RadioMessage::StartRename(id, name) => {
                self.radio_renaming_id = Some(id);
                self.radio_rename_input = name;
                widget::text_input::focus(widget::Id::new(crate::views::radio::RENAME_INPUT_ID))
                    .map(cosmic::Action::App)
            }
            RadioMessage::RenameInputChanged(v) => {
                self.radio_rename_input = v;
                Task::none()
            }
            RadioMessage::CancelRename => {
                self.radio_renaming_id = None;
                Task::none()
            }
            RadioMessage::CommitRename(id) => self.commit_rename(id),

            RadioMessage::SearchChanged(q) => {
                self.radio_search_query = q;
                Task::none()
            }
            RadioMessage::SearchSubmit => self.dispatch_search(),
            RadioMessage::TagSelected(tag) => {
                self.radio_search_tag = tag;
                self.dispatch_search()
            }
            RadioMessage::CountryToggled => {
                self.radio_search_country = if self.radio_search_country.is_empty() {
                    self.session.radio.locale_country.clone()
                } else {
                    String::new()
                };
                self.dispatch_search()
            }
            RadioMessage::SortSelected(idx) => {
                if let Some(order) = SortOrder::ALL.get(idx) {
                    self.radio_search_sort = *order;
                }
                self.dispatch_search()
            }
            RadioMessage::RetrySearch => self.dispatch_search(),
            RadioMessage::Preset(preset) => session_effect(self.session.radio.preset(preset)),
            RadioMessage::PlayResult(uuid) => {
                session_effect(self.session.radio.play_search_result(&uuid))
            }
            RadioMessage::SaveResult(uuid) => {
                session_effect(self.session.radio.save_search_result(&uuid))
            }

            // `src/app/view.rs` maps this variant straight to the
            // top-level `Message::Stop` at the call site (player
            // transport belongs to the QueueEngine slice) -- it never
            // actually reaches here.
            RadioMessage::Stop => Task::none(),
        }
    }

    /// A station was saved: the add form has done its job.
    pub(super) fn reset_radio_form(&mut self) {
        self.radio_add_name.clear();
        self.radio_add_url.clear();
        self.radio_add_open = false;
        self.radio_add_error = None;
    }

    fn submit_add_station(&mut self) -> Task<cosmic::Action<Message>> {
        let name = self.radio_add_name.trim().to_string();
        let url = self.radio_add_url.trim().to_string();
        match self.session.radio.add_by_url(name, url) {
            Ok(add) => {
                self.radio_add_error = None;
                session_effect(add)
            }
            Err(UrlError::Empty) => {
                self.radio_add_error = Some(fl!("radio-add-url-required"));
                Task::none()
            }
            Err(UrlError::NotHttp) => {
                self.radio_add_error = Some(fl!("radio-add-url-invalid"));
                Task::none()
            }
        }
    }

    /// Remove a saved station, with an Undo toast that re-saves it exactly
    /// as it was.
    fn remove_saved_station(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        let Some((station, remove)) = self.session.radio.remove(id) else {
            return Task::none();
        };
        let toast_name = station.name.clone();
        let toast = widget::toaster::Toast::new(fl!("toast-radio-removed", name = toast_name))
            .action(fl!("radio-undo"), move |_id| {
                Message::Radio(RadioMessage::UndoRemove(station.clone()))
            });
        Task::batch([session_effect(remove), self.push_toast(toast)])
    }

    fn commit_rename(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        let new_name = self.radio_rename_input.trim().to_string();
        self.radio_renaming_id = None;
        session_effect(self.session.radio.rename(id, new_name))
    }

    fn dispatch_search(&mut self) -> Task<cosmic::Action<Message>> {
        let query = StationQuery {
            name: self.radio_search_query.trim().to_string(),
            tag: self.radio_search_tag.unwrap_or_default().to_string(),
            countrycode: self.radio_search_country.clone(),
            order: self.radio_search_sort,
            limit: radio::DISCOVER_LIMIT,
        };
        session_effect(self.session.radio.search(query))
    }
}
