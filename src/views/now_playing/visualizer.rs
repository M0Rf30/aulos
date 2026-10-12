// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! lactea visualizer integration (behind `visualizer` feature flag).
//!
//! Provides an offscreen-rendered MilkDrop-style music visualizer using the
//! lactea engine on a private wgpu device. Each frame is rendered to an
//! `Rgba8Unorm` texture, read back into a reusable buffer, and handed as raw
//! RGBA bytes to `viz_shader::VizFrameBuffer`, which the shader widget in
//! `viz_shader.rs` uploads into a single persistent GPU texture every frame —
//! no per-frame `image::Handle` churn.

use super::preset_playlist::PresetPlaylist;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Commands sent from the UI thread to the dedicated lactea render
/// thread (see `lactea_render_stream` in `app.rs`). Drained via
/// `try_recv()` once per frame, before rendering.
#[derive(Debug, Clone)]
pub enum VizCommand {
    /// Advance to the next preset via the shuffled playlist (hard cut).
    /// Replaces the old `preset_signal: AtomicBool` flag.
    NextPreset,
    /// Load a specific preset file directly, outside the shuffled rotation,
    /// with a smooth transition.
    LoadPreset(PathBuf),
    /// Lock/unlock automatic preset transitions (hard/soft cuts driven by
    /// preset duration or beat detection). Manual switches — `NextPreset`
    /// and `LoadPreset` — are always executed regardless of lock state
    /// (per lactea's `PresetControl::set_preset_locked` semantics).
    SetLocked(bool),
    /// Adjust beat-reactivity sensitivity (typical range 0.0-2.0).
    SetBeatSensitivity(f32),
}

/// One `.milk` preset file discovered by `scan_presets`.
///
/// The same stem routinely exists in several category directories (the
/// stock MilkDrop pack ships ~940 duplicated names), so an entry is
/// identified by its full `path`, never by `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetEntry {
    /// File stem (filename without the `.milk` extension) — display only.
    pub name: String,
    /// The preset's immediate parent directory name, prettified by
    /// stripping the MilkDrop pack convention `presets_` prefix (e.g.
    /// `presets_milkdrop` -> `milkdrop`).
    pub category: String,
    /// Full path: the preset's identity. Passed to `VizCommand::LoadPreset`
    /// when selected and compared with the render thread's current preset
    /// to highlight the active row.
    pub path: PathBuf,
    /// Lowercased `"category name"`, matched against the browser's search
    /// tokens (precomputed: the filter runs on every view rebuild).
    pub search_key: String,
}

impl PresetEntry {
    pub fn from_path(path: PathBuf) -> Self {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let category = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| prettify_category(&s.to_string_lossy()))
            .unwrap_or_default();
        let search_key = format!("{category} {name}").to_lowercase();
        Self {
            name,
            category,
            path,
            search_key,
        }
    }
}

/// Returns the full ordered list of preset search directories: the
/// caller-supplied `user_dir` (if any) first, then the common
/// system-wide MilkDrop preset install locations, then the Flatpak location under
/// `dirs::data_dir()`. Shared by `VizRenderer::new` and the UI-side
/// `scan_presets` so both always agree on where presets live.
pub fn preset_search_dirs(user_dir: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = user_dir {
        dirs.push(dir);
    }
    dirs.extend([
        PathBuf::from("/usr/share/lactea/presets"),
        PathBuf::from("/usr/share/projectM/presets"),
        PathBuf::from("/usr/local/share/projectM/presets"),
        PathBuf::from("/usr/share/projectm/presets"),
    ]);
    if let Some(data) = dirs::data_dir() {
        dirs.push(data.join("lactea").join("presets"));
        dirs.push(data.join("projectM").join("presets"));
    }
    dirs
}

/// Strips the MilkDrop pack convention `presets_` prefix from a category
/// directory name (e.g. `presets_milkdrop` -> `milkdrop`); other names are
/// left untouched.
fn prettify_category(raw: &str) -> String {
    raw.strip_prefix("presets_").unwrap_or(raw).to_string()
}

/// Recursively scans `dirs` for `.milk` preset files (case-insensitive
/// extension) off the render thread — no GPU needed.
/// Returns one entry per file, sorted by `(category, name)` and
/// deduplicated by path (in case two search dirs alias the same tree).
pub fn scan_presets(dirs: &[PathBuf]) -> Vec<PresetEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        for entry in WalkDir::new(dir).follow_links(true) {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let is_milk = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("milk"));
            // Non-UTF-8 paths are skipped: preset identity and display
            // names are UTF-8 throughout the UI.
            if !is_milk || path.to_str().is_none() {
                continue;
            }
            let path = path.to_path_buf();
            if !seen.insert(path.clone()) {
                continue;
            }
            entries.push(PresetEntry::from_path(path));
        }
    }
    // Case-insensitive, with the path as a total-order tiebreak so the list
    // (and therefore every row's position) is stable between scans.
    entries.sort_by_cached_key(|e| {
        (
            e.category.to_lowercase(),
            e.name.to_lowercase(),
            e.path.clone(),
        )
    });
    entries
}

/// Render resolution for the visualizer (16:9).
/// Balanced for crispness when scaled to fullscreen vs. GPU→CPU readback cost
/// and texture upload overhead at 30fps.
const RENDER_WIDTH: usize = 960;
const RENDER_HEIGHT: usize = 540;

/// How many broken preset files in a row an automatic/"next" switch skips
/// over before giving up.
const MAX_SWITCH_ATTEMPTS: usize = 5;

/// Length of a soft cut between two presets, in seconds.
const SOFT_CUT_SECONDS: f64 = 3.0;

/// Seconds a preset is shown before the rotation asks for the next one.
const PRESET_DURATION_SECONDS: f64 = 30.0;

/// Frame rate the render loop targets (reported to presets as `fps`).
const RENDER_FPS: u32 = 30;

/// Sample rate PCM is assumed to have for the analyser.
const PCM_SAMPLE_RATE: u32 = 44_100;

/// Interleaved stereo f32 samples read from the PCM ring per render frame:
/// 1152 samples = 576 stereo frames. `Engine::push_pcm_f32` accepts any
/// length, so this is only the read window the render loop asks
/// [`PcmBuffer::read_since`] for.
pub const PCM_FEED_SAMPLES: usize = 576 * 2;

/// Image extensions a preset `sampler_*` texture may use.
const TEXTURE_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "tga", "bmp", "dib"];

/// Directory-backed [`lactea::TextureProvider`]: user textures found under
/// the preset search directories (and their sibling `textures` dirs). Keyed
/// by lowercase file stem; built-in noise textures come from lactea itself.
struct DirTextures {
    /// `(lowercase stem, path)`, sorted.
    files: Vec<(String, PathBuf)>,
}

impl DirTextures {
    fn scan(roots: &[PathBuf]) -> Self {
        let mut files = Vec::new();
        for root in roots {
            if !root.is_dir() {
                continue;
            }
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

/// The offscreen lactea renderer.
///
/// Owns its own wgpu device (a separate wgpu major from the one iced/libcosmic
/// uses, so objects cannot be shared) and a [`lactea::Engine`]. Each frame is
/// rendered to an `Rgba8Unorm` texture, copied to a reusable mapped buffer and
/// handed to the UI as tightly packed RGBA bytes. The engine is not `Send`:
/// construct and use the renderer on the render thread only.
pub struct VizRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    engine: lactea::Engine,
    /// lactea's preset-switch timer (display time, beat-driven hard cuts,
    /// lock, beat sensitivity).
    control: lactea::PresetControl,
    /// Shuffled, path-identified preset rotation (see `preset_playlist`).
    presets: PresetPlaylist,
    /// Preset file currently on screen; see `take_current_preset_change`.
    current_preset: Option<PathBuf>,
    /// `current_preset` changed since `take_current_preset_change` last ran.
    current_changed: bool,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    /// Reused GPU→CPU copy destination (rows padded to 256 bytes).
    readback: wgpu::Buffer,
    padded_bytes_per_row: u32,
    started: std::time::Instant,
    frame: u32,
    /// Reusable double-buffered pixel storage, shared with downstream readers
    /// via `Arc`. A slot is only mutated in place once `Arc::get_mut` proves
    /// it is uniquely owned (no reader still holds it); otherwise a fresh
    /// buffer is allocated for that frame instead of racing a reader.
    pixel_pool: [Arc<Vec<u8>>; 2],
    /// Index of the next pool slot to render into.
    pool_next: usize,
}

/// Selects pool slot `idx` for a fresh readback of `len` bytes.
///
/// Reuses the slot's existing allocation in place when `Arc::get_mut`
/// proves it is uniquely owned (no reader — e.g. a `VizPrimitive`
/// mid-upload — still holds a clone); otherwise a slow downstream
/// consumer still has it, so a brand-new buffer takes its place rather
/// than mutating memory a reader might be reading from.
fn ensure_unique_pool_slot(pool: &mut [Arc<Vec<u8>>; 2], idx: usize, len: usize) -> &mut Vec<u8> {
    if Arc::get_mut(&mut pool[idx]).is_none() {
        pool[idx] = Arc::new(vec![0u8; len]);
    }
    Arc::get_mut(&mut pool[idx])
        .expect("uniquely owned immediately after the check/replacement above")
}

/// Copies the mapped `readback` buffer into `out`, stripping the 256-byte
/// row padding (`pitch` bytes per padded row), and forces alpha to opaque
/// (the frame is composited onto the UI background, where lactea's alpha
/// would show through).
fn read_back(
    device: &wgpu::Device,
    readback: &wgpu::Buffer,
    pitch: usize,
    out: &mut [u8],
) -> Result<(), String> {
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("poll: {e:?}"))?;
    rx.recv()
        .map_err(|e| format!("map: {e}"))?
        .map_err(|e| format!("map: {e}"))?;
    {
        let data = slice
            .get_mapped_range()
            .map_err(|e| format!("mapped range: {e}"))?;
        let row = RENDER_WIDTH * 4;
        for (y, dst) in out.chunks_exact_mut(row).enumerate() {
            dst.copy_from_slice(&data[y * pitch..y * pitch + row]);
            for px in dst.as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
        }
    }
    readback.unmap();
    Ok(())
}

impl VizRenderer {
    /// Create the wgpu device (low-power adapter) and the lactea engine, and
    /// load the first preset of the rotation.
    pub fn new(preset_dir: Option<PathBuf>) -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            ..Default::default()
        }))
        .map_err(|e| format!("no wgpu adapter: {e}"))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|e| format!("request_device: {e}"))?;

        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);

        let mut engine = lactea::Engine::new(
            &device,
            &queue,
            lactea::EngineConfig {
                width: RENDER_WIDTH as u32,
                height: RENDER_HEIGHT as u32,
                seed,
                sample_rate: PCM_SAMPLE_RATE,
                fps: RENDER_FPS,
                preset_duration: PRESET_DURATION_SECONDS,
                ..Default::default()
            },
        );
        let mut control = lactea::PresetControl::new(seed);
        control.set_preset_duration(PRESET_DURATION_SECONDS);
        control.set_soft_cut_duration(SOFT_CUT_SECONDS);

        // Preset directories are shared with the UI-side preset browser via
        // `preset_search_dirs` so both always agree on where presets live.
        let search_dirs = preset_search_dirs(preset_dir);
        let mut texture_roots = search_dirs.clone();
        texture_roots.extend(
            search_dirs
                .iter()
                .filter_map(|d| d.parent().map(|p| p.join("textures"))),
        );
        let textures = DirTextures::scan(&texture_roots);
        tracing::info!("lactea: {} user textures", textures.files.len());
        engine.set_texture_provider(Box::new(textures));

        let items: Vec<PathBuf> = scan_presets(&search_dirs)
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        tracing::info!(
            "lactea: loaded {} presets from {search_dirs:?}",
            items.len()
        );
        if items.is_empty() {
            tracing::warn!("lactea: no presets found in any search directory");
        }

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aulos viz target"),
            size: wgpu::Extent3d {
                width: RENDER_WIDTH as u32,
                height: RENDER_HEIGHT as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let padded_bytes_per_row = (RENDER_WIDTH as u32 * 4)
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aulos viz readback"),
            size: u64::from(padded_bytes_per_row) * RENDER_HEIGHT as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut renderer = Self {
            device,
            queue,
            engine,
            control,
            presets: PresetPlaylist::new(items, seed),
            current_preset: None,
            current_changed: false,
            target,
            target_view,
            readback,
            padded_bytes_per_row,
            started: std::time::Instant::now(),
            frame: 0,
            pixel_pool: [
                Arc::new(vec![0u8; RENDER_WIDTH * RENDER_HEIGHT * 4]),
                Arc::new(vec![0u8; RENDER_WIDTH * RENDER_HEIGHT * 4]),
            ],
            pool_next: 0,
        };
        renderer.next_preset();
        Ok(renderer)
    }

    /// Render one frame and return RGBA pixel bytes (row 0 = top).
    ///
    /// Pushes `pcm` (interleaved stereo) to the analyser, renders into the
    /// offscreen texture and reads it back. The returned bytes are handed
    /// directly to `viz_shader::VizFrameBuffer::update`.
    ///
    /// Reuses one of two pooled buffers instead of allocating per call; see
    /// [`ensure_unique_pool_slot`].
    pub fn render_frame(&mut self, pcm: &[f32]) -> Arc<Vec<u8>> {
        if !pcm.is_empty() {
            self.engine.push_pcm_f32(pcm, 2);
        }

        let time = self.started.elapsed().as_secs_f64();
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.engine.render(
            &mut enc,
            &self.target_view,
            lactea::FrameClock {
                time,
                frame: self.frame,
            },
        );
        self.frame = self.frame.wrapping_add(1);
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(RENDER_HEIGHT as u32),
                },
            },
            wgpu::Extent3d {
                width: RENDER_WIDTH as u32,
                height: RENDER_HEIGHT as u32,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(enc.finish()));

        let request = self.control.update(time, self.engine.audio_frame().vol);

        let idx = self.pool_next;
        self.pool_next = (self.pool_next + 1) % self.pixel_pool.len();
        let pixels =
            ensure_unique_pool_slot(&mut self.pixel_pool, idx, RENDER_WIDTH * RENDER_HEIGHT * 4);
        if let Err(e) = read_back(
            &self.device,
            &self.readback,
            self.padded_bytes_per_row as usize,
            pixels,
        ) {
            tracing::warn!("lactea: readback failed: {e}");
        }

        if let Some(request) = request {
            self.advance(!request.hard_cut);
        }
        Arc::clone(&self.pixel_pool[idx])
    }

    /// Return the render resolution (width, height).
    pub const fn resolution() -> (u32, u32) {
        (RENDER_WIDTH as u32, RENDER_HEIGHT as u32)
    }

    /// Advance to the next preset of the shuffled rotation (hard cut).
    pub fn next_preset(&mut self) {
        self.advance(false);
    }

    /// Load a specific preset file directly, with a soft cut.
    ///
    /// Returns whether lactea accepted the file. On failure the previous
    /// preset keeps playing and stays the tracked current one — nothing else
    /// is substituted behind the user's back.
    pub fn load_preset(&mut self, path: &Path) -> bool {
        self.try_load(path, true)
    }

    /// The preset file most recently *successfully* put on screen (by a
    /// manual load, `next_preset`, or an automatic timer/beat switch), if it
    /// changed since the last call.
    pub fn take_current_preset_change(&mut self) -> Option<PathBuf> {
        if std::mem::take(&mut self.current_changed) {
            self.current_preset.clone()
        } else {
            None
        }
    }

    /// Loads the next presets of the rotation until one is accepted, giving
    /// up after `MAX_SWITCH_ATTEMPTS` broken files in a row.
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
                tracing::warn!("lactea: failed to read preset {}: {e}", path.display());
                return false;
            }
        };
        let name = path
            .file_stem()
            .map_or_else(|| "preset".into(), |s| s.to_string_lossy().into_owned());
        // The very first preset has nothing to blend from.
        let soft = smooth && self.current_preset.is_some();
        let transition = if soft {
            lactea::Transition::Soft {
                seconds: SOFT_CUT_SECONDS,
            }
        } else {
            lactea::Transition::Cut
        };
        if let Err(e) = self.engine.load_preset(&bytes, &name, transition) {
            tracing::warn!("lactea: failed to load preset {}: {e}", path.display());
            return false;
        }
        let now = self.engine.last_frame_time();
        if soft {
            self.control.start_smoothing(now);
        } else {
            self.control.start_preset(now);
        }
        if self.current_preset.as_deref() != Some(path) {
            self.current_preset = Some(path.to_path_buf());
            self.current_changed = true;
        }
        true
    }

    /// Lock/unlock automatic preset transitions. Manual switches (this
    /// renderer's `load_preset`/`next_preset`) always keep working.
    pub fn set_locked(&mut self, locked: bool) {
        self.control.set_preset_locked(locked);
    }

    /// Adjust beat-reactivity sensitivity (clamped to 0..=2 by lactea).
    pub fn set_beat_sensitivity(&mut self, sensitivity: f32) {
        self.control.set_beat_sensitivity(sensitivity);
    }
}

/// Shared PCM ring buffer for audio tapping.
///
/// The writer (the local engine's cpal output callback, or — once wired —
/// an MPD PipeWire capture thread) pushes stereo-interleaved samples in;
/// the visualizer render thread pulls out whatever is new since its own
/// cursor. Only one writer is ever active at a time, but reader and writer
/// always run on different threads, so every access is through a lock
/// (`Arc<std::sync::Mutex<PcmBuffer>>` at the call sites) — this type
/// itself holds no lock.
pub struct PcmBuffer {
    /// Circular buffer of interleaved stereo f32 samples.
    buffer: Vec<f32>,
    /// Write position in the buffer: the index the *next* pushed sample
    /// lands at. Always equal to `total_written % capacity`.
    write_pos: usize,
    /// Total capacity (number of f32 samples, i.e. stereo frames × 2).
    capacity: usize,
    /// Monotonic count of stereo-interleaved f32 samples ever written
    /// (never decremented, never wrapped in practice — at 44.1kHz stereo
    /// this takes millions of years to overflow a `u64`). This is what
    /// lets `read_since` tell "nothing new" apart from "reader fell behind
    /// the ring" without any wraparound ambiguity: it's real-valued,
    /// unlike `write_pos` which is only ever a position mod `capacity`.
    total_written: u64,
}

impl PcmBuffer {
    /// Create a new PCM buffer. `capacity` is in f32 samples
    /// (stereo-interleaved, so `capacity / 2` stereo frames).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            buffer: vec![0.0; capacity],
            write_pos: 0,
            capacity,
            total_written: 0,
        }
    }

    /// Write interleaved `samples` with `channels` channels (`channels`
    /// clamped to a minimum of 1), converting to stereo as they're pushed:
    /// mono is duplicated to L/R, 2-channel passes through unchanged, and
    /// any wider layout keeps only channels 0/1 (front-left/front-right)
    /// and drops the rest.
    ///
    /// Converting at write time — rather than trusting every reader to
    /// know the source channel count — is what lets `render_frame` always
    /// call `push_pcm_f32(.., 2)` correctly regardless of whether the
    /// track underneath is mono, stereo, or multichannel.
    pub fn write_interleaved(&mut self, samples: &[f32], channels: usize) {
        if samples.is_empty() {
            return;
        }
        match channels.max(1) {
            1 => {
                for &s in samples {
                    self.push(s);
                    self.push(s);
                }
            }
            2 => {
                for &s in samples {
                    self.push(s);
                }
            }
            n => {
                for frame in samples.chunks_exact(n) {
                    self.push(frame[0]);
                    self.push(frame[1]);
                }
            }
        }
    }

    #[inline]
    fn push(&mut self, sample: f32) {
        self.buffer[self.write_pos] = sample;
        self.write_pos = (self.write_pos + 1) % self.capacity;
        self.total_written += 1;
    }

    /// Stereo-interleaved samples written since `*cursor`, advancing
    /// `*cursor` to match. Returns at most `max` samples — the most
    /// recent `max` when more than that arrived since the last call (or
    /// when `*cursor` has fallen so far behind the ring that not all of
    /// the gap is still available), never more than the ring's own
    /// `capacity`. Empty when nothing new has arrived since `*cursor`.
    pub fn read_since(&self, cursor: &mut u64, max: usize) -> Vec<f32> {
        let max = max.min(self.capacity);
        let available = self.total_written.saturating_sub(*cursor);
        let count = available.min(max as u64) as usize;
        if count == 0 {
            // Nothing new — but still snap a stale/ahead cursor up to the
            // current write head so the next call's `available` reflects
            // only genuinely new data, not leftover skew.
            *cursor = self.total_written;
            return Vec::new();
        }

        // `write_pos` always equals `total_written % capacity` (they're
        // incremented in lockstep by `push`), so walking back `count`
        // slots from `write_pos` finds the ring position of the oldest
        // sample we're about to return.
        let start_pos = (self.write_pos + self.capacity - count) % self.capacity;
        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            result.push(self.buffer[(start_pos + i) % self.capacity]);
        }
        *cursor = self.total_written;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn write_interleaved_duplicates_mono_to_stereo() {
        let mut buf = PcmBuffer::new(16);
        buf.write_interleaved(&[1.0, 2.0, 3.0], 1);
        let mut cursor = 0u64;
        assert_eq!(
            buf.read_since(&mut cursor, 16),
            vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0]
        );
    }

    #[test]
    fn write_interleaved_passes_stereo_through_unchanged() {
        let mut buf = PcmBuffer::new(16);
        buf.write_interleaved(&[1.0, -1.0, 2.0, -2.0], 2);
        let mut cursor = 0u64;
        assert_eq!(buf.read_since(&mut cursor, 16), vec![1.0, -1.0, 2.0, -2.0]);
    }

    #[test]
    fn write_interleaved_keeps_only_front_left_right_from_multichannel() {
        let mut buf = PcmBuffer::new(16);
        // Two 4-channel frames (FL, FR, RL, RR); only FL/FR should survive.
        buf.write_interleaved(&[1.0, 2.0, 99.0, 99.0, 3.0, 4.0, 99.0, 99.0], 4);
        let mut cursor = 0u64;
        assert_eq!(buf.read_since(&mut cursor, 16), vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn read_since_returns_empty_once_caught_up() {
        let mut buf = PcmBuffer::new(16);
        buf.write_interleaved(&[1.0, 2.0], 2);
        let mut cursor = 0u64;
        assert_eq!(buf.read_since(&mut cursor, 16), vec![1.0, 2.0]);
        assert!(buf.read_since(&mut cursor, 16).is_empty());
    }

    #[test]
    fn read_since_returns_only_samples_written_after_the_cursor() {
        let mut buf = PcmBuffer::new(16);
        let mut cursor = 0u64;
        buf.write_interleaved(&[1.0, 2.0], 2);
        let _ = buf.read_since(&mut cursor, 16);
        buf.write_interleaved(&[3.0, 4.0], 2);
        assert_eq!(buf.read_since(&mut cursor, 16), vec![3.0, 4.0]);
    }

    #[test]
    fn read_since_wraps_around_the_ring() {
        let mut buf = PcmBuffer::new(4); // 2 stereo frames
        let mut cursor = 0u64;
        buf.write_interleaved(&[1.0, 2.0, 3.0, 4.0], 2); // fills exactly
        let _ = buf.read_since(&mut cursor, 4);
        // One more frame wraps, overwriting the oldest.
        buf.write_interleaved(&[5.0, 6.0], 2);
        assert_eq!(buf.read_since(&mut cursor, 4), vec![5.0, 6.0]);
    }

    #[test]
    fn read_since_clamps_a_cursor_that_fell_behind_the_ring() {
        let mut buf = PcmBuffer::new(4);
        // Write far more than the ring holds without ever reading —
        // simulates a reader that stalled for a long time.
        for i in 0..100 {
            buf.write_interleaved(&[i as f32, i as f32], 2);
        }
        let mut cursor = 0u64; // never advanced — badly behind
        let out = buf.read_since(&mut cursor, 4);
        // Only the most recent 2 stereo frames, not all 100.
        assert_eq!(out, vec![98.0, 98.0, 99.0, 99.0]);
        assert_eq!(cursor, 200);
    }

    /// One render period at 30fps/44.1kHz is ~1470 stereo frames: a read
    /// capped at `PCM_FEED_SAMPLES` must hand back a whole 1152-sample
    /// window.
    #[test]
    fn read_since_returns_a_whole_ring_window_for_a_30fps_period() {
        let mut buf = PcmBuffer::new(8192);
        let period: Vec<f32> = (0..2940).map(|i| i as f32).collect();
        buf.write_interleaved(&period, 2);
        let mut cursor = 0u64;
        let out = buf.read_since(&mut cursor, PCM_FEED_SAMPLES);
        assert_eq!(out.len(), PCM_FEED_SAMPLES);
        // Newest window, in order, ending on the last sample written.
        assert_eq!(out[0], (2940 - PCM_FEED_SAMPLES) as f32);
        assert_eq!(*out.last().unwrap(), 2939.0);
    }

    /// A slot with no outstanding reader must be reused in place (same
    /// allocation, no clone/allocation on this call).
    #[test]
    fn ensure_unique_pool_slot_reuses_unshared_buffer() {
        let mut pool: [Arc<Vec<u8>>; 2] = [Arc::new(vec![0u8; 8]), Arc::new(vec![0u8; 8])];
        let original_ptr = Arc::as_ptr(&pool[0]);

        let slot = ensure_unique_pool_slot(&mut pool, 0, 8);
        slot[0] = 0xAB;

        assert_eq!(Arc::as_ptr(&pool[0]), original_ptr);
        assert_eq!(pool[0][0], 0xAB);
    }

    /// A slot still held by a reader (simulated via an extra `Arc` clone,
    /// e.g. a `VizPrimitive` mid-upload) must never be mutated in place —
    /// a fresh buffer is allocated instead, so the writer can never race
    /// that reader, and the reader's snapshot stays untouched.
    #[test]
    fn ensure_unique_pool_slot_replaces_buffer_still_held_by_a_reader() {
        let mut pool: [Arc<Vec<u8>>; 2] = [Arc::new(vec![0u8; 8]), Arc::new(vec![0u8; 8])];
        let reader_snapshot = Arc::clone(&pool[0]);

        let slot = ensure_unique_pool_slot(&mut pool, 0, 8);
        slot[0] = 0xAB;

        // The writer got a distinct allocation from the one the reader
        // still holds, and the reader's bytes are untouched.
        assert!(!Arc::ptr_eq(&reader_snapshot, &pool[0]));
        assert_eq!(reader_snapshot[0], 0);
        assert_eq!(pool[0][0], 0xAB);
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aulos-viz-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Nested dirs, noise files, and a mixed-case extension all in one
    /// tree: only the three `.milk` files should surface, each attributed
    /// to its immediate parent directory (prettified), sorted by
    /// `(category, name)`.
    #[test]
    fn scan_presets_finds_nested_case_insensitive_milk_files_grouped_by_category() {
        let root = temp_root("nested");
        let milkdrop_dir = root.join("presets_milkdrop");
        let nested_dir = milkdrop_dir.join("subdir");
        let stock_dir = root.join("presets_stock");
        fs::create_dir_all(&nested_dir).unwrap();
        fs::create_dir_all(&stock_dir).unwrap();

        fs::write(milkdrop_dir.join("Cool - Preset.milk"), b"").unwrap();
        fs::write(milkdrop_dir.join("noise.txt"), b"").unwrap();
        fs::write(nested_dir.join("Deep.MILK"), b"").unwrap();
        fs::write(stock_dir.join("Basic.milk"), b"").unwrap();
        fs::write(root.join("readme.txt"), b"").unwrap();

        let entries = scan_presets(std::slice::from_ref(&root));

        assert_eq!(
            entries
                .iter()
                .map(|e| (e.category.as_str(), e.name.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("milkdrop", "Cool - Preset"),
                ("stock", "Basic"),
                ("subdir", "Deep"),
            ]
        );

        fs::remove_dir_all(root).unwrap();
    }

    /// The same directory reachable twice in the search-dir list (e.g. a
    /// duplicated config entry) must not double up the preset it contains.
    #[test]
    fn scan_presets_dedupes_when_the_same_dir_is_listed_twice() {
        let root = temp_root("dedup");
        fs::write(root.join("Solo.milk"), b"").unwrap();

        let entries = scan_presets(&[root.clone(), root.clone()]);

        assert_eq!(entries.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preset_entry_from_path_derives_name_category_and_lowercase_key() {
        let e = PresetEntry::from_path(PathBuf::from("/x/presets_milkdrop/Geiss - Eddies 2.milk"));
        assert_eq!(e.name, "Geiss - Eddies 2");
        assert_eq!(e.category, "milkdrop");
        assert_eq!(e.search_key, "milkdrop geiss - eddies 2");
    }

    /// Same stem in two categories stays two distinct, path-identified
    /// entries, and ordering ignores case (`alpha` before `Zeta`).
    #[test]
    fn scan_presets_keeps_duplicate_stems_and_sorts_case_insensitively() {
        let root = temp_root("dupes");
        for dir in ["presets_b", "presets_a"] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join("Same.milk"), b"").unwrap();
        }
        fs::write(root.join("presets_a").join("alpha.milk"), b"").unwrap();
        fs::write(root.join("presets_a").join("Zeta.milk"), b"").unwrap();

        let entries = scan_presets(std::slice::from_ref(&root));

        let order: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| (e.category.as_str(), e.name.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![("a", "alpha"), ("a", "Same"), ("a", "Zeta"), ("b", "Same")]
        );
        assert_ne!(entries[1].path, entries[3].path);
        fs::remove_dir_all(root).unwrap();
    }

    /// Needs a GPU/wgpu adapter and a preset directory, so it is ignored by
    /// default:
    /// `AULOS_TEST_PRESET_DIR=/usr/share/projectM/presets cargo test --features visualizer --lib -- --ignored --nocapture renders_opaque`
    /// Renders a few frames with a sine wave, checks the frame is opaque and
    /// not black, and writes the last frame to `$TMPDIR/aulos-viz-frame.png`.
    #[test]
    #[ignore = "requires a wgpu adapter and a directory of presets"]
    fn renders_opaque_non_black_frames() {
        let dir = std::env::var("AULOS_TEST_PRESET_DIR")
            .unwrap_or_else(|_| "/usr/share/projectM/presets".into());
        let mut r = VizRenderer::new(Some(PathBuf::from(dir))).expect("renderer");
        let first = r.take_current_preset_change().expect("initial preset");
        eprintln!("preset: {}", first.display());
        let pcm: Vec<f32> = (0..1152)
            .map(|i| ((i / 2) as f32 * 0.05).sin() * 0.5)
            .collect();
        let mut frame = r.render_frame(&pcm);
        for _ in 0..60 {
            frame = r.render_frame(&pcm);
        }
        assert_eq!(frame.len(), RENDER_WIDTH * RENDER_HEIGHT * 4);
        assert!(frame.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        let mean = frame
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2]))
            .sum::<f64>()
            / (frame.len() / 4) as f64;
        eprintln!("mean rgb sum {mean}");
        image::save_buffer(
            std::env::temp_dir().join("aulos-viz-frame.png"),
            &frame,
            RENDER_WIDTH as u32,
            RENDER_HEIGHT as u32,
            image::ColorType::Rgba8,
        )
        .unwrap();
        assert!(mean > 5.0, "frame is black (mean {mean})");
        assert!(r.load_preset(&first));
        r.set_locked(true);
        r.set_beat_sensitivity(1.5);
    }
}
