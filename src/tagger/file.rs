// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! One file loaded into the tag editor.

use crate::library::fingerprint::Fingerprint;
use crate::library::tagio::{Container, FileTags, TagSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Identity of a loaded file inside one editor session. Never reused, so a
/// late async result for a removed file can't land on another one.
pub type FileId = u64;

/// Where a file stands relative to disk.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FileStatus {
    /// `edited` equals what is on disk.
    #[default]
    Unchanged,
    /// `edited` differs from what is on disk.
    Modified,
    /// A write is in flight; the file is locked against edits.
    Saving,
    /// The last save succeeded (until the next edit).
    Saved,
    /// The last save failed; the message says why. The edits are kept so
    /// the save can be retried.
    Error(String),
}

/// The release track a file was matched to by the last applied release.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackAssignment {
    /// Index into the release's media.
    pub medium: usize,
    /// Index into that medium's tracks.
    pub track: usize,
    /// Match similarity in `0.0..=1.0` (1.0 for an identified recording).
    pub score: f32,
    /// Position label: `"03"`, or `"2-03"` on a multi-disc release.
    pub number: String,
    pub title: String,
}

/// The AcoustID a file's fingerprint resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcoustIdHit {
    pub acoustid: String,
    /// MusicBrainz recording MBIDs the AcoustID maps to (usually one).
    pub recording_ids: Vec<String>,
}

/// Where a file's fingerprint submission stands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SubmissionState {
    #[default]
    NotSubmitted,
    /// Being fingerprinted / sent.
    Pending,
    /// AcoustID accepted it; `status` is its processing state
    /// (`pending` / `imported`).
    Submitted {
        status: String,
    },
    Failed(String),
}

/// A file in the editor.
#[derive(Debug, Clone)]
pub struct TagFile {
    pub id: FileId,
    pub path: PathBuf,
    /// `None` when the file could not be read.
    pub container: Option<Container>,
    /// The tags as they are on disk.
    pub original: TagSet,
    /// The tags being built; written on save.
    pub edited: TagSet,
    pub duration_secs: Option<f64>,
    pub status: FileStatus,
    /// Why the file could not be read; such files are listed read-only.
    pub read_error: Option<String>,
    /// The release track the last applied release matched this file to.
    pub assignment: Option<TrackAssignment>,
    /// Computed by an AcoustID scan or a submission.
    pub fingerprint: Option<Arc<Fingerprint>>,
    pub acoustid: Option<AcoustIdHit>,
    pub submission: SubmissionState,
}

impl TagFile {
    /// Builds the file from the result of reading it.
    #[must_use]
    pub fn new(
        id: FileId,
        path: PathBuf,
        read: Result<FileTags, String>,
        duration_secs: Option<f64>,
    ) -> Self {
        let (container, original, read_error) = match read {
            Ok(FileTags { container, tags }) => (Some(container), tags, None),
            Err(error) => (None, TagSet::new(), Some(error)),
        };
        Self {
            id,
            path,
            container,
            edited: original.clone(),
            original,
            duration_secs,
            status: FileStatus::Unchanged,
            read_error,
            assignment: None,
            fingerprint: None,
            acoustid: None,
            submission: SubmissionState::NotSubmitted,
        }
    }

    /// The file name for display.
    #[must_use]
    pub fn file_name(&self) -> String {
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.path.parent()
    }

    /// Whether the editor may change and write this file: it was read and
    /// its container is one the tag writer supports.
    #[must_use]
    pub fn is_editable(&self) -> bool {
        self.read_error.is_none() && self.container.is_some_and(Container::is_writable)
    }

    /// The reason the file is read-only, when it is.
    #[must_use]
    pub fn read_only_reason(&self) -> Option<String> {
        if let Some(error) = &self.read_error {
            return Some(error.clone());
        }
        match self.container {
            Some(c) if !c.is_writable() => Some("this file format can't be written".to_owned()),
            _ => None,
        }
    }

    /// Whether `edited` differs from `original`.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.edited != self.original
    }

    /// Recomputes the status after `edited` changed. A write in flight
    /// keeps its status; anything else becomes `Unchanged`/`Modified`.
    pub fn refresh_status(&mut self) {
        if self.status == FileStatus::Saving {
            return;
        }
        self.status = if self.is_dirty() {
            FileStatus::Modified
        } else {
            FileStatus::Unchanged
        };
    }

    /// Whether this file has edits that would be lost by dropping it (the
    /// status is kept up to date by every edit, so this never compares tags).
    #[must_use]
    pub fn has_unsaved_edits(&self) -> bool {
        matches!(self.status, FileStatus::Modified | FileStatus::Error(_))
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::library::tagio::keys;

    /// A writable (FLAC) file with the given tags.
    pub fn file(id: FileId, tags: &[(&str, &str)]) -> TagFile {
        let mut set = TagSet::new();
        for (key, value) in tags {
            set.push(key, *value);
        }
        TagFile::new(
            id,
            PathBuf::from(format!("/music/{id:02}.flac")),
            Ok(FileTags {
                container: Container::Flac,
                tags: set,
            }),
            Some(200.0),
        )
    }

    /// A file of `album` by `artist`.
    pub fn track(id: FileId, artist: &str, album: &str, number: u32, title: &str) -> TagFile {
        let number = number.to_string();
        file(
            id,
            &[
                (keys::ALBUMARTIST, artist),
                (keys::ALBUM, album),
                (keys::TRACKNUMBER, number.as_str()),
                (keys::TITLE, title),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::library::tagio::keys;

    #[test]
    fn unreadable_files_are_read_only_and_empty() {
        let f = TagFile::new(
            1,
            PathBuf::from("/x.xyz"),
            Err("unsupported file: x".into()),
            None,
        );
        assert!(!f.is_editable());
        assert_eq!(f.read_only_reason().as_deref(), Some("unsupported file: x"));
        assert!(f.original.is_empty());
    }

    #[test]
    fn status_follows_the_edits() {
        let mut f = file(1, &[(keys::TITLE, "A")]);
        assert_eq!(f.status, FileStatus::Unchanged);
        f.edited.set_one(keys::TITLE, "B");
        f.refresh_status();
        assert_eq!(f.status, FileStatus::Modified);
        f.edited.set_one(keys::TITLE, "A");
        f.refresh_status();
        assert_eq!(f.status, FileStatus::Unchanged);
    }

    #[test]
    fn saving_files_keep_their_status() {
        let mut f = file(1, &[(keys::TITLE, "A")]);
        f.edited.set_one(keys::TITLE, "B");
        f.status = FileStatus::Saving;
        f.refresh_status();
        assert_eq!(f.status, FileStatus::Saving);
    }
}
