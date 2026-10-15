//! Headless-ish smoke test, enabled by `AULOS_SMOKE=<out.ppm>`: once the
//! library is loaded, opens the requested view, plays the first album for a
//! moment, saves a screenshot of the window (egui's own capture, so nothing
//! else on screen is read) and exits.
//!
//! Knobs (all optional):
//! - `AULOS_SMOKE_PAGE=<home|albums|album:<n>|artists|artist:<name>|songs|genres|genre:<name>|folders|playlists|podcasts|radio|tagger|converter|settings|expanded|mini>`
//! - `AULOS_SMOKE_TAB=songs` (alias of `AULOS_SMOKE_PAGE=songs`)
//! - `AULOS_SMOKE_SEARCH=<text>`: type into the search box
//! - `AULOS_SMOKE_OVERLAY=<queue|props|presets>`: open a side sheet (`presets`: the visualizer's browser)
//! - `AULOS_SMOKE_ALBUM=<n>`: play the n-th visible album instead of the first
//! - `AULOS_SMOKE_PLAY=0`: do not play anything (idle bar)
//! - `AULOS_SMOKE_EMPTY=1`: do not wait for a library (empty-state shots)
//! - `AULOS_SMOKE_LIGHT=1`: light theme
//! - `AULOS_SMOKE_SIZE=<w>x<h>`: window size (default 1280x800)
//! - `AULOS_SMOKE_DELAY_MS=<n>`: wait after setup before the capture (default 1500)
//! - `AULOS_SMOKE_SECOND=<out2.ppm>`: take a second screenshot one second after the first
//! - `AULOS_SMOKE_SCROLL=<px>`: scroll the page down by `<px>` every frame until the shot
//! - `AULOS_SMOKE_REPEAT=1`: repeat the playing track (short fixtures)
//!
//! `AULOS_SMOKE_PAGE=visualizer` opens the visualizer (feature `visualizer`);
//! `AULOS_SMOKE_VIZ_LOCK=1` locks its preset.

use crate::app::{App, Route, Sheet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Smoke {
    out: PathBuf,
    started: Option<Instant>,
    launched: Instant,
    requested: bool,
    /// Set once the first screenshot is saved and a second one is wanted.
    second_at: Option<Instant>,
}

/// The window size requested for smoke runs.
pub fn window_size() -> Option<[f32; 2]> {
    let spec = std::env::var("AULOS_SMOKE_SIZE").ok()?;
    let (w, h) = spec.split_once('x')?;
    Some([w.parse().ok()?, h.parse().ok()?])
}

fn route_of(spec: &str, app: &App) -> Option<Route> {
    let (head, arg) = spec
        .split_once(':')
        .map_or((spec, None), |(h, a)| (h, Some(a)));
    Some(match head {
        "home" => Route::Home,
        "albums" => Route::Albums,
        "album" => {
            let albums = app.session.library.visible_albums();
            let i = arg.and_then(|a| a.parse::<usize>().ok()).unwrap_or(0);
            Route::Album(albums.get(i)?.cover_key.clone())
        }
        "artists" => Route::Artists,
        "artist" => Route::Artist(match arg {
            Some(name) => name.to_owned(),
            None => app.session.library.visible_artists().first()?.name.clone(),
        }),
        "songs" => Route::Songs,
        "genres" => Route::Genres,
        "genre" => Route::Genre(match arg {
            Some(name) => name.to_owned(),
            None => app.session.library.genres.first()?.clone(),
        }),
        "folders" => Route::Folders,
        "playlists" => Route::Playlists,
        "podcasts" => Route::Podcasts,
        "radio" => Route::Radio,
        "tagger" => Route::Tagger,
        "converter" => Route::Converter,
        "settings" => Route::Settings,
        _ => return None,
    })
}

impl Smoke {
    pub fn from_env() -> Option<Self> {
        let out = std::env::var_os("AULOS_SMOKE")?;
        Some(Self {
            out: out.into(),
            started: None,
            launched: Instant::now(),
            requested: false,
            second_at: None,
        })
    }

    /// Feeds `AULOS_SMOKE_SCROLL` wheel events (and keeps frames coming)
    /// once the setup is done.
    pub fn scroll(&self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let Some(px) = std::env::var("AULOS_SMOKE_SCROLL")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
        else {
            return;
        };
        if self.started.is_none() || self.requested {
            return;
        }
        raw_input
            .events
            .push(egui::Event::PointerMoved(egui::pos2(700.0, 350.0)));
        raw_input.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -px),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        ctx.request_repaint();
    }

    /// Returns true once the screenshot is written and the app should close.
    pub fn step(&mut self, app: &mut App, ctx: &egui::Context) -> bool {
        let Some(started) = self.started else {
            let lib = &app.session.library;
            let ready = if std::env::var_os("AULOS_SMOKE_EMPTY").is_some() {
                self.launched.elapsed() > Duration::from_secs(2)
            } else {
                !lib.albums.is_empty() && !lib.scanning
            };
            if !ready {
                return false;
            }
            let page = std::env::var("AULOS_SMOKE_PAGE")
                .ok()
                .or_else(|| std::env::var("AULOS_SMOKE_TAB").ok());
            if let Some(query) = std::env::var("AULOS_SMOKE_SEARCH")
                .ok()
                .filter(|q| !q.is_empty())
            {
                app.search = query.clone();
                let effect = app.session.set_search_query(query);
                app.run(effect);
            }
            if std::env::var("AULOS_SMOKE_PLAY").map_or(true, |v| v != "0")
                && !app.session.library.albums.is_empty()
            {
                let album = std::env::var("AULOS_SMOKE_ALBUM")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let update = app.session.play_album(album, 0);
                app.run(update);
                if std::env::var_os("AULOS_SMOKE_REPEAT").is_some() {
                    let update = app
                        .session
                        .set_repeat_mode(aulos_core::config::RepeatMode::One);
                    app.run(update);
                }
            }
            match page.as_deref() {
                Some("expanded") => app.expanded = true,
                Some("mini") => app.set_mini(ctx, true),
                #[cfg(feature = "visualizer")]
                Some("visualizer") => {
                    app.set_visualizer(true);
                    if std::env::var_os("AULOS_SMOKE_VIZ_LOCK").is_some() {
                        app.viz_settings.locked = true;
                        if let Some(viz) = &app.viz {
                            viz.send(aulos_core::visualizer::VizCommand::SetLocked(true));
                        }
                    }
                }
                Some(spec) => {
                    if let Some(route) = route_of(spec, app) {
                        app.navigate(route);
                    }
                }
                None => {}
            }
            match std::env::var("AULOS_SMOKE_OVERLAY").as_deref() {
                Ok("queue") => app.sheet = Some(Sheet::Queue),
                Ok("props") => app.sheet = Some(Sheet::Properties),
                #[cfg(feature = "visualizer")]
                Ok("presets") => app.views.visualizer.open_browser(),
                _ => {}
            }
            self.started = Some(Instant::now());
            return false;
        };
        let delay = std::env::var("AULOS_SMOKE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1500);
        if !self.requested
            && self.second_at.is_none()
            && started.elapsed() > Duration::from_millis(delay)
        {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        if let Some(at) = self.second_at
            && !self.requested
            && Instant::now() >= at
        {
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
        let second = self.second_at.is_some();
        let out = if second {
            std::env::var_os("AULOS_SMOKE_SECOND").map_or_else(|| self.out.clone(), PathBuf::from)
        } else {
            self.out.clone()
        };
        if let Err(e) = std::fs::write(&out, ppm) {
            tracing::error!("smoke: cannot write screenshot: {e}");
        }
        tracing::info!("smoke: screenshot saved to {}", out.display());
        if !second && std::env::var_os("AULOS_SMOKE_SECOND").is_some() {
            self.second_at = Some(Instant::now() + Duration::from_secs(1));
            self.requested = false;
            return false;
        }
        let update = app.session.stop();
        app.run(update);
        true
    }
}
