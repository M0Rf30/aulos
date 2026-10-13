// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The tag editor's state and every transition on it that doesn't need I/O.
//!
//! The page logic (`crate::app::tagger_page`) asks the state for work
//! (`begin_*` methods return the job to run), runs it off the UI thread,
//! and hands each result back through the matching `finish_*` method. Every
//! piece of async work carries an id the state checks, so a result that
//! belongs to a session the user has since cleared, restarted or replaced is
//! dropped instead of landing on the wrong files.

use super::TagAction;
use super::cluster::{self, Cluster, ClusterKey};
use super::edit::{self, FieldRow, PICTURE_KINDS, PictureRow};
use super::file::{AcoustIdHit, FileId, FileStatus, SubmissionState, TagFile};
use super::pick::{self, ReleaseChoice};
use super::release::{self, ReleasePlan};
use crate::library::CoverArt;
use crate::library::fingerprint::Fingerprint;
use crate::library::tagio::{EditPicture, FileTags, TagSet, keys, model::KNOWN_KEYS, picture_type};
use crate::online::acoustid::AcoustIdResult;
use crate::online::musicbrainz::{self, Release, ReleaseCandidate, ReleaseQuery};
use cosmic::widget;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// A result that is still coming, has arrived, or failed.
#[derive(Debug, Clone, Default)]
pub enum Remote<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Remote<T> {
    #[must_use]
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    #[must_use]
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(error) => Some(error),
            _ => None,
        }
    }
}

// --- Payloads of async work -------------------------------------------------

/// A file read off the UI thread.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    pub path: PathBuf,
    pub read: Result<FileTags, String>,
    pub duration_secs: Option<f64>,
}

/// What to read, and which load it belongs to.
#[derive(Debug, Clone)]
pub struct LoadRequest {
    pub generation: u64,
    pub paths: Vec<PathBuf>,
}

/// A release search to run.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub request: u64,
    pub query: ReleaseQuery,
}

/// A release to fetch.
#[derive(Debug, Clone)]
pub struct ReleaseRequest {
    pub request: u64,
    pub release_id: String,
}

/// A Cover Art Archive front cover to fetch.
#[derive(Debug, Clone)]
pub struct CoverRequest {
    pub request: u64,
    pub release_id: String,
}

/// A file to write.
#[derive(Debug, Clone)]
pub struct SaveJob {
    pub file: FileId,
    pub path: PathBuf,
    pub tags: TagSet,
}

/// Outcome of the last write of a save batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveSummary {
    /// Files written successfully.
    pub written: Vec<PathBuf>,
    pub failed: usize,
    /// Library cover keys (`CoverArt::album_key`) of the albums the written
    /// files belonged to before and after the edit.
    pub album_keys: Vec<String>,
}

/// Fingerprint scan work.
#[derive(Debug, Clone)]
pub struct ScanStart {
    pub generation: u64,
    pub files: Vec<(FileId, PathBuf)>,
}

/// What came out of fingerprinting and looking up one file.
#[derive(Debug, Clone)]
pub struct ScanFileResult {
    pub fingerprint: Result<Fingerprint, String>,
    /// `None` when there was no fingerprint to look up.
    pub lookup: Option<Result<Vec<AcoustIdResult>, String>>,
}

/// Where a scan goes after a file's result.
#[derive(Debug, Clone)]
pub enum ScanStep {
    /// More files are still being scanned (or the result was stale).
    Pending,
    /// The scan found a release; fetch it.
    FetchRelease(ReleaseRequest),
    /// The scan is over without finding a release.
    NoMatch,
}

/// A fingerprint submission to send.
#[derive(Debug, Clone)]
pub struct SubmitStart {
    pub generation: u64,
    pub jobs: Vec<SubmitJob>,
}

#[derive(Debug, Clone)]
pub struct SubmitJob {
    pub file: FileId,
    pub path: PathBuf,
    /// The file's tags without pictures.
    pub tags: TagSet,
    pub recording_id: String,
    /// `None`: compute it first.
    pub fingerprint: Option<Arc<Fingerprint>>,
}

#[derive(Debug, Clone)]
pub struct SubmitResult {
    pub file: FileId,
    /// A fingerprint computed for the submission, to keep for later.
    pub fingerprint: Option<Fingerprint>,
    /// The AcoustID submission status (`pending`, `imported`), or why it failed.
    pub outcome: Result<String, String>,
}

// --- Sessions ----------------------------------------------------------------

/// Where the release being looked at came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupSource {
    /// A MusicBrainz search; the user picks the release.
    Search,
    /// An AcoustID scan picked the release.
    AcoustId,
}

/// What a fetched Cover Art Archive cover is applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoverTarget {
    /// The files matched by the release (when it is applied).
    Matched,
    /// The selected files ("fetch cover" button).
    Selection,
}

/// A MusicBrainz lookup in progress: the search, the chosen release and how
/// the files line up with it.
#[derive(Debug)]
pub struct LookupState {
    pub source: LookupSource,
    /// The files the lookup is for, in display order.
    pub targets: Vec<FileId>,
    /// The search terms (editable before searching again).
    pub artist: String,
    pub album: String,
    pub track_count: Option<u32>,
    pub candidates: Remote<Vec<ReleaseCandidate>>,
    /// MBID of the chosen release.
    pub chosen: Option<String>,
    pub release: Remote<Arc<Release>>,
    /// How the targets map onto the chosen release; its tags are applied to
    /// the files' edits as soon as it exists.
    pub plan: Option<ReleasePlan>,
    /// The front cover from the Cover Art Archive; `Ready(None)` when the
    /// release has none.
    pub cover: Remote<Option<EditPicture>>,
    /// The AcoustID scan's pick, for the summary line.
    pub acoustid_choice: Option<ReleaseChoice>,
    /// Whether the plan's cover is added to the matched files.
    pub use_cover: bool,

    search_request: u64,
    release_request: u64,
    cover_request: u64,
    cover_target: CoverTarget,
    /// The targets' edits before the first release was applied; every
    /// release applied afterwards starts from these, so switching release
    /// doesn't pile one release's fields on another's.
    baseline: HashMap<FileId, TagSet>,
    /// Recording MBIDs AcoustID identified per file.
    hints: HashMap<FileId, Vec<String>>,
}

impl LookupState {
    /// Whether a release's tags are applied to the files.
    #[must_use]
    pub fn applied(&self) -> bool {
        !self.baseline.is_empty()
    }
}

/// Where a fingerprint scan stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanPhase {
    /// Fingerprinting and looking up files.
    Running,
    /// Every file is done; the chosen release is being fetched.
    Resolving,
    /// Done, a release was applied.
    Done,
    /// Done, nothing identified (or nothing on a release).
    NoMatch,
    Cancelled,
}

/// An AcoustID scan in progress.
#[derive(Debug)]
pub struct ScanState {
    pub files: Vec<FileId>,
    /// Files finished (successfully or not).
    pub done: usize,
    /// Files AcoustID identified.
    pub identified: usize,
    pub failures: Vec<(FileId, String)>,
    pub phase: ScanPhase,
    generation: u64,
    hits: Vec<(FileId, AcoustIdResult)>,
}

/// Fingerprint submission in progress.
#[derive(Debug, Default)]
struct SubmitBatch {
    generation: u64,
    pending: usize,
}

/// A question the user must answer before files with unsaved edits are
/// dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    ClearAll,
    Remove(Vec<FileId>),
}

/// A message under the toolbar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub error: bool,
    pub text: String,
}

/// Why the "add field" form can't add what it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddFieldError {
    /// No file that can be edited is selected.
    NoSelection,
    /// Neither a known field nor a custom name was given.
    NoKey,
    /// The custom name can't be a tag name.
    InvalidKey,
    NoValue,
}

/// The "add field" form: a field from [`KNOWN_KEYS`] or a custom name, and
/// its value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddFieldDraft {
    /// Index into [`KNOWN_KEYS`].
    pub known: Option<usize>,
    /// A custom field name; takes precedence over `known` when not empty.
    pub custom: String,
    pub value: String,
}

impl AddFieldDraft {
    /// The key the form would add: the custom name (normalised; `None`
    /// when invalid) or the chosen known field.
    #[must_use]
    pub fn key(&self) -> Option<String> {
        if self.custom.trim().is_empty() {
            self.known
                .and_then(|i| KNOWN_KEYS.get(i))
                .map(|k| (*k).to_owned())
        } else {
            edit::normalize_key(&self.custom)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditBuffer {
    key: String,
    text: String,
}

#[derive(Debug, Default)]
struct SaveBatch {
    total: usize,
    pending: usize,
    written: Vec<PathBuf>,
    failed: usize,
    album_keys: BTreeSet<String>,
}

/// The library album key a file's tags put it under (see the scanner's
/// album-artist fallback): used to invalidate cached covers.
#[must_use]
pub fn library_album_key(tags: &TagSet) -> String {
    let artist = tags
        .get(keys::ALBUMARTIST)
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if tags.get(keys::COMPILATION) == Some("1") {
                crate::library::compilations::VARIOUS_ARTISTS.to_owned()
            } else {
                tags.get(keys::ARTIST).unwrap_or_default().to_owned()
            }
        });
    CoverArt::album_key(&artist, tags.get(keys::ALBUM).unwrap_or_default())
}

/// The state of the whole tag editor page.
#[derive(Debug, Default)]
pub struct TaggerState {
    files: Vec<TagFile>,
    next_file_id: FileId,
    next_request: u64,
    selection: BTreeSet<FileId>,

    load_generation: u64,
    loading: usize,
    in_flight: HashSet<PathBuf>,
    /// Paths of the loads that should end up selected.
    select_paths: HashSet<PathBuf>,
    /// Already loaded files that join them in the selection.
    select_known: BTreeSet<FileId>,
    /// The selection is replaced by the first of those that arrives.
    replace_selection: bool,
    pending_action: Option<TagAction>,

    clusters: Vec<Cluster>,
    rows: Vec<FieldRow>,
    pictures: Vec<PictureRow>,
    edit_buffer: Option<EditBuffer>,
    add_field: AddFieldDraft,
    add_field_error: Option<AddFieldError>,
    new_picture_kind: u8,
    /// Decoded cover thumbnails by [`edit::picture_hash`]; `None` when the
    /// image could not be decoded.
    thumbs: HashMap<u64, Option<widget::icon::Handle>>,
    thumbs_pending: HashSet<u64>,

    lookup: Option<LookupState>,
    scan: Option<ScanState>,
    submit: Option<SubmitBatch>,
    save: Option<SaveBatch>,
    confirm: Option<Confirm>,
    notice: Option<Notice>,
}

impl TaggerState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            new_picture_kind: picture_type::FRONT_COVER,
            ..Self::default()
        }
    }

    fn request_id(&mut self) -> u64 {
        self.next_request += 1;
        self.next_request
    }

    // --- Reading ----------------------------------------------------------

    #[must_use]
    pub fn files(&self) -> &[TagFile] {
        &self.files
    }

    #[must_use]
    pub fn file(&self, id: FileId) -> Option<&TagFile> {
        self.files.iter().find(|f| f.id == id)
    }

    fn file_mut(&mut self, id: FileId) -> Option<&mut TagFile> {
        self.files.iter_mut().find(|f| f.id == id)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    #[must_use]
    pub fn clusters(&self) -> &[Cluster] {
        &self.clusters
    }

    /// Files in the order they are listed (cluster by cluster).
    #[must_use]
    pub fn display_order(&self) -> Vec<FileId> {
        self.clusters
            .iter()
            .flat_map(|c| c.files.iter().copied())
            .collect()
    }

    #[must_use]
    pub fn is_selected(&self, id: FileId) -> bool {
        self.selection.contains(&id)
    }

    #[must_use]
    pub fn selected_count(&self) -> usize {
        self.selection.len()
    }

    /// The selected files in display order.
    #[must_use]
    pub fn selected_files(&self) -> Vec<&TagFile> {
        self.display_order()
            .into_iter()
            .filter(|id| self.selection.contains(id))
            .filter_map(|id| self.file(id))
            .collect()
    }

    /// Whether every file of `cluster` is selected.
    #[must_use]
    pub fn cluster_selected(&self, cluster: &Cluster) -> bool {
        !cluster.files.is_empty() && cluster.files.iter().all(|id| self.selection.contains(id))
    }

    /// The files the metadata table and cover panel describe: the selected
    /// files that can be edited, or when none can, the selected ones.
    fn table_files(&self) -> Vec<&TagFile> {
        let selected = self.selected_files();
        let editable: Vec<&TagFile> = selected
            .iter()
            .copied()
            .filter(|f| f.is_editable())
            .collect();
        if editable.is_empty() {
            selected
        } else {
            editable
        }
    }

    /// Whether the table can be edited: some selected file is editable.
    #[must_use]
    pub fn selection_editable(&self) -> bool {
        self.selected_files()
            .iter()
            .any(|f| f.is_editable() && f.status != FileStatus::Saving)
    }

    /// The metadata table for the selection.
    #[must_use]
    pub fn rows(&self) -> &[FieldRow] {
        &self.rows
    }

    /// What the text box of `row` shows: what is being typed into it, else
    /// the row's new value.
    #[must_use]
    pub fn input_text(&self, row: &FieldRow) -> String {
        match &self.edit_buffer {
            Some(buffer) if buffer.key == row.key => buffer.text.clone(),
            _ => row.new.display(),
        }
    }

    /// The distinct pictures of the selection.
    #[must_use]
    pub fn pictures(&self) -> &[PictureRow] {
        &self.pictures
    }

    /// The type new pictures are added as.
    #[must_use]
    pub fn new_picture_kind(&self) -> u8 {
        self.new_picture_kind
    }

    #[must_use]
    pub fn thumb(&self, hash: u64) -> Option<&widget::icon::Handle> {
        self.thumbs.get(&hash).and_then(Option::as_ref)
    }

    #[must_use]
    pub fn add_field(&self) -> &AddFieldDraft {
        &self.add_field
    }

    #[must_use]
    pub fn add_field_error(&self) -> Option<AddFieldError> {
        self.add_field_error
    }

    /// Files still being read.
    #[must_use]
    pub fn loading(&self) -> usize {
        self.loading
    }

    #[must_use]
    pub fn lookup(&self) -> Option<&LookupState> {
        self.lookup.as_ref()
    }

    #[must_use]
    pub fn scan(&self) -> Option<&ScanState> {
        self.scan.as_ref()
    }

    #[must_use]
    pub fn confirm(&self) -> Option<&Confirm> {
        self.confirm.as_ref()
    }

    #[must_use]
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    pub fn set_notice(&mut self, error: bool, text: impl Into<String>) {
        self.notice = Some(Notice {
            error,
            text: text.into(),
        });
    }

    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// Files with edits that were not saved.
    #[must_use]
    pub fn unsaved_count(&self) -> usize {
        self.files.iter().filter(|f| f.has_unsaved_edits()).count()
    }

    /// Files a save would write.
    #[must_use]
    pub fn savable_count(&self) -> usize {
        self.files.iter().filter(|f| Self::savable(f)).count()
    }

    fn savable(file: &TagFile) -> bool {
        file.is_editable() && file.has_unsaved_edits()
    }

    #[must_use]
    pub fn is_saving(&self) -> bool {
        self.save.is_some()
    }

    /// `(finished, total)` of the running save.
    #[must_use]
    pub fn save_progress(&self) -> Option<(usize, usize)> {
        self.save
            .as_ref()
            .map(|batch| (batch.total - batch.pending, batch.total))
    }

    /// Whether any long-running job is active (clearing is refused then).
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.is_saving()
            || self.is_submitting()
            || self
                .scan
                .as_ref()
                .is_some_and(|s| matches!(s.phase, ScanPhase::Running | ScanPhase::Resolving))
    }

    // --- Derived data -------------------------------------------------------

    fn refresh_clusters(&mut self) {
        self.clusters = cluster::group(&self.files);
    }

    fn refresh_rows(&mut self) {
        self.rows = edit::field_rows(&self.table_files());
    }

    fn refresh_pictures(&mut self) {
        self.pictures = edit::picture_rows(&self.table_files());
    }

    fn refresh_all(&mut self) {
        self.refresh_clusters();
        self.refresh_rows();
        self.refresh_pictures();
    }

    // --- Loading files --------------------------------------------------------

    /// Starts loading `paths` (those not already loaded or loading). With
    /// `select`, the files end up selected (replacing the selection) once
    /// read; `action` runs when everything requested has been read — fetch it
    /// with [`Self::take_ready_action`].
    pub fn begin_load(
        &mut self,
        paths: Vec<PathBuf>,
        select: bool,
        action: Option<TagAction>,
    ) -> Option<LoadRequest> {
        if action.is_some() {
            self.pending_action = action;
        }
        let mut fresh: Vec<PathBuf> = Vec::new();
        let mut known: Vec<FileId> = Vec::new();
        for path in paths {
            if let Some(file) = self.files.iter().find(|f| f.path == path) {
                known.push(file.id);
            } else if !self.in_flight.contains(&path) && !fresh.contains(&path) {
                fresh.push(path);
            }
        }
        if select {
            if fresh.is_empty() {
                // Everything is loaded already: select it right away.
                if !known.is_empty() {
                    self.selection = known.into_iter().collect();
                    self.selection_changed();
                }
            } else {
                // The files read now replace the selection when they arrive,
                // together with those that were loaded before.
                self.select_paths.extend(fresh.iter().cloned());
                self.select_known.extend(known);
                self.replace_selection = true;
            }
        }
        if fresh.is_empty() {
            return None;
        }
        self.in_flight.extend(fresh.iter().cloned());
        self.loading += fresh.len();
        Some(LoadRequest {
            generation: self.load_generation,
            paths: fresh,
        })
    }

    /// Adds the files of a finished read. A result of a load that was
    /// cleared in the meantime is dropped.
    pub fn finish_load(&mut self, generation: u64, loaded: Vec<LoadedFile>) {
        if generation != self.load_generation {
            tracing::debug!(generation, "dropping stale file load");
            return;
        }
        let mut new_ids: Vec<FileId> = Vec::new();
        for item in loaded {
            self.in_flight.remove(&item.path);
            self.loading = self.loading.saturating_sub(1);
            if self.files.iter().any(|f| f.path == item.path) {
                continue;
            }
            self.next_file_id += 1;
            let id = self.next_file_id;
            let selected = self.select_paths.remove(&item.path);
            self.files
                .push(TagFile::new(id, item.path, item.read, item.duration_secs));
            if selected {
                new_ids.push(id);
            }
        }
        if !new_ids.is_empty() {
            if self.replace_selection {
                self.selection = std::mem::take(&mut self.select_known);
                self.replace_selection = false;
            }
            self.selection.extend(new_ids);
        }
        if self.loading == 0 {
            self.select_paths.clear();
            self.select_known.clear();
            self.replace_selection = false;
        }
        self.refresh_clusters();
        self.selection_changed();
    }

    /// The action requested with the loaded files, once all of them are read.
    pub fn take_ready_action(&mut self) -> Option<TagAction> {
        if self.loading == 0 {
            self.pending_action.take()
        } else {
            None
        }
    }

    // --- Selection ------------------------------------------------------------

    fn selection_changed(&mut self) {
        self.selection
            .retain(|id| self.files.iter().any(|f| f.id == *id));
        self.edit_buffer = None;
        self.add_field_error = None;
        self.refresh_rows();
        self.refresh_pictures();
    }

    /// Selects only `id`.
    pub fn select_only(&mut self, id: FileId) {
        self.selection = BTreeSet::from([id]);
        self.selection_changed();
    }

    /// Adds `id` to the selection, or removes it.
    pub fn toggle(&mut self, id: FileId) {
        if !self.selection.remove(&id) {
            self.selection.insert(id);
        }
        self.selection_changed();
    }

    /// Selects exactly the files of the cluster `key`.
    pub fn select_cluster(&mut self, key: &ClusterKey) {
        if let Some(cluster) = self.clusters.iter().find(|c| &c.key == key) {
            self.selection = cluster.files.iter().copied().collect();
            self.selection_changed();
        }
    }

    /// Adds the cluster `key` to the selection, or removes it when it is
    /// selected entirely.
    pub fn toggle_cluster(&mut self, key: &ClusterKey) {
        if let Some(cluster) = self.clusters.iter().find(|c| &c.key == key) {
            let all = cluster.files.iter().all(|id| self.selection.contains(id));
            for id in &cluster.files {
                if all {
                    self.selection.remove(id);
                } else {
                    self.selection.insert(*id);
                }
            }
            self.selection_changed();
        }
    }

    pub fn select_all(&mut self) {
        self.selection = self.files.iter().map(|f| f.id).collect();
        self.selection_changed();
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
        self.selection_changed();
    }

    // --- Editing the selection ------------------------------------------------

    /// Applies `f` to every selected file that can be edited, then updates
    /// their status and the derived data.
    fn edit_selection(&mut self, mut f: impl FnMut(&mut TagFile)) -> bool {
        let selection = &self.selection;
        let mut touched = false;
        for file in self.files.iter_mut().filter(|file| {
            selection.contains(&file.id) && file.is_editable() && file.status != FileStatus::Saving
        }) {
            f(file);
            file.refresh_status();
            touched = true;
        }
        touched
    }

    /// The user typed `text` into the box of `key`.
    pub fn set_field_text(&mut self, key: &str, text: String) {
        let current = self
            .rows
            .iter()
            .find(|r| r.key == key)
            .map_or(0, |r| r.max_values);
        if self.edit_selection(|file| edit::apply_text(&mut file.edited, key, &text, current)) {
            self.refresh_rows();
            self.edit_buffer = Some(EditBuffer {
                key: key.to_owned(),
                text,
            });
        }
    }

    /// Puts `key` of the selected files back to what is on disk.
    pub fn revert_field(&mut self, key: &str) {
        if self.edit_selection(|file| edit::revert_field(&mut file.edited, &file.original, key)) {
            self.edit_buffer = None;
            self.refresh_rows();
        }
    }

    /// Removes `key` from the selected files.
    pub fn delete_field(&mut self, key: &str) {
        if self.edit_selection(|file| file.edited.remove(key)) {
            self.edit_buffer = None;
            self.refresh_rows();
        }
    }

    /// Drops the edits of the selected files.
    pub fn revert_selected(&mut self) {
        self.edit_selection(|file| {
            file.edited = file.original.clone();
            file.assignment = None;
        });
        self.after_bulk_edit();
    }

    /// Drops the edits of every file (those not being saved).
    pub fn revert_all(&mut self) {
        for file in self
            .files
            .iter_mut()
            .filter(|f| f.status != FileStatus::Saving)
        {
            file.edited = file.original.clone();
            file.assignment = None;
            file.refresh_status();
        }
        self.after_bulk_edit();
    }

    fn after_bulk_edit(&mut self) {
        self.edit_buffer = None;
        // A release applied to the files is gone with their edits.
        if let Some(lookup) = &mut self.lookup {
            lookup.baseline.clear();
        }
        self.refresh_rows();
        self.refresh_pictures();
    }

    pub fn set_add_known(&mut self, index: Option<usize>) {
        self.add_field.known = index.filter(|&i| i < KNOWN_KEYS.len());
        self.add_field_error = None;
    }

    pub fn set_add_custom(&mut self, name: String) {
        self.add_field.custom = name;
        self.add_field_error = None;
    }

    pub fn set_add_value(&mut self, value: String) {
        self.add_field.value = value;
        self.add_field_error = None;
    }

    /// Adds the field of the "add field" form to the selected files.
    pub fn commit_add_field(&mut self) -> Result<(), AddFieldError> {
        let result = self.try_add_field();
        self.add_field_error = result.err();
        result
    }

    fn try_add_field(&mut self) -> Result<(), AddFieldError> {
        if !self.selection_editable() {
            return Err(AddFieldError::NoSelection);
        }
        let draft = self.add_field.clone();
        let key = match draft.key() {
            Some(key) => key,
            None if draft.custom.trim().is_empty() => return Err(AddFieldError::NoKey),
            None => return Err(AddFieldError::InvalidKey),
        };
        if draft.value.trim().is_empty() {
            return Err(AddFieldError::NoValue);
        }
        self.edit_selection(|file| edit::apply_text(&mut file.edited, &key, &draft.value, 0));
        self.add_field = AddFieldDraft::default();
        self.edit_buffer = None;
        self.refresh_rows();
        Ok(())
    }

    // --- Pictures ---------------------------------------------------------------

    /// Chooses the type new pictures are added as (one of [`PICTURE_KINDS`]).
    pub fn set_new_picture_kind(&mut self, kind: u8) {
        if PICTURE_KINDS.contains(&kind) {
            self.new_picture_kind = kind;
        }
    }

    /// Adds `picture` to the selected files (see [`edit::add_picture`]).
    pub fn add_picture(&mut self, picture: &EditPicture) {
        if self.edit_selection(|file| edit::add_picture(&mut file.edited, picture.clone())) {
            self.refresh_pictures();
        }
    }

    pub fn remove_picture(&mut self, hash: u64, kind: u8) {
        if self.edit_selection(|file| {
            edit::remove_picture(&mut file.edited, hash, kind);
        }) {
            self.refresh_pictures();
        }
    }

    pub fn set_picture_kind(&mut self, hash: u64, kind: u8, new_kind: u8) {
        if self.edit_selection(|file| {
            edit::set_picture_kind(&mut file.edited, hash, kind, new_kind);
        }) {
            self.refresh_pictures();
        }
    }

    /// Puts the selected files' pictures back to what is on disk.
    pub fn revert_pictures(&mut self) {
        if self.edit_selection(|file| file.edited.pictures = file.original.pictures.clone()) {
            self.refresh_pictures();
        }
    }

    /// Pictures of the selection that have no decoded thumbnail yet, as
    /// `(hash, image bytes)`; each is handed out once.
    pub fn missing_thumbs(&mut self) -> Vec<(u64, Vec<u8>)> {
        let mut wanted: Vec<(u64, Vec<u8>)> = Vec::new();
        for row in &self.pictures {
            if self.thumbs.contains_key(&row.hash) || self.thumbs_pending.contains(&row.hash) {
                continue;
            }
            let data = self
                .table_files()
                .iter()
                .flat_map(|f| f.edited.pictures.iter())
                .find(|p| edit::picture_hash(p) == row.hash)
                .map(|p| p.data.clone());
            if let Some(data) = data {
                wanted.push((row.hash, data));
            }
        }
        for (hash, _) in &wanted {
            self.thumbs_pending.insert(*hash);
        }
        wanted
    }

    /// Records the decoded thumbnail (`(width, height, RGBA pixels)`) of the
    /// picture with `hash`, or that it could not be decoded.
    pub fn set_thumb(&mut self, hash: u64, pixels: Option<(u32, u32, Vec<u8>)>) {
        self.thumbs_pending.remove(&hash);
        self.thumbs.insert(
            hash,
            pixels.map(|(w, h, rgba)| widget::icon::from_raster_pixels(w, h, rgba)),
        );
    }

    // --- Removing and clearing -----------------------------------------------------

    /// Removes the selected files. Returns `false` when the files have
    /// unsaved edits and [`Confirm`] was raised instead.
    pub fn request_remove_selected(&mut self) -> bool {
        if self.is_busy() {
            return false;
        }
        let ids: Vec<FileId> = self
            .selection
            .iter()
            .copied()
            .filter(|id| {
                self.file(*id)
                    .is_some_and(|f| f.status != FileStatus::Saving)
            })
            .collect();
        if ids.is_empty() {
            return true;
        }
        let unsaved = ids
            .iter()
            .any(|id| self.file(*id).is_some_and(TagFile::has_unsaved_edits));
        if unsaved {
            self.confirm = Some(Confirm::Remove(ids));
            return false;
        }
        self.remove_files(&ids);
        true
    }

    /// Removes every file. Returns `false` when [`Confirm`] was raised.
    pub fn request_clear(&mut self) -> bool {
        if self.is_busy() {
            return false;
        }
        if self.unsaved_count() > 0 {
            self.confirm = Some(Confirm::ClearAll);
            return false;
        }
        self.clear();
        true
    }

    /// The user agreed to lose the unsaved edits.
    pub fn accept_confirm(&mut self) {
        match self.confirm.take() {
            Some(Confirm::ClearAll) if !self.is_busy() => self.clear(),
            Some(Confirm::Remove(ids)) => self.remove_files(&ids),
            _ => {}
        }
    }

    pub fn cancel_confirm(&mut self) {
        self.confirm = None;
    }

    fn remove_files(&mut self, ids: &[FileId]) {
        self.files
            .retain(|f| !ids.contains(&f.id) || f.status == FileStatus::Saving);
        for id in ids {
            self.selection.remove(id);
        }
        if let Some(lookup) = &mut self.lookup {
            lookup.targets.retain(|t| !ids.contains(t));
            lookup.baseline.retain(|f, _| !ids.contains(f));
            if lookup.targets.is_empty() {
                self.lookup = None;
            }
        }
        self.refresh_all();
    }

    fn clear(&mut self) {
        self.files.clear();
        self.selection.clear();
        self.load_generation += 1;
        self.loading = 0;
        self.in_flight.clear();
        self.select_paths.clear();
        self.select_known.clear();
        self.replace_selection = false;
        self.pending_action = None;
        self.lookup = None;
        self.scan = None;
        self.submit = None;
        self.confirm = None;
        self.edit_buffer = None;
        self.refresh_all();
    }

    // --- Saving ------------------------------------------------------------------------

    /// Marks every file with unsaved edits as being written and returns what
    /// to write.
    pub fn begin_save(&mut self) -> Vec<SaveJob> {
        if self.save.is_some() {
            return Vec::new();
        }
        let mut batch = SaveBatch::default();
        let mut jobs = Vec::new();
        for file in self.files.iter_mut().filter(|f| Self::savable(f)) {
            file.status = FileStatus::Saving;
            batch.album_keys.insert(library_album_key(&file.original));
            batch.album_keys.insert(library_album_key(&file.edited));
            jobs.push(SaveJob {
                file: file.id,
                path: file.path.clone(),
                tags: file.edited.clone(),
            });
        }
        batch.pending = jobs.len();
        batch.total = jobs.len();
        if jobs.is_empty() {
            return jobs;
        }
        self.save = Some(batch);
        jobs
    }

    /// Records the result of writing `file`. Returns the batch summary when
    /// that was the last write of the batch.
    pub fn finish_save(&mut self, file: FileId, result: Result<(), String>) -> Option<SaveSummary> {
        let batch = self.save.as_mut()?;
        batch.pending = batch.pending.saturating_sub(1);
        if let Some(f) = self.files.iter_mut().find(|f| f.id == file) {
            match result {
                Ok(()) => {
                    f.original = f.edited.clone();
                    f.status = FileStatus::Saved;
                    batch.written.push(f.path.clone());
                }
                Err(message) => {
                    f.status = FileStatus::Error(message);
                    batch.failed += 1;
                }
            }
        } else {
            batch.failed += 1;
        }
        let batch = self.save.as_ref()?;
        if batch.pending > 0 {
            self.refresh_rows();
            self.refresh_pictures();
            return None;
        }
        let batch = self.save.take()?;
        self.refresh_all();
        Some(SaveSummary {
            written: batch.written,
            failed: batch.failed,
            album_keys: batch.album_keys.into_iter().collect(),
        })
    }

    // --- Lookup ------------------------------------------------------------------------

    /// The files a lookup or scan works on: the selected editable files in
    /// display order, or every editable file when nothing is selected.
    #[must_use]
    pub fn target_files(&self) -> Vec<FileId> {
        let selected: Vec<FileId> = self
            .display_order()
            .into_iter()
            .filter(|id| self.selection.contains(id))
            .filter(|id| self.file(*id).is_some_and(TagFile::is_editable))
            .collect();
        if !selected.is_empty() {
            return selected;
        }
        if !self.selection.is_empty() {
            return Vec::new();
        }
        self.display_order()
            .into_iter()
            .filter(|id| self.file(*id).is_some_and(TagFile::is_editable))
            .collect()
    }

    fn files_of(&self, ids: &[FileId]) -> Vec<&TagFile> {
        ids.iter().filter_map(|id| self.file(*id)).collect()
    }

    /// Opens a lookup for the target files and returns the search to run.
    /// `None` when there is nothing to look up or nothing searchable.
    pub fn begin_lookup(&mut self) -> Option<SearchRequest> {
        let targets = self.target_files();
        if targets.is_empty() {
            return None;
        }
        let query = release::lookup_query(&self.files_of(&targets));
        let request = self.request_id();
        self.lookup = Some(LookupState {
            source: LookupSource::Search,
            targets,
            artist: query.artist.clone(),
            album: query.album.clone(),
            track_count: query.track_count,
            candidates: Remote::Idle,
            chosen: None,
            release: Remote::Idle,
            plan: None,
            cover: Remote::Idle,
            acoustid_choice: None,
            use_cover: true,
            search_request: request,
            release_request: 0,
            cover_request: 0,
            cover_target: CoverTarget::Matched,
            baseline: HashMap::new(),
            hints: HashMap::new(),
        });
        self.search_now()
    }

    pub fn set_query_artist(&mut self, artist: String) {
        if let Some(lookup) = &mut self.lookup {
            lookup.artist = artist;
        }
    }

    pub fn set_query_album(&mut self, album: String) {
        if let Some(lookup) = &mut self.lookup {
            lookup.album = album;
        }
    }

    /// Runs the lookup's (possibly edited) query again.
    pub fn search_again(&mut self) -> Option<SearchRequest> {
        self.search_now()
    }

    fn search_now(&mut self) -> Option<SearchRequest> {
        let request = self.request_id();
        let lookup = self.lookup.as_mut()?;
        let query = ReleaseQuery {
            artist: lookup.artist.clone(),
            album: lookup.album.clone(),
            track_count: lookup.track_count,
            barcode: None,
            catalog_number: None,
        };
        if query.lucene().is_none() {
            lookup.candidates = Remote::Idle;
            return None;
        }
        lookup.search_request = request;
        lookup.candidates = Remote::Loading;
        Some(SearchRequest { request, query })
    }

    /// Records the candidates of a finished search.
    pub fn finish_search(&mut self, request: u64, result: Result<Vec<ReleaseCandidate>, String>) {
        let Some(lookup) = &mut self.lookup else {
            return;
        };
        if lookup.search_request != request {
            tracing::debug!(request, "dropping stale release search");
            return;
        }
        lookup.candidates = match result {
            Ok(candidates) => Remote::Ready(candidates),
            Err(error) => Remote::Failed(error),
        };
    }

    /// The user chose a release from the candidates: returns the fetch to run.
    pub fn choose_release(&mut self, release_id: &str) -> Option<ReleaseRequest> {
        let request = self.request_id();
        let lookup = self.lookup.as_mut()?;
        lookup.chosen = Some(release_id.to_owned());
        lookup.release = Remote::Loading;
        lookup.plan = None;
        lookup.cover = Remote::Idle;
        lookup.release_request = request;
        Some(ReleaseRequest {
            request,
            release_id: release_id.to_owned(),
        })
    }

    /// A fetched release: match the files to it and apply its tags to their
    /// edits (the previous release's, if any, is replaced).
    pub fn finish_release(&mut self, request: u64, result: Result<Release, String>) {
        let Some(lookup) = &mut self.lookup else {
            return;
        };
        if lookup.release_request != request {
            tracing::debug!(request, "dropping stale release");
            return;
        }
        match result {
            Err(error) => {
                lookup.release = Remote::Failed(error);
                lookup.plan = None;
            }
            Ok(release) => {
                let release = Arc::new(release);
                lookup.release = Remote::Ready(Arc::clone(&release));
                let targets = lookup.targets.clone();
                let hints = lookup.hints.clone();
                let files: Vec<&TagFile> = targets.iter().filter_map(|id| self.file(*id)).collect();
                let plan = release::plan_release(&files, &release, &hints);
                if let Some(lookup) = &mut self.lookup {
                    lookup.plan = Some(plan);
                }
                self.apply_plan();
            }
        }
    }

    /// Writes the plan's tags into the target files' edits, starting from the
    /// edits they had before any release was applied.
    fn apply_plan(&mut self) {
        let Some(lookup) = self.lookup.as_mut() else {
            return;
        };
        let Some(plan) = lookup.plan.as_ref() else {
            return;
        };
        let release = Arc::clone(&plan.release);
        for entry in &plan.entries {
            let Some(file) = self.files.iter_mut().find(|f| f.id == entry.file) else {
                continue;
            };
            if file.status == FileStatus::Saving {
                continue;
            }
            let base = lookup
                .baseline
                .entry(file.id)
                .or_insert_with(|| file.edited.clone())
                .clone();
            file.edited = match &entry.assignment {
                Some(assignment) => {
                    release::tags_for(&base, &release, assignment, file.acoustid.as_ref())
                }
                None => {
                    let mut tags = base;
                    if let Some(hit) = &file.acoustid {
                        tags.set_one(keys::ACOUSTID_ID, hit.acoustid.as_str());
                        if let [recording] = hit.recording_ids.as_slice() {
                            tags.set_one(keys::MUSICBRAINZ_RECORDINGID, recording.as_str());
                        }
                    }
                    tags
                }
            };
            file.assignment = entry.assignment.clone();
            file.refresh_status();
        }
        self.edit_buffer = None;
        self.refresh_rows();
        self.refresh_pictures();
    }

    /// The Cover Art Archive fetch the applied release wants: its front
    /// cover, when the release has one and covers are enabled. Call after a
    /// release was applied; hands the work out once.
    pub fn take_cover_request(&mut self, use_cover: bool) -> Option<CoverRequest> {
        let request = self.request_id();
        let lookup = self.lookup.as_mut()?;
        lookup.use_cover = use_cover;
        if !use_cover || !matches!(lookup.cover, Remote::Idle) {
            return None;
        }
        let release = lookup.release.ready()?;
        if !release.cover_art_archive.as_ref().is_some_and(|c| c.front) {
            lookup.cover = Remote::Ready(None);
            return None;
        }
        let release_id = release.id.clone();
        lookup.cover = Remote::Loading;
        lookup.cover_target = CoverTarget::Matched;
        lookup.cover_request = request;
        Some(CoverRequest {
            request,
            release_id,
        })
    }

    /// "Fetch cover" for the selected files: applies the release's front
    /// cover now when it is already downloaded, else returns the download.
    pub fn fetch_cover_for_selection(&mut self) -> Option<CoverRequest> {
        let request = self.request_id();
        let lookup = self.lookup.as_mut()?;
        match &lookup.cover {
            Remote::Ready(Some(picture)) => {
                let picture = picture.clone();
                self.apply_cover(&picture, CoverTarget::Selection);
                return None;
            }
            Remote::Loading => {
                lookup.cover_target = CoverTarget::Selection;
                return None;
            }
            Remote::Ready(None) => return None,
            Remote::Idle | Remote::Failed(_) => {}
        }
        let release_id = lookup.release.ready()?.id.clone();
        lookup.cover = Remote::Loading;
        lookup.cover_target = CoverTarget::Selection;
        lookup.cover_request = request;
        Some(CoverRequest {
            request,
            release_id,
        })
    }

    /// A downloaded cover: it replaces the front cover of the files it was
    /// requested for.
    pub fn finish_cover(&mut self, request: u64, result: Result<Option<EditPicture>, String>) {
        let Some(lookup) = &mut self.lookup else {
            return;
        };
        if lookup.cover_request != request {
            tracing::debug!(request, "dropping stale cover");
            return;
        }
        let target = lookup.cover_target;
        match result {
            Ok(picture) => {
                lookup.cover = Remote::Ready(picture.clone());
                if let Some(picture) = picture {
                    self.apply_cover(&picture, target);
                }
            }
            Err(error) => lookup.cover = Remote::Failed(error),
        }
    }

    fn apply_cover(&mut self, picture: &EditPicture, target: CoverTarget) {
        match target {
            CoverTarget::Selection => {
                self.edit_selection(|file| edit::set_front_cover(&mut file.edited, picture));
            }
            CoverTarget::Matched => {
                let Some(plan) = self.lookup.as_ref().and_then(|l| l.plan.as_ref()) else {
                    return;
                };
                let matched: Vec<FileId> = plan
                    .entries
                    .iter()
                    .filter(|e| e.assignment.is_some())
                    .map(|e| e.file)
                    .collect();
                for file in self
                    .files
                    .iter_mut()
                    .filter(|f| matched.contains(&f.id) && f.status != FileStatus::Saving)
                {
                    edit::set_front_cover(&mut file.edited, picture);
                    file.refresh_status();
                }
            }
        }
        self.refresh_pictures();
    }

    /// Undoes the applied release: the targets' edits go back to what they
    /// were before it, and the release is dropped (the candidates stay).
    pub fn discard_release(&mut self) {
        let Some(lookup) = self.lookup.as_mut() else {
            return;
        };
        let baseline = std::mem::take(&mut lookup.baseline);
        lookup.chosen = None;
        lookup.release = Remote::Idle;
        lookup.plan = None;
        lookup.cover = Remote::Idle;
        lookup.release_request = 0;
        lookup.cover_request = 0;
        for (id, tags) in baseline {
            if let Some(file) = self.file_mut(id).filter(|f| f.status != FileStatus::Saving) {
                file.edited = tags;
                file.assignment = None;
                file.refresh_status();
            }
        }
        self.edit_buffer = None;
        self.refresh_rows();
        self.refresh_pictures();
    }

    /// Hides the lookup panel; whatever it applied stays in the edits.
    pub fn close_lookup(&mut self) {
        self.lookup = None;
    }

    // --- AcoustID scan ----------------------------------------------------------------

    /// Starts scanning the target files; returns what to fingerprint.
    pub fn begin_scan(&mut self) -> Option<ScanStart> {
        let targets = self.target_files();
        if targets.is_empty() {
            return None;
        }
        let generation = self.request_id();
        let files: Vec<(FileId, PathBuf)> = targets
            .iter()
            .filter_map(|id| self.file(*id).map(|f| (f.id, f.path.clone())))
            .collect();
        self.scan = Some(ScanState {
            files: targets,
            done: 0,
            identified: 0,
            failures: Vec::new(),
            phase: ScanPhase::Running,
            generation,
            hits: Vec::new(),
        });
        Some(ScanStart { generation, files })
    }

    /// Abandons the running scan; results still on their way are dropped.
    pub fn cancel_scan(&mut self) {
        if let Some(scan) = &mut self.scan
            && matches!(scan.phase, ScanPhase::Running | ScanPhase::Resolving)
        {
            scan.phase = ScanPhase::Cancelled;
            scan.generation = 0;
        }
    }

    /// Records one file's scan result; when it was the last, picks the
    /// release (see [`pick::choose_release`]) and opens a lookup on it.
    pub fn finish_scan_file(
        &mut self,
        generation: u64,
        file: FileId,
        result: ScanFileResult,
    ) -> ScanStep {
        let Some(scan) = &mut self.scan else {
            return ScanStep::Pending;
        };
        if scan.generation != generation
            || scan.phase != ScanPhase::Running
            || !scan.files.contains(&file)
        {
            return ScanStep::Pending;
        }
        scan.done += 1;

        let mut hit: Option<(AcoustIdResult, AcoustIdHit)> = None;
        let mut fingerprint = None;
        match result.fingerprint {
            Ok(fp) => fingerprint = Some(Arc::new(fp)),
            Err(error) => scan.failures.push((file, error)),
        }
        match result.lookup {
            Some(Ok(results)) => {
                if let Some(best) = pick::best_result(&results) {
                    let recording_ids = best.recordings.iter().map(|r| r.id.clone()).collect();
                    hit = Some((
                        best.clone(),
                        AcoustIdHit {
                            acoustid: best.id.clone(),
                            recording_ids,
                        },
                    ));
                }
            }
            Some(Err(error)) => scan.failures.push((file, error)),
            None => {}
        }
        if let Some((best, _)) = &hit {
            scan.identified += 1;
            scan.hits.push((file, best.clone()));
        }
        let finished = scan.done >= scan.files.len();

        if let Some(f) = self.file_mut(file) {
            if let Some(fp) = fingerprint {
                f.fingerprint = Some(fp);
            }
            if let Some((_, hit)) = hit {
                f.acoustid = Some(hit);
            }
        }
        if !finished {
            return ScanStep::Pending;
        }
        self.finish_scan()
    }

    fn finish_scan(&mut self) -> ScanStep {
        let Some(scan) = &mut self.scan else {
            return ScanStep::NoMatch;
        };
        let file_count = scan.files.len();
        let Some(choice) = pick::choose_release(&scan.hits, file_count) else {
            scan.phase = ScanPhase::NoMatch;
            return ScanStep::NoMatch;
        };
        scan.phase = ScanPhase::Resolving;
        let targets = scan.files.clone();
        let hints: HashMap<FileId, Vec<String>> = scan
            .hits
            .iter()
            .map(|(file, result)| {
                (
                    *file,
                    result.recordings.iter().map(|r| r.id.clone()).collect(),
                )
            })
            .collect();

        let query = release::lookup_query(&self.files_of(&targets));
        let request = self.request_id();
        self.lookup = Some(LookupState {
            source: LookupSource::AcoustId,
            targets,
            artist: query.artist,
            album: query.album,
            track_count: query.track_count,
            candidates: Remote::Idle,
            chosen: Some(choice.release_id.clone()),
            release: Remote::Loading,
            plan: None,
            cover: Remote::Idle,
            acoustid_choice: Some(choice.clone()),
            use_cover: true,
            search_request: 0,
            release_request: request,
            cover_request: 0,
            cover_target: CoverTarget::Matched,
            baseline: HashMap::new(),
            hints,
        });
        ScanStep::FetchRelease(ReleaseRequest {
            request,
            release_id: choice.release_id,
        })
    }

    /// The scan's release arrived (or failed): closes the scan.
    pub fn finish_scan_release(&mut self) {
        if let Some(scan) = &mut self.scan
            && scan.phase == ScanPhase::Resolving
        {
            scan.phase = ScanPhase::Done;
        }
    }

    // --- Submitting fingerprints -----------------------------------------------------------

    fn recording_id(file: &TagFile) -> Option<String> {
        file.edited
            .get(keys::MUSICBRAINZ_RECORDINGID)
            .filter(|id| musicbrainz::is_mbid(id))
            .map(str::to_owned)
    }

    /// Files a submission would send: the selected files (every file when
    /// none is selected) that carry a MusicBrainz recording id.
    #[must_use]
    pub fn submittable(&self) -> usize {
        self.submit_targets().len()
    }

    fn submit_targets(&self) -> Vec<FileId> {
        self.display_order()
            .into_iter()
            .filter(|id| self.selection.is_empty() || self.selection.contains(id))
            .filter(|id| {
                self.file(*id)
                    .is_some_and(|f| Self::recording_id(f).is_some())
            })
            .collect()
    }

    #[must_use]
    pub fn is_submitting(&self) -> bool {
        self.submit.is_some()
    }

    /// Marks the submittable files as being sent and returns the work.
    pub fn begin_submit(&mut self) -> Option<SubmitStart> {
        if self.submit.is_some() {
            return None;
        }
        let ids = self.submit_targets();
        if ids.is_empty() {
            return None;
        }
        let generation = self.request_id();
        let mut jobs = Vec::new();
        for id in ids {
            let Some(file) = self.file_mut(id) else {
                continue;
            };
            let Some(recording_id) = Self::recording_id(file) else {
                continue;
            };
            file.submission = SubmissionState::Pending;
            let mut tags = file.edited.clone();
            tags.pictures.clear();
            jobs.push(SubmitJob {
                file: id,
                path: file.path.clone(),
                tags,
                recording_id,
                fingerprint: file.fingerprint.clone(),
            });
        }
        self.submit = Some(SubmitBatch {
            generation,
            pending: jobs.len(),
        });
        Some(SubmitStart { generation, jobs })
    }

    /// Records the outcome of a submission batch.
    pub fn finish_submit(&mut self, generation: u64, results: Vec<SubmitResult>) {
        let Some(batch) = &mut self.submit else {
            return;
        };
        if batch.generation != generation {
            return;
        }
        batch.pending = batch.pending.saturating_sub(results.len());
        for result in results {
            let Some(file) = self.files.iter_mut().find(|f| f.id == result.file) else {
                continue;
            };
            if let Some(fp) = result.fingerprint {
                file.fingerprint = Some(Arc::new(fp));
            }
            file.submission = match result.outcome {
                Ok(status) => SubmissionState::Submitted { status },
                Err(error) => SubmissionState::Failed(error),
            };
        }
        if self.submit.as_ref().is_some_and(|b| b.pending == 0) {
            self.submit = None;
        }
    }

    /// Gives up on a submission that could not run at all (no key, no
    /// keyring…): its files go back to "not submitted".
    pub fn abort_submit(&mut self, generation: u64, error: &str) {
        let Some(batch) = &self.submit else { return };
        if batch.generation != generation {
            return;
        }
        for file in self
            .files
            .iter_mut()
            .filter(|f| f.submission == SubmissionState::Pending)
        {
            file.submission = SubmissionState::Failed(error.to_owned());
        }
        self.submit = None;
    }
}

#[cfg(test)]
mod tests {
    use super::super::file::testing::*;
    use super::*;

    fn loaded(path: &str, tags: &[(&str, &str)]) -> LoadedFile {
        let mut set = TagSet::new();
        for (k, v) in tags {
            set.push(k, *v);
        }
        LoadedFile {
            path: PathBuf::from(path),
            read: Ok(FileTags {
                container: crate::library::tagio::Container::Flac,
                tags: set,
            }),
            duration_secs: Some(180.0),
        }
    }

    /// A state with `n` files of one album, none selected.
    fn state_with_album(n: u64) -> TaggerState {
        let mut state = TaggerState::new();
        let request = state
            .begin_load(
                (1..=n)
                    .map(|i| PathBuf::from(format!("/m/{i:02}.flac")))
                    .collect(),
                false,
                None,
            )
            .unwrap();
        state.finish_load(
            request.generation,
            (1..=n)
                .map(|i| {
                    loaded(
                        &format!("/m/{i:02}.flac"),
                        &[
                            (keys::ALBUMARTIST, "Artist"),
                            (keys::ALBUM, "Album"),
                            (keys::TITLE, &format!("Song {i}")),
                            (keys::TRACKNUMBER, &i.to_string()),
                        ],
                    )
                })
                .collect(),
        );
        state
    }

    #[test]
    fn loading_adds_files_groups_them_and_selects_when_asked() {
        let mut state = TaggerState::new();
        let req = state
            .begin_load(
                vec!["/a/1.flac".into(), "/a/2.flac".into()],
                true,
                Some(TagAction::LookUp),
            )
            .unwrap();
        assert_eq!(state.loading(), 2);
        assert!(
            state.take_ready_action().is_none(),
            "not before the files are read"
        );
        state.finish_load(
            req.generation,
            vec![
                loaded("/a/1.flac", &[(keys::ALBUM, "A")]),
                loaded("/a/2.flac", &[(keys::ALBUM, "A")]),
            ],
        );
        assert_eq!(state.files().len(), 2);
        assert_eq!(state.clusters().len(), 1);
        assert_eq!(state.selected_count(), 2);
        assert_eq!(state.take_ready_action(), Some(TagAction::LookUp));
        assert_eq!(state.take_ready_action(), None, "taken once");
    }

    #[test]
    fn already_loaded_paths_are_not_read_twice_but_are_selected() {
        let mut state = state_with_album(2);
        assert_eq!(state.selected_count(), 0);
        let req = state.begin_load(vec!["/m/01.flac".into()], true, Some(TagAction::Edit));
        assert!(req.is_none());
        assert_eq!(state.selected_count(), 1);
        assert_eq!(state.take_ready_action(), Some(TagAction::Edit));
    }

    #[test]
    fn a_load_cleared_in_the_meantime_is_dropped() {
        let mut state = TaggerState::new();
        let req = state
            .begin_load(vec!["/a.flac".into()], false, None)
            .unwrap();
        assert!(state.request_clear());
        state.finish_load(req.generation, vec![loaded("/a.flac", &[])]);
        assert!(state.is_empty());
        assert_eq!(state.loading(), 0);
    }

    #[test]
    fn unreadable_files_are_listed_read_only() {
        let mut state = TaggerState::new();
        let req = state.begin_load(vec!["/a.xyz".into()], true, None).unwrap();
        state.finish_load(
            req.generation,
            vec![LoadedFile {
                path: "/a.xyz".into(),
                read: Err("unsupported file".into()),
                duration_secs: None,
            }],
        );
        let file = &state.files()[0];
        assert!(!file.is_editable());
        assert!(!state.selection_editable());
        // Edits don't touch it.
        state.set_field_text(keys::TITLE, "x".into());
        assert!(state.files()[0].edited.is_empty());
    }

    #[test]
    fn typing_applies_to_every_selected_file_and_marks_them_modified() {
        let mut state = state_with_album(3);
        state.select_all();
        state.set_field_text(keys::ALBUM, "New Album".into());
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.edited.get(keys::ALBUM) == Some("New Album"))
        );
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.status == FileStatus::Modified)
        );
        let row = state.rows().iter().find(|r| r.key == keys::ALBUM).unwrap();
        assert!(row.changed);
        assert_eq!(state.input_text(row), "New Album");
        // Titles still differ: the table says so and editing is not applied to them.
        let title = state.rows().iter().find(|r| r.key == keys::TITLE).unwrap();
        assert!(title.new.is_mixed());
    }

    #[test]
    fn only_the_selected_files_change() {
        let mut state = state_with_album(3);
        let first = state.display_order()[0];
        state.select_only(first);
        state.set_field_text(keys::GENRE, "Rock; Pop".into());
        assert_eq!(
            state.file(first).unwrap().edited.get_all(keys::GENRE),
            ["Rock", "Pop"]
        );
        assert_eq!(state.files().iter().filter(|f| f.is_dirty()).count(), 1);
    }

    #[test]
    fn the_text_box_keeps_what_was_typed_while_the_value_is_normalised() {
        let mut state = state_with_album(1);
        state.select_all();
        state.set_field_text(keys::GENRE, "Rock; ".into());
        let row = state
            .rows()
            .iter()
            .find(|r| r.key == keys::GENRE)
            .unwrap()
            .clone();
        assert_eq!(row.new.display(), "Rock");
        assert_eq!(
            state.input_text(&row),
            "Rock; ",
            "the trailing separator survives the keystroke"
        );
        // Moving the selection drops the buffer.
        state.clear_selection();
        state.select_all();
        let row = state
            .rows()
            .iter()
            .find(|r| r.key == keys::GENRE)
            .unwrap()
            .clone();
        assert_eq!(state.input_text(&row), "Rock");
    }

    #[test]
    fn reverting_and_deleting_fields() {
        let mut state = state_with_album(2);
        state.select_all();
        state.set_field_text(keys::ALBUM, "X".into());
        state.revert_field(keys::ALBUM);
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.edited.get(keys::ALBUM) == Some("Album"))
        );
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.status == FileStatus::Unchanged)
        );
        state.delete_field(keys::ALBUM);
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.edited.get(keys::ALBUM).is_none())
        );
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.status == FileStatus::Modified)
        );
        state.revert_selected();
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.status == FileStatus::Unchanged)
        );
    }

    #[test]
    fn the_add_field_form_validates_and_adds_known_and_custom_fields() {
        let mut state = state_with_album(2);
        assert_eq!(state.commit_add_field(), Err(AddFieldError::NoSelection));
        state.select_all();
        assert_eq!(state.commit_add_field(), Err(AddFieldError::NoKey));
        let genre = KNOWN_KEYS.iter().position(|k| *k == keys::GENRE);
        state.set_add_known(genre);
        assert_eq!(state.commit_add_field(), Err(AddFieldError::NoValue));
        state.set_add_value("Jazz; Blues".into());
        assert_eq!(state.commit_add_field(), Ok(()));
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.edited.get_all(keys::GENRE) == ["Jazz", "Blues"])
        );
        assert_eq!(*state.add_field(), AddFieldDraft::default());
        // A custom name wins over the dropdown and must be a legal name.
        state.set_add_custom("my field".into());
        state.set_add_value("1".into());
        assert_eq!(state.commit_add_field(), Err(AddFieldError::InvalidKey));
        state.set_add_custom("MyField".into());
        assert_eq!(state.commit_add_field(), Ok(()));
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.edited.get("myfield") == Some("1"))
        );
    }

    #[test]
    fn saving_walks_files_through_saving_to_saved_or_error() {
        let mut state = state_with_album(3);
        state.select_all();
        state.set_field_text(keys::TITLE, "Same".into());
        assert_eq!(state.savable_count(), 3);
        let jobs = state.begin_save();
        assert_eq!(jobs.len(), 3);
        assert!(state.files().iter().all(|f| f.status == FileStatus::Saving));
        assert!(
            state.begin_save().is_empty(),
            "no second batch while one runs"
        );
        // Locked while saving.
        state.set_field_text(keys::TITLE, "Other".into());
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.edited.get(keys::TITLE) == Some("Same"))
        );

        assert!(state.finish_save(jobs[0].file, Ok(())).is_none());
        assert!(
            state
                .finish_save(jobs[1].file, Err("disk full".into()))
                .is_none()
        );
        let summary = state.finish_save(jobs[2].file, Ok(())).unwrap();
        assert_eq!(summary.written.len(), 2);
        assert_eq!(summary.failed, 1);
        assert!(
            summary
                .album_keys
                .contains(&CoverArt::album_key("Artist", "Album"))
        );

        let by_id = |id| state.file(id).unwrap();
        assert_eq!(by_id(jobs[0].file).status, FileStatus::Saved);
        assert_eq!(by_id(jobs[0].file).original.get(keys::TITLE), Some("Same"));
        assert_eq!(
            by_id(jobs[1].file).status,
            FileStatus::Error("disk full".into())
        );
        assert!(
            by_id(jobs[1].file).is_dirty(),
            "failed edits are kept for a retry"
        );
        assert!(!state.is_saving());
        // Editing a saved file makes it modified again; the failed one retries.
        assert_eq!(state.savable_count(), 1);
    }

    #[test]
    fn a_saved_file_becomes_modified_again_when_edited() {
        let mut state = state_with_album(1);
        state.select_all();
        state.set_field_text(keys::TITLE, "A".into());
        let jobs = state.begin_save();
        state.finish_save(jobs[0].file, Ok(()));
        assert_eq!(state.files()[0].status, FileStatus::Saved);
        state.set_field_text(keys::TITLE, "B".into());
        assert_eq!(state.files()[0].status, FileStatus::Modified);
        state.set_field_text(keys::TITLE, "A".into());
        assert_eq!(state.files()[0].status, FileStatus::Unchanged);
    }

    #[test]
    fn saved_files_regroup_by_their_new_tags() {
        let mut state = state_with_album(2);
        state.select_all();
        let first = state.display_order()[0];
        state.select_only(first);
        state.set_field_text(keys::ALBUM, "Elsewhere".into());
        assert_eq!(state.clusters().len(), 1, "unsaved edits don't regroup");
        let jobs = state.begin_save();
        state.finish_save(jobs[0].file, Ok(()));
        assert_eq!(state.clusters().len(), 2);
    }

    #[test]
    fn clearing_or_removing_files_with_unsaved_edits_asks_first() {
        let mut state = state_with_album(2);
        state.select_all();
        state.set_field_text(keys::TITLE, "x".into());
        assert!(!state.request_clear());
        assert_eq!(state.confirm(), Some(&Confirm::ClearAll));
        state.cancel_confirm();
        assert_eq!(state.files().len(), 2);
        assert!(!state.request_remove_selected());
        assert!(matches!(state.confirm(), Some(Confirm::Remove(ids)) if ids.len() == 2));
        state.accept_confirm();
        assert!(state.is_empty());
        assert!(state.request_clear(), "nothing left to lose");
    }

    #[test]
    fn removing_clean_files_needs_no_confirmation() {
        let mut state = state_with_album(3);
        let first = state.display_order()[0];
        state.select_only(first);
        assert!(state.request_remove_selected());
        assert_eq!(state.files().len(), 2);
        assert_eq!(state.selected_count(), 0);
    }

    #[test]
    fn files_being_saved_are_not_cleared() {
        let mut state = state_with_album(1);
        state.select_all();
        state.set_field_text(keys::TITLE, "x".into());
        let _ = state.begin_save();
        assert!(!state.request_clear());
        assert!(state.confirm().is_none());
        assert_eq!(state.files().len(), 1);
    }

    fn fixture_release() -> Release {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/musicbrainz/release_lookup.json"
        ))
        .unwrap()
    }

    fn moderat_state() -> TaggerState {
        let mut state = TaggerState::new();
        let names = ["The Mark (interlude)", "Bad Kingdom", "Versions"];
        let req = state
            .begin_load(
                (1..=3)
                    .map(|i| PathBuf::from(format!("/m/{i}.flac")))
                    .collect(),
                true,
                None,
            )
            .unwrap();
        state.finish_load(
            req.generation,
            names
                .iter()
                .enumerate()
                .map(|(i, title)| {
                    loaded(
                        &format!("/m/{}.flac", i + 1),
                        &[
                            (keys::ALBUMARTIST, "moderat"),
                            (keys::ALBUM, "ii"),
                            (keys::TITLE, title),
                            (keys::TRACKNUMBER, &(i + 1).to_string()),
                            (keys::COMMENT, "mine"),
                        ],
                    )
                })
                .collect(),
        );
        state
    }

    #[test]
    fn a_lookup_searches_for_the_clusters_artist_and_album() {
        let mut state = moderat_state();
        let search = state.begin_lookup().unwrap();
        assert_eq!(search.query.artist, "moderat");
        assert_eq!(search.query.album, "ii");
        let lookup = state.lookup().unwrap();
        assert!(lookup.candidates.is_loading());
        assert_eq!(lookup.targets.len(), 3);
    }

    #[test]
    fn stale_search_results_are_dropped() {
        let mut state = moderat_state();
        let first = state.begin_lookup().unwrap();
        let second = state.search_again().unwrap();
        state.finish_search(first.request, Ok(vec![ReleaseCandidate::default()]));
        assert!(state.lookup().unwrap().candidates.is_loading());
        state.finish_search(second.request, Ok(Vec::new()));
        assert!(state.lookup().unwrap().candidates.ready().is_some());
    }

    #[test]
    fn choosing_a_release_matches_and_applies_it_keeping_unrelated_fields() {
        let mut state = moderat_state();
        state.begin_lookup().unwrap();
        let req = state
            .choose_release("00398749-1816-4c98-8a39-377627740365")
            .unwrap();
        state.finish_release(req.request, Ok(fixture_release()));

        let lookup = state.lookup().unwrap();
        let plan = lookup.plan.as_ref().unwrap();
        assert_eq!(plan.matched(), 3);
        assert!(lookup.applied());
        for file in state.files() {
            assert_eq!(file.edited.get(keys::ALBUM), Some("II"));
            assert_eq!(file.edited.get(keys::ALBUMARTIST), Some("Moderat"));
            assert_eq!(
                file.edited.get(keys::COMMENT),
                Some("mine"),
                "unrelated fields are kept"
            );
            assert_eq!(file.status, FileStatus::Modified);
            assert!(file.assignment.is_some());
            // The disk values are untouched until saved.
            assert_eq!(file.original.get(keys::ALBUM), Some("ii"));
        }
        let versions = state
            .files()
            .iter()
            .find(|f| f.edited.get(keys::TITLE) == Some("Versions"))
            .unwrap();
        assert_eq!(versions.edited.get(keys::TRACKNUMBER), Some("3"));
    }

    #[test]
    fn switching_releases_restarts_from_the_edits_before_the_first_one() {
        let mut state = moderat_state();
        state.begin_lookup().unwrap();
        let req = state.choose_release("a").unwrap();
        state.finish_release(req.request, Ok(fixture_release()));
        // A second "release" that provides no label.
        let mut other = fixture_release();
        other.label_info.clear();
        other.title = "Other".into();
        let req = state.choose_release("b").unwrap();
        state.finish_release(req.request, Ok(other));
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.edited.get(keys::ALBUM) == Some("Other"))
        );
        assert!(
            state
                .files()
                .iter()
                .all(|f| f.edited.get(keys::LABEL).is_none()),
            "the first release's label must not leak into the second"
        );
    }

    #[test]
    fn a_stale_release_is_dropped() {
        let mut state = moderat_state();
        state.begin_lookup().unwrap();
        let first = state.choose_release("a").unwrap();
        let second = state.choose_release("b").unwrap();
        state.finish_release(first.request, Ok(fixture_release()));
        assert!(state.lookup().unwrap().plan.is_none());
        assert!(state.files().iter().all(|f| !f.is_dirty()));
        state.finish_release(second.request, Ok(fixture_release()));
        assert!(state.lookup().unwrap().plan.is_some());
    }

    #[test]
    fn discarding_the_release_restores_the_earlier_edits() {
        let mut state = moderat_state();
        let first = state.display_order()[0];
        state.select_only(first);
        state.set_field_text(keys::GENRE, "Mine".into());
        state.select_all();
        state.begin_lookup().unwrap();
        let req = state.choose_release("a").unwrap();
        state.finish_release(req.request, Ok(fixture_release()));
        state.discard_release();
        assert!(state.lookup().unwrap().plan.is_none());
        assert_eq!(
            state.file(first).unwrap().edited.get(keys::GENRE),
            Some("Mine")
        );
        assert_eq!(
            state.file(first).unwrap().edited.get(keys::ALBUM),
            Some("ii")
        );
        assert!(state.files().iter().all(|f| f.assignment.is_none()));
    }

    #[test]
    fn the_release_cover_replaces_the_front_cover_of_matched_files_only() {
        let mut state = moderat_state();
        state.begin_lookup().unwrap();
        let req = state.choose_release("a").unwrap();
        let mut release = fixture_release();
        release.cover_art_archive = Some(crate::online::musicbrainz::CoverArtArchive {
            front: true,
            back: false,
            count: 1,
        });
        state.finish_release(req.request, Ok(release));
        assert!(
            state.take_cover_request(false).is_none(),
            "setting off: no fetch"
        );
        let cover_req = state.take_cover_request(true).unwrap();
        assert!(state.take_cover_request(true).is_none(), "handed out once");
        let picture = EditPicture {
            kind: picture_type::FRONT_COVER,
            mime: "image/png".into(),
            description: String::new(),
            data: vec![1, 2, 3],
        };
        state.finish_cover(cover_req.request, Ok(Some(picture.clone())));
        for file in state.files() {
            assert_eq!(file.edited.pictures, vec![picture.clone()]);
        }
    }

    #[test]
    fn releases_without_a_front_cover_fetch_nothing() {
        let mut state = moderat_state();
        state.begin_lookup().unwrap();
        let req = state.choose_release("a").unwrap();
        let mut release = fixture_release();
        release.cover_art_archive = None;
        state.finish_release(req.request, Ok(release));
        assert!(state.take_cover_request(true).is_none());
        assert!(matches!(state.lookup().unwrap().cover, Remote::Ready(None)));
    }

    fn acoustid_result(score: f64, release: &str, recording: &str) -> AcoustIdResult {
        use crate::online::acoustid::{AcoustIdRecording, AcoustIdRelease};
        AcoustIdResult {
            id: format!("acoustid-{recording}"),
            score,
            recordings: vec![AcoustIdRecording {
                id: recording.into(),
                releases: vec![AcoustIdRelease {
                    id: release.into(),
                    title: "II".into(),
                    track_count: 11,
                    ..AcoustIdRelease::default()
                }],
                ..AcoustIdRecording::default()
            }],
        }
    }

    fn scan_result(results: Vec<AcoustIdResult>) -> ScanFileResult {
        ScanFileResult {
            fingerprint: Ok(Fingerprint::from_raw(200, vec![1, 2, 3])),
            lookup: Some(Ok(results)),
        }
    }

    #[test]
    fn an_acoustid_scan_picks_the_release_and_applies_it_with_the_acoustid() {
        let mut state = moderat_state();
        let start = state.begin_scan().unwrap();
        assert_eq!(start.files.len(), 3);
        let release = "00398749-1816-4c98-8a39-377627740365";
        let recordings = [
            "c35eb744-c525-44d0-ad21-b6af3e6caad2",
            "56ac9e2f-8b30-4b67-b5ec-c24251a5cb44",
            "ff1e4cc1-9049-4e48-bbdf-d5f3ade6d110",
        ];
        let mut step = ScanStep::Pending;
        for ((file, _), recording) in start.files.iter().zip(recordings) {
            assert!(matches!(step, ScanStep::Pending));
            step = state.finish_scan_file(
                start.generation,
                *file,
                scan_result(vec![acoustid_result(0.97, release, recording)]),
            );
        }
        let ScanStep::FetchRelease(req) = step else {
            panic!("expected a release to fetch, got {step:?}");
        };
        assert_eq!(req.release_id, release);
        assert_eq!(state.scan().unwrap().phase, ScanPhase::Resolving);
        assert_eq!(state.scan().unwrap().identified, 3);
        assert_eq!(state.lookup().unwrap().source, LookupSource::AcoustId);

        state.finish_release(req.request, Ok(fixture_release()));
        state.finish_scan_release();
        assert_eq!(state.scan().unwrap().phase, ScanPhase::Done);
        for file in state.files() {
            assert!(
                file.edited
                    .get(keys::ACOUSTID_ID)
                    .unwrap()
                    .starts_with("acoustid-")
            );
            assert!(file.fingerprint.is_some());
            assert!(file.assignment.is_some());
            assert_eq!(file.edited.get(keys::MUSICBRAINZ_ALBUMID), Some(release));
        }
    }

    #[test]
    fn a_scan_with_no_usable_hit_ends_without_a_match() {
        let mut state = moderat_state();
        let start = state.begin_scan().unwrap();
        let mut last = ScanStep::Pending;
        for (file, _) in &start.files {
            last = state.finish_scan_file(start.generation, *file, scan_result(vec![]));
        }
        assert!(matches!(last, ScanStep::NoMatch));
        assert_eq!(state.scan().unwrap().phase, ScanPhase::NoMatch);
        assert!(state.lookup().is_none());
        // The fingerprints are kept for a later submission.
        assert!(state.files().iter().all(|f| f.fingerprint.is_some()));
    }

    #[test]
    fn failed_files_are_reported_and_do_not_stop_the_scan() {
        let mut state = moderat_state();
        let start = state.begin_scan().unwrap();
        let (bad, _) = start.files[0];
        state.finish_scan_file(
            start.generation,
            bad,
            ScanFileResult {
                fingerprint: Err("cannot decode".into()),
                lookup: None,
            },
        );
        assert_eq!(state.scan().unwrap().failures.len(), 1);
        assert_eq!(state.scan().unwrap().done, 1);
    }

    #[test]
    fn a_cancelled_scan_ignores_late_results() {
        let mut state = moderat_state();
        let start = state.begin_scan().unwrap();
        state.cancel_scan();
        let step = state.finish_scan_file(start.generation, start.files[0].0, scan_result(vec![]));
        assert!(matches!(step, ScanStep::Pending));
        assert_eq!(state.scan().unwrap().done, 0);
        assert_eq!(state.scan().unwrap().phase, ScanPhase::Cancelled);
    }

    #[test]
    fn submission_needs_a_recording_id_and_tracks_per_file_results() {
        let mut state = moderat_state();
        assert_eq!(state.submittable(), 0);
        state.select_all();
        let ids = state.display_order();
        // Give two of the three files a recording id.
        state.select_only(ids[0]);
        state.add_field.custom = keys::MUSICBRAINZ_RECORDINGID.into();
        state.add_field.value = "c35eb744-c525-44d0-ad21-b6af3e6caad2".into();
        state.commit_add_field().unwrap();
        state.select_only(ids[1]);
        state.add_field.custom = keys::MUSICBRAINZ_RECORDINGID.into();
        state.add_field.value = "not-an-mbid".into();
        state.commit_add_field().unwrap();
        state.select_all();
        assert_eq!(state.submittable(), 1, "only a well-formed MBID counts");

        let start = state.begin_submit().unwrap();
        assert_eq!(start.jobs.len(), 1);
        assert!(start.jobs[0].fingerprint.is_none());
        assert!(state.is_submitting());
        assert_eq!(
            state.file(ids[0]).unwrap().submission,
            SubmissionState::Pending
        );
        assert!(state.begin_submit().is_none());

        state.finish_submit(
            start.generation,
            vec![SubmitResult {
                file: ids[0],
                fingerprint: Some(Fingerprint::from_raw(200, vec![1])),
                outcome: Ok("pending".into()),
            }],
        );
        let file = state.file(ids[0]).unwrap();
        assert_eq!(
            file.submission,
            SubmissionState::Submitted {
                status: "pending".into()
            }
        );
        assert!(
            file.fingerprint.is_some(),
            "a fingerprint computed for the submission is kept"
        );
        assert!(!state.is_submitting());
    }

    #[test]
    fn an_aborted_submission_marks_its_files_failed() {
        let mut state = moderat_state();
        let first = state.display_order()[0];
        state.select_only(first);
        state.add_field.custom = keys::MUSICBRAINZ_RECORDINGID.into();
        state.add_field.value = "c35eb744-c525-44d0-ad21-b6af3e6caad2".into();
        state.commit_add_field().unwrap();
        let start = state.begin_submit().unwrap();
        state.abort_submit(start.generation, "no key");
        assert_eq!(
            state.file(first).unwrap().submission,
            SubmissionState::Failed("no key".into())
        );
        assert!(!state.is_submitting());
    }

    #[test]
    fn library_album_keys_follow_the_scanners_artist_fallback() {
        let mut tags = TagSet::new();
        tags.set_one(keys::ALBUM, "Mix");
        tags.set_one(keys::ARTIST, "Solo");
        assert_eq!(library_album_key(&tags), CoverArt::album_key("Solo", "Mix"));
        tags.set_one(keys::ALBUMARTIST, "Band");
        assert_eq!(library_album_key(&tags), CoverArt::album_key("Band", "Mix"));
        tags.remove(keys::ALBUMARTIST);
        tags.set_one(keys::COMPILATION, "1");
        assert_eq!(
            library_album_key(&tags),
            CoverArt::album_key("Various Artists", "Mix")
        );
    }

    #[test]
    fn target_files_are_the_selected_editable_files_or_all_of_them() {
        let mut state = state_with_album(3);
        assert_eq!(
            state.target_files().len(),
            3,
            "nothing selected: every file"
        );
        let first = state.display_order()[0];
        state.select_only(first);
        assert_eq!(state.target_files(), vec![first]);
    }

    #[test]
    fn selecting_clusters() {
        let mut state = TaggerState::new();
        let req = state
            .begin_load(
                vec!["/1.flac".into(), "/2.flac".into(), "/3.flac".into()],
                false,
                None,
            )
            .unwrap();
        state.finish_load(
            req.generation,
            vec![
                loaded("/1.flac", &[(keys::ALBUM, "A")]),
                loaded("/2.flac", &[(keys::ALBUM, "A")]),
                loaded("/3.flac", &[(keys::ALBUM, "B")]),
            ],
        );
        let a = state.clusters()[0].key.clone();
        state.select_cluster(&a);
        assert_eq!(state.selected_count(), 2);
        state.toggle_cluster(&a);
        assert_eq!(state.selected_count(), 0);
        state.toggle_cluster(&a);
        state.toggle(state.clusters()[1].files[0]);
        assert_eq!(state.selected_count(), 3);
        state.select_only(state.clusters()[1].files[0]);
        assert_eq!(state.selected_count(), 1);
    }

    #[test]
    fn unused_test_helpers_compile() {
        // `track` and `file` come from the shared test helpers.
        let f = track(1, "A", "B", 1, "t");
        assert!(f.is_editable());
    }
}
