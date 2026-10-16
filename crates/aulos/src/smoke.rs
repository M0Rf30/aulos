//! Headless-ish smoke test, enabled by `AULOS_SMOKE=<out.ppm>`: once the
//! library is loaded, opens the requested view, plays the first album for a
//! moment, saves a screenshot of the window (egui's own capture, so nothing
//! else on screen is read) and exits.
//!
//! Knobs (all optional):
//! - `AULOS_SMOKE_PAGE=<home|albums|album:<n>|artists|artist:<name>|songs|genres|genre:<name>|folders|playlists|podcasts|radio|tagger|converter|settings|settings:<n>|expanded|mini>`
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
//! - `AULOS_SMOKE_DRAG=<queue|playlist>`: run a scripted pointer drag
//!   through the real views and check the resulting order before the
//!   screenshot (queue: row 3 above row 1; playlist: row 2 to the end). Prints
//!   `smoke drag: PASS|FAIL …` and exits non-zero on failure. Needs an album
//!   of 5+ tracks (queue) or a plain playlist of 4+ tracks (playlist).
//!
//! `AULOS_SMOKE_PAGE=visualizer` opens the visualizer (feature `visualizer`);
//! `AULOS_SMOKE_VIZ_LOCK=1` locks its preset.

use crate::app::{App, Route, Sheet};
use std::path::PathBuf;
use std::time::Duration;
use web_time::Instant;

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Queue,
    Playlist,
}

enum DragPhase {
    /// Waiting for the rows to be drawn and stop moving (the sheet slides
    /// in); nothing injected yet. Holds the rows of the previous frame and
    /// how many frames in a row they stayed put.
    Prepare {
        last: Vec<(usize, egui::Rect)>,
        still: u32,
    },
    /// Injecting pointer events, `tick` frames in.
    Run { tick: u32 },
    /// Released; waiting for the session to reflect the move.
    Settle { frames: u32 },
}

struct DragScript {
    kind: DragKind,
    phase: DragPhase,
    src: egui::Pos2,
    dst: egui::Pos2,
    expected: Vec<PathBuf>,
}

pub struct Smoke {
    drag: Option<DragScript>,
    out: PathBuf,
    started: Option<Instant>,
    launched: Instant,
    requested: bool,
    /// Set once the first screenshot is saved and a second one is wanted.
    second_at: Option<Instant>,
}

/// The window size requested for smoke runs.
#[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
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
        let drag = std::env::var("AULOS_SMOKE_DRAG")
            .ok()
            .and_then(|v| match v.as_str() {
                "queue" => Some(DragKind::Queue),
                "playlist" => Some(DragKind::Playlist),
                _ => None,
            })
            .map(|kind| DragScript {
                kind,
                phase: DragPhase::Prepare {
                    last: Vec::new(),
                    still: 0,
                },
                src: egui::Pos2::ZERO,
                dst: egui::Pos2::ZERO,
                expected: Vec::new(),
            });
        Some(Self {
            drag,
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

    /// Injects the scripted drag's pointer events; true while it owns the
    /// pointer (the caller then must not add its own `PointerGone`).
    pub fn drag_events(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) -> bool {
        use egui::{Event, PointerButton};
        let Some(drag) = &mut self.drag else {
            return false;
        };
        let DragPhase::Run { tick } = &mut drag.phase else {
            return false;
        };
        let (src, dst) = (drag.src, drag.dst);
        let button = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        match *tick {
            0 => raw_input.events.push(Event::PointerMoved(src)),
            1 => raw_input.events.push(button(src, true)),
            2..=5 => {
                let t = (*tick - 1) as f32 / 5.0;
                raw_input.events.push(Event::PointerMoved(src.lerp(dst, t)));
            }
            6 | 7 => raw_input.events.push(Event::PointerMoved(dst)),
            _ => {
                raw_input.events.push(button(dst, false));
                drag.phase = DragPhase::Settle { frames: 0 };
                ctx.request_repaint();
                return true;
            }
        }
        *tick += 1;
        ctx.request_repaint();
        true
    }

    /// Advances the drag script from the frame loop; `None` without a script.
    fn drag_step(&mut self, app: &mut App) -> Option<bool> {
        let drag = self.drag.as_mut()?;
        let order = |app: &App| -> Vec<PathBuf> {
            match drag.kind {
                DragKind::Queue => app
                    .session
                    .playback
                    .player
                    .as_ref()
                    .map_or_else(Vec::new, |p| {
                        p.queue().iter().map(|t| t.path.clone()).collect()
                    }),
                DragKind::Playlist => crate::views::playlists::open_tracks(app)
                    .map(|t| t.iter().map(|t| t.path.clone()).collect())
                    .unwrap_or_default(),
            }
        };
        match drag.phase {
            DragPhase::Prepare { .. } => {
                let rows: Vec<(usize, egui::Rect)> = match drag.kind {
                    DragKind::Queue => app.views.queue.rows().to_vec(),
                    DragKind::Playlist => {
                        if !crate::views::playlists::has_open(app) {
                            crate::views::playlists::open_first_with(app, 4);
                        }
                        app.views.playlists.rows().to_vec()
                    }
                };
                let DragPhase::Prepare { last, still } = &mut drag.phase else {
                    unreachable!()
                };
                if *last == rows && !rows.is_empty() {
                    *still += 1;
                } else {
                    *still = 0;
                    last.clone_from(&rows);
                }
                if *still < 20 {
                    return Some(false);
                }
                let before = order(app);
                match drag.kind {
                    DragKind::Queue if rows.len() >= 3 => {
                        let (a0, a2) = (rows[0].0, rows[2].0);
                        let (r0, r2) = (rows[0].1, rows[2].1);
                        let mut expected = before.clone();
                        let moved = expected.remove(a2);
                        expected.insert(a0, moved);
                        drag.src = r2.center();
                        drag.dst = egui::pos2(r2.center().x, r0.top() + 2.0);
                        drag.expected = expected;
                    }
                    DragKind::Playlist
                        if rows.len() >= 2
                            && rows.last().map(|r| r.0) == Some(before.len() - 1) =>
                    {
                        let mut expected = before.clone();
                        let moved = expected.remove(1);
                        expected.push(moved);
                        let src = rows.iter().find(|r| r.0 == 1)?.1;
                        let last = rows.last()?.1;
                        drag.src = src.center();
                        drag.dst = egui::pos2(src.center().x, last.bottom() + 3.0);
                        drag.expected = expected;
                    }
                    _ => return Some(false),
                }
                drag.phase = DragPhase::Run { tick: 0 };
                Some(false)
            }
            DragPhase::Run { .. } => Some(false),
            DragPhase::Settle { frames } => {
                let actual = order(app);
                let done = actual == drag.expected;
                if !done && frames < 60 {
                    drag.phase = DragPhase::Settle { frames: frames + 1 };
                    return Some(false);
                }
                let name = |v: &[PathBuf]| {
                    v.iter()
                        .map(|p| {
                            p.file_stem()
                                .map_or_else(String::new, |s| s.to_string_lossy().into_owned())
                        })
                        .collect::<Vec<_>>()
                        .join(" | ")
                };
                if done {
                    println!("smoke drag: PASS {}", name(&actual));
                    // Carry on to the usual screenshot of the result.
                    self.drag = None;
                    Some(false)
                } else {
                    println!(
                        "smoke drag: FAIL\n  expected {}\n  actual   {}",
                        name(&drag.expected),
                        name(&actual)
                    );
                    std::process::exit(1);
                }
            }
        }
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
                // Never play real music: every queued track points at a
                // silent file, the first one is the file itself.
                let silence = std::env::var_os("AULOS_SMOKE_FILE")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| "target/smoke/silence.flac".into());
                let mut tracks = app
                    .session
                    .library
                    .albums
                    .get(album)
                    .map(|a| a.tracks.clone())
                    .unwrap_or_default();
                for track in &mut tracks {
                    track.source_uri = silence.to_string_lossy().into_owned();
                    track.path.clone_from(&silence);
                    track.duration = Duration::from_secs(600);
                }
                if let Some(first) = tracks.first_mut() {
                    first.title = "Silence".into();
                }
                let update = app.session.play_tracks(tracks, 0);
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
                    // `settings:<n>` opens the n-th section (phone layout).
                    if let Some(n) = spec
                        .strip_prefix("settings:")
                        .and_then(|n| n.parse::<usize>().ok())
                    {
                        crate::views::settings::open_section(app, n);
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
            if let Some(drag) = &self.drag {
                match drag.kind {
                    DragKind::Queue => {
                        let big = app
                            .session
                            .library
                            .visible_albums()
                            .iter()
                            .position(|a| a.tracks.len() >= 5);
                        let update = app.session.play_album(big.unwrap_or(0), 0);
                        app.run(update);
                        app.sheet = Some(Sheet::Queue);
                    }
                    DragKind::Playlist => app.navigate(Route::Playlists),
                }
            }
            self.started = Some(Instant::now());
            return false;
        };
        if self.drag.is_some() {
            ctx.request_repaint();
            return self.drag_step(app).unwrap_or(false);
        }
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
