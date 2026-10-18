// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Background, coalescing persistence of the whole [`Config`].
//!
//! Writing the settings means serialising them, fsyncing a temporary file
//! and renaming it over `settings.ron`; doing that on the UI thread on every
//! section switch and slider step would stall it. Saves are instead handed
//! to a dedicated thread that:
//! - waits [`DEBOUNCE`] for the burst to settle and writes only the latest
//!   snapshot (a slider drag or rapid page switching = one write);
//! - skips the write entirely when nothing changed since the last one.
//!
//! [`ConfigWriter::flush`] blocks until pending writes are on disk; call it
//! before the process exits.

use super::Config;
use std::path::{Path, PathBuf};
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
    /// Start the writer thread persisting to `path`. `on_disk` is the config
    /// as currently stored there, used as the baseline for change detection.
    pub fn spawn(path: PathBuf, on_disk: Config) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("aulos-config".into())
            .spawn(move || run(&path, on_disk, &rx))
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

fn run(path: &Path, mut written: Config, rx: &Receiver<Command>) {
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
                    write(path, &mut written, *config);
                }
                let _ = ack.send(());
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(config) = pending.take() {
                    write(path, &mut written, *config);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                if let Some(config) = pending.take() {
                    write(path, &mut written, *config);
                }
                return;
            }
        }
    }
}

fn write(path: &Path, written: &mut Config, config: Config) {
    if *written == config {
        return;
    }
    let start = web_time::Instant::now();
    match config.save_to(path) {
        Ok(()) => {
            tracing::debug!(
                "config saved in {:.1} ms",
                start.elapsed().as_secs_f64() * 1000.0
            );
            *written = config;
        }
        // Keep the old baseline so the next save of this config retries.
        Err(e) => tracing::error!("Failed to save config: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aulos-writer-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("settings.ron")
    }

    fn with_volume(volume: f32) -> Config {
        Config {
            volume,
            ..Config::default()
        }
    }

    #[test]
    fn flush_writes_the_latest_snapshot_only() {
        let path = path("coalesce");
        let writer = ConfigWriter::spawn(path.clone(), Config::default()).unwrap();
        for step in 1..=20u8 {
            writer.save(&with_volume(f32::from(step) / 100.0));
        }
        writer.flush();
        assert_eq!(Config::load_from(&path), with_volume(0.2));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn saves_are_debounced_not_written_immediately() {
        let path = path("debounce");
        let writer = ConfigWriter::spawn(path.clone(), Config::default()).unwrap();
        writer.save(&with_volume(0.3));
        // Well inside the debounce window nothing is on disk yet.
        assert!(!path.exists());
        std::thread::sleep(DEBOUNCE * 3);
        assert_eq!(Config::load_from(&path), with_volume(0.3));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_unchanged_config_is_not_written() {
        let path = path("unchanged");
        let writer = ConfigWriter::spawn(path.clone(), Config::default()).unwrap();
        writer.save(&Config::default());
        writer.flush();
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_failed_write_is_retried_on_the_next_save() {
        let path = path("retry");
        // A directory squatting on the target makes the rename fail.
        std::fs::create_dir_all(path.join("blocker")).unwrap();
        let writer = ConfigWriter::spawn(path.clone(), Config::default()).unwrap();
        writer.save(&with_volume(0.5));
        writer.flush();
        assert!(path.is_dir());
        std::fs::remove_dir_all(&path).unwrap();
        // The identical config must be written now, not skipped as unchanged.
        writer.save(&with_volume(0.5));
        writer.flush();
        assert_eq!(Config::load_from(&path), with_volume(0.5));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn dropping_the_writer_writes_pending_saves() {
        let path = path("drop");
        let writer = ConfigWriter::spawn(path.clone(), Config::default()).unwrap();
        writer.save(&with_volume(0.4));
        drop(writer);
        for _ in 0..50 {
            if path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(Config::load_from(&path), with_volume(0.4));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
