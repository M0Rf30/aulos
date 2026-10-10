// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Persistent offline scrobble queue.
//!
//! Every scrobble is queued per service and removed only after the service
//! accepted it, so nothing is lost across offline periods or restarts. The
//! queue is a small JSON file in the data dir (`scrobble_queue.json`).

use super::{ScrobbleTrack, Service};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Hard cap on queued listens; the oldest are dropped beyond it.
pub const MAX_QUEUED: usize = 5000;
/// Last.fm rejects listens older than 14 days; don't keep them forever.
pub const MAX_AGE_SECS: i64 = 14 * 24 * 3600;

const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedScrobble {
    pub service: Service,
    pub track: ScrobbleTrack,
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    items: Vec<QueuedScrobble>,
}

#[derive(Debug, Default)]
pub struct Queue {
    path: Option<PathBuf>,
    items: Vec<QueuedScrobble>,
}

/// Default location of the queue file.
pub fn default_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aulos")
        .join("scrobble_queue.json")
}

impl Queue {
    /// An in-memory queue that is never written to disk.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load the queue from `path`. A missing or corrupt file yields an
    /// empty queue (a corrupt one is left in place until the next save).
    pub fn load(path: &Path) -> Self {
        let items = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<FileFormat>(&bytes).ok())
            .filter(|f| f.version == FILE_VERSION)
            .map(|f| f.items)
            .unwrap_or_default();
        Self {
            path: Some(path.to_path_buf()),
            items,
        }
    }

    /// Write the queue to disk atomically (temp file + rename).
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let data = serde_json::to_vec(&FileFormat {
            version: FILE_VERSION,
            items: self.items.clone(),
        })
        .map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, path)
    }

    pub fn push(&mut self, service: Service, track: ScrobbleTrack) {
        self.items.push(QueuedScrobble { service, track });
        if self.items.len() > MAX_QUEUED {
            let excess = self.items.len() - MAX_QUEUED;
            self.items.drain(..excess);
        }
    }

    /// Up to `max` oldest queued listens for `service`.
    pub fn pending(&self, service: Service, max: usize) -> Vec<ScrobbleTrack> {
        self.items
            .iter()
            .filter(|q| q.service == service)
            .take(max)
            .map(|q| q.track.clone())
            .collect()
    }

    /// Remove the `n` oldest listens of `service`.
    pub fn remove_front(&mut self, service: Service, n: usize) {
        let mut left = n;
        self.items.retain(|q| {
            if left > 0 && q.service == service {
                left -= 1;
                false
            } else {
                true
            }
        });
    }

    /// Drop everything queued for `service`.
    pub fn purge(&mut self, service: Service) {
        self.items.retain(|q| q.service != service);
    }

    /// Drop listens older than [`MAX_AGE_SECS`]. Returns how many.
    pub fn prune(&mut self, now: i64) -> usize {
        let before = self.items.len();
        self.items
            .retain(|q| q.track.timestamp == 0 || now - q.track.timestamp <= MAX_AGE_SECS);
        before - self.items.len()
    }

    pub fn count(&self, service: Service) -> usize {
        self.items.iter().filter(|q| q.service == service).count()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, ts: i64) -> ScrobbleTrack {
        ScrobbleTrack {
            artist: "Artist".into(),
            title: title.into(),
            album: "Album".into(),
            album_artist: String::new(),
            duration_secs: 200,
            track_number: 1,
            timestamp: ts,
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("aulos-scrobble-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn persists_and_reloads() {
        let dir = temp_path("persist");
        let path = dir.join("queue.json");
        let mut q = Queue::load(&path);
        assert!(q.is_empty());
        q.push(Service::LastFm, track("one", 100));
        q.push(Service::ListenBrainz, track("two", 200));
        q.save().unwrap();

        let q2 = Queue::load(&path);
        assert_eq!(q2.len(), 2);
        assert_eq!(q2.pending(Service::LastFm, 10), vec![track("one", 100)]);
        assert_eq!(q2.count(Service::ListenBrainz), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_file_yields_empty_queue() {
        let dir = temp_path("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queue.json");
        std::fs::write(&path, b"{not json").unwrap();
        let mut q = Queue::load(&path);
        assert!(q.is_empty());
        q.push(Service::LibreFm, track("x", 1));
        q.save().unwrap();
        assert_eq!(Queue::load(&path).len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_front_is_per_service_and_ordered() {
        let mut q = Queue::in_memory();
        q.push(Service::LastFm, track("a", 1));
        q.push(Service::LibreFm, track("b", 2));
        q.push(Service::LastFm, track("c", 3));
        q.push(Service::LastFm, track("d", 4));
        q.remove_front(Service::LastFm, 2);
        assert_eq!(
            q.pending(Service::LastFm, 10),
            vec![track("d", 4)],
            "oldest two lastfm removed"
        );
        assert_eq!(q.count(Service::LibreFm), 1);
    }

    #[test]
    fn pending_respects_limit_and_purge() {
        let mut q = Queue::in_memory();
        for i in 0..5 {
            q.push(Service::ListenBrainz, track("t", i));
        }
        assert_eq!(q.pending(Service::ListenBrainz, 3).len(), 3);
        q.purge(Service::ListenBrainz);
        assert!(q.is_empty());
    }

    #[test]
    fn prune_drops_old_entries() {
        let mut q = Queue::in_memory();
        let now = 10_000_000;
        q.push(Service::LastFm, track("old", now - MAX_AGE_SECS - 1));
        q.push(Service::LastFm, track("new", now - 10));
        assert_eq!(q.prune(now), 1);
        assert_eq!(q.pending(Service::LastFm, 10), vec![track("new", now - 10)]);
    }

    #[test]
    fn capacity_drops_oldest() {
        let mut q = Queue::in_memory();
        for i in 0..(MAX_QUEUED + 3) {
            q.push(Service::LastFm, track("t", i as i64 + 1));
        }
        assert_eq!(q.len(), MAX_QUEUED);
        assert_eq!(q.pending(Service::LastFm, 1)[0].timestamp, 4);
    }
}
