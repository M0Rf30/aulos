//! The MilkDrop visualizer engine (feature `visualizer`).
//!
//! lactea 0.2 renders with a host-provided wgpu 30 device, which is the very
//! one eframe draws with, so a frame never leaves the GPU: [`Loop`] drives a
//! [`lactea::Engine`] into one `Rgba8Unorm` texture, and the UI shows that
//! texture through a registered native egui texture (re-pointed in place
//! when the render size changes, never reallocated per frame).
//!
//! What runs the loop depends on the platform (`viz/native.rs`, `viz/web.rs`):
//! a render thread on the desktop, spawned by [`Viz::start`] and joined by
//! [`Viz::stop`] (or drop); the page's one thread in the browser, which steps
//! the loop from [`Viz::poll`], i.e. once per UI frame. Either way it exists
//! only while the visualizer is on screen, and [`Viz::stop`] frees the egui
//! texture. Audio comes from the session's [`PcmBuffer`] (the local output,
//! or the MPD PipeWire capture which the session runs while
//! `set_visualizer_capture` is on). Presets and textures come from
//! [`aulos_core::visualizer::source`]: files on the desktop, the bundled and
//! imported ones in the browser.

use aulos_core::player::pcm::PcmBuffer;
use aulos_core::visualizer::playlist::PresetPlaylist;
use aulos_core::visualizer::presets::PresetEntry;
use aulos_core::visualizer::source::{self, Textures};
use aulos_core::visualizer::{PCM_FEED_SAMPLES, VizCommand};
use eframe::egui_wgpu::RenderState;
use eframe::wgpu;
use host::Host;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use web_time::Instant;

// What runs the render loop: a thread, or the page's frame callback.
#[cfg_attr(target_arch = "wasm32", path = "viz/web.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "viz/native.rs")]
mod host;

/// Frame period of the render loop (~60 fps).
const FRAME_PERIOD: Duration = Duration::from_millis(16);
/// Most frames the GPU may still be working on before the loop skips a tick.
const MAX_IN_FLIGHT: u32 = 2;
/// Render size limits, in physical pixels.
const MIN_SIZE: [u32; 2] = [320, 180];
const MAX_PIXELS: u64 = 1920 * 1080;
/// A new size only takes effect once the request has been stable this long
/// (resizing clears the feedback image).
const RESIZE_SETTLE: Duration = Duration::from_millis(300);
/// How many broken preset files in a row a switch skips before giving up.
const MAX_SWITCH_ATTEMPTS: usize = 5;
/// Length of a soft cut between two presets, in seconds.
const SOFT_CUT_SECONDS: f64 = 3.0;
/// Seconds a preset is shown before the rotation moves on.
const PRESET_DURATION_SECONDS: f64 = 30.0;
/// Frame rate reported to presets as `fps`.
const RENDER_FPS: u32 = 60;
/// Sample rate PCM is assumed to have for the analyser.
const PCM_SAMPLE_RATE: u32 = 44_100;
/// How long PCM may stay quiet before explicit silence is fed instead.
const PCM_STALE_TIMEOUT: Duration = Duration::from_millis(100);
/// One frame's worth of stereo silence.
const SILENCE_SAMPLES: usize = (PCM_SAMPLE_RATE as usize / 60) * 2;

/// What the visualizer remembers between openings (stored in the config via
/// `Session::set_visualizer_prefs`).
#[derive(Clone)]
pub struct Settings {
    pub locked: bool,
    /// Beat sensitivity, `0.0..=2.0`: the gain applied to the audio the
    /// analyser hears (projectM itself never reads its own setting).
    pub sensitivity: f32,
    /// The preset on screen when the visualizer was last closed.
    pub preset: Option<PathBuf>,
    /// Scanned presets, kept so reopening does not rescan the disk.
    pub entries: Option<Arc<Vec<PresetEntry>>>,
}

impl Settings {
    /// The remembered settings of the config.
    pub fn from_config(config: &aulos_core::config::Config) -> Self {
        Self {
            locked: config.visualizer_locked,
            sensitivity: config.visualizer_sensitivity,
            preset: config.visualizer_preset.clone(),
            entries: None,
        }
    }
}

/// What the render loop tells the UI.
#[derive(Default)]
struct Published {
    /// Bumped whenever `view` is replaced.
    generation: u64,
    view: Option<wgpu::TextureView>,
    size: [u32; 2],
    preset: Option<PathBuf>,
    entries: Option<Arc<Vec<PresetEntry>>>,
    /// The engine could not start.
    failure: Option<String>,
}

struct Shared {
    published: Mutex<Published>,
    /// Wanted render size, `width << 32 | height`.
    want: AtomicU64,
    /// Set while the window is minimized: the loop renders nothing.
    paused: AtomicBool,
}

/// A snapshot for drawing, see [`Viz::poll`].
pub struct Frame {
    /// The egui texture holding the latest frame (None before the first one).
    pub texture: Option<egui::TextureId>,
    pub preset: Option<PathBuf>,
    pub entries: Option<Arc<Vec<PresetEntry>>>,
    pub failure: Option<String>,
}

/// What the render loop owns; handed to the [`Host`] that runs it.
struct Inputs {
    shared: Arc<Shared>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pcm: Arc<Mutex<PcmBuffer>>,
    commands: mpsc::Receiver<VizCommand>,
    ctx: egui::Context,
    settings: Settings,
}

/// UI-side handle of a running visualizer.
pub struct Viz {
    shared: Arc<Shared>,
    commands: mpsc::Sender<VizCommand>,
    host: Host,
    render_state: RenderState,
    texture: Option<egui::TextureId>,
    seen_generation: u64,
}

impl Viz {
    /// Starts the render loop.
    pub fn start(
        render_state: RenderState,
        ctx: egui::Context,
        pcm: Arc<Mutex<PcmBuffer>>,
        settings: Settings,
        size: [u32; 2],
    ) -> Self {
        let shared = Arc::new(Shared {
            published: Mutex::new(Published::default()),
            want: AtomicU64::new(pack(fit(size))),
            paused: AtomicBool::new(false),
        });
        let (commands, rx) = mpsc::channel();
        let host = Host::start(Inputs {
            shared: Arc::clone(&shared),
            device: render_state.device.clone(),
            queue: render_state.queue.clone(),
            pcm,
            commands: rx,
            ctx,
            settings,
        });
        Self {
            shared,
            commands,
            host,
            render_state,
            texture: None,
            seen_generation: 0,
        }
    }

    /// Sends a command to the render loop.
    pub fn send(&self, command: VizCommand) {
        let _ = self.commands.send(command);
    }

    /// Stops rendering while the window is minimized.
    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    /// Tells the loop how many physical pixels the view covers.
    pub fn want_size(&self, size: [u32; 2]) {
        self.shared.want.store(pack(fit(size)), Ordering::Relaxed);
    }

    /// The latest published state; keeps the egui texture pointing at the
    /// current render target. Where the loop has no thread of its own, this
    /// is also what runs it.
    pub fn poll(&mut self) -> Frame {
        self.host.drive();
        let (view, generation, preset, entries, failure) = {
            let published = self
                .shared
                .published
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                (published.generation != self.seen_generation)
                    .then(|| published.view.clone())
                    .flatten(),
                published.generation,
                published.preset.clone(),
                published.entries.clone(),
                published.failure.clone(),
            )
        };
        if let Some(view) = view {
            let mut renderer = self.render_state.renderer.write();
            match self.texture {
                Some(id) => renderer.update_egui_texture_from_wgpu_texture(
                    &self.render_state.device,
                    &view,
                    wgpu::FilterMode::Linear,
                    id,
                ),
                None => {
                    self.texture = Some(renderer.register_native_texture(
                        &self.render_state.device,
                        &view,
                        wgpu::FilterMode::Linear,
                    ));
                }
            }
            self.seen_generation = generation;
        }
        Frame {
            texture: self.texture,
            preset,
            entries,
            failure,
        }
    }

    /// What to remember about this run once it ends.
    pub fn snapshot(&self, mut settings: Settings) -> Settings {
        let published = self
            .shared
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if published.preset.is_some() {
            settings.preset.clone_from(&published.preset);
        }
        if published.entries.is_some() {
            settings.entries.clone_from(&published.entries);
        }
        settings
    }

    /// Stops the render loop and frees the egui texture.
    pub fn stop(&mut self) {
        self.host.stop();
        if let Some(id) = self.texture.take() {
            self.render_state.renderer.write().free_texture(&id);
        }
    }
}

impl Drop for Viz {
    fn drop(&mut self) {
        self.stop();
    }
}

fn pack([w, h]: [u32; 2]) -> u64 {
    u64::from(w) << 32 | u64::from(h)
}

fn unpack(v: u64) -> [u32; 2] {
    [(v >> 32) as u32, v as u32]
}

/// `size` limited to the render budget, snapped to multiples of 16.
fn fit([w, h]: [u32; 2]) -> [u32; 2] {
    let (mut w, mut h) = (w.max(MIN_SIZE[0]), h.max(MIN_SIZE[1]));
    let pixels = u64::from(w) * u64::from(h);
    if pixels > MAX_PIXELS {
        let k = (MAX_PIXELS as f64 / pixels as f64).sqrt();
        w = (f64::from(w) * k) as u32;
        h = (f64::from(h) * k) as u32;
    }
    [
        (w / 16 * 16).max(MIN_SIZE[0]),
        (h / 16 * 16).max(MIN_SIZE[1]),
    ]
}

// -- render thread ---------------------------------------------------------------

struct Target {
    view: wgpu::TextureView,
    size: [u32; 2],
}

impl Target {
    fn new(device: &wgpu::Device, size: [u32; 2]) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aulos visualizer target"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            size,
        }
    }
}

struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    engine: lactea::Engine,
    control: lactea::PresetControl,
    presets: PresetPlaylist,
    entries: Arc<Vec<PresetEntry>>,
    seed: u64,
    current: Option<PathBuf>,
    target: Target,
    started: Instant,
    frame: u32,
    gain: f32,
    /// The audio after `gain`, kept between frames so none allocates.
    scaled: Vec<f32>,
    in_flight: Arc<AtomicU32>,
}

impl Renderer {
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        settings: &Settings,
        size: [u32; 2],
    ) -> Result<Self, String> {
        let seed = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        let mut engine = lactea::Engine::new(
            &device,
            &queue,
            lactea::EngineConfig {
                width: size[0],
                height: size[1],
                seed,
                sample_rate: PCM_SAMPLE_RATE,
                fps: RENDER_FPS,
                preset_duration: PRESET_DURATION_SECONDS,
                // `.milk2` double presets (and `.milk` files using MilkDrop 3
                // features) need `Compat::MilkDrop3`; pick it per preset.
                auto_compat: true,
                ..Default::default()
            },
        )
        .map_err(|e| format!("lactea engine: {e}"))?;
        let mut control = lactea::PresetControl::new(seed);
        control.set_preset_duration(PRESET_DURATION_SECONDS);
        control.set_soft_cut_duration(SOFT_CUT_SECONDS);
        control.set_preset_locked(settings.locked);
        control.set_beat_sensitivity(settings.sensitivity);

        let entries = settings
            .entries
            .clone()
            .unwrap_or_else(|| Arc::new(source::find_presets()));
        engine.set_texture_provider(Box::new(CoreTextures(Textures::find())));

        Ok(Self {
            presets: PresetPlaylist::new(paths(&entries), seed),
            entries,
            seed,
            control,
            target: Target::new(&device, size),
            device,
            queue,
            engine,
            current: None,
            started: Instant::now(),
            frame: 0,
            gain: settings.sensitivity,
            scaled: Vec::new(),
            in_flight: Arc::new(AtomicU32::new(0)),
        })
    }

    /// Puts the first preset on screen: the remembered one if it still loads,
    /// else the next of the rotation, else lactea's idle preset.
    fn start_preset(&mut self, remembered: Option<&Path>) {
        if remembered.is_some_and(|p| self.try_load(p, false)) {
            return;
        }
        self.advance(false);
        if self.current.is_none()
            && let Err(e) = self.engine.load_idle_preset()
        {
            tracing::warn!("lactea: idle preset failed: {e}");
        }
    }

    fn advance(&mut self, smooth: bool) {
        for _ in 0..MAX_SWITCH_ATTEMPTS {
            let Some(path) = self.presets.next_preset().map(Path::to_path_buf) else {
                return;
            };
            if self.try_load(&path, smooth) {
                return;
            }
        }
    }

    fn try_load(&mut self, path: &Path, smooth: bool) -> bool {
        let bytes = match source::read_preset(path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("lactea: cannot read {}: {e}", path.display());
                return false;
            }
        };
        // The full file name: lactea dispatches on the extension.
        let name = path
            .file_name()
            .map_or_else(|| "preset".into(), |s| s.to_string_lossy().into_owned());
        let soft = smooth && self.current.is_some();
        let transition = if soft {
            lactea::Transition::Soft {
                seconds: SOFT_CUT_SECONDS,
            }
        } else {
            lactea::Transition::Cut
        };
        if let Err(e) = self.engine.load_preset(&bytes, &name, transition) {
            tracing::warn!("lactea: cannot load {}: {e}", path.display());
            return false;
        }
        let now = self.engine.last_frame_time();
        if soft {
            self.control.start_smoothing(now);
        } else {
            self.control.start_preset(now);
        }
        self.current = Some(path.to_path_buf());
        true
    }

    fn command(&mut self, command: VizCommand) {
        match command {
            VizCommand::NextPreset => self.advance(true),
            VizCommand::LoadPreset(path) => {
                self.try_load(&path, true);
            }
            VizCommand::SetLocked(locked) => self.control.set_preset_locked(locked),
            VizCommand::SetBeatSensitivity(s) => {
                self.control.set_beat_sensitivity(s);
                self.gain = s.clamp(0.0, 2.0);
            }
            VizCommand::RescanPresets => self.rescan(),
        }
    }

    /// Takes stock of the presets and textures again; the preset on screen
    /// stays, the rotation starts over on the new list.
    fn rescan(&mut self) {
        self.entries = Arc::new(source::find_presets());
        self.presets = PresetPlaylist::new(paths(&self.entries), self.seed);
        self.engine
            .set_texture_provider(Box::new(CoreTextures(Textures::find())));
    }

    /// Moves to `size`; returns whether a new target was made.
    fn resize(&mut self, size: [u32; 2]) -> bool {
        if size == self.target.size {
            return false;
        }
        if let Err(e) = self.engine.resize(size[0], size[1]) {
            tracing::warn!("lactea: resize to {size:?} failed: {e}");
            return false;
        }
        self.target = Target::new(&self.device, size);
        true
    }

    fn render(&mut self, pcm: &[f32]) {
        if !pcm.is_empty() {
            if (self.gain - 1.0).abs() > f32::EPSILON {
                self.scaled.clear();
                self.scaled.extend(pcm.iter().map(|s| s * self.gain));
                self.engine.push_pcm_f32(&self.scaled, 2);
            } else {
                self.engine.push_pcm_f32(pcm, 2);
            }
        }
        let time = self.started.elapsed().as_secs_f64();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.engine.render(
            &mut encoder,
            &self.target.view,
            lactea::FrameClock {
                time,
                frame: self.frame,
            },
        );
        self.frame = self.frame.wrapping_add(1);
        self.queue.submit(Some(encoder.finish()));
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let in_flight = Arc::clone(&self.in_flight);
        self.queue.on_submitted_work_done(move || {
            in_flight.fetch_sub(1, Ordering::AcqRel);
        });

        if let Some(request) = self.control.update(time, self.engine.audio_frame().vol) {
            self.advance(!request.hard_cut);
        }
    }
}

/// The render loop's state: what one thread's locals were when the loop had
/// one. [`Loop::tick`] is one turn of it; the [`Host`] decides when.
struct Loop {
    shared: Arc<Shared>,
    renderer: Renderer,
    commands: mpsc::Receiver<VizCommand>,
    pcm: Arc<Mutex<PcmBuffer>>,
    ctx: egui::Context,
    /// The render size in force.
    size: [u32; 2],
    /// How much of the PCM buffer has been read.
    cursor: u64,
    last_pcm: Instant,
    /// The size the view asked for last, and since when.
    wanted_since: ([u32; 2], Instant),
    /// What the UI was last told about the preset and the list.
    shown: Option<PathBuf>,
    listed: Arc<Vec<PresetEntry>>,
}

impl Loop {
    /// Starts the engine and puts the first preset on screen. When the
    /// engine cannot start, the UI is told why and there is no loop.
    fn new(inputs: Inputs) -> Option<Self> {
        let Inputs {
            shared,
            device,
            queue,
            pcm,
            commands,
            ctx,
            settings,
        } = inputs;
        let size = unpack(shared.want.load(Ordering::Relaxed));
        let mut renderer = match Renderer::new(device, queue, &settings, size) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("lactea: {e}");
                publish(&shared, &ctx, |p| p.failure = Some(e.clone()));
                return None;
            }
        };
        renderer.start_preset(settings.preset.as_deref());
        publish(&shared, &ctx, |p| {
            p.view = Some(renderer.target.view.clone());
            p.size = size;
            p.generation += 1;
            p.entries = Some(Arc::clone(&renderer.entries));
            p.preset.clone_from(&renderer.current);
        });
        Some(Self {
            shown: renderer.current.clone(),
            listed: Arc::clone(&renderer.entries),
            shared,
            renderer,
            commands,
            pcm,
            ctx,
            size,
            cursor: 0,
            last_pcm: Instant::now(),
            wanted_since: (size, Instant::now()),
        })
    }

    /// One turn: commands, resize, then (unless the GPU is behind) the next
    /// frame from the audio that arrived since the last one.
    fn tick(&mut self) {
        // Lets finished-work callbacks run.
        let _ = self.renderer.device.poll(wgpu::PollType::Poll);
        while let Ok(command) = self.commands.try_recv() {
            self.renderer.command(command);
        }
        if self.shared.paused.load(Ordering::Relaxed) {
            return;
        }
        let want = unpack(self.shared.want.load(Ordering::Relaxed));
        if want != self.wanted_since.0 {
            self.wanted_since = (want, Instant::now());
        }
        let mut resized = false;
        if want != self.size && self.wanted_since.1.elapsed() >= RESIZE_SETTLE {
            resized = self.renderer.resize(want);
            if resized || self.renderer.target.size == want {
                self.size = want;
            } else {
                // The old target stays; try again after another settle period.
                self.wanted_since.1 = Instant::now();
            }
        }
        if self.renderer.in_flight.load(Ordering::Acquire) >= MAX_IN_FLIGHT {
            return;
        }

        let fresh = self
            .pcm
            .lock()
            .ok()
            .map(|buf| buf.read_since(&mut self.cursor, PCM_FEED_SAMPLES))
            .unwrap_or_default();
        let samples = if !fresh.is_empty() {
            self.last_pcm = Instant::now();
            fresh
        } else if self.last_pcm.elapsed() >= PCM_STALE_TIMEOUT {
            // Nothing is playing: explicit zeros let the analyser decay to idle.
            vec![0.0; SILENCE_SAMPLES]
        } else {
            Vec::new()
        };
        self.renderer.render(&samples);

        let preset_changed = self.shown != self.renderer.current;
        if preset_changed {
            self.shown.clone_from(&self.renderer.current);
        }
        let list_changed = !Arc::ptr_eq(&self.listed, &self.renderer.entries);
        if list_changed {
            self.listed = Arc::clone(&self.renderer.entries);
        }
        let (renderer, size) = (&self.renderer, self.size);
        publish(&self.shared, &self.ctx, |p| {
            if resized {
                p.view = Some(renderer.target.view.clone());
                p.size = size;
                p.generation += 1;
            }
            if preset_changed {
                p.preset.clone_from(&renderer.current);
            }
            if list_changed {
                p.entries = Some(Arc::clone(&renderer.entries));
            }
        });
    }
}

/// Changes what the UI sees and wakes it.
fn publish(shared: &Shared, ctx: &egui::Context, change: impl FnOnce(&mut Published)) {
    change(
        &mut shared
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    ctx.request_repaint();
}

/// The paths of `entries`, in order: the rotation's items.
fn paths(entries: &[PresetEntry]) -> Vec<PathBuf> {
    entries.iter().map(|e| e.path.clone()).collect()
}

/// The textures of [`source`] as lactea's [`lactea::TextureProvider`].
struct CoreTextures(Textures);

impl lactea::TextureProvider for CoreTextures {
    fn load(&mut self, name: &str) -> Option<lactea::Image> {
        let image = self.0.load(name)?;
        Some(lactea::Image {
            width: image.width,
            height: image.height,
            rgba8: image.rgba8,
        })
    }

    fn random_pool(&self) -> Vec<String> {
        self.0.names()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_snaps_and_bounds_the_render_size() {
        assert_eq!(fit([10, 10]), [320, 180]);
        assert_eq!(fit([1000, 500]), [992, 496]);
        let [w, h] = fit([3840, 2160]);
        assert!(u64::from(w) * u64::from(h) <= MAX_PIXELS);
        assert_eq!([w % 16, h % 16], [0, 0]);
    }

    #[test]
    fn size_packing_round_trips() {
        assert_eq!(unpack(pack([1280, 720])), [1280, 720]);
    }
}
