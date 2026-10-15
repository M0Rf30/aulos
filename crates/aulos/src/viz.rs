//! The MilkDrop visualizer engine (feature `visualizer`).
//!
//! lactea 0.2 renders with a host-provided wgpu 30 device, which is the very
//! one eframe draws with, so a frame never leaves the GPU: a render thread
//! drives [`lactea::Engine`] into one `Rgba8Unorm` texture, and the UI shows
//! that texture through a registered native egui texture (re-pointed in place
//! when the render size changes, never reallocated per frame).
//!
//! The thread exists only while the visualizer is on screen: [`Viz::start`]
//! spawns it, [`Viz::stop`] (or drop) joins it and frees the egui texture.
//! Audio comes from the session's [`PcmBuffer`] (local engine, or the MPD
//! PipeWire capture which the session runs while `set_visualizer_capture` is on).

use aulos_core::player::pcm::PcmBuffer;
use aulos_core::visualizer::playlist::PresetPlaylist;
use aulos_core::visualizer::presets::{PresetEntry, preset_search_dirs, scan_presets};
use aulos_core::visualizer::{PCM_FEED_SAMPLES, VizCommand};
use eframe::egui_wgpu::RenderState;
use eframe::wgpu;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;
use walkdir::WalkDir;
use web_time::Instant;

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
/// Image extensions a preset `sampler_*` texture may use.
const TEXTURE_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "tga", "bmp", "dib"];

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

/// What the render thread tells the UI.
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
    stop: AtomicBool,
    /// Wanted render size, `width << 32 | height`.
    want: AtomicU64,
}

/// A snapshot for drawing, see [`Viz::poll`].
pub struct Frame {
    /// The egui texture holding the latest frame (None before the first one).
    pub texture: Option<egui::TextureId>,
    pub preset: Option<PathBuf>,
    pub entries: Option<Arc<Vec<PresetEntry>>>,
    pub failure: Option<String>,
}

/// UI-side handle of a running visualizer.
pub struct Viz {
    shared: Arc<Shared>,
    commands: mpsc::Sender<VizCommand>,
    thread: Option<JoinHandle<()>>,
    render_state: RenderState,
    texture: Option<egui::TextureId>,
    seen_generation: u64,
}

impl Viz {
    /// Starts the render thread.
    pub fn start(
        render_state: RenderState,
        ctx: egui::Context,
        pcm: Arc<Mutex<PcmBuffer>>,
        settings: Settings,
        size: [u32; 2],
    ) -> Self {
        let shared = Arc::new(Shared {
            published: Mutex::new(Published::default()),
            stop: AtomicBool::new(false),
            want: AtomicU64::new(pack(fit(size))),
        });
        let (commands, rx) = mpsc::channel();
        let thread = {
            let shared = Arc::clone(&shared);
            let (device, queue) = (render_state.device.clone(), render_state.queue.clone());
            std::thread::Builder::new()
                .name("lactea-render".into())
                .spawn(move || run(&shared, device, queue, &pcm, &rx, &ctx, settings))
                .map_err(|e| tracing::error!("lactea: cannot spawn the render thread: {e}"))
                .ok()
        };
        Self {
            shared,
            commands,
            thread,
            render_state,
            texture: None,
            seen_generation: 0,
        }
    }

    /// Sends a command to the render thread.
    pub fn send(&self, command: VizCommand) {
        let _ = self.commands.send(command);
    }

    /// Tells the thread how many physical pixels the view covers.
    pub fn want_size(&self, size: [u32; 2]) {
        self.shared.want.store(pack(fit(size)), Ordering::Relaxed);
    }

    /// The latest published state; keeps the egui texture pointing at the
    /// current render target.
    pub fn poll(&mut self) -> Frame {
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

    /// Stops the render thread and frees the egui texture.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
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
    current: Option<PathBuf>,
    target: Target,
    started: Instant,
    frame: u32,
    gain: f32,
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

        let dirs = preset_search_dirs(None);
        let entries = settings.entries.clone().unwrap_or_else(|| {
            let found = scan_presets(&dirs);
            tracing::info!("lactea: {} presets from {dirs:?}", found.len());
            Arc::new(found)
        });
        let mut texture_roots = dirs.clone();
        texture_roots.extend(
            dirs.iter()
                .filter_map(|d| d.parent().map(|p| p.join("textures"))),
        );
        engine.set_texture_provider(Box::new(DirTextures::scan(&texture_roots)));

        let items = entries.iter().map(|e| e.path.clone()).collect();
        Ok(Self {
            presets: PresetPlaylist::new(items, seed),
            entries,
            control,
            target: Target::new(&device, size),
            device,
            queue,
            engine,
            current: None,
            started: Instant::now(),
            frame: 0,
            gain: settings.sensitivity,
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
        let bytes = match std::fs::read(path) {
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
        }
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
                let scaled: Vec<f32> = pcm.iter().map(|s| s * self.gain).collect();
                self.engine.push_pcm_f32(&scaled, 2);
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

fn run(
    shared: &Shared,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pcm: &Mutex<PcmBuffer>,
    commands: &mpsc::Receiver<VizCommand>,
    ctx: &egui::Context,
    settings: Settings,
) {
    let publish = |f: &mut dyn FnMut(&mut Published)| {
        f(&mut shared
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner));
        ctx.request_repaint();
    };
    let mut size = unpack(shared.want.load(Ordering::Relaxed));
    let mut renderer = match Renderer::new(device, queue, &settings, size) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("lactea: {e}");
            publish(&mut |p| p.failure = Some(e.clone()));
            return;
        }
    };
    renderer.start_preset(settings.preset.as_deref());
    publish(&mut |p| {
        p.view = Some(renderer.target.view.clone());
        p.size = size;
        p.generation += 1;
        p.entries = Some(Arc::clone(&renderer.entries));
        p.preset.clone_from(&renderer.current);
    });

    let mut cursor = 0u64;
    let mut last_pcm = Instant::now();
    let mut wanted_since = (size, Instant::now());
    let mut shown = renderer.current.clone();
    let mut next_frame = Instant::now() + FRAME_PERIOD;
    while !shared.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if let Some(wait) = next_frame.checked_duration_since(now) {
            std::thread::sleep(wait);
            next_frame += FRAME_PERIOD;
        } else {
            next_frame = now + FRAME_PERIOD;
        }
        // Lets finished-work callbacks run.
        let _ = renderer.device.poll(wgpu::PollType::Poll);
        while let Ok(command) = commands.try_recv() {
            renderer.command(command);
        }
        let want = unpack(shared.want.load(Ordering::Relaxed));
        if want != wanted_since.0 {
            wanted_since = (want, Instant::now());
        }
        let mut resized = false;
        if want != size && wanted_since.1.elapsed() >= RESIZE_SETTLE {
            size = want;
            resized = renderer.resize(size);
        }
        if renderer.in_flight.load(Ordering::Acquire) >= MAX_IN_FLIGHT {
            continue;
        }

        let fresh = pcm
            .lock()
            .ok()
            .map(|buf| buf.read_since(&mut cursor, PCM_FEED_SAMPLES))
            .unwrap_or_default();
        let samples = if !fresh.is_empty() {
            last_pcm = Instant::now();
            fresh
        } else if last_pcm.elapsed() >= PCM_STALE_TIMEOUT {
            // Nothing is playing: explicit zeros let the analyser decay to idle.
            vec![0.0; SILENCE_SAMPLES]
        } else {
            Vec::new()
        };
        renderer.render(&samples);

        let preset_changed = shown != renderer.current;
        if preset_changed {
            shown.clone_from(&renderer.current);
        }
        publish(&mut |p| {
            if resized {
                p.view = Some(renderer.target.view.clone());
                p.size = size;
                p.generation += 1;
            }
            if preset_changed {
                p.preset.clone_from(&renderer.current);
            }
        });
    }
}

/// Directory-backed [`lactea::TextureProvider`]: user textures found under
/// the preset search directories (and their sibling `textures` dirs), keyed
/// by lowercase file stem; lactea supplies its own noise textures.
struct DirTextures {
    /// `(lowercase stem, path)`, sorted.
    files: Vec<(String, PathBuf)>,
}

impl DirTextures {
    fn scan(roots: &[PathBuf]) -> Self {
        let mut files = Vec::new();
        for root in roots.iter().filter(|r| r.is_dir()) {
            for entry in WalkDir::new(root).follow_links(true).into_iter().flatten() {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_ascii_lowercase);
                if let (Some(ext), Some(stem)) = (ext, path.file_stem().and_then(|s| s.to_str()))
                    && TEXTURE_EXTENSIONS.contains(&ext.as_str())
                {
                    files.push((stem.to_ascii_lowercase(), path.to_path_buf()));
                }
            }
        }
        files.sort();
        files.dedup();
        tracing::info!("lactea: {} user textures", files.len());
        Self { files }
    }
}

impl lactea::TextureProvider for DirTextures {
    fn load(&mut self, name: &str) -> Option<lactea::Image> {
        let lower = name.to_ascii_lowercase();
        self.files
            .iter()
            .filter(|(stem, _)| *stem == lower)
            .find_map(|(_, path)| {
                let rgba = image::open(path).ok()?.to_rgba8();
                let (width, height) = rgba.dimensions();
                Some(lactea::Image {
                    width,
                    height,
                    rgba8: rgba.into_raw(),
                })
            })
    }

    fn random_pool(&self) -> Vec<String> {
        self.files.iter().map(|(stem, _)| stem.clone()).collect()
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
