// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Secure credential storage using the system keyring.
//!
//! Passwords for MPD and Subsonic providers are stored in the platform's
//! native credential store via the `keyring-core` crate: the kernel keyutils
//! or the D-Bus secret service (GNOME Keyring, KDE Wallet) on Linux, the
//! Credential Manager on Windows, the Keychain on macOS. The web build has no
//! keyring: its store keeps passwords **unencrypted** in the browser's
//! `localStorage` (see [`web_store`]). Other targets have no store, so every
//! call reports the keyring as unavailable. Each password is keyed by
//! `(SERVICE, provider_id)`.

use std::sync::{Arc, Once};

use keyring_core::api::CredentialStore;

#[cfg(target_arch = "wasm32")]
mod web_store;

const SERVICE: &str = "io.github.m0rf30.Aulos";

/// Keyring service used before the Lyra → Aulos rename. Read as a fallback by
/// [`retrieve_password`], which re-stores hits under [`SERVICE`] and deletes
/// the legacy entry, so saved MPD/Subsonic passwords survive the rename.
const LEGACY_SERVICE: &str = "io.github.m0rf30.Lyra";

static INIT: Once = Once::new();

/// The platform's credential store, `None` when it cannot be reached.
///
/// On Linux the `keyring` facade only wires up the D-Bus secret service, so
/// the stack is built here: the kernel keyutils store first, the secret
/// service when keyutils is unavailable (the old keyring v3 default).
#[cfg(target_os = "linux")]
fn platform_store() -> Option<Arc<CredentialStore>> {
    linux_keyutils_keyring_store::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .or_else(|_| {
            zbus_secret_service_keyring_store::Store::new()
                .map(|store| store as Arc<CredentialStore>)
        })
        .ok()
}

/// Windows Credential Manager.
#[cfg(target_os = "windows")]
fn platform_store() -> Option<Arc<CredentialStore>> {
    windows_native_keyring_store::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .ok()
}

/// The macOS (login) Keychain.
#[cfg(target_os = "macos")]
fn platform_store() -> Option<Arc<CredentialStore>> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|store| store as Arc<CredentialStore>)
        .ok()
}

/// The browser's `localStorage`, unencrypted (the web has no keyring).
#[cfg(target_arch = "wasm32")]
fn platform_store() -> Option<Arc<CredentialStore>> {
    Some(web_store::Store::new())
}

/// No credential store on this target.
#[cfg(not(any(
    target_os = "linux",
    target_os = "windows",
    target_os = "macos",
    target_arch = "wasm32"
)))]
fn platform_store() -> Option<Arc<CredentialStore>> {
    None
}

/// Select the default credential store, once, before any [`keyring_core::Entry`] is created.
///
/// `keyring-core` requires a default credential store to be set globally before
/// `Entry::new` can succeed; [`platform_store`] picks it per OS.
///
/// Safe to call multiple times (and from multiple threads/functions): the actual
/// selection only happens on the first call, via [`std::sync::Once`]. If the
/// store fails to initialize, no default store is set and subsequent
/// `Entry::new` calls will error; callers already treat keyring errors as
/// "keyring unavailable" degraded mode.
pub fn init() {
    INIT.call_once(|| {
        if let Some(store) = platform_store() {
            keyring_core::set_default_store(store);
        }
    });
}

/// Store a password in the system keyring for the given provider ID.
pub fn store_password(provider_id: &str, password: &str) -> Result<(), String> {
    init();
    let entry = keyring_core::Entry::new(SERVICE, provider_id)
        .map_err(|e| format!("Failed to create keyring entry for '{provider_id}': {e}"))?;
    entry
        .set_password(password)
        .map_err(|e| format!("Failed to store password for '{provider_id}': {e}"))
}

/// Retrieve a password from the system keyring for the given provider ID.
///
/// Returns `Ok(None)` if no entry exists (rather than treating it as an error).
pub fn retrieve_password(provider_id: &str) -> Result<Option<String>, String> {
    init();
    let entry = keyring_core::Entry::new(SERVICE, provider_id)
        .map_err(|e| format!("Failed to create keyring entry for '{provider_id}': {e}"))?;
    match entry.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring_core::Error::NoEntry) => Ok(migrate_legacy_password(provider_id)),
        Err(e) => Err(format!(
            "Failed to retrieve password for '{provider_id}': {e}"
        )),
    }
}

/// Look the password up under [`LEGACY_SERVICE`]; on a hit, move it to
/// [`SERVICE`]. Failures are swallowed: a missing legacy entry is the normal
/// case, and a failed move just means the lookup repeats next time.
fn migrate_legacy_password(provider_id: &str) -> Option<String> {
    let legacy = keyring_core::Entry::new(LEGACY_SERVICE, provider_id).ok()?;
    let password = legacy.get_password().ok()?;
    if store_password(provider_id, &password).is_ok() {
        let _ = legacy.delete_credential();
    }
    Some(password)
}

/// Delete a password from the system keyring for the given provider ID.
///
/// Silently succeeds if no entry exists.
pub fn delete_password(provider_id: &str) -> Result<(), String> {
    init();
    let entry = keyring_core::Entry::new(SERVICE, provider_id)
        .map_err(|e| format!("Failed to create keyring entry for '{provider_id}': {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!(
            "Failed to delete password for '{provider_id}': {e}"
        )),
    }
}

/// Check whether the system keyring is available and functional.
///
/// Performs a test store/delete cycle with a probe key. Returns `true` if the
/// keyring is usable, `false` otherwise.
pub fn is_keyring_available() -> bool {
    init();
    const PROBE_USER: &str = "__aulos_keyring_probe__";
    const PROBE_PASSWORD: &str = "probe";

    let entry = match keyring_core::Entry::new(SERVICE, PROBE_USER) {
        Ok(e) => e,
        Err(_) => return false,
    };

    if entry.set_password(PROBE_PASSWORD).is_err() {
        return false;
    }

    // Clean up the probe entry; ignore errors during cleanup.
    let _ = entry.delete_credential();
    true
}
