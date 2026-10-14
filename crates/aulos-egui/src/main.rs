//! Minimal egui player on top of `aulos-core`. All behaviour lives in the
//! core [`Session`]; this file only plumbs it: runs the effects and
//! background streams it hands out on a tokio runtime, feeds every
//! resulting [`SessionEvent`] back into [`Session::apply_session_event`]
//! from `App::logic`, and presents the [`SessionUpdate`] it answers with.

mod ui;

use aulos_core::session::{BackgroundKey, Session, SessionEvent, SessionNotice, SessionUpdate};
use aulos_core::task::Effect;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub struct App {
    session: Session,
    handle: tokio::runtime::Handle,
    ctx: egui::Context,
    tx: mpsc::Sender<SessionEvent>,
    rx: mpsc::Receiver<SessionEvent>,
    /// Running background streams, by the session's own identity for them.
    streams: HashMap<BackgroundKey, tokio::task::JoinHandle<()>>,
    /// The session's state changed: re-read [`Session::background_streams`].
    streams_dirty: bool,
    /// Album cover textures, by cover key; rebuilt from the session's
    /// thumbnails for the keys a [`SessionUpdate`] reports as changed.
    textures: HashMap<String, egui::TextureHandle>,
    tab: ui::Tab,
    /// Search box text.
    search: String,
    /// Seek slider position (0..=1) while the user drags it.
    seek_preview: Option<f32>,
    volume_dirty: bool,
    status: String,
    smoke: Option<Smoke>,
}

/// Headless-ish smoke test, enabled by `AULOS_EGUI_SMOKE=<out.ppm>`: once the
/// library is loaded, plays the first album for a second, saves a screenshot
/// of the window (egui's own capture, so nothing else on screen is read) and
/// exits. `AULOS_EGUI_SMOKE_TAB=songs` and `AULOS_EGUI_SMOKE_SEARCH=<text>`
/// pick the tab and a search to show.
struct Smoke {
    out: PathBuf,
    started: Option<Instant>,
    requested: bool,
}

impl Smoke {
    fn step(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let Some(started) = self.started else {
            if !app.session.library.albums.is_empty() && !app.session.library.scanning {
                if std::env::var("AULOS_EGUI_SMOKE_TAB").is_ok_and(|t| t == "songs") {
                    app.tab = ui::Tab::Songs;
                }
                if let Ok(query) = std::env::var("AULOS_EGUI_SMOKE_SEARCH") {
                    app.search = query.clone();
                    let update = app.session.set_search_query(query);
                    app.run(update);
                }
                let update = app.session.play_album(0, 0);
                app.run(update);
                self.started = Some(Instant::now());
            }
            return false;
        };
        if !self.requested && started.elapsed() > Duration::from_millis(800) {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = shot else { return false };
        let [w, h] = image.size;
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        ppm.extend(image.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]));
        if let Err(e) = std::fs::write(&self.out, ppm) {
            tracing::error!("smoke: cannot write screenshot: {e}");
        }
        let update = app.session.stop();
        app.run(update);
        tracing::info!("smoke: screenshot saved to {}", self.out.display());
        true
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, handle: tokio::runtime::Handle) -> Self {
        let (tx, rx) = mpsc::channel();
        let (session, init_errors) = Session::bootstrap();
        let mut app = Self {
            session,
            handle,
            ctx: cc.egui_ctx.clone(),
            tx,
            rx,
            streams: HashMap::new(),
            streams_dirty: true,
            textures: HashMap::new(),
            tab: ui::Tab::Albums,
            search: String::new(),
            seek_preview: None,
            volume_dirty: false,
            status: String::new(),
            smoke: std::env::var_os("AULOS_EGUI_SMOKE").map(|out| Smoke {
                out: out.into(),
                started: None,
                requested: false,
            }),
        };
        if let Some((name, reason)) = init_errors.last() {
            app.status = format!("provider {name} failed: {reason}");
        }
        let start = app.session.start();
        app.run(start);
        app
    }

    /// A sink that queues an event for `logic()` and wakes the UI.
    fn sink(&self) -> impl FnMut(SessionEvent) + Send + 'static {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        move |event| {
            if tx.send(event).is_ok() {
                ctx.request_repaint();
            }
        }
    }

    /// Handles what the session answered with: runs the effect, shows the
    /// notices and refreshes the cover textures that changed. Every action
    /// and every event goes through here.
    fn run(&mut self, update: impl Into<SessionUpdate>) {
        let SessionUpdate {
            effect,
            notices,
            changes,
        } = update.into();
        effect.spawn(&self.handle, self.sink());
        if let Some(notice) = notices.last() {
            self.status = notice_text(notice);
        }
        let covers = changes.covers;
        if covers.cleared {
            self.textures.clear();
        }
        for key in &covers.removed {
            self.textures.remove(key);
        }
        for key in covers.changed {
            if let Some(thumb) = self.session.library.covers.get(&key) {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [thumb.width as usize, thumb.height as usize],
                    &thumb.rgba,
                );
                let texture = self
                    .ctx
                    .load_texture(&key, image, egui::TextureOptions::LINEAR);
                self.textures.insert(key, texture);
            }
        }
        self.streams_dirty = true;
    }

    /// Starts the background streams whose key appeared and stops those
    /// whose key went away.
    fn sync_streams(&mut self) {
        let wanted = self.session.background_streams();
        let keys: HashSet<&BackgroundKey> = wanted.iter().map(|(k, _)| k).collect();
        self.streams.retain(|key, task| {
            let keep = keys.contains(key);
            if !keep {
                task.abort();
            }
            keep
        });
        for (key, stream) in wanted {
            if !self.streams.contains_key(&key)
                && let Some(task) = Effect::stream(stream).spawn(&self.handle, self.sink())
            {
                self.streams.insert(key, task);
            }
        }
    }
}

/// The status-line wording of a notice.
fn notice_text(notice: &SessionNotice) -> String {
    use aulos_core::session::PlaybackNotice;
    match notice {
        SessionNotice::Playback(PlaybackNotice::Queued(n)) => format!("queued {n} track(s)"),
        SessionNotice::Playback(PlaybackNotice::OpenFilesFailed) => "could not open files".into(),
        SessionNotice::ProviderActionFailed(reason) => format!("action failed: {reason}"),
        SessionNotice::ProviderConnectFailed { provider, reason } => {
            format!("provider {provider} failed: {reason}")
        }
        SessionNotice::PlaylistsImported(files) => {
            format!("imported {} playlist file(s)", files.len())
        }
        SessionNotice::PlaylistImportFailed(reason) => format!("playlist import failed: {reason}"),
        other => format!("{other:?}"),
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        self.session.set_window_focused(focused);
        self.session.set_seeking(self.seek_preview.is_some());
        while let Ok(event) = self.rx.try_recv() {
            let update = self.session.apply_session_event(event);
            self.run(update);
        }
        if std::mem::take(&mut self.streams_dirty) {
            self.sync_streams();
        }
        if let Some(mut smoke) = self.smoke.take() {
            if smoke.step(self, ctx) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.smoke = Some(smoke);
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self) {
        self.session.config.flush();
    }
}

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    aulos_core::i18n::init_from_system();

    // `Session::bootstrap` builds providers that need a runtime handle; the
    // guard keeps one entered on this (the UI) thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _guard = runtime.enter();
    let handle = runtime.handle().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 650.0])
            .with_app_id("io.github.m0rf30.Aulos.egui"),
        ..Default::default()
    };
    eframe::run_native(
        "Aulos (egui)",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc, handle)))),
    )
}
