// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! UI-side scrobbling state: settings inputs, connect flows, and the glue
//! between playback ticks and the background worker.

use super::audioscrobbler::{self, Endpoint};
use super::import::{self, ImportOutcome, ImportProgress, ImportRequest};
use super::listenbrainz;
use super::tracker::PlayTracker;
use super::worker::{Cmd, WorkerHandle, WorkerSettings};
use super::{ScrobbleTrack, Service, is_stream_provider, keys, queue, unix_now};
use crate::config::Config;
use crate::fl;
use cosmic::iced::Task;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum ScrobbleMessage {
    SetEnabled(Service, bool),
    SetStreams(bool),
    SetLoveSync(bool),
    LbTokenInput(String),
    LbConnect,
    /// The validated ListenBrainz user name, or an error.
    LbValidated(Result<String, String>),
    LastFmKeyInput(String),
    LastFmSecretInput(String),
    SaveLastFmKeys,
    LastFmKeysSaved(Result<(), String>),
    /// Request a token and open the browser to authorise it.
    StartAuth(Service),
    AuthStarted(Service, Result<String, String>),
    /// Exchange the authorised token for a session.
    CompleteAuth(Service),
    /// `(user name, session key)` — the key is already stored in the keyring.
    AuthCompleted(Service, Result<String, String>),
    Disconnect(Service),
    RetryNow,
    SyncLoved,
    /// Loved `(artist, title)` pairs from Last.fm; handled by the app, which
    /// owns the library.
    LovedFetched(Result<Vec<(String, String)>, String>),
    /// Import the service's listening history into the play history.
    ImportStart(Service),
    /// Ask a running import to stop after the current page.
    ImportCancel(Service),
    /// Running totals of an import (never emitted by the view).
    ImportProgress(Service, ImportProgress),
    /// An import ended; the app refreshes Home and shows a toast.
    ImportDone(Service, Result<ImportOutcome, String>),
    /// Start an import right after connecting a service.
    SetImportOnConnect(bool),
    /// Show "Because you listened to…" shelves on Home.
    SetOnlineSuggestions(bool),
    Noop,
}

/// A running history import.
struct ImportRun {
    cancel: Arc<AtomicBool>,
    progress: ImportProgress,
}

pub struct ScrobbleController {
    handle: WorkerHandle,
    tracker: PlayTracker,
    /// Wall-clock reference for streams, whose playback position is unreliable.
    stream_clock: Option<(String, Instant)>,
    pub lb_token_input: String,
    pub lastfm_key_input: String,
    pub lastfm_secret_input: String,
    /// Request tokens waiting for browser authorisation.
    pending: HashMap<Service, String>,
    busy: HashSet<Service>,
    status: HashMap<Service, String>,
    /// Track ids whose next favorite toggle must not be echoed to Last.fm
    /// (they were changed *by* the loved-track sync).
    suppress_love: HashSet<String>,
    /// Running history imports (at most one at a time).
    imports: HashMap<Service, ImportRun>,
    active: bool,
    allow_streams: bool,
    love_enabled: bool,
}

impl ScrobbleController {
    pub fn new(config: &Config) -> Self {
        let mut c = Self {
            handle: WorkerHandle::spawn(queue::default_path()),
            tracker: PlayTracker::new(),
            stream_clock: None,
            lb_token_input: String::new(),
            lastfm_key_input: config.scrobble_lastfm_api_key.clone(),
            lastfm_secret_input: String::new(),
            pending: HashMap::new(),
            busy: HashSet::new(),
            status: HashMap::new(),
            suppress_love: HashSet::new(),
            imports: HashMap::new(),
            active: false,
            allow_streams: false,
            love_enabled: false,
        };
        c.reconfigure(config);
        c
    }

    /// Push the current config to the worker and refresh cached flags.
    pub fn reconfigure(&mut self, config: &Config) {
        self.active = Service::ALL.iter().any(|s| is_active(config, *s));
        self.allow_streams = config.scrobble_streams;
        self.love_enabled = is_active(config, Service::LastFm) && config.scrobble_lastfm_love_sync;
        self.handle.send(Cmd::Configure(WorkerSettings {
            listenbrainz: is_active(config, Service::ListenBrainz),
            lastfm: is_active(config, Service::LastFm),
            librefm: is_active(config, Service::LibreFm),
            lastfm_api_key: config.scrobble_lastfm_api_key.clone(),
        }));
        if !self.active {
            self.tracker.reset();
        }
    }

    pub fn queued(&self, service: Service) -> usize {
        self.handle.queued(service)
    }

    pub fn error(&self, service: Service) -> Option<String> {
        self.handle.error(service)
    }

    pub fn status(&self, service: Service) -> Option<&str> {
        self.status.get(&service).map(String::as_str)
    }

    pub fn is_pending(&self, service: Service) -> bool {
        self.pending.contains_key(&service)
    }

    /// Counters of the running import of `service`, if any.
    pub fn import_progress(&self, service: Service) -> Option<&ImportProgress> {
        self.imports.get(&service).map(|run| &run.progress)
    }

    /// Whether any history import is running.
    pub fn is_importing(&self) -> bool {
        !self.imports.is_empty()
    }

    pub fn is_busy(&self, service: Service) -> bool {
        self.busy.contains(&service)
    }

    /// Whether at least one service is connected and enabled.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Feed the current playback state. Sends now-playing once per track and
    /// queues a scrobble when the track has been played long enough.
    pub fn on_playback(&mut self, track: &crate::library::Track, position: Duration) {
        if !self.active {
            return;
        }
        let Some(mut listen) = ScrobbleTrack::from_track(track, self.allow_streams) else {
            self.tracker.reset();
            return;
        };
        let stream = is_stream_provider(&track.provider_id);
        let key = format!(
            "{}|{}|{}",
            track.provider_id, track.source_uri, listen.title
        );
        let position = if stream {
            // Streams report no useful position: use time since the
            // station/episode (or ICY title) came on.
            match &self.stream_clock {
                Some((k, t0)) if *k == key => t0.elapsed(),
                _ => {
                    self.stream_clock = Some((key.clone(), Instant::now()));
                    Duration::ZERO
                }
            }
        } else {
            position
        };
        let now = unix_now();
        let out = self
            .tracker
            .tick(&key, position, track.duration, now, stream);
        if out.send_now_playing {
            listen.timestamp = 0;
            self.handle.send(Cmd::NowPlaying(listen.clone()));
        }
        if let Some(started) = out.scrobble_at {
            listen.timestamp = if stream {
                now - self.tracker.played().as_secs() as i64
            } else {
                started
            };
            self.handle.send(Cmd::Scrobble(listen));
        }
    }

    /// Mirror a favorite toggle to Last.fm. Returns without doing anything
    /// when love-sync is off or the toggle came from the loved-track sync.
    pub fn on_favorite_changed(
        &mut self,
        track_id: &str,
        track: &crate::library::Track,
        loved: bool,
    ) {
        if self.suppress_love.remove(track_id) || !self.love_enabled {
            return;
        }
        if let Some(l) = ScrobbleTrack::from_track(track, true) {
            self.handle.send(Cmd::Love {
                artist: l.artist,
                title: l.title,
                loved,
                bulk: false,
            });
        }
    }

    /// Mark `track_id` as about to be toggled by the sync (see `suppress_love`).
    pub fn suppress_next_love(&mut self, track_id: String) {
        self.suppress_love.insert(track_id);
    }

    /// Love `(artist, title)` on Last.fm as part of a bulk sync.
    pub fn love_bulk(&self, artist: String, title: String) {
        self.handle.send(Cmd::Love {
            artist,
            title,
            loved: true,
            bulk: true,
        });
    }

    pub fn set_status(&mut self, service: Service, text: String) {
        self.status.insert(service, text);
    }

    /// Mark the loved-track sync finished and show `text`.
    pub fn finish_sync(&mut self, text: String) {
        self.busy.remove(&Service::LastFm);
        self.status.insert(Service::LastFm, text);
    }

    /// Handle a settings-UI message. Returns the follow-up task and whether
    /// `config` changed (caller must persist it).
    pub fn update(
        &mut self,
        msg: ScrobbleMessage,
        config: &mut Config,
    ) -> (Task<ScrobbleMessage>, bool) {
        let mut changed = false;
        let mut auto_import = false;
        let task = match msg {
            ScrobbleMessage::SetEnabled(service, on) => {
                set_enabled(config, service, on);
                changed = true;
                Task::none()
            }
            ScrobbleMessage::SetStreams(on) => {
                config.scrobble_streams = on;
                changed = true;
                Task::none()
            }
            ScrobbleMessage::SetLoveSync(on) => {
                config.scrobble_lastfm_love_sync = on;
                changed = true;
                Task::none()
            }
            ScrobbleMessage::LbTokenInput(v) => {
                self.lb_token_input = v;
                Task::none()
            }
            ScrobbleMessage::LbConnect => {
                let token = self.lb_token_input.trim().to_string();
                if token.is_empty() {
                    Task::none()
                } else {
                    self.busy.insert(Service::ListenBrainz);
                    self.status.remove(&Service::ListenBrainz);
                    blocking(
                        move || {
                            let user = listenbrainz::validate_token(
                                listenbrainz::DEFAULT_BASE_URL,
                                &token,
                            )
                            .map_err(|e| e.to_string())?;
                            crate::credentials::store_password(
                                &keys::listenbrainz_token(),
                                &token,
                            )?;
                            Ok(user)
                        },
                        ScrobbleMessage::LbValidated,
                    )
                }
            }
            ScrobbleMessage::LbValidated(res) => {
                self.busy.remove(&Service::ListenBrainz);
                match res {
                    Ok(user) => {
                        config.scrobble_listenbrainz_user = user;
                        config.scrobble_listenbrainz_enabled = true;
                        self.lb_token_input.clear();
                        self.status.remove(&Service::ListenBrainz);
                        changed = true;
                        auto_import = config.scrobble_import_on_connect;
                    }
                    Err(e) => {
                        self.status
                            .insert(Service::ListenBrainz, fl!("scrobble-error", message = e));
                    }
                }
                if auto_import {
                    self.start_import(Service::ListenBrainz, config)
                } else {
                    Task::none()
                }
            }
            ScrobbleMessage::LastFmKeyInput(v) => {
                self.lastfm_key_input = v;
                Task::none()
            }
            ScrobbleMessage::LastFmSecretInput(v) => {
                self.lastfm_secret_input = v;
                Task::none()
            }
            ScrobbleMessage::SaveLastFmKeys => {
                config.scrobble_lastfm_api_key = self.lastfm_key_input.trim().to_string();
                changed = true;
                let secret = self.lastfm_secret_input.trim().to_string();
                if secret.is_empty() {
                    Task::none()
                } else {
                    blocking(
                        move || crate::credentials::store_password(&keys::lastfm_secret(), &secret),
                        ScrobbleMessage::LastFmKeysSaved,
                    )
                }
            }
            ScrobbleMessage::LastFmKeysSaved(res) => {
                match res {
                    Ok(()) => {
                        self.lastfm_secret_input.clear();
                        self.status.remove(&Service::LastFm);
                    }
                    Err(e) => {
                        self.status
                            .insert(Service::LastFm, fl!("scrobble-error", message = e));
                    }
                }
                Task::none()
            }
            ScrobbleMessage::StartAuth(service) => self.start_auth(service, config),
            ScrobbleMessage::AuthStarted(service, res) => {
                self.busy.remove(&service);
                match res {
                    Ok(token) => {
                        if let Some(endpoint) = Endpoint::for_service(service) {
                            let (key, _) = audioscrobbler::api_credentials(
                                service,
                                &config.scrobble_lastfm_api_key,
                                "",
                            );
                            let url = audioscrobbler::auth_page_url(&endpoint, &key, &token);
                            if let Err(e) = open::that_detached(&url) {
                                tracing::warn!("could not open browser: {e}");
                            }
                        }
                        self.pending.insert(service, token);
                        self.status.insert(service, fl!("scrobble-auth-hint"));
                    }
                    Err(e) => {
                        self.status
                            .insert(service, fl!("scrobble-error", message = e));
                    }
                }
                Task::none()
            }
            ScrobbleMessage::CompleteAuth(service) => self.complete_auth(service, config),
            ScrobbleMessage::AuthCompleted(service, res) => {
                self.busy.remove(&service);
                match res {
                    Ok(user) => {
                        self.pending.remove(&service);
                        self.status.remove(&service);
                        set_user(config, service, user);
                        set_enabled(config, service, true);
                        changed = true;
                        auto_import = config.scrobble_import_on_connect;
                    }
                    Err(e) => {
                        self.status
                            .insert(service, fl!("scrobble-error", message = e));
                    }
                }
                if auto_import {
                    self.start_import(service, config)
                } else {
                    Task::none()
                }
            }
            ScrobbleMessage::Disconnect(service) => {
                if let Some(run) = self.imports.get(&service) {
                    run.cancel.store(true, Ordering::Relaxed);
                }
                set_user(config, service, String::new());
                set_enabled(config, service, false);
                self.pending.remove(&service);
                self.status.remove(&service);
                self.handle.send(Cmd::Purge(service));
                changed = true;
                blocking(
                    move || {
                        let id = match service {
                            Service::ListenBrainz => keys::listenbrainz_token(),
                            _ => keys::session_key(service),
                        };
                        crate::credentials::delete_password(&id)
                    },
                    |_| ScrobbleMessage::Noop,
                )
            }
            ScrobbleMessage::RetryNow => {
                self.handle.send(Cmd::FlushNow);
                Task::none()
            }
            ScrobbleMessage::SyncLoved => {
                let user = config.scrobble_lastfm_user.clone();
                let key = config.scrobble_lastfm_api_key.clone();
                match Endpoint::for_service(Service::LastFm) {
                    Some(endpoint) if !user.is_empty() && !key.is_empty() => {
                        self.busy.insert(Service::LastFm);
                        blocking(
                            move || {
                                audioscrobbler::get_loved_tracks(&endpoint, &key, &user, 10)
                                    .map_err(|e| e.to_string())
                            },
                            ScrobbleMessage::LovedFetched,
                        )
                    }
                    _ => Task::none(),
                }
            }
            ScrobbleMessage::LovedFetched(res) => {
                // The app applies the result to the library; if it did not
                // intercept (it always does), at least clear the busy flag.
                self.busy.remove(&Service::LastFm);
                if let Err(e) = res {
                    self.status
                        .insert(Service::LastFm, fl!("scrobble-error", message = e));
                }
                Task::none()
            }
            ScrobbleMessage::ImportStart(service) => self.start_import(service, config),
            ScrobbleMessage::ImportCancel(service) => {
                if let Some(run) = self.imports.get(&service) {
                    run.cancel.store(true, Ordering::Relaxed);
                }
                Task::none()
            }
            ScrobbleMessage::ImportProgress(service, progress) => {
                if let Some(run) = self.imports.get_mut(&service) {
                    run.progress = progress;
                }
                Task::none()
            }
            ScrobbleMessage::ImportDone(service, res) => {
                self.imports.remove(&service);
                let text = match res {
                    Ok(out) => import_summary(&out),
                    Err(e) => fl!("scrobble-error", message = e),
                };
                self.status.insert(service, text);
                Task::none()
            }
            ScrobbleMessage::SetImportOnConnect(on) => {
                config.scrobble_import_on_connect = on;
                changed = true;
                Task::none()
            }
            ScrobbleMessage::SetOnlineSuggestions(on) => {
                config.home_online_suggestions = on;
                changed = true;
                Task::none()
            }
            ScrobbleMessage::Noop => Task::none(),
        };
        if changed {
            self.reconfigure(config);
        }
        (task, changed)
    }

    /// Start importing `service`'s listening history in the background.
    /// Only one import runs at a time, so a second service's listens are
    /// de-duplicated against the first one's.
    fn start_import(&mut self, service: Service, config: &Config) -> Task<ScrobbleMessage> {
        let user = user_of(config, service).to_string();
        if user.is_empty() || !self.imports.is_empty() {
            return Task::none();
        }
        if service == Service::LastFm && config.scrobble_lastfm_api_key.is_empty() {
            self.status.insert(service, fl!("scrobble-error-no-keys"));
            return Task::none();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.imports.insert(
            service,
            ImportRun {
                cancel: Arc::clone(&cancel),
                progress: ImportProgress::default(),
            },
        );
        self.status.remove(&service);
        let request = ImportRequest {
            service,
            user,
            lastfm_api_key: config.scrobble_lastfm_api_key.clone(),
            db_path: import::library_db_path(),
        };
        Task::stream(import_stream(request, cancel))
    }

    fn start_auth(&mut self, service: Service, config: &Config) -> Task<ScrobbleMessage> {
        let Some(endpoint) = Endpoint::for_service(service) else {
            return Task::none();
        };
        let api_key = config.scrobble_lastfm_api_key.clone();
        if service == Service::LastFm && api_key.is_empty() {
            self.status.insert(service, fl!("scrobble-error-no-keys"));
            return Task::none();
        }
        self.busy.insert(service);
        self.status.remove(&service);
        blocking(
            move || {
                let (key, secret) = credentials_for(service, &api_key);
                if key.is_empty() || secret.is_empty() {
                    return Err(fl!("scrobble-error-no-keys"));
                }
                audioscrobbler::get_token(&endpoint, &key, &secret).map_err(|e| e.to_string())
            },
            move |r| ScrobbleMessage::AuthStarted(service, r),
        )
    }

    fn complete_auth(&mut self, service: Service, config: &Config) -> Task<ScrobbleMessage> {
        let Some(token) = self.pending.get(&service).cloned() else {
            self.status.insert(service, fl!("scrobble-error-no-token"));
            return Task::none();
        };
        let Some(endpoint) = Endpoint::for_service(service) else {
            return Task::none();
        };
        let api_key = config.scrobble_lastfm_api_key.clone();
        self.busy.insert(service);
        blocking(
            move || {
                let (key, secret) = credentials_for(service, &api_key);
                let (user, session) = audioscrobbler::get_session(&endpoint, &key, &secret, &token)
                    .map_err(|e| e.to_string())?;
                crate::credentials::store_password(&keys::session_key(service), &session)?;
                Ok(user)
            },
            move |r| ScrobbleMessage::AuthCompleted(service, r),
        )
    }
}

/// One-line, localized result of an import.
pub fn import_summary(out: &ImportOutcome) -> String {
    let p = &out.progress;
    if let Some(e) = &out.error {
        return fl!(
            "scrobble-import-failed",
            message = e.clone(),
            imported = p.imported.to_string()
        );
    }
    if out.cancelled {
        return fl!(
            "scrobble-import-cancelled",
            fetched = p.fetched.to_string(),
            imported = p.imported.to_string()
        );
    }
    fl!(
        "scrobble-import-done",
        imported = p.imported.to_string(),
        matched = p.matched.to_string(),
        fetched = p.fetched.to_string(),
        skipped = p.skipped().to_string()
    )
}

/// Run an import on the blocking pool and stream its progress; ends with
/// exactly one `ImportDone`.
fn import_stream(
    request: ImportRequest,
    cancel: Arc<AtomicBool>,
) -> impl cosmic::iced::futures::Stream<Item = ScrobbleMessage> {
    use cosmic::iced::futures::SinkExt;
    let service = request.service;
    cosmic::iced::stream::channel(
        8,
        move |mut out: cosmic::iced::futures::channel::mpsc::Sender<ScrobbleMessage>| async move {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ScrobbleMessage>();
            let worker = tokio::task::spawn_blocking(move || {
                let progress_tx = tx.clone();
                let result = import::run_request(&request, &cancel, &mut |p| {
                    let _ = progress_tx.send(ScrobbleMessage::ImportProgress(service, p));
                });
                let _ = tx.send(ScrobbleMessage::ImportDone(service, result));
            });
            let mut finished = false;
            while let Some(msg) = rx.recv().await {
                finished |= matches!(msg, ScrobbleMessage::ImportDone(..));
                if out.send(msg).await.is_err() {
                    return;
                }
            }
            let _ = worker.await;
            if !finished {
                let _ = out
                    .send(ScrobbleMessage::ImportDone(
                        service,
                        Err("import task failed".into()),
                    ))
                    .await;
            }
        },
    )
}

/// `(api key, secret)` for `service`; the Last.fm secret comes from the keyring.
fn credentials_for(service: Service, lastfm_key: &str) -> (String, String) {
    let secret = if service == Service::LastFm {
        crate::credentials::retrieve_password(&keys::lastfm_secret())
            .ok()
            .flatten()
            .unwrap_or_default()
    } else {
        String::new()
    };
    audioscrobbler::api_credentials(service, lastfm_key, &secret)
}

/// Run `f` on the blocking pool and map its result to a message.
fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
    to_msg: impl Fn(Result<T, String>) -> ScrobbleMessage + Send + 'static,
) -> Task<ScrobbleMessage> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(f)
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
        },
        to_msg,
    )
}

// ---- config accessors -----------------------------------------------------

pub fn user_of(config: &Config, service: Service) -> &str {
    match service {
        Service::ListenBrainz => &config.scrobble_listenbrainz_user,
        Service::LastFm => &config.scrobble_lastfm_user,
        Service::LibreFm => &config.scrobble_librefm_user,
    }
}

fn set_user(config: &mut Config, service: Service, user: String) {
    match service {
        Service::ListenBrainz => config.scrobble_listenbrainz_user = user,
        Service::LastFm => config.scrobble_lastfm_user = user,
        Service::LibreFm => config.scrobble_librefm_user = user,
    }
}

pub fn enabled_of(config: &Config, service: Service) -> bool {
    match service {
        Service::ListenBrainz => config.scrobble_listenbrainz_enabled,
        Service::LastFm => config.scrobble_lastfm_enabled,
        Service::LibreFm => config.scrobble_librefm_enabled,
    }
}

fn set_enabled(config: &mut Config, service: Service, on: bool) {
    match service {
        Service::ListenBrainz => config.scrobble_listenbrainz_enabled = on,
        Service::LastFm => config.scrobble_lastfm_enabled = on,
        Service::LibreFm => config.scrobble_librefm_enabled = on,
    }
}

/// Connected (has a user name) — credentials live in the keyring.
pub fn is_connected(config: &Config, service: Service) -> bool {
    !user_of(config, service).is_empty()
}

/// Connected and switched on.
pub fn is_active(config: &Config, service: Service) -> bool {
    is_connected(config, service) && enabled_of(config, service)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_requires_connection_and_toggle() {
        let mut c = Config::default();
        assert!(!is_active(&c, Service::LastFm));
        c.scrobble_lastfm_enabled = true;
        assert!(!is_active(&c, Service::LastFm), "enabled but not connected");
        c.scrobble_lastfm_user = "bob".into();
        assert!(is_active(&c, Service::LastFm));
        c.scrobble_lastfm_enabled = false;
        assert!(!is_active(&c, Service::LastFm), "connected but disabled");
        assert!(is_connected(&c, Service::LastFm));
        assert!(!is_active(&c, Service::LibreFm));
    }

    #[test]
    fn setters_round_trip() {
        let mut c = Config::default();
        for s in Service::ALL {
            set_user(&mut c, s, "u".into());
            set_enabled(&mut c, s, true);
            assert_eq!(user_of(&c, s), "u");
            assert!(enabled_of(&c, s));
        }
    }
}
