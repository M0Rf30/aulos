// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Update logic of the tag editor page: everything the view and the
//! "Edit tags…" menu entries can trigger, and the background I/O behind it.
//!
//! The model ([`crate::tagger::TaggerState`]) decides what happens; this
//! module only starts the work a transition asks for (always off the UI
//! thread, inside `spawn_blocking`) and feeds the result back as a
//! [`TaggerEvent`]. Every job carries the generation / request id the state
//! handed out, so a result that arrives after its files were removed or its
//! lookup replaced is dropped by the state instead of landing on the wrong
//! files.

mod jobs;

use super::{AppModel, ContextPage, Message, Page};
use crate::fl;
use crate::library::tagio::{self, EditPicture, TagSet};
use crate::library::{CoverArt, LibraryDb, LibraryScanner, Track, fingerprint, tags};
use crate::online::acoustid::{self, Submission};
use crate::online::musicbrainz::{self, FileForMatch};
use crate::tagger::edit::{self, PICTURE_KINDS};
use crate::tagger::state::{CoverRequest, ReleaseRequest, SearchRequest};
use crate::tagger::{
    ClusterKey, FileId, LoadRequest, LoadedFile, SaveJob, SaveSummary, ScanFileResult, ScanStep,
    SubmitJob, SubmitResult, SubmitStart, TagAction, TaggerEvent, TaggerMessage, is_taggable,
};
use cosmic::Task;
use jobs::{LOAD_CHUNK, LOOKUP_MIN_GAP, SUBMIT_BATCH};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::{Mutex, Semaphore};

/// Files being read at once (in chunks of [`LOAD_CHUNK`]).
static LOAD_PERMITS: Semaphore = Semaphore::const_new(4);
/// Files being written at once.
static SAVE_PERMITS: Semaphore = Semaphore::const_new(4);
/// Files being fingerprinted at once.
static FINGERPRINT_PERMITS: Semaphore = Semaphore::const_new(3);
/// When the last AcoustID lookup started, shared by every scan.
static LAST_LOOKUP: Mutex<Option<Instant>> = Mutex::const_new(None);
/// The scan whose files may still be fingerprinted; `0` when none (a
/// cancelled scan's queued files stop at their next checkpoint).
static ACTIVE_SCAN: AtomicU64 = AtomicU64::new(0);

type Action = cosmic::Action<Message>;

fn event(event: TaggerEvent) -> Action {
    cosmic::Action::App(Message::TaggerEvent(event))
}

/// Runs blocking work off the UI thread.
async fn blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| format!("background task failed: {e}"))?
}

/// Waits until an AcoustID lookup may start, and claims the slot.
async fn wait_for_lookup_slot() {
    let mut last = LAST_LOOKUP.lock().await;
    let wait = jobs::throttle_wait(*last, Instant::now(), LOOKUP_MIN_GAP);
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
    *last = Some(Instant::now());
}

impl AppModel {
    // --- Entry points ---------------------------------------------------------

    /// Opens the tag editor on `tracks` (those of the local library) and
    /// runs `action` once they are loaded.
    pub(super) fn open_in_tagger(&mut self, tracks: Vec<Track>, action: TagAction) -> Task<Action> {
        let paths: Vec<PathBuf> = tracks
            .iter()
            .filter(|t| is_taggable(t))
            .map(|t| t.path.clone())
            .collect();
        if paths.is_empty() {
            return Task::none();
        }
        let show = self.show_tagger_page();
        let load = self.start_load(paths, true, Some(action));
        let ready = self.run_ready_action();
        let thumbs = self.decode_missing_thumbs();
        Task::batch([show, load, ready, thumbs])
    }

    /// Switches to the tag editor page.
    fn show_tagger_page(&mut self) -> Task<Action> {
        let entity = self
            .nav
            .iter()
            .find(|&id| self.nav.data::<Page>(id) == Some(&Page::Tagger));
        match entity {
            Some(id) => self.select_nav(id),
            None => Task::none(),
        }
    }

    /// Handles the user's actions on the tag editor page.
    pub(super) fn update_tagger(&mut self, msg: TaggerMessage) -> Task<Action> {
        let task = self.handle_tagger_message(msg);
        let thumbs = self.decode_missing_thumbs();
        Task::batch([task, thumbs])
    }

    /// Handles the results of the page's background work.
    pub(super) fn update_tagger_event(&mut self, ev: TaggerEvent) -> Task<Action> {
        let task = self.handle_tagger_event(ev);
        let thumbs = self.decode_missing_thumbs();
        Task::batch([task, thumbs])
    }

    fn handle_tagger_message(&mut self, msg: TaggerMessage) -> Task<Action> {
        use TaggerMessage as M;
        match msg {
            // --- Files ---
            M::AddFiles => pick_files(),
            M::AddFolder => pick_folder(),
            M::SelectFile(id) => {
                self.tagger.select_only(id);
                Task::none()
            }
            M::ToggleFile(id) => {
                self.tagger.toggle(id);
                Task::none()
            }
            M::SelectCluster(key) => {
                self.tagger.select_cluster(&key);
                Task::none()
            }
            M::ToggleCluster(key) => {
                self.tagger.toggle_cluster(&key);
                Task::none()
            }
            M::SelectAll => {
                self.tagger.select_all();
                Task::none()
            }
            M::ClearSelection => {
                self.tagger.clear_selection();
                Task::none()
            }
            M::RemoveSelected => {
                self.tagger.request_remove_selected();
                Task::none()
            }
            M::ClearAll => {
                self.tagger.request_clear();
                Task::none()
            }
            M::ConfirmDiscard => {
                self.tagger.accept_confirm();
                Task::none()
            }
            M::CancelDiscard => {
                self.tagger.cancel_confirm();
                Task::none()
            }

            // --- Metadata table ---
            M::EditField { key, text } => {
                self.tagger.set_field_text(&key, text);
                Task::none()
            }
            M::RevertField(key) => {
                self.tagger.revert_field(&key);
                Task::none()
            }
            M::DeleteField(key) => {
                self.tagger.delete_field(&key);
                Task::none()
            }
            M::AddFieldKnown(index) => {
                self.tagger.set_add_known(Some(index));
                Task::none()
            }
            M::AddFieldCustom(name) => {
                self.tagger.set_add_custom(name);
                Task::none()
            }
            M::AddFieldValue(value) => {
                self.tagger.set_add_value(value);
                Task::none()
            }
            M::AddFieldCommit => {
                // A refused form keeps its error in the state, shown by the view.
                let _ = self.tagger.commit_add_field();
                Task::none()
            }

            // --- Cover art ---
            M::AddPicture => pick_image(self.tagger.new_picture_kind()),
            M::NewPictureKind(index) => {
                if let Some(&kind) = PICTURE_KINDS.get(index) {
                    self.tagger.set_new_picture_kind(kind);
                }
                Task::none()
            }
            M::RemovePicture { hash, kind } => {
                self.tagger.remove_picture(hash, kind);
                Task::none()
            }
            M::SetPictureKind {
                hash,
                kind,
                new_kind,
            } => {
                self.tagger.set_picture_kind(hash, kind, new_kind);
                Task::none()
            }
            M::RevertPictures => {
                self.tagger.revert_pictures();
                Task::none()
            }
            M::FetchCover => match self.tagger.fetch_cover_for_selection() {
                Some(request) => cover_task(request),
                None => Task::none(),
            },

            // --- Save / revert ---
            M::Save => self.start_save(),
            M::RevertSelected => {
                self.tagger.revert_selected();
                Task::none()
            }
            M::RevertAll => {
                self.tagger.revert_all();
                Task::none()
            }

            // --- MusicBrainz ---
            M::Lookup => self.start_lookup(),
            M::LookupCluster(key) => {
                self.tagger.select_cluster(&key);
                self.start_lookup()
            }
            M::QueryArtist(artist) => {
                self.tagger.set_query_artist(artist);
                Task::none()
            }
            M::QueryAlbum(album) => {
                self.tagger.set_query_album(album);
                Task::none()
            }
            M::SearchAgain => match self.tagger.search_again() {
                Some(request) => search_task(request),
                None => Task::none(),
            },
            M::ChooseRelease(id) => match self.tagger.choose_release(&id) {
                Some(request) => release_task(request),
                None => Task::none(),
            },
            M::DiscardRelease => {
                self.tagger.discard_release();
                Task::none()
            }
            M::CloseLookup => {
                self.tagger.close_lookup();
                Task::none()
            }
            M::OpenReleasePage(id) => {
                self.report_open(musicbrainz::open_release_page(&id));
                Task::none()
            }
            M::OpenRecordingPage(id) => {
                self.report_open(musicbrainz::open_recording_page(&id));
                Task::none()
            }
            M::OpenAcoustIdPage(id) => {
                self.report_open(
                    open::that_detached(acoustid::track_url(&id))
                        .map_err(|e| format!("Could not open the browser: {e}")),
                );
                Task::none()
            }
            M::AddAsRelease(cluster) => self.start_seed(cluster),

            // --- AcoustID ---
            M::Scan => self.start_scan(),
            M::ScanCluster(key) => {
                self.tagger.select_cluster(&key);
                self.start_scan()
            }
            M::CancelScan => {
                ACTIVE_SCAN.store(0, Ordering::Relaxed);
                self.tagger.cancel_scan();
                Task::none()
            }
            M::SubmitAcoustIds => self.start_submit(),

            // --- Page chrome ---
            M::OpenSettings => {
                if self.context_page == ContextPage::Settings && self.core.window.show_context {
                    Task::none()
                } else {
                    self.handle_message(Message::ToggleContextPage(ContextPage::Settings))
                }
            }
            M::DismissNotice => {
                self.tagger.dismiss_notice();
                Task::none()
            }
        }
    }

    fn handle_tagger_event(&mut self, ev: TaggerEvent) -> Task<Action> {
        use TaggerEvent as E;
        match ev {
            E::FilesPicked(Ok(paths)) => {
                if paths.is_empty() {
                    return Task::none();
                }
                // Files added to an empty editor are selected, so the table
                // shows them right away; later additions keep the selection.
                let select = self.tagger.is_empty();
                let load = self.start_load(paths, select, None);
                let ready = self.run_ready_action();
                Task::batch([load, ready])
            }
            E::FilesPicked(Err(error)) => {
                self.tagger
                    .set_notice(true, fl!("tagger-notice-pick-failed", error = error));
                Task::none()
            }
            E::FolderPicked(Ok(dir)) => cosmic::task::future(async move {
                let files = blocking(move || Ok(jobs::audio_files_under(&dir))).await;
                event(E::FilesPicked(match files {
                    Ok(files) if files.is_empty() => Err(fl!("tagger-notice-no-audio")),
                    other => other,
                }))
            }),
            E::FolderPicked(Err(error)) => {
                self.tagger
                    .set_notice(true, fl!("tagger-notice-pick-failed", error = error));
                Task::none()
            }
            E::ImagePicked(Ok(picture)) => {
                self.tagger.add_picture(&picture);
                Task::none()
            }
            E::ImagePicked(Err(error)) => {
                self.tagger
                    .set_notice(true, fl!("tagger-notice-image-failed", error = error));
                Task::none()
            }
            E::FilesLoaded { generation, files } => {
                self.tagger.finish_load(generation, files);
                self.run_ready_action()
            }
            E::ThumbDecoded { hash, pixels } => {
                self.tagger.set_thumb(hash, pixels);
                Task::none()
            }
            E::SearchFinished { request, result } => {
                self.tagger.finish_search(request, result);
                Task::none()
            }
            E::ReleaseFetched { request, result } => {
                self.tagger
                    .finish_release(request, result.map(|release| *release));
                if self
                    .tagger
                    .lookup()
                    .is_some_and(|l| l.source == crate::tagger::LookupSource::AcoustId)
                {
                    self.tagger.finish_scan_release();
                }
                match self
                    .tagger
                    .take_cover_request(self.config.tagger_use_caa_cover)
                {
                    Some(request) => cover_task(request),
                    None => Task::none(),
                }
            }
            E::CoverFetched { request, result } => {
                self.tagger.finish_cover(request, result);
                Task::none()
            }
            E::FileScanned {
                generation,
                file,
                result,
            } => match self.tagger.finish_scan_file(generation, file, result) {
                ScanStep::Pending => Task::none(),
                ScanStep::FetchRelease(request) => release_task(request),
                ScanStep::NoMatch => {
                    self.tagger
                        .set_notice(false, fl!("tagger-notice-scan-no-match"));
                    Task::none()
                }
            },
            E::FileSaved { file, result } => match self.tagger.finish_save(file, result) {
                Some(summary) => self.after_save(summary),
                None => Task::none(),
            },
            E::Submitted {
                generation,
                results,
            } => {
                let ok = results.iter().filter(|r| r.outcome.is_ok()).count();
                let failed = results.len() - ok;
                self.tagger.finish_submit(generation, results);
                if failed == 0 {
                    self.tagger
                        .set_notice(false, fl!("tagger-notice-submitted", count = ok));
                } else {
                    self.tagger.set_notice(
                        true,
                        fl!("tagger-notice-submit-partial", ok = ok, failed = failed),
                    );
                }
                Task::none()
            }
            E::SubmitAborted { generation, error } => {
                self.tagger.abort_submit(generation, &error);
                self.tagger.set_notice(true, error);
                Task::none()
            }
            E::SeedOpened(Ok(())) => Task::none(),
            E::SeedOpened(Err(error)) => {
                self.tagger
                    .set_notice(true, fl!("tagger-notice-seed-failed", error = error));
                Task::none()
            }
        }
    }

    fn report_open(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.tagger
                .set_notice(true, fl!("tagger-notice-open-failed", error = error));
        }
    }

    // --- Loading --------------------------------------------------------------

    fn start_load(
        &mut self,
        paths: Vec<PathBuf>,
        select: bool,
        action: Option<TagAction>,
    ) -> Task<Action> {
        match self.tagger.begin_load(paths, select, action) {
            Some(request) => load_tasks(request),
            None => Task::none(),
        }
    }

    /// Runs the action that was waiting for the files to be read.
    fn run_ready_action(&mut self) -> Task<Action> {
        match self.tagger.take_ready_action() {
            Some(TagAction::Edit) | None => Task::none(),
            Some(TagAction::LookUp) => self.start_lookup(),
            Some(TagAction::AcoustId) => self.start_scan(),
        }
    }

    /// Decodes the thumbnails of pictures the table shows for the first time.
    fn decode_missing_thumbs(&mut self) -> Task<Action> {
        let wanted = self.tagger.missing_thumbs();
        Task::batch(wanted.into_iter().map(|(hash, bytes)| {
            cosmic::task::future(async move {
                let pixels =
                    tokio::task::spawn_blocking(move || CoverArt::decode_thumbnail(&bytes, 160))
                        .await
                        .ok()
                        .flatten();
                event(TaggerEvent::ThumbDecoded { hash, pixels })
            })
        }))
    }

    // --- MusicBrainz ----------------------------------------------------------

    fn start_lookup(&mut self) -> Task<Action> {
        match self.tagger.begin_lookup() {
            Some(request) => search_task(request),
            None => {
                if self.tagger.lookup().is_none() {
                    self.tagger
                        .set_notice(true, fl!("tagger-notice-nothing-to-look-up"));
                }
                Task::none()
            }
        }
    }

    fn start_seed(&mut self, cluster: Option<ClusterKey>) -> Task<Action> {
        let ids: Vec<FileId> = match cluster {
            Some(key) => self
                .tagger
                .clusters()
                .iter()
                .find(|c| c.key == key)
                .map(|c| c.files.clone())
                .unwrap_or_default(),
            None => {
                let selected: Vec<FileId> =
                    self.tagger.selected_files().iter().map(|f| f.id).collect();
                if selected.is_empty() {
                    self.tagger.display_order()
                } else {
                    selected
                }
            }
        };
        let files: Vec<(TagSet, Option<f64>, Option<String>)> = ids
            .iter()
            .filter_map(|id| self.tagger.file(*id))
            .map(|f| {
                let mut tags = f.edited.clone();
                tags.pictures.clear();
                let stem = f.path.file_stem().map(|s| s.to_string_lossy().into_owned());
                (tags, f.duration_secs, stem)
            })
            .collect();
        if files.is_empty() {
            self.tagger
                .set_notice(true, fl!("tagger-notice-nothing-to-add"));
            return Task::none();
        }
        cosmic::task::future(async move {
            let result = blocking(move || {
                let for_match: Vec<FileForMatch<'_>> = files
                    .iter()
                    .map(|(tags, duration_secs, stem)| FileForMatch {
                        tags,
                        duration_secs: *duration_secs,
                        file_stem: stem.as_deref(),
                    })
                    .collect();
                let page = musicbrainz::seed_release_add(&for_match)?;
                open::that_detached(&page).map_err(|e| format!("Could not open the browser: {e}"))
            })
            .await;
            event(TaggerEvent::SeedOpened(result))
        })
    }

    // --- AcoustID -------------------------------------------------------------

    fn start_scan(&mut self) -> Task<Action> {
        let Some(key) = self.config.acoustid_key().map(str::to_owned) else {
            self.tagger
                .set_notice(true, fl!("tagger-notice-no-acoustid-key"));
            return Task::none();
        };
        let Some(start) = self.tagger.begin_scan() else {
            self.tagger
                .set_notice(true, fl!("tagger-notice-nothing-to-scan"));
            return Task::none();
        };
        ACTIVE_SCAN.store(start.generation, Ordering::Relaxed);
        let generation = start.generation;
        Task::batch(start.files.into_iter().map(|(file, path)| {
            let key = key.clone();
            cosmic::task::future(async move {
                let result = scan_file(generation, path, key).await;
                event(TaggerEvent::FileScanned {
                    generation,
                    file,
                    result,
                })
            })
        }))
    }

    fn start_submit(&mut self) -> Task<Action> {
        let Some(start) = self.tagger.begin_submit() else {
            self.tagger
                .set_notice(true, fl!("tagger-notice-nothing-to-submit"));
            return Task::none();
        };
        let Some(app_key) = self.config.acoustid_key().map(str::to_owned) else {
            let error = fl!("tagger-notice-no-acoustid-key");
            self.tagger.abort_submit(start.generation, &error);
            self.tagger.set_notice(true, error);
            return Task::none();
        };
        cosmic::task::future(async move { event(run_submit(start, app_key).await) })
    }

    // --- Saving ---------------------------------------------------------------

    fn start_save(&mut self) -> Task<Action> {
        let jobs = self.tagger.begin_save();
        if jobs.is_empty() {
            self.tagger
                .set_notice(false, fl!("tagger-notice-nothing-to-save"));
            return Task::none();
        }
        Task::batch(jobs.into_iter().map(save_task))
    }

    /// The last write of a save batch finished: report it and bring the
    /// library (and its covers) up to date with the written files.
    fn after_save(&mut self, summary: SaveSummary) -> Task<Action> {
        let saved = summary.written.len();
        if summary.failed == 0 {
            self.tagger
                .set_notice(false, fl!("tagger-notice-saved", count = saved));
        } else {
            self.tagger.set_notice(
                true,
                fl!(
                    "tagger-notice-save-partial",
                    saved = saved,
                    failed = summary.failed
                ),
            );
        }
        // Covers of the albums the files left or joined are stale now.
        for key in &summary.album_keys {
            self.cover_art_bytes.remove(key);
        }
        if summary.written.is_empty() {
            return Task::none();
        }
        let Some(provider) = self.registry.active_shared() else {
            return Task::none();
        };
        if provider.provider_type() != crate::provider::ProviderType::Local {
            return Task::none();
        }
        self.library_scanning = true;
        let generation = self.begin_reload_generation();
        let provider_id = provider.id().to_string();
        let SaveSummary {
            written,
            album_keys,
            ..
        } = summary;
        cosmic::task::future(async move {
            let count = tokio::task::spawn_blocking(move || refresh_library(&written, &album_keys))
                .await
                .unwrap_or(0);
            cosmic::Action::App(Message::LibraryScanComplete {
                generation,
                provider_id,
                count,
            })
        })
    }
}

/// Re-reads the written files into the library database, after forgetting
/// the cached covers of the albums they belonged to. Returns the number of
/// tracks updated.
fn refresh_library(written: &[PathBuf], album_keys: &[String]) -> usize {
    let db_path = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("aulos")
        .join("library.db");
    let db = match LibraryDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            tracing::error!("Failed to open DB after saving tags: {e}");
            return 0;
        }
    };
    for key in album_keys {
        if let Err(e) = db.remove_cached_cover(key) {
            tracing::warn!("could not drop cached cover {key}: {e}");
        }
    }
    LibraryScanner::scan_paths(&db, written).unwrap_or_else(|e| {
        tracing::error!("scan_paths after saving tags failed: {e}");
        0
    })
}

// --- Background jobs -------------------------------------------------------------

fn pick_files() -> Task<Action> {
    cosmic::task::future(async {
        let result = async {
            use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

            let mut filter = FileFilter::new("Audio files");
            for ext in crate::player::engine::decoder::SUPPORTED_EXTENSIONS {
                filter = filter.glob(&format!("*.{ext}"));
            }
            let selected = match SelectedFiles::open_file()
                .title("Add files to the tag editor")
                .multiple(true)
                .modal(true)
                .filter(filter)
                .send()
                .await
                .map_err(|e| format!("Portal request failed: {e}"))?
                .response()
            {
                Ok(selected) => selected,
                Err(e) if is_cancelled(&e) => return Ok(Vec::new()),
                Err(e) => return Err(format!("Portal response failed: {e}")),
            };
            selected
                .uris()
                .iter()
                .map(|uri| jobs::uri_to_path(uri.as_str()))
                .collect()
        }
        .await;
        event(TaggerEvent::FilesPicked(result))
    })
}

fn pick_folder() -> Task<Action> {
    cosmic::task::future(async {
        let result = async {
            use ashpd::desktop::file_chooser::SelectedFiles;

            let selected = match SelectedFiles::open_file()
                .title("Add a folder to the tag editor")
                .directory(true)
                .modal(true)
                .send()
                .await
                .map_err(|e| format!("Portal request failed: {e}"))?
                .response()
            {
                Ok(selected) => selected,
                Err(e) if is_cancelled(&e) => return Ok(None),
                Err(e) => return Err(format!("Portal response failed: {e}")),
            };
            selected
                .uris()
                .first()
                .map(|uri| jobs::uri_to_path(uri.as_str()))
                .transpose()
        }
        .await;
        match result {
            Ok(Some(dir)) => event(TaggerEvent::FolderPicked(Ok(dir))),
            // Cancelled: nothing to add.
            Ok(None) => event(TaggerEvent::FilesPicked(Ok(Vec::new()))),
            Err(e) => event(TaggerEvent::FolderPicked(Err(e))),
        }
    })
}

fn pick_image(kind: u8) -> Task<Action> {
    cosmic::task::future(async move {
        let picked = async {
            use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

            let mut filter = FileFilter::new("Images");
            for ext in ["png", "jpg", "jpeg", "webp", "bmp"] {
                filter = filter.glob(&format!("*.{ext}"));
            }
            let selected = match SelectedFiles::open_file()
                .title("Choose an image")
                .modal(true)
                .filter(filter)
                .send()
                .await
                .map_err(|e| format!("Portal request failed: {e}"))?
                .response()
            {
                Ok(selected) => selected,
                Err(e) if is_cancelled(&e) => return Ok(None),
                Err(e) => return Err(format!("Portal response failed: {e}")),
            };
            let Some(uri) = selected.uris().first() else {
                return Ok(None);
            };
            Ok(Some(jobs::uri_to_path(uri.as_str())?))
        }
        .await;
        match picked {
            Ok(Some(path)) => {
                let result = blocking(move || {
                    let data = std::fs::read(&path)
                        .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
                    edit::picture_from_bytes(data, kind)
                })
                .await;
                event(TaggerEvent::ImagePicked(result))
            }
            // Cancelled: nothing to add. A pending thumbnail pass is a no-op.
            Ok(None) => event(TaggerEvent::FilesPicked(Ok(Vec::new()))),
            Err(e) => event(TaggerEvent::ImagePicked(Err(e))),
        }
    })
}

fn is_cancelled(error: &ashpd::Error) -> bool {
    matches!(
        error,
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)
    )
}

/// Reads the files of one load request, in chunks of [`LOAD_CHUNK`], a few
/// chunks at a time.
fn load_tasks(request: LoadRequest) -> Task<Action> {
    let LoadRequest { generation, paths } = request;
    Task::batch(
        jobs::split_batches(paths, LOAD_CHUNK)
            .into_iter()
            .map(move |chunk| {
                cosmic::task::future(async move {
                    let permit = LOAD_PERMITS.acquire().await;
                    let paths = chunk.clone();
                    let files = match tokio::task::spawn_blocking(move || {
                        chunk.into_iter().map(read_file).collect::<Vec<_>>()
                    })
                    .await
                    {
                        Ok(files) => files,
                        // Every requested file must be answered, or the
                        // editor would wait for it forever.
                        Err(e) => paths
                            .into_iter()
                            .map(|path| LoadedFile {
                                path,
                                read: Err(format!("background task failed: {e}")),
                                duration_secs: None,
                            })
                            .collect(),
                    };
                    drop(permit);
                    event(TaggerEvent::FilesLoaded { generation, files })
                })
            }),
    )
}

fn read_file(path: PathBuf) -> LoadedFile {
    let read = tagio::read(&path).map_err(|e| e.to_string());
    let duration_secs = if read.is_ok() {
        tags::probe(&path, false)
            .map(|p| p.properties.duration.as_secs_f64())
            .filter(|d| *d > 0.0)
    } else {
        None
    };
    LoadedFile {
        path,
        read,
        duration_secs,
    }
}

fn search_task(request: SearchRequest) -> Task<Action> {
    cosmic::task::future(async move {
        let SearchRequest { request, query } = request;
        let result =
            blocking(move || jobs::search_with_retry(&query, musicbrainz::search_releases)).await;
        event(TaggerEvent::SearchFinished { request, result })
    })
}

fn release_task(request: ReleaseRequest) -> Task<Action> {
    cosmic::task::future(async move {
        let ReleaseRequest {
            request,
            release_id,
        } = request;
        let result = blocking(move || musicbrainz::release(&release_id)).await;
        event(TaggerEvent::ReleaseFetched {
            request,
            result: result.map(Box::new),
        })
    })
}

fn cover_task(request: CoverRequest) -> Task<Action> {
    cosmic::task::future(async move {
        let CoverRequest {
            request,
            release_id,
        } = request;
        let result: Result<Option<EditPicture>, String> =
            blocking(move || musicbrainz::fetch_front_cover(&release_id)).await;
        event(TaggerEvent::CoverFetched { request, result })
    })
}

fn save_task(job: SaveJob) -> Task<Action> {
    cosmic::task::future(async move {
        let SaveJob { file, path, tags } = job;
        let permit = SAVE_PERMITS.acquire().await;
        let result = blocking(move || tagio::write(&path, &tags).map_err(|e| e.to_string())).await;
        drop(permit);
        event(TaggerEvent::FileSaved { file, result })
    })
}

/// Fingerprints one file and looks it up. A scan that was cancelled (or
/// replaced) while the file waited stops without doing the work.
async fn scan_file(generation: u64, path: PathBuf, key: String) -> ScanFileResult {
    let cancelled = || ScanFileResult {
        fingerprint: Err("Scan cancelled".to_owned()),
        lookup: None,
    };
    let permit = FINGERPRINT_PERMITS.acquire().await;
    if ACTIVE_SCAN.load(Ordering::Relaxed) != generation {
        return cancelled();
    }
    let fingerprint = blocking(move || fingerprint::compute(&path)).await;
    drop(permit);
    let fingerprint = match fingerprint {
        Ok(fp) => fp,
        Err(error) => {
            return ScanFileResult {
                fingerprint: Err(error),
                lookup: None,
            };
        }
    };
    wait_for_lookup_slot().await;
    if ACTIVE_SCAN.load(Ordering::Relaxed) != generation {
        return cancelled();
    }
    let sent = fingerprint.clone();
    let lookup = blocking(move || acoustid::lookup(&key, &sent)).await;
    ScanFileResult {
        fingerprint: Ok(fingerprint),
        lookup: Some(lookup),
    }
}

/// A file ready to submit, or why it can't be.
struct Prepared {
    /// Computed here (to keep for later), as opposed to taken from the file.
    computed: Option<fingerprint::Fingerprint>,
    submission: Result<Submission, String>,
}

async fn prepare_submission(job: SubmitJob) -> Prepared {
    let (fingerprint, computed) = match &job.fingerprint {
        Some(fp) => (Ok(fingerprint::Fingerprint::clone(fp)), None),
        None => {
            let permit = FINGERPRINT_PERMITS.acquire().await;
            let path = job.path.clone();
            let result = blocking(move || fingerprint::compute(&path)).await;
            drop(permit);
            match result {
                Ok(fp) => (Ok(fp.clone()), Some(fp)),
                Err(e) => (Err(e), None),
            }
        }
    };
    let submission = fingerprint.map(|fp| {
        let mut submission = Submission::from_tags(fp, &job.tags, Some(job.recording_id.clone()));
        submission.file_format = jobs::file_format(&job.path);
        submission.bitrate_kbps = None;
        submission
    });
    Prepared {
        computed,
        submission,
    }
}

/// Fingerprints what needs it, then sends everything to AcoustID in batches.
async fn run_submit(start: SubmitStart, app_key: String) -> TaggerEvent {
    let SubmitStart { generation, jobs } = start;
    let aborted = |error: String| TaggerEvent::SubmitAborted { generation, error };

    let user_key = match blocking(|| {
        crate::credentials::retrieve_password(crate::tagger::ACOUSTID_USER_KEY_ENTRY)
    })
    .await
    {
        Ok(Some(key)) if !key.trim().is_empty() => key,
        Ok(_) => return aborted(fl!("tagger-notice-no-user-key")),
        Err(e) => return aborted(fl!("tagger-notice-keyring-failed", error = e)),
    };

    let files: Vec<FileId> = jobs.iter().map(|j| j.file).collect();
    let mut set = tokio::task::JoinSet::new();
    for (index, job) in jobs.into_iter().enumerate() {
        set.spawn(async move { (index, prepare_submission(job).await) });
    }
    let mut prepared: Vec<Option<Prepared>> = files.iter().map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        if let Ok((index, item)) = joined {
            prepared[index] = Some(item);
        }
    }

    let mut results: Vec<SubmitResult> = Vec::with_capacity(files.len());
    let mut ready: Vec<(FileId, Option<fingerprint::Fingerprint>, Submission)> = Vec::new();
    for (file, item) in files.into_iter().zip(prepared) {
        match item {
            Some(Prepared {
                computed,
                submission: Ok(submission),
            }) => ready.push((file, computed, submission)),
            Some(Prepared {
                submission: Err(error),
                ..
            }) => results.push(SubmitResult {
                file,
                fingerprint: None,
                outcome: Err(error),
            }),
            None => results.push(SubmitResult {
                file,
                fingerprint: None,
                outcome: Err("The fingerprinting task failed".to_owned()),
            }),
        }
    }

    for batch in jobs::split_batches(ready, SUBMIT_BATCH) {
        let (meta, submissions): (Vec<_>, Vec<_>) = batch
            .into_iter()
            .map(|(file, computed, submission)| ((file, computed), submission))
            .unzip();
        let (app_key, user_key) = (app_key.clone(), user_key.clone());
        let response = blocking(move || acoustid::submit(&app_key, &user_key, &submissions)).await;
        results.extend(jobs::map_submit_response(meta, response));
    }
    TaggerEvent::Submitted {
        generation,
        results,
    }
}
