//! Minimal egui player on top of `aulos-core`: owns one [`Session`], runs the
//! `Effect`s it returns on a tokio runtime and feeds their results back into
//! the matching `Session::apply_*` method from `App::logic`.

mod ui;

use aulos_core::config::Config;
use aulos_core::library::search::is_latest_generation;
use aulos_core::session::{
    ConfigEvent, CoverReset, CoverSet, HomeEvent, LibraryApplied, LibraryEvent, PlaybackEvent,
    PlaybackNotice, PlaybackUpdate, PlaylistEvent, PlaylistOutcome, Reload, Session,
};
use aulos_core::task::{Effect, interval_stream};
use eframe::egui;
use futures::{Stream, StreamExt};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// Debounce between the last keystroke in the search box and the filter run.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// Playback tick period (same as the COSMIC UI).
const TICK: Duration = Duration::from_millis(500);

/// Everything that can come back from background work.
enum Msg {
    Tick,
    Library(LibraryEvent),
    Playback(PlaybackEvent),
    Playlist(PlaylistEvent),
    Config(ConfigEvent),
    Home(HomeEvent),
    FilesChanged(Vec<PathBuf>),
    SearchDebounced(u64),
}

/// Spawns effects/streams on the tokio runtime and wakes egui for every
/// result.
#[derive(Clone)]
struct Runner {
    handle: tokio::runtime::Handle,
    tx: mpsc::Sender<Msg>,
    ctx: egui::Context,
}

impl Runner {
    fn stream<T: Send + 'static>(
        &self,
        mut stream: impl Stream<Item = T> + Send + Unpin + 'static,
        wrap: impl Fn(T) -> Msg + Send + 'static,
    ) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.handle.spawn(async move {
            while let Some(item) = stream.next().await {
                if tx.send(wrap(item)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
    }

    fn run<T: Send + 'static>(&self, effect: Effect<T>, wrap: impl Fn(T) -> Msg + Send + 'static) {
        if let Some(stream) = effect.into_stream() {
            self.stream(stream, wrap);
        }
    }
}

/// An album/track cover thumbnail as an egui texture, with the content
/// fingerprint it was made from (so a reload keeps unchanged textures).
struct Cover {
    fingerprint: u64,
    texture: egui::TextureHandle,
}

pub struct App {
    session: Session,
    runner: Runner,
    rx: mpsc::Receiver<Msg>,
    tab: ui::Tab,
    /// Search box text (the session keeps the query the filter ran with).
    search: String,
    /// Seek slider position (0..=1) while the user drags it.
    seek_preview: Option<f32>,
    volume_dirty: bool,
    status: String,
    covers: HashMap<String, Cover>,
    /// Covers of a refresh in progress, swapped in on `Completed`.
    cover_staging: Option<HashMap<String, Cover>>,
    smoke: Option<Smoke>,
}

/// Headless-ish smoke test, enabled by `AULOS_EGUI_SMOKE=<out.ppm>`: once the
/// library is loaded, plays the first album for a second, saves a screenshot
/// of the window (egui's own capture, so nothing else on screen is read) and
/// exits.
struct Smoke {
    out: PathBuf,
    started: Option<std::time::Instant>,
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
                    app.session.library.search_query = query;
                    app.refresh_search();
                }
                let update = app.session.play_album(0, 0);
                app.apply_playback(update);
                self.started = Some(std::time::Instant::now());
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
        app.apply_playback(update);
        tracing::info!("smoke: screenshot saved to {}", self.out.display());
        true
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, handle: tokio::runtime::Handle) -> Self {
        let (tx, rx) = mpsc::channel();
        let runner = Runner {
            handle,
            tx,
            ctx: cc.egui_ctx.clone(),
        };

        aulos_core::migrate::migrate_legacy_dirs();
        let (context, config) = Config::load();
        let (session, init_errors) = Session::new(context, config);
        let mut status = String::new();
        for (name, reason) in init_errors {
            status = format!("provider {name} failed: {reason}");
        }

        let mut app = Self {
            session,
            runner,
            rx,
            tab: ui::Tab::Albums,
            search: String::new(),
            seek_preview: None,
            volume_dirty: false,
            status,
            covers: HashMap::new(),
            cover_staging: None,
            smoke: std::env::var_os("AULOS_EGUI_SMOKE").map(|out| Smoke {
                out: out.into(),
                started: None,
                requested: false,
            }),
        };

        // Initial effects: library scan, playlists, keyring checks, ticker
        // and (for the local provider) the filesystem watcher.
        let reload = app.session.scan_library();
        app.run_reload(reload);
        app.runner.run(app.session.load_playlists(), Msg::Playlist);
        app.runner
            .run(app.session.config.startup_checks(), Msg::Config);
        app.runner
            .stream(Box::pin(interval_stream(TICK)), |()| Msg::Tick);
        let dirs = app.session.config.config.music_dirs.clone();
        app.runner.stream(
            Box::pin(aulos_core::library::watcher::watch_stream(&dirs)),
            Msg::FilesChanged,
        );
        app
    }

    // -- Event plumbing ---------------------------------------------------

    fn drain_events(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.dispatch(msg);
        }
    }

    fn dispatch(&mut self, msg: Msg) {
        match msg {
            Msg::Tick => {
                let update = self.session.playback_tick(self.seek_preview.is_some());
                self.apply_playback(update);
                self.runner
                    .run(self.session.track_play_history(), Msg::Home);
            }
            Msg::Playback(PlaybackEvent::MpdCommand(Ok(()))) => self.dispatch(Msg::Tick),
            Msg::Playback(event) => {
                let update = self.session.apply_playback_event(event);
                self.apply_playback(update);
            }
            Msg::Home(event) => {
                let follow_up = self.session.apply_home_event(event);
                self.runner.run(follow_up, Msg::Home);
            }
            Msg::Config(event) => self.session.config.apply_event(event),
            Msg::Playlist(event) => {
                let (follow_up, outcome) = self.session.apply_playlist_event(event);
                self.runner.run(follow_up, Msg::Playlist);
                match outcome {
                    PlaylistOutcome::PlaylistsChanged => self.refresh_search(),
                    PlaylistOutcome::Failed(reason) | PlaylistOutcome::ImportFailed(reason) => {
                        self.status = format!("playlist action failed: {reason}");
                    }
                    PlaylistOutcome::Play(tracks) => {
                        let update = self.session.play_tracks(tracks, 0);
                        self.apply_playback(update);
                    }
                    PlaylistOutcome::Imported(files) => {
                        self.status = format!("imported {} playlist file(s)", files.len());
                    }
                    PlaylistOutcome::None | PlaylistOutcome::Created | PlaylistOutcome::Deleted => {
                    }
                }
            }
            Msg::FilesChanged(paths) => {
                self.runner
                    .run(self.session.files_changed(paths), Msg::Library);
            }
            Msg::SearchDebounced(pending) => {
                if is_latest_generation(pending, self.session.library.search_debounce_gen) {
                    self.refresh_search();
                }
            }
            Msg::Library(event) => self.apply_library(event),
        }
    }

    /// Runs a playback workflow's effect and the UI half of a track change.
    fn apply_playback(&mut self, update: PlaybackUpdate) {
        let PlaybackUpdate {
            track_changed,
            notice,
            effect,
        } = update;
        self.runner.run(effect, Msg::Playback);
        if track_changed {
            let focused = self
                .runner
                .ctx
                .input(|i| i.viewport().focused.unwrap_or(true));
            let notify = self.session.notify_track_changed(focused, |_| None);
            self.runner.run(notify, Msg::Playback);
        }
        match notice {
            Some(PlaybackNotice::Queued(n)) => self.status = format!("queued {n} track(s)"),
            Some(PlaybackNotice::OpenFilesFailed) => self.status = "could not open files".into(),
            None => {}
        }
    }

    fn refresh_search(&mut self) {
        let hide_compilations = !self.session.config.config.show_compilations_in_artists;
        self.session
            .library
            .refresh_search(&self.session.playlists.playlists, hide_compilations);
    }

    /// Starts `reload`: applies its cover reset, then runs its effect.
    fn run_reload(&mut self, reload: Reload) {
        match reload.covers {
            CoverReset::Keep => {}
            CoverReset::Stage => self.cover_staging = Some(HashMap::new()),
            CoverReset::Clear => {
                self.cover_staging = None;
                self.covers.clear();
            }
        }
        self.runner.run(reload.effect, Msg::Library);
    }

    /// Textures for `covers`, reusing the ones whose fingerprint is unchanged.
    fn cover_textures(&self, covers: CoverSet) -> HashMap<String, Cover> {
        use aulos_core::session::CoverImage;
        covers
            .into_iter()
            .filter_map(|(key, thumb)| {
                if let Some(existing) = self.covers.get(&key)
                    && existing.fingerprint == thumb.fingerprint
                {
                    return Some((
                        key,
                        Cover {
                            fingerprint: existing.fingerprint,
                            texture: existing.texture.clone(),
                        },
                    ));
                }
                // The encoded fallback would need an image decoder here.
                let CoverImage::Pixels {
                    width,
                    height,
                    rgba,
                } = thumb.image
                else {
                    return None;
                };
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
                    &rgba,
                );
                let texture =
                    self.runner
                        .ctx
                        .load_texture(&key, image, egui::TextureOptions::LINEAR);
                Some((
                    key,
                    Cover {
                        fingerprint: thumb.fingerprint,
                        texture,
                    },
                ))
            })
            .collect()
    }

    fn apply_library(&mut self, event: LibraryEvent) {
        match self.session.apply_library_event(event) {
            LibraryApplied::Stale | LibraryApplied::Done | LibraryApplied::GenresLoaded => {}
            LibraryApplied::ScanComplete { reload } => self.run_reload(reload),
            LibraryApplied::Loaded { covers } => {
                self.covers = self.cover_textures(covers);
                self.refresh_search();
                self.log_library("loaded");
            }
            LibraryApplied::Batch {
                covers,
                staged,
                needs_refilter,
            } => {
                let textures = self.cover_textures(covers);
                if staged {
                    if let Some(staging) = &mut self.cover_staging {
                        staging.extend(textures);
                    }
                    return;
                }
                self.covers.extend(textures);
                if needs_refilter {
                    self.refresh_search();
                }
            }
            LibraryApplied::Completed { swapped } => {
                let staged = self.cover_staging.take();
                if swapped && let Some(staged) = staged {
                    self.covers = staged;
                }
                self.refresh_search();
                self.log_library("reload completed");
            }
            LibraryApplied::GenreTracks { .. } => {}
            LibraryApplied::FavoriteChanged { track_id, state } => {
                // Scrobbling services want to know about loves.
                let library = &self.session.library;
                let track = library
                    .tracks
                    .iter()
                    .find(|t| t.id.to_string() == track_id)
                    .cloned();
                if let Some(track) = track {
                    self.session
                        .playback
                        .scrobble
                        .on_favorite_changed(&track_id, &track, state);
                }
            }
            LibraryApplied::ActionFailed(reason) => {
                self.status = format!("provider action failed: {reason}");
            }
        }
    }

    fn log_library(&self, what: &str) {
        let lib = &self.session.library;
        tracing::info!(
            "library {what}: {} tracks, {} albums, {} artists",
            lib.tracks.len(),
            lib.albums.len(),
            lib.artists.len()
        );
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
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

    // `Session::new` builds providers that need a runtime handle; the guard
    // keeps one entered on this (the UI) thread.
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
