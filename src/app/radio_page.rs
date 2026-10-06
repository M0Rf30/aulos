// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Radio page controller.
//!
//! Owns every piece of update logic for the Radio section: the
//! `Message::Radio` UI-event dispatch (`update_radio`) and the
//! `Message::RadioEvent` async-result dispatch (`handle_radio_event`),
//! plus the private helpers/tasks that back them. The view
//! (`crate::views::radio`) stays pure -- this is the only place that
//! touches the online store, the radio-browser HTTP client, or
//! `AppModel`'s `radio_*` fields.

use super::tasks::resolve_and_play_radio;
use super::{AppModel, HTTP_CLIENT, Message, open_online_store};
use crate::fl;
use crate::library::Track;
use crate::online::radio::{self, SortOrder, StationQuery, StationSearchResult};
use crate::online::store::RadioStation;
use crate::views::radio::{DiscoverPreset, RadioMessage};
use cosmic::prelude::*;
use cosmic::widget;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Async results for the radio page: search/list/save/play outcomes.
#[derive(Debug, Clone)]
pub enum RadioEvent {
    StationsLoaded(Vec<RadioStation>),
    /// `generation` pairs with the request that kicked it off, so a slow,
    /// since-superseded query's results (or error) can be recognized and
    /// dropped instead of overwriting a newer one.
    SearchResults {
        generation: u64,
        result: Result<Vec<StationSearchResult>, String>,
    },
    Added {
        name: String,
        already_existed: bool,
        result: Result<i64, String>,
    },
    StreamResolved {
        name: String,
        favicon: String,
        key: String,
        result: Result<String, String>,
    },
}

/// How many results a Discover search/preset fetch asks for.
const DISCOVER_LIMIT: u32 = 50;

/// Derive a two-letter upper-case country code from the `LC_ALL`/`LANG`
/// locale (e.g. `en_US.UTF-8` -> `US`), or an empty string when it can't
/// be determined. Read once at startup from the process environment --
/// never a network lookup.
pub(super) fn locale_country_code() -> String {
    for key in ["LC_ALL", "LANG"] {
        if let Ok(v) = std::env::var(key)
            && let Some(region) = v.split('.').next().and_then(|l| l.split('_').nth(1))
            && region.len() == 2
            && region.chars().all(|c| c.is_ascii_alphabetic())
        {
            return region.to_ascii_uppercase();
        }
    }
    String::new()
}

impl AppModel {
    /// Load saved radio stations from the online store.
    pub(super) fn load_radio_stations(&self) -> Task<cosmic::Action<Message>> {
        cosmic::task::future(async move {
            let stations = tokio::task::spawn_blocking(|| {
                open_online_store()
                    .and_then(|store| store.list_radio_stations())
                    .unwrap_or_else(|e| {
                        tracing::warn!("list_radio_stations failed: {e}");
                        Vec::new()
                    })
            })
            .await
            .unwrap_or_default();
            cosmic::Action::App(Message::RadioEvent(RadioEvent::StationsLoaded(stations)))
        })
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
                }
                Task::none()
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
            RadioMessage::PlaySaved(id) => self.play_saved_station(id),
            RadioMessage::RemoveSaved(id) => self.remove_saved_station(id),
            RadioMessage::UndoRemove(station) => self.dispatch_add(
                station.name,
                station.stream_url,
                station.homepage,
                station.favicon_url,
                station.tags,
            ),
            RadioMessage::StartRename(id, name) => {
                self.radio_renaming_id = Some(id);
                self.radio_rename_input = name;
                Task::none()
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
                    self.radio_locale_country.clone()
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
            RadioMessage::Preset(preset) => self.dispatch_preset(preset),
            RadioMessage::PlayResult(uuid) => self.play_search_result(&uuid),
            RadioMessage::SaveResult(uuid) => self.save_search_result(&uuid),

            // `src/app/view.rs` maps this variant straight to the
            // top-level `Message::Stop` at the call site (player
            // transport belongs to the QueueEngine slice) -- it never
            // actually reaches here.
            RadioMessage::Stop => Task::none(),
        }
    }

    /// Handle an async result dispatched by one of the radio page's tasks.
    pub(super) fn handle_radio_event(
        &mut self,
        event: RadioEvent,
    ) -> Task<cosmic::Action<Message>> {
        match event {
            RadioEvent::StationsLoaded(stations) => {
                let icon_urls: Vec<String> = stations
                    .iter()
                    .map(|s| s.favicon_url.clone())
                    .filter(|u| !u.is_empty())
                    .collect();
                self.radio_stations = stations;
                self.load_online_icons(icon_urls)
            }
            RadioEvent::SearchResults { generation, result } => {
                if generation != self.radio_search_generation {
                    // Superseded by a newer request; drop silently.
                    return Task::none();
                }
                self.radio_search_loading = false;
                match result {
                    Ok(results) => {
                        let icon_urls: Vec<String> = results
                            .iter()
                            .map(|r| r.favicon.clone())
                            .filter(|u| !u.is_empty())
                            .collect();
                        self.radio_search_results = results;
                        self.radio_search_error = None;
                        self.load_online_icons(icon_urls)
                    }
                    Err(e) => {
                        self.radio_search_results.clear();
                        self.radio_search_error = Some(e);
                        Task::none()
                    }
                }
            }
            RadioEvent::Added {
                name,
                already_existed,
                result,
            } => match result {
                Ok(_) => {
                    self.radio_add_name.clear();
                    self.radio_add_url.clear();
                    self.radio_add_open = false;
                    self.radio_add_error = None;
                    let toast = if already_existed {
                        widget::toaster::Toast::new(fl!("toast-radio-already-saved", name = name))
                    } else {
                        widget::toaster::Toast::new(fl!("toast-radio-saved", name = name))
                    };
                    Task::batch([self.load_radio_stations(), self.push_toast(toast)])
                }
                Err(e) => self.push_toast(widget::toaster::Toast::new(fl!(
                    "toast-radio-save-failed",
                    reason = e
                ))),
            },
            RadioEvent::StreamResolved {
                name,
                favicon,
                key,
                result,
            } => match result {
                Ok(resolved_url) => {
                    self.radio_now_playing_favicon = favicon.clone();
                    self.radio_now_playing_key = key;
                    let icon_task = if favicon.is_empty() {
                        Task::none()
                    } else {
                        self.load_online_icons(vec![favicon])
                    };
                    let track = Track {
                        id: -1,
                        path: PathBuf::new(),
                        title: name,
                        artist: String::new(),
                        album_artist: String::new(),
                        album: String::new(),
                        genre: String::new(),
                        track_number: 0,
                        disc_number: 0,
                        year: 0,
                        duration: Duration::ZERO,
                        bitrate: 0,
                        sample_rate: 0,
                        provider_id: Arc::from("radio"),
                        source_uri: resolved_url,
                        is_favorite: false,
                        rating: None,
                        rg_track_gain: None,
                        rg_album_gain: None,
                    };
                    let play_task = self.play_track_list(vec![track], 0);
                    Task::batch([icon_task, play_task])
                }
                Err(e) => self.push_toast(widget::toaster::Toast::new(fl!(
                    "toast-radio-play-failed",
                    reason = e
                ))),
            },
        }
    }

    fn submit_add_station(&mut self) -> Task<cosmic::Action<Message>> {
        let name = self.radio_add_name.trim().to_string();
        let url = self.radio_add_url.trim().to_string();
        if url.is_empty() {
            self.radio_add_error = Some(fl!("radio-add-url-required"));
            return Task::none();
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.radio_add_error = Some(fl!("radio-add-url-invalid"));
            return Task::none();
        }
        self.radio_add_error = None;
        let display_name = if name.is_empty() { url.clone() } else { name };
        self.dispatch_add(
            display_name,
            url,
            String::new(),
            String::new(),
            String::new(),
        )
    }

    /// Shared by add-by-URL, Save-from-Discover and Undo-remove: runs the
    /// blocking DB upsert and dispatches `RadioEvent::Added` with the
    /// outcome (including whether the station already existed).
    fn dispatch_add(
        &mut self,
        name: String,
        stream_url: String,
        homepage: String,
        favicon_url: String,
        tags: String,
    ) -> Task<cosmic::Action<Message>> {
        let name_for_event = name.clone();
        cosmic::task::future(async move {
            let outcome = tokio::task::spawn_blocking(move || {
                open_online_store().and_then(|store| {
                    store.add_radio_station(&name, &stream_url, &homepage, &favicon_url, &tags)
                })
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            let (result, already_existed) = match outcome {
                Ok((id, existed)) => (Ok(id), existed),
                Err(e) => (Err(e), false),
            };
            cosmic::Action::App(Message::RadioEvent(RadioEvent::Added {
                name: name_for_event,
                already_existed,
                result,
            }))
        })
    }

    fn play_saved_station(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        let Some(station) = self.radio_stations.iter().find(|s| s.id == id) else {
            return Task::none();
        };
        resolve_and_play_radio(
            station.name.clone(),
            station.favicon_url.clone(),
            station.stream_url.clone(),
            station.stream_url.clone(),
        )
    }

    fn play_search_result(&mut self, uuid: &str) -> Task<cosmic::Action<Message>> {
        let Some(result) = self
            .radio_search_results
            .iter()
            .find(|r| r.stationuuid == uuid)
        else {
            return Task::none();
        };
        resolve_and_play_radio(
            result.name.clone(),
            result.favicon.clone(),
            result.url.clone(),
            result.url.clone(),
        )
    }

    fn save_search_result(&mut self, uuid: &str) -> Task<cosmic::Action<Message>> {
        let Some(result) = self
            .radio_search_results
            .iter()
            .find(|r| r.stationuuid == uuid)
            .cloned()
        else {
            return Task::none();
        };
        self.dispatch_add(
            result.name,
            result.url,
            result.homepage,
            result.favicon,
            result.tags,
        )
    }

    /// Remove a saved station: drop it from the in-memory list right away
    /// (so the row disappears immediately) and delete it from the store,
    /// with an Undo toast that re-saves it exactly as it was.
    fn remove_saved_station(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        let Some(station) = self.radio_stations.iter().find(|s| s.id == id).cloned() else {
            return Task::none();
        };
        self.radio_stations.retain(|s| s.id != id);
        let toast_name = station.name.clone();
        let toast = widget::toaster::Toast::new(fl!("toast-radio-removed", name = toast_name))
            .action(fl!("radio-undo"), move |_id| {
                Message::Radio(RadioMessage::UndoRemove(station.clone()))
            });
        let remove_task = cosmic::task::future(async move {
            let stations = tokio::task::spawn_blocking(move || {
                let store = open_online_store()?;
                store.remove_radio_station(id)?;
                store.list_radio_stations()
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
            .unwrap_or_else(|e| {
                tracing::error!("Failed to remove radio station: {e}");
                Vec::new()
            });
            cosmic::Action::App(Message::RadioEvent(RadioEvent::StationsLoaded(stations)))
        });
        Task::batch([remove_task, self.push_toast(toast)])
    }

    fn commit_rename(&mut self, id: i64) -> Task<cosmic::Action<Message>> {
        let new_name = self.radio_rename_input.trim().to_string();
        self.radio_renaming_id = None;
        if new_name.is_empty() {
            return Task::none();
        }
        cosmic::task::future(async move {
            let stations = tokio::task::spawn_blocking(move || {
                let store = open_online_store()?;
                store.rename_radio_station(id, &new_name)?;
                store.list_radio_stations()
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
            .unwrap_or_else(|e| {
                tracing::error!("Failed to rename radio station: {e}");
                Vec::new()
            });
            cosmic::Action::App(Message::RadioEvent(RadioEvent::StationsLoaded(stations)))
        })
    }

    fn dispatch_search(&mut self) -> Task<cosmic::Action<Message>> {
        self.radio_search_generation += 1;
        let generation = self.radio_search_generation;
        self.radio_search_loading = true;
        self.radio_search_error = None;
        let query = StationQuery {
            name: self.radio_search_query.trim().to_string(),
            tag: self.radio_search_tag.unwrap_or_default().to_string(),
            countrycode: self.radio_search_country.clone(),
            order: self.radio_search_sort,
            limit: DISCOVER_LIMIT,
        };
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                let client = HTTP_CLIENT.clone();
                radio::search_stations(&client, &query)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::RadioEvent(RadioEvent::SearchResults {
                generation,
                result,
            }))
        })
    }

    fn dispatch_preset(&mut self, preset: DiscoverPreset) -> Task<cosmic::Action<Message>> {
        self.radio_search_generation += 1;
        let generation = self.radio_search_generation;
        self.radio_search_loading = true;
        self.radio_search_error = None;
        cosmic::task::future(async move {
            let result = tokio::task::spawn_blocking(move || {
                let client = HTTP_CLIENT.clone();
                match preset {
                    DiscoverPreset::Popular => radio::top_click_stations(&client, DISCOVER_LIMIT),
                    DiscoverPreset::TopVoted => radio::top_vote_stations(&client, DISCOVER_LIMIT),
                }
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            cosmic::Action::App(Message::RadioEvent(RadioEvent::SearchResults {
                generation,
                result,
            }))
        })
    }
}
