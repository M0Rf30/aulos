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

use aulos_core::visualizer::playlist::PresetPlaylist;
use aulos_core::visualizer::presets::{preset_search_dirs, scan_presets};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

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
                // Per-preset compat detection: `.milk2` double presets (and
                // `.milk` files using MilkDrop 3 features) need
                // `Compat::MilkDrop3`, which the default projectM mode
                // rejects with `EngineError::Unsupported`.
                auto_compat: true,
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
        // The full file name, not the stem: lactea dispatches on the
        // extension (`.milk` = plain preset, otherwise a `.milk2` marker scan).
        let name = path
            .file_name()
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

#[cfg(test)]
mod tests {
    use super::*;

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
