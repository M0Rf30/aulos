// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::library::Reload;
use super::{Session, SessionUpdate};
use crate::config::edit_state::{MpdEditState, SubsonicEditState};
use crate::config::{self, Config};
use crate::fl;
use crate::library::LibraryDb;
use crate::player::PlaybackState;
use crate::provider::MusicProvider;
use crate::provider::local::LocalProvider;
#[cfg(not(target_arch = "wasm32"))]
use crate::provider::mpd::{MpdConfig, MpdProvider};
use crate::provider::mpd_events::{IdleAction, IdleSubsystem, idle_action};
use crate::provider::subsonic::{SubsonicConfig, SubsonicProvider};
use crate::provider::{ProviderRegistry, ProviderType};
use crate::task::Effect;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// A Subsonic server that could not be constructed: `(name, reason)`.
pub type ProviderInitError = (String, String);

/// The registered music providers and which one is active.
pub struct ProvidersState {
    pub registry: ProviderRegistry,
    /// Shared references to MPD providers for idle event subscriptions.
    #[cfg(not(target_arch = "wasm32"))]
    pub mpd_providers: Vec<Arc<MpdProvider>>,
    /// Bumped each time the MPD providers are rebuilt (`reinit_mpd`).
    /// Part of the MPD background-stream keys, so streams holding the
    /// previous providers are replaced instead of kept running on them.
    #[cfg(not(target_arch = "wasm32"))]
    pub mpd_generation: u64,
    /// Shared references to Subsonic providers for scrobbling.
    pub subsonic_providers: Vec<Arc<SubsonicProvider>>,
    /// Ordered list of (provider_id, display_name) for the selector dropdown.
    pub provider_list: Vec<(String, String)>,
    /// Index of the active provider in `provider_list`.
    pub active_index: Option<usize>,
    /// Set once the user explicitly picks a provider. Guards the
    /// startup-restoration logic in the MPD-connected handler so a later
    /// reconnect of a previously-active MPD server never overrides a manual
    /// switch away from it.
    pub manually_switched: bool,
    /// Editing state of the configured MPD servers (settings cards).
    pub mpd_edit_states: Vec<MpdEditState>,
    /// Last connection-test / connection status text per MPD card.
    pub mpd_connection_status: Vec<Option<String>>,
    /// Editing state of the configured Subsonic servers.
    pub subsonic_edit_states: Vec<SubsonicEditState>,
    /// Last connection-test status text per Subsonic card.
    pub subsonic_connection_status: Vec<Option<String>>,
    /// Last failure per provider id (MPD connect failure, failed connection
    /// test, Subsonic construction error); cleared when the provider
    /// connects or a test succeeds. Drives the sidebar status dots.
    pub provider_errors: HashMap<String, String>,
}

/// Health of one provider, for the sidebar status dots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStatus {
    pub id: String,
    pub name: String,
    /// Whether it is the provider the library is browsing.
    pub active: bool,
    /// The last failure, `None` while the provider is healthy or untested.
    pub error: Option<String>,
}

/// Async results of the provider workflows; feed them to
/// [`ProvidersState::apply_event`].
#[derive(Debug, Clone)]
pub enum ProviderEvent {
    /// Result of testing the connection of MPD card `0`.
    MpdTestResult(usize, Result<(), String>),
    /// Result of testing the connection of Subsonic card `0`.
    SubsonicTestResult(usize, Result<(), String>),
}

/// The outcome of re-registering Subsonic providers: servers that failed to
/// construct (for the UI to surface) and the library reload to run.
#[must_use]
pub struct ProviderReinit {
    pub reload: Reload,
    pub errors: Vec<ProviderInitError>,
}

impl ProviderReinit {
    fn none() -> Self {
        Self {
            reload: Reload::none(),
            errors: Vec::new(),
        }
    }
}

/// What the UI must do after an MPD server connected.
#[must_use]
pub struct MpdConnected {
    /// Swap the player's backend for the (now connected) active MPD
    /// provider.
    pub recreate_player: bool,
    pub reload: Reload,
}

/// What to do about an MPD idle event.
#[must_use]
pub enum MpdIdleOutcome {
    Ignore,
    Reload(Reload),
    ReloadPlaylists,
}

impl ProvidersState {
    /// Opens the library database, registers the local provider plus one
    /// MPD/Subsonic provider per configured server (browsing returns
    /// `NotConnected` until the connection is established), and restores the
    /// provider that was active at the last exit.
    pub(super) fn new(config: &Config) -> (Self, Vec<ProviderInitError>) {
        let db_path = crate::paths::library_db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let mut registry = ProviderRegistry::new();
        if let Ok(db) = LibraryDb::open(&db_path) {
            let local = LocalProvider::new(db, config.music_dirs.clone());
            registry.register(Arc::new(local));
        } else {
            tracing::error!("Failed to open library database");
        }

        let mut state = Self {
            registry,
            #[cfg(not(target_arch = "wasm32"))]
            mpd_providers: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            mpd_generation: 0,
            subsonic_providers: Vec::new(),
            provider_list: Vec::new(),
            active_index: None,
            manually_switched: false,
            mpd_edit_states: Vec::new(),
            mpd_connection_status: Vec::new(),
            subsonic_edit_states: Vec::new(),
            subsonic_connection_status: Vec::new(),
            provider_errors: HashMap::new(),
        };
        state.register_mpd(config);
        let init_errors = state.register_subsonic(config);
        state.rebuild_edit_states(config);

        // Restore the provider that was active when the app last exited,
        // falling back to local if it's no longer registered (server removed
        // from config, etc). All providers above are registered synchronously
        // regardless of connection state, so this already reflects the final
        // registered set even though connections complete asynchronously.
        let registered_ids: Vec<String> = state
            .registry
            .list()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        if let Some(target) =
            config::resolve_active_provider(config.startup_provider_choice(), &registered_ids)
        {
            state.registry.set_active(&target);
        }
        state.rebuild_list();

        (state, init_errors)
    }

    /// Registers one MPD provider per configured server.
    #[cfg(not(target_arch = "wasm32"))]
    fn register_mpd(&mut self, config: &Config) {
        for entry in &config.mpd_servers {
            let mpd_config: MpdConfig = entry.clone().into();
            let provider = Arc::new(MpdProvider::new(mpd_config));
            self.mpd_providers.push(Arc::clone(&provider));
            self.registry
                .register(Arc::clone(&provider) as Arc<dyn MusicProvider>);
        }
    }

    /// A browser page cannot open the TCP connection MPD speaks over: the
    /// configured servers (their cards stay editable) have no provider.
    #[cfg(target_arch = "wasm32")]
    fn register_mpd(&mut self, _config: &Config) {}

    /// Registers one Subsonic provider per configured server; returns the
    /// ones that failed to construct.
    fn register_subsonic(&mut self, config: &Config) -> Vec<ProviderInitError> {
        let mut errors = Vec::new();
        for entry in &config.subsonic_servers {
            let subsonic_config: SubsonicConfig = entry.clone().into();
            match SubsonicProvider::new(subsonic_config) {
                Ok(provider) => {
                    let provider = Arc::new(provider);
                    self.subsonic_providers.push(Arc::clone(&provider));
                    self.registry
                        .register(Arc::clone(&provider) as Arc<dyn MusicProvider>);
                }
                Err(e) => {
                    tracing::error!("Failed to create Subsonic provider '{}': {e}", entry.name);
                    self.provider_errors.insert(entry.id.clone(), e.to_string());
                    errors.push((entry.name.clone(), e.to_string()));
                }
            }
        }
        errors
    }

    /// Rebuilds the settings cards' editing state (and clears their
    /// connection status) from the configured servers.
    fn rebuild_edit_states(&mut self, config: &Config) {
        self.rebuild_mpd_edit_states(config);
        self.rebuild_subsonic_edit_states(config);
    }

    fn rebuild_mpd_edit_states(&mut self, config: &Config) {
        self.mpd_edit_states = config
            .mpd_servers
            .iter()
            .map(MpdEditState::from_config)
            .collect();
        self.mpd_connection_status = vec![None; self.mpd_edit_states.len()];
    }

    fn rebuild_subsonic_edit_states(&mut self, config: &Config) {
        self.subsonic_edit_states = config
            .subsonic_servers
            .iter()
            .map(SubsonicEditState::from_config)
            .collect();
        self.subsonic_connection_status = vec![None; self.subsonic_edit_states.len()];
    }

    /// Append an empty MPD server card to the settings form (not persisted
    /// until saved).
    pub fn add_mpd_card(&mut self) {
        let state = MpdEditState::new_default(self.mpd_edit_states.iter().map(|s| s.id.as_str()));
        self.mpd_edit_states.push(state);
        self.mpd_connection_status.push(None);
    }

    /// Append an empty Subsonic server card to the settings form (not
    /// persisted until saved).
    pub fn add_subsonic_card(&mut self) {
        let state =
            SubsonicEditState::new_default(self.subsonic_edit_states.iter().map(|s| s.id.as_str()));
        self.subsonic_edit_states.push(state);
        self.subsonic_connection_status.push(None);
    }

    /// Rebuild `provider_list` and `active_index` from the registry. Call
    /// after any change to the set of registered providers.
    pub fn rebuild_list(&mut self) {
        self.provider_list = self
            .registry
            .list()
            .into_iter()
            .map(|(id, name, _)| (id, name))
            .collect();
        self.active_index = self
            .provider_list
            .iter()
            .position(|(id, _)| id == self.registry.active_id());
    }

    /// Whether the active provider is of type `provider_type`.
    #[must_use]
    pub fn active_is(&self, provider_type: ProviderType) -> bool {
        self.registry
            .active()
            .is_some_and(|p| p.provider_type() == provider_type)
    }

    /// Re-registers the MPD providers from `config` (and rebuilds the MPD
    /// cards).
    fn reinit_mpd(&mut self, config: &Config) {
        self.registry.remove_by_type(ProviderType::Mpd);
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.mpd_providers.clear();
            self.mpd_generation += 1;
        }
        self.register_mpd(config);
        self.rebuild_list();
        self.rebuild_mpd_edit_states(config);
    }

    /// Re-registers the Subsonic providers from `config` (and rebuilds the
    /// Subsonic cards); returns the servers that failed to construct.
    fn reinit_subsonic(&mut self, config: &Config) -> Vec<ProviderInitError> {
        self.registry.remove_by_type(ProviderType::Subsonic);
        self.subsonic_providers.clear();
        for entry in self
            .subsonic_edit_states
            .iter()
            .map(|s| s.id.as_str())
            .chain(config.subsonic_servers.iter().map(|s| s.id.as_str()))
        {
            self.provider_errors.remove(entry);
        }
        let errors = self.register_subsonic(config);
        self.rebuild_list();
        self.rebuild_subsonic_edit_states(config);
        errors
    }

    /// Re-registers the local provider with the current scan directories.
    fn reinit_local(&mut self, config: &Config) {
        self.registry.remove_by_type(ProviderType::Local);
        if let Ok(db) = LibraryDb::open(&crate::paths::library_db_path()) {
            let local = LocalProvider::new(db, config.music_dirs.clone());
            self.registry.register(Arc::new(local));
        } else {
            tracing::error!("Failed to open library database for reinit");
        }
        self.rebuild_list();
    }

    /// Test the connection with the editing state of MPD card `i`.
    #[cfg(target_arch = "wasm32")]
    pub fn test_mpd_connection(&self, i: usize) -> Effect<ProviderEvent> {
        Effect::future(async move {
            ProviderEvent::MpdTestResult(i, Err("MPD is not available in the browser yet".into()))
        })
    }

    /// Test the connection with the editing state of MPD card `i`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn test_mpd_connection(&self, i: usize) -> Effect<ProviderEvent> {
        let Some(state) = self.mpd_edit_states.get(i) else {
            return Effect::none();
        };
        let host = state.host.clone();
        let port = state.port.parse().unwrap_or(6600u16);
        let password = Some(state.password.clone()).filter(|p| !p.is_empty());
        Effect::future(async move {
            let result =
                crate::provider::mpd::MpdProvider::connect_to(&host, port, password.as_deref())
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string());
            ProviderEvent::MpdTestResult(i, result)
        })
    }

    /// Test the connection with the editing state of Subsonic card `i`.
    pub fn test_subsonic_connection(&self, i: usize) -> Effect<ProviderEvent> {
        let Some(state) = self.subsonic_edit_states.get(i) else {
            return Effect::none();
        };
        let url = state.url.clone();
        let username = state.username.clone();
        let password = state.password.clone();
        let accept_invalid_certs = state.accept_invalid_certs;
        Effect::future(async move {
            let result = crate::provider::subsonic::test_connection(
                &url,
                &username,
                &password,
                accept_invalid_certs,
            )
            .await;
            ProviderEvent::SubsonicTestResult(i, result)
        })
    }

    /// Show a connection-test result on its card.
    pub fn apply_event(&mut self, event: ProviderEvent) {
        let status = |result: Result<(), String>| match result {
            Ok(()) => fl!("connected"),
            Err(e) => format!("{}: {e}", fl!("connection-failed")),
        };
        match event {
            ProviderEvent::MpdTestResult(i, result) => {
                if let Some(id) = self.mpd_edit_states.get(i).map(|s| s.id.clone()) {
                    self.note_health(id, result.as_ref().err());
                }
                if let Some(s) = self.mpd_connection_status.get_mut(i) {
                    *s = Some(status(result));
                }
            }
            ProviderEvent::SubsonicTestResult(i, result) => {
                if let Some(id) = self.subsonic_edit_states.get(i).map(|s| s.id.clone()) {
                    self.note_health(id, result.as_ref().err());
                }
                if let Some(s) = self.subsonic_connection_status.get_mut(i) {
                    *s = Some(status(result));
                }
            }
        }
    }

    /// Records (`Some`) or clears (`None`) the failure of provider `id`.
    fn note_health(&mut self, id: String, error: Option<&String>) {
        match error {
            Some(e) => {
                self.provider_errors.insert(id, e.clone());
            }
            None => {
                self.provider_errors.remove(&id);
            }
        }
    }

    /// One entry per registered provider (in selector order), followed by
    /// configured servers that failed to construct and so are not
    /// registered; the sidebar draws one status dot each.
    #[must_use]
    pub fn statuses(&self) -> Vec<ProviderStatus> {
        let mut out: Vec<ProviderStatus> = self
            .provider_list
            .iter()
            .enumerate()
            .map(|(i, (id, name))| ProviderStatus {
                id: id.clone(),
                name: name.clone(),
                active: self.active_index == Some(i),
                error: self.provider_errors.get(id).cloned(),
            })
            .collect();
        for state in &self.subsonic_edit_states {
            if out.iter().any(|s| s.id == state.id) {
                continue;
            }
            if let Some(error) = self.provider_errors.get(&state.id) {
                out.push(ProviderStatus {
                    id: state.id.clone(),
                    name: state.name.clone(),
                    active: false,
                    error: Some(error.clone()),
                });
            }
        }
        out
    }
}

impl Session {
    /// Make provider row `index` the active one. Persists the choice and
    /// resets everything derived from the previous provider's library and
    /// recreates the player for the new backend. The update carries the
    /// library reload plus [`SessionChanges::artist_photos_cleared`]; it is
    /// empty when `index` is not a registered provider.
    pub fn switch_provider(&mut self, index: usize) -> SessionUpdate {
        let Some((id, _name)) = self.providers.provider_list.get(index).cloned() else {
            return SessionUpdate::none();
        };
        if !self.providers.registry.set_active(&id) {
            return SessionUpdate::none();
        }
        self.providers.active_index = Some(index);
        self.providers.manually_switched = true;
        self.config.config.active_provider = Some(id.clone());
        self.config.save();
        tracing::info!("Switched to provider: {id}");

        // Clear current library data and reload from the new provider.
        let covers_cleared = self.library.covers.clear().cleared;
        self.cover_bytes.clear();
        self.library.clear_collections();
        self.refresh_search();
        self.artist_info.clear();
        self.recreate_player();
        let mut reload = self.reload_library();
        reload.covers.cleared |= covers_cleared;
        let mut update = SessionUpdate::from(reload);
        update.changes.artist_photos_cleared = true;
        update
    }

    /// Store the password of a settings card in the keyring right away, so
    /// it is never left as plaintext in the config file.
    fn store_entry_password(
        kind: &str,
        id: &str,
        password: &mut Option<String>,
        in_keyring: &mut bool,
    ) {
        if password.as_deref().is_none_or(str::is_empty) {
            *password = None;
            if let Err(e) = crate::credentials::delete_password(id) {
                tracing::warn!("Failed to delete {kind} password for '{id}': {e}");
            }
            *in_keyring = false;
        } else if let Some(pw) = password.as_deref() {
            match crate::credentials::store_password(id, pw) {
                Ok(()) => {
                    *in_keyring = true;
                    *password = None;
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to store {kind} password for '{id}' in keyring, \
                         keeping plaintext in config: {e}"
                    );
                }
            }
        }
    }

    /// Save MPD card `i` into the config (a new entry when `i` is past the
    /// configured ones) and re-register the MPD providers.
    pub fn save_mpd_server(&mut self, i: usize) -> Reload {
        let Some(state) = self.providers.mpd_edit_states.get(i) else {
            return Reload::none();
        };
        let mut entry = state.to_config();
        Self::store_entry_password(
            "MPD",
            &entry.id.clone(),
            &mut entry.password,
            &mut entry.password_in_keyring,
        );
        tracing::info!("MPD server config saved: {}", state.name);
        if i < self.config.config.mpd_servers.len() {
            self.config.config.mpd_servers[i] = entry;
        } else {
            self.config.config.mpd_servers.push(entry);
        }
        self.config.save();
        self.reinit_mpd_providers()
    }

    /// Remove MPD card `i` and its config entry, and re-register the MPD
    /// providers.
    pub fn remove_mpd_server(&mut self, i: usize) -> Reload {
        if i >= self.providers.mpd_edit_states.len() {
            return Reload::none();
        }
        let removed = self.providers.mpd_edit_states.remove(i);
        self.providers.mpd_connection_status.remove(i);
        if i < self.config.config.mpd_servers.len() {
            self.config.config.mpd_servers.remove(i);
        }
        if let Err(e) = crate::credentials::delete_password(&removed.id) {
            tracing::warn!("Failed to delete MPD password for '{}': {e}", removed.id);
        }
        tracing::info!("MPD server removed at index {i}");
        self.config.save();
        self.reinit_mpd_providers()
    }

    /// Save Subsonic card `i` into the config (a new entry when `i` is past
    /// the configured ones) and re-register the Subsonic providers.
    pub fn save_subsonic_server(&mut self, i: usize) -> ProviderReinit {
        let Some(state) = self.providers.subsonic_edit_states.get(i) else {
            return ProviderReinit::none();
        };
        let mut entry = state.to_config();
        Self::store_entry_password(
            "Subsonic",
            &entry.id.clone(),
            &mut entry.password,
            &mut entry.password_in_keyring,
        );
        tracing::info!("Subsonic server config saved: {}", state.name);
        if i < self.config.config.subsonic_servers.len() {
            self.config.config.subsonic_servers[i] = entry;
        } else {
            self.config.config.subsonic_servers.push(entry);
        }
        self.config.save();
        self.reinit_subsonic_providers()
    }

    /// Remove Subsonic card `i` and its config entry, and re-register the
    /// Subsonic providers.
    pub fn remove_subsonic_server(&mut self, i: usize) -> ProviderReinit {
        if i >= self.providers.subsonic_edit_states.len() {
            return ProviderReinit::none();
        }
        self.providers.subsonic_edit_states.remove(i);
        self.providers.subsonic_connection_status.remove(i);
        let removed_id = self
            .config
            .config
            .subsonic_servers
            .get(i)
            .map(|entry| entry.id.clone());
        if i < self.config.config.subsonic_servers.len() {
            self.config.config.subsonic_servers.remove(i);
        }
        tracing::info!("Subsonic server removed at index {i}");
        self.config.save();
        if let Some(id) = removed_id {
            if let Err(e) = crate::credentials::delete_password(&id) {
                tracing::warn!("Failed to delete Subsonic password for '{id}': {e}");
            }
            Self::forget_provider_cache(&id);
        }
        self.reinit_subsonic_providers()
    }

    /// Drops a removed provider's cached tracks and cover thumbnails from
    /// the shared database, so they don't linger once it is no longer
    /// configured.
    fn forget_provider_cache(provider_id: &str) {
        let Ok(db) = LibraryDb::open(&crate::paths::library_db_path()) else {
            return;
        };
        if let Err(e) = db.remove_provider_tracks(provider_id) {
            tracing::warn!("Failed to drop cached tracks for {provider_id}: {e}");
        }
        if let Err(e) = db.prune_remote_thumbs(provider_id, &std::collections::HashSet::new()) {
            tracing::warn!("Failed to drop cached covers for {provider_id}: {e}");
        }
    }

    /// Re-register all MPD providers from the config. The library is only
    /// reloaded when the local provider is active: an MPD provider's idle
    /// subscription fires [`Self::on_mpd_connected`] once connected, which
    /// reloads.
    fn reinit_mpd_providers(&mut self) -> Reload {
        self.providers.reinit_mpd(&self.config.config);
        if self.providers.active_is(ProviderType::Local) {
            self.reload_library()
        } else {
            Reload::none()
        }
    }

    /// Re-register all Subsonic providers from the config. Subsonic
    /// providers connect on demand, so the library reloads immediately when
    /// one is active.
    fn reinit_subsonic_providers(&mut self) -> ProviderReinit {
        let errors = self.providers.reinit_subsonic(&self.config.config);
        let reload = if self.providers.active_is(ProviderType::Subsonic) {
            self.reload_library()
        } else {
            Reload::none()
        };
        ProviderReinit { reload, errors }
    }

    /// Add `path` to the music directories (a no-op if already there),
    /// persist, and re-register the local provider. `true` when the
    /// directories changed and the library should be rescanned.
    pub fn add_music_dir(&mut self, path: PathBuf) -> bool {
        if self.config.config.music_dirs.contains(&path) {
            tracing::info!("Directory already in music_dirs: {}", path.display());
            return false;
        }
        tracing::info!("Adding music directory: {}", path.display());
        self.config.config.music_dirs.push(path);
        self.music_dirs_changed();
        true
    }

    /// Remove music directory `index`, persist, and re-register the local
    /// provider. `true` when a directory was removed and the library should
    /// be rescanned.
    pub fn remove_music_dir(&mut self, index: usize) -> bool {
        if index >= self.config.config.music_dirs.len() {
            return false;
        }
        let removed = self.config.config.music_dirs.remove(index);
        tracing::info!("Removed music directory: {}", removed.display());
        // The browser keeps no access to the folder once it is not listed;
        // the rescan the caller starts drops its tracks.
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            crate::web_fs::forget_root(&removed).await;
        });
        self.music_dirs_changed();
        true
    }

    fn music_dirs_changed(&mut self) {
        self.config.save();
        self.library.music_dirs_present = self
            .config
            .config
            .music_dirs
            .iter()
            .any(|d| crate::library::music_dir_exists(d));
        self.providers.reinit_local(&self.config.config);
    }

    /// An MPD server connected: show it on its card, restore the persisted
    /// active provider if this is it (unless the user switched manually
    /// since), and reload the library when it is the active one.
    pub fn on_mpd_connected(&mut self, provider_id: &str) -> MpdConnected {
        tracing::info!("MPD provider '{provider_id}' is now connected");
        self.providers.provider_errors.remove(provider_id);

        if let Some(idx) = self
            .providers
            .mpd_edit_states
            .iter()
            .position(|s| s.id == provider_id)
            && let Some(s) = self.providers.mpd_connection_status.get_mut(idx)
        {
            *s = Some(fl!("connected"));
        }

        // A manual choice always wins over the saved one, even across a
        // later reconnect of this server.
        if !self.providers.manually_switched
            && self.config.config.startup_provider_choice() == Some(provider_id)
            && self.providers.registry.active_id() != provider_id
            && self.providers.registry.set_active(provider_id)
        {
            self.providers.rebuild_list();
        }

        if self.providers.registry.active_id() != provider_id {
            return MpdConnected {
                recreate_player: false,
                reload: Reload::none(),
            };
        }
        // Recreating the player throws away the queue and current track, so
        // never do it mid-playback: a file opened from a file manager
        // starts playing before a configured MPD server finishes
        // connecting, and would otherwise be silently killed a moment
        // later. The backend gets attached on the next provider switch
        // instead.
        let recreate_player = self
            .playback
            .player
            .as_ref()
            .is_none_or(|p| p.state() == PlaybackState::Stopped);
        MpdConnected {
            recreate_player,
            reload: self.reload_library(),
        }
    }

    /// An MPD server failed to connect: show it on its card. Returns
    /// `(server name, reason)` for a notification only on the transition
    /// into the failure state; the reconnect loop retries every 5 s and
    /// would otherwise spam one per retry.
    pub fn on_mpd_connection_failed(
        &mut self,
        provider_id: &str,
        error: &str,
    ) -> Option<ProviderInitError> {
        tracing::error!("MPD provider '{provider_id}' failed to connect: {error}");

        self.providers
            .provider_errors
            .insert(provider_id.to_string(), error.to_string());
        let new_status = format!("{}: {error}", fl!("connection-failed"));
        let idx = self
            .providers
            .mpd_edit_states
            .iter()
            .position(|s| s.id == provider_id)?;
        let status = self.providers.mpd_connection_status.get_mut(idx)?;
        let already_failed = status.as_deref() == Some(new_status.as_str());
        *status = Some(new_status);
        if already_failed {
            return None;
        }
        let name = Some(self.providers.mpd_edit_states[idx].name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| provider_id.to_string());
        Some((name, error.to_string()))
    }

    /// An MPD idle event: reload what changed server-side, if the event is
    /// from the active provider.
    pub fn on_mpd_idle(&mut self, provider_id: &str, subsystem: IdleSubsystem) -> MpdIdleOutcome {
        if self.providers.registry.active_id() != provider_id {
            return MpdIdleOutcome::Ignore;
        }
        match idle_action(subsystem) {
            IdleAction::ReloadLibrary => {
                tracing::debug!("MPD idle event from provider '{provider_id}': reloading library");
                MpdIdleOutcome::Reload(self.reload_library())
            }
            IdleAction::ReloadPlaylists => {
                tracing::debug!(
                    "MPD idle event from provider '{provider_id}': reloading playlists"
                );
                MpdIdleOutcome::ReloadPlaylists
            }
            IdleAction::None => MpdIdleOutcome::Ignore,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::test_session;
    use super::*;

    #[test]
    fn mpd_failure_marks_the_provider_and_connecting_clears_it() {
        let mut session = test_session();
        session.providers.provider_list = vec![
            ("local".into(), "Local".into()),
            ("mpd-a".into(), "Studio".into()),
        ];
        session.providers.active_index = Some(0);
        session.providers.mpd_edit_states = vec![MpdEditState::new_default([])];
        session.providers.mpd_edit_states[0].id = "mpd-a".into();
        session.providers.mpd_connection_status = vec![None];

        let _ = session.on_mpd_connection_failed("mpd-a", "refused");
        let statuses = session.providers.statuses();
        assert_eq!(statuses.len(), 2);
        assert!(statuses[0].active && statuses[0].error.is_none());
        assert_eq!(statuses[1].error.as_deref(), Some("refused"));

        // `on_mpd_connected` also reloads the library; only the bookkeeping
        // matters here, so clear the way it does.
        session.providers.provider_errors.remove("mpd-a");
        assert!(session.providers.statuses()[1].error.is_none());
    }

    #[test]
    fn failed_connection_test_marks_the_server_until_a_test_passes() {
        let mut providers = test_session().providers;
        providers.provider_list = vec![("sub-a".into(), "Nimbus".into())];
        providers.subsonic_edit_states = vec![SubsonicEditState::new_default([])];
        providers.subsonic_edit_states[0].id = "sub-a".into();
        providers.subsonic_connection_status = vec![None];

        providers.apply_event(ProviderEvent::SubsonicTestResult(
            0,
            Err("Ping: 401".into()),
        ));
        assert_eq!(providers.statuses()[0].error.as_deref(), Some("Ping: 401"));
        providers.apply_event(ProviderEvent::SubsonicTestResult(0, Ok(())));
        assert_eq!(providers.statuses()[0].error, None);
    }

    #[test]
    fn unregistered_server_with_an_error_still_gets_a_status() {
        let mut providers = test_session().providers;
        providers.subsonic_edit_states = vec![SubsonicEditState::new_default([])];
        providers.subsonic_edit_states[0].id = "sub-x".into();
        providers
            .provider_errors
            .insert("sub-x".into(), "bad url".into());
        let statuses = providers.statuses();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].id, "sub-x");
        assert!(!statuses[0].active);
    }
}
