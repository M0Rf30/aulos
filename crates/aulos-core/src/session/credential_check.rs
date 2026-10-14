// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The startup keyring check: verifies that server passwords flagged as
//! stored in the system keyring still exist there and migrates plaintext
//! passwords into it. The keyring costs secret-service D-Bus round trips
//! (50-500 ms, or an unlock prompt), so the check runs off the UI thread
//! after the first frame and reports the config changes it wants applied.

use crate::config::{Config, MpdConfigEntry, SubsonicConfigEntry};

/// Which kind of provider entry a [`CredentialUpdate`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Mpd,
    Subsonic,
}

/// A change the background keyring check wants applied to the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialUpdate {
    /// The plaintext password was moved into the keyring.
    Migrated(CredentialKind, String),
    /// The entry claimed a keyring password that no longer exists.
    Lost(CredentialKind, String),
}

/// Whether any configured server has a password the keyring check would have
/// to look at (stored in the keyring, or still plaintext). With none, the
/// keyring is not touched at startup at all.
pub(super) fn needs_credential_check(config: &Config) -> bool {
    config
        .mpd_servers
        .iter()
        .any(|e| e.password_in_keyring || e.password.is_some())
        || config
            .subsonic_servers
            .iter()
            .any(|e| e.password_in_keyring || e.password.is_some())
}

/// Verify/migrate one entry's password; returns the config change, if any.
fn check_credential(
    kind: CredentialKind,
    id: &str,
    in_keyring: bool,
    plaintext: Option<&str>,
    retrieve: &impl Fn(&str) -> Result<Option<String>, String>,
    store: &impl Fn(&str, &str) -> Result<(), String>,
) -> Option<CredentialUpdate> {
    if in_keyring {
        // Verify the keyring entry still exists; reset if lost.
        match retrieve(id) {
            Ok(None) => {
                tracing::warn!(
                    "{kind:?} password for '{id}' was marked as stored in keyring \
                     but the entry is missing; resetting so user can re-enter."
                );
                Some(CredentialUpdate::Lost(kind, id.to_string()))
            }
            Err(e) => {
                tracing::warn!("Failed to verify keyring entry for {kind:?} '{id}': {e}");
                None
            }
            Ok(Some(_)) => None,
        }
    } else if let Some(pw) = plaintext {
        match store(id, pw) {
            Ok(()) => {
                tracing::info!("Migrated {kind:?} password for '{id}' to system keyring");
                Some(CredentialUpdate::Migrated(kind, id.to_string()))
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to migrate {kind:?} password for '{id}' to keyring, \
                     keeping plaintext in config: {e}"
                );
                None
            }
        }
    } else {
        None
    }
}

/// Blocking: probe the keyring, then verify/migrate every server password.
/// The keyring operations are injected so the logic is testable without a
/// secret service.
pub(super) fn run_credential_check(
    mpd_servers: &[MpdConfigEntry],
    subsonic_servers: &[SubsonicConfigEntry],
    keyring_available: impl Fn() -> bool,
    retrieve: impl Fn(&str) -> Result<Option<String>, String>,
    store: impl Fn(&str, &str) -> Result<(), String>,
) -> Vec<CredentialUpdate> {
    if !keyring_available() {
        tracing::warn!(
            "System keyring is not available; passwords will remain in plaintext config"
        );
        return Vec::new();
    }
    let mpd = mpd_servers.iter().filter_map(|e| {
        check_credential(
            CredentialKind::Mpd,
            &e.id,
            e.password_in_keyring,
            e.password.as_deref(),
            &retrieve,
            &store,
        )
    });
    let subsonic = subsonic_servers.iter().filter_map(|e| {
        check_credential(
            CredentialKind::Subsonic,
            &e.id,
            e.password_in_keyring,
            e.password.as_deref(),
            &retrieve,
            &store,
        )
    });
    mpd.chain(subsonic).collect()
}

/// Apply the background check's results to `config`; `true` if anything
/// changed (the caller then persists). Each update is only applied if the
/// entry is still in the state the check saw, so a server the user edited
/// meanwhile is left alone.
pub(super) fn apply_credential_updates(config: &mut Config, updates: &[CredentialUpdate]) -> bool {
    let mut changed = false;
    for update in updates {
        match update {
            CredentialUpdate::Migrated(CredentialKind::Mpd, id) => {
                if let Some(e) = config
                    .mpd_servers
                    .iter_mut()
                    .find(|e| &e.id == id && !e.password_in_keyring && e.password.is_some())
                {
                    e.password_in_keyring = true;
                    e.password = None;
                    changed = true;
                }
            }
            CredentialUpdate::Migrated(CredentialKind::Subsonic, id) => {
                if let Some(e) = config
                    .subsonic_servers
                    .iter_mut()
                    .find(|e| &e.id == id && !e.password_in_keyring && e.password.is_some())
                {
                    e.password_in_keyring = true;
                    e.password = None;
                    changed = true;
                }
            }
            CredentialUpdate::Lost(CredentialKind::Mpd, id) => {
                if let Some(e) = config
                    .mpd_servers
                    .iter_mut()
                    .find(|e| &e.id == id && e.password_in_keyring)
                {
                    e.password_in_keyring = false;
                    changed = true;
                }
            }
            CredentialUpdate::Lost(CredentialKind::Subsonic, id) => {
                if let Some(e) = config
                    .subsonic_servers
                    .iter_mut()
                    .find(|e| &e.id == id && e.password_in_keyring)
                {
                    e.password_in_keyring = false;
                    changed = true;
                }
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn mpd(id: &str, in_keyring: bool, password: Option<&str>) -> MpdConfigEntry {
        MpdConfigEntry {
            id: id.into(),
            password_in_keyring: in_keyring,
            password: password.map(String::from),
            ..Default::default()
        }
    }

    fn subsonic(id: &str, in_keyring: bool, password: Option<&str>) -> SubsonicConfigEntry {
        SubsonicConfigEntry {
            id: id.into(),
            password_in_keyring: in_keyring,
            password: password.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn no_passwords_means_no_keyring_access() {
        let mut config = Config::default();
        assert!(!needs_credential_check(&config));
        config.mpd_servers.push(mpd("a", false, None));
        assert!(!needs_credential_check(&config));
        config
            .subsonic_servers
            .push(subsonic("b", false, Some("pw")));
        assert!(needs_credential_check(&config));
    }

    #[test]
    fn unavailable_keyring_changes_nothing() {
        let stored = RefCell::new(Vec::new());
        let updates = run_credential_check(
            &[mpd("a", false, Some("pw"))],
            &[],
            || false,
            |_| Ok(None),
            |id, _| {
                stored.borrow_mut().push(id.to_string());
                Ok(())
            },
        );
        assert!(updates.is_empty());
        assert!(stored.borrow().is_empty());
    }

    #[test]
    fn migrates_plaintext_and_flags_lost_entries() {
        let updates = run_credential_check(
            &[mpd("plain", false, Some("pw")), mpd("ok", true, None)],
            &[
                subsonic("gone", true, None),
                subsonic("fail", false, Some("x")),
            ],
            || true,
            |id| Ok((id != "gone").then(|| "secret".to_string())),
            |id, _| {
                if id == "fail" {
                    Err("locked".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            updates,
            vec![
                CredentialUpdate::Migrated(CredentialKind::Mpd, "plain".into()),
                CredentialUpdate::Lost(CredentialKind::Subsonic, "gone".into()),
            ]
        );
    }

    #[test]
    fn updates_apply_only_to_entries_still_in_the_checked_state() {
        let mut config = Config::default();
        config.mpd_servers.push(mpd("plain", false, Some("pw")));
        config.mpd_servers.push(mpd("edited", true, None));
        config.subsonic_servers.push(subsonic("gone", true, None));
        let updates = [
            CredentialUpdate::Migrated(CredentialKind::Mpd, "plain".into()),
            // User re-saved this one into the keyring meanwhile: no plaintext
            // left, so a stale "migrated" result must not be applied.
            CredentialUpdate::Migrated(CredentialKind::Mpd, "edited".into()),
            CredentialUpdate::Lost(CredentialKind::Subsonic, "gone".into()),
            CredentialUpdate::Lost(CredentialKind::Subsonic, "unknown".into()),
        ];
        assert!(apply_credential_updates(&mut config, &updates));
        assert!(config.mpd_servers[0].password_in_keyring);
        assert!(config.mpd_servers[0].password.is_none());
        assert!(config.mpd_servers[1].password_in_keyring);
        assert!(!config.subsonic_servers[0].password_in_keyring);
        assert!(!apply_credential_updates(&mut config, &updates));
    }
}
