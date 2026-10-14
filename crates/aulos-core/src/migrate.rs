// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! One-shot migration of on-disk state from the app's former name, Lyra.
//!
//! Moves each legacy directory to its new location, but only when the new
//! one does not exist yet, so it never overwrites state created under the
//! new name and is a no-op on every launch after the first. Keyring entries
//! are migrated lazily in [`crate::credentials`].

use std::path::{Path, PathBuf};

const LEGACY_NAME: &str = "lyra";
const NEW_NAME: &str = "aulos";
const LEGACY_APP_ID: &str = "io.github.m0rf30.Lyra";
const NEW_APP_ID: &str = "io.github.m0rf30.Aulos";

/// Rename legacy Lyra data, cache, config and COSMIC config/state
/// directories to their Aulos equivalents. Call once at startup, before
/// anything opens the library database or reads the config.
pub fn migrate_legacy_dirs() {
    for (from, to) in legacy_dir_pairs() {
        migrate_dir(&from, &to);
    }
}

fn legacy_dir_pairs() -> Vec<(PathBuf, PathBuf)> {
    let mut pairs = Vec::new();
    for base in [dirs::data_dir(), dirs::cache_dir(), dirs::config_dir()]
        .into_iter()
        .flatten()
    {
        pairs.push((base.join(LEGACY_NAME), base.join(NEW_NAME)));
    }
    // cosmic-config keys its config and state trees by app id.
    for base in [dirs::config_dir(), dirs::state_dir()]
        .into_iter()
        .flatten()
    {
        let cosmic = base.join("cosmic");
        pairs.push((cosmic.join(LEGACY_APP_ID), cosmic.join(NEW_APP_ID)));
    }
    pairs
}

fn migrate_dir(from: &Path, to: &Path) {
    if !from.is_dir() || to.exists() {
        return;
    }
    match std::fs::rename(from, to) {
        Ok(()) => tracing::info!("Migrated {} -> {}", from.display(), to.display()),
        Err(e) => tracing::warn!(
            "Could not migrate {} -> {}: {e}",
            from.display(),
            to.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::migrate_dir;

    #[test]
    fn moves_only_when_target_absent() {
        let root = std::env::temp_dir().join(format!("aulos-migrate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let from = root.join("lyra");
        let to = root.join("aulos");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("library.db"), b"old").unwrap();

        migrate_dir(&from, &to);
        assert!(!from.exists());
        assert_eq!(std::fs::read(to.join("library.db")).unwrap(), b"old");

        // A second legacy dir must not clobber the already-migrated one.
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("library.db"), b"stale").unwrap();
        migrate_dir(&from, &to);
        assert!(from.exists());
        assert_eq!(std::fs::read(to.join("library.db")).unwrap(), b"old");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
