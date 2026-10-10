// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Background, coalescing persistence of the whole [`Config`].
//!
//! cosmic-config stores every field as its own file and `write_entry`
//! rewrites all of them atomically (temp file + fsync + rename each) — about
//! 50 fsyncs, measured at ~425 ms on ext4. Doing that on the UI thread froze
//! the app on every section switch and on every slider step. Saves are now
//! handed to a dedicated thread that:
//! - waits [`DEBOUNCE`] for the burst to settle and writes only the latest
//!   snapshot (a slider drag or rapid page switching = one write);
//! - skips the write entirely when nothing changed since the last one;
//! - writes only the keys that differ from what is on disk when it can tell
//!   (see [`Config::write_changed`]).
//!
//! [`ConfigWriter::flush`] blocks until pending writes are on disk; call it
//! before the process exits.

use crate::config::Config;
use cosmic::cosmic_config;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::time::Duration;

/// Quiet period before a burst of saves is written.
const DEBOUNCE: Duration = Duration::from_millis(400);

enum Command {
    Save(Box<Config>),
    Flush(SyncSender<()>),
}

#[derive(Clone)]
pub struct ConfigWriter {
    tx: Sender<Command>,
}

impl std::fmt::Debug for ConfigWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConfigWriter")
    }
}

impl ConfigWriter {
    /// Start the writer thread. `on_disk` is the config as currently stored,
    /// used as the baseline for change detection.
    pub fn spawn(context: cosmic_config::Config, on_disk: Config) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("aulos-config".into())
            .spawn(move || run(&context, on_disk, &rx))
            .map_err(|e| tracing::error!("config writer thread not spawned: {e}"))
            .ok()?;
        Some(Self { tx })
    }

    /// Queue `config` to be persisted (non-blocking).
    pub fn save(&self, config: &Config) {
        let _ = self.tx.send(Command::Save(Box::new(config.clone())));
    }

    /// Block until every queued save has been written (bounded wait).
    pub fn flush(&self) {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        if self.tx.send(Command::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(5));
        }
    }
}

fn run(context: &cosmic_config::Config, mut written: Config, rx: &Receiver<Command>) {
    let mut pending: Option<Box<Config>> = None;
    loop {
        // Idle: block until something arrives. Pending: wait out the
        // debounce window, collapsing newer saves into the latest one.
        let next = if pending.is_some() {
            rx.recv_timeout(DEBOUNCE)
        } else {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        match next {
            Ok(Command::Save(config)) => pending = Some(config),
            Ok(Command::Flush(ack)) => {
                if let Some(config) = pending.take() {
                    write(context, &mut written, *config);
                }
                let _ = ack.send(());
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(config) = pending.take() {
                    write(context, &mut written, *config);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(config) = pending.take() {
                    write(context, &mut written, *config);
                }
                return;
            }
        }
    }
}

fn write(context: &cosmic_config::Config, written: &mut Config, config: Config) {
    if *written == config {
        return;
    }
    let start = std::time::Instant::now();
    match config.write_changed(context, written) {
        Ok(keys) => tracing::debug!(
            "config saved ({keys} key(s)) in {:.1} ms",
            start.elapsed().as_secs_f64() * 1000.0
        ),
        Err(e) => tracing::error!("Failed to save config: {e:?}"),
    }
    *written = config;
}
