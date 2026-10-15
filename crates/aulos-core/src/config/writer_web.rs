// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Persistence of the whole [`Config`] in the browser's `localStorage`.
//!
//! The same surface as the desktop writer (`writer.rs`). There is no thread
//! and no disk to wait for, so a save is written on the spot, skipped when
//! nothing changed since the last write; [`ConfigWriter::flush`] has nothing
//! left to do.

use super::Config;
use crate::web_storage;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct ConfigWriter {
    /// The config as last stored, the baseline for change detection.
    written: Arc<Mutex<Config>>,
}

impl std::fmt::Debug for ConfigWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConfigWriter")
    }
}

impl ConfigWriter {
    /// A writer whose baseline is `stored`, the config as currently stored.
    #[must_use]
    pub fn new(stored: Config) -> Self {
        Self {
            written: Arc::new(Mutex::new(stored)),
        }
    }

    /// Store `config` (unless it equals what is stored).
    pub fn save(&self, config: &Config) {
        let mut written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        if *written == *config {
            return;
        }
        let stored = config
            .to_text()
            .map_err(|e| e.to_string())
            .and_then(|text| web_storage::set(web_storage::SETTINGS_KEY, &text));
        match stored {
            Ok(()) => *written = config.clone(),
            Err(e) => tracing::error!("Failed to save config: {e}"),
        }
    }

    /// Every save is already stored.
    pub fn flush(&self) {}
}
