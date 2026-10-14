// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::credential_check::{
    CredentialUpdate, apply_credential_updates, needs_credential_check, run_credential_check,
};
use crate::config::Config;
use crate::config::writer::ConfigWriter;
use crate::task::Effect;
use std::path::PathBuf;

/// Results of the background keyring jobs of [`ConfigState`].
#[derive(Debug, Clone)]
pub enum ConfigEvent {
    /// The startup keyring check finished; the config changes it wants.
    CredentialsChecked(Vec<CredentialUpdate>),
    /// Whether the user's AcoustID key is in the keyring (after a probe or a
    /// save/delete); `Err` when the keyring could not be read.
    AcoustidKeyStored(Result<bool, String>),
}

/// The loaded settings and how they are persisted.
pub struct ConfigState {
    pub config: Config,
    /// Where the settings are persisted; `None`: not persisted.
    pub path: Option<PathBuf>,
    /// Background, coalescing config persistence (see
    /// [`crate::config::writer`]).
    pub writer: Option<ConfigWriter>,
    /// Whether the user's AcoustID key is in the keyring; `None` until
    /// known.
    pub acoustid_key_stored: Option<bool>,
}

impl ConfigState {
    /// `config` must be exactly what is stored at `path` now: it is the
    /// writer's baseline for change detection. `path` `None`: the settings
    /// are never persisted.
    pub(super) fn new(path: Option<PathBuf>, config: Config) -> Self {
        let writer = path
            .clone()
            .and_then(|path| ConfigWriter::spawn(path, config.clone()));
        Self {
            config,
            path,
            writer,
            acoustid_key_stored: None,
        }
    }

    /// Persist the current config. Non-blocking: the write happens on the
    /// config writer thread, coalesced and skipped when nothing changed.
    pub fn save(&self) {
        if let Some(writer) = &self.writer {
            writer.save(&self.config);
        }
    }

    /// Wait for queued config writes to reach disk (before exiting).
    pub fn flush(&self) {
        if let Some(writer) = &self.writer {
            writer.flush();
        }
    }

    /// Startup keyring work, off the UI thread: verifying/migrating the
    /// server passwords (only when some server has one) and probing whether
    /// the AcoustID key is stored. Costs secret-service round trips, so it
    /// must not run before the first frame.
    pub fn startup_checks(&self) -> Effect<ConfigEvent> {
        let credentials = needs_credential_check(&self.config).then(|| {
            let mpd_servers = self.config.mpd_servers.clone();
            let subsonic_servers = self.config.subsonic_servers.clone();
            Effect::future(async move {
                let updates = tokio::task::spawn_blocking(move || {
                    run_credential_check(
                        &mpd_servers,
                        &subsonic_servers,
                        crate::credentials::is_keyring_available,
                        crate::credentials::retrieve_password,
                        crate::credentials::store_password,
                    )
                })
                .await
                .unwrap_or_default();
                ConfigEvent::CredentialsChecked(updates)
            })
        });
        let acoustid = Effect::blocking(
            || {
                crate::credentials::retrieve_password(crate::tagger::ACOUSTID_USER_KEY_ENTRY)
                    .map(|key| key.is_some_and(|k| !k.is_empty()))
            },
            ConfigEvent::AcoustidKeyStored,
        );
        Effect::batch(credentials.into_iter().chain([acoustid]))
    }

    /// Store (or, when `key` is empty, delete) the user's AcoustID key in
    /// the keyring.
    pub fn save_acoustid_user_key(&mut self, key: String) -> Effect<ConfigEvent> {
        self.acoustid_key_stored = None;
        Effect::blocking(
            move || {
                let entry = crate::tagger::ACOUSTID_USER_KEY_ENTRY;
                if key.is_empty() {
                    crate::credentials::delete_password(entry).map(|()| false)
                } else {
                    crate::credentials::store_password(entry, &key).map(|()| true)
                }
            },
            ConfigEvent::AcoustidKeyStored,
        )
    }

    /// Apply the result of a background keyring job; persists config changes
    /// the credential check asked for.
    pub fn apply_event(&mut self, event: ConfigEvent) {
        match event {
            ConfigEvent::CredentialsChecked(updates) => {
                if apply_credential_updates(&mut self.config, &updates) {
                    self.save();
                }
            }
            ConfigEvent::AcoustidKeyStored(Ok(stored)) => self.acoustid_key_stored = Some(stored),
            ConfigEvent::AcoustidKeyStored(Err(e)) => {
                tracing::warn!("AcoustID key keyring access failed: {e}");
                self.acoustid_key_stored = None;
            }
        }
    }
}
