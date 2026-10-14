// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Playlist glue: the M3U/M3U8/PLS import/export portal file dialogs, the
//! and the toast text of an import (the session's playlist workflows,
//! `aulos_core::session::playlists`, report through session updates).

use super::tasks::session_effect;
use super::{AppModel, Message};
use aulos_core::fl;
use aulos_core::library::m3u;
use aulos_core::library::playlist_io::{self, ExportOutcome, ExportSource, ImportSummary};
use aulos_core::session::PlaylistEvent;
use cosmic::prelude::*;
use cosmic::widget;
use std::path::PathBuf;

/// Messages of the playlist import/export feature.
#[derive(Debug, Clone)]
pub enum PlaylistIo {
    /// Export the playlist at this index of `Session::playlists`.
    Export(usize),
    /// Export the smart playlist at this index of `Session::smart_playlists`.
    ExportSmart(usize),
    /// Export finished (`Ok(None)` = the dialog was cancelled).
    Exported(Result<Option<ExportOutcome>, String>),
    /// Show the open-file dialog to pick playlist files to import.
    PickFiles,
    /// Import these playlist files (from the dialog, the CLI or `OpenUri`).
    Import(Vec<PathBuf>),
}

impl AppModel {
    pub(super) fn update_playlist_io(&mut self, msg: PlaylistIo) -> Task<cosmic::Action<Message>> {
        match msg {
            PlaylistIo::Export(index) => {
                let Some((name, source)) = self.session.playlists.export_tracks(index) else {
                    return Task::none();
                };
                if self.session.playlists.playlist_tracks(index).is_none() {
                    return self.push_toast(widget::toaster::Toast::new(fl!(
                        "toast-playlist-export-empty"
                    )));
                }
                export_task(name, source, self.session.config.config.m3u_relative_paths)
            }
            PlaylistIo::ExportSmart(index) => {
                let Some((name, source)) = self.session.playlists.export_smart(index) else {
                    return Task::none();
                };
                export_task(name, source, self.session.config.config.m3u_relative_paths)
            }
            PlaylistIo::Exported(Ok(Some(outcome))) => {
                let file = outcome
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let text = if outcome.skipped > 0 {
                    fl!(
                        "toast-playlist-exported-partial",
                        count = outcome.written,
                        skipped = outcome.skipped,
                        file = file
                    )
                } else {
                    fl!(
                        "toast-playlist-exported",
                        count = outcome.written,
                        file = file
                    )
                };
                self.push_toast(widget::toaster::Toast::new(text))
            }
            PlaylistIo::Exported(Ok(None)) => Task::none(),
            PlaylistIo::Exported(Err(reason)) => self.push_toast(widget::toaster::Toast::new(fl!(
                "toast-playlist-export-failed",
                reason = reason
            ))),
            PlaylistIo::PickFiles => cosmic::task::future(async {
                let paths = pick_playlist_files().await;
                cosmic::Action::App(match paths {
                    Ok(paths) => Message::PlaylistIo(PlaylistIo::Import(paths)),
                    Err(e) => Message::Session(PlaylistEvent::Imported(Err(e)).into()),
                })
            }),
            PlaylistIo::Import(paths) => self.import_playlist_files(paths),
        }
    }

    /// Imports playlist files into new library playlists (background task).
    /// Also the entry point for playlist files given on the command line or
    /// via MPRIS `OpenUri` (see `open_files`).
    pub(super) fn import_playlist_files(
        &mut self,
        paths: Vec<PathBuf>,
    ) -> Task<cosmic::Action<Message>> {
        session_effect(aulos_core::session::import_playlist_files(paths))
    }

    /// Reload the active provider's playlists.
    pub(super) fn load_playlists(&self) -> Task<cosmic::Action<Message>> {
        session_effect(self.session.load_playlists())
    }
}

fn export_task(
    name: String,
    source: ExportSource,
    relative: bool,
) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let result = export_flow(&name, source, relative).await;
        cosmic::Action::App(Message::PlaylistIo(PlaylistIo::Exported(result)))
    })
}

async fn export_flow(
    name: &str,
    source: ExportSource,
    relative: bool,
) -> Result<Option<ExportOutcome>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let title = fl!("export-playlist-dialog-title");
    let default_name = format!("{}.m3u8", playlist_io::sanitize_file_name(name));
    let filter = FileFilter::new("M3U playlist").glob("*.m3u8").glob("*.m3u");

    let request = SelectedFiles::save_file()
        .title(title.as_str())
        .current_name(default_name.as_str())
        .modal(true)
        .filter(filter)
        .send()
        .await
        .map_err(|e| format!("Portal request failed: {e}"))?;
    let selected = match request.response() {
        Ok(selected) => selected,
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => return Ok(None),
        Err(e) => return Err(format!("Portal response failed: {e}")),
    };
    let Some(mut path) = selected
        .uris()
        .first()
        .and_then(|uri| aulos_core::file_uri_to_path(uri.as_str()))
    else {
        return Ok(None);
    };
    if path.extension().is_none() {
        path.set_extension("m3u8");
    }

    aulos_core::session::export_playlist(source, path, relative)
        .await
        .map(Some)
}

async fn pick_playlist_files() -> Result<Vec<PathBuf>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};

    let title = fl!("import-playlist-dialog-title");
    let mut filter = FileFilter::new("Playlists (M3U, M3U8, PLS)");
    for ext in m3u::PLAYLIST_EXTENSIONS {
        filter = filter.glob(&format!("*.{ext}"));
    }
    let request = SelectedFiles::open_file()
        .title(title.as_str())
        .multiple(true)
        .modal(true)
        .filter(filter)
        .send()
        .await
        .map_err(|e| format!("Portal request failed: {e}"))?;
    match request.response() {
        Ok(selected) => Ok(selected
            .uris()
            .iter()
            .filter_map(|uri| aulos_core::file_uri_to_path(uri.as_str()))
            .collect()),
        Err(ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)) => Ok(Vec::new()),
        Err(e) => Err(format!("Portal response failed: {e}")),
    }
}

pub(super) fn import_toast_text(summaries: &[ImportSummary]) -> String {
    let matched: usize = summaries.iter().map(|s| s.matched).sum();
    let unmatched: usize = summaries.iter().map(|s| s.unmatched).sum();
    match summaries {
        [one] if unmatched == 0 => fl!(
            "toast-playlist-imported",
            name = one.name.clone(),
            count = matched
        ),
        [one] => fl!(
            "toast-playlist-imported-partial",
            name = one.name.clone(),
            count = matched,
            missing = unmatched
        ),
        many if unmatched == 0 => fl!(
            "toast-playlists-imported",
            playlists = many.len(),
            count = matched
        ),
        many => fl!(
            "toast-playlists-imported-partial",
            playlists = many.len(),
            count = matched,
            missing = unmatched
        ),
    }
}
