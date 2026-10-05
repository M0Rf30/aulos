// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Secure credential storage using the system keyring.
//!
//! Passwords for MPD and Subsonic providers are stored in the platform's
//! native credential store (e.g., GNOME Keyring, KDE Wallet, macOS Keychain)
//! via the `keyring-core` crate. Each password is keyed by `(SERVICE, provider_id)`.

use std::sync::Once;

const SERVICE: &str = "io.github.m0rf30.Lyra";

static INIT: Once = Once::new();

/// Select the default credential store, once, before any [`keyring_core::Entry`] is created.
///
/// `keyring-core` requires a default credential store to be set globally before
/// `Entry::new` can succeed. The `keyring` facade crate only wires up the D-Bus
/// secret service backend on Linux, so we build the store stack ourselves here,
/// preferring the Linux kernel keyutils store and falling back to the D-Bus
/// secret service when keyutils is unavailable. This restores the old keyring v3
/// default behavior.
///
/// Safe to call multiple times (and from multiple threads/functions): the actual
/// selection only happens on the first call, via [`std::sync::Once`]. If both
/// backends fail to initialize, no default store is set and subsequent
/// `Entry::new` calls will error; callers already treat keyring errors as
/// "keyring unavailable" degraded mode.
pub fn init() {
    INIT.call_once(|| {
        let store = linux_keyutils_keyring_store::Store::new()
            .map(|store| store as std::sync::Arc<keyring_core::api::CredentialStore>)
            .or_else(|_| {
                zbus_secret_service_keyring_store::Store::new()
                    .map(|store| store as std::sync::Arc<keyring_core::api::CredentialStore>)
            });
        if let Ok(store) = store {
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
        Err(keyring_core::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!(
            "Failed to retrieve password for '{provider_id}': {e}"
        )),
    }
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
    const PROBE_USER: &str = "__lyra_keyring_probe__";
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
