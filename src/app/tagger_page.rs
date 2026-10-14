// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Update logic of the tag editor page: everything the view and the
//! "Edit tags…" menu entries can trigger, and the portal pickers.
//!
//! The model ([`aulos_core::tagger::TaggerState`]) decides what happens; this
//! module only routes messages and events, asks for files and starts the
//! jobs a transition asks for ([`aulos_core::tagger::work`], each resolving
//! to a [`TaggerEvent`] fed back into the model).

use super::{AppModel, ContextPage, Message, Page};
use aulos_core::fl;
use aulos_core::library::Track;
use aulos_core::online::{acoustid, musicbrainz};
use aulos_core::tagger::edit::PICTURE_KINDS;
use aulos_core::tagger::{
    ClusterKey, SaveSummary, ScanStep, TagAction, TaggerEvent, TaggerMessage, is_taggable, jobs,
    work,
};
use cosmic::Task;
use std::path::PathBuf;

type Action = cosmic::Action<Message>;

fn event(event: TaggerEvent) -> Action {
    cosmic::Action::App(Message::TaggerEvent(event))
}

/// Runs one of the editor's background jobs, feeding its event back.
fn run(job: impl Future<Output = TaggerEvent> + Send + 'static) -> Task<Action> {
    cosmic::task::future(async move { event(job.await) })
}

/// Runs a batch of background jobs side by side.
fn run_all<F>(batch: impl IntoIterator<Item = F>) -> Task<Action>
where
    F: Future<Output = TaggerEvent> + Send + 'static,
{
    Task::batch(batch.into_iter().map(run))
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
                self.session.tagger.select_only(id);
                Task::none()
            }
            M::ToggleFile(id) => {
                self.session.tagger.toggle(id);
                Task::none()
            }
            M::SelectCluster(key) => {
                self.session.tagger.select_cluster(&key);
                Task::none()
            }
            M::ToggleCluster(key) => {
                self.session.tagger.toggle_cluster(&key);
                Task::none()
            }
            M::SelectAll => {
                self.session.tagger.select_all();
                Task::none()
            }
            M::ClearSelection => {
                self.session.tagger.clear_selection();
                Task::none()
            }
            M::RemoveSelected => {
                self.session.tagger.request_remove_selected();
                Task::none()
            }
            M::ClearAll => {
                self.session.tagger.request_clear();
                Task::none()
            }
            M::ConfirmDiscard => {
                self.session.tagger.accept_confirm();
                Task::none()
            }
            M::CancelDiscard => {
                self.session.tagger.cancel_confirm();
                Task::none()
            }

            // --- Metadata table ---
            M::EditField { key, text } => {
                self.session.tagger.set_field_text(&key, text);
                Task::none()
            }
            M::RevertField(key) => {
                self.session.tagger.revert_field(&key);
                Task::none()
            }
            M::DeleteField(key) => {
                self.session.tagger.delete_field(&key);
                Task::none()
            }
            M::AddFieldKnown(index) => {
                self.session.tagger.set_add_known(Some(index));
                Task::none()
            }
            M::AddFieldCustom(name) => {
                self.session.tagger.set_add_custom(name);
                Task::none()
            }
            M::AddFieldValue(value) => {
                self.session.tagger.set_add_value(value);
                Task::none()
            }
            M::AddFieldCommit => {
                // A refused form keeps its error in the state, shown by the view.
                let _ = self.session.tagger.commit_add_field();
                Task::none()
            }

            // --- Cover art ---
            M::AddPicture => pick_image(self.session.tagger.new_picture_kind()),
            M::NewPictureKind(index) => {
                if let Some(&kind) = PICTURE_KINDS.get(index) {
                    self.session.tagger.set_new_picture_kind(kind);
                }
                Task::none()
            }
            M::RemovePicture { hash, kind } => {
                self.session.tagger.remove_picture(hash, kind);
                Task::none()
            }
            M::SetPictureKind {
                hash,
                kind,
                new_kind,
            } => {
                self.session.tagger.set_picture_kind(hash, kind, new_kind);
                Task::none()
            }
            M::RevertPictures => {
                self.session.tagger.revert_pictures();
                Task::none()
            }
            M::FetchCover => match self.session.tagger.fetch_cover_for_selection() {
                Some(request) => run(work::cover(request)),
                None => Task::none(),
            },

            // --- Save / revert ---
            M::Save => self.start_save(),
            M::RevertSelected => {
                self.session.tagger.revert_selected();
                Task::none()
            }
            M::RevertAll => {
                self.session.tagger.revert_all();
                Task::none()
            }

            // --- MusicBrainz ---
            M::Lookup => self.start_lookup(),
            M::LookupCluster(key) => {
                self.session.tagger.select_cluster(&key);
                self.start_lookup()
            }
            M::QueryArtist(artist) => {
                self.session.tagger.set_query_artist(artist);
                Task::none()
            }
            M::QueryAlbum(album) => {
                self.session.tagger.set_query_album(album);
                Task::none()
            }
            M::SearchAgain => match self.session.tagger.search_again() {
                Some(request) => run(work::search(request)),
                None => Task::none(),
            },
            M::ChooseRelease(id) => match self.session.tagger.choose_release(&id) {
                Some(request) => run(work::release(request)),
                None => Task::none(),
            },
            M::DiscardRelease => {
                self.session.tagger.discard_release();
                Task::none()
            }
            M::CloseLookup => {
                self.session.tagger.close_lookup();
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
                self.report_open(acoustid::open_track_page(&id));
                Task::none()
            }
            M::AddAsRelease(cluster) => self.start_seed(cluster),

            // --- AcoustID ---
            M::Scan => self.start_scan(),
            M::ScanCluster(key) => {
                self.session.tagger.select_cluster(&key);
                self.start_scan()
            }
            M::CancelScan => {
                work::cancel_scan();
                self.session.tagger.cancel_scan();
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
                self.session.tagger.dismiss_notice();
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
                let select = self.session.tagger.is_empty();
                let load = self.start_load(paths, select, None);
                let ready = self.run_ready_action();
                Task::batch([load, ready])
            }
            E::FilesPicked(Err(error)) => {
                self.session
                    .tagger
                    .set_notice(true, fl!("tagger-notice-pick-failed", error = error));
                Task::none()
            }
            E::FolderPicked(Ok(dir)) => run(work::folder_files(dir)),
            E::FolderPicked(Err(error)) => {
                self.session
                    .tagger
                    .set_notice(true, fl!("tagger-notice-pick-failed", error = error));
                Task::none()
            }
            E::ImagePicked(Ok(picture)) => {
                self.session.tagger.add_picture(&picture);
                Task::none()
            }
            E::ImagePicked(Err(error)) => {
                self.session
                    .tagger
                    .set_notice(true, fl!("tagger-notice-image-failed", error = error));
                Task::none()
            }
            E::FilesLoaded { generation, files } => {
                self.session.tagger.finish_load(generation, files);
                self.run_ready_action()
            }
            E::ThumbDecoded { hash, pixels } => {
                self.tagger_thumbs.insert(
                    hash,
                    pixels.map(|(w, h, rgba)| cosmic::widget::icon::from_raster_pixels(w, h, rgba)),
                );
                Task::none()
            }
            E::SearchFinished { request, result } => {
                self.session.tagger.finish_search(request, result);
                Task::none()
            }
            E::ReleaseFetched { request, result } => {
                self.session
                    .tagger
                    .finish_release(request, result.map(|release| *release));
                if self
                    .session
                    .tagger
                    .lookup()
                    .is_some_and(|l| l.source == aulos_core::tagger::LookupSource::AcoustId)
                {
                    self.session.tagger.finish_scan_release();
                }
                match self
                    .session
                    .tagger
                    .take_cover_request(self.session.config.config.tagger_use_caa_cover)
                {
                    Some(request) => run(work::cover(request)),
                    None => Task::none(),
                }
            }
            E::CoverFetched { request, result } => {
                self.session.tagger.finish_cover(request, result);
                Task::none()
            }
            E::FileScanned {
                generation,
                file,
                result,
            } => match self
                .session
                .tagger
                .finish_scan_file(generation, file, result)
            {
                ScanStep::Pending => Task::none(),
                ScanStep::FetchRelease(request) => run(work::release(request)),
                ScanStep::NoMatch => {
                    self.session
                        .tagger
                        .set_notice(false, fl!("tagger-notice-scan-no-match"));
                    Task::none()
                }
            },
            E::FileSaved { file, result } => match self.session.tagger.finish_save(file, result) {
                Some(summary) => self.after_save(summary),
                None => Task::none(),
            },
            E::Submitted {
                generation,
                results,
            } => {
                let ok = results.iter().filter(|r| r.outcome.is_ok()).count();
                let failed = results.len() - ok;
                self.session.tagger.finish_submit(generation, results);
                if failed == 0 {
                    self.session
                        .tagger
                        .set_notice(false, fl!("tagger-notice-submitted", count = ok));
                } else {
                    self.session.tagger.set_notice(
                        true,
                        fl!("tagger-notice-submit-partial", ok = ok, failed = failed),
                    );
                }
                Task::none()
            }
            E::SubmitAborted { generation, error } => {
                self.session.tagger.abort_submit(generation, &error);
                self.session.tagger.set_notice(true, error);
                Task::none()
            }
            E::SeedOpened(Ok(())) => Task::none(),
            E::SeedOpened(Err(error)) => {
                self.session
                    .tagger
                    .set_notice(true, fl!("tagger-notice-seed-failed", error = error));
                Task::none()
            }
        }
    }

    fn report_open(&mut self, result: Result<(), String>) {
        if let Err(error) = result {
            self.session
                .tagger
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
        match self.session.tagger.begin_load(paths, select, action) {
            Some(request) => run_all(work::load(request)),
            None => Task::none(),
        }
    }

    /// Runs the action that was waiting for the files to be read.
    fn run_ready_action(&mut self) -> Task<Action> {
        match self.session.tagger.take_ready_action() {
            Some(TagAction::Edit) | None => Task::none(),
            Some(TagAction::LookUp) => self.start_lookup(),
            Some(TagAction::AcoustId) => self.start_scan(),
        }
    }

    /// Decodes the thumbnails of pictures the table shows for the first time.
    fn decode_missing_thumbs(&mut self) -> Task<Action> {
        let wanted = self.tagger_thumbs.take_missing(&self.session.tagger);
        run_all(
            wanted
                .into_iter()
                .map(|(hash, bytes)| work::decode_thumb(hash, bytes)),
        )
    }

    // --- MusicBrainz ----------------------------------------------------------

    fn start_lookup(&mut self) -> Task<Action> {
        match self.session.tagger.begin_lookup() {
            Some(request) => run(work::search(request)),
            None => {
                if self.session.tagger.lookup().is_none() {
                    self.session
                        .tagger
                        .set_notice(true, fl!("tagger-notice-nothing-to-look-up"));
                }
                Task::none()
            }
        }
    }

    fn start_seed(&mut self, cluster: Option<ClusterKey>) -> Task<Action> {
        let files = self.session.tagger.seed_files(cluster.as_ref());
        if files.is_empty() {
            self.session
                .tagger
                .set_notice(true, fl!("tagger-notice-nothing-to-add"));
            return Task::none();
        }
        run(work::open_seed(files))
    }

    // --- AcoustID -------------------------------------------------------------

    fn start_scan(&mut self) -> Task<Action> {
        let Some(key) = self.session.config.config.acoustid_key().map(str::to_owned) else {
            self.session
                .tagger
                .set_notice(true, fl!("tagger-notice-no-acoustid-key"));
            return Task::none();
        };
        let Some(start) = self.session.tagger.begin_scan() else {
            self.session
                .tagger
                .set_notice(true, fl!("tagger-notice-nothing-to-scan"));
            return Task::none();
        };
        run_all(work::scan(start, key))
    }

    fn start_submit(&mut self) -> Task<Action> {
        let Some(start) = self.session.tagger.begin_submit() else {
            self.session
                .tagger
                .set_notice(true, fl!("tagger-notice-nothing-to-submit"));
            return Task::none();
        };
        let Some(app_key) = self.session.config.config.acoustid_key().map(str::to_owned) else {
            let error = fl!("tagger-notice-no-acoustid-key");
            self.session.tagger.abort_submit(start.generation, &error);
            self.session.tagger.set_notice(true, error);
            return Task::none();
        };
        run(work::submit(start, app_key))
    }

    // --- Saving ---------------------------------------------------------------

    fn start_save(&mut self) -> Task<Action> {
        let jobs = self.session.tagger.begin_save();
        if jobs.is_empty() {
            self.session
                .tagger
                .set_notice(false, fl!("tagger-notice-nothing-to-save"));
            return Task::none();
        }
        run_all(jobs.into_iter().map(work::save))
    }

    /// The last write of a save batch finished: report it and bring the
    /// library (and its covers) up to date with the written files.
    fn after_save(&mut self, summary: SaveSummary) -> Task<Action> {
        let saved = summary.written.len();
        if summary.failed == 0 {
            self.session
                .tagger
                .set_notice(false, fl!("tagger-notice-saved", count = saved));
        } else {
            self.session.tagger.set_notice(
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
            self.session.cover_bytes.remove(key);
        }
        if summary.written.is_empty() {
            return Task::none();
        }
        let SaveSummary {
            written,
            album_keys,
            ..
        } = summary;
        super::tasks::session_effect(
            self.session
                .rescan_local(work::refresh_library(written, album_keys)),
        )
    }
}

// --- Background jobs -------------------------------------------------------------

fn pick_files() -> Task<Action> {
    cosmic::task::future(async {
        let result = async {
            use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

            let mut filter = FileFilter::new("Audio files");
            for ext in aulos_core::player::engine::decoder::SUPPORTED_EXTENSIONS {
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
            Ok(Some(path)) => event(work::read_image(path, kind).await),
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
