// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The desktop window loop.
//!
//! The [`App`] lives here, outside the windows: eframe's `run_native` returns
//! when a window closes and can be called again, so "close to the tray"
//! really closes the window (the one way a Wayland client can disappear, and
//! what an X11 one does too) and opens a new one on the tray's "Show", a
//! left click or MPRIS `Raise`. While there is no window, a headless loop
//! keeps the session going (queue, scrobbling, MPRIS, tray commands) by
//! running the part of a frame that needs no window ([`App::pump_hidden`]).
//!
//! An open window does not guarantee frames either: A Wayland compositor stops sending
//! frame callbacks for an invisible surface (minimized, fully covered, another workspace)
//! and winit defers every requested repaint to the next callback, so the frame-driven pump
//! can stall while the window is open: queued events (media keys, tray commands, playback ticks)
//! then wait until the window is visible again. The [`Watchdog`] runs the same pump ([`App::pump_events`])
//! on a timer whenever no frame comes.

use crate::app::{App, Hidden};
use crate::smoke;
use aulos_core::config::Config;
use aulos_core::task::Spawner;
use eframe::App as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Duration;

/// How long the headless loop sleeps when no event wakes it.
const HIDDEN_POLL: Duration = Duration::from_millis(100);

/// How often the watchdog looks at the event queues while no frame comes.
const WATCHDOG_POLL: Duration = Duration::from_millis(100);

/// A window that ran no frame for this long counts as stalled and the
/// watchdog pumps in the UI's place (an idle visible window: a cheap no-op).
const FRAME_STALL: Duration = Duration::from_millis(250);

/// The app, shared between the window eframe runs, the headless loop and
/// the watchdog.
type Shared = Arc<Mutex<App>>;

/// Locks the app, ignoring poisoning (plain data cannot go inconsistent).
fn lock(app: &Mutex<App>) -> MutexGuard<'_, App> {
    app.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// [`lock`] without waiting: `None` while a frame or headless pump holds it.
fn try_lock(app: &Mutex<App>) -> Option<MutexGuard<'_, App>> {
    match app.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// What eframe runs: the shared app, for one window's lifetime. The locks are short (one call each)
/// and never nest, so the watchdog's `try_lock` can always slip in between frames.
struct Window(Shared);

impl eframe::App for Window {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        lock(&self.0).raw_input_hook(ctx, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        lock(&self.0).logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        lock(&self.0).ui(ui, frame);
    }

    fn on_exit(&mut self) {
        lock(&self.0).on_exit();
    }
}

/// Runs [`App::pump_events`] on a timer while an open window produces no
/// frames, so media keys, tray and track advancement never depend on redraws.
struct Watchdog {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn start(app: Shared) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let spawned = std::thread::Builder::new()
            .name("aulos-watchdog".into())
            .spawn(move || {
                // Effects spawned while pumping may use bare `tokio::spawn`, which needs the runtime
                // entered on the calling thread; the UI thread does the same.
                let runtime = lock(&app).handle.runtime().clone();
                let _entered = runtime.enter();
                while !stopping.load(Ordering::Relaxed) {
                    std::thread::sleep(WATCHDOG_POLL);
                    // A frame or headless pump holding the lock already drains the queues.
                    let Some(mut app) = try_lock(&app) else {
                        continue;
                    };
                    // A window closed to the tray is the headless loop's.
                    if !app.is_hidden() && app.frames_stalled(FRAME_STALL) {
                        app.pump_events();
                    }
                }
            });
        match spawned {
            Ok(thread) => Self {
                stop,
                thread: Some(thread),
            },
            Err(error) => {
                // Not fatal: with frames flowing the UI pumps as before.
                tracing::warn!("event watchdog unavailable: {error}");
                Self { stop, thread: None }
            }
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Opens the window on the calling thread (with `runtime` entered) and runs
/// until the app quits.
pub fn run(runtime: &tokio::runtime::Runtime, mut open_paths: Vec<PathBuf>) -> eframe::Result {
    let _guard = runtime.enter();
    let handle = Spawner::new(runtime.handle().clone());
    let mut app: Option<Shared> = None;
    // Declared after `app`: the watchdog stops before the app is released.
    let mut watchdog: Option<Watchdog> = None;
    // Vulkan alone keeps Mesa's GL stack (libgallium, LLVM; ~50 MB) from
    // loading; fall back to every backend when it has no adapter.
    let mut backends = (cfg!(target_os = "linux") && std::env::var_os("WGPU_BACKEND").is_none())
        .then_some(eframe::wgpu::Backends::VULKAN);
    loop {
        match open_window(&handle, &mut open_paths, &mut app, &mut watchdog, backends) {
            Err(eframe::Error::Wgpu(err)) if backends.is_some() => {
                tracing::warn!("Vulkan unavailable ({err}); retrying with all backends");
                backends = None;
                continue;
            }
            Err(error) => return Err(error),
            Ok(()) => {}
        }
        let Some(shared) = &app else {
            return Ok(());
        };
        if !lock(shared).is_hidden() {
            return Ok(());
        }
        if !run_hidden(shared) {
            // The window's own exit path (`on_exit`) did not run for this one.
            lock(shared).on_exit();
            return Ok(());
        }
    }
}

/// Runs one window until it closes. The first one creates the app; later
/// ones (opened from the tray) bind the running app to the new window.
fn open_window(
    handle: &Spawner,
    open_paths: &mut Vec<PathBuf>,
    app: &mut Option<Shared>,
    watchdog: &mut Option<Watchdog>,
    backends: Option<eframe::wgpu::Backends>,
) -> eframe::Result {
    // The last window size and place come from the settings (not eframe's own
    // persistence): one settings file, one save path. A smoke run takes its
    // size from the environment and leaves the saved geometry alone.
    let smoke_size = smoke::window_size();
    let saved = match (&smoke_size, &*app) {
        (Some(_), _) => None,
        (None, Some(app)) => lock(app).session.config.config.window.clone(),
        (None, None) => Config::load().window,
    }
    .and_then(|window| window.clamped(crate::MIN_WINDOW_SIZE));
    let size = smoke_size
        .or_else(|| saved.as_ref().map(|window| window.size))
        .unwrap_or([1280.0, 800.0]);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(size)
        .with_min_inner_size(crate::MIN_WINDOW_SIZE)
        // eframe shrinks the requested size to the largest monitor in
        // logical points, but a Wayland output reports its scale rounded
        // up (2 for 1.25 or 1.5), so the "monitor" it measures is far
        // smaller than the screen and a saved size above it was cut down
        // on every launch. A saved size came from a real window; only
        // the default size needs the clamp.
        .with_clamp_size_to_monitor_size(saved.is_none());
    if let Some(window) = &saved {
        // Wayland ignores the position; a compositor places the window.
        if let Some(position) = window.position {
            viewport = viewport.with_position(position);
        }
        viewport = viewport.with_maximized(window.maximized);
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_title("Aulos")
            .with_app_id("io.github.m0rf30.Aulos")
            // X11 and desktops that don't resolve the icon from the
            // desktop file via the app id need it on the window itself.
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png"))
                    .unwrap_or_default(),
            ),
        // wgpu's default reserves 128-256 MB blocks; ~10 MB is ever used.
        wgpu_options: crate::low_memory_wgpu(backends),
        ..Default::default()
    };
    eframe::run_native(
        "Aulos",
        options,
        Box::new(|cc| {
            let shared = match app {
                Some(shared) => {
                    lock(shared).attach(cc);
                    Arc::clone(shared)
                }
                None => {
                    let mut created = App::new(cc, handle.clone());
                    created.open_files(std::mem::take(open_paths));
                    let shared = Arc::new(Mutex::new(created));
                    // Must start now: the stall it guards against begins with this window.
                    *watchdog = Some(Watchdog::start(Arc::clone(&shared)));
                    *app = Some(Arc::clone(&shared));
                    shared
                }
            };
            Ok(Box::new(Window(shared)))
        }),
    )
}

/// The headless loop of an app whose window is closed to the tray. Returns
/// `true` when the window should open again, `false` when the app quits.
fn run_hidden(app: &Shared) -> bool {
    let wake = {
        let mut app = lock(app);
        app.detach();
        app.wake.clone()
    };
    loop {
        match lock(app).pump_hidden() {
            Hidden::Show => return true,
            Hidden::Quit => return false,
            Hidden::Wait => wake.wait(HIDDEN_POLL),
        }
    }
}
