// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::{AppFlags, AppModel, Message};
use cosmic::app::context_drawer;
use cosmic::iced::Subscription;
use cosmic::prelude::*;
use cosmic::widget::nav_bar;

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = AppFlags;
    type Message = Message;
    const APP_ID: &'static str = "io.github.m0rf30.Aulos";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, flags: Self::Flags) -> (Self, Task<cosmic::Action<Self::Message>>) {
        Self::init_model(core, flags)
    }

    fn header_start(&self) -> Vec<Element<'_, Self::Message>> {
        self.header_start_elements()
    }

    fn header_center(&self) -> Vec<Element<'_, Self::Message>> {
        self.header_center_elements()
    }

    fn header_end(&self) -> Vec<Element<'_, Self::Message>> {
        self.header_end_elements()
    }

    fn nav_model(&self) -> Option<&nav_bar::Model> {
        if self.extras.mini_player {
            return None;
        }
        Some(&self.nav)
    }

    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Self::Message>> {
        self.context_drawer_page()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let start = std::time::Instant::now();
        let element = self.view_page();
        log_elapsed("view", "view", start.elapsed());
        element
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        self.build_subscription()
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        let label = message_label(&message);
        let start = std::time::Instant::now();
        let before = self.view_signature();
        let task = self.handle_message(message);
        // A page/detail/layout change remounts the virtual lists at the
        // top; put them back where the user left them.
        let task = if self.view_signature() != before {
            Task::batch([task, self.restore_scroll_positions()])
        } else {
            task
        };
        // Detail pages can be entered from many messages (selection,
        // links, back history, library reloads); checking here once keeps
        // their hero artwork in sync without touching each handler.
        let task = Task::batch([task, self.maybe_update_detail_art()]);
        log_elapsed("update", label, start.elapsed());
        task
    }

    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<cosmic::Action<Self::Message>> {
        let start = std::time::Instant::now();
        let task = self.select_nav(id);
        let task = Task::batch([
            task,
            self.restore_scroll_positions(),
            self.maybe_update_detail_art(),
        ]);
        log_elapsed("update", "NavSelect", start.elapsed());
        task
    }

    /// Closing the window (header close button) goes through
    /// `PlaybackExtrasMessage::CloseRequested`: quit, or keep playing in the
    /// background when that is enabled.
    fn on_app_exit(&mut self) -> Option<Self::Message> {
        Some(Message::Playback(
            super::playback_extras::PlaybackExtrasMessage::CloseRequested,
        ))
    }
}

/// `update()`/`view()` timing threshold (ms) above which the log escalates
/// from `debug` to `warn` -- a rough "this frame likely dropped below a
/// 60fps budget" marker, not a hard SLA.
const SLOW_THRESHOLD_MS: f64 = 30.0;

/// Logs how long a `view()`/`update()` call took. `debug` below
/// `SLOW_THRESHOLD_MS`, `warn` at or above it, so slow page/menu switches
/// (the "strangely slow" bug reports) surface in a plain `RUST_LOG=warn`
/// run without needing debug-level logging enabled everywhere.
fn log_elapsed(kind: &str, label: &str, elapsed: std::time::Duration) {
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    if elapsed_ms >= SLOW_THRESHOLD_MS {
        tracing::warn!("{kind}({label}) took {elapsed_ms:.2}ms");
    } else {
        tracing::debug!("{kind}({label}) took {elapsed_ms:.2}ms");
    }
}

/// Cheap textual label for a message, for `update()` timing logs. Only
/// matches variants worth distinguishing (hot/frequent paths, and ones
/// prone to doing real work); everything else collapses to a generic
/// bucket rather than `format!("{:?}", message)` over the whole enum,
/// which would force rendering the full payload (e.g. `LibraryLoaded`'s
/// vectors) on every single `update()` call just to get a label.
fn message_label(message: &Message) -> &'static str {
    match message {
        Message::PlaybackTick => "PlaybackTick",
        Message::TogglePlayback => "TogglePlayback",
        Message::NextTrack => "NextTrack",
        Message::PreviousTrack => "PreviousTrack",
        Message::SeekPreview(_) => "SeekPreview",
        Message::SeekCommit => "SeekCommit",
        Message::SetVolume(_) => "SetVolume",
        Message::VolumeCommit => "VolumeCommit",
        Message::Stop => "Stop",
        Message::ToggleShuffle => "ToggleShuffle",
        Message::CycleRepeat => "CycleRepeat",
        Message::MpdIdleEvent(..) => "MpdIdleEvent",
        Message::MpdStatusUpdate { .. } => "MpdStatusUpdate",
        Message::MpdConnected(_) => "MpdConnected",
        Message::MpdConnectionFailed(..) => "MpdConnectionFailed",
        Message::ScanLibrary => "ScanLibrary",
        Message::LibraryScanComplete { .. } => "LibraryScanComplete",
        Message::LibraryLoaded { .. } => "LibraryLoaded",
        Message::LibraryBatch { .. } => "LibraryBatch",
        Message::LibraryLoadComplete { .. } => "LibraryLoadComplete",
        Message::FilesChanged(_) => "FilesChanged",
        Message::SwitchProvider(_) => "SwitchProvider",
        Message::ToggleContextPage(_) => "ToggleContextPage",
        Message::LibrarySearchChanged(_) => "LibrarySearchChanged",
        Message::BlurReady(..) => "BlurReady",
        Message::OnlineIconLoaded(..) => "OnlineIconLoaded",
        Message::ExpandAnimTick => "ExpandAnimTick",
        Message::ExpandNowPlaying => "ExpandNowPlaying",
        Message::CollapseNowPlaying => "CollapseNowPlaying",
        Message::Mpris(_) => "Mpris",
        Message::Shortcut(_) => "Shortcut",
        Message::QueueJump(_) => "QueueJump",
        Message::QueueRemove(_) => "QueueRemove",
        Message::QueueMove { .. } => "QueueMove",
        Message::QueueClear => "QueueClear",
        _ => "Other",
    }
}
