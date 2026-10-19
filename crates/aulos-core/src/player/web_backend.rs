// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The browser's local playback backend (replaces `local_backend.rs`, which
//! drives the cpal engine, on wasm): Web Audio.
//!
//! A track's bytes are read whole (a virtual-path file through
//! [`crate::web_fs`], an HTTP track with `fetch`), decoded with the same
//! symphonia decoder the native engine uses, and played as a stream of short
//! `AudioBuffer` chunks that are scheduled back to back on the shared
//! `AudioContext` timeline, a few seconds ahead of the clock. The whole track
//! is never expanded to PCM. Behind it the graph is
//! `sources -> 10 peaking filters (the equaliser) -> gain (volume) -> output`.
//!
//! Everything runs on the page's single thread. [`LocalBackend`] only holds a
//! `SendWrapper` around the shared state (the trait requires `Send`), a
//! detached task ([`pump`]) feeds the chunks off a timer, and the clock
//! arithmetic (position, gapless boundaries, "finished") lives in
//! [`super::web_timeline`], which is plain code and tested natively.
//!
//! Gapless: [`PlaybackBackend::queue_next`] loads the next track in the
//! background; when the current one runs out of data its chunks continue with
//! the next track's, frame-exact, and the position/duration flip at the
//! boundary. The backend reports that crossing as "finished" once (the player
//! then advances its queue), and a last track that runs out as finished until
//! the player stops or restarts. The player learns about
//! failures that happen after `play` returned (a missing file, an unreachable
//! server, an undecodable format) through [`PlaybackBackend::take_error`].
//!
//! The visualizer (feature `visualizer`) is fed by reading back the chunks
//! already scheduled: while one is on screen ([`LocalBackend::set_pcm_capture`])
//! a timer hands it, every few milliseconds, the frames whose turn at the
//! output device came since the last time (the clock arithmetic is in
//! [`super::web_tap`]). That is the audio as it is *heard* -- decode runs
//! seconds ahead -- taken before the equaliser and the volume (replay gain
//! is already in it), and it costs nothing while no visualizer is open.
//!
//! Not provided in the browser: live internet radio, crossfade and the
//! fade-in/out settings (pause, resume, stop and seeks are de-clicked with a
//! few milliseconds' ramp regardless).

use super::PlaybackState;
use super::backend::{PlaybackBackend, PlayerError, extension_hint};
use super::engine::conversion::db_to_linear;
use super::engine::decoder::SymphoniaDecoder;
use super::engine::resampler::{ResamplerQuality, StreamResampler};
use super::eq_source::{EqController, NUM_BANDS, new_shared_coeffs};
use super::equalizer::BAND_FREQUENCIES;
#[cfg(feature = "visualizer")]
use super::pcm::PcmBuffer;
#[cfg(feature = "visualizer")]
use super::web_tap;
use super::web_timeline::Timeline;
use crate::library::TrackSource;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Cursor;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use send_wrapper::SendWrapper;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AudioBuffer, AudioBufferSourceNode, AudioContext, AudioContextOptions, AudioContextState,
    AudioScheduledSourceNode, BiquadFilterNode, BiquadFilterType, GainNode,
};
use web_time::Instant;

/// Seconds between "now" and the first chunk after a start, a seek or an
/// underrun.
const LEAD: f64 = 0.06;
/// Seconds of audio kept scheduled ahead of the clock while the page is
/// visible, and while it is hidden (timers are throttled to about once a
/// second there).
const AHEAD_VISIBLE: f64 = 8.0;
const AHEAD_HIDDEN: f64 = 20.0;
/// Below this many scheduled seconds the pump may work longer per turn.
const AHEAD_URGENT: f64 = 2.0;
/// How long one pump turn may decode before yielding to the page.
const BUDGET_RELAXED: Duration = Duration::from_millis(6);
const BUDGET_URGENT: Duration = Duration::from_millis(20);
/// Pump period while there is more to decode / nothing to do.
const PUMP_BUSY: Duration = Duration::from_millis(4);
const PUMP_IDLE: Duration = Duration::from_millis(100);
/// Time constant (seconds) of the volume ramp and of the click-free ramps.
const VOLUME_TAU: f64 = 0.02;
const DECLICK_TAU: f64 = 0.006;
/// How long the output ramps down before it is stopped or suspended.
const DECLICK_SECS: f64 = 0.03;
/// Sample rate DSD is decimated to before it is resampled to the context's.
const DSD_PCM_RATE: u32 = 88_200;
/// Gestures that may start an `AudioContext` (browsers' autoplay policies).
const UNLOCK_EVENTS: [&str; 5] = ["pointerdown", "pointerup", "click", "keydown", "touchend"];

/// Local playback in the browser.
pub struct LocalBackend {
    shared: SendWrapper<Rc<RefCell<Shared>>>,
    /// UI-side EQ controller; the graph follows the gains it records.
    eq_controller: EqController,
}

impl LocalBackend {
    /// Creates the backend. The `AudioContext` is created by the first user
    /// gesture or play, whichever comes first.
    pub fn new() -> Result<Self, PlayerError> {
        let eq_controller = EqController::new(
            new_shared_coeffs(),
            Arc::new(AtomicBool::new(true)), // EQ disabled by default
            44100.0,
        );
        let shared = Rc::new(RefCell::new(Shared::new(eq_controller.clone())));
        spawn_pump(Rc::downgrade(&shared));
        install_unlock_listeners(Rc::downgrade(&shared));
        Ok(Self {
            shared: SendWrapper::new(shared),
            eq_controller,
        })
    }

    /// The EQ controller for UI-thread use.
    pub fn eq_controller(&self) -> &EqController {
        &self.eq_controller
    }

    /// Live streams are not played in the browser, so there is no ICY title.
    pub fn icy_title(&self) -> Option<String> {
        None
    }

    /// Crossfade is not available in the browser.
    pub fn set_crossfade(&mut self, _secs: f32) {}

    /// Fade in/out is not available in the browser.
    pub fn set_fade_secs(&mut self, _secs: f32) {}

    /// Drops the pre-queued track (and its audio, if it was already
    /// scheduled) without touching the current one.
    pub fn clear_pre_queued(&mut self) {
        self.shared.borrow_mut().clear_next();
    }

    /// Sets the replay gain to apply to the next track.
    pub fn set_replay_gain_db(&mut self, gain_db: Option<f32>) {
        self.shared.borrow_mut().replay_gain_db = gain_db;
    }

    /// Sets the buffer the visualizer reads; filled only while
    /// [`Self::set_pcm_capture`] says it is wanted.
    #[cfg(feature = "visualizer")]
    pub fn set_pcm_buffer(&mut self, buffer: Arc<std::sync::Mutex<PcmBuffer>>) {
        self.shared.borrow_mut().tap.sink = Some(buffer);
    }

    /// Starts or stops filling the visualizer's buffer. Starting does not
    /// replay what was heard before: the buffer follows the output from now.
    #[cfg(feature = "visualizer")]
    pub fn set_pcm_capture(&mut self, wanted: bool) {
        let mut shared = self.shared.borrow_mut();
        shared.tap.wanted = wanted;
        shared.tap.cursor.reset();
    }

    /// Turns decoder trimming of encoder delay and padding on or off for the
    /// next track opened; the track being heard keeps its setting.
    pub fn set_gapless(&mut self, on: bool) {
        self.shared.borrow_mut().gapless = on;
    }
}

impl PlaybackBackend for LocalBackend {
    fn play(&mut self, source: TrackSource) -> Result<(), PlayerError> {
        check_source(&source)?;
        let weak = Rc::downgrade(&self.shared);
        let mut shared = self.shared.borrow_mut();
        shared.ensure_graph().map_err(PlayerError)?;
        shared.begin_play();
        let (generation, rg_db) = (shared.generation, shared.replay_gain_db);
        drop(shared);
        spawn_local(async move {
            let loaded = load(&source, rg_db).await;
            if let Some(shared) = weak.upgrade() {
                shared.borrow_mut().on_loaded(generation, loaded);
            }
        });
        Ok(())
    }

    fn pause(&mut self) -> Result<(), PlayerError> {
        let weak = Rc::downgrade(&self.shared);
        let mut shared = self.shared.borrow_mut();
        if shared.state != PlaybackState::Playing {
            return Ok(());
        }
        shared.state = PlaybackState::Paused;
        shared.pause_epoch += 1;
        let epoch = shared.pause_epoch;
        shared.ramp_master(DECLICK_TAU, 0.0);
        drop(shared);
        // Let the output ramp down before the clock stops.
        spawn_local(async move {
            super::super::task::sleep(Duration::from_secs_f64(DECLICK_SECS)).await;
            if let Some(shared) = weak.upgrade() {
                let shared = shared.borrow();
                if shared.pause_epoch == epoch
                    && shared.state == PlaybackState::Paused
                    && let Some(graph) = &shared.graph
                {
                    settle(graph.ctx.suspend(), "suspend");
                }
            }
        });
        Ok(())
    }

    fn resume(&mut self) -> Result<(), PlayerError> {
        let mut shared = self.shared.borrow_mut();
        if shared.state != PlaybackState::Paused {
            return Ok(());
        }
        shared.state = PlaybackState::Playing;
        shared.pause_epoch += 1;
        shared.ensure_graph().map_err(PlayerError)?;
        if let Some(graph) = &shared.graph {
            graph.resume();
        }
        shared.ramp_master(VOLUME_TAU, shared.volume);
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        self.shared.borrow_mut().stop();
        Ok(())
    }

    fn seek(&mut self, position: Duration) -> Result<(), PlayerError> {
        let mut shared = self.shared.borrow_mut();
        if let Some(graph) = &shared.graph {
            graph.resume_if_playing(shared.state);
        }
        shared.seek(position.as_secs_f64()).map_err(PlayerError)
    }

    fn set_volume(&mut self, volume: f32) -> Result<(), PlayerError> {
        let mut shared = self.shared.borrow_mut();
        shared.volume = volume.clamp(0.0, 1.0);
        let target = shared.target_gain();
        shared.ramp_master(VOLUME_TAU, target);
        Ok(())
    }

    fn volume(&self) -> f32 {
        self.shared.borrow().volume
    }

    fn state(&self) -> PlaybackState {
        self.shared.borrow().state
    }

    fn position(&self) -> Duration {
        let mut shared = self.shared.borrow_mut();
        shared.poll_clock();
        match shared.now() {
            Some(now) if shared.state != PlaybackState::Stopped && !shared.loading => {
                Duration::from_secs_f64(shared.timeline.position(now))
            }
            _ => Duration::ZERO,
        }
    }

    fn duration(&self) -> Duration {
        let shared = self.shared.borrow();
        shared
            .timeline
            .duration()
            .map_or(Duration::ZERO, Duration::from_secs_f64)
    }

    fn is_finished(&self) -> Result<bool, PlayerError> {
        let mut shared = self.shared.borrow_mut();
        shared.poll_clock();
        Ok(shared.take_finished())
    }

    fn queue_next(&mut self, source: TrackSource) -> Result<(), PlayerError> {
        check_source(&source)?;
        let weak = Rc::downgrade(&self.shared);
        let mut shared = self.shared.borrow_mut();
        shared.next_epoch += 1;
        shared.next = Next::Loading;
        // Queuing a next track un-signals "finished", as in the native engine.
        shared.crossed = false;
        let (epoch, rg_db) = (shared.next_epoch, shared.replay_gain_db);
        drop(shared);
        spawn_local(async move {
            let loaded = load(&source, rg_db).await;
            if let Some(shared) = weak.upgrade() {
                shared.borrow_mut().on_next_loaded(epoch, loaded);
            }
        });
        Ok(())
    }

    fn take_error(&mut self) -> Option<String> {
        self.shared.borrow_mut().error.take()
    }
}

/// Sources the browser can play.
fn check_source(source: &TrackSource) -> Result<(), PlayerError> {
    match source {
        TrackSource::LocalFile(_) | TrackSource::HttpStream { .. } => Ok(()),
        TrackSource::LiveStream(_) => Err(PlayerError(
            "Live internet radio cannot be played in the browser yet".to_string(),
        )),
        TrackSource::MpdFile(_) => Err(PlayerError(
            "MPD files should use MpdBackend, not LocalBackend".to_string(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// The bytes of a track, shared between the decoder and the backend (which
/// re-opens the track to seek it once its decoder has moved on).
#[derive(Clone)]
struct Bytes(Arc<Vec<u8>>);

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// A loaded track.
#[derive(Clone)]
struct TrackData {
    bytes: Bytes,
    /// File extension, to speed up probing the format.
    hint: Option<String>,
    rg_db: Option<f32>,
}

/// Reads a track's bytes: from the picked folder, or over HTTP.
async fn load(source: &TrackSource, rg_db: Option<f32>) -> Result<TrackData, String> {
    let (bytes, hint) = match source {
        TrackSource::LocalFile(path) => {
            let hint = path.extension().and_then(|e| e.to_str()).map(str::to_owned);
            (crate::web_fs::read_file(path).await?, hint)
        }
        // A browser decides on certificates itself: the option cannot apply.
        TrackSource::HttpStream { url, .. } => (fetch_bytes(url).await?, extension_hint(url)),
        TrackSource::LiveStream(_) | TrackSource::MpdFile(_) => {
            return Err("This source cannot be played in the browser".to_string());
        }
    };
    if bytes.is_empty() {
        return Err("The file is empty".to_string());
    }
    Ok(TrackData {
        bytes: Bytes(Arc::new(bytes)),
        hint,
        rg_db,
    })
}

/// The whole body of `url`. Failures say what went wrong in terms a user can
/// act on (the wording lives in [`crate::http`]): the request itself failing
/// is, in a browser, mostly the server not allowing this page to read it
/// (CORS), which the page cannot tell apart from the server being down;
/// mixed content is caught before the request is sent.
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    crate::http::check_mixed_content(url)?;
    let host = crate::http::host_of(url);
    let window = web_sys::window().ok_or("No browser window")?;
    let response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|_| crate::http::no_answer_message(url))?;
    let response: web_sys::Response = response
        .dyn_into()
        .map_err(|_| "Unexpected answer from the server".to_string())?;
    if !response.ok() {
        return Err(crate::http::status_message(
            &host,
            response.status(),
            &response.status_text(),
        ));
    }
    let body = response.array_buffer().map_err(|e| js_message(&e))?;
    let body = JsFuture::from(body).await.map_err(|e| {
        format!(
            "Downloading the track from {host} failed: {}",
            js_message(&e)
        )
    })?;
    Ok(js_sys::Uint8Array::new(&body).to_vec())
}

fn js_message(error: &JsValue) -> String {
    error
        .dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| error.as_string())
        .unwrap_or_else(|| "unknown error".to_string())
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Decodes one track into context-rate PCM chunks.
struct Feeder {
    decoder: SymphoniaDecoder,
    /// The decoder's output rate (the DSD decimation rate for DSD).
    src_rate: u32,
    out_rate: u32,
    /// Converts to `out_rate`; created with the first chunk, when the channel
    /// count is certain.
    resampler: Option<StreamResampler>,
    resampler_ready: bool,
    gain: f32,
    /// The next chunk is the first after a start or a seek: keep it short so
    /// playback starts quickly.
    fresh: bool,
    eof: bool,
    /// Channels of the last chunk.
    channels: usize,
    buf: Vec<f32>,
}

impl Feeder {
    /// Opens `data` for decoding at `out_rate`; `gapless` trims encoder delay
    /// and padding.
    fn open(data: &TrackData, out_rate: u32, gapless: bool) -> Result<Self, String> {
        let len = data.bytes.0.len() as u64;
        let mut decoder = SymphoniaDecoder::open_reader(
            Cursor::new(data.bytes.clone()),
            Some(len),
            data.hint.as_deref(),
            gapless,
        )
        .map_err(|e| format!("This file cannot be played: {e}"))?;
        if decoder.is_dsd() {
            decoder
                .enable_pcm_conversion(DSD_PCM_RATE)
                .map_err(|e| format!("This DSD file cannot be played: {e}"))?;
        }
        Ok(Self {
            src_rate: decoder.sample_rate(),
            decoder,
            out_rate,
            resampler: None,
            resampler_ready: false,
            gain: data.rg_db.map_or(1.0, db_to_linear),
            fresh: true,
            eof: false,
            channels: 2,
            buf: Vec::new(),
        })
    }

    /// Track length (seconds), when the container tells it.
    fn duration(&self) -> Option<f64> {
        self.decoder.duration()
    }

    /// Continues decoding at `secs`.
    fn seek(&mut self, secs: f64) -> Result<(), String> {
        self.decoder
            .seek(secs)
            .map_err(|e| format!("Seeking failed: {e}"))?;
        self.resampler = None;
        self.resampler_ready = false;
        self.fresh = true;
        self.eof = false;
        Ok(())
    }

    /// The next chunk: interleaved samples at the context's rate, `self.eof`
    /// set once the track has run out of data (the chunk may be empty then).
    fn next_chunk(&mut self) -> Result<Vec<f32>, String> {
        let frames = if self.fresh {
            self.src_rate / 8
        } else {
            self.src_rate / 2
        } as usize;
        self.fresh = false;
        let channels = usize::from(self.decoder.channels()).max(1);
        self.buf.resize(frames.max(1) * channels, 0.0);
        let read = self
            .decoder
            .read(&mut self.buf)
            .map_err(|e| format!("Decoding failed: {e}"))?;
        if read < self.buf.len() {
            self.eof = true;
        }
        // The channel count is certain once something was decoded.
        let channels = usize::from(self.decoder.channels()).max(1);
        self.channels = channels;
        let samples = &self.buf[..read - read % channels];
        if !self.resampler_ready && !samples.is_empty() {
            self.resampler_ready = true;
            if self.src_rate != self.out_rate {
                self.resampler = StreamResampler::new(
                    self.src_rate,
                    self.out_rate,
                    channels,
                    ResamplerQuality::SincMedium,
                );
            }
        }
        let mut out = match &mut self.resampler {
            Some(resampler) => resampler.process(samples),
            None => samples.to_vec(),
        };
        if self.eof
            && let Some(resampler) = &mut self.resampler
        {
            out.extend(resampler.flush());
        }
        if self.gain != 1.0 {
            for sample in &mut out {
                *sample *= self.gain;
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// The audio graph
// ---------------------------------------------------------------------------

/// The `AudioContext` and the fixed part of the graph.
struct Graph {
    ctx: AudioContext,
    rate: u32,
    /// First filter of the equaliser chain: where sources connect.
    input: BiquadFilterNode,
    filters: Vec<BiquadFilterNode>,
    master: GainNode,
    /// The equaliser gains the filters were last set to.
    applied_eq: Option<[f32; NUM_BANDS]>,
}

impl Graph {
    fn new() -> Result<Self, String> {
        let unavailable = |e: JsValue| format!("Web Audio is not available: {}", js_message(&e));
        let options = AudioContextOptions::new();
        // Nothing here is latency-critical, glitch-free matters.
        options.set_latency_hint(&JsValue::from_str("playback"));
        let ctx = AudioContext::new_with_context_options(&options).map_err(unavailable)?;
        let rate = ctx.sample_rate().round() as u32;
        let master = ctx.create_gain().map_err(unavailable)?;
        master.gain().set_value(0.0);
        master
            .connect_with_audio_node(&ctx.destination())
            .map_err(unavailable)?;
        let mut filters: Vec<BiquadFilterNode> = Vec::with_capacity(NUM_BANDS);
        for (index, freq) in BAND_FREQUENCIES.iter().enumerate() {
            let filter = ctx.create_biquad_filter().map_err(unavailable)?;
            filter.set_type(BiquadFilterType::Peaking);
            filter.frequency().set_value(*freq);
            filter
                .q()
                .set_value(EqController::band_q(index, rate as f32));
            filter.gain().set_value(0.0);
            filters.push(filter);
        }
        for pair in filters.windows(2) {
            pair[0]
                .connect_with_audio_node(&pair[1])
                .map_err(unavailable)?;
        }
        let (Some(first), Some(last)) = (filters.first(), filters.last()) else {
            return Err("Web Audio is not available".to_string());
        };
        last.connect_with_audio_node(&master).map_err(unavailable)?;
        Ok(Self {
            ctx,
            rate,
            input: first.clone(),
            filters,
            master,
            applied_eq: None,
        })
    }

    fn now(&self) -> f64 {
        self.ctx.current_time()
    }

    /// The context time of the sample the speakers are playing now: the
    /// clock runs ahead of it by the output latency.
    #[cfg(feature = "visualizer")]
    fn heard(&self) -> f64 {
        let ctx: &JsValue = self.ctx.as_ref();
        let number = |object: &JsValue, key: &str| {
            js_sys::Reflect::get(object, &JsValue::from_str(key))
                .ok()
                .and_then(|v| v.as_f64())
        };
        // `getOutputTimestamp` and the two latencies are missing from
        // web-sys, and from some browsers.
        let stamp = js_sys::Reflect::get(ctx, &JsValue::from_str("getOutputTimestamp"))
            .ok()
            .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
            .and_then(|f| f.call0(ctx).ok())
            .and_then(|stamp| number(&stamp, "contextTime"));
        web_tap::heard_time(
            self.now(),
            stamp,
            number(ctx, "baseLatency").unwrap_or(0.0),
            number(ctx, "outputLatency").unwrap_or(0.0),
        )
    }

    /// Starts (or restarts) the clock: browsers only allow it from a gesture,
    /// hence the calls from the play/resume/seek paths too.
    fn resume(&self) {
        if self.ctx.state() != AudioContextState::Running {
            settle(self.ctx.resume(), "resume");
        }
    }

    fn resume_if_playing(&self, state: PlaybackState) {
        if state == PlaybackState::Playing {
            self.resume();
        }
    }

    /// Follows the equaliser: its gains when enabled, flat otherwise.
    fn sync_eq(&mut self, eq: &EqController) {
        let enabled = eq.is_enabled();
        let gains = eq.gains();
        let target: [f32; NUM_BANDS] =
            std::array::from_fn(|i| if enabled { gains[i] } else { 0.0 });
        if self.applied_eq == Some(target) {
            return;
        }
        let now = self.now();
        for (filter, gain) in self.filters.iter().zip(target) {
            let _ = filter.gain().set_target_at_time(gain, now, VOLUME_TAU);
        }
        self.applied_eq = Some(target);
    }
}

impl Drop for Graph {
    fn drop(&mut self) {
        settle(self.ctx.close(), "close");
    }
}

/// Waits for a promise only to log its failure: an unhandled rejection would
/// land in the console as an uncaught error.
fn settle(promise: Result<js_sys::Promise, JsValue>, what: &'static str) {
    match promise {
        Ok(promise) => spawn_local(async move {
            if let Err(e) = JsFuture::from(promise).await {
                tracing::warn!("audio context {what} failed: {}", js_message(&e));
            }
        }),
        Err(e) => tracing::warn!("audio context {what} failed: {}", js_message(&e)),
    }
}

fn page_hidden() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| d.hidden())
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// A scheduled chunk of audio.
struct Chunk {
    node: AudioBufferSourceNode,
    /// Timeline frame it starts at.
    start_frame: u64,
    /// Context time it ends at.
    end_time: f64,
    /// The audio itself and the context time it starts at, for the
    /// visualizer tap to read back.
    #[cfg(feature = "visualizer")]
    buffer: AudioBuffer,
    #[cfg(feature = "visualizer")]
    start_time: f64,
}

/// The visualizer's tap on the output: what it feeds, whether anyone is
/// watching, and the scratch the read-back works in.
#[cfg(feature = "visualizer")]
#[derive(Default)]
struct Tap {
    sink: Option<Arc<std::sync::Mutex<PcmBuffer>>>,
    wanted: bool,
    cursor: web_tap::Cursor,
    /// One channel of a window, then the second, then both interleaved.
    left: Vec<f32>,
    right: Vec<f32>,
    stereo: Vec<f32>,
}

/// The track queued behind the current one.
enum Next {
    None,
    Loading,
    Ready(TrackData),
}

struct Shared {
    graph: Option<Graph>,
    timeline: Timeline,
    eq: EqController,
    state: PlaybackState,
    volume: f32,
    /// Replay gain for the next `play`/`queue_next`.
    replay_gain_db: Option<f32>,
    chunks: VecDeque<Chunk>,
    /// Decodes the track whose audio is being scheduled: the current one, or
    /// the upcoming one once the current has run out of data.
    feeder: Option<Feeder>,
    /// The track being heard, kept to seek it.
    current: Option<TrackData>,
    /// The track scheduled right behind it (gapless).
    upcoming: Option<TrackData>,
    next: Next,
    /// Bumped by `play`/`stop`: loads from before are discarded.
    generation: u64,
    /// Bumped by `queue_next`/`clear_pre_queued`.
    next_epoch: u64,
    /// The current track's bytes are being read.
    loading: bool,
    /// A gapless boundary was crossed since the player last asked (or since
    /// the last `play`/`queue_next`). A pulse: reading it clears it, so the
    /// player advances once per boundary, not on every tick afterwards.
    crossed: bool,
    /// Bumped on pause/resume: a pending suspend checks it is still wanted.
    pause_epoch: u64,
    error: Option<String>,
    /// Planar scratch for building `AudioBuffer`s.
    planar: Vec<f32>,
    /// Decoder trimming of encoder delay and padding for tracks opened next.
    gapless: bool,
    #[cfg(feature = "visualizer")]
    tap: Tap,
}

impl Shared {
    fn new(eq: EqController) -> Self {
        Self {
            graph: None,
            timeline: Timeline::new(48_000),
            eq,
            state: PlaybackState::Stopped,
            volume: 0.8,
            replay_gain_db: None,
            chunks: VecDeque::new(),
            feeder: None,
            current: None,
            upcoming: None,
            next: Next::None,
            generation: 0,
            next_epoch: 0,
            loading: false,
            crossed: false,
            pause_epoch: 0,
            error: None,
            planar: Vec::new(),
            gapless: true,
            #[cfg(feature = "visualizer")]
            tap: Tap::default(),
        }
    }

    /// Context time, once there is a context.
    fn now(&self) -> Option<f64> {
        self.graph.as_ref().map(Graph::now)
    }

    /// Creates the context and graph when there are none yet.
    fn ensure_graph(&mut self) -> Result<(), String> {
        if self.graph.is_none() {
            let mut graph = Graph::new()?;
            graph.sync_eq(&self.eq);
            self.timeline = Timeline::new(graph.rate);
            self.graph = Some(graph);
        }
        if let Some(graph) = &self.graph {
            graph.resume_if_playing(self.state);
        }
        Ok(())
    }

    /// What a user gesture may do: have the context ready (and running unless
    /// paused) so a later play, which may no longer be inside a gesture, works.
    fn unlock(&mut self) {
        if self.ensure_graph().is_err() {
            return;
        }
        if self.state != PlaybackState::Paused
            && let Some(graph) = &self.graph
        {
            graph.resume();
        }
    }

    /// The gain the master node should have now.
    fn target_gain(&self) -> f32 {
        if self.state == PlaybackState::Paused {
            0.0
        } else {
            self.volume
        }
    }

    /// Moves the master gain to `target`.
    fn ramp_master(&self, tau: f64, target: f32) {
        if let Some(graph) = &self.graph {
            let _ = graph
                .master
                .gain()
                .set_target_at_time(target, graph.now(), tau);
        }
    }

    /// Stops everything scheduled. With `fade` the output ramps down first
    /// (the nodes end a few milliseconds later); the caller ramps it back up.
    fn retire_chunks(&mut self, fade: bool) {
        let fade = fade && !self.chunks.is_empty();
        if let Some(graph) = &self.graph {
            let now = graph.now();
            if fade {
                let gain = graph.master.gain();
                let _ = gain.cancel_scheduled_values(now);
                let _ = gain.set_target_at_time(0.0, now, DECLICK_TAU);
            }
            let when = if fade { now + DECLICK_SECS } else { now };
            for chunk in self.chunks.drain(..) {
                stop_node(&chunk.node, when);
            }
        }
        self.chunks.clear();
    }

    /// Back to the unstarted state, ready for a new track.
    fn reset(&mut self) {
        let fade = self.state == PlaybackState::Playing;
        self.retire_chunks(fade);
        self.feeder = None;
        self.timeline.clear();
        self.current = None;
        self.upcoming = None;
        self.next = Next::None;
        self.next_epoch += 1;
        self.loading = false;
        self.crossed = false;
        self.error = None;
    }

    /// `play` after the source was accepted: stops what plays and waits for
    /// the new track's bytes.
    fn begin_play(&mut self) {
        self.generation += 1;
        self.reset();
        self.state = PlaybackState::Playing;
        self.pause_epoch += 1;
        self.loading = true;
        if let Some(graph) = &self.graph {
            graph.resume();
        }
        self.ramp_master_at(VOLUME_TAU, self.volume, DECLICK_SECS + 0.01);
    }

    /// Like [`Self::ramp_master`], starting `delay` seconds from now.
    fn ramp_master_at(&self, tau: f64, target: f32, delay: f64) {
        if let Some(graph) = &self.graph {
            let _ = graph
                .master
                .gain()
                .set_target_at_time(target, graph.now() + delay, tau);
        }
    }

    /// A load started by `play` finished.
    fn on_loaded(&mut self, generation: u64, loaded: Result<TrackData, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        let opened = loaded.and_then(|data| {
            let rate = self.graph.as_ref().map_or(48_000, |g| g.rate);
            Feeder::open(&data, rate, self.gapless).map(|feeder| (data, feeder))
        });
        match opened {
            Ok((data, feeder)) => {
                let now = self.now().unwrap_or(0.0);
                self.timeline.start(now, LEAD, 0.0, feeder.duration());
                self.current = Some(data);
                self.feeder = Some(feeder);
                self.fill(BUDGET_URGENT);
            }
            Err(message) => self.fail(message),
        }
    }

    /// A load started by `queue_next` finished.
    fn on_next_loaded(&mut self, epoch: u64, loaded: Result<TrackData, String>) {
        if epoch != self.next_epoch {
            return;
        }
        match loaded {
            Ok(data) => self.next = Next::Ready(data),
            Err(message) => {
                self.next = Next::None;
                self.error = Some(message);
            }
        }
    }

    /// Gives up the current track.
    fn fail(&mut self, message: String) {
        tracing::warn!("playback failed: {message}");
        self.retire_chunks(false);
        self.feeder = None;
        self.timeline.clear();
        self.loading = false;
        self.state = PlaybackState::Stopped;
        self.error = Some(message);
    }

    fn stop(&mut self) {
        self.generation += 1;
        self.reset();
        self.state = PlaybackState::Stopped;
        self.pause_epoch += 1;
    }

    /// Drops the queued track, and its audio if it is already scheduled.
    fn clear_next(&mut self) {
        self.next_epoch += 1;
        self.next = Next::None;
        if let Some(from_frame) = self.timeline.cancel_upcoming() {
            while self
                .chunks
                .back()
                .is_some_and(|chunk| chunk.start_frame >= from_frame)
            {
                if let Some(chunk) = self.chunks.pop_back() {
                    stop_node(&chunk.node, 0.0);
                }
            }
            // It was decoding the upcoming track.
            self.feeder = None;
            self.upcoming = None;
        }
    }

    /// Restarts the current track's audio at `secs`.
    fn seek(&mut self, secs: f64) -> Result<(), String> {
        if self.loading || self.graph.is_none() {
            return Ok(());
        }
        let Some(current) = self.current.clone() else {
            return Ok(());
        };
        let duration = self.timeline.duration();
        let secs = match duration {
            Some(duration) => secs.min((duration - 0.05).max(0.0)),
            None => secs,
        };
        // The upcoming track goes back to waiting its turn.
        let reuse = self.upcoming.is_none() && self.feeder.is_some();
        if let Some(upcoming) = self.upcoming.take() {
            self.next_epoch += 1;
            self.next = Next::Ready(upcoming);
            self.feeder = None;
        }
        let fade = self.state == PlaybackState::Playing;
        self.retire_chunks(fade);
        let result = match self.feeder.take() {
            Some(feeder) if reuse => Ok(feeder),
            _ => Feeder::open(
                &current,
                self.graph.as_ref().map_or(48_000, |g| g.rate),
                self.gapless,
            ),
        }
        .and_then(|mut feeder| feeder.seek(secs).map(|()| feeder));
        let mut feeder = match result {
            Ok(feeder) => feeder,
            Err(message) => {
                self.fail(message);
                return Ok(());
            }
        };
        let now = self.now().unwrap_or(0.0);
        self.timeline.start(now, LEAD, secs, duration);
        // Seeking a track that had ended resumes it.
        self.crossed = false;
        feeder.eof = false;
        self.feeder = Some(feeder);
        if fade {
            let target = self.target_gain();
            self.ramp_master_at(VOLUME_TAU, target, DECLICK_SECS + 0.01);
        }
        self.fill(BUDGET_URGENT);
        Ok(())
    }

    /// What `is_finished` reports: a gapless boundary just crossed (once), or
    /// the last track ran out with nothing queued behind it (until the player
    /// stops or restarts). While a next track is still loading the current
    /// one is not finished: the player would skip over the track about to
    /// start.
    fn take_finished(&mut self) -> bool {
        if self.state != PlaybackState::Playing {
            return false;
        }
        if std::mem::take(&mut self.crossed) {
            return true;
        }
        matches!(self.next, Next::None) && self.now().is_some_and(|now| self.timeline.ended(now))
    }

    /// Moves the clock-derived state on: the heard track changes at a gapless
    /// boundary.
    fn poll_clock(&mut self) {
        let Some(now) = self.now() else {
            return;
        };
        let had_upcoming = self.timeline.has_upcoming();
        if self.timeline.advance(now) {
            self.crossed = true;
        }
        if had_upcoming && !self.timeline.has_upcoming() {
            self.current = self.upcoming.take();
        }
    }

    /// One turn of the pump: follows the clock, the equaliser and the page
    /// visibility, tops up the scheduled audio, and says when to come back.
    fn pump(&mut self) -> Duration {
        #[cfg(feature = "visualizer")]
        if self.tap.wanted {
            let wait = self.top_up();
            self.feed_tap();
            return wait.min(web_tap::TICK);
        }
        self.top_up()
    }

    /// The part of a pump turn that keeps the output fed.
    fn top_up(&mut self) -> Duration {
        self.poll_clock();
        if let Some(graph) = self.graph.as_mut() {
            graph.sync_eq(&self.eq);
        }
        let Some(now) = self.now() else {
            return PUMP_IDLE;
        };
        while self
            .chunks
            .front()
            .is_some_and(|chunk| chunk.end_time < now - 0.5)
        {
            self.chunks.pop_front();
        }
        if self.loading || self.state == PlaybackState::Stopped {
            return PUMP_IDLE;
        }
        let urgent = self.timeline.ahead(now) < AHEAD_URGENT;
        self.fill(if urgent {
            BUDGET_URGENT
        } else {
            BUDGET_RELAXED
        });
        if self.wants_more(now) {
            PUMP_BUSY
        } else {
            PUMP_IDLE
        }
    }

    /// Hands the visualizer the audio that was heard since the last turn:
    /// the frames of the scheduled chunks that fall between the two
    /// readings of the output clock.
    #[cfg(feature = "visualizer")]
    fn feed_tap(&mut self) {
        let Tap {
            sink: Some(sink),
            cursor,
            left,
            right,
            stereo,
            ..
        } = &mut self.tap
        else {
            return;
        };
        let Some(graph) = &self.graph else {
            return;
        };
        let Some((from, to)) = cursor.advance(graph.heard()) else {
            return;
        };
        stereo.clear();
        for chunk in &self.chunks {
            let buffer = &chunk.buffer;
            let range = web_tap::frames_within(
                chunk.start_time,
                buffer.length() as usize,
                f64::from(buffer.sample_rate()),
                from,
                to,
            );
            if range.is_empty() {
                continue;
            }
            let start = range.start as u32;
            left.resize(range.len(), 0.0);
            if buffer
                .copy_from_channel_with_start_in_channel(left, 0, start)
                .is_err()
            {
                continue;
            }
            // Mono is heard on both sides.
            right.clear();
            right.extend_from_slice(left);
            if buffer.number_of_channels() > 1 {
                let _ = buffer.copy_from_channel_with_start_in_channel(right, 1, start);
            }
            for (l, r) in left.iter().zip(right.iter()) {
                stereo.extend([*l, *r]);
            }
        }
        if !stereo.is_empty()
            && let Ok(mut buffer) = sink.lock()
        {
            buffer.write_interleaved(stereo, 2);
        }
    }

    fn ahead_target() -> f64 {
        if page_hidden() {
            AHEAD_HIDDEN
        } else {
            AHEAD_VISIBLE
        }
    }

    /// Whether there is audio left to schedule and room for it.
    fn wants_more(&self, now: f64) -> bool {
        let source_left = self.feeder.is_some()
            || (self.timeline.feed_ended()
                && !self.timeline.has_upcoming()
                && matches!(self.next, Next::Ready(_)));
        source_left && self.timeline.ahead(now) < Self::ahead_target()
    }

    /// Decodes and schedules chunks until enough is ahead of the clock, the
    /// tracks run out, or `budget` is spent.
    fn fill(&mut self, budget: Duration) {
        let started = Instant::now();
        let target = Self::ahead_target();
        loop {
            let Some(now) = self.now() else {
                return;
            };
            if self.timeline.ahead(now) >= target {
                return;
            }
            if self.feeder.is_none() && !self.attach_next() {
                return;
            }
            self.schedule_chunk(now);
            if started.elapsed() >= budget {
                return;
            }
        }
    }

    /// The current track has run out of data: continues with the queued one.
    fn attach_next(&mut self) -> bool {
        if !matches!(self.next, Next::Ready(_))
            || !self.timeline.feed_ended()
            || self.timeline.has_upcoming()
        {
            return false;
        }
        let Next::Ready(data) = std::mem::replace(&mut self.next, Next::None) else {
            return false;
        };
        let rate = self.graph.as_ref().map_or(48_000, |g| g.rate);
        match Feeder::open(&data, rate, self.gapless) {
            Ok(feeder) => {
                self.timeline.begin_next(feeder.duration());
                self.upcoming = Some(data);
                self.feeder = Some(feeder);
                true
            }
            Err(message) => {
                self.error = Some(message);
                false
            }
        }
    }

    /// Decodes one chunk of the feeder's track and schedules it.
    fn schedule_chunk(&mut self, now: f64) {
        let Some(feeder) = self.feeder.as_mut() else {
            return;
        };
        let decoded = feeder.next_chunk();
        let channels = feeder.channels;
        let mut ended = feeder.eof;
        match decoded {
            Ok(samples) if samples.is_empty() => {}
            Ok(samples) => {
                if let Err(message) = self.place(&samples, channels, now) {
                    self.error = Some(message);
                    ended = true;
                }
            }
            Err(message) => {
                tracing::warn!("{message}");
                self.error = Some(message);
                ended = true;
            }
        }
        if ended {
            // Out of data: what is scheduled plays out, then the track is over.
            self.timeline.end_feed();
            self.feeder = None;
        }
    }

    /// Schedules `samples` (interleaved) right behind the audio before them.
    fn place(&mut self, samples: &[f32], channels: usize, now: f64) -> Result<(), String> {
        let Some(graph) = &self.graph else {
            return Ok(());
        };
        let frames = samples.len() / channels;
        let buffer = make_buffer(&graph.ctx, samples, channels, graph.rate, &mut self.planar)
            .map_err(|e| format!("Web Audio refused the audio: {}", js_message(&e)))?;
        let node = graph
            .ctx
            .create_buffer_source()
            .map_err(|e| js_message(&e))?;
        node.set_buffer(Some(&buffer));
        node.connect_with_audio_node(&graph.input)
            .map_err(|e| js_message(&e))?;
        let start = self.timeline.next_start(now, LEAD);
        node.start_with_when(start).map_err(|e| js_message(&e))?;
        self.chunks.push_back(Chunk {
            node,
            start_frame: self.timeline.frames(),
            end_time: start + frames as f64 / f64::from(graph.rate),
            #[cfg(feature = "visualizer")]
            buffer,
            #[cfg(feature = "visualizer")]
            start_time: start,
        });
        self.timeline.commit(frames as u64);
        Ok(())
    }
}

/// Ends a scheduled node at context time `when` (an already past time: now).
fn stop_node(node: &AudioBufferSourceNode, when: f64) {
    let node: &AudioScheduledSourceNode = node.as_ref();
    let _ = node.stop_with_when(when);
}

/// An `AudioBuffer` of `rate` Hz holding the interleaved `samples`.
fn make_buffer(
    ctx: &AudioContext,
    samples: &[f32],
    channels: usize,
    rate: u32,
    planar: &mut Vec<f32>,
) -> Result<AudioBuffer, JsValue> {
    let frames = samples.len() / channels;
    let buffer = ctx.create_buffer(channels as u32, frames as u32, rate as f32)?;
    planar.resize(frames, 0.0);
    for channel in 0..channels {
        for (frame, out) in planar.iter_mut().enumerate() {
            *out = samples[frame * channels + channel];
        }
        buffer.copy_to_channel(planar, channel as i32)?;
    }
    Ok(buffer)
}

// ---------------------------------------------------------------------------
// Detached tasks
// ---------------------------------------------------------------------------

/// Feeds the output for as long as the backend lives. A page timer, not an
/// animation frame: it keeps running (throttled) in a background tab, where
/// the larger [`AHEAD_HIDDEN`] buffer covers the gaps.
fn spawn_pump(shared: Weak<RefCell<Shared>>) {
    spawn_local(async move {
        loop {
            let Some(strong) = shared.upgrade() else {
                return;
            };
            let wait = strong.borrow_mut().pump();
            drop(strong);
            super::super::task::sleep(wait).await;
        }
    });
}

/// Starts the context from the first gesture: browsers (Safari above all)
/// only let a context start inside the event handler of one, and a play that
/// reaches the backend through the UI's frame loop is no longer inside it.
fn install_unlock_listeners(shared: Weak<RefCell<Shared>>) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let handler = Closure::<dyn FnMut()>::new(move || {
        if let Some(shared) = shared.upgrade() {
            shared.borrow_mut().unlock();
        }
    });
    for event in UNLOCK_EVENTS {
        let _ = document.add_event_listener_with_callback_and_bool(
            event,
            handler.as_ref().unchecked_ref(),
            true,
        );
    }
    // Lives as long as the page; one closure per backend.
    handler.forget();
}
