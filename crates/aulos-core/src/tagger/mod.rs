// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The tag editor's model: everything that is not widgets or I/O.
//!
//! The editor works on a set of loaded files ([`TagFile`]), each with the
//! tags it had on disk (`original`) and the tags the user is building
//! (`edited`). This module owns the rules that decide what the editor shows
//! and writes — Picard-style clusters, the field table over a multi-file
//! selection, merging a MusicBrainz release onto the existing tags, picking
//! a release from AcoustID hits, per-file save status — as plain, testable
//! data and functions. The I/O (reading, writing, MusicBrainz, AcoustID) lives
//! in [`work`] as futures resolving to [`TaggerEvent`]s; the UI runs them,
//! feeds the events back through [`TaggerState`] and renders it.

pub mod cluster;
pub mod edit;
pub mod facts;
pub mod file;
pub mod jobs;
pub mod labels;
pub mod message;
pub mod pick;
pub mod release;
pub mod state;
pub mod thumbs;
pub mod work;

pub use cluster::{Cluster, ClusterKey};
pub use edit::{FieldRow, FieldValue, PictureRow};
pub use facts::Facts;
pub use file::{AcoustIdHit, FileId, FileStatus, SubmissionState, TagFile, TrackAssignment};
pub use message::{TaggerEvent, TaggerMessage};
pub use state::{
    AddFieldDraft, AddFieldError, Confirm, LoadRequest, LoadedFile, LookupSource, LookupState,
    Notice, Remote, SaveJob, SaveSummary, ScanFileResult, ScanPhase, ScanStart, ScanState,
    ScanStep, SeedFile, SubmitJob, SubmitResult, SubmitStart, TaggerState,
};

use crate::library::Track;

/// Keyring entry holding the user's own AcoustID API key (needed to submit
/// fingerprints). The application key lives in `Config::acoustid_api_key`.
pub const ACOUSTID_USER_KEY_ENTRY: &str = "acoustid-user-key";

/// Where users get an AcoustID application key.
pub const ACOUSTID_NEW_APPLICATION_URL: &str = "https://acoustid.org/new-application";
/// Where users find their personal AcoustID API key.
pub const ACOUSTID_USER_KEY_URL: &str = "https://acoustid.org/api-key";

/// What a library context-menu entry asks the tag editor to do once the
/// tracks it was opened on are loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagAction {
    /// "Edit tags…": just load and select the files.
    Edit,
    /// "Look up on MusicBrainz": load, then search releases for the cluster.
    LookUp,
    /// "Scan with AcoustID": load, then fingerprint and identify the files.
    AcoustId,
}

/// Whether `track` is a file Aulos can edit tags of: a file of the local
/// library (MPD and Subsonic tracks live on servers).
#[must_use]
pub fn is_taggable(track: &Track) -> bool {
    track.provider_id.as_ref() == "local"
}

/// The editable (local-file) tracks of `tracks`, cloned for the tag editor.
#[must_use]
pub fn taggable_tracks(tracks: &[Track]) -> Vec<Track> {
    tracks
        .iter()
        .filter(|track| is_taggable(track))
        .cloned()
        .collect()
}
