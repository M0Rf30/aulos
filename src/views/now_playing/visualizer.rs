// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! ProjectM visualizer integration (behind `visualizer` feature flag).
//!
//! Provides an offscreen-rendered music visualizer using the projectM library.
//! Renders to an FBO via a headless EGL context, reads pixels back, and hands
//! the raw RGBA bytes to `viz_shader::VizFrameBuffer`, which the shader
//! widget in `viz_shader.rs` uploads into a single persistent GPU texture
//! every frame — no per-frame `image::Handle` churn.

use super::preset_playlist::PresetPlaylist;
use projectm::core::ProjectM;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
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
    /// Load a specific preset file directly, outside the shuffled rotation,
    /// with a smooth transition.
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
///
/// The same stem routinely exists in several category directories (the
/// stock projectM install ships ~940 duplicated names), so an entry is
/// identified by its full `path`, never by `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetEntry {
    /// File stem (filename without the `.milk` extension) — display only.
    pub name: String,
    /// The preset's immediate parent directory name, prettified by
    /// stripping the projectM convention `presets_` prefix (e.g.
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
            // A non-UTF-8 path can't be handed to projectM (its binding takes
            // `&str`), so listing it would only offer a row that never loads.
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
/// over before giving up (matches libprojectM's playlist retry count).
const MAX_SWITCH_ATTEMPTS: usize = 5;

/// Stereo frames in libprojectM's internal PCM ring (`AudioBufferSamples`
/// in `Audio/AudioConstants.hpp`; the C API exposes no getter for it —
/// `projectm_pcm_get_max_samples()` returns the *waveform* length, 480).
/// On every `render_frame` projectM FFTs and beat-detects exactly the most
/// recent ring-full of audio, so that is all that is worth feeding it.
const PROJECTM_PCM_RING_FRAMES: usize = 576;

/// Interleaved stereo f32 samples to hand projectM per render frame: one
/// whole ring window (see [`PROJECTM_PCM_RING_FRAMES`]). The render loop
/// reads at most this many from [`PcmBuffer::read_since`].
///
/// Previously the loop read only `pcm_get_max_samples()` = 480 interleaved
/// samples (240 stereo frames, ~5ms) per ~33ms frame, so projectM analysed
/// a stitched mosaic of 5ms fragments (240 fresh frames + 336 stale ones
/// from the previous frames) instead of a contiguous window — which
/// blurred the spectrum and flattened beat detection.
pub const PCM_FEED_SAMPLES: usize = PROJECTM_PCM_RING_FRAMES * 2;

/// Split one frame's interleaved stereo `pcm` into the pieces to pass to
/// `ProjectM::pcm_add_float`, oldest first.
///
/// projectm-rs asserts every call carries at most
/// `pcm_get_max_samples()` (480) interleaved samples, so a ring-sized
/// window needs several calls. Keeps only the newest [`PCM_FEED_SAMPLES`]
/// (older audio would be overwritten in projectM's ring anyway), drops a
/// dangling half-frame, and rounds the chunk size down to a whole number
/// of stereo frames so L/R never get swapped across a call boundary.
fn pcm_feed_chunks(pcm: &[f32], max_per_call: usize) -> std::slice::Chunks<'_, f32> {
    let whole = &pcm[..pcm.len() & !1];
    let newest = &whole[whole.len().saturating_sub(PCM_FEED_SAMPLES)..];
    newest.chunks((max_per_call & !1).max(2))
}

/// The offscreen projectM renderer.
///
/// Owns a headless EGL/GL context backed by a pbuffer surface and a
/// projectM instance. All rendering happens on a dedicated thread. Frames
/// are read back as RGBA pixels and sent to the UI.
///
/// A pbuffer (not a surfaceless context + FBO) is required: libprojectM 4.1
/// always draws its final, composite-shader output into framebuffer 0
/// (`ProjectM.cpp`: "ToDo: Allow external apps to provide a custom target
/// framebuffer"). Without a default framebuffer that draw is discarded and
/// the readback only sees projectM's pre-composite main texture, so every
/// MilkDrop 2 comp shader appears skipped.
pub struct ProjectMRenderer {
    projectm: ProjectM,
    /// Shuffled, path-identified preset rotation (replaces libprojectM's
    /// playlist — see `preset_playlist`).
    presets: PresetPlaylist,
    /// Preset file currently on screen; see `take_current_preset_change`.
    current_preset: Option<PathBuf>,
    /// `current_preset` changed since `take_current_preset_change` last ran.
    current_changed: bool,
    /// Set by projectM's switch-requested callback (payload: hard cut?),
    /// serviced after the frame in `service_switch_request`.
    switch_request: Rc<Cell<Option<bool>>>,
    /// `(file, message)` pairs reported by projectM's switch-failed callback
    /// since the current load attempt began.
    load_failures: Rc<LoadFailures>,
    /// Pbuffer surface providing framebuffer 0; must outlive rendering.
    _surface: glutin::api::egl::surface::Surface<glutin::surface::PbufferSurface>,
    /// The current GL context; kept alive alongside the surface.
    _context: glutin::api::egl::context::PossiblyCurrentContext,
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

type LoadFailures = RefCell<Vec<(String, String)>>;

/// projectM's switch-failed callback: records `(filename, message)` in the
/// `LoadFailures` that `user_data` points at. Registered through the raw
/// bindings because `projectm`'s own wrapper `unwrap()`s a UTF-8 conversion
/// of both strings inside `extern "C"` — a panic there aborts the process —
/// whereas a preset path or shader log may well not be valid UTF-8.
unsafe extern "C" fn on_preset_switch_failed(
    filename: *const std::os::raw::c_char,
    message: *const std::os::raw::c_char,
    user_data: *mut std::os::raw::c_void,
) {
    let text = |p: *const std::os::raw::c_char| {
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: projectM passes NUL-terminated strings valid for the call.
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned()
        }
    };
    // SAFETY: `user_data` is the pointer `register_load_failure_sink` leaked
    // an `Rc` strong count for, so it outlives every callback invocation;
    // callbacks only run on the render thread that owns the renderer.
    let sink = unsafe { &*user_data.cast::<LoadFailures>() };
    sink.borrow_mut().push((text(filename), text(message)));
}

/// Routes projectM's switch-failed events into `sink`. One `Rc` strong count
/// is leaked per renderer on purpose: the projectM instance is never
/// destroyed either, and the callback must stay valid for its lifetime.
fn register_load_failure_sink(pm: &ProjectM, sink: &Rc<LoadFailures>) {
    let handle = *pm.get_instance().borrow();
    // SAFETY: `handle` is the live projectM instance owned by `pm`.
    unsafe {
        projectm_sys::projectm_set_preset_switch_failed_event_callback(
            handle,
            Some(on_preset_switch_failed),
            Rc::into_raw(Rc::clone(sink)).cast_mut().cast(),
        );
    }
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
        use glutin::context::{ContextApi, ContextAttributesBuilder, NotCurrentGlContext};
        use glutin::display::GlDisplay;
        use glutin::surface::{PbufferSurface, SurfaceAttributesBuilder};
        use std::num::NonZeroU32;

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

        // Configure for pbuffer offscreen rendering (see struct docs: projectM
        // needs a real framebuffer 0 for its final composite pass).
        let template = ConfigTemplateBuilder::new()
            .with_alpha_size(8)
            .with_surface_type(ConfigSurfaceTypes::PBUFFER)
            .build();

        let config = unsafe {
            display
                .find_configs(template)
                .map_err(|e| format!("Failed to find EGL configs: {e}"))?
                .next()
                .ok_or("No suitable EGL pbuffer config found")?
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

        let surface_attrs = SurfaceAttributesBuilder::<PbufferSurface>::new().build(
            NonZeroU32::new(RENDER_WIDTH as u32).expect("non-zero width"),
            NonZeroU32::new(RENDER_HEIGHT as u32).expect("non-zero height"),
        );
        let surface = unsafe {
            display
                .create_pbuffer_surface(&config, &surface_attrs)
                .map_err(|e| format!("Failed to create EGL pbuffer surface: {e}"))?
        };

        let context = context
            .make_current(&surface)
            .map_err(|e| format!("Failed to make context current: {e}"))?;

        // Load GL function pointers
        gl::load_with(|symbol| {
            let cstr = std::ffi::CString::new(symbol).unwrap();
            display.get_proc_address(cstr.as_c_str()) as *const _
        });

        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::Viewport(0, 0, RENDER_WIDTH as i32, RENDER_HEIGHT as i32);
        }

        // Initialize projectM
        let pm = ProjectM::create();
        pm.set_window_size(RENDER_WIDTH, RENDER_HEIGHT);
        pm.set_fps(30);

        // Search for preset directories in common locations — shared with
        // the UI-side preset browser via `preset_search_dirs` so both
        // always agree on where presets live.
        let search_dirs = preset_search_dirs(preset_dir.clone());

        let mut texture_paths = Vec::new();
        for dir in &search_dirs {
            if dir.exists() {
                texture_paths.push(dir.to_string_lossy().to_string());
            }
        }
        if !texture_paths.is_empty() {
            pm.set_texture_search_paths(&texture_paths, texture_paths.len());
        }

        let switch_request = Rc::new(Cell::new(None));
        {
            let switch_request = Rc::clone(&switch_request);
            pm.set_preset_switch_requested_event_callback(move |hard_cut| {
                switch_request.set(Some(hard_cut));
            });
        }
        let load_failures = Rc::new(RefCell::new(Vec::new()));
        register_load_failure_sink(&pm, &load_failures);

        let items: Vec<PathBuf> = scan_presets(&search_dirs)
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        tracing::info!(
            "ProjectM: loaded {} presets from {search_dirs:?}",
            items.len()
        );
        if items.is_empty() {
            tracing::warn!("ProjectM: no presets found in any search directory");
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);

        let mut renderer = Self {
            projectm: pm,
            presets: PresetPlaylist::new(items, seed),
            current_preset: None,
            current_changed: false,
            switch_request,
            load_failures,
            _surface: surface,
            _context: context,
            _gl_ready: true,
            pixel_pool: [
                Arc::new(vec![0u8; RENDER_WIDTH * RENDER_HEIGHT * 4]),
                Arc::new(vec![0u8; RENDER_WIDTH * RENDER_HEIGHT * 4]),
            ],
            pool_next: 0,
        };
        renderer.next_preset();
        Ok(renderer)
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
        // Feed the newest ring-full of audio (projectM caps a single call at
        // `pcm_get_max_samples()`, so it goes in a few chunks; see
        // `pcm_feed_chunks`).
        if !pcm.is_empty() {
            let max = ProjectM::pcm_get_max_samples() as usize;
            for chunk in pcm_feed_chunks(pcm, max) {
                self.projectm.pcm_add_float(chunk, projectm::core::STEREO);
            }
        }

        // Render the visualization. projectM draws its final composite into
        // framebuffer 0 (our pbuffer).
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::Viewport(0, 0, RENDER_WIDTH as i32, RENDER_HEIGHT as i32);
        }
        self.projectm.render_frame();
        self.service_switch_request();

        let idx = self.pool_next;
        self.pool_next = (self.pool_next + 1) % self.pixel_pool.len();

        let pixels =
            ensure_unique_pool_slot(&mut self.pixel_pool, idx, RENDER_WIDTH * RENDER_HEIGHT * 4);

        // Read pixels from the FBO
        unsafe {
            gl::Finish();
            // projectM leaves its internal FBOs bound for reading; read the
            // composited output from framebuffer 0 instead.
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::PixelStorei(gl::PACK_ALIGNMENT, 1);
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

    /// Advance to the next preset of the shuffled rotation (hard cut).
    pub fn next_preset(&mut self) {
        self.advance(false);
    }

    /// Load a specific preset file directly, with a smooth transition.
    ///
    /// Returns whether projectM accepted the file. On failure the previous
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

    /// Performs the automatic preset switch projectM asked for during the
    /// last `render_frame` (preset timer elapsed, or a beat-driven hard
    /// cut). Deferred out of the callback itself because projectM invokes it
    /// from inside `RenderFrame`.
    fn service_switch_request(&mut self) {
        if let Some(hard_cut) = self.switch_request.take() {
            self.advance(!hard_cut);
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
        self.load_failures.borrow_mut().clear();
        // `ProjectM::load_preset_file` hands the `&str`'s raw pointer to C,
        // which needs a NUL terminator that a Rust `str` does not have —
        // without one projectM reads past the path into whatever heap bytes
        // follow and (usually) fails to open the file. Pass a slice of a
        // buffer that is NUL-terminated right after the slice end.
        let mut c_path = path.to_string_lossy().into_owned();
        c_path.push('\0');
        self.projectm
            .load_preset_file(&c_path[..c_path.len() - 1], smooth);

        let failures = std::mem::take(&mut *self.load_failures.borrow_mut());
        if let Some((file, message)) = failures.first() {
            tracing::warn!("ProjectM: failed to load preset {file}: {message}");
            return false;
        }
        if self.current_preset.as_deref() != Some(path) {
            self.current_preset = Some(path.to_path_buf());
            self.current_changed = true;
        }
        true
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

    /// One render period at 30fps/44.1kHz is ~1470 stereo frames: a read
    /// capped at `PCM_FEED_SAMPLES` must hand back a whole projectM ring
    /// window (1152 samples), not the 480 `pcm_get_max_samples()` allows
    /// per `pcm_add_float` call.
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

    #[test]
    fn pcm_feed_chunks_splits_in_order_within_the_call_limit() {
        let pcm: Vec<f32> = (0..1152).map(|i| i as f32).collect();
        let chunks: Vec<&[f32]> = pcm_feed_chunks(&pcm, 480).collect();
        assert_eq!(
            chunks.iter().map(|c| c.len()).collect::<Vec<_>>(),
            vec![480, 480, 192]
        );
        let rejoined: Vec<f32> = chunks.concat();
        assert_eq!(rejoined, pcm);
    }

    #[test]
    fn pcm_feed_chunks_keeps_only_the_newest_ring_window() {
        let pcm: Vec<f32> = (0..4000).map(|i| i as f32).collect();
        let rejoined: Vec<f32> = pcm_feed_chunks(&pcm, 480).flatten().copied().collect();
        assert_eq!(rejoined.len(), PCM_FEED_SAMPLES);
        assert_eq!(rejoined[0], (4000 - PCM_FEED_SAMPLES) as f32);
        assert_eq!(*rejoined.last().unwrap(), 3999.0);
    }

    /// Chunk boundaries must always fall between stereo frames (even
    /// lengths) so L/R never swap, whatever the per-call limit, and a
    /// dangling half-frame is dropped.
    #[test]
    fn pcm_feed_chunks_never_splits_a_stereo_frame() {
        let pcm: Vec<f32> = (0..1001).map(|i| i as f32).collect(); // odd length
        for max in [1usize, 2, 3, 479, 480, 481] {
            let chunks: Vec<&[f32]> = pcm_feed_chunks(&pcm, max).collect();
            assert!(chunks.iter().all(|c| c.len() % 2 == 0), "max={max}");
            assert!(chunks.iter().all(|c| c.len() <= max.max(2)), "max={max}");
            let total: usize = chunks.iter().map(|c| c.len()).sum();
            assert_eq!(total, 1000, "max={max}");
        }
        assert_eq!(pcm_feed_chunks(&[], 480).count(), 0);
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

    /// Regression: the readback must contain projectM's *composited* output
    /// (comp shader applied), not its pre-composite main texture. Needs a GPU
    /// / EGL, so it is ignored by default:
    /// `AULOS_TEST_PRESET=path.milk cargo test --features visualizer -- --ignored comp_shader`
    /// Writes the last frame to `$TMPDIR/aulos-viz-comp.png` for inspection.
    #[test]
    #[ignore = "requires EGL/GPU and a MilkDrop 2 preset with a comp shader"]
    fn comp_shader_output_is_read_back() {
        let Ok(preset) = std::env::var("AULOS_TEST_PRESET") else {
            eprintln!("AULOS_TEST_PRESET not set; skipping");
            return;
        };
        let mut r = ProjectMRenderer::new(None).expect("renderer");
        r.set_locked(true);
        r.load_preset(Path::new(&preset));
        let pcm: Vec<f32> = (0..1470)
            .flat_map(|i| {
                let s = (i as f32 * 0.05).sin() * 0.5;
                [s, s]
            })
            .collect();
        let mut last = Arc::new(Vec::new());
        for _ in 0..180 {
            last = r.render_frame(&pcm);
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
        let img = image::RgbaImage::from_raw(RENDER_WIDTH as u32, RENDER_HEIGHT as u32, (*last).clone())
            .expect("frame size");
        img.save(std::env::temp_dir().join("aulos-viz-comp.png")).unwrap();
        // A blank/garbage frame would be (near) uniform; a composited one is not.
        let lum: Vec<u32> = last.chunks(4).map(|p| p[0] as u32 + p[1] as u32 + p[2] as u32).collect();
        let mean = lum.iter().sum::<u32>() as f64 / lum.len() as f64;
        assert!(mean > 5.0, "frame is black (mean {mean})");
    }

    /// Regression: `ProjectM::load_preset_file` passes `str::as_ptr()` to C
    /// without a NUL terminator, so loading by path read garbage after the
    /// filename and often failed. Compares the raw wrapper call against
    /// `ProjectMRenderer::load_preset` over many presets:
    /// `AULOS_TEST_PRESET_DIR=/usr/share/projectM/presets cargo test --features visualizer --lib -- --ignored --nocapture nul_terminated`
    #[test]
    #[ignore = "requires EGL/GPU and a directory of presets"]
    fn preset_paths_are_passed_nul_terminated() {
        let dir = std::env::var("AULOS_TEST_PRESET_DIR")
            .unwrap_or_else(|_| "/usr/share/projectM/presets".into());
        let entries = scan_presets(&[PathBuf::from(dir)]);
        let sample: Vec<PathBuf> = entries
            .iter()
            .step_by(7)
            .take(150)
            .map(|e| e.path.clone())
            .collect();
        if sample.is_empty() {
            eprintln!("no presets found; skipping");
            return;
        }
        let mut r = ProjectMRenderer::new(None).expect("renderer");

        for path in &sample {
            // Exactly what `load_preset` did before the fix.
            r.projectm.load_preset_file(&path.to_string_lossy(), false);
        }
        let raw_failures = std::mem::take(&mut *r.load_failures.borrow_mut()).len();

        let fixed_failures = sample.iter().filter(|p| !r.load_preset(p)).count();
        eprintln!(
            "{} presets: raw wrapper call failed {raw_failures}, NUL-terminated call failed {fixed_failures}",
            sample.len(),
        );
        assert!(fixed_failures <= raw_failures);
    }

    /// The renderer must always report exactly the preset on screen: the
    /// initial one, a manual pick, and an automatic timer switch — while a
    /// failed load changes nothing:
    /// `cargo test --features visualizer --lib -- --ignored --nocapture auto_switch`
    #[test]
    #[ignore = "requires EGL/GPU and a directory of presets"]
    fn auto_switch_and_failed_load_keep_current_preset_exact() {
        let dir = std::env::var("AULOS_TEST_PRESET_DIR")
            .unwrap_or_else(|_| "/usr/share/projectM/presets".into());
        let entries = scan_presets(&[PathBuf::from(dir)]);
        if entries.len() < 3 {
            eprintln!("need >= 3 presets; skipping");
            return;
        }
        let mut r = ProjectMRenderer::new(None).expect("renderer");
        let first = r.take_current_preset_change().expect("initial preset");
        assert!(r.take_current_preset_change().is_none());

        // Applies to presets started after this point (the duration is
        // sampled when a preset begins), i.e. the manual pick below.
        r.projectm.set_preset_duration(1.0);
        let manual = entries[0].path.clone();
        assert!(r.load_preset(&manual));
        assert_eq!(r.take_current_preset_change(), Some(manual.clone()));

        assert!(!r.load_preset(Path::new("/nonexistent/Missing.milk")));
        assert_eq!(
            r.take_current_preset_change(),
            None,
            "failed load must not change current"
        );

        let mut auto = None;
        for _ in 0..300 {
            r.render_frame(&[]);
            std::thread::sleep(std::time::Duration::from_millis(33));
            if let Some(p) = r.take_current_preset_change() {
                auto = Some(p);
                break;
            }
        }
        let auto = auto.expect("timer switch should have been reported");
        eprintln!("first {first:?}\nmanual {manual:?}\nauto {auto:?}");
        assert_ne!(auto, manual);
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
}
