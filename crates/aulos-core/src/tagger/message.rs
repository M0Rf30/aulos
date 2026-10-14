// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The two message families of the tag editor page, dispatched from
//! `app::update` like the Convert page's: [`TaggerMessage`] for what the
//! user does, [`TaggerEvent`] for results of work the page started.

use super::cluster::ClusterKey;
use super::file::FileId;
use super::state::{LoadedFile, ScanFileResult, SubmitResult};
use crate::library::tagio::EditPicture;
use crate::online::musicbrainz::{Release, ReleaseCandidate};
use std::path::PathBuf;

/// Direct UI actions, emitted by `views::tagger`.
#[derive(Debug, Clone)]
pub enum TaggerMessage {
    // --- Files ---
    /// "Add files…": opens the file picker.
    AddFiles,
    /// "Add folder…": opens the folder picker; its audio files are added.
    AddFolder,
    /// Select only this file.
    SelectFile(FileId),
    /// Add the file to the selection, or remove it.
    ToggleFile(FileId),
    /// Select exactly the files of a cluster.
    SelectCluster(ClusterKey),
    /// Add a cluster to the selection, or remove it when it is all selected.
    ToggleCluster(ClusterKey),
    SelectAll,
    ClearSelection,
    /// Remove the selected files (asks first when they have unsaved edits).
    RemoveSelected,
    /// Remove every file (asks first when some have unsaved edits).
    ClearAll,
    /// Answer "discard unsaved edits?" with yes.
    ConfirmDiscard,
    /// Answer it with no.
    CancelDiscard,

    // --- Metadata table (applies to the selection) ---
    /// Text typed into the box of a field.
    EditField {
        key: String,
        text: String,
    },
    /// Put a field back to what is on disk.
    RevertField(String),
    /// Remove a field.
    DeleteField(String),
    /// A well-known field was chosen in the "add field" dropdown (index into
    /// `KNOWN_KEYS`).
    AddFieldKnown(usize),
    /// The custom field name of the "add field" form changed.
    AddFieldCustom(String),
    /// The value of the "add field" form changed.
    AddFieldValue(String),
    /// Add the field of the form.
    AddFieldCommit,

    // --- Cover art (applies to the selection) ---
    /// "Add image…": pick an image file and add it as the chosen type.
    AddPicture,
    /// The type new pictures are added as was chosen (index into
    /// `edit::PICTURE_KINDS`).
    NewPictureKind(usize),
    RemovePicture {
        hash: u64,
        kind: u8,
    },
    SetPictureKind {
        hash: u64,
        kind: u8,
        new_kind: u8,
    },
    /// Put the pictures back to what is on disk.
    RevertPictures,
    /// Fetch the loaded release's front cover from the Cover Art Archive.
    FetchCover,

    // --- Save / revert ---
    /// Write every file with unsaved edits.
    Save,
    /// Drop the selected files' edits.
    RevertSelected,
    /// Drop every file's edits.
    RevertAll,

    // --- MusicBrainz ---
    /// Look up the selected files (every file when nothing is selected).
    Lookup,
    /// Select a cluster and look it up.
    LookupCluster(ClusterKey),
    /// The artist / album terms of the lookup panel changed.
    QueryArtist(String),
    QueryAlbum(String),
    SearchAgain,
    /// A release was chosen from the candidates (MBID).
    ChooseRelease(String),
    /// Undo the applied release.
    DiscardRelease,
    /// Hide the lookup panel (applied tags stay as edits).
    CloseLookup,
    /// Open a release / recording / AcoustID page in the browser (MBID or
    /// AcoustID).
    OpenReleasePage(String),
    OpenRecordingPage(String),
    OpenAcoustIdPage(String),
    /// "Add cluster as release on MusicBrainz": the cluster, or the
    /// selection (every file when nothing is selected) for `None`.
    AddAsRelease(Option<ClusterKey>),

    // --- AcoustID ---
    /// Fingerprint and identify the selected files (every file when nothing
    /// is selected).
    Scan,
    /// Select a cluster and scan it.
    ScanCluster(ClusterKey),
    CancelScan,
    /// Submit the fingerprints of the files that have a recording id.
    SubmitAcoustIds,

    // --- Page chrome ---
    /// Open the Settings drawer (to set the AcoustID keys).
    OpenSettings,
    DismissNotice,
}

/// Results of work started by the page, sent back to `update`.
#[derive(Debug, Clone)]
pub enum TaggerEvent {
    /// The "Add files" picker finished (also carries a picked folder's
    /// audio files).
    FilesPicked(Result<Vec<PathBuf>, String>),
    /// The "Add folder" picker finished.
    FolderPicked(Result<PathBuf, String>),
    /// The image picked for "Add image…", read and checked.
    ImagePicked(Result<EditPicture, String>),
    /// A batch of files was read. `generation` is the load it belongs to.
    FilesLoaded {
        generation: u64,
        files: Vec<LoadedFile>,
    },
    /// A cover thumbnail was decoded (`None`: undecodable).
    ThumbDecoded {
        hash: u64,
        pixels: Option<(u32, u32, Vec<u8>)>,
    },
    SearchFinished {
        request: u64,
        result: Result<Vec<ReleaseCandidate>, String>,
    },
    ReleaseFetched {
        request: u64,
        /// Boxed: a release is far larger than every other event payload.
        result: Result<Box<Release>, String>,
    },
    CoverFetched {
        request: u64,
        result: Result<Option<EditPicture>, String>,
    },
    /// One file of a scan was fingerprinted and looked up.
    FileScanned {
        generation: u64,
        file: FileId,
        result: ScanFileResult,
    },
    /// One file was written.
    FileSaved {
        file: FileId,
        result: Result<(), String>,
    },
    /// A fingerprint submission batch finished.
    Submitted {
        generation: u64,
        results: Vec<SubmitResult>,
    },
    /// A submission could not run at all (no keys, keyring unavailable…).
    SubmitAborted { generation: u64, error: String },
    /// "Add as release" prepared its form (or failed to).
    SeedOpened(Result<(), String>),
}
