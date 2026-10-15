// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! A `keyring-core` credential store backed by the browser's `localStorage`.
//!
//! The web has no system keyring, so passwords live in the page origin's
//! `localStorage`, **unencrypted**, one item per `(service, user)` under the
//! key `aulos.credentials.<service>/<user>`. Anyone with access to the
//! browser profile, or to script running on this origin, can read them; the
//! UI says so next to every password field.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use keyring_core::api::{CredentialApi, CredentialPersistence, CredentialStoreApi};
use keyring_core::{Credential, Entry, Error, Result};

/// Prefix of every `localStorage` key this store writes.
const KEY_PREFIX: &str = "aulos.credentials.";

/// The `localStorage` key of one credential.
fn storage_key(service: &str, user: &str) -> String {
    format!("{KEY_PREFIX}{service}/{user}")
}

/// A storage failure, carried inside the keyring error types.
#[derive(Debug)]
struct StorageError(String);

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StorageError {}

fn no_access(what: &str) -> Error {
    Error::NoStorageAccess(Box::new(StorageError(what.to_string())))
}

fn failure(what: &str, detail: &wasm_bindgen::JsValue) -> Error {
    Error::PlatformFailure(Box::new(StorageError(format!("{what}: {detail:?}"))))
}

/// The page's `localStorage`; unavailable when the browser blocks storage
/// for this site.
fn storage() -> Result<web_sys::Storage> {
    web_sys::window()
        .ok_or_else(|| no_access("no window"))?
        .local_storage()
        .map_err(|e| failure("localStorage", &e))?
        .ok_or_else(|| no_access("localStorage is disabled for this site"))
}

/// One `(service, user)` item.
struct WebCredential {
    service: String,
    user: String,
}

impl fmt::Debug for WebCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never prints the secret.
        f.debug_struct("WebCredential")
            .field("service", &self.service)
            .field("user", &self.user)
            .finish()
    }
}

impl WebCredential {
    fn key(&self) -> String {
        storage_key(&self.service, &self.user)
    }

    fn read(&self) -> Result<String> {
        storage()?
            .get_item(&self.key())
            .map_err(|e| failure("read", &e))?
            .ok_or(Error::NoEntry)
    }
}

impl CredentialApi for WebCredential {
    fn set_password(&self, password: &str) -> Result<()> {
        storage()?
            .set_item(&self.key(), password)
            .map_err(|e| failure("write", &e))
    }

    fn set_secret(&self, secret: &[u8]) -> Result<()> {
        match std::str::from_utf8(secret) {
            Ok(text) => self.set_password(text),
            Err(_) => Err(Error::NotSupportedByStore(
                "the web store only keeps UTF-8 text".to_string(),
            )),
        }
    }

    fn get_password(&self) -> Result<String> {
        self.read()
    }

    fn get_secret(&self) -> Result<Vec<u8>> {
        self.read().map(String::into_bytes)
    }

    fn delete_credential(&self) -> Result<()> {
        let storage = storage()?;
        let key = self.key();
        if storage
            .get_item(&key)
            .map_err(|e| failure("read", &e))?
            .is_none()
        {
            return Err(Error::NoEntry);
        }
        storage.remove_item(&key).map_err(|e| failure("delete", &e))
    }

    fn get_credential(&self) -> Result<Option<Arc<Credential>>> {
        self.read().map(|_| None)
    }

    fn get_specifiers(&self) -> Option<(String, String)> {
        Some((self.service.clone(), self.user.clone()))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn debug_fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// The `localStorage` credential store.
#[derive(Debug)]
pub struct Store;

impl Store {
    /// A store over the current page's `localStorage`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl CredentialStoreApi for Store {
    fn vendor(&self) -> String {
        "Aulos web store (browser localStorage, unencrypted)".to_string()
    }

    fn id(&self) -> String {
        "aulos-web-local-storage".to_string()
    }

    fn build(
        &self,
        service: &str,
        user: &str,
        mods: Option<&HashMap<&str, &str>>,
    ) -> Result<Entry> {
        if mods.is_some_and(|m| !m.is_empty()) {
            return Err(Error::NotSupportedByStore(
                "the web store does not take entry modifiers".to_string(),
            ));
        }
        Ok(Entry::new_with_credential(Arc::new(WebCredential {
            service: service.to_string(),
            user: user.to_string(),
        })))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn persistence(&self) -> CredentialPersistence {
        CredentialPersistence::UntilDelete
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_namespaced_per_service_and_user() {
        assert_eq!(
            storage_key("io.github.m0rf30.Aulos", "subsonic-home"),
            "aulos.credentials.io.github.m0rf30.Aulos/subsonic-home"
        );
    }
}
