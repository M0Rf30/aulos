//! The application: session plumbing, navigation, shared resources (cover
//! textures, accent, backdrop, toasts) and the window layout.

use crate::theme::{self, Accent, Palette, Quality, Theme};
use crate::views;
use crate::widgets::Covers;
use aulos_core::config::{Config, ThemeMode};
use aulos_core::library::{
    CoverArt, Track, palette,
    quality::{self, AudioQuality},
};
use aulos_core::mpris::{self, MprisCommand, MprisEvent, NativeWindow};
use aulos_core::session::{
    BackgroundKey, ConvertNotice, PodcastNotice, RadioNotice, Session, SessionEvent, SessionNotice,
    SessionUpdate,
};
use aulos_core::task::{Effect, Spawner, TaskHandle};
use egui::{Color32, Context, TextureHandle, TextureOptions};
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use std::time::Duration;

/// Where the user is. Detail routes carry what they show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    Home,
    Albums,
    /// An album's detail, by `Album::cover_key`.
    Album(String),
    Artists,
    /// An artist's detail, by name.
    Artist(String),
    Songs,
    Genres,
    /// A genre's detail, by name.
    Genre(String),
    Folders,
    Playlists,
    Podcasts,
    Radio,
    Tagger,
    Converter,
    Settings,
}

impl Route {
    /// The top-level page a route belongs to (what the sidebar highlights).
    pub fn root(&self) -> Route {
        match self {
            Route::Album(_) => Route::Albums,
            Route::Artist(_) => Route::Artists,
            Route::Genre(_) => Route::Genres,
            other => other.clone(),
        }
    }

    /// The persisted key of a top-level page (`Config::last_view`).
    pub fn key(&self) -> &'static str {
        match self.root() {
            Route::Home => "home",
            Route::Albums => "albums",
            Route::Artists => "artists",
            Route::Songs => "songs",
            Route::Genres => "genres",
            Route::Folders => "folders",
            Route::Playlists => "playlists",
            Route::Podcasts => "podcasts",
            Route::Radio => "radio",
            Route::Tagger => "tagger",
            Route::Converter => "convert",
            _ => "settings",
        }
    }

    pub fn from_key(key: &str) -> Option<Route> {
        Some(match key.trim() {
            "home" => Route::Home,
            "albums" => Route::Albums,
            "artists" => Route::Artists,
            "songs" => Route::Songs,
            "genres" => Route::Genres,
            "folders" => Route::Folders,
            "playlists" | "smart_playlists" => Route::Playlists,
            "podcasts" => Route::Podcasts,
            "radio" => Route::Radio,
            _ => return None,
        })
    }
}

/// The side sheet that is open, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sheet {
    Queue,
    Properties,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

/// What a toast's action chip does when clicked.
pub enum ToastAction {
    /// Re-save a removed radio station.
    RestoreStation(aulos_core::online::store::RadioStation),
    /// Ask the browser for access to the music folders waiting for it.
    AllowMusicFolders,
}

impl ToastAction {
    pub fn label(&self) -> String {
        match self {
            Self::RestoreStation(_) => aulos_core::fl!("app-toast-undo"),
            Self::AllowMusicFolders => aulos_core::fl!("app-toast-allow"),
        }
    }
}

pub struct Toast {
    /// The text, or its part before `subject` when there is one.
    pub text: String,
    /// A bold name inside the text, followed by `suffix`.
    pub subject: Option<String>,
    pub suffix: String,
    pub icon: Option<&'static str>,
    pub action: Option<ToastAction>,
    pub kind: ToastKind,
    pub born: f64,
    pub life: f64,
}

/// Results of work done off the UI thread that is not a `SessionEvent`.
pub enum AppEvent {
    /// Blurred backdrop, accent and a large cover for `key`'s album.
    Art {
        key: String,
        backdrop: Option<(u32, u32, Vec<u8>)>,
        accent: Option<palette::Accent>,
        large: Option<(u32, u32, Vec<u8>)>,
    },
    /// The OS media session (media keys, Now Playing, MPRIS) came up or
    /// relayed a command.
    Media(MprisEvent),
}

/// Smoke-screenshot driver (see `smoke.rs`).
pub use crate::smoke::Smoke;

pub struct App {
    pub session: Session,
    pub handle: Spawner,
    pub ctx: Context,
    tx: mpsc::Sender<SessionEvent>,
    rx: mpsc::Receiver<SessionEvent>,
    app_tx: mpsc::Sender<AppEvent>,
    app_rx: mpsc::Receiver<AppEvent>,
    streams: HashMap<BackgroundKey, TaskHandle>,
    streams_dirty: bool,

    /// Every texture the views draw.
    pub covers: Covers,
    pub dark: bool,
    pub pal: Palette,
    /// The accent currently painted (animated towards `accent_to`).
    pub accent: Accent,
    accent_from: Accent,
    accent_to: Accent,
    /// Dominant colour of the playing cover (None: nothing playing or no
    /// dominant hue), kept so the accent can be re-derived when the theme
    /// or the "tint with cover" setting changes.
    cover_tint: Option<Color32>,
    accent_start: f64,
    /// Cover key of the current track (None: nothing playing).
    pub now_key: Option<String>,
    art_dirty: bool,
    /// The pixels-per-point the large cover was last decoded for.
    art_ppp: f32,
    /// Smoke runs only: multiplies the zoom so one point is one pixel at 100%.
    zoom_base: Option<f32>,
    art_pending: Option<String>,
    art_done: Option<String>,
    /// Blurred backdrop of the current cover (expanded and mini views).
    pub backdrop: Option<TextureHandle>,
    /// Large cover of the current track (expanded view).
    pub cover_large: Option<TextureHandle>,
    /// The previous art, faded out under the new one in the expanded view.
    pub art_fade: ArtFade,

    pub route: Route,
    history: Vec<Route>,
    history_pos: usize,
    pub search: String,
    pub search_id: egui::Id,
    pub class: theme::WidthClass,
    /// The sidebar was collapsed to the icon rail by hand.
    pub sidebar_collapsed: bool,
    pub sheet: Option<Sheet>,
    pub expanded: bool,
    pub mini: bool,
    pub seek_preview: Option<f32>,
    pub toasts: Vec<Toast>,
    pub views: views::State,
    saved_size: Option<egui::Vec2>,
    pub smoke: Option<Smoke>,
    /// The running visualizer (None: hidden, nothing renders or captures).
    #[cfg(feature = "visualizer")]
    pub viz: Option<crate::viz::Viz>,
    /// Lock, beat sensitivity and last preset, kept while the view is closed.
    #[cfg(feature = "visualizer")]
    pub viz_settings: crate::viz::Settings,
    #[cfg(feature = "visualizer")]
    render_state: Option<eframe::egui_wgpu::RenderState>,
}

/// The interface zoom levels the settings control and Ctrl+= / Ctrl+- walk through.
pub const UI_SCALE_STEPS: [f32; 8] = [0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];
/// The widest the expanded cover is drawn, in points.
const EXPANDED_COVER_POINTS: f32 = 480.0;

/// The art of the previous track, kept for the expanded-view crossfades.
#[derive(Default)]
pub struct ArtFade {
    pub backdrop: Option<TextureHandle>,
    pub cover: Option<TextureHandle>,
    pub started: f64,
}

impl ArtFade {
    /// Progress (0 = old art, 1 = new art) of a crossfade lasting `secs`;
    /// requests repaints while running. Reduced motion snaps to 1.
    pub fn progress(&self, ctx: &Context, secs: f32) -> f32 {
        if theme::motion::reduced(ctx) || secs <= 0.0 {
            return 1.0;
        }
        let t = ((ctx.input(|i| i.time) - self.started) as f32 / secs).clamp(0.0, 1.0);
        if t < 1.0 {
            ctx.request_repaint();
        }
        t
    }
}

/// The native window handle the Windows media controls attach to.
#[cfg(windows)]
fn native_window(cc: &eframe::CreationContext<'_>) -> Option<NativeWindow> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(NativeWindow(handle.hwnd.get() as usize)),
        _ => None,
    }
}

/// Only Windows' media controls need the window.
#[cfg(not(windows))]
fn native_window(_: &eframe::CreationContext<'_>) -> Option<NativeWindow> {
    None
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, handle: Spawner) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        // Zoom keys are handled in `shortcuts` so the choice persists.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        if std::env::var_os("AULOS_SMOKE").is_some() {
            // Smoke shots are compared pixel for pixel with the design: 1 logical px = 1 physical px.
            // Real runs keep the display's native scale factor; `logic` cancels it here.
        }
        let (tx, rx) = mpsc::channel();
        let (app_tx, app_rx) = mpsc::channel();
        let (session, init_errors) = Session::bootstrap();
        // Thumbnails at the physical size of a grid card (256 px at 1x).
        aulos_core::library::covers::CoverThumb::set_max_dim(
            (256.0 * cc.egui_ctx.pixels_per_point())
                .round()
                .clamp(128.0, 640.0) as u32,
        );
        let dark = Self::resolve_dark(session.config.config.theme_mode, cc.egui_ctx.system_theme());
        let pal = if dark { theme::DARK } else { theme::LIGHT };
        let accent = Accent::default_for(&pal);
        #[cfg(feature = "visualizer")]
        let viz_settings = crate::viz::Settings::from_config(&session.config.config);
        let mut app = Self {
            session,
            handle,
            ctx: cc.egui_ctx.clone(),
            tx,
            rx,
            app_tx,
            app_rx,
            streams: HashMap::new(),
            streams_dirty: true,
            covers: Covers::new(cc.egui_ctx.clone()),
            dark,
            pal,
            accent,
            accent_from: accent,
            accent_to: accent,
            cover_tint: None,
            accent_start: 0.0,
            now_key: None,
            art_dirty: true,
            art_ppp: cc.egui_ctx.pixels_per_point(),
            zoom_base: std::env::var_os("AULOS_SMOKE").is_none().then_some(1.0),
            art_pending: None,
            art_done: None,
            backdrop: None,
            cover_large: None,
            art_fade: ArtFade::default(),
            route: Route::Home,
            history: vec![Route::Home],
            history_pos: 0,
            search: String::new(),
            search_id: egui::Id::new("aulos-search"),
            class: theme::WidthClass::Expanded,
            sidebar_collapsed: false,
            sheet: None,
            expanded: false,
            mini: false,
            seek_preview: None,
            toasts: Vec::new(),
            views: views::State::default(),
            saved_size: None,
            smoke: Smoke::from_env(),
            #[cfg(feature = "visualizer")]
            viz: None,
            #[cfg(feature = "visualizer")]
            viz_settings,
            #[cfg(feature = "visualizer")]
            render_state: cc.wgpu_render_state.clone(),
        };
        theme::apply(&app.ctx, &app.pal);
        app.apply_zoom();
        for (name, reason) in &init_errors {
            views::overlays::on_connect_failed(&mut app, name, reason);
        }
        views::settings::notify_folders_needing_access(&mut app);
        app.start_media_session(native_window(cc));
        let start = app.session.start();
        app.run(start);
        let startup = app.startup_route();
        app.navigate(startup);
        app
    }

    /// Starts the OS media session (media keys, Now Playing, MPRIS); its
    /// events come back as [`AppEvent::Media`]. Smoke runs skip it so they
    /// never claim a name on the user's session bus.
    fn start_media_session(&self, window: Option<NativeWindow>) {
        if std::env::var_os("AULOS_SMOKE").is_some() {
            return;
        }
        let (tx, ctx) = (self.app_tx.clone(), self.ctx.clone());
        self.handle.spawn(async move {
            let mut events = std::pin::pin!(mpris::mpris_stream(window));
            while let Some(event) = events.next().await {
                if tx.send(AppEvent::Media(event)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
    }

    /// The route the user chose to open on (Home when unknown).
    fn startup_route(&self) -> Route {
        let cfg = &self.session.config.config;
        let key = match cfg.startup_page.trim() {
            "last" => cfg.last_view.as_str(),
            key => key,
        };
        Route::from_key(key).unwrap_or(Route::Home)
    }

    /// A sink that queues an event for `logic()` and wakes the UI.
    pub fn sink(&self) -> impl FnMut(SessionEvent) + Send + 'static {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        move |event| {
            if tx.send(event).is_ok() {
                ctx.request_repaint();
            }
        }
    }

    /// Handles what the session answered with: runs the effect, shows the
    /// notices and refreshes what the changes touched. Every action and every
    /// event goes through here.
    pub fn run(&mut self, update: impl Into<SessionUpdate>) {
        let SessionUpdate {
            effect,
            notices,
            changes,
        } = update.into();
        effect.spawn(&self.handle, self.sink());
        for notice in &notices {
            self.show_notice(notice);
        }
        let covers = changes.covers;
        if covers.cleared {
            self.covers.clear_thumbs();
        }
        for key in &covers.removed {
            self.covers.remove_thumb(key);
        }
        for key in covers.changed {
            self.covers.queue_thumb(key);
        }
        if changes.artist_photos_cleared {
            self.covers.clear_photos();
        }
        for photo in changes.artist_photos {
            self.covers.add_photo(photo);
        }
        for icon in changes.online_icons {
            self.covers.add_icon(icon);
        }
        if changes.track_changed || changes.cover_bytes_loaded || changes.library_loaded {
            self.art_dirty = true;
        }
        self.streams_dirty = true;
    }

    fn show_notice(&mut self, notice: &SessionNotice) {
        use aulos_core::session::PlaybackNotice;
        match notice {
            SessionNotice::Playback(PlaybackNotice::Queued(n)) => {
                self.toast(aulos_core::fl!("app-toast-queued", count = (*n)));
            }
            SessionNotice::Playback(PlaybackNotice::OpenFilesFailed) => {
                self.toast_error(aulos_core::fl!("app-toast-open-files-failed"))
            }
            SessionNotice::Playback(PlaybackNotice::PlayFailed(reason)) => self.toast_error(
                aulos_core::fl!("app-toast-playback-failed", reason = reason.to_string()),
            ),
            SessionNotice::ProviderActionFailed(reason) => self.toast_error(aulos_core::fl!(
                "app-toast-action-failed",
                reason = reason.to_string()
            )),
            SessionNotice::ProviderConnectFailed { provider, reason } => {
                views::overlays::on_connect_failed(self, provider, reason);
            }
            SessionNotice::PlaylistsImported(files) => self.toast(aulos_core::fl!(
                "app-toast-playlists-imported",
                count = files.len()
            )),
            SessionNotice::PlaylistImportFailed(reason) => self.toast_error(aulos_core::fl!(
                "app-toast-playlist-import-failed",
                reason = reason.to_string()
            )),
            SessionNotice::Podcast(notice) => self.show_podcast_notice(notice),
            SessionNotice::Radio(notice) => self.show_radio_notice(notice),
            SessionNotice::Convert(ConvertNotice::QueueFinished { done, failed }) => {
                if *failed == 0 {
                    self.toast(aulos_core::fl!("app-toast-converted", count = (*done)));
                } else {
                    self.toast_error(aulos_core::fl!(
                        "app-toast-converted-failed",
                        count = (*done),
                        failed = (*failed)
                    ));
                }
            }
        }
    }

    fn show_podcast_notice(&mut self, notice: &PodcastNotice) {
        match notice {
            PodcastNotice::Subscribed { name } => self.toast(aulos_core::fl!(
                "app-toast-subscribed",
                name = name.to_string()
            )),
            PodcastNotice::SubscribeFailed { reason } => self.toast_error(aulos_core::fl!(
                "app-toast-subscribe-failed",
                reason = reason.to_string()
            )),
            PodcastNotice::UnsubscribeFailed { reason } => self.toast_error(aulos_core::fl!(
                "app-toast-unsubscribe-failed",
                reason = reason.to_string()
            )),
            PodcastNotice::DownloadFailed { reason } => self.toast_error(aulos_core::fl!(
                "app-toast-download-failed",
                reason = reason.to_string()
            )),
        }
    }

    fn show_radio_notice(&mut self, notice: &RadioNotice) {
        match notice {
            RadioNotice::Saved { name } => self.toast(aulos_core::fl!(
                "app-toast-station-saved",
                name = name.to_string()
            )),
            RadioNotice::AlreadySaved { name } => self.toast(aulos_core::fl!(
                "app-toast-station-already-saved",
                name = name.to_string()
            )),
            RadioNotice::SaveFailed { reason } => self.toast_error(aulos_core::fl!(
                "app-toast-station-save-failed",
                reason = reason.to_string()
            )),
            RadioNotice::PlayFailed { reason } => self.toast_error(aulos_core::fl!(
                "app-toast-station-play-failed",
                reason = reason.to_string()
            )),
        }
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.push_toast(text.into(), ToastKind::Info);
    }

    pub fn toast_error(&mut self, text: impl Into<String>) {
        self.push_toast(text.into(), ToastKind::Error);
    }

    /// A toast with an icon, a bold `subject` between `prefix` and `suffix`,
    /// and an optional action chip (e.g. Undo).
    pub fn toast_rich(
        &mut self,
        icon: &'static str,
        prefix: impl Into<String>,
        subject: impl Into<String>,
        suffix: impl Into<String>,
        action: Option<ToastAction>,
    ) {
        self.push_toast(prefix.into(), ToastKind::Info);
        if let Some(toast) = self.toasts.last_mut() {
            toast.icon = Some(icon);
            toast.subject = Some(subject.into());
            toast.suffix = suffix.into();
            if action.is_some() {
                toast.life = 8.0;
            }
            toast.action = action;
        }
    }

    fn push_toast(&mut self, text: String, kind: ToastKind) {
        let born = self.ctx.input(|i| i.time);
        self.toasts.push(Toast {
            text,
            subject: None,
            suffix: String::new(),
            icon: None,
            action: None,
            kind,
            born,
            life: f64::from(theme::motion::TOAST_LIFE),
        });
        self.ctx.request_repaint();
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

    // -- navigation ---------------------------------------------------------

    /// Goes to `route`, pushing it on the history.
    pub fn navigate(&mut self, route: Route) {
        if route == self.route
            && self.history_pos + 1 == self.history.len()
            && self.history.len() > 1
        {
            return;
        }
        self.history.truncate(self.history_pos + 1);
        self.history.push(route.clone());
        self.history_pos = self.history.len() - 1;
        self.enter(route);
    }

    pub fn can_go_back(&self) -> bool {
        self.history_pos > 0
    }
    pub fn can_go_forward(&self) -> bool {
        self.history_pos + 1 < self.history.len()
    }

    pub fn go_back(&mut self) {
        if self.can_go_back() {
            self.history_pos -= 1;
            let route = self.history[self.history_pos].clone();
            self.enter(route);
        }
    }

    pub fn go_forward(&mut self) {
        if self.can_go_forward() {
            self.history_pos += 1;
            let route = self.history[self.history_pos].clone();
            self.enter(route);
        }
    }

    fn enter(&mut self, route: Route) {
        self.route = route.clone();
        self.expanded = false;
        let key = route.key();
        if self.session.config.config.last_view != key {
            self.session.config.config.last_view = key.to_owned();
            self.session.config.save();
        }
        views::enter(self, &route);
    }

    // -- playback conveniences ----------------------------------------------

    /// The audio-quality tier of `track` for badges (None: unknown).
    pub fn quality_of(track: &Track) -> Option<Quality> {
        match quality::classify(&track.path, track.sample_rate, track.bitrate) {
            AudioQuality::Unknown => None,
            AudioQuality::Lossy => Some(Quality::Lossy),
            AudioQuality::CdLossless => Some(Quality::Cd),
            AudioQuality::HiRes => Some(Quality::HiRes),
            AudioQuality::Dsd => Some(Quality::Dsd),
        }
    }

    pub fn toggle_favorite(&mut self, track: &Track) {
        let effect = self.session.toggle_favorite(track.id.to_string());
        self.run(effect);
    }

    pub fn toggle_play(&mut self) {
        let update = self.session.toggle_playback();
        self.run(update);
    }

    /// Current volume (0..=1).
    pub fn volume(&self) -> f32 {
        self.session
            .playback
            .player
            .as_ref()
            .map_or(self.session.config.config.volume, |p| p.volume())
    }

    /// Switches the window between the full UI and the mini player.
    pub fn set_mini(&mut self, ctx: &Context, mini: bool) {
        if self.mini == mini {
            return;
        }
        self.mini = mini;
        #[cfg(feature = "visualizer")]
        if mini {
            self.set_visualizer(false);
        }
        if mini {
            self.saved_size = Some(ctx.content_rect().size());
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                360.0, 176.0,
            )));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(360.0, 176.0)));
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                420.0, 190.0,
            )));
            if let Some(size) = self.saved_size.take() {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            }
        }
    }

    /// Whether the visualizer view is showing.
    #[cfg(feature = "visualizer")]
    pub fn viz_open(&self) -> bool {
        self.viz.is_some()
    }

    /// Whether lactea can run here: a GPU renderer exists and, in the browser,
    /// it is WebGPU (lactea's shaders need compute-class features the WebGL2
    /// path lacks).
    #[cfg(feature = "visualizer")]
    pub fn viz_available(&self) -> bool {
        self.render_state.as_ref().is_some_and(|rs| {
            !cfg!(target_arch = "wasm32")
                || rs.adapter.get_info().backend != eframe::wgpu::Backend::Gl
        })
    }

    /// Opens or closes the visualizer. Opening starts the render thread and,
    /// for MPD, the audio capture; closing stops both.
    #[cfg(feature = "visualizer")]
    pub fn set_visualizer(&mut self, open: bool) {
        if open == self.viz.is_some() || (open && self.mini) {
            return;
        }
        if open {
            let Some(render_state) = self.render_state.clone() else {
                self.toast_error(aulos_core::fl!("app-toast-visualizer-needs-gpu"));
                return;
            };
            let ppp = self.ctx.pixels_per_point();
            let size = self.ctx.content_rect().size() * ppp;
            let now = self.ctx.input(|i| i.time);
            self.views.visualizer.reset(now);
            self.viz = Some(crate::viz::Viz::start(
                render_state,
                self.ctx.clone(),
                std::sync::Arc::clone(&self.session.playback.pcm_buffer),
                self.viz_settings.clone(),
                [size.x as u32, size.y as u32],
            ));
        } else if let Some(mut viz) = self.viz.take() {
            self.viz_settings = viz.snapshot(self.viz_settings.clone());
            viz.stop();
            if std::mem::take(&mut self.views.visualizer.fullscreen) {
                self.ctx
                    .send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
        }
        self.session.set_visualizer_capture(open);
        self.streams_dirty = true;
        self.ctx.request_repaint();
    }

    /// Horizontal page padding of the current layout (32px wide, 24px
    /// narrow, 16px on phones).
    pub fn page_pad(&self) -> f32 {
        match self.class {
            theme::WidthClass::Compact => theme::sp::S7,
            theme::WidthClass::Medium => theme::sp::S9,
            theme::WidthClass::Expanded => theme::sp::S10,
        }
    }

    /// The window is narrower than the desktop layout (rail, trimmed panes).
    pub fn narrow(&self) -> bool {
        self.class != theme::WidthClass::Expanded
    }

    /// The phone layout: bottom tab bar, one pane, full-width sheets.
    pub fn compact(&self) -> bool {
        self.class == theme::WidthClass::Compact
    }

    // -- cover-derived art ----------------------------------------------------

    /// Re-derives the cover key of the current track and starts loading its
    /// large art when it changed. Cheap when nothing is dirty.
    fn update_art(&mut self) {
        // The expanded cover is decoded for the current density; redo it when
        // the scale factor (display change, interface zoom) moves.
        let ppp = self.ctx.pixels_per_point();
        if (ppp - self.art_ppp).abs() > 0.01 {
            self.art_ppp = ppp;
            self.art_done = None;
            self.art_dirty = true;
        }
        if !std::mem::take(&mut self.art_dirty) {
            return;
        }
        let Some(track) = self.session.playback.current_track.as_ref() else {
            self.now_key = None;
            self.art_done = None;
            self.retarget_accent(None);
            self.backdrop = None;
            self.cover_large = None;
            return;
        };
        let key = track.cover_key();
        if self.now_key.as_deref() != Some(&key) {
            self.now_key = Some(key.clone());
        }
        if self.art_done.as_deref() == Some(&key) || self.art_pending.as_deref() == Some(&key) {
            return;
        }
        let Some(bytes) = self.session.cover_bytes.get(&key) else {
            // Not cached: load it; `CoverBytes` marks art dirty again.
            if self.session.cover_bytes.should_load(&key)
                && let Some(hint) = self
                    .session
                    .library
                    .cover_hint_for(track.grouping_artist(), &track.album)
            {
                let effect = self.session.load_cover_bytes(key.clone(), hint);
                effect.spawn(&self.handle, self.sink());
            } else if !self.session.cover_bytes.should_load(&key) {
                // Known absent (or in flight): without bytes there is no art.
                self.retarget_accent(None);
                self.backdrop = None;
                self.cover_large = None;
                self.art_done = Some(key);
            }
            return;
        };
        self.art_pending = Some(key.clone());
        let (tx, ctx) = (self.app_tx.clone(), self.ctx.clone());
        // The expanded cover is drawn up to ~480 points wide: keep it sharp at
        // any density (the blurred backdrop is soft by design, so it is not).
        let large_dim =
            ((EXPANDED_COVER_POINTS * ppp).ceil() as u32).max(CoverArt::EXPANDED_COVER_MAX_DIM);
        self.handle.spawn_blocking(move || {
            let backdrop = CoverArt::blurred_backdrop(&bytes);
            let accent = palette::extract(&bytes);
            let large = CoverArt::decode_thumbnail(&bytes, large_dim);
            if tx
                .send(AppEvent::Art {
                    key,
                    backdrop,
                    accent,
                    large,
                })
                .is_ok()
            {
                ctx.request_repaint();
            }
        });
    }

    fn apply_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Media(MprisEvent::Ready(handle)) => {
                let update = self.session.connect_media_session(handle);
                self.run(update);
            }
            AppEvent::Media(MprisEvent::Command(command)) => self.media_command(command),
            AppEvent::Art {
                key,
                backdrop,
                accent,
                large,
            } => {
                if self.art_pending.as_deref() == Some(&key) {
                    self.art_pending = None;
                }
                if self.now_key.as_deref() != Some(&key) {
                    // The track moved on; the next dirty pass handles the new one.
                    self.art_dirty = true;
                    return;
                }
                let tex = |name: &str, (w, h, px): (u32, u32, Vec<u8>)| {
                    let image =
                        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
                    self.ctx.load_texture(name, image, TextureOptions::LINEAR)
                };
                let backdrop = backdrop.map(|b| tex("backdrop", b));
                let large = large.map(|l| tex("cover-large", l));
                self.art_fade = ArtFade {
                    backdrop: self.backdrop.take(),
                    cover: self.cover_large.take(),
                    started: self.ctx.input(|i| i.time),
                };
                self.backdrop = backdrop;
                self.cover_large = large;
                let color = accent.map(|a| {
                    Color32::from_rgb(
                        (a.color[0] * 255.0) as u8,
                        (a.color[1] * 255.0) as u8,
                        (a.color[2] * 255.0) as u8,
                    )
                });
                self.retarget_accent(color);
                self.art_done = Some(key);
            }
        }
    }

    /// Carries out a command from the media session: window commands here,
    /// playback ones in the session.
    fn media_command(&mut self, command: MprisCommand) {
        match command {
            MprisCommand::Raise => {
                self.ctx
                    .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            MprisCommand::Quit => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            command => {
                let update = self.session.apply_mpris_command(command);
                self.run(update);
            }
        }
    }

    /// Starts easing the accent towards the one of `color` (None: default).
    fn retarget_accent(&mut self, color: Option<Color32>) {
        self.cover_tint = color;
        self.ease_accent();
    }

    /// Eases the accent to the one of the remembered cover colour, or to
    /// the default when "tint with cover" is off. Jumps with reduced motion.
    fn ease_accent(&mut self) {
        let tint = self
            .session
            .config
            .config
            .tint_ui_with_cover
            .then_some(self.cover_tint)
            .flatten();
        let target = Accent::from_cover(tint, &self.pal);
        if target == self.accent_to {
            return;
        }
        self.accent_from = self.accent;
        self.accent_to = target;
        if theme::motion::reduced(&self.ctx) {
            self.accent = target;
        }
        self.accent_start = self.ctx.input(|i| i.time);
        self.ctx.request_repaint();
    }

    /// Turns the music-folder watcher on or off right away.
    pub fn set_watch_for_changes(&mut self, on: bool) {
        self.session.set_watch_for_changes(on);
        self.streams_dirty = true;
    }

    /// Re-derives the accent after the "tint with cover" setting changed.
    pub fn set_tint_ui_with_cover(&mut self, on: bool) {
        self.session.config.config.tint_ui_with_cover = on;
        self.session.config.save();
        self.ease_accent();
    }

    fn step_accent(&mut self, now: f64) {
        if self.accent == self.accent_to {
            return;
        }
        let k =
            ((now - self.accent_start) / f64::from(theme::motion::ACCENT)).clamp(0.0, 1.0) as f32;
        let eased = k * k * (3.0 - 2.0 * k);
        self.accent = Accent::lerp(&self.accent_from, &self.accent_to, eased);
        if k >= 1.0 {
            self.accent = self.accent_to;
        } else {
            self.ctx.request_repaint();
        }
    }

    /// Sets, applies and saves the interface zoom (snapped to the supported range).
    pub fn set_ui_scale(&mut self, scale: f32) {
        let scale = Config::clamp_ui_scale(scale);
        if (self.session.config.config.ui_scale - scale).abs() < f32::EPSILON {
            return;
        }
        self.session.config.config.ui_scale = scale;
        self.session.config.save();
        self.apply_zoom();
    }

    /// Sets and saves the interface language (`""` follows the system) and
    /// switches the loaded translations at once.
    pub fn set_language(&mut self, language: &str) {
        if self.session.config.config.language == language {
            return;
        }
        self.session.config.config.language = language.to_owned();
        self.session.config.save();
        aulos_core::i18n::init_for(language);
        self.ctx.request_repaint();
    }

    /// Applies `ui_scale` on top of the density base, capped so the layout
    /// never gets narrower than [`theme::size::MIN_WIDTH`] logical points (a
    /// 200% zoom on a phone would otherwise leave ~190pt).
    fn apply_zoom(&self) {
        let base = self.zoom_base.unwrap_or(1.0);
        let mut zoom = self.session.config.config.ui_scale * base;
        if let Some(native) = self.ctx.native_pixels_per_point() {
            let px_width = self.ctx.content_rect().width() * self.ctx.pixels_per_point();
            if px_width > 0.0 {
                zoom = zoom.min(px_width / (native * theme::size::MIN_WIDTH));
            }
        }
        if (self.ctx.zoom_factor() - zoom).abs() > 1e-3 {
            self.ctx.set_zoom_factor(zoom);
        }
    }

    /// Moves the interface zoom one step along [`UI_SCALE_STEPS`] (`0` resets).
    fn step_ui_scale(&mut self, direction: i8) {
        let current = self.session.config.config.ui_scale;
        let next = match direction {
            0 => 1.0,
            d if d > 0 => UI_SCALE_STEPS
                .iter()
                .copied()
                .find(|s| *s > current + 0.001)
                .unwrap_or(current),
            _ => UI_SCALE_STEPS
                .iter()
                .rev()
                .copied()
                .find(|s| *s < current - 0.001)
                .unwrap_or(current),
        };
        self.set_ui_scale(next);
    }

    /// Whether `mode` is dark given the OS preference. `AULOS_SMOKE_LIGHT`
    /// forces light for the design comparison shots.
    fn resolve_dark(mode: ThemeMode, system: Option<egui::Theme>) -> bool {
        if std::env::var_os("AULOS_SMOKE_LIGHT").is_some() {
            return false;
        }
        mode.is_dark(system.map(|t| t == egui::Theme::Dark))
    }

    /// Chooses and saves the theme; [`Self::sync_theme`] applies it.
    pub fn set_theme_mode(&mut self, mode: ThemeMode) {
        if self.session.config.config.theme_mode == mode {
            return;
        }
        self.session.config.config.theme_mode = mode;
        self.session.config.save();
        self.ctx.request_repaint();
    }

    /// Applies the chosen theme, following the OS while it is "System",
    /// and re-derives the accent for the palette.
    fn sync_theme(&mut self, ctx: &Context) {
        let dark = Self::resolve_dark(self.session.config.config.theme_mode, ctx.system_theme());
        if self.dark == dark {
            return;
        }
        self.dark = dark;
        self.pal = if dark { theme::DARK } else { theme::LIGHT };
        theme::apply(&self.ctx, &self.pal);
        self.ease_accent();
        self.accent = self.accent_to;
        self.art_done = None;
        self.art_dirty = true;
    }

    // -- frame ------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let (space, slash, esc, back, fwd, digit) = ctx.input(|i| {
            let digit = [
                egui::Key::Num1,
                egui::Key::Num2,
                egui::Key::Num3,
                egui::Key::Num4,
                egui::Key::Num5,
                egui::Key::Num6,
                egui::Key::Num7,
                egui::Key::Num8,
            ]
            .iter()
            .position(|k| i.key_pressed(*k) && !i.modifiers.any());
            (
                i.key_pressed(egui::Key::Space) && !i.modifiers.any(),
                i.key_pressed(egui::Key::Slash) && !i.modifiers.any(),
                i.key_pressed(egui::Key::Escape),
                i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft),
                i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight),
                digit,
            )
        });
        let zoom = ctx.input_mut(|i| {
            let m = egui::Modifiers::COMMAND;
            if i.consume_key(m, egui::Key::Num0) {
                Some(0)
            } else if i.consume_key(m, egui::Key::Equals) || i.consume_key(m, egui::Key::Plus) {
                Some(1)
            } else if i.consume_key(m, egui::Key::Minus) {
                Some(-1)
            } else {
                None
            }
        });
        if let Some(direction) = zoom {
            self.step_ui_scale(direction);
        }
        let (find, toggle_mini) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::F),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::M),
            )
        });
        if toggle_mini {
            self.set_mini(ctx, !self.mini);
        }
        if self.mini {
            if esc {
                self.set_mini(ctx, false);
            }
            return;
        }
        if find {
            ctx.memory_mut(|m| m.request_focus(self.search_id));
        }
        // The visualizer handles its own Esc (browser, fullscreen, close).
        #[cfg(feature = "visualizer")]
        let esc = esc && !self.viz_open();
        if esc {
            if typing {
                ctx.memory_mut(|m| m.surrender_focus(self.search_id));
                if !self.search.is_empty() {
                    self.search.clear();
                    let effect = self.session.set_search_query(String::new());
                    self.run(effect);
                }
            } else if self.sheet.is_some() {
                self.sheet = None;
            } else if self.expanded {
                self.expanded = false;
            }
        }
        #[cfg(feature = "visualizer")]
        let viz_open = self.viz_open();
        #[cfg(not(feature = "visualizer"))]
        let viz_open = false;
        if back && !viz_open {
            self.go_back();
        }
        if fwd && !viz_open {
            self.go_forward();
        }
        if typing {
            return;
        }
        if space {
            self.toggle_play();
        }
        // The visualizer owns the rest of the keys (and covers every page).
        if viz_open {
            return;
        }
        if slash {
            ctx.memory_mut(|m| m.request_focus(self.search_id));
        }
        if let Some(i) = digit {
            let routes = [
                Route::Home,
                Route::Albums,
                Route::Artists,
                Route::Songs,
                Route::Genres,
                Route::Folders,
                Route::Playlists,
                Route::Podcasts,
            ];
            self.navigate(routes[i].clone());
        }
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &Context, raw_input: &mut egui::RawInput) {
        // Android draws edge to edge: keep the content clear of the system bars.
        #[cfg(target_os = "android")]
        if let Some([left, top, right, bottom]) = crate::android::system_insets() {
            let ppp = ctx.pixels_per_point();
            raw_input.safe_area_insets = Some(egui::SafeAreaInsets(egui::epaint::MarginF32 {
                left: left / ppp,
                right: right / ppp,
                top: top / ppp,
                bottom: bottom / ppp,
            }));
        }
        // Smoke shots must not show hover tints or tooltips from a stray pointer.
        if self.smoke.is_some() {
            raw_input.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::PointerMoved(_)
                        | egui::Event::PointerButton { .. }
                        | egui::Event::MouseWheel { .. }
                )
            });
            let scripted = self
                .smoke
                .as_mut()
                .is_some_and(|smoke| smoke.drag_events(ctx, raw_input));
            if !scripted {
                raw_input.events.push(egui::Event::PointerGone);
            }
            if let Some(smoke) = &self.smoke {
                smoke.scroll(ctx, raw_input);
            }
        }
    }

    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        self.session.set_window_focused(focused);
        self.session.set_seeking(self.seek_preview.is_some());
        while let Ok(event) = self.rx.try_recv() {
            let update = self.session.apply_session_event(event);
            self.run(update);
        }
        while let Ok(event) = self.app_rx.try_recv() {
            self.apply_app_event(event);
        }
        if std::mem::take(&mut self.streams_dirty) {
            self.sync_streams();
        }
        if self.zoom_base.is_none()
            && let Some(native) = ctx.native_pixels_per_point()
        {
            self.zoom_base = Some(1.0 / native);
        }
        // Every frame: a rotation or a window resize changes the cap.
        self.apply_zoom();
        self.covers.upload_pending(&self.session.library.covers);
        #[cfg(target_arch = "wasm32")]
        for command in crate::media_session::sync(ctx, &mut self.session) {
            self.media_command(command);
        }
        views::tagger::pump(self);
        views::settings::pump(self);
        self.update_art();
        let now = ctx.input(|i| i.time);
        self.step_accent(now);
        self.toasts.retain(|t| now - t.born < t.life);
        if !self.toasts.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
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
        // Edge to edge on Android: lay out inside the system bars.
        #[cfg(target_os = "android")]
        let mut inset_ui = ui.new_child(egui::UiBuilder::new().max_rect(ui.ctx().content_rect()));
        #[cfg(target_os = "android")]
        let ui = &mut inset_ui;
        let ctx = ui.ctx().clone();
        self.sync_theme(&ctx);
        Theme {
            pal: self.pal,
            accent: self.accent,
        }
        .publish(&ctx);
        theme::motion::set_reduced(&ctx, self.session.config.config.reduce_motion);
        self.class = theme::WidthClass::of(ctx.content_rect().width());
        theme::set_touch(&ctx, self.compact() || cfg!(target_os = "android"));
        #[cfg(target_os = "android")]
        views::edit_bar::begin(&ctx);
        self.shortcuts(&ctx);
        // Hero pages announce their tint while drawing; the toolbar, drawn
        // before the page, uses last frame's value.
        {
            let (key, next_key) = (egui::Id::new("hero_tint"), egui::Id::new("hero_tint_next"));
            let next: Option<Color32> = ctx.data_mut(|d| d.remove_temp(next_key));
            let current: Option<Color32> = ctx.data(|d| d.get_temp(key));
            if next != current {
                ctx.data_mut(|d| match next {
                    Some(c) => {
                        d.insert_temp(key, c);
                    }
                    None => d.remove::<Color32>(key),
                });
                ctx.request_repaint();
            }
        }
        #[cfg(feature = "visualizer")]
        let viz_open = self.viz_open();
        #[cfg(not(feature = "visualizer"))]
        let viz_open = false;
        if viz_open {
            #[cfg(feature = "visualizer")]
            views::visualizer::show(self, ui);
        } else if self.mini {
            views::mini::show(self, ui);
        } else if self.expanded {
            views::expanded::show(self, ui);
        } else {
            views::chrome::show(self, ui);
        }
        views::toasts::show(self, &ctx);
        views::overlays::show(self, &ctx);
        #[cfg(target_os = "android")]
        views::edit_bar::show(&ctx);
    }

    fn on_exit(&mut self) {
        #[cfg(feature = "visualizer")]
        self.set_visualizer(false);
        self.session.config.flush();
    }
}
