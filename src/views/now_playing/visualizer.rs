// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! ProjectM visualizer integration (behind `visualizer` feature flag).
//!
//! Provides an offscreen-rendered music visualizer using the projectM library.
//! Renders to an FBO via a headless EGL context, reads pixels back, and hands
//! the raw RGBA bytes to `viz_shader::VizFrameBuffer`, which the shader
//! widget in `viz_shader.rs` uploads into a single persistent GPU texture
//! every frame — no per-frame `image::Handle` churn.

use projectm::core::ProjectM;
use projectm::playlist::Playlist;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Commands sent from the UI thread to the dedicated projectM render
/// thread (see `projectm_render_stream` in `app.rs`). Drained via
/// `try_recv()` once per frame, before rendering.
#[derive(Debug, Clone)]
pub enum VizCommand {
    /// Advance to the next preset via the shuffled playlist (hard cut).
    /// Replaces the old `preset_signal: AtomicBool` flag.
    NextPreset,
    /// Load a specific preset file directly, bypassing the playlist, with
    /// a smooth transition.
    LoadPreset(PathBuf),
    /// Lock/unlock automatic preset transitions (hard/soft cuts driven by
    /// preset duration or beat detection). Manual switches — `NextPreset`
    /// and `LoadPreset` — are always executed regardless of lock state
    /// (per libprojectM's `projectm_set_preset_locked` semantics).
    SetLocked(bool),
    /// Adjust beat-reactivity sensitivity (typical range 0.0-2.0).
    SetBeatSensitivity(f32),
}

/// One `.milk` preset file discovered by `scan_presets`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetEntry {
    /// File stem (filename without the `.milk` extension) — the display
    /// name, matched against the render thread's tracked current-preset
    /// name to highlight the active row.
    pub name: String,
    /// The preset's immediate parent directory name, prettified by
    /// stripping the projectM convention `presets_` prefix (e.g.
    /// `presets_milkdrop` -> `milkdrop`).
    pub category: String,
    /// Full path, passed to `VizCommand::LoadPreset` when selected.
    pub path: PathBuf,
}

/// Returns the full ordered list of preset search directories: the
/// caller-supplied `user_dir` (if any) first, then projectM's common
/// system-wide install locations, then the Flatpak location under
/// `dirs::data_dir()`. Shared by `ProjectMRenderer::new` and the UI-side
/// `scan_presets` so both always agree on where presets live.
pub fn preset_search_dirs(user_dir: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = user_dir {
        dirs.push(dir);
    }
    dirs.extend([
        PathBuf::from("/usr/share/projectM/presets"),
        PathBuf::from("/usr/local/share/projectM/presets"),
        PathBuf::from("/usr/share/projectm/presets"),
    ]);
    if let Some(data) = dirs::data_dir() {
        dirs.push(data.join("projectM").join("presets"));
    }
    dirs
}

/// Strips the projectM convention `presets_` prefix from a category
/// directory name (e.g. `presets_milkdrop` -> `milkdrop`); other names are
/// left untouched.
fn prettify_category(raw: &str) -> String {
    raw.strip_prefix("presets_").unwrap_or(raw).to_string()
}

/// Recursively scans `dirs` for `.milk` preset files (case-insensitive
/// extension) off the render thread — no `ProjectM`/GL context needed.
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
            if !is_milk {
                continue;
            }
            let path = path.to_path_buf();
            if !seen.insert(path.clone()) {
                continue;
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let category = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| prettify_category(&s.to_string_lossy()))
                .unwrap_or_default();
            entries.push(PresetEntry {
                name,
                category,
                path,
            });
        }
    }
    entries.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
    entries
}

/// Render resolution for the visualizer (16:9).
/// Balanced for crispness when scaled to fullscreen vs. GPU→CPU readback cost
/// and texture upload overhead at 30fps.
const RENDER_WIDTH: usize = 960;
const RENDER_HEIGHT: usize = 540;

/// The offscreen projectM renderer.
///
/// Owns a headless EGL/GL context, a projectM instance, and an FBO.
/// All rendering happens on a dedicated thread. Frames are read back
/// as RGBA pixels and sent to the UI.
pub struct ProjectMRenderer {
    projectm: ProjectM,
    /// Playlist manager for preset cycling.
    playlist: Option<Playlist>,
    /// OpenGL framebuffer object for offscreen rendering.
    _fbo: u32,
    /// OpenGL renderbuffer for color attachment.
    _color_rb: u32,
    /// Whether we successfully set up the GL context.
    _gl_ready: bool,
    /// Reusable double-buffered pixel storage for `render_frame`'s GL
    /// readback, shared with downstream readers via `Arc`. A slot is only
    /// mutated in place once `Arc::get_mut` proves it is uniquely owned
    /// (no reader still holds it); otherwise a fresh buffer is allocated
    /// for that frame instead of racing a reader.
    pixel_pool: [Arc<Vec<u8>>; 2],
    /// Index of the next pool slot to render into.
    pool_next: usize,
}

/// Selects pool slot `idx` for a fresh GL readback of `len` bytes.
///
/// Reuses the slot's existing allocation in place when `Arc::get_mut`
/// proves it is uniquely owned (no reader — e.g. a `VizPrimitive`
/// mid-upload — still holds a clone); otherwise a slow downstream
/// consumer still has it, so a brand-new buffer takes its place rather
/// than mutating memory a reader might be reading from. This is the
/// double-buffering safety net that lets `render_frame` avoid a fresh
/// allocation on the common path without ever racing a reader.
fn ensure_unique_pool_slot(pool: &mut [Arc<Vec<u8>>; 2], idx: usize, len: usize) -> &mut Vec<u8> {
    if Arc::get_mut(&mut pool[idx]).is_none() {
        pool[idx] = Arc::new(vec![0u8; len]);
    }
    Arc::get_mut(&mut pool[idx])
        .expect("uniquely owned immediately after the check/replacement above")
}

// SAFETY: ProjectM implements Send + Sync. The GL context is only used
// from the renderer thread.
unsafe impl Send for ProjectMRenderer {}

impl ProjectMRenderer {
    /// Create a new offscreen projectM renderer.
    ///
    /// Sets up a headless EGL context, creates an FBO, and initializes
    /// projectM with the given preset directory.
    pub fn new(preset_dir: Option<PathBuf>) -> Result<Self, String> {
        // --- EGL device-based headless context ---
        use glutin::api::egl::device::Device;
        use glutin::api::egl::display::Display;
        use glutin::config::{ConfigSurfaceTypes, ConfigTemplateBuilder};
        use glutin::context::{ContextApi, ContextAttributesBuilder};
        use glutin::display::GlDisplay;

        // Query EGL devices
        let devices: Vec<_> = Device::query_devices()
            .map_err(|e| format!("Failed to query EGL devices: {e}"))?
            .collect();

        if devices.is_empty() {
            return Err("No EGL devices available for headless rendering".to_string());
        }

        let device = &devices[0];

        // Create display from device (no windowing system)
        let display = unsafe {
            Display::with_device(device, None)
                .map_err(|e| format!("Failed to create EGL display: {e}"))?
        };

        // Configure for surfaceless offscreen rendering
        let template = ConfigTemplateBuilder::new()
            .with_alpha_size(8)
            .with_depth_size(24)
            .with_surface_type(ConfigSurfaceTypes::empty())
            .build();

        let config = unsafe {
            display
                .find_configs(template)
                .map_err(|e| format!("Failed to find EGL configs: {e}"))?
                .next()
                .ok_or("No suitable EGL config found")?
        };

        // Create context (OpenGL, no window handle)
        let context_attrs = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(None))
            .build(None);

        let context = unsafe {
            display
                .create_context(&config, &context_attrs)
                .map_err(|e| format!("Failed to create GL context: {e}"))?
        };

        // Make current without a surface (surfaceless)
        let _context = context
            .make_current_surfaceless()
            .map_err(|e| format!("Failed to make context current (surfaceless): {e}"))?;

        // Load GL function pointers
        gl::load_with(|symbol| {
            let cstr = std::ffi::CString::new(symbol).unwrap();
            display.get_proc_address(cstr.as_c_str()) as *const _
        });

        // Create FBO + renderbuffer for offscreen rendering
        let (fbo, color_rb) = unsafe {
            let mut fbo = 0u32;
            let mut rb = 0u32;
            gl::GenFramebuffers(1, &mut fbo);
            gl::GenRenderbuffers(1, &mut rb);

            gl::BindFramebuffer(gl::FRAMEBUFFER, fbo);
            gl::BindRenderbuffer(gl::RENDERBUFFER, rb);
            gl::RenderbufferStorage(
                gl::RENDERBUFFER,
                gl::RGBA8,
                RENDER_WIDTH as i32,
                RENDER_HEIGHT as i32,
            );
            gl::FramebufferRenderbuffer(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::RENDERBUFFER,
                rb,
            );

            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
            if status != gl::FRAMEBUFFER_COMPLETE {
                return Err(format!("Framebuffer incomplete: {status:#x}"));
            }

            gl::Viewport(0, 0, RENDER_WIDTH as i32, RENDER_HEIGHT as i32);

            (fbo, rb)
        };

        // Initialize projectM
        let pm = ProjectM::create();
        pm.set_window_size(RENDER_WIDTH, RENDER_HEIGHT);
        pm.set_fps(30);

        // Search for preset directories in common locations — shared with
        // the UI-side preset browser via `preset_search_dirs` so both
        // always agree on where presets live.
        let search_dirs = preset_search_dirs(preset_dir.clone());

        let mut pl = Playlist::create(&pm);
        let mut texture_paths = Vec::new();
        for dir in &search_dirs {
            if dir.exists() {
                let dir_str = dir.to_string_lossy().to_string();
                pl.add_path(&dir_str, true);
                texture_paths.push(dir_str);
            }
        }
        if !texture_paths.is_empty() {
            pm.set_texture_search_paths(&texture_paths, texture_paths.len());
        }

        let count = pl.len();
        tracing::info!("ProjectM: loaded {count} presets from {search_dirs:?}");
        let playlist = if count > 0 {
            pl.set_shuffle(true);
            pl.play_next();
            Some(pl)
        } else {
            tracing::warn!("ProjectM: no presets found in any search directory");
            None
        };

        Ok(Self {
            projectm: pm,
            playlist,
            _fbo: fbo,
            _color_rb: color_rb,
            _gl_ready: true,
            pixel_pool: [
                Arc::new(vec![0u8; RENDER_WIDTH * RENDER_HEIGHT * 4]),
                Arc::new(vec![0u8; RENDER_WIDTH * RENDER_HEIGHT * 4]),
            ],
            pool_next: 0,
        })
    }

    /// Render one frame and return RGBA pixel bytes.
    ///
    /// Feed PCM audio data to projectM, render a frame into the FBO,
    /// and read pixels back. The returned bytes are handed directly to
    /// `viz_shader::VizFrameBuffer::update` — no PNG encoding needed.
    ///
    /// Reuses one of two pooled buffers for the GL readback instead of
    /// allocating a fresh one every call: a slot is reused in place when
    /// `Arc::get_mut` proves no reader still holds it, otherwise (a slow
    /// downstream consumer) a fresh buffer is allocated just for that
    /// frame so the readback never aliases memory a reader is using.
    pub fn render_frame(&mut self, pcm: &[f32]) -> Arc<Vec<u8>> {
        // Feed audio samples if available, clamped to projectM's max buffer size
        if !pcm.is_empty() {
            let max = ProjectM::pcm_get_max_samples() as usize;
            let clamped = if pcm.len() > max { &pcm[..max] } else { pcm };
            self.projectm.pcm_add_float(clamped, projectm::core::STEREO);
        }

        // Render the visualization
        self.projectm.render_frame();

        let idx = self.pool_next;
        self.pool_next = (self.pool_next + 1) % self.pixel_pool.len();

        let pixels =
            ensure_unique_pool_slot(&mut self.pixel_pool, idx, RENDER_WIDTH * RENDER_HEIGHT * 4);

        // Read pixels from the FBO
        unsafe {
            gl::Finish();
            gl::ReadPixels(
                0,
                0,
                RENDER_WIDTH as i32,
                RENDER_HEIGHT as i32,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                pixels.as_mut_ptr() as *mut std::ffi::c_void,
            );
        }

        // OpenGL reads bottom-to-top — flip vertically in-place
        let row_size = RENDER_WIDTH * 4;
        for y in 0..RENDER_HEIGHT / 2 {
            let top = y * row_size;
            let bot = (RENDER_HEIGHT - 1 - y) * row_size;
            // Swap rows using split_at_mut to satisfy borrow checker
            let (first, second) = pixels.split_at_mut(bot);
            first[top..top + row_size].swap_with_slice(&mut second[..row_size]);
        }

        // Force alpha to fully opaque. projectM often renders with alpha < 255
        // which causes washed-out/transparent-looking colors when the image is
        // composited onto the UI background.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }

        Arc::clone(&self.pixel_pool[idx])
    }

    /// Return the render resolution (width, height).
    pub const fn resolution() -> (u32, u32) {
        (RENDER_WIDTH as u32, RENDER_HEIGHT as u32)
    }

    /// Switch to the next preset via the playlist (hard cut).
    pub fn next_preset(&mut self) {
        if let Some(ref mut pl) = self.playlist {
            pl.play_next();
        }
    }

    /// Load a specific preset file directly, bypassing the playlist, with
    /// a smooth transition.
    pub fn load_preset(&mut self, path: &Path) {
        self.projectm
            .load_preset_file(&path.to_string_lossy(), true);
    }

    /// Lock/unlock automatic preset transitions. Manual switches (this
    /// renderer's `load_preset`/`next_preset`) always keep working.
    pub fn set_locked(&self, locked: bool) {
        self.projectm.set_preset_locked(locked);
    }

    /// Adjust beat-reactivity sensitivity.
    pub fn set_beat_sensitivity(&self, sensitivity: f32) {
        self.projectm.set_beat_sensitivity(sensitivity);
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
    /// call `projectm::core::STEREO` correctly regardless of whether the
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
            "lyra-viz-{label}-{}-{}",
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
}
