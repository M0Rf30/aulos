// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! A `keyring-core` credential store backed by files in the app's private
//! directory.
//!
//! Android has no Secret Service or keyutils. The app-private directory is
//! readable by this app only (and encrypted at rest by the device's
//! file-based encryption), but the passwords are **not** encrypted on top of
//! that: one file per `(service, user)` under `<data>/credentials/`, mode
//! `0600`.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use keyring_core::api::{CredentialApi, CredentialPersistence, CredentialStoreApi};
use keyring_core::{Credential, Entry, Error, Result};

/// A filesystem failure, carried inside the keyring error types.
#[derive(Debug)]
struct StorageError(String);

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StorageError {}

fn failure(what: &str, error: &io::Error) -> Error {
    Error::PlatformFailure(Box::new(StorageError(format!("{what}: {error}"))))
}

/// The file name of one credential: the hex of `service/user`, so no
/// character of either can escape the directory.
fn file_name(service: &str, user: &str) -> String {
    format!("{service}/{user}")
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One `(service, user)` file.
struct FileCredential {
    service: String,
    user: String,
    dir: PathBuf,
}

impl fmt::Debug for FileCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never prints the secret.
        f.debug_struct("FileCredential")
            .field("service", &self.service)
            .field("user", &self.user)
            .finish()
    }
}

impl FileCredential {
    fn path(&self) -> PathBuf {
        self.dir.join(file_name(&self.service, &self.user))
    }

    fn read(&self) -> Result<Vec<u8>> {
        std::fs::read(self.path()).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Error::NoEntry,
            _ => failure("read", &e),
        })
    }

    fn write(&self, secret: &[u8]) -> Result<()> {
        // Written beside the target and renamed: a crash never leaves half a
        // password behind.
        crate::fs_atomic::write(&self.path(), secret, true).map_err(|e| failure("write", &e))
    }
}

impl CredentialApi for FileCredential {
    fn set_password(&self, password: &str) -> Result<()> {
        self.write(password.as_bytes())
    }

    fn set_secret(&self, secret: &[u8]) -> Result<()> {
        self.write(secret)
    }

    fn get_password(&self) -> Result<String> {
        String::from_utf8(self.read()?).map_err(|_| Error::BadEncoding(Vec::new()))
    }

    fn get_secret(&self) -> Result<Vec<u8>> {
        self.read()
    }

    fn delete_credential(&self) -> Result<()> {
        std::fs::remove_file(self.path()).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Error::NoEntry,
            _ => failure("delete", &e),
        })
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

/// The file credential store.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// A store keeping its files in `dir`.
    pub fn new(dir: PathBuf) -> Arc<Self> {
        Arc::new(Self { dir })
    }
}

impl CredentialStoreApi for Store {
    fn vendor(&self) -> String {
        "Aulos file store (app-private files, unencrypted)".to_string()
    }

    fn id(&self) -> String {
        "aulos-file-store".to_string()
    }

    fn build(
        &self,
        service: &str,
        user: &str,
        mods: Option<&HashMap<&str, &str>>,
    ) -> Result<Entry> {
        if mods.is_some_and(|m| !m.is_empty()) {
            return Err(Error::NotSupportedByStore(
                "the file store does not take entry modifiers".to_string(),
            ));
        }
        Ok(Entry::new_with_credential(Arc::new(FileCredential {
            service: service.to_string(),
            user: user.to_string(),
            dir: self.dir.clone(),
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

    /// A store in a fresh directory (removed on drop).
    struct Fixture {
        root: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn store(tag: &str) -> (Fixture, Arc<Store>) {
        let root = std::env::temp_dir().join(format!("aulos-cred-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::new(root.join("credentials"));
        (Fixture { root }, store)
    }

    #[test]
    fn a_password_round_trips_and_deletes() {
        let (_dir, store) = store("roundtrip");
        let entry = store.build("svc", "provider/1", None).unwrap();
        assert!(matches!(entry.get_password(), Err(Error::NoEntry)));
        entry.set_password("s3cret").unwrap();
        assert_eq!(entry.get_password().unwrap(), "s3cret");
        entry.set_password("changed").unwrap();
        assert_eq!(entry.get_password().unwrap(), "changed");
        entry.delete_credential().unwrap();
        assert!(matches!(entry.get_password(), Err(Error::NoEntry)));
        assert!(matches!(entry.delete_credential(), Err(Error::NoEntry)));
    }

    #[test]
    fn users_that_look_like_paths_stay_inside_the_directory() {
        let (dir, store) = store("escape");
        let entry = store.build("svc", "../../escape", None).unwrap();
        entry.set_password("x").unwrap();
        let files: Vec<_> = std::fs::read_dir(dir.root.join("credentials"))
            .unwrap()
            .collect();
        assert_eq!(files.len(), 1);
        assert!(!dir.root.join("escape").exists());
    }
}
