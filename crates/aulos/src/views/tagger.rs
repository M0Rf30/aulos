//! The tag editor page: loaded files grouped by album on the left, the field
//! table of the selection in the middle, cover art / MusicBrainz / AcoustID
//! cards on the right.
//!
//! `aulos_core::tagger::TaggerState` (on the session) decides what happens;
//! this module draws it, turns clicks into `TaggerMessage`s, starts the
//! background jobs a transition asks for (`aulos_core::tagger::work`) and
//! feeds their `TaggerEvent`s back. Jobs report through a channel drained by
//! [`pump`].

use crate::app::{App, Route};
use crate::dialogs::{self, Filter};
use crate::theme::{self, ColorExt, Palette};
use crate::widgets::{
    PillKind, caps_label, cover_image, label_text, paint_icon, paint_text, paint_text_spaced,
    pill_button_enabled, skeleton, text_width,
};
use aulos_core::library::Track;
use aulos_core::library::tagio::keys;
use aulos_core::library::tagio::model::KNOWN_KEYS;
use aulos_core::online::{acoustid, musicbrainz};
use aulos_core::tagger::edit::{PICTURE_KINDS, join_values};
use aulos_core::tagger::labels::{self, Tone};
use aulos_core::tagger::state::library_album_key;
use aulos_core::tagger::thumbs::ThumbCache;
use aulos_core::tagger::{
    Cluster, ClusterKey, Confirm, Facts, FieldRow, FieldValue, FileId, FileStatus, LookupSource,
    LookupState, Remote, SaveSummary, ScanPhase, ScanStep, SubmissionState, TagAction, TagFile,
    TaggerEvent, TaggerMessage, TaggerState, is_taggable, work,
};
use egui::epaint::TextShape;
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Align2, Color32, ColorImage, CornerRadius, FontId, Frame, Id, Key, Layout, Margin,
    Popup, Pos2, Rect, Response, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit,
    TextureHandle, TextureOptions, Ui, UiBuilder, pos2, vec2,
};
use egui_phosphor::{fill, regular};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Repaint interval of the busy spinner and the indeterminate rail.
const BUSY_FRAME: Duration = Duration::from_millis(50);
/// Width of the files panel (wide / narrow layout).
const FILES_W: f32 = 240.0;
const FILES_W_NARROW: f32 = 200.0;
/// Width of the right column.
const SIDE_W: f32 = 280.0;
/// Below this width the right column's cards move under the field table.
const NARROW_BELOW: f32 = 940.0;
const GAP: f32 = 10.0;

/// Height of a cluster header (8 above, 24 content, 4 below).
const HEAD_H: f32 = 36.0;
/// Height of a file row and the gap below it.
const FILE_H: f32 = 26.0;
const FILE_STEP: f32 = FILE_H + 1.0;
/// Padding of the list inside the files panel.
const LIST_PAD: f32 = 6.0;
/// Height of the filter strip and of the footer of the files panel.
const FILTER_H: f32 = 45.0;
const FOOTER_H: f32 = 30.0;

/// Height of one field row (28 input + 3 padding each side).
const FIELD_ROW_H: f32 = 34.0;
/// Height of the meta panel's top bar.
const TOP_BAR_H: f32 = 38.0;
/// Height of the column captions row.
const CAPTIONS_H: f32 = 28.0;
/// Height of the add-field toggle row and of its form.
const ADD_ROW_H: f32 = 32.0;
const ADD_FORM_H: f32 = 108.0;
/// Space the new-value box leaves on the right for the row's icons.
const ACTIONS_W: f32 = 44.0;

/// Height of a candidate release row and the gap below it.
const CANDIDATE_H: f32 = 44.0;
const CANDIDATE_STEP: f32 = CANDIDATE_H + 4.0;
const CANDIDATES_SHOWN: usize = 5;

pub struct State {
    tx: Sender<TaggerEvent>,
    rx: Receiver<TaggerEvent>,
    thumbs: ThumbCache<TextureHandle>,
    /// Text of the files filter.
    filter: String,
    /// The file the last plain click landed on; shift-click extends from it.
    anchor: Option<FileId>,
    /// The add-field form is open.
    add_open: bool,
}

impl State {
    /// Forgets the thumbnails: their textures belong to a window that closed.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub fn release_textures(&mut self) {
        self.thumbs = ThumbCache::default();
    }
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            thumbs: ThumbCache::default(),
            filter: String::new(),
            anchor: None,
            add_open: false,
        }
    }
}

/// What drawing the page asks for.
enum Act {
    Msg(TaggerMessage),
    Filter(String),
    Anchor(FileId),
    Range { from: FileId, to: FileId },
    AddOpen(bool),
}

// ---------------------------------------------------------------- entry points

pub fn enter(app: &mut App) {
    pump(app);
}

/// Applies the results of finished background jobs. [`show`] calls it every
/// frame; the app loop SHOULD also call it while another page is shown, or
/// results (a finished save, say) wait until the editor is opened again.
pub fn pump(app: &mut App) {
    while let Ok(event) = app.views.tagger.rx.try_recv() {
        handle_event(app, event);
    }
    decode_thumbs(app);
}

/// Opens the editor on the local `tracks` and runs `action` once they are
/// loaded ("Edit tags…", "Look up on MusicBrainz", "Scan with AcoustID").
pub fn open_tracks(app: &mut App, tracks: &[Track], action: TagAction) {
    let paths: Vec<PathBuf> = tracks
        .iter()
        .filter(|t| is_taggable(t))
        .map(|t| t.path.clone())
        .collect();
    if paths.is_empty() {
        return;
    }
    app.navigate(Route::Tagger);
    start_load(app, paths, true, Some(action));
    run_ready_action(app);
}

pub fn show(app: &mut App, ui: &mut Ui) {
    pump(app);
    let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .collect()
    });
    if !dropped.is_empty() {
        let (dirs, files): (Vec<_>, Vec<_>) = dropped.into_iter().partition(|p| p.is_dir());
        for dir in dirs {
            spawn(app, work::folder_files(dir));
        }
        if !files.is_empty() {
            let select = app.session.tagger.is_empty();
            start_load(app, files, select, None);
        }
    }

    let mut acts = Vec::new();
    page(app, ui, &mut acts);
    for act in acts {
        match act {
            Act::Msg(msg) => dispatch(app, msg),
            Act::Filter(text) => app.views.tagger.filter = text,
            Act::Anchor(id) => app.views.tagger.anchor = Some(id),
            Act::Range { from, to } => select_range(app, from, to),
            Act::AddOpen(open) => app.views.tagger.add_open = open,
        }
    }
    decode_thumbs(app);
}

/// Selects every file from `from` to `to` in display order, skipping the ones
/// the file-list filter hides.
fn select_range(app: &mut App, from: FileId, to: FileId) {
    let order: Vec<FileId> = groups(&app.session.tagger, &app.views.tagger.filter)
        .iter()
        .flat_map(|g| g.ids.iter().copied())
        .collect();
    let (Some(a), Some(b)) = (
        order.iter().position(|id| *id == from),
        order.iter().position(|id| *id == to),
    ) else {
        app.session.tagger.select_only(to);
        return;
    };
    let (lo, hi) = (a.min(b), a.max(b));
    app.session.tagger.select_only(order[lo]);
    for id in &order[lo + 1..=hi] {
        app.session.tagger.toggle(*id);
    }
}

// ---------------------------------------------------------------- update logic

/// Runs one of the editor's background jobs, feeding its event back.
fn spawn(app: &App, job: impl Future<Output = TaggerEvent> + Send + 'static) {
    let tx = app.views.tagger.tx.clone();
    let ctx = app.wake.clone();
    app.handle.spawn(async move {
        let event = job.await;
        let _ = tx.send(event);
        ctx.request_repaint();
    });
}

/// Runs a batch of background jobs side by side.
fn spawn_all<F>(app: &App, batch: impl IntoIterator<Item = F>)
where
    F: Future<Output = TaggerEvent> + Send + 'static,
{
    for job in batch {
        spawn(app, job);
    }
}

fn notice(app: &mut App, error: bool, text: impl Into<String>) {
    app.session.tagger.set_notice(error, text.into());
}

fn dispatch(app: &mut App, msg: TaggerMessage) {
    use TaggerMessage as M;
    match msg {
        // Files.
        M::AddFiles => spawn(app, pick_files()),
        M::AddFolder => spawn(app, pick_folder()),
        M::SelectFile(id) => app.session.tagger.select_only(id),
        M::ToggleFile(id) => app.session.tagger.toggle(id),
        M::SelectCluster(key) => app.session.tagger.select_cluster(&key),
        M::ToggleCluster(key) => app.session.tagger.toggle_cluster(&key),
        M::SelectAll => app.session.tagger.select_all(),
        M::ClearSelection => app.session.tagger.clear_selection(),
        M::RemoveSelected => {
            app.session.tagger.request_remove_selected();
        }
        M::ClearAll => {
            app.session.tagger.request_clear();
        }
        M::ConfirmDiscard => app.session.tagger.accept_confirm(),
        M::CancelDiscard => app.session.tagger.cancel_confirm(),

        // Metadata table.
        M::EditField { key, text } => app.session.tagger.set_field_text(&key, text),
        M::RevertField(key) => app.session.tagger.revert_field(&key),
        M::DeleteField(key) => app.session.tagger.delete_field(&key),
        M::AddFieldKnown(index) => app.session.tagger.set_add_known(Some(index)),
        M::AddFieldCustom(name) => app.session.tagger.set_add_custom(name),
        M::AddFieldValue(value) => app.session.tagger.set_add_value(value),
        M::AddFieldCommit => {
            // A refused form keeps its error in the state, shown under it.
            if app.session.tagger.commit_add_field().is_ok() {
                app.views.tagger.add_open = false;
            }
        }

        // Cover art.
        M::AddPicture => {
            let kind = app.session.tagger.new_picture_kind();
            spawn(app, pick_image(kind));
        }
        M::NewPictureKind(index) => {
            if let Some(&kind) = PICTURE_KINDS.get(index) {
                app.session.tagger.set_new_picture_kind(kind);
            }
        }
        M::RemovePicture { hash, kind } => app.session.tagger.remove_picture(hash, kind),
        M::SetPictureKind {
            hash,
            kind,
            new_kind,
        } => app.session.tagger.set_picture_kind(hash, kind, new_kind),
        M::RevertPictures => app.session.tagger.revert_pictures(),
        M::FetchCover => {
            if let Some(request) = app.session.tagger.fetch_cover_for_selection() {
                spawn(app, work::cover(request));
            }
        }

        // Save / revert.
        M::Save => start_save(app),
        M::RevertSelected => app.session.tagger.revert_selected(),
        M::RevertAll => app.session.tagger.revert_all(),

        // MusicBrainz.
        M::Lookup => start_lookup(app),
        M::LookupCluster(key) => {
            app.session.tagger.select_cluster(&key);
            start_lookup(app);
        }
        M::QueryArtist(artist) => app.session.tagger.set_query_artist(artist),
        M::QueryAlbum(album) => app.session.tagger.set_query_album(album),
        M::SearchAgain => {
            if let Some(request) = app.session.tagger.search_again() {
                spawn(app, work::search(request));
            }
        }
        M::ChooseRelease(id) => {
            if let Some(request) = app.session.tagger.choose_release(&id) {
                spawn(app, work::release(request));
            }
        }
        M::DiscardRelease => app.session.tagger.discard_release(),
        M::CloseLookup => app.session.tagger.close_lookup(),
        M::OpenReleasePage(id) => report_open(app, musicbrainz::open_release_page(&id)),
        M::OpenRecordingPage(id) => report_open(app, musicbrainz::open_recording_page(&id)),
        M::OpenAcoustIdPage(id) => report_open(app, acoustid::open_track_page(&id)),
        M::AddAsRelease(cluster) => start_seed(app, cluster),

        // AcoustID.
        M::Scan => start_scan(app),
        M::ScanCluster(key) => {
            app.session.tagger.select_cluster(&key);
            start_scan(app);
        }
        M::CancelScan => {
            work::cancel_scan();
            app.session.tagger.cancel_scan();
        }
        M::SubmitAcoustIds => start_submit(app),

        // Page chrome.
        M::OpenSettings => app.navigate(Route::Settings),
        M::DismissNotice => app.session.tagger.dismiss_notice(),
    }
}

fn handle_event(app: &mut App, event: TaggerEvent) {
    use TaggerEvent as E;
    match event {
        E::FilesPicked(Ok(paths)) => {
            if paths.is_empty() {
                return;
            }
            // Files added to an empty editor are selected so the table shows
            // them right away; later additions keep the selection.
            let select = app.session.tagger.is_empty();
            start_load(app, paths, select, None);
            run_ready_action(app);
        }
        E::FilesPicked(Err(error)) | E::FolderPicked(Err(error)) => notice(
            app,
            true,
            aulos_core::fl!("tagger-ui-add-files-failed", error = error.to_string()),
        ),
        E::FolderPicked(Ok(dir)) => spawn(app, work::folder_files(dir)),
        E::ImagePicked(Ok(picture)) => app.session.tagger.add_picture(&picture),
        E::ImagePicked(Err(error)) => notice(
            app,
            true,
            aulos_core::fl!("tagger-ui-add-image-failed", error = error.to_string()),
        ),
        E::FilesLoaded { generation, files } => {
            app.session.tagger.finish_load(generation, files);
            run_ready_action(app);
        }
        E::ThumbDecoded { hash, pixels } => {
            let texture = pixels.map(|(w, h, rgba)| {
                app.ctx.load_texture(
                    format!("tag-thumb-{hash}"),
                    ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
                    TextureOptions::LINEAR,
                )
            });
            app.views.tagger.thumbs.insert(hash, texture);
        }
        E::SearchFinished { request, result } => app.session.tagger.finish_search(request, result),
        E::ReleaseFetched { request, result } => {
            app.session
                .tagger
                .finish_release(request, result.map(|release| *release));
            if app
                .session
                .tagger
                .lookup()
                .is_some_and(|l| l.source == LookupSource::AcoustId)
            {
                app.session.tagger.finish_scan_release();
            }
            if let Some(request) = app
                .session
                .tagger
                .take_cover_request(app.session.config.config.tagger_use_caa_cover)
            {
                spawn(app, work::cover(request));
            }
        }
        E::CoverFetched { request, result } => app.session.tagger.finish_cover(request, result),
        E::FileScanned {
            generation,
            file,
            result,
        } => match app
            .session
            .tagger
            .finish_scan_file(generation, file, result)
        {
            ScanStep::Pending => {}
            ScanStep::FetchRelease(request) => spawn(app, work::release(request)),
            ScanStep::NoMatch => notice(app, false, aulos_core::fl!("tagger-ui-scan-unrecognised")),
        },
        E::FileSaved { file, result } => {
            if let Some(summary) = app.session.tagger.finish_save(file, result) {
                after_save(app, summary);
            }
        }
        E::Submitted {
            generation,
            results,
        } => {
            let ok = results.iter().filter(|r| r.outcome.is_ok()).count();
            let failed = results.len() - ok;
            app.session.tagger.finish_submit(generation, results);
            if failed == 0 {
                notice(
                    app,
                    false,
                    aulos_core::fl!("tagger-ui-submitted", count = ok),
                );
            } else {
                notice(
                    app,
                    true,
                    aulos_core::fl!("tagger-ui-submit-partial", ok = ok, failed = failed),
                );
            }
        }
        E::SubmitAborted { generation, error } => {
            app.session.tagger.abort_submit(generation, &error);
            notice(app, true, error);
        }
        E::SeedOpened(Ok(())) => {}
        E::SeedOpened(Err(error)) => notice(
            app,
            true,
            aulos_core::fl!("tagger-ui-open-mb-failed", error = error.to_string()),
        ),
    }
}

fn report_open(app: &mut App, result: Result<(), String>) {
    if let Err(error) = result {
        notice(
            app,
            true,
            aulos_core::fl!("tagger-ui-open-page-failed", error = error.to_string()),
        );
    }
}

fn start_load(app: &mut App, paths: Vec<PathBuf>, select: bool, action: Option<TagAction>) {
    if let Some(request) = app.session.tagger.begin_load(paths, select, action) {
        spawn_all(app, work::load(request));
    }
}

/// Runs the action that was waiting for the files to be read.
fn run_ready_action(app: &mut App) {
    match app.session.tagger.take_ready_action() {
        Some(TagAction::Edit) | None => {}
        Some(TagAction::LookUp) => start_lookup(app),
        Some(TagAction::AcoustId) => start_scan(app),
    }
}

/// Decodes the thumbnails of pictures the table shows for the first time.
fn decode_thumbs(app: &mut App) {
    let wanted = app.views.tagger.thumbs.take_missing(&app.session.tagger);
    for (hash, bytes) in wanted {
        spawn(app, work::decode_thumb(hash, bytes));
    }
}

fn start_lookup(app: &mut App) {
    match app.session.tagger.begin_lookup() {
        Some(request) => spawn(app, work::search(request)),
        None => {
            if app.session.tagger.lookup().is_none() {
                notice(app, true, aulos_core::fl!("tagger-ui-no-files-lookup"));
            }
        }
    }
}

fn start_seed(app: &mut App, cluster: Option<ClusterKey>) {
    let files = app.session.tagger.seed_files(cluster.as_ref());
    if files.is_empty() {
        notice(app, true, aulos_core::fl!("tagger-ui-no-files-release"));
        return;
    }
    spawn(app, work::open_seed(files));
}

fn start_scan(app: &mut App) {
    let Some(key) = app.session.config.config.acoustid_key().map(str::to_owned) else {
        notice(app, true, aulos_core::fl!("tagger-ui-key-missing-notice"));
        return;
    };
    let Some(start) = app.session.tagger.begin_scan() else {
        notice(app, true, aulos_core::fl!("tagger-ui-no-files-scan"));
        return;
    };
    spawn_all(app, work::scan(start, key));
}

fn start_submit(app: &mut App) {
    let Some(start) = app.session.tagger.begin_submit() else {
        notice(app, true, aulos_core::fl!("tagger-ui-no-recording-ids"));
        return;
    };
    let Some(app_key) = app.session.config.config.acoustid_key().map(str::to_owned) else {
        let error = aulos_core::fl!("tagger-ui-key-missing-notice");
        app.session.tagger.abort_submit(start.generation, &error);
        notice(app, true, error);
        return;
    };
    spawn(app, work::submit(start, app_key));
}

fn start_save(app: &mut App) {
    let jobs = app.session.tagger.begin_save();
    if jobs.is_empty() {
        notice(app, false, aulos_core::fl!("tagger-ui-no-changes"));
        return;
    }
    spawn_all(app, jobs.into_iter().map(work::save));
}

/// The last write of a save batch finished: report it and bring the library
/// (and its covers) up to date with the written files.
fn after_save(app: &mut App, summary: SaveSummary) {
    let saved = summary.written.len();
    if summary.failed == 0 {
        notice(
            app,
            false,
            aulos_core::fl!("tagger-ui-saved", count = saved),
        );
    } else {
        notice(
            app,
            true,
            aulos_core::fl!(
                "tagger-ui-save-partial",
                saved = saved,
                failed = summary.failed
            ),
        );
    }
    // Covers of the albums the files left or joined are stale now.
    for key in &summary.album_keys {
        app.session.cover_bytes.remove(key);
    }
    if summary.written.is_empty() {
        return;
    }
    let SaveSummary {
        written,
        album_keys,
        ..
    } = summary;
    let effect = app
        .session
        .rescan_local(work::refresh_library(written, album_keys));
    app.run(effect);
}

// ---------------------------------------------------------------- pickers

/// The files the user picked, as an event; an empty list when cancelled.
async fn pick_files() -> TaggerEvent {
    let filter_name = aulos_core::fl!("tagger-ui-filter-audio");
    let filter = Filter::new(
        &filter_name,
        aulos_core::player::engine::format_registry::SCAN_EXTENSIONS
            .iter()
            .copied(),
    );
    TaggerEvent::FilesPicked(Ok(dialogs::pick_files(
        &aulos_core::fl!("tagger-ui-pick-files"),
        Some(&filter),
    )
    .await))
}

async fn pick_folder() -> TaggerEvent {
    match dialogs::pick_folder(&aulos_core::fl!("tagger-ui-pick-folder")).await {
        Some(dir) => TaggerEvent::FolderPicked(Ok(dir)),
        // Cancelled: nothing to add.
        None => TaggerEvent::FilesPicked(Ok(Vec::new())),
    }
}

async fn pick_image(kind: u8) -> TaggerEvent {
    let filter_name = aulos_core::fl!("tagger-ui-filter-images");
    let filter = Filter::new(&filter_name, ["png", "jpg", "jpeg", "webp", "bmp"]);
    match dialogs::pick_file(&aulos_core::fl!("tagger-ui-pick-image"), Some(&filter)).await {
        Some(path) => work::read_image(path, kind).await,
        // Cancelled: nothing to add.
        None => TaggerEvent::FilesPicked(Ok(Vec::new())),
    }
}

// ---------------------------------------------------------------- small widgets

fn tone_color(pal: &Palette, tone: Tone) -> Color32 {
    match tone {
        Tone::Neutral => pal.text_2,
        Tone::Accent => pal.text_1,
        Tone::Success => pal.ok,
        Tone::Warning => pal.warn,
        Tone::Danger => pal.danger,
    }
}

/// "1 file" / "N files".
fn files_text(n: usize) -> String {
    aulos_core::fl!("tagger-files-count", count = n)
}

fn first_line(text: &str) -> Cow<'_, str> {
    match text.split_once('\n') {
        Some((first, _)) => Cow::Owned(format!("{first}…")),
        None => Cow::Borrowed(text),
    }
}

/// A 30px-high rectangular tool button.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Tool,
    Primary,
    Ghost,
}

fn tool_button(
    ui: &mut Ui,
    pal: &Palette,
    label: &str,
    icon: &str,
    look: Look,
    enabled: bool,
) -> Response {
    let (fill, ink, pad) = match look {
        Look::Tool => (pal.surface_hi, pal.text, 12.0),
        Look::Primary => (pal.text, pal.bg, 14.0),
        Look::Ghost => (Color32::TRANSPARENT, pal.text_15, 12.0),
    };
    let ink = if enabled {
        ink
    } else {
        ink.gamma_multiply(0.38)
    };
    let icon_galley = ui
        .painter()
        .layout_no_wrap(icon.to_owned(), theme::icon(14.0), ink);
    let text_galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), theme::semibold(13.0), ink);
    let width = pad * 2.0 + icon_galley.size().x + 6.0 + text_galley.size().x;
    let (rect, resp) = ui.allocate_exact_size(
        vec2(width, 30.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let radius = CornerRadius::same(theme::radius::MD);
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        radius,
        if enabled {
            fill
        } else {
            fill.gamma_multiply(0.6)
        },
    );
    if enabled && resp.hovered() {
        painter.rect_filled(
            rect,
            radius,
            if look == Look::Primary {
                pal.bg.alpha(0.12)
            } else {
                pal.hover
            },
        );
    }
    let mid = rect.center().y;
    let icon_pos = pos2(rect.left() + pad, mid - icon_galley.size().y / 2.0);
    let text_pos = pos2(
        rect.left() + pad + icon_galley.size().x + 6.0,
        mid - text_galley.size().y / 2.0,
    );
    painter.galley(icon_pos, icon_galley, ink);
    painter.galley(text_pos, text_galley, ink);
    resp
}

/// A 30px square tool button holding only an icon.
fn tool_icon(ui: &mut Ui, pal: &Palette, icon: &str, tip: &str, enabled: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(
        vec2(30.0, 30.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let ink = if enabled {
        pal.text
    } else {
        pal.text.gamma_multiply(0.38)
    };
    let radius = CornerRadius::same(theme::radius::MD);
    ui.painter().rect_filled(
        rect,
        radius,
        if enabled {
            pal.surface_hi
        } else {
            pal.surface_hi.gamma_multiply(0.6)
        },
    );
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, radius, pal.hover);
    }
    paint_icon(ui.painter(), rect.center(), icon, 15.0, ink, false);
    resp.on_hover_text(tip)
}

/// A small icon button at `rect`; true when clicked.
fn icon_hit(
    ui: &mut Ui,
    pal: &Palette,
    rect: Rect,
    id: Id,
    glyph: &str,
    tip: &str,
    enabled: bool,
) -> bool {
    let resp = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if enabled && resp.hovered() {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(theme::radius::SM),
            pal.text.alpha(0.08),
        );
    }
    let ink = if enabled { pal.text_2 } else { pal.text_4 };
    paint_icon(ui.painter(), rect.center(), glyph, 14.0, ink, false);
    resp.on_hover_text(tip).clicked()
}

/// An underlined text link at the right end of `x_right`; returns its left
/// edge so further links can be laid out leftwards.
fn link_right(
    ui: &mut Ui,
    pal: &Palette,
    id: Id,
    text: &str,
    x_right: f32,
    y: f32,
    enabled: bool,
) -> (f32, bool) {
    let font = theme::regular(11.5);
    let width = text_width(ui, text, font.clone());
    let rect = Rect::from_min_size(
        pos2(x_right - width - 4.0, y - 10.0),
        vec2(width + 8.0, 20.0),
    );
    let resp = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let ink = if !enabled {
        pal.text_4
    } else if resp.hovered() {
        pal.text
    } else {
        pal.text_2
    };
    paint_text(
        ui.painter(),
        rect.shrink2(vec2(4.0, 0.0)),
        Align::Min,
        text,
        font,
        ink,
    );
    (rect.left() - 4.0, enabled && resp.clicked())
}

fn hint(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.add(
        egui::Label::new(
            RichText::new(text)
                .font(theme::regular(11.5))
                .color(pal.text_3),
        )
        .wrap(),
    );
}

fn tone_hint(ui: &mut Ui, pal: &Palette, text: &str, tone: Tone) {
    ui.add(
        egui::Label::new(
            RichText::new(text)
                .font(theme::regular(11.5))
                .color(tone_color(pal, tone)),
        )
        .wrap(),
    );
}

fn paint_spinner(ui: &Ui, center: Pos2, size: f32, color: Color32) {
    let time = ui.input(|i| i.time) as f32;
    let galley =
        ui.painter()
            .layout_no_wrap(regular::CIRCLE_NOTCH.to_owned(), theme::icon(size), color);
    let pos = center - galley.size() / 2.0;
    ui.painter().add(
        TextShape::new(pos, galley, color).with_angle_and_anchor(time * 6.0, Align2::CENTER_CENTER),
    );
    ui.ctx().request_repaint_after(BUSY_FRAME);
}

/// A spinner followed by `text`.
fn busy_line(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        paint_spinner(ui, rect.center(), 15.0, pal.text_2);
        ui.add(
            egui::Label::new(
                RichText::new(text)
                    .font(theme::regular(12.0))
                    .color(pal.text_2),
            )
            .wrap(),
        );
    });
}

/// Paints a thin progress rail.
fn paint_rail(ui: &Ui, rect: Rect, frac: f32, fill: Color32, track: Color32) {
    let radius = CornerRadius::same((rect.height() / 2.0).ceil() as u8);
    ui.painter().rect_filled(rect, radius, track);
    let width = rect.width() * frac.clamp(0.0, 1.0);
    if width > 0.5 {
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, vec2(width, rect.height())),
            radius,
            fill,
        );
    }
}

/// An indeterminate rail: a segment sliding across.
fn paint_busy_rail(ui: &Ui, rect: Rect, fill: Color32, track: Color32) {
    let radius = CornerRadius::same((rect.height() / 2.0).ceil() as u8);
    ui.painter().rect_filled(rect, radius, track);
    let time = ui.input(|i| i.time) as f32;
    let phase = (time * 0.9).fract();
    let seg = rect.width() * 0.3;
    let x = rect.left() - seg + (rect.width() + seg) * phase;
    let seg_rect = Rect::from_min_max(
        pos2(x.max(rect.left()), rect.top()),
        pos2((x + seg).min(rect.right()), rect.bottom()),
    );
    if seg_rect.width() > 0.0 {
        ui.painter().rect_filled(seg_rect, radius, fill);
    }
    ui.ctx().request_repaint_after(BUSY_FRAME);
}

/// What a banner shows under its text.
enum Bar {
    None,
    Fraction(f32),
    Busy,
}

/// A tinted notice bar; returns the index of the action link clicked.
fn banner(
    ui: &mut Ui,
    pal: &Palette,
    tone: Color32,
    icon: &str,
    text: &str,
    bar: Bar,
    actions: &[&str],
) -> Option<usize> {
    let mut clicked = None;
    Frame::new()
        .fill(tone.alpha(0.08))
        .stroke(Stroke::new(1.0, tone.alpha(0.28)))
        .corner_radius(CornerRadius::same(theme::radius::MD))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(10.0, 6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).font(theme::icon(15.0)).color(tone));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (i, label) in actions.iter().enumerate().rev() {
                        let resp = ui.add(
                            egui::Label::new(
                                RichText::new(*label)
                                    .font(theme::regular(12.5))
                                    .color(pal.text_15)
                                    .underline(),
                            )
                            .sense(Sense::click()),
                        );
                        if resp.clicked() {
                            clicked = Some(i);
                        }
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(text)
                                    .font(theme::regular(12.5))
                                    .color(pal.text),
                            )
                            .wrap(),
                        );
                    });
                });
            });
            match bar {
                Bar::None => {}
                Bar::Fraction(frac) => {
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
                    paint_rail(ui, rect, frac, pal.reed, pal.track);
                }
                Bar::Busy => {
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
                    paint_busy_rail(ui, rect, pal.reed, pal.track);
                }
            }
        });
    clicked
}

/// A `surface_1` card with a 1px border and 10px padding.
fn card(ui: &mut Ui, pal: &Palette, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(pal.surface_1)
        .stroke(Stroke::new(1.0, pal.stroke))
        .corner_radius(CornerRadius::same(theme::radius::LG))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            add(ui);
        });
}

fn paint_panel(ui: &Ui, pal: &Palette, rect: Rect) {
    ui.painter()
        .rect_filled(rect, CornerRadius::same(theme::radius::LG), pal.surface_1);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(theme::radius::LG),
        Stroke::new(1.0, pal.stroke),
        StrokeKind::Inside,
    );
}

/// How a text box is drawn and wired.
struct TextBox<'a> {
    id: Id,
    hint: &'a str,
    /// The value differs from disk: warn-tinted border.
    changed: bool,
    left_pad: f32,
    right_pad: f32,
    enabled: bool,
    font: FontId,
}

impl<'a> TextBox<'a> {
    fn new(id: Id, hint: &'a str) -> Self {
        Self {
            id,
            hint,
            changed: false,
            left_pad: 8.0,
            right_pad: 0.0,
            enabled: true,
            font: theme::regular(12.5),
        }
    }
}

/// A single-line input drawn into `rect`: `bg` fill, 1px border, focus ring.
fn text_box(
    ui: &mut Ui,
    pal: &Palette,
    rect: Rect,
    tb: &TextBox<'_>,
    buf: &mut String,
) -> Response {
    let radius = CornerRadius::same(theme::radius::SM);
    ui.painter().rect_filled(rect, radius, pal.bg);
    let edit_rect = Rect::from_min_max(
        pos2(rect.left(), rect.top()),
        pos2(
            (rect.right() - tb.right_pad).max(rect.left() + 20.0),
            rect.bottom(),
        ),
    );
    let edit = TextEdit::singleline(buf)
        .id(tb.id)
        .frame(Frame::NONE)
        .margin(vec2(tb.left_pad, 0.0))
        .font(tb.font.clone())
        .text_color(if tb.enabled { pal.text } else { pal.text_2 })
        .hint_text(RichText::new(tb.hint).italics().color(pal.text_3))
        .desired_width(edit_rect.width())
        .min_size(edit_rect.size())
        .vertical_align(Align::Center)
        .interactive(tb.enabled);
    let resp = ui.put(edit_rect, edit);
    crate::widgets::reveal_focused(ui, &resp);
    let focused = resp.has_focus();
    if focused {
        ui.painter().rect_stroke(
            rect.expand(1.5),
            CornerRadius::same(8),
            Stroke::new(3.0, pal.text.alpha(0.16)),
            StrokeKind::Outside,
        );
    }
    let border = if focused {
        pal.text
    } else if tb.changed {
        pal.warn.alpha(0.45)
    } else {
        pal.stroke
    };
    ui.painter()
        .rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);
    resp
}

/// A dropdown field: what it shows and which option is current.
struct Dropdown<'a> {
    id: Id,
    value: &'a str,
    selected: Option<usize>,
    count: usize,
    enabled: bool,
    placeholder: bool,
}

/// A dropdown drawn into `rect`; `label_of(i)` names option `i`. Returns the
/// option picked this frame.
fn dropdown(
    ui: &mut Ui,
    pal: &Palette,
    rect: Rect,
    dd: &Dropdown<'_>,
    label_of: impl Fn(usize) -> String,
) -> Option<usize> {
    let opacity = if dd.enabled { 1.0 } else { 0.38 };
    let resp = ui.interact(
        rect,
        dd.id,
        if dd.enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let radius = CornerRadius::same(theme::radius::SM);
    ui.painter().rect_filled(rect, radius, pal.bg);
    let border = if resp.hovered() && dd.enabled {
        pal.stroke_strong
    } else {
        pal.input_bd
    };
    ui.painter().rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, border.gamma_multiply(opacity)),
        StrokeKind::Inside,
    );
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 8.0, rect.top()),
        pos2(rect.right() - 22.0, rect.bottom()),
    );
    let ink = if dd.placeholder { pal.text_3 } else { pal.text };
    paint_text(
        ui.painter(),
        text_rect,
        Align::Min,
        dd.value,
        theme::regular(12.5),
        ink.gamma_multiply(opacity),
    );
    paint_icon(
        ui.painter(),
        pos2(rect.right() - 12.0, rect.center().y),
        regular::CARET_DOWN,
        10.0,
        pal.text_3,
        false,
    );
    if !dd.enabled || dd.count == 0 {
        return None;
    }
    let mut picked = None;
    Popup::menu(&resp)
        .width(rect.width().max(150.0))
        .show(|ui| {
            ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for i in 0..dd.count {
                    if ui
                        .selectable_label(dd.selected == Some(i), label_of(i))
                        .clicked()
                    {
                        picked = Some(i);
                    }
                }
            });
        });
    picked
}

// ---------------------------------------------------------------- page

fn page(app: &App, ui: &mut Ui, acts: &mut Vec<Act>) {
    let facts = Facts::compute(&app.session.tagger);
    let full = ui.available_rect_before_wrap();
    let inner = Rect::from_min_max(
        pos2(full.left() + 16.0, full.top()),
        pos2(full.right() - 16.0, full.bottom() - 12.0),
    );
    let mut pui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut pui;
    ui.spacing_mut().item_spacing = vec2(8.0, 10.0);

    header(app, ui, &facts, acts);
    banners(app, ui, &facts, acts);

    let body = ui.available_rect_before_wrap();
    if body.height() < 60.0 {
        return;
    }
    if app.compact() {
        // Phones: the file list or the tag cards, one at a time.
        let id = Id::new("tagger-pane");
        let mut pane: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0);
        if crate::widgets::segmented(
            ui,
            &mut pane,
            &[
                aulos_core::fl!("tagger-pane-files").as_str(),
                aulos_core::fl!("tagger-metadata-title").as_str(),
            ],
            34.0,
        ) {
            ui.data_mut(|d| d.insert_temp(id, pane));
        }
        let body = ui.available_rect_before_wrap();
        if body.height() < 60.0 {
            return;
        }
        if pane == 0 {
            files_panel(app, ui, body, &facts, acts);
        } else {
            let mut cui = ui.new_child(
                UiBuilder::new()
                    .max_rect(body)
                    .layout(Layout::top_down(Align::Min)),
            );
            ScrollArea::vertical()
                .id_salt("tagger-stacked")
                .auto_shrink([false, false])
                .show(&mut cui, |ui| {
                    ui.spacing_mut().item_spacing.y = GAP;
                    meta_card(app, ui, &facts, false, acts);
                    side_cards(app, ui, &facts, acts);
                });
        }
        return;
    }
    let narrow = body.width() < NARROW_BELOW;
    let files_w = if narrow { FILES_W_NARROW } else { FILES_W };
    let files_rect = Rect::from_min_max(body.min, pos2(body.left() + files_w, body.bottom()));
    files_panel(app, ui, files_rect, &facts, acts);
    let mid_left = files_rect.right() + GAP;

    if narrow {
        let rect = Rect::from_min_max(pos2(mid_left, body.top()), body.max);
        let mut cui = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        ScrollArea::vertical()
            .id_salt("tagger-stacked")
            .auto_shrink([false, false])
            .show(&mut cui, |ui| {
                ui.spacing_mut().item_spacing.y = GAP;
                meta_card(app, ui, &facts, false, acts);
                side_cards(app, ui, &facts, acts);
            });
    } else {
        let side_rect = Rect::from_min_max(pos2(body.right() - SIDE_W, body.top()), body.max);
        let meta_rect = Rect::from_min_max(
            pos2(mid_left, body.top()),
            pos2(side_rect.left() - GAP, body.bottom()),
        );
        let mut mui = ui.new_child(
            UiBuilder::new()
                .max_rect(meta_rect)
                .layout(Layout::top_down(Align::Min)),
        );
        meta_card(app, &mut mui, &facts, true, acts);
        let mut sui = ui.new_child(
            UiBuilder::new()
                .max_rect(side_rect)
                .layout(Layout::top_down(Align::Min)),
        );
        ScrollArea::vertical()
            .id_salt("tagger-side")
            .auto_shrink([false, false])
            .show(&mut sui, |ui| {
                ui.spacing_mut().item_spacing.y = GAP;
                side_cards(app, ui, &facts, acts);
            });
    }
}

// ---------------------------------------------------------------- header & banners

/// Header row height. The right-aligned group is laid out in a box of this
/// height: a bare `right_to_left(Align::Center)` takes the whole remaining
/// page height on the first frame, leaving no room for the panes.
const HEADER_H: f32 = 32.0;

fn header(app: &App, ui: &mut Ui, facts: &Facts, acts: &mut Vec<Act>) {
    let right_aligned = |ui: &mut Ui, acts: &mut Vec<Act>| {
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), HEADER_H),
            Layout::right_to_left(Align::Center),
            |ui| header_right(app, ui, facts, acts),
        );
    };
    if ui.available_width() >= 900.0 {
        ui.horizontal(|ui| {
            header_left(app, ui, facts, acts);
            header_scan(app, ui, acts);
            right_aligned(ui, acts);
        });
    } else {
        ui.horizontal_wrapped(|ui| header_left(app, ui, facts, acts));
        if app
            .session
            .tagger
            .scan()
            .is_some_and(|s| matches!(s.phase, ScanPhase::Running | ScanPhase::Resolving))
        {
            ui.horizontal(|ui| header_scan(app, ui, acts));
        }
        right_aligned(ui, acts);
    }
}

fn header_left(app: &App, ui: &mut Ui, facts: &Facts, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let key_set = app.session.config.config.acoustid_key().is_some();
    ui.spacing_mut().item_spacing.x = 8.0;
    ui.label(
        RichText::new(aulos_core::fl!("tagger-title"))
            .font(theme::semibold(22.0))
            .color(pal.text),
    );
    ui.add_space(8.0);
    if tool_icon(
        ui,
        &pal,
        regular::FILE_PLUS,
        &aulos_core::fl!("tagger-add-files"),
        true,
    )
    .clicked()
    {
        acts.push(Act::Msg(TaggerMessage::AddFiles));
    }
    if tool_icon(
        ui,
        &pal,
        regular::FOLDER_PLUS,
        &aulos_core::fl!("tagger-add-folder"),
        true,
    )
    .clicked()
    {
        acts.push(Act::Msg(TaggerMessage::AddFolder));
    }
    let lookup = tool_button(
        ui,
        &pal,
        &aulos_core::fl!("tagger-lookup-title"),
        regular::MAGNIFYING_GLASS,
        Look::Tool,
        !facts.busy && facts.has_target,
    );
    if lookup.clicked() {
        acts.push(Act::Msg(TaggerMessage::Lookup));
    }
    let scan = tool_button(
        ui,
        &pal,
        &aulos_core::fl!("tagger-ui-scan-button"),
        regular::FINGERPRINT,
        Look::Tool,
        !facts.busy && facts.has_target && key_set,
    );
    if !key_set {
        scan.on_disabled_hover_text(aulos_core::fl!("tagger-ui-scan-disabled-hover"));
    } else if scan.clicked() {
        acts.push(Act::Msg(TaggerMessage::Scan));
    }
    if tool_icon(
        ui,
        &pal,
        regular::LIST_PLUS,
        &aulos_core::fl!("tagger-add-as-release-tooltip"),
        !st.is_empty() && !facts.busy,
    )
    .clicked()
    {
        acts.push(Act::Msg(TaggerMessage::AddAsRelease(None)));
    }
    if st.loading() > 0 {
        let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        paint_spinner(ui, rect.center(), 15.0, pal.text_2);
    }
}

/// The AcoustID scan progress: label, rail and a cancel control.
fn header_scan(app: &App, ui: &mut Ui, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let Some(scan) = app.session.tagger.scan() else {
        return;
    };
    let total = scan.files.len();
    let (label, frac, busy) = match scan.phase {
        ScanPhase::Running => (
            aulos_core::fl!("tagger-ui-scan-progress", done = scan.done, total = total),
            if total == 0 {
                0.0
            } else {
                scan.done as f32 / total as f32
            },
            false,
        ),
        ScanPhase::Resolving => (aulos_core::fl!("tagger-ui-scan-loading-release"), 0.0, true),
        ScanPhase::Done | ScanPhase::NoMatch | ScanPhase::Cancelled => return,
    };
    ui.add_space(8.0);
    ui.label(
        RichText::new(label)
            .font(theme::mono(11.5))
            .color(pal.text_2),
    );
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width().clamp(80.0, 200.0), 4.0),
        Sense::hover(),
    );
    if busy {
        paint_busy_rail(ui, rect, pal.reed, pal.track);
    } else {
        paint_rail(ui, rect, frac, pal.reed, pal.track);
    }
    let (hit, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
    if icon_hit(
        ui,
        &pal,
        hit,
        Id::new("tagger-cancel-scan"),
        regular::X,
        &aulos_core::fl!("tagger-scan-cancel"),
        true,
    ) {
        acts.push(Act::Msg(TaggerMessage::CancelScan));
    }
}

fn header_right(app: &App, ui: &mut Ui, facts: &Facts, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let key_set = app.session.config.config.acoustid_key().is_some();
    ui.spacing_mut().item_spacing.x = 8.0;

    let save_label = match st.save_progress() {
        Some((done, total)) => aulos_core::fl!("tagger-saving", done = done, total = total),
        None if facts.savable > 0 => aulos_core::fl!("tagger-ui-save-files", count = facts.savable),
        None => aulos_core::fl!("tagger-save"),
    };
    if tool_button(
        ui,
        &pal,
        &save_label,
        regular::FLOPPY_DISK,
        Look::Primary,
        !st.is_saving() && facts.savable > 0,
    )
    .clicked()
    {
        acts.push(Act::Msg(TaggerMessage::Save));
    }

    let user_key = app.session.config.acoustid_key_stored == Some(true);
    let can_submit = !facts.busy && facts.submittable > 0 && key_set && user_key;
    let submit_label = if facts.submittable > 0 {
        aulos_core::fl!("tagger-submit-count", count = facts.submittable)
    } else {
        aulos_core::fl!("tagger-submit")
    };
    let submit = tool_button(
        ui,
        &pal,
        &submit_label,
        regular::PAPER_PLANE_TILT,
        Look::Tool,
        can_submit,
    );
    if !can_submit && facts.submittable > 0 && !user_key {
        submit.on_disabled_hover_text(aulos_core::fl!("tagger-ui-submit-key-hover"));
    } else if submit.clicked() {
        acts.push(Act::Msg(TaggerMessage::SubmitAcoustIds));
    }

    let revert = tool_button(
        ui,
        &pal,
        &aulos_core::fl!("tagger-revert"),
        regular::ARROW_COUNTER_CLOCKWISE,
        Look::Ghost,
        !st.is_saving() && facts.unsaved > 0,
    );
    if revert.clicked() {
        let msg = if facts.selected_dirty {
            TaggerMessage::RevertSelected
        } else {
            TaggerMessage::RevertAll
        };
        acts.push(Act::Msg(msg));
    }
}

fn banners(app: &App, ui: &mut Ui, facts: &Facts, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;

    if let Some(n) = st.notice() {
        let tone = if n.error { pal.danger } else { pal.ok };
        let icon = if n.error {
            regular::WARNING_CIRCLE
        } else {
            regular::INFO
        };
        if banner(
            ui,
            &pal,
            tone,
            icon,
            &n.text,
            Bar::None,
            &[&aulos_core::fl!("tagger-dismiss")],
        )
        .is_some()
        {
            acts.push(Act::Msg(TaggerMessage::DismissNotice));
        }
    }
    if let Some(confirm) = st.confirm() {
        let unsaved = match confirm {
            Confirm::ClearAll => facts.unsaved,
            Confirm::Remove(ids) => ids
                .iter()
                .filter_map(|id| st.file(*id))
                .filter(|f| f.has_unsaved_edits())
                .count(),
        };
        let text = aulos_core::fl!("tagger-ui-discard-banner", count = unsaved);
        match banner(
            ui,
            &pal,
            pal.warn,
            regular::WARNING,
            &text,
            Bar::None,
            &[
                &aulos_core::fl!("tagger-discard"),
                &aulos_core::fl!("tagger-cancel"),
            ],
        ) {
            Some(0) => acts.push(Act::Msg(TaggerMessage::ConfirmDiscard)),
            Some(_) => acts.push(Act::Msg(TaggerMessage::CancelDiscard)),
            None => {}
        }
    }
    // Selected files whose Album artist differs from the selection's majority.
    let selected = st.selected_files();
    if selected.len() > 1 {
        let artist = |f: &TagFile| f.edited.get(keys::ALBUMARTIST).unwrap_or("").to_owned();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for f in &selected {
            *counts.entry(artist(f)).or_default() += 1;
        }
        if let Some((major, _)) = counts
            .iter()
            .max_by_key(|(k, n)| (**n, std::cmp::Reverse((*k).clone())))
        {
            let odd: Vec<FileId> = selected
                .iter()
                .filter(|f| artist(f) != *major)
                .map(|f| f.id)
                .collect();
            if !odd.is_empty() {
                let text = aulos_core::fl!("tagger-ui-album-artist-mismatch", count = odd.len());
                if banner(
                    ui,
                    &pal,
                    pal.warn,
                    regular::WARNING,
                    &text,
                    Bar::None,
                    &[&aulos_core::fl!("tagger-ui-show-files")],
                )
                .is_some()
                {
                    acts.push(Act::Msg(TaggerMessage::SelectFile(odd[0])));
                    for id in &odd[1..] {
                        acts.push(Act::Msg(TaggerMessage::ToggleFile(*id)));
                    }
                }
            }
        }
    }
    let key_set = app.session.config.config.acoustid_key().is_some();
    if !key_set && !st.is_empty() {
        let text = aulos_core::fl!("tagger-ui-key-missing-banner");
        if banner(
            ui,
            &pal,
            pal.warn,
            regular::WARNING,
            &text,
            Bar::None,
            &[&aulos_core::fl!("tagger-open-settings")],
        )
        .is_some()
        {
            acts.push(Act::Msg(TaggerMessage::OpenSettings));
        }
    } else if app.session.config.acoustid_key_stored == Some(false) && facts.submittable > 0 {
        let text = aulos_core::fl!("tagger-ui-user-key-missing-banner");
        if banner(
            ui,
            &pal,
            pal.warn,
            regular::WARNING,
            &text,
            Bar::None,
            &[&aulos_core::fl!("tagger-open-settings")],
        )
        .is_some()
        {
            acts.push(Act::Msg(TaggerMessage::OpenSettings));
        }
    }
    if let Some(scan) = st.scan() {
        let total = scan.files.len();
        let failures = scan.failures.len();
        match scan.phase {
            ScanPhase::Running => {}
            ScanPhase::Resolving => {
                let text = aulos_core::fl!("tagger-ui-scan-identified", count = scan.identified);
                if banner(
                    ui,
                    &pal,
                    pal.reed,
                    regular::FINGERPRINT,
                    &text,
                    Bar::Busy,
                    &[&aulos_core::fl!("tagger-scan-cancel")],
                )
                .is_some()
                {
                    acts.push(Act::Msg(TaggerMessage::CancelScan));
                }
            }
            ScanPhase::NoMatch => {
                let text = aulos_core::fl!(
                    "tagger-scan-no-match",
                    identified = scan.identified,
                    total = total
                );
                banner(ui, &pal, pal.warn, regular::WARNING, &text, Bar::None, &[]);
            }
            ScanPhase::Done | ScanPhase::Cancelled => {
                if failures > 0 {
                    let text = aulos_core::fl!("tagger-scan-failures", count = failures);
                    banner(ui, &pal, pal.warn, regular::WARNING, &text, Bar::None, &[]);
                }
            }
        }
    }
    if st.is_submitting() {
        banner(
            ui,
            &pal,
            pal.reed,
            regular::PAPER_PLANE_TILT,
            &aulos_core::fl!("tagger-submitting"),
            Bar::Busy,
            &[],
        );
    }
    if let Some((done, total)) = st.save_progress() {
        let frac = if total == 0 {
            0.0
        } else {
            done as f32 / total as f32
        };
        banner(
            ui,
            &pal,
            pal.reed,
            regular::FLOPPY_DISK,
            &aulos_core::fl!("tagger-saving", done = done, total = total),
            Bar::Fraction(frac),
            &[],
        );
    }
}

// ---------------------------------------------------------------- files panel

/// One cluster of the list with the files shown for it.
struct Group<'a> {
    cluster: &'a Cluster,
    ids: Cow<'a, [FileId]>,
}

fn file_matches(file: &TagFile, needle: &str) -> bool {
    file.file_name().to_lowercase().contains(needle)
        || file
            .edited
            .get(keys::TITLE)
            .is_some_and(|t| t.to_lowercase().contains(needle))
}

fn groups<'a>(st: &'a TaggerState, filter: &str) -> Vec<Group<'a>> {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return st
            .clusters()
            .iter()
            .map(|cluster| Group {
                cluster,
                ids: Cow::Borrowed(&cluster.files[..]),
            })
            .collect();
    }
    let by_id: HashMap<FileId, &TagFile> = st.files().iter().map(|f| (f.id, f)).collect();
    st.clusters()
        .iter()
        .filter_map(|cluster| {
            let ids: Vec<FileId> = cluster
                .files
                .iter()
                .copied()
                .filter(|id| by_id.get(id).is_some_and(|f| file_matches(f, &needle)))
                .collect();
            (!ids.is_empty()).then_some(Group {
                cluster,
                ids: Cow::Owned(ids),
            })
        })
        .collect()
}

fn files_panel(app: &App, ui: &mut Ui, rect: Rect, facts: &Facts, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    paint_panel(ui, &pal, rect);
    let mut pui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
    );
    pui.shrink_clip_rect(rect);
    let ui = &mut pui;

    // Filter strip.
    let strip = Rect::from_min_size(rect.min, vec2(rect.width(), FILTER_H));
    ui.painter().line_segment(
        [
            pos2(strip.left(), strip.bottom() - 0.5),
            pos2(strip.right(), strip.bottom() - 0.5),
        ],
        Stroke::new(1.0, pal.stroke),
    );
    let buttons_w = 2.0 * 24.0 + 4.0;
    let input = Rect::from_min_size(
        strip.min + vec2(8.0, 8.0),
        vec2(strip.width() - 16.0 - buttons_w - 4.0, 28.0),
    );
    let mut filter = app.views.tagger.filter.clone();
    let filter_hint = aulos_core::fl!("tagger-ui-filter-placeholder");
    let tb = TextBox {
        left_pad: 26.0,
        ..TextBox::new(Id::new("tagger-filter"), &filter_hint)
    };
    let resp = text_box(ui, &pal, input, &tb, &mut filter);
    paint_icon(
        ui.painter(),
        pos2(input.left() + 14.0, input.center().y),
        regular::FUNNEL_SIMPLE,
        13.0,
        pal.text_3,
        false,
    );
    if resp.changed() {
        acts.push(Act::Filter(filter));
    }
    let busy = facts.busy;
    let remove = Rect::from_min_size(pos2(input.right() + 4.0, input.top()), vec2(24.0, 28.0));
    let clear = Rect::from_min_size(pos2(remove.right() + 4.0, input.top()), vec2(24.0, 28.0));
    if icon_hit(
        ui,
        &pal,
        remove,
        Id::new("tagger-remove"),
        regular::TRASH_SIMPLE,
        &aulos_core::fl!("tagger-ui-remove-selected-tooltip"),
        !busy && st.selected_count() > 0,
    ) {
        acts.push(Act::Msg(TaggerMessage::RemoveSelected));
    }
    if icon_hit(
        ui,
        &pal,
        clear,
        Id::new("tagger-clear"),
        regular::BROOM,
        &aulos_core::fl!("tagger-ui-clear-all-tooltip"),
        !busy && !st.is_empty(),
    ) {
        acts.push(Act::Msg(TaggerMessage::ClearAll));
    }

    let list_bottom = if st.is_empty() {
        rect.bottom()
    } else {
        rect.bottom() - FOOTER_H
    };
    let list_rect = Rect::from_min_max(
        pos2(rect.left(), rect.top() + FILTER_H),
        pos2(rect.right(), list_bottom),
    );

    if st.is_empty() {
        files_empty(app, ui, list_rect, acts);
        return;
    }

    file_list(app, ui, list_rect, acts);
    files_footer(
        app,
        ui,
        Rect::from_min_max(pos2(rect.left(), list_bottom), rect.max),
        facts,
        acts,
    );
}

fn files_empty(app: &App, ui: &mut Ui, rect: Rect, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    if st.loading() > 0 {
        let label =
            Rect::from_min_size(rect.min + vec2(14.0, 10.0), vec2(rect.width() - 28.0, 16.0));
        paint_text(
            ui.painter(),
            label,
            Align::Min,
            &aulos_core::fl!("tagger-loading", count = st.loading()),
            theme::regular(11.5),
            pal.text_2,
        );
        for i in 0..9 {
            let y = rect.top() + 40.0 + i as f32 * FILE_STEP;
            let inset = if i % 4 == 0 { 8.0 } else { 28.0 };
            let w = rect.width() - 12.0 - inset - 6.0 - (i % 3) as f32 * 24.0;
            skeleton(
                ui,
                Rect::from_min_size(
                    pos2(rect.left() + 6.0 + inset, y),
                    vec2(w.max(40.0), FILE_H - 6.0),
                ),
                5,
            );
        }
        return;
    }
    let mut cui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink(16.0))
            .layout(Layout::top_down(Align::Center)),
    );
    cui.spacing_mut().item_spacing.y = 8.0;
    // Roughly 200pt of content: tighten the top gap when the pane is short.
    cui.add_space(((rect.height() - 32.0 - 200.0) * 0.5).clamp(2.0, 36.0));
    cui.label(
        RichText::new(regular::MUSIC_NOTES)
            .font(theme::icon(30.0))
            .color(pal.text_3),
    );
    cui.label(
        RichText::new(aulos_core::fl!("tagger-empty-title"))
            .font(theme::semibold(15.0))
            .color(pal.text),
    );
    cui.add(
        egui::Label::new(
            RichText::new(aulos_core::fl!("tagger-empty-hint"))
                .font(theme::regular(12.0))
                .color(pal.text_2),
        )
        .wrap(),
    );
    cui.add_space(4.0);
    if pill_button_enabled(
        &mut cui,
        &aulos_core::fl!("tagger-ui-add-files-button"),
        Some(regular::FILE_PLUS),
        PillKind::Primary,
        30.0,
        0.0,
        true,
    )
    .clicked()
    {
        acts.push(Act::Msg(TaggerMessage::AddFiles));
    }
    if pill_button_enabled(
        &mut cui,
        &aulos_core::fl!("tagger-ui-add-folder-button"),
        Some(regular::FOLDER_PLUS),
        PillKind::Secondary,
        30.0,
        0.0,
        true,
    )
    .clicked()
    {
        acts.push(Act::Msg(TaggerMessage::AddFolder));
    }
}

fn file_list(app: &App, ui: &mut Ui, rect: Rect, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let groups = groups(st, &app.views.tagger.filter);
    if groups.is_empty() {
        let label =
            Rect::from_min_size(rect.min + vec2(14.0, 14.0), vec2(rect.width() - 28.0, 16.0));
        paint_text(
            ui.painter(),
            label,
            Align::Min,
            &aulos_core::fl!("tagger-ui-no-filter-match"),
            theme::regular(12.0),
            pal.text_3,
        );
        return;
    }
    let release_applied = st.lookup().is_some_and(LookupState::applied);
    let unmatched: HashSet<FileId> = match st.lookup().and_then(|l| l.plan.as_ref()) {
        Some(plan) if release_applied => plan.unmatched().into_iter().collect(),
        _ => HashSet::new(),
    };
    let anchor = app.views.tagger.anchor;
    let total: f32 = LIST_PAD * 2.0
        + groups
            .iter()
            .map(|g| HEAD_H + g.ids.len() as f32 * FILE_STEP)
            .sum::<f32>();

    let mut lui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
    );
    ScrollArea::vertical()
        .id_salt("tagger-files")
        .auto_shrink([false, false])
        .show_viewport(&mut lui, |ui, viewport| {
            ui.set_height(total);
            let origin = ui.max_rect().min;
            let width = ui.max_rect().width();
            let mut y = LIST_PAD;
            for (ci, group) in groups.iter().enumerate() {
                let height = HEAD_H + group.ids.len() as f32 * FILE_STEP;
                if y + height < viewport.min.y || y > viewport.max.y {
                    y += height;
                    continue;
                }
                let head = Rect::from_min_size(
                    origin + vec2(LIST_PAD, y),
                    vec2(width - 2.0 * LIST_PAD, HEAD_H),
                );
                if viewport.intersects(Rect::from_min_size(pos2(0.0, y), vec2(width, HEAD_H))) {
                    cluster_header(app, ui, head, ci, group, acts);
                }
                for (i, id) in group.ids.iter().enumerate() {
                    let row_y = y + HEAD_H + i as f32 * FILE_STEP;
                    if row_y + FILE_H < viewport.min.y {
                        continue;
                    }
                    if row_y > viewport.max.y {
                        break;
                    }
                    let Some(file) = st.file(*id) else { continue };
                    let row = Rect::from_min_size(
                        origin + vec2(LIST_PAD, row_y),
                        vec2(width - 2.0 * LIST_PAD, FILE_H),
                    );
                    let mark = if release_applied && unmatched.contains(id) {
                        Mark::Unmatched
                    } else {
                        Mark::None
                    };
                    file_row(app, ui, row, file, mark, anchor, acts);
                }
                y += height;
            }
        });
}

/// How a file row flags its relation to the applied release.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    None,
    Unmatched,
}

fn cluster_header(
    app: &App,
    ui: &mut Ui,
    rect: Rect,
    index: usize,
    group: &Group<'_>,
    acts: &mut Vec<Act>,
) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let cluster = group.cluster;
    let key = &cluster.key;
    let unclustered = key.is_unclustered();

    let resp = ui.interact(rect, Id::new(("tagger-cluster", index)), Sense::click());
    if resp.clicked() {
        acts.push(Act::Msg(TaggerMessage::SelectCluster(key.clone())));
    }
    let editable = group
        .ids
        .iter()
        .filter_map(|id| st.file(*id))
        .any(TagFile::is_editable);
    let busy = st.is_busy();
    let key_set = app.session.config.config.acoustid_key().is_some();
    resp.context_menu(|ui| {
        if ui
            .button(aulos_core::fl!("tagger-cluster-select"))
            .clicked()
        {
            acts.push(Act::Msg(TaggerMessage::SelectCluster(key.clone())));
            ui.close();
        }
        if ui
            .add_enabled(
                !busy && editable,
                egui::Button::new(aulos_core::fl!("tagger-cluster-lookup")),
            )
            .clicked()
        {
            acts.push(Act::Msg(TaggerMessage::LookupCluster(key.clone())));
            ui.close();
        }
        if ui
            .add_enabled(
                !busy && editable && key_set,
                egui::Button::new(aulos_core::fl!("tagger-cluster-scan")),
            )
            .clicked()
        {
            acts.push(Act::Msg(TaggerMessage::ScanCluster(key.clone())));
            ui.close();
        }
        if !unclustered
            && ui
                .add_enabled(
                    !busy,
                    egui::Button::new(aulos_core::fl!("tagger-cluster-add-release")),
                )
                .clicked()
        {
            acts.push(Act::Msg(TaggerMessage::AddAsRelease(Some(key.clone()))));
            ui.close();
        }
    });

    // Checkbox.
    let selected = st.cluster_selected(cluster);
    let some_selected = group.ids.iter().any(|id| st.is_selected(*id));
    let box_rect = Rect::from_center_size(
        pos2(rect.left() + 6.0 + 7.0, rect.top() + 8.0 + 12.0),
        vec2(14.0, 14.0),
    );
    let hit = ui.interact(
        box_rect.expand(5.0),
        Id::new(("tagger-cluster-check", index)),
        Sense::click(),
    );
    if hit.clicked() {
        acts.push(Act::Msg(TaggerMessage::ToggleCluster(key.clone())));
    }
    let r = CornerRadius::same(theme::radius::XS);
    if selected || some_selected {
        ui.painter().rect_filled(box_rect, r, pal.text);
        let glyph = if selected {
            regular::CHECK
        } else {
            regular::MINUS
        };
        paint_icon(ui.painter(), box_rect.center(), glyph, 10.0, pal.bg, false);
    } else {
        ui.painter().rect_stroke(
            box_rect,
            r,
            Stroke::new(1.0, pal.text_4),
            StrokeKind::Inside,
        );
    }

    // Cover.
    let cover = Rect::from_center_size(
        pos2(box_rect.right() + 8.0 + 12.0, box_rect.center().y),
        vec2(24.0, 24.0),
    );
    let first = group.ids.first().and_then(|id| st.file(*id));
    let seed = if unclustered {
        "unclustered".to_owned()
    } else {
        format!("{}-{}", cluster.artist, cluster.album)
    };
    let thumb = first.and_then(|f| app.covers.thumb(&library_album_key(&f.original)));
    cover_image(ui, &app.covers, cover, thumb, &seed, theme::radius::XS);

    // Labels.
    let text_left = cover.right() + 8.0;
    let title = if unclustered {
        aulos_core::fl!("tagger-unclustered")
    } else if cluster.album.is_empty() {
        aulos_core::fl!("tagger-ui-unknown-album")
    } else {
        cluster.album.clone()
    };
    let artist = if unclustered {
        String::new()
    } else if cluster.various {
        aulos_core::fl!("tagger-various-artists")
    } else if cluster.artist.is_empty() {
        aulos_core::fl!("tagger-unknown-artist")
    } else {
        cluster.artist.clone()
    };
    let meta = labels::dotted([artist, files_text(cluster.files.len())]);
    let text_rect = |top: f32, h: f32| {
        Rect::from_min_size(
            pos2(text_left, rect.top() + top),
            vec2((rect.right() - text_left - 4.0).max(10.0), h),
        )
    };
    paint_text(
        ui.painter(),
        text_rect(6.0, 14.0),
        Align::Min,
        &title,
        theme::semibold(12.5),
        pal.text,
    );
    paint_text(
        ui.painter(),
        text_rect(20.0, 13.0),
        Align::Min,
        &meta,
        theme::regular(11.0),
        pal.text_3,
    );
}

fn file_row(
    app: &App,
    ui: &mut Ui,
    rect: Rect,
    file: &TagFile,
    mark: Mark,
    anchor: Option<FileId>,
    acts: &mut Vec<Act>,
) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let id = file.id;
    let selected = st.is_selected(id);
    let resp = ui.interact(rect, Id::new(("tagger-file", id)), Sense::click());
    let radius = CornerRadius::same(5);
    if selected {
        ui.painter().rect_filled(rect, radius, pal.row_sel);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, radius, pal.hover);
    }
    if resp.clicked() {
        let modifiers = ui.input(|i| i.modifiers);
        if modifiers.command || modifiers.ctrl {
            acts.push(Act::Msg(TaggerMessage::ToggleFile(id)));
            acts.push(Act::Anchor(id));
        } else if modifiers.shift
            && let Some(from) = anchor
        {
            acts.push(Act::Range { from, to: id });
        } else {
            acts.push(Act::Msg(TaggerMessage::SelectFile(id)));
            acts.push(Act::Anchor(id));
        }
    }

    // Right side: status dot, score, lock.
    let mid = rect.center().y;
    let mut right = rect.right() - 6.0;
    let dot = match &file.status {
        FileStatus::Unchanged => None,
        FileStatus::Modified => Some(pal.warn),
        FileStatus::Saving => Some(app.accent.fill),
        FileStatus::Saved => Some(pal.ok),
        FileStatus::Error(_) => Some(pal.danger),
    };
    if let Some(color) = dot {
        ui.painter()
            .circle_filled(pos2(right - 3.0, mid), 3.0, color);
        right -= 6.0 + 8.0;
    }
    let score = match (&file.assignment, mark) {
        (Some(a), _) => Some((format!("{}%", labels::percent(a.score)), pal.text_3)),
        (None, Mark::Unmatched) => Some((aulos_core::fl!("tagger-ui-no-match"), pal.warn)),
        (None, Mark::None) => None,
    };
    if let Some((text, color)) = score {
        let w = paint_text(
            ui.painter(),
            Rect::from_min_max(
                pos2(rect.left() + 60.0, rect.top()),
                pos2(right, rect.bottom()),
            ),
            Align::Max,
            &text,
            theme::mono(10.5),
            color,
        );
        right -= w + 8.0;
    }
    if !file.is_editable() {
        paint_icon(
            ui.painter(),
            pos2(right - 5.0, mid),
            regular::LOCK_SIMPLE,
            11.0,
            pal.text_3,
            false,
        );
        right -= 11.0 + 8.0;
    }

    // Track number and title.
    let number = labels::track_label(&file.edited);
    let ink = if selected { pal.text } else { pal.text_2 };
    let mut left = rect.left() + 28.0;
    if !number.is_empty() {
        paint_text(
            ui.painter(),
            Rect::from_min_size(pos2(left, rect.top()), vec2(20.0, FILE_H)),
            Align::Max,
            &number,
            theme::mono(10.5),
            pal.text_3,
        );
    }
    left += 26.0;
    let title = file
        .edited
        .get(keys::TITLE)
        .filter(|t| !t.trim().is_empty())
        .map_or_else(|| file.file_name(), str::to_owned);
    paint_text(
        ui.painter(),
        Rect::from_min_max(
            pos2(left, rect.top()),
            pos2(right.max(left + 20.0), rect.bottom()),
        ),
        Align::Min,
        &title,
        theme::regular(12.5),
        ink,
    );

    resp.on_hover_ui(|ui| {
        ui.set_max_width(320.0);
        ui.label(RichText::new(file.file_name()).color(pal.text));
        match &file.status {
            FileStatus::Unchanged => {}
            FileStatus::Modified => {
                ui.label(RichText::new(aulos_core::fl!("tagger-status-modified")).color(pal.warn));
            }
            FileStatus::Saving => {
                ui.label(aulos_core::fl!("tagger-status-saving"));
            }
            FileStatus::Saved => {
                ui.label(RichText::new(aulos_core::fl!("tagger-status-saved")).color(pal.ok));
            }
            FileStatus::Error(error) => {
                ui.label(
                    RichText::new(aulos_core::fl!(
                        "tagger-status-error",
                        error = error.to_string()
                    ))
                    .color(pal.danger),
                );
            }
        }
        if let Some(a) = &file.assignment {
            ui.label(aulos_core::fl!(
                "tagger-assignment",
                number = a.number.to_string(),
                title = a.title.as_str(),
                score = labels::percent(a.score)
            ));
        } else if mark == Mark::Unmatched {
            ui.label(RichText::new(aulos_core::fl!("tagger-unmatched")).color(pal.warn));
        }
        if let Some(reason) = file.read_only_reason() {
            ui.label(
                RichText::new(aulos_core::fl!(
                    "tagger-ui-read-only-reason",
                    reason = reason.to_string()
                ))
                .color(pal.warn),
            );
        }
    });
}

fn files_footer(app: &App, ui: &mut Ui, rect: Rect, facts: &Facts, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    ui.painter().line_segment(
        [
            pos2(rect.left(), rect.top() + 0.5),
            pos2(rect.right(), rect.top() + 0.5),
        ],
        Stroke::new(1.0, pal.stroke),
    );
    let total = st.files().len();
    let selected = st.selected_count();
    let mut parts = vec![
        files_text(total),
        aulos_core::fl!("tagger-selected-count", count = selected),
    ];
    if st.loading() > 0 {
        parts.push(aulos_core::fl!("tagger-loading", count = st.loading()));
    }
    let summary = labels::dotted(parts);
    let y = rect.center().y;
    let mut x = rect.right() - 8.0;
    let (next, clicked) = link_right(
        ui,
        &pal,
        Id::new("tagger-revert-all"),
        &aulos_core::fl!("tagger-revert-all"),
        x,
        y,
        facts.unsaved > 0 && !st.is_saving(),
    );
    if clicked {
        acts.push(Act::Msg(TaggerMessage::RevertAll));
    }
    x = next;
    let (label, msg) = if selected < total {
        (
            aulos_core::fl!("tagger-select-all"),
            TaggerMessage::SelectAll,
        )
    } else {
        (
            aulos_core::fl!("tagger-clear"),
            TaggerMessage::ClearSelection,
        )
    };
    let (next, clicked) = link_right(ui, &pal, Id::new("tagger-select-all"), &label, x, y, true);
    if clicked {
        acts.push(Act::Msg(msg));
    }
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 10.0, rect.top()),
        pos2(next.max(rect.left() + 40.0), rect.bottom()),
    );
    paint_text(
        ui.painter(),
        text_rect,
        Align::Min,
        &summary,
        theme::regular(11.0),
        pal.text_3,
    );
}

// ---------------------------------------------------------------- meta panel

/// The x ranges (as full-height rects) of the three columns of `rect`.
fn meta_cols(rect: Rect) -> [Rect; 3] {
    let left = rect.left() + 12.0;
    let right = rect.right() - 12.0;
    let rest = (right - left - 120.0 - 2.0 * GAP).max(60.0);
    let disk_w = rest / 2.2;
    let new_w = rest * 1.2 / 2.2;
    let col =
        |x0: f32, w: f32| Rect::from_min_max(pos2(x0, rect.top()), pos2(x0 + w, rect.bottom()));
    [
        col(left, 120.0),
        col(left + 120.0 + GAP, disk_w),
        col(left + 120.0 + disk_w + 2.0 * GAP, new_w),
    ]
}

/// Paints `text` in italics, ellipsised to `rect`.
fn paint_italic(ui: &Ui, rect: Rect, text: &str, font: FontId, color: Color32) {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            color,
            italics: true,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping::truncate_at_width(rect.width());
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

/// The meta card. `pinned`: fills its (fixed-size) rect, top bar and captions
/// stay put and the rows scroll; otherwise it grows with its rows.
fn meta_card(app: &App, ui: &mut Ui, facts: &Facts, pinned: bool, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    Frame::new()
        .fill(pal.surface_1)
        .stroke(Stroke::new(1.0, pal.stroke))
        .corner_radius(CornerRadius::same(theme::radius::LG))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let width = ui.available_width();
            if pinned {
                ui.set_min_height(ui.available_height());
            }
            ui.set_width(width);
            meta_top_bar(app, ui, facts);

            let selected = st.selected_count();
            if selected == 0 {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.label(
                        RichText::new(regular::PENCIL_SIMPLE)
                            .font(theme::icon(30.0))
                            .color(pal.text_3),
                    );
                    let (title, body) = if st.is_empty() {
                        (
                            aulos_core::fl!("tagger-ui-meta-nothing-title"),
                            aulos_core::fl!("tagger-ui-meta-nothing-hint"),
                        )
                    } else {
                        (
                            aulos_core::fl!("tagger-metadata-empty-title"),
                            aulos_core::fl!("tagger-metadata-empty-hint"),
                        )
                    };
                    ui.label(
                        RichText::new(title)
                            .font(theme::semibold(15.0))
                            .color(pal.text),
                    );
                    ui.label(
                        RichText::new(body)
                            .font(theme::regular(12.0))
                            .color(pal.text_2),
                    );
                });
                ui.add_space(40.0);
                return;
            }
            let editable = facts.selection_editable;

            // Captions.
            let (caps, _) = ui.allocate_exact_size(vec2(width, CAPTIONS_H), Sense::hover());
            let captions = [
                aulos_core::fl!("tagger-ui-col-field"),
                aulos_core::fl!("tagger-ui-col-disk"),
                aulos_core::fl!("tagger-ui-col-new"),
            ];
            for (col, text) in meta_cols(caps).iter().zip(&captions) {
                paint_text_spaced(
                    ui.painter(),
                    Rect::from_min_max(
                        pos2(col.left(), caps.top()),
                        pos2(col.right(), caps.bottom()),
                    ),
                    Align::Min,
                    text,
                    theme::semibold(10.5),
                    pal.text_3,
                    0.84,
                );
            }
            ui.painter().line_segment(
                [
                    pos2(caps.left(), caps.bottom() - 0.5),
                    pos2(caps.right(), caps.bottom() - 0.5),
                ],
                Stroke::new(1.0, pal.stroke.alpha(0.6)),
            );

            if !editable {
                let (note, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
                let r = Rect::from_min_max(
                    pos2(note.left() + 12.0, note.top()),
                    pos2(note.right() - 12.0, note.bottom()),
                );
                paint_text(
                    ui.painter(),
                    r,
                    Align::Min,
                    &aulos_core::fl!("tagger-metadata-readonly"),
                    theme::regular(11.5),
                    pal.warn,
                );
            }

            let rows = st.rows();
            let add_open = app.views.tagger.add_open;
            let error = st.add_field_error();
            let footer_h = ADD_ROW_H
                + if add_open {
                    ADD_FORM_H + if error.is_some() { 18.0 } else { 0.0 }
                } else {
                    0.0
                };
            if pinned {
                let rest = (ui.available_height() - footer_h - 8.0).max(FIELD_ROW_H);
                ScrollArea::vertical()
                    .id_salt("tagger-rows")
                    .max_height(rest)
                    .auto_shrink([false, false])
                    .show_rows(ui, FIELD_ROW_H, rows.len(), |ui, range| {
                        for row in &rows[range] {
                            field_row(app, ui, row, editable, acts);
                        }
                    });
            } else {
                ui.add_space(4.0);
                for row in rows {
                    field_row(app, ui, row, editable, acts);
                }
                ui.add_space(4.0);
            }
            add_field_footer(app, ui, width, editable, acts);
        });
}

fn meta_top_bar(app: &App, ui: &mut Ui, facts: &Facts) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), TOP_BAR_H), Sense::hover());
    ui.painter().line_segment(
        [
            pos2(rect.left(), rect.bottom() - 0.5),
            pos2(rect.right(), rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, pal.stroke),
    );
    let inner = Rect::from_min_max(
        pos2(rect.left() + 12.0, rect.top()),
        pos2(rect.right() - 12.0, rect.bottom()),
    );
    let selected = st.selected_count();

    // Right side first, so the left text can use the remaining width.
    let mut right = inner.right();
    if facts.selected_dirty {
        let w = paint_text(
            ui.painter(),
            Rect::from_min_max(
                pos2(inner.left() + 60.0, inner.top()),
                pos2(right, inner.bottom()),
            ),
            Align::Max,
            &aulos_core::fl!("tagger-ui-edited"),
            theme::regular(12.0),
            pal.text_2,
        );
        ui.painter()
            .circle_filled(pos2(right - w - 9.0, inner.center().y), 3.0, pal.warn);
        right -= w + 18.0;
    }
    if selected > 0 && !facts.selection_editable {
        let w = paint_text(
            ui.painter(),
            Rect::from_min_max(
                pos2(inner.left() + 60.0, inner.top()),
                pos2(right, inner.bottom()),
            ),
            Align::Max,
            &aulos_core::fl!("tagger-ui-readonly-label"),
            theme::regular(12.0),
            pal.text_3,
        );
        paint_icon(
            ui.painter(),
            pos2(right - w - 11.0, inner.center().y),
            regular::LOCK_SIMPLE,
            12.0,
            pal.text_3,
            false,
        );
        right -= w + 28.0;
    }

    let title = match selected {
        0 => aulos_core::fl!("tagger-ui-no-files-selected"),
        1 => st
            .selected_files()
            .first()
            .map_or_else(String::new, |f| f.file_name()),
        n => aulos_core::fl!("tagger-metadata-selection", count = n),
    };
    let ink = if selected == 0 { pal.text_3 } else { pal.text };
    let avail = Rect::from_min_max(
        inner.min,
        pos2((right - 8.0).max(inner.left() + 20.0), inner.bottom()),
    );
    let w = paint_text(
        ui.painter(),
        avail,
        Align::Min,
        &title,
        theme::semibold(13.0),
        ink,
    );
    let album = st
        .rows()
        .iter()
        .find(|r| r.key == keys::ALBUM)
        .map(|r| r.new.display())
        .unwrap_or_default();
    if selected > 0 && !album.is_empty() {
        let left = inner.left() + w + 10.0;
        if left + 30.0 < right {
            paint_text(
                ui.painter(),
                Rect::from_min_max(pos2(left, inner.top()), pos2(right - 8.0, inner.bottom())),
                Align::Min,
                &album,
                theme::regular(13.0),
                pal.text_3,
            );
        }
    }
}

fn field_row(app: &App, ui: &mut Ui, row: &FieldRow, editable: bool, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), FIELD_ROW_H), Sense::hover());
    let hovered = ui.rect_contains_pointer(rect);
    let [field, disk, new] = meta_cols(rect);
    let mid = rect.center().y;

    // Field name.
    let name = labels::field_label(&row.key);
    let name_rect = Rect::from_min_max(
        field.min,
        pos2(
            field.right() - if row.changed { 12.0 } else { 0.0 },
            field.bottom(),
        ),
    );
    let w = paint_text(
        ui.painter(),
        name_rect,
        Align::Min,
        &name,
        theme::regular(13.0),
        pal.text_2,
    );
    if row.changed {
        ui.painter().circle_filled(
            pos2(field.left() + w.min(name_rect.width()) + 9.0, mid),
            3.0,
            pal.warn,
        );
    }
    ui.interact(field, Id::new(("tag-field-name", &row.key)), Sense::hover())
        .on_hover_text(row.key.as_str());

    // On disk.
    match &row.original {
        FieldValue::Empty => {}
        FieldValue::Same(values) => {
            let text = join_values(values);
            paint_text(
                ui.painter(),
                disk,
                Align::Min,
                &first_line(&text),
                theme::regular(12.5),
                pal.text_15,
            );
        }
        FieldValue::Mixed => paint_italic(
            ui,
            disk,
            &aulos_core::fl!("tagger-different-values"),
            theme::regular(12.5),
            pal.text_3,
        ),
    }

    // New value.
    let boxed = Rect::from_min_size(pos2(new.left(), mid - 14.0), vec2(new.width(), 28.0));
    if row.multiline {
        let radius = CornerRadius::same(theme::radius::SM);
        ui.painter().rect_filled(boxed, radius, pal.bg);
        ui.painter().rect_stroke(
            boxed,
            radius,
            Stroke::new(1.0, pal.stroke),
            StrokeKind::Inside,
        );
        let text = row.new.display();
        paint_text(
            ui.painter(),
            boxed.shrink2(vec2(8.0, 0.0)),
            Align::Min,
            &first_line(&text),
            theme::regular(12.5),
            pal.text_2,
        );
        ui.interact(
            boxed,
            Id::new(("tag-field-multi", &row.key)),
            Sense::hover(),
        )
        .on_hover_text(aulos_core::fl!("tagger-multiline-tooltip"));
        return;
    }
    let mut buf = st.input_text(row);
    let hint = if row.new.is_mixed() {
        aulos_core::fl!("tagger-different-values")
    } else {
        String::new()
    };
    let tb = TextBox {
        changed: row.changed,
        right_pad: if editable { ACTIONS_W } else { 0.0 },
        enabled: editable,
        ..TextBox::new(Id::new(("tag-field", &row.key)), &hint)
    };
    let resp = text_box(ui, &pal, boxed, &tb, &mut buf);
    if resp.changed() {
        acts.push(Act::Msg(TaggerMessage::EditField {
            key: row.key.clone(),
            text: buf,
        }));
    }
    if editable {
        let delete = Rect::from_center_size(pos2(boxed.right() - 13.0, mid), vec2(20.0, 20.0));
        let revert = delete.translate(vec2(-22.0, 0.0));
        if row.changed
            && icon_hit(
                ui,
                &pal,
                revert,
                Id::new(("tag-revert", &row.key)),
                regular::ARROW_COUNTER_CLOCKWISE,
                &aulos_core::fl!("tagger-revert-field"),
                true,
            )
        {
            acts.push(Act::Msg(TaggerMessage::RevertField(row.key.clone())));
        }
        if (hovered || resp.has_focus())
            && icon_hit(
                ui,
                &pal,
                delete,
                Id::new(("tag-delete", &row.key)),
                regular::TRASH,
                &aulos_core::fl!("tagger-delete-field"),
                true,
            )
        {
            acts.push(Act::Msg(TaggerMessage::DeleteField(row.key.clone())));
        }
    }
}

fn add_field_footer(app: &App, ui: &mut Ui, width: f32, editable: bool, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let open = app.views.tagger.add_open;

    let (rect, _) = ui.allocate_exact_size(vec2(width, ADD_ROW_H), Sense::hover());
    let hit = ui.interact(
        Rect::from_min_max(
            pos2(rect.left() + 6.0, rect.top()),
            pos2(rect.right() - 6.0, rect.bottom()),
        ),
        Id::new("tag-add-toggle"),
        Sense::click(),
    );
    if hit.hovered() {
        ui.painter()
            .rect_filled(hit.rect, CornerRadius::same(theme::radius::SM), pal.hover);
    }
    let glyph = if open { regular::MINUS } else { regular::PLUS };
    paint_icon(
        ui.painter(),
        pos2(rect.left() + 12.0 + 6.5, rect.center().y),
        glyph,
        13.0,
        pal.text_2,
        false,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_max(
            pos2(rect.left() + 34.0, rect.top()),
            pos2(rect.right() - 12.0, rect.bottom()),
        ),
        Align::Min,
        &aulos_core::fl!("tagger-ui-add-field-toggle"),
        theme::regular(12.5),
        pal.text_2,
    );
    if hit.clicked() {
        acts.push(Act::AddOpen(!open));
    }
    if !open {
        return;
    }

    let draft = st.add_field();
    let error = st.add_field_error();
    let mut form_h = ADD_FORM_H;
    if error.is_some() {
        form_h += 18.0;
    }
    let (form, _) = ui.allocate_exact_size(vec2(width, form_h), Sense::hover());
    let inner = Rect::from_min_max(
        pos2(form.left() + 12.0, form.top() + 4.0),
        pos2(form.right() - 12.0, form.bottom()),
    );

    // Row 1: known field dropdown + custom name.
    let known_w = (inner.width() * 0.45).min(220.0);
    let known_rect = Rect::from_min_size(inner.min, vec2(known_w, 28.0));
    let known_value = draft.known.and_then(|i| KNOWN_KEYS.get(i)).map_or_else(
        || aulos_core::fl!("tagger-add-field-known"),
        |k| labels::field_label(k),
    );
    let dd = Dropdown {
        id: Id::new("tag-add-known"),
        value: &known_value,
        selected: draft.known,
        count: KNOWN_KEYS.len(),
        enabled: editable,
        placeholder: draft.known.is_none(),
    };
    if let Some(i) = dropdown(ui, &pal, known_rect, &dd, |i| {
        labels::field_label(KNOWN_KEYS[i])
    }) {
        acts.push(Act::Msg(TaggerMessage::AddFieldKnown(i)));
    }
    let custom_rect = Rect::from_min_max(
        pos2(known_rect.right() + 6.0, inner.top()),
        pos2(inner.right(), inner.top() + 28.0),
    );
    let mut custom = draft.custom.clone();
    let custom_hint = aulos_core::fl!("tagger-add-field-custom");
    let tb = TextBox {
        enabled: editable,
        ..TextBox::new(Id::new("tag-add-custom"), &custom_hint)
    };
    let custom_resp = text_box(ui, &pal, custom_rect, &tb, &mut custom);
    if custom_resp.changed() {
        acts.push(Act::Msg(TaggerMessage::AddFieldCustom(custom)));
    }

    // Row 2: value + Add.
    let row2 = inner.top() + 34.0;
    let add_w = 64.0;
    let value_rect = Rect::from_min_max(
        pos2(inner.left(), row2),
        pos2(inner.right() - add_w - 6.0, row2 + 28.0),
    );
    let mut value = draft.value.clone();
    let value_hint = aulos_core::fl!("tagger-add-field-value");
    let tb = TextBox {
        enabled: editable,
        ..TextBox::new(Id::new("tag-add-value"), &value_hint)
    };
    let value_resp = text_box(ui, &pal, value_rect, &tb, &mut value);
    if value_resp.changed() {
        acts.push(Act::Msg(TaggerMessage::AddFieldValue(value)));
    }
    let enter = ui.input(|i| i.key_pressed(Key::Enter));
    if enter && (custom_resp.lost_focus() || value_resp.lost_focus()) && editable {
        acts.push(Act::Msg(TaggerMessage::AddFieldCommit));
    }
    let add_rect = Rect::from_min_size(pos2(inner.right() - add_w, row2), vec2(add_w, 28.0));
    let mut add_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(add_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    if pill_button_enabled(
        &mut add_ui,
        &aulos_core::fl!("tagger-add-field"),
        None,
        PillKind::Secondary,
        28.0,
        add_w,
        editable,
    )
    .clicked()
    {
        acts.push(Act::Msg(TaggerMessage::AddFieldCommit));
    }

    // Row 3: hint and error.
    let mut y = row2 + 34.0;
    if let Some(error) = error {
        paint_text(
            ui.painter(),
            Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 16.0)),
            Align::Min,
            &labels::add_field_error_text(error),
            theme::regular(11.5),
            pal.danger,
        );
        y += 18.0;
    }
    let hint_rect = Rect::from_min_max(
        pos2(inner.left(), y),
        pos2(inner.right(), form.bottom() - 2.0),
    );
    let mut hui = ui.new_child(
        UiBuilder::new()
            .max_rect(hint_rect)
            .layout(Layout::top_down(Align::Min)),
    );
    hint(&mut hui, &pal, &aulos_core::fl!("tagger-multi-hint"));
}

// ---------------------------------------------------------------- side cards

fn side_cards(app: &App, ui: &mut Ui, facts: &Facts, acts: &mut Vec<Act>) {
    cover_card(app, ui, facts, acts);
    if app.session.tagger.lookup().is_some() {
        lookup_card(app, ui, acts);
    }
    acoustid_card(app, ui, acts);
}

fn cover_card(app: &App, ui: &mut Ui, facts: &Facts, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    card(ui, &pal, |ui| {
        caps_label(ui, &aulos_core::fl!("tagger-ui-cover-caps"));
        if st.selected_count() == 0 {
            hint(ui, &pal, &aulos_core::fl!("tagger-ui-cover-select"));
            return;
        }
        let editable = facts.selection_editable;
        let pictures = st.pictures();
        if pictures.is_empty() {
            hint(ui, &pal, &aulos_core::fl!("tagger-cover-empty"));
        } else {
            let any_changed = pictures.iter().any(|p| p.changed);
            let gap = 8.0;
            let cell_w = ((ui.available_width() - gap) / 2.0).floor();
            for pair in pictures.chunks(2) {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for picture in pair {
                        ui.allocate_ui_with_layout(
                            vec2(cell_w, 0.0),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(cell_w);
                                picture_cell(app, ui, picture, cell_w, any_changed, editable, acts);
                            },
                        );
                    }
                });
            }
        }

        // Add picture, with the type it will get.
        let new_kind = st.new_picture_kind();
        let selected = PICTURE_KINDS.iter().position(|k| *k == new_kind);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if pill_button_enabled(
                ui,
                &aulos_core::fl!("tagger-ui-cover-add-picture"),
                Some(regular::IMAGE),
                PillKind::Secondary,
                28.0,
                0.0,
                editable,
            )
            .clicked()
            {
                acts.push(Act::Msg(TaggerMessage::AddPicture));
            }
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width().max(60.0), 28.0), Sense::hover());
            let value = labels::picture_kind_label(new_kind);
            let dd = Dropdown {
                id: Id::new("tag-new-picture-kind"),
                value: &value,
                selected,
                count: PICTURE_KINDS.len(),
                enabled: editable,
                placeholder: false,
            };
            if let Some(i) = dropdown(ui, &pal, rect, &dd, |i| {
                labels::picture_kind_label(PICTURE_KINDS[i])
            }) {
                acts.push(Act::Msg(TaggerMessage::NewPictureKind(i)));
            }
        });
        let release_loaded = st.lookup().is_some_and(|l| l.release.ready().is_some());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            if pill_button_enabled(
                ui,
                &aulos_core::fl!("tagger-cover-revert"),
                Some(regular::ARROW_COUNTER_CLOCKWISE),
                PillKind::Ghost,
                28.0,
                0.0,
                editable,
            )
            .clicked()
            {
                acts.push(Act::Msg(TaggerMessage::RevertPictures));
            }
            if pill_button_enabled(
                ui,
                &aulos_core::fl!("tagger-ui-cover-fetch"),
                Some(regular::DOWNLOAD_SIMPLE),
                PillKind::Secondary,
                28.0,
                0.0,
                editable && release_loaded,
            )
            .clicked()
            {
                acts.push(Act::Msg(TaggerMessage::FetchCover));
            }
        });
        if !release_loaded {
            hint(ui, &pal, &aulos_core::fl!("tagger-ui-cover-fetch-hint"));
        }
    });
}

fn picture_cell(
    app: &App,
    ui: &mut Ui,
    picture: &aulos_core::tagger::PictureRow,
    width: f32,
    any_changed: bool,
    editable: bool,
    acts: &mut Vec<Act>,
) {
    let pal = app.pal;
    ui.spacing_mut().item_spacing.y = 5.0;
    let (cell, _) = ui.allocate_exact_size(vec2(width, width), Sense::hover());
    let thumb = cell.shrink(4.0);
    let texture = app.views.tagger.thumbs.get(picture.hash);
    cover_image(
        ui,
        &app.covers,
        thumb,
        texture,
        &format!("picture-{}", picture.hash),
        theme::radius::SM,
    );
    if picture.changed {
        ui.painter().rect_stroke(
            thumb.expand(3.0),
            CornerRadius::same(9),
            Stroke::new(2.0, pal.text),
            StrokeKind::Middle,
        );
    } else if any_changed {
        ui.painter().rect_filled(
            thumb,
            CornerRadius::same(theme::radius::SM),
            pal.surface_1.alpha(0.4),
        );
    }
    if texture.is_none() {
        ui.interact(
            thumb,
            Id::new(("tag-pic", picture.hash, picture.kind)),
            Sense::hover(),
        )
        .on_hover_text(aulos_core::fl!("tagger-cover-placeholder"));
    }

    // Type and remove.
    let (row, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    let (hash, kind) = (picture.hash, picture.kind);
    if editable {
        let dd_rect = Rect::from_min_max(row.min, pos2(row.right() - 26.0, row.bottom()));
        let (names, selected) = labels::picture_kind_choices(kind);
        let value = selected
            .and_then(|i| names.get(i))
            .cloned()
            .unwrap_or_default();
        let dd = Dropdown {
            id: Id::new(("tag-pic-kind", hash, kind)),
            value: &value,
            selected,
            count: names.len(),
            enabled: true,
            placeholder: false,
        };
        if let Some(i) = dropdown(ui, &pal, dd_rect, &dd, |i| names[i].clone())
            && let Some(&new_kind) = PICTURE_KINDS.get(i)
        {
            acts.push(Act::Msg(TaggerMessage::SetPictureKind {
                hash,
                kind,
                new_kind,
            }));
        }
        let trash = Rect::from_min_size(pos2(row.right() - 24.0, row.top()), vec2(24.0, 24.0));
        if icon_hit(
            ui,
            &pal,
            trash,
            Id::new(("tag-pic-remove", hash, kind)),
            regular::TRASH,
            &aulos_core::fl!("tagger-cover-remove"),
            true,
        ) {
            acts.push(Act::Msg(TaggerMessage::RemovePicture { hash, kind }));
        }
    } else {
        let font = if picture.changed {
            theme::semibold(11.5)
        } else {
            theme::regular(11.5)
        };
        paint_text(
            ui.painter(),
            row,
            Align::Min,
            &labels::picture_kind_label(kind),
            font,
            pal.text,
        );
    }
    let details = aulos_core::fl!(
        "tagger-cover-details",
        mime = picture.mime.as_str(),
        size = labels::format_size(picture.bytes)
    );
    label_text(ui, &details, theme::mono(10.5), pal.text_3);
    if picture.files < picture.of {
        label_text(
            ui,
            &aulos_core::fl!(
                "tagger-cover-in-files",
                count = picture.files,
                total = picture.of
            ),
            theme::regular(11.0),
            pal.warn,
        );
    }
}

fn lookup_card(app: &App, ui: &mut Ui, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let Some(lookup) = st.lookup() else { return };
    card(ui, &pal, |ui| {
        let title = match lookup.source {
            LookupSource::Search => aulos_core::fl!("tagger-ui-lookup-caps-musicbrainz"),
            LookupSource::AcoustId => aulos_core::fl!("tagger-ui-lookup-caps-acoustid"),
        };
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        paint_text_spaced(
            ui.painter(),
            Rect::from_min_max(head.min, pos2(head.right() - 24.0, head.bottom())),
            Align::Min,
            &title,
            theme::semibold(10.5),
            pal.text_3,
            0.84,
        );
        let close = Rect::from_min_size(pos2(head.right() - 20.0, head.top()), vec2(20.0, 18.0));
        if icon_hit(
            ui,
            &pal,
            close,
            Id::new("tag-lookup-close"),
            regular::X,
            &aulos_core::fl!("tagger-lookup-close"),
            true,
        ) {
            acts.push(Act::Msg(TaggerMessage::CloseLookup));
        }
        hint(
            ui,
            &pal,
            &aulos_core::fl!("tagger-lookup-files", count = lookup.targets.len()),
        );
        if lookup.source == LookupSource::AcoustId
            && let Some(choice) = &lookup.acoustid_choice
        {
            let text = aulos_core::fl!(
                "tagger-lookup-acoustid-summary",
                matched = choice.covered.len(),
                total = lookup.targets.len(),
                release = choice.title.as_str()
            );
            tone_hint(ui, &pal, &text, Tone::Accent);
        }

        // Search terms.
        let searching = lookup.candidates.is_loading();
        let mut artist = lookup.artist.clone();
        let mut album = lookup.album.clone();
        let width = ui.available_width();
        let (r1, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
        let artist_hint = aulos_core::fl!("tagger-lookup-artist");
        let tb = TextBox::new(Id::new("tag-query-artist"), &artist_hint);
        let artist_resp = text_box(ui, &pal, r1, &tb, &mut artist);
        if artist_resp.changed() {
            acts.push(Act::Msg(TaggerMessage::QueryArtist(artist)));
        }
        let (r2, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
        let album_hint = aulos_core::fl!("tagger-lookup-album");
        let tb = TextBox::new(Id::new("tag-query-album"), &album_hint);
        let album_resp = text_box(ui, &pal, r2, &tb, &mut album);
        if album_resp.changed() {
            acts.push(Act::Msg(TaggerMessage::QueryAlbum(album)));
        }
        let enter = ui.input(|i| i.key_pressed(Key::Enter));
        if enter && (artist_resp.lost_focus() || album_resp.lost_focus()) && !searching {
            acts.push(Act::Msg(TaggerMessage::SearchAgain));
        }
        if pill_button_enabled(
            ui,
            &aulos_core::fl!("tagger-lookup-search"),
            Some(regular::MAGNIFYING_GLASS),
            PillKind::Secondary,
            28.0,
            0.0,
            !searching,
        )
        .clicked()
        {
            acts.push(Act::Msg(TaggerMessage::SearchAgain));
        }

        // Candidates.
        match &lookup.candidates {
            Remote::Idle => hint(ui, &pal, &aulos_core::fl!("tagger-lookup-idle")),
            Remote::Loading => busy_line(ui, &pal, &aulos_core::fl!("tagger-lookup-searching")),
            Remote::Failed(error) => tone_hint(
                ui,
                &pal,
                &aulos_core::fl!("tagger-lookup-failed", error = error.to_string()),
                Tone::Danger,
            ),
            Remote::Ready(items) if items.is_empty() => {
                hint(ui, &pal, &aulos_core::fl!("tagger-lookup-none"))
            }
            Remote::Ready(items) => {
                let shown = items.len().min(CANDIDATES_SHOWN);
                let spacing = ui.spacing().item_spacing.y;
                ui.spacing_mut().item_spacing.y = 4.0;
                ScrollArea::vertical()
                    .id_salt("tag-candidates")
                    .max_height(shown as f32 * CANDIDATE_STEP)
                    .auto_shrink([false, true])
                    .show_rows(ui, CANDIDATE_H, items.len(), |ui, range| {
                        for candidate in &items[range] {
                            let chosen = lookup.chosen.as_deref() == Some(candidate.id.as_str());
                            candidate_row(ui, &pal, candidate, chosen, acts);
                        }
                    });
                ui.spacing_mut().item_spacing.y = spacing;
            }
        }

        release_state(app, ui, lookup, acts);
    });
}

fn candidate_row(
    ui: &mut Ui,
    pal: &Palette,
    candidate: &aulos_core::online::musicbrainz::ReleaseCandidate,
    chosen: bool,
    acts: &mut Vec<Act>,
) {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), CANDIDATE_H), Sense::click());
    let radius = CornerRadius::same(theme::radius::SM);
    if chosen {
        ui.painter().rect_filled(rect, radius, pal.surface_sel);
        ui.painter().rect_stroke(
            rect.shrink(0.5),
            radius,
            Stroke::new(1.0, pal.text_5),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, radius, pal.hover);
    }
    let inner = rect.shrink2(vec2(6.0, 6.0));
    let mut right = inner.right();
    if candidate.score > 0 {
        let ink = if chosen { pal.ok } else { pal.text_2 };
        let w = paint_text(
            ui.painter(),
            Rect::from_min_max(
                pos2(inner.left() + 40.0, inner.top()),
                pos2(right, inner.bottom()),
            ),
            Align::Max,
            &format!("{}%", candidate.score),
            theme::mono_semibold(11.0),
            ink,
        );
        right -= w + 8.0;
    }
    let title = if candidate.disambiguation.trim().is_empty() {
        candidate.title.clone()
    } else {
        format!("{} ({})", candidate.title, candidate.disambiguation)
    };
    let text_w = (right - inner.left()).max(20.0);
    paint_text(
        ui.painter(),
        Rect::from_min_size(inner.min, vec2(text_w, 16.0)),
        Align::Min,
        &title,
        theme::semibold(12.5),
        pal.text,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_size(inner.min + vec2(0.0, 18.0), vec2(text_w, 14.0)),
        Align::Min,
        &labels::candidate_details(candidate),
        theme::regular(11.0),
        pal.text_2,
    );
    if resp.clicked() {
        acts.push(Act::Msg(TaggerMessage::ChooseRelease(candidate.id.clone())));
    }
}

fn release_state(app: &App, ui: &mut Ui, lookup: &LookupState, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    match &lookup.release {
        Remote::Idle => {}
        Remote::Loading => busy_line(ui, &pal, &aulos_core::fl!("tagger-lookup-loading-release")),
        Remote::Failed(error) => tone_hint(
            ui,
            &pal,
            &aulos_core::fl!("tagger-lookup-release-failed", error = error.to_string()),
            Tone::Danger,
        ),
        Remote::Ready(release) => {
            if let Some(plan) = &lookup.plan {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(fill::CHECK_CIRCLE)
                            .font(theme::icon_fill(14.0))
                            .color(pal.ok),
                    );
                    ui.label(
                        RichText::new(aulos_core::fl!(
                            "tagger-lookup-matched",
                            matched = plan.matched(),
                            total = plan.entries.len()
                        ))
                        .font(theme::semibold(12.5))
                        .color(pal.text),
                    );
                });
                let unused = plan.unused_tracks();
                if unused > 0 {
                    hint(
                        ui,
                        &pal,
                        &aulos_core::fl!("tagger-lookup-unused", count = unused),
                    );
                }
                let names: Vec<String> = plan
                    .unmatched()
                    .into_iter()
                    .filter_map(|id| st.file(id))
                    .map(TagFile::file_name)
                    .collect();
                if !names.is_empty() {
                    let (shown, more) = labels::name_list(&names, 5);
                    let text = if more == 0 {
                        aulos_core::fl!("tagger-lookup-unmatched", files = shown)
                    } else {
                        aulos_core::fl!("tagger-lookup-unmatched-more", files = shown, more = more)
                    };
                    tone_hint(ui, &pal, &text, Tone::Warning);
                }
                match &lookup.cover {
                    Remote::Idle if !app.session.config.config.tagger_use_caa_cover => {
                        hint(ui, &pal, &aulos_core::fl!("tagger-ui-lookup-cover-off"));
                    }
                    Remote::Idle => {}
                    Remote::Loading => {
                        busy_line(ui, &pal, &aulos_core::fl!("tagger-lookup-cover-loading"))
                    }
                    Remote::Failed(error) => tone_hint(
                        ui,
                        &pal,
                        &aulos_core::fl!("tagger-lookup-cover-failed", error = error.to_string()),
                        Tone::Warning,
                    ),
                    Remote::Ready(None) => {
                        hint(ui, &pal, &aulos_core::fl!("tagger-lookup-cover-none"))
                    }
                    Remote::Ready(Some(_)) => tone_hint(
                        ui,
                        &pal,
                        &aulos_core::fl!("tagger-lookup-cover-found"),
                        Tone::Success,
                    ),
                }
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if pill_button_enabled(
                    ui,
                    &aulos_core::fl!("tagger-lookup-open"),
                    Some(regular::ARROW_SQUARE_OUT),
                    PillKind::Secondary,
                    28.0,
                    0.0,
                    true,
                )
                .clicked()
                {
                    acts.push(Act::Msg(TaggerMessage::OpenReleasePage(release.id.clone())));
                }
                if lookup.applied()
                    && pill_button_enabled(
                        ui,
                        &aulos_core::fl!("tagger-lookup-discard"),
                        Some(regular::ARROW_COUNTER_CLOCKWISE),
                        PillKind::Ghost,
                        28.0,
                        0.0,
                        true,
                    )
                    .clicked()
                {
                    acts.push(Act::Msg(TaggerMessage::DiscardRelease));
                }
            });
        }
    }
}

/// The file has something the AcoustID card can show.
fn acoustid_relevant(file: &TagFile) -> bool {
    file.acoustid.is_some()
        || file.fingerprint.is_some()
        || !matches!(file.submission, SubmissionState::NotSubmitted)
}

fn acoustid_card(app: &App, ui: &mut Ui, acts: &mut Vec<Act>) {
    let pal = app.pal;
    let st = &app.session.tagger;
    let selected = st.selected_files();
    if st.scan().is_none() && !selected.iter().any(|f| acoustid_relevant(f)) {
        return;
    }
    card(ui, &pal, |ui| {
        let mut title = String::from("ACOUSTID");
        if let [only] = selected.as_slice() {
            let name = only
                .edited
                .get(keys::TITLE)
                .filter(|t| !t.trim().is_empty())
                .map_or_else(|| only.file_name(), str::to_owned);
            title.push_str(&format!(" · {}", name.to_uppercase()));
        }
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
        paint_text_spaced(
            ui.painter(),
            head,
            Align::Min,
            &title,
            theme::semibold(10.5),
            pal.text_3,
            0.84,
        );
        if let Some(scan) = st.scan() {
            let text = aulos_core::fl!(
                "tagger-ui-identified",
                identified = scan.identified,
                total = scan.files.len()
            );
            ui.label(
                RichText::new(text)
                    .font(theme::regular(12.0))
                    .color(pal.text_2),
            );
        }

        let files: Vec<&TagFile> = selected
            .iter()
            .copied()
            .filter(|f| acoustid_relevant(f))
            .collect();
        for file in files.iter().take(8) {
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
            let hit = file.acoustid.is_some();
            let (glyph, color, filled) = if hit {
                (fill::CHECK_CIRCLE, pal.ok, true)
            } else {
                (regular::CIRCLE, pal.text_4, false)
            };
            paint_icon(
                ui.painter(),
                pos2(row.left() + 7.0, row.center().y),
                glyph,
                14.0,
                color,
                filled,
            );
            let mut right = row.right();
            if let Some(a) = &file.assignment {
                let w = paint_text(
                    ui.painter(),
                    Rect::from_min_max(
                        pos2(row.left() + 40.0, row.top()),
                        pos2(right, row.bottom()),
                    ),
                    Align::Max,
                    &format!("{}%", labels::percent(a.score)),
                    theme::mono_semibold(11.0),
                    pal.text,
                );
                right -= w + 8.0;
            }
            let ink = if hit { pal.text } else { pal.text_2 };
            paint_text(
                ui.painter(),
                Rect::from_min_max(
                    pos2(row.left() + 22.0, row.top()),
                    pos2(right.max(row.left() + 30.0), row.bottom()),
                ),
                Align::Min,
                &file.file_name(),
                theme::regular(12.5),
                ink,
            );
            if let Some((text, tone)) = labels::submission_text(&file.submission) {
                tone_hint(ui, &pal, &text, tone);
            }
        }
        if files.len() > 8 {
            hint(
                ui,
                &pal,
                &aulos_core::fl!("tagger-ui-and-more", count = (files.len() - 8)),
            );
        }

        if let [only] = selected.as_slice() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if let Some(hit) = &only.acoustid
                    && pill_button_enabled(
                        ui,
                        "AcoustID",
                        Some(regular::ARROW_SQUARE_OUT),
                        PillKind::Ghost,
                        26.0,
                        0.0,
                        true,
                    )
                    .on_hover_text(aulos_core::fl!("tagger-acoustid-open"))
                    .clicked()
                {
                    acts.push(Act::Msg(TaggerMessage::OpenAcoustIdPage(
                        hit.acoustid.clone(),
                    )));
                }
                if let Some(recording) = only.known_recording_id()
                    && pill_button_enabled(
                        ui,
                        &aulos_core::fl!("tagger-ui-recording-link"),
                        Some(regular::ARROW_SQUARE_OUT),
                        PillKind::Ghost,
                        26.0,
                        0.0,
                        true,
                    )
                    .on_hover_text(aulos_core::fl!("tagger-recording-open"))
                    .clicked()
                {
                    acts.push(Act::Msg(TaggerMessage::OpenRecordingPage(recording)));
                }
            });
        }
    });
}
