// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Live progress of a library scan: how many of the audio files found were
//! processed, and which music folder is being walked.
//!
//! The scanner runs on a blocking thread and updates a shared
//! [`ScanProgress`]; a UI reads [`ScanProgress::snapshot`] every frame while
//! `LibraryState::scanning` is set, so no events cross threads per file.

use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Shared counters of the running scan.
#[derive(Debug, Default)]
pub struct ScanProgress {
    done: AtomicUsize,
    total: AtomicUsize,
    root: Mutex<String>,
}

/// A consistent-enough reading of a [`ScanProgress`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanSnapshot {
    /// Audio files processed so far.
    pub done: usize,
    /// Audio files found; `0` while the folders are still being counted.
    pub total: usize,
    /// The music folder being scanned, with the home directory shown as
    /// `~` (e.g. `~/Music`); empty when no scan has started.
    pub root: String,
}

impl ScanSnapshot {
    /// Fraction done in `0.0..=1.0`, or `None` while the total is unknown.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f32 / self.total as f32).min(1.0))
    }
}

impl ScanProgress {
    /// Forget the previous scan: nothing counted, no folder.
    pub fn reset(&self) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
        self.root.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// The folder now being scanned.
    pub fn set_root(&self, root: &Path) {
        *self.root.lock().unwrap_or_else(|e| e.into_inner()) = tilde(root);
    }

    /// `n` more audio files were found.
    pub fn add_total(&self, n: usize) {
        self.total.fetch_add(n, Ordering::Relaxed);
    }

    /// One more audio file was processed.
    pub fn advance(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    /// The current counters.
    #[must_use]
    pub fn snapshot(&self) -> ScanSnapshot {
        ScanSnapshot {
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            root: self.root.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        }
    }
}

/// `path` with the home directory replaced by `~`.
fn tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_fraction() {
        let p = ScanProgress::default();
        assert_eq!(p.snapshot().fraction(), None);
        p.add_total(4);
        p.advance();
        let s = p.snapshot();
        assert_eq!((s.done, s.total), (1, 4));
        assert_eq!(s.fraction(), Some(0.25));
        p.reset();
        assert_eq!(p.snapshot(), ScanSnapshot::default());
    }

    #[test]
    fn root_shows_the_home_directory_as_tilde() {
        let p = ScanProgress::default();
        if let Some(home) = dirs::home_dir() {
            p.set_root(&home.join("Music"));
            assert_eq!(p.snapshot().root, "~/Music");
            p.set_root(&home);
            assert_eq!(p.snapshot().root, "~");
        }
        p.set_root(Path::new("/srv/music"));
        assert_eq!(p.snapshot().root, "/srv/music");
    }
}
