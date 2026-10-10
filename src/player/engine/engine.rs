// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The playback-thread engine: decode → gain → EQ → volume → output.
//!
//! One dedicated [`std::thread`] per [`PlaybackEngine::play`] call owns the
//! decoder(s) and drives decode → gain → EQ → volume → output for the
//! entire remaining lifetime of the queue — surviving gapless/crossfade
//! transitions via look-ahead, only respawning on an explicit new `play()`,
//! manual skip, or `stop()`. Playback state (playing/paused) is shared via
//! an `Arc<AtomicU8>` so the hot loop never takes a lock to check pause
//! state.
//!
//! Mirrors the hot-loop *shape* of `~/M0Rf30/rmpd`'s
//! `rmpd-player/src/engine.rs` — see `local://rmpd_player_report.md`
//! (sections 2, 5, 6, 7) for the full architecture this is based on — with
//! one deliberate simplification per that report's §9: rmpd's decode
//! thread → `MultiOutput` worker → `CpalOutput`-internal-channel double hop
//! collapses to a single hop here, since aulos only ever has one output.
//! The decode thread applies ReplayGain, [`EqFilter`], and [`VolumeFilter`]
//! directly, then hands samples straight to [`CpalOutput::write`] /
//! [`DopOutput::write`] — no `MultiOutput`-equivalent fan-out type exists.
//! With the `visualizer` feature, [`CpalOutput`]'s cpal device callback
//! (in `output.rs`, downstream of the ~500ms-deep buffered `sync_channel`
//! `CpalOutput::write` sends into) additionally taps the exact samples it
//! just handed to the device — deliberately *not* on this decode thread,
//! which would otherwise let the visualizer see/react to audio up to that
//! whole buffer depth before it's actually heard.
//!
//! The whole module is deliberately synchronous — no `tokio`/`async`
//! anywhere — since [`crate::player::backend::PlaybackBackend`] is a fully
//! synchronous trait.

use std::num::NonZero;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use symphonia::core::codecs::audio::{BitOrder, ChannelDataLayout};

use super::cpal_utils;
use super::crossfade;
use super::decoder::{AudioFormat as DecoderFormat, SymphoniaDecoder};
use super::dop::DopEncoder;
use super::dop_output::DopOutput;
use super::fade::{FadeControl, MAX_FADE_SECS};
use super::filter::{AudioFilter, VolumeFilter};
use super::output::{AudioFormat as OutputFormat, CpalOutput};
use super::resampler::ResamplerQuality;
use crate::player::backend::PlayerError;
use crate::player::eq_source::{EqFilter, SharedCoeffs};
use crate::player::http_range_reader::HttpRangeReader;
use crate::player::icy_reader::IcyStrippingReader;
#[cfg(feature = "visualizer")]
use crate::views::now_playing::visualizer::PcmBuffer;

pub type Result<T, E = PlayerError> = std::result::Result<T, E>;

/// Interleaved `f32` samples decoded per chunk (matches rmpd's `BUFFER_SIZE`).
const BUFFER_SIZE: usize = 4096;

/// Default output ring-buffer depth in milliseconds (matches rmpd's default;
/// see `CpalOutput::start`'s channel-depth sizing).
const DEFAULT_BUFFER_TIME_MS: u32 = 500;

/// Lock-free playback run-state values stored in `Arc<AtomicU8>`.
const RUN_STOPPED: u8 = 0;
const RUN_PLAYING: u8 = 1;
const RUN_PAUSED: u8 = 2;

/// Valid DSD-to-PCM decode rates, ascending. DSD decimates cleanly only by an
/// integer power of two, so every target is 44.1 kHz-family.
const DSD_PCM_RATES: [u32; 4] = [44_100, 88_200, 176_400, 352_800];

/// Choose the DSD-to-PCM decode rate for a device running at `device_rate`.
///
/// Returns the SMALLEST DSD-family rate that both covers `device_rate` and is
/// reported as supported, falling back to the largest supported family rate
/// and finally to 88.2 kHz.
///
/// Decoding to the highest rate a device merely *advertises* is harmful:
/// systems like PipeWire advertise enormous ranges (up to ~768 kHz) but
/// resample internally, so an over-high PCM rate (a) gives a punishingly
/// short real-time callback period that underruns on scheduling jitter
/// (audible crackle), and (b) leaves DSD's ultrasonic shaped noise in the
/// PCM, muddying the sound. A moderate rate lets the decimation filter
/// remove that noise and keeps the buffer period comfortable.
///
/// Ported near-verbatim from rmpd's `engine.rs` (see the report's §5/§9) —
/// zero rmpd-specific dependencies.
fn select_dsd_pcm_rate(device_rate: u32, supports_rate: impl Fn(u32) -> bool) -> u32 {
    DSD_PCM_RATES
        .iter()
        .copied()
        .find(|&r| r >= device_rate && supports_rate(r))
        .or_else(|| {
            DSD_PCM_RATES
                .iter()
                .rev()
                .copied()
                .find(|&r| supports_rate(r))
        })
        .unwrap_or(88_200)
}

/// The device rate the cpal output should open at for a DSD-to-PCM stream
/// decoded at `decode_rate` on a device whose native rate is `device_rate`.
///
/// Returns `None` (open the stream at `decode_rate`, no resampling) when the
/// rates already match, or when `native_decode_ok` — the resolved output is
/// an explicitly-configured device that natively supports `decode_rate`, so
/// it is safe to play bit-perfect. Otherwise returns `Some(device_rate)` so
/// the engine resamples to the device's native rate itself, rather than
/// letting a sound server (e.g. PipeWire) resample a rate it merely
/// advertises — which underruns and leaves DSD ultrasonic noise in-band.
///
/// Ported verbatim from rmpd's `engine.rs` (see the report's §5/§9).
fn dsd_output_target_rate(
    decode_rate: u32,
    device_rate: u32,
    native_decode_ok: bool,
) -> Option<u32> {
    if native_decode_ok || decode_rate == device_rate {
        None
    } else {
        Some(device_rate)
    }
}

/// A playable source. `LocalFile` is opened synchronously (a fast header
/// probe) by [`PlaybackEngine::play`] so a bad/missing file surfaces
/// immediately through its `Result`. `Reader` and `LiveStream` carry the
/// *ingredients* for an HTTP connection rather than an already-connected
/// one: constructing a [`HttpRangeReader`]/live response performs a real
/// blocking `GET`, so that connect — and the subsequent format probe —
/// happens on the dedicated playback thread instead of the caller's thread.
/// This is the one deliberate deviation from the shape sketched in the
/// assignment: a pre-built `reader` field would have reintroduced the exact
/// blocking-the-caller problem the old background-thread dance existed to
/// avoid.
pub enum PlaySource {
    LocalFile(PathBuf),
    Reader {
        url: String,
        client: reqwest::blocking::Client,
        hint_extension: Option<String>,
    },
    /// An internet radio / Shoutcast-Icecast live stream. `icy_title` is
    /// updated in place from any embedded ICY `StreamTitle` metadata so the
    /// UI can read it back without threading a message through the engine.
    LiveStream {
        url: String,
        client: reqwest::blocking::Client,
        hint_extension: Option<String>,
        icy_title: Arc<Mutex<Option<String>>>,
    },
}

impl PlaySource {
    /// Open the decoder for this source. For `Reader`/`LiveStream`, this is
    /// where the (potentially slow) HTTP connect + probe actually happens.
    fn open_decoder(self) -> Result<SymphoniaDecoder> {
        match self {
            PlaySource::LocalFile(path) => SymphoniaDecoder::open(&path),
            PlaySource::Reader {
                url,
                client,
                hint_extension,
            } => {
                let reader = HttpRangeReader::new(url, Some(client)).map_err(PlayerError)?;
                let byte_len = {
                    let len = reader.content_length();
                    (len > 0).then_some(len)
                };
                SymphoniaDecoder::open_reader(reader, byte_len, hint_extension.as_deref())
            }
            PlaySource::LiveStream {
                url,
                client,
                hint_extension,
                icy_title,
            } => {
                let response = client
                    .get(&url)
                    .header("Icy-MetaData", "1")
                    .send()
                    .map_err(|e| PlayerError(format!("Failed to connect to stream: {e}")))?;
                let metadata_interval =
                    icy_metadata::IcyHeaders::parse_from_headers(response.headers())
                        .metadata_interval();
                match metadata_interval {
                    Some(interval) => {
                        // `icy_metadata::IcyMetadataReader` (crate 0.6.0) panics
                        // whenever `icy-metaint` exceeds the caller's read-buffer
                        // size — which Symphonia's `MediaSourceStream` (~32 KiB
                        // internal buffer) triggers for any station whose
                        // metaint is bigger than that (e.g. SomaFM's 45000),
                        // a few seconds into playback. See `icy_reader`'s module
                        // docs for the full explanation; this is the direct fix
                        // for "radio streams play for a few seconds and stop".
                        let reader =
                            IcyStrippingReader::new(response, interval.get(), move |metadata| {
                                let title =
                                    metadata.and_then(|m| m.stream_title().map(str::to_string));
                                *icy_title.lock() = title;
                            });
                        SymphoniaDecoder::open_stream(reader, hint_extension.as_deref())
                    }
                    None => SymphoniaDecoder::open_stream(response, hint_extension.as_deref()),
                }
            }
        }
    }
}

/// Maximum consecutive reconnect attempts for a live radio stream hiccup
/// (transient network drop, server-side reset) before giving up and
/// letting the track be treated as genuinely finished — see `LiveRetry`
/// and `reconnect_live`.
const MAX_LIVE_RECONNECT_ATTEMPTS: u32 = 5;

/// Reconnect ingredients captured from a `PlaySource::LiveStream` before it
/// is consumed opening the initial connection. Internet radio has no
/// concept of "the track ended" — a bare EOS or IO error almost always
/// means the TCP connection dropped, not that playback should stop, so
/// `run_pcm` uses this to transparently re-`GET` the same URL instead of
/// surfacing the drop as "finished" (see `try_reconnect`). Cheap to clone:
/// `client` is a `reqwest::blocking::Client`, itself just a handle to a
/// shared connection pool.
#[derive(Clone)]
struct LiveRetry {
    url: String,
    client: reqwest::blocking::Client,
    hint_extension: Option<String>,
    icy_title: Arc<Mutex<Option<String>>>,
}

fn live_retry_from_source(source: &PlaySource) -> Option<LiveRetry> {
    match source {
        PlaySource::LiveStream {
            url,
            client,
            hint_extension,
            icy_title,
        } => Some(LiveRetry {
            url: url.clone(),
            client: client.clone(),
            hint_extension: hint_extension.clone(),
            icy_title: Arc::clone(icy_title),
        }),
        PlaySource::LocalFile(_) | PlaySource::Reader { .. } => None,
    }
}

/// Mirrors `live_retry_from_source`, but for the value the playback thread
/// is actually started with (a `ThreadStart::Ready` local file never needs
/// reconnect ingredients — it can't be a live stream).
fn live_retry_from_start(start: &ThreadStart) -> Option<LiveRetry> {
    match start {
        ThreadStart::Pending(source) => live_retry_from_source(source),
        ThreadStart::Ready(_) => None,
    }
}

/// Re-establish a live stream's HTTP connection after a hiccup, retrying
/// with a short, capped backoff up to `MAX_LIVE_RECONNECT_ATTEMPTS` times.
/// Bails out immediately (returning `None`) if `stop()` is requested
/// mid-retry — including mid-backoff-sleep — so the user's Stop is never
/// delayed by a reconnect loop. Also returns `None` once every attempt is
/// exhausted, at which point the caller falls back to treating the stream
/// as genuinely finished.
fn reconnect_live(retry: &LiveRetry, ctx: &ThreadContext) -> Option<SymphoniaDecoder> {
    for attempt in 1..=MAX_LIVE_RECONNECT_ATTEMPTS {
        if ctx.stop_flag.load(Ordering::Acquire) {
            return None;
        }
        let source = PlaySource::LiveStream {
            url: retry.url.clone(),
            client: retry.client.clone(),
            hint_extension: retry.hint_extension.clone(),
            icy_title: Arc::clone(&retry.icy_title),
        };
        match source.open_decoder() {
            Ok(decoder) => {
                tracing::info!("live stream reconnected after a hiccup (attempt {attempt})");
                return Some(decoder);
            }
            Err(e) => {
                tracing::warn!(
                    "live stream reconnect attempt {attempt}/{MAX_LIVE_RECONNECT_ATTEMPTS} failed: {e}"
                );
                if attempt == MAX_LIVE_RECONNECT_ATTEMPTS {
                    break;
                }
                // Capped linear backoff, polled in small steps so a
                // `stop()` fired mid-wait still takes effect promptly.
                let backoff =
                    Duration::from_millis(300 * u64::from(attempt)).min(Duration::from_secs(3));
                let deadline = std::time::Instant::now() + backoff;
                while std::time::Instant::now() < deadline {
                    if ctx.stop_flag.load(Ordering::Acquire) {
                        return None;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    tracing::error!(
        "live stream reconnect exhausted {MAX_LIVE_RECONNECT_ATTEMPTS} attempts, giving up"
    );
    None
}

/// What `try_reconnect` found on success — mirrors the two shapes
/// `run_pcm`'s existing gapless-advance handling already knows how to deal
/// with: continue in place when the reconnected stream's format matches
/// the currently-open output, or hand back a decoder for the caller to
/// rebuild the output around when it doesn't.
enum ReconnectOutcome {
    SameFormat(SymphoniaDecoder),
    FormatChanged(SymphoniaDecoder, f32),
}

/// Attempts a live-stream reconnect via `reconnect_live`, if `live_retry`
/// carries any (i.e. the current source actually is a live stream).
/// Returns `None` when there's nothing to reconnect (not a live stream) or
/// every attempt failed — either way, the caller should fall back to its
/// original "really done" behavior.
fn try_reconnect(
    live_retry: Option<&LiveRetry>,
    format: DecoderFormat,
    gain: f32,
    ctx: &ThreadContext,
) -> Option<ReconnectOutcome> {
    let retry = live_retry?;
    let decoder = reconnect_live(retry, ctx)?;
    if !decoder.is_dsd() && decoder.format() == format {
        Some(ReconnectOutcome::SameFormat(decoder))
    } else {
        Some(ReconnectOutcome::FormatChanged(decoder, gain))
    }
}

/// Out-of-band commands sent to the playback thread (mirrors rmpd's
/// `PlaybackCommand`, minus everything but seek — aulos has no CUE ranges).
enum EngineCommand {
    Seek(Duration),
}

/// A track queued via [`PlaybackEngine::queue_next`] for gapless/crossfade
/// look-ahead — mirrors rmpd's `next_song` slot (see the report's §7).
struct PendingTrack {
    source: PlaySource,
    replay_gain_db: Option<f32>,
}

/// What to run on the freshly spawned playback thread.
enum ThreadStart {
    /// Already open (local files — opened synchronously by `play()`).
    Ready(SymphoniaDecoder),
    /// Opened lazily on the playback thread (HTTP streams).
    Pending(PlaySource),
}

/// Lock-free playback status, readable by the controlling side
/// (`LocalBackend`) without ever touching the hot loop.
#[derive(Default)]
struct SharedStatus {
    /// Nanoseconds of the current track played so far.
    position_nanos: AtomicU64,
    /// Nanoseconds total duration of the current track (0 = unknown).
    duration_nanos: AtomicU64,
    /// `true` between `play()` returning and the playback thread finishing
    /// its open/probe — generalizes the pre-engine backend's HTTP-only
    /// `loading` flag to both source kinds (for local files this window is
    /// a header-probe's worth of time; for HTTP it is a real network RTT).
    probing: AtomicBool,
    /// Set by the playback thread whenever it crosses a track boundary
    /// (gapless/crossfade-advanced, or truly exhausted) — mirrors the old
    /// `TrackBoundarySource`'s shared flag. Cleared by `play()`/
    /// `queue_next()` on the calling side.
    track_finished: AtomicBool,
    /// Set once the playback thread has permanently stopped because the
    /// whole queue is exhausted (no pending track at the final EOS) or an
    /// unrecoverable output error occurred. Fallback for `is_finished()`,
    /// analogous to the old backend's `sink.empty()` check.
    queue_exhausted: AtomicBool,
}

impl SharedStatus {
    fn reset_for_play(&self) {
        self.probing.store(true, Ordering::Release);
        self.track_finished.store(false, Ordering::Release);
        self.queue_exhausted.store(false, Ordering::Release);
        self.position_nanos.store(0, Ordering::Release);
        self.duration_nanos.store(0, Ordering::Release);
    }

    fn reset_for_stop(&self) {
        self.probing.store(false, Ordering::Release);
        self.track_finished.store(false, Ordering::Release);
        self.queue_exhausted.store(false, Ordering::Release);
        self.position_nanos.store(0, Ordering::Release);
        self.duration_nanos.store(0, Ordering::Release);
    }
}

/// Everything the playback thread needs, cloned once per `play()` call.
/// All fields are `Arc`s so `#[derive(Clone)]` is cheap and config changes
/// (volume, crossfade, EQ, the visualizer buffer) made on the controlling
/// side are visible to the hot loop immediately, with no respawn.
#[derive(Clone)]
struct ThreadContext {
    run_state: Arc<AtomicU8>,
    stop_flag: Arc<AtomicBool>,
    volume: Arc<AtomicU8>,
    eq_coeffs: SharedCoeffs,
    eq_bypass: Arc<AtomicBool>,
    /// `f32` crossfade seconds, bit-packed for lock-free reads.
    crossfade_bits: Arc<AtomicU32>,
    next_track: Arc<Mutex<Option<PendingTrack>>>,
    status: Arc<SharedStatus>,
    /// Fade in/out ramp control shared with the output's device callback.
    fade: Arc<FadeControl>,
    /// Set (together with `stop_flag`) when this thread should fade out
    /// before closing its stream — see `PlaybackEngine::retire_gracefully`.
    graceful_stop: Arc<AtomicBool>,
    #[cfg(feature = "visualizer")]
    pcm_buffer: Arc<Mutex<Option<Arc<std::sync::Mutex<PcmBuffer>>>>>,
}

/// Playback engine: owns the dedicated decode/output thread and the shared
/// state the controlling side (`LocalBackend`) reads/writes without ever
/// blocking the hot loop.
pub struct PlaybackEngine {
    run_state: Arc<AtomicU8>,
    stop_flag: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    command_tx: Option<mpsc::Sender<EngineCommand>>,
    volume: Arc<AtomicU8>,
    eq_coeffs: SharedCoeffs,
    eq_bypass: Arc<AtomicBool>,
    crossfade_bits: Arc<AtomicU32>,
    next_track: Arc<Mutex<Option<PendingTrack>>>,
    status: Arc<SharedStatus>,
    fade: Arc<FadeControl>,
    graceful_stop: Arc<AtomicBool>,
    /// Threads told to fade out and exit that may still be running; joined
    /// by the next playback thread (or on drop) so `stop()`/`play()` never
    /// block the UI thread for the duration of a fade.
    retiring: Vec<thread::JoinHandle<()>>,
    #[cfg(feature = "visualizer")]
    pcm_buffer: Arc<Mutex<Option<Arc<std::sync::Mutex<PcmBuffer>>>>>,
}

impl PlaybackEngine {
    /// Create a new, idle engine. The output device is opened lazily by the
    /// first `play()` call (sized to that track's format), not here.
    ///
    /// `eq_coeffs`/`eq_bypass` should be the same `Arc`s backing the
    /// caller's [`crate::player::eq_source::EqController`] — the engine
    /// builds a fresh [`EqFilter`] from them for each open output so UI-side
    /// EQ adjustments apply live, exactly as before.
    pub fn new(eq_coeffs: SharedCoeffs, eq_bypass: Arc<AtomicBool>) -> Self {
        Self {
            run_state: Arc::new(AtomicU8::new(RUN_STOPPED)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            thread: None,
            command_tx: None,
            volume: Arc::new(AtomicU8::new(100)),
            eq_coeffs,
            eq_bypass,
            crossfade_bits: Arc::new(AtomicU32::new(0f32.to_bits())),
            next_track: Arc::new(Mutex::new(None)),
            status: Arc::new(SharedStatus::default()),
            fade: FadeControl::new(),
            graceful_stop: Arc::new(AtomicBool::new(false)),
            retiring: Vec::new(),
            #[cfg(feature = "visualizer")]
            pcm_buffer: Arc::new(Mutex::new(None)),
        }
    }

    fn context(&self) -> ThreadContext {
        ThreadContext {
            run_state: self.run_state.clone(),
            stop_flag: self.stop_flag.clone(),
            volume: self.volume.clone(),
            eq_coeffs: self.eq_coeffs.clone(),
            eq_bypass: self.eq_bypass.clone(),
            crossfade_bits: self.crossfade_bits.clone(),
            next_track: self.next_track.clone(),
            status: self.status.clone(),
            fade: self.fade.clone(),
            graceful_stop: self.graceful_stop.clone(),
            #[cfg(feature = "visualizer")]
            pcm_buffer: self.pcm_buffer.clone(),
        }
    }

    /// Signal the current playback thread to stop and join it (together
    /// with any thread still fading out behind it). Leaves
    /// `run_state`/`status` untouched — callers decide what those should
    /// become afterward (a fresh `play()` vs. a real `stop()`).
    fn join_thread(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        self.command_tx = None;
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        for handle in self.retiring.drain(..) {
            let _ = handle.join();
        }
    }

    /// Whether a fade-out is worth doing right now: fades are enabled and
    /// audio is audibly running (not paused/stopped/still opening).
    fn can_fade_out(&self) -> bool {
        self.fade.enabled()
            && self.run_state.load(Ordering::Acquire) == RUN_PLAYING
            && !self.status.probing.load(Ordering::Acquire)
            && self.thread.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Ask the current thread to fade out and exit WITHOUT waiting for it.
    /// Returns every handle the caller is now responsible for joining
    /// (this thread plus earlier retirees).
    fn retire_gracefully(&mut self) -> Vec<thread::JoinHandle<()>> {
        self.graceful_stop.store(true, Ordering::Release);
        self.stop_flag.store(true, Ordering::Release);
        self.command_tx = None;
        let mut handles = std::mem::take(&mut self.retiring);
        if let Some(handle) = self.thread.take() {
            handles.push(handle);
        }
        handles
    }

    /// Start playing `source` on a fresh dedicated playback thread,
    /// replacing any previously running one. `replay_gain_db` is a
    /// precomputed dB adjustment, converted to a linear multiplier once
    /// here.
    ///
    /// With fades enabled and audio running, the old thread fades out on
    /// its own and the new thread waits for it before opening its output,
    /// so a manual skip is "fade out, then fade in" without blocking the
    /// caller. Otherwise the old thread is stopped and joined immediately.
    pub fn play(&mut self, source: PlaySource, replay_gain_db: Option<f32>) -> Result<()> {
        let prev = if self.can_fade_out() {
            self.retire_gracefully()
        } else {
            self.stop_flag.store(true, Ordering::Release);
            self.command_tx = None;
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
            // Predecessors that are already fading out are not waited for
            // here; the new thread joins them before it opens its output.
            std::mem::take(&mut self.retiring)
        };
        // Each thread owns its own stop flags: the retired one keeps
        // seeing `true` while the new one starts from `false`.
        self.stop_flag = Arc::new(AtomicBool::new(false));
        self.graceful_stop = Arc::new(AtomicBool::new(false));

        *self.next_track.lock() = None;
        self.status.reset_for_play();

        // Local files: open (a fast header probe) synchronously so a bad or
        // missing file surfaces immediately through this `Result`, matching
        // the previous backend's behaviour exactly. HTTP streams and live
        // radio streams: the connect + probe is genuinely slow (a real
        // network round trip), so it happens on the dedicated playback
        // thread instead — see `PlaySource::open_decoder` / `ThreadStart::Pending`.
        let start = match source {
            PlaySource::LocalFile(path) => match SymphoniaDecoder::open(&path) {
                Ok(decoder) => {
                    self.status
                        .duration_nanos
                        .store(secs_to_nanos(decoder.duration()), Ordering::Release);
                    ThreadStart::Ready(decoder)
                }
                Err(e) => {
                    // Keep tracking the threads that are still fading out.
                    self.retiring = prev;
                    return Err(e);
                }
            },
            pending_source => ThreadStart::Pending(pending_source),
        };

        let (tx, rx) = mpsc::channel();
        self.command_tx = Some(tx);
        let ctx = self.context();
        self.fade.request_fade_in();

        self.thread = Some(thread::spawn(move || {
            playback_thread_main(start, replay_gain_db, rx, ctx, prev);
        }));
        self.run_state.store(RUN_PLAYING, Ordering::Release);
        Ok(())
    }

    /// Pause playback (hot loop polls `run_state` and pauses the hardware
    /// stream within one decode chunk).
    pub fn pause(&self) {
        self.run_state.store(RUN_PAUSED, Ordering::Release);
    }

    /// Resume playback after a pause.
    pub fn resume(&self) {
        self.run_state.store(RUN_PLAYING, Ordering::Release);
    }

    /// Stop playback entirely. With fades enabled and audio running, the
    /// thread fades out and exits in the background (joined later); the
    /// caller is never blocked for the fade's duration.
    pub fn stop(&mut self) {
        if self.can_fade_out() {
            self.retiring = self.retire_gracefully();
        } else {
            self.join_thread();
        }
        self.run_state.store(RUN_STOPPED, Ordering::Release);
        *self.next_track.lock() = None;
        self.status.reset_for_stop();
    }

    /// Seek to `position` in the current track. Fire-and-forget: the
    /// playback thread applies it within one decode chunk, mirroring rmpd's
    /// non-blocking `PlaybackCommand::Seek`.
    pub fn seek(&self, position: Duration) -> Result<()> {
        match &self.command_tx {
            Some(tx) => tx
                .send(EngineCommand::Seek(position))
                .map_err(|_| PlayerError("playback thread is not running".to_owned())),
            None => Err(PlayerError("no active playback".to_owned())),
        }
    }

    /// Set the software volume, `0.0..=1.0`, converted to `VolumeFilter`'s
    /// live `0..=100` scale.
    pub fn set_volume(&self, volume: f32) {
        let scaled = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
        self.volume.store(scaled.min(100), Ordering::Release);
    }

    /// Set the crossfade duration in seconds (`0.0` = disabled/gapless-only).
    pub fn set_crossfade(&self, seconds: f32) {
        self.crossfade_bits
            .store(seconds.max(0.0).to_bits(), Ordering::Release);
    }

    /// Set the fade in/out duration in seconds (`0.0` = fades disabled).
    pub fn set_fade_secs(&self, seconds: f32) {
        self.fade.set_secs(seconds);
    }

    /// Pre-queue `source` for a gapless (claimed at end-of-stream) or
    /// crossfade (claimed early, once the current track nears its end)
    /// transition. Mirrors rmpd's `next_song` look-ahead slot; which
    /// mechanism actually fires is decided by the playback thread based on
    /// the current crossfade setting, not by the caller.
    pub fn queue_next(&self, source: PlaySource, replay_gain_db: Option<f32>) {
        *self.next_track.lock() = Some(PendingTrack {
            source,
            replay_gain_db,
        });
        // Matches the old `TrackBoundarySource`'s eager reset: queuing a next
        // track immediately un-signals "finished" for the currently playing
        // one, exactly as constructing a new wrapped source used to.
        self.status.track_finished.store(false, Ordering::Release);
    }

    /// Clear the look-ahead slot without affecting current playback.
    /// Call whenever the caller's prediction of "what comes next" changes
    /// (queue edit, shuffle/repeat toggle, jump) so the playback thread
    /// doesn't gapless/crossfade into a track that's no longer next.
    pub fn clear_next(&self) {
        *self.next_track.lock() = None;
    }

    /// Current playback position within the current track. Zero while the
    /// playback thread is still opening/probing the source.
    pub fn position(&self) -> Duration {
        if self.status.probing.load(Ordering::Acquire) {
            return Duration::ZERO;
        }
        Duration::from_nanos(self.status.position_nanos.load(Ordering::Acquire))
    }

    /// Duration of the current track. Zero while unknown (still probing, or
    /// a stream with no reported length, e.g. internet radio).
    pub fn duration(&self) -> Duration {
        Duration::from_nanos(self.status.duration_nanos.load(Ordering::Acquire))
    }

    /// Whether the current track has finished — a boundary was crossed
    /// (gapless/crossfade-advanced or truly ended) since the last `play()`/
    /// `queue_next()`. Always `false` while still probing.
    pub fn is_finished(&self) -> bool {
        if self.status.probing.load(Ordering::Acquire) {
            return false;
        }
        self.status.track_finished.load(Ordering::Acquire)
            || self.status.queue_exhausted.load(Ordering::Acquire)
    }

    /// Set (or clear) the shared PCM buffer the visualizer reads from.
    #[cfg(feature = "visualizer")]
    pub fn set_pcm_buffer(&self, buffer: Option<Arc<std::sync::Mutex<PcmBuffer>>>) {
        *self.pcm_buffer.lock() = buffer;
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        for handle in self.retiring.drain(..) {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Playback thread
// ---------------------------------------------------------------------------

/// Entry point run on the dedicated playback thread. Owns the decoder(s) for
/// the whole remaining lifetime of the queue: `'song` loops once per track,
/// only exiting on `stop()` or true end-of-queue (no pending track at EOS).
fn playback_thread_main(
    start: ThreadStart,
    replay_gain_db: Option<f32>,
    command_rx: Receiver<EngineCommand>,
    ctx: ThreadContext,
    prev: Vec<thread::JoinHandle<()>>,
) {
    // Captured before `resolve_start` consumes `start`: lets a network
    // hiccup on a live radio stream reconnect instead of the track being
    // treated as finished. Cleared the moment playback moves to a
    // genuinely different decoder (`Advance`, from either path below) —
    // see `run_pcm`'s own (finer-grained, in-place) handling for hiccups
    // that don't require leaving this function at all.
    let mut live_retry = live_retry_from_start(&start);

    let opened = resolve_start(start);

    // Wait for the predecessor(s) that are fading out; only after they have
    // closed their stream is it safe to open ours and to reset the shared
    // status they may have touched while winding down.
    if !prev.is_empty() {
        for handle in prev {
            let _ = handle.join();
        }
        ctx.status.track_finished.store(false, Ordering::Release);
        ctx.status.queue_exhausted.store(false, Ordering::Release);
        ctx.status.position_nanos.store(0, Ordering::Release);
    }

    let mut decoder = match opened {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("failed to open audio source: {e}");
            if !ctx.stop_flag.load(Ordering::Acquire) {
                ctx.status.probing.store(false, Ordering::Release);
                ctx.status.queue_exhausted.store(true, Ordering::Release);
            }
            return;
        }
    };
    let mut gain = replay_gain_db.map(db_to_linear).unwrap_or(1.0);

    'song: loop {
        if ctx.stop_flag.load(Ordering::Acquire) {
            break 'song;
        }

        // DSD/DoP decision tree (report §5): attempt DoP first; on failure,
        // fall back to DSD-to-PCM conversion and fall through to the PCM
        // path below. This check runs fresh for every decoder we advance
        // to, so a gapless/crossfade transition between a DSD and a PCM
        // track (in either direction) is handled correctly.
        let mut dsd_target_rate = None;
        if decoder.is_dsd() {
            match try_dop(&decoder) {
                Ok((encoder, output)) => match run_dsd(decoder, encoder, output, &command_rx, &ctx)
                {
                    DsdOutcome::Advance(next_decoder, next_gain) => {
                        decoder = next_decoder;
                        gain = next_gain;
                        live_retry = None;
                        continue 'song;
                    }
                    DsdOutcome::Done => break 'song,
                },
                Err(e) => {
                    tracing::warn!("DoP unavailable ({e}); falling back to DSD-to-PCM conversion");
                    let device_rate = CpalOutput::default_output_rate().unwrap_or(48_000);
                    let decode_rate = select_dsd_pcm_rate(device_rate, CpalOutput::supports_rate);
                    if let Err(e) = decoder.enable_pcm_conversion(decode_rate) {
                        tracing::error!("DSD-to-PCM conversion fallback failed: {e}");
                        break 'song;
                    }
                    let native_ok = cpal_utils::output_device_configured()
                        && CpalOutput::supports_rate(decode_rate);
                    dsd_target_rate = dsd_output_target_rate(decode_rate, device_rate, native_ok);
                }
            }
        }

        match run_pcm(
            decoder,
            gain,
            dsd_target_rate,
            live_retry.as_ref(),
            &command_rx,
            &ctx,
        ) {
            PcmOutcome::Advance(next_decoder, next_gain) => {
                decoder = next_decoder;
                gain = next_gain;
                live_retry = None;
                continue 'song;
            }
            PcmOutcome::Done => break 'song,
        }
    }

    // A thread that was told to stop must not flag "queue exhausted" for
    // whatever `play()`/`stop()` set the shared status up for since.
    if !ctx.stop_flag.load(Ordering::Acquire) {
        ctx.status.queue_exhausted.store(true, Ordering::Release);
    }
}

fn resolve_start(start: ThreadStart) -> Result<SymphoniaDecoder> {
    match start {
        ThreadStart::Ready(decoder) => Ok(decoder),
        ThreadStart::Pending(source) => source.open_decoder(),
    }
}

/// Open a pre-queued track's decoder, computing its linear gain. Logs and
/// returns `None` on failure (mirrors rmpd's `next_song` handling: a failed
/// open is treated as "nothing pending", never propagated as a hard error).
fn open_pending(pending: PendingTrack) -> Option<(SymphoniaDecoder, f32)> {
    let gain = pending.replay_gain_db.map(db_to_linear).unwrap_or(1.0);
    match pending.source.open_decoder() {
        Ok(decoder) => Some((decoder, gain)),
        Err(e) => {
            tracing::warn!("failed to open pre-queued next track: {e}");
            None
        }
    }
}

/// Build the DoP encoder + output from the decoder's own reported DSD
/// metadata. Building and starting the stream here means any failure (no
/// compatible hardware, device busy) surfaces as an `Err` so the caller can
/// cleanly fall back to DSD-to-PCM conversion. Mirrors rmpd's `setup_dop`.
fn try_dop(decoder: &SymphoniaDecoder) -> Result<(DopEncoder, DopOutput)> {
    let dsd_sample_rate = decoder.sample_rate();
    let channels = decoder.channels();
    let channel_layout = decoder
        .channel_data_layout()
        .unwrap_or(ChannelDataLayout::Planar);
    let bit_order = decoder.bit_order().unwrap_or(BitOrder::LsbFirst);

    let encoder = DopEncoder::new(
        dsd_sample_rate,
        channels as usize,
        channel_layout,
        bit_order,
    )?;
    let mut output = DopOutput::new(encoder.pcm_sample_rate(), channels)?;
    output.start()?;
    Ok((encoder, output))
}

// ---------------------------------------------------------------------------
// DSD/DoP path — structurally isolated: never touches EQ/volume/resampling
// ---------------------------------------------------------------------------

enum DsdOutcome {
    /// Advance to a freshly-opened decoder (the outer loop re-decides
    /// DSD-vs-PCM for it from scratch).
    Advance(SymphoniaDecoder, f32),
    Done,
}

/// DSD playback loop over an already-started DoP output. A single-track
/// loop by design (matching rmpd): DoP never crossfades (blending a DoP
/// bitstream would corrupt it) and never gapless-continues into another
/// *open* DoP stream — at EOS it just hands an already-opened next decoder
/// back to the outer loop, which pays the (small, unavoidable) cost of a
/// fresh DAC primer/reset sequence for the new stream. Never touches
/// [`EqFilter`], [`VolumeFilter`], gain, or [`super::resampler::StreamResampler`] —
/// that structural separation is what makes "DoP reaches the DAC
/// unprocessed" a guarantee rather than a runtime flag.
fn run_dsd(
    mut decoder: SymphoniaDecoder,
    mut encoder: DopEncoder,
    mut output: DopOutput,
    command_rx: &Receiver<EngineCommand>,
    ctx: &ThreadContext,
) -> DsdOutcome {
    // DoP can't be gain-scaled: swallow any pending fade-in request so a
    // later PCM stream doesn't inherit it.
    let _ = ctx.fade.take_fade_in_pending();
    let dsd_sample_rate = decoder.sample_rate();
    let channels = decoder.channels();
    let bytes_per_second = (dsd_sample_rate / 8) as u64 * channels.max(1) as u64;

    ctx.status.probing.store(false, Ordering::Release);
    ctx.status
        .duration_nanos
        .store(secs_to_nanos(decoder.duration()), Ordering::Release);

    let mut dsd_buf = Vec::new();
    let mut dop_buf = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut paused = false;

    loop {
        if ctx.stop_flag.load(Ordering::Acquire) {
            let _ = output.stop();
            return DsdOutcome::Done;
        }

        if let Ok(EngineCommand::Seek(pos)) = command_rx.try_recv() {
            match decoder.seek(pos.as_secs_f64()) {
                Ok(()) => {
                    total_bytes = (pos.as_secs_f64() * bytes_per_second as f64) as u64;
                    ctx.status
                        .position_nanos
                        .store(pos.as_nanos() as u64, Ordering::Release);
                }
                Err(e) => tracing::error!("seek failed (DSD): {e}"),
            }
        }

        let is_paused = ctx.run_state.load(Ordering::Acquire) == RUN_PAUSED;
        if is_paused {
            if !paused {
                let _ = output.pause();
                paused = true;
            }
            thread::sleep(Duration::from_millis(100));
            continue;
        } else if paused {
            let _ = output.resume();
            paused = false;
        }

        let bytes_read = match decoder.read_dsd_raw(&mut dsd_buf) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("DSD read error: {e}");
                let _ = output.stop();
                return DsdOutcome::Done;
            }
        };

        if bytes_read == 0 {
            ctx.status.track_finished.store(true, Ordering::Release);
            let next = ctx.next_track.lock().take().and_then(open_pending);
            let _ = output.stop();
            return match next {
                Some((next_decoder, next_gain)) => DsdOutcome::Advance(next_decoder, next_gain),
                None => DsdOutcome::Done,
            };
        }

        encoder.encode(&dsd_buf, &mut dop_buf);
        if let Err(e) = output.write(&dop_buf) {
            tracing::warn!("DoP output write failed: {e}");
            let _ = output.stop();
            return DsdOutcome::Done;
        }

        total_bytes += bytes_read as u64;
        ctx.status.position_nanos.store(
            units_to_nanos(total_bytes, bytes_per_second),
            Ordering::Release,
        );
    }
}

// ---------------------------------------------------------------------------
// PCM path — ReplayGain → EQ → volume → output, with gapless/crossfade
// ---------------------------------------------------------------------------

enum PcmOutcome {
    /// Advance to a freshly-opened decoder that needs a rebuilt output
    /// (format changed, or it turned out to be DSD).
    Advance(SymphoniaDecoder, f32),
    Done,
}

/// The live PCM output pipeline: hardware output, EQ, and volume, bundled
/// together since every decoded chunk (normal decode or a crossfade blend)
/// runs through the exact same gain → EQ → volume → write sequence.
///
/// The visualizer tap no longer lives in this pipeline: it moved to
/// [`CpalOutput`]'s cpal device callback (see `output.rs`'s module docs),
/// wired once via `CpalOutput::set_viz_tap` in [`Self::new`] below, so it
/// sees exactly what's handed to the device, at the moment it's handed
/// over — not whatever left the decode thread up to
/// [`DEFAULT_BUFFER_TIME_MS`] sooner.
struct PcmSink {
    eq: EqFilter,
    volume: VolumeFilter,
    output: CpalOutput,
    paused: bool,
    fade: Arc<FadeControl>,
    stop_flag: Arc<AtomicBool>,
    graceful_stop: Arc<AtomicBool>,
    /// While a fade-out ahead of a pause is running: when the stream is
    /// really paused (the fade has reached silence).
    pause_fade_until: Option<Instant>,
}

impl PcmSink {
    fn new(
        format: DecoderFormat,
        dsd_target_rate: Option<u32>,
        ctx: &ThreadContext,
    ) -> Result<Self> {
        let out_format = OutputFormat {
            sample_rate: format.sample_rate,
            channels: format.channels,
            bits_per_sample: format.bits_per_sample,
        };
        let mut output = match dsd_target_rate {
            Some(rate) => CpalOutput::with_target_rate(
                out_format,
                ResamplerQuality::default(),
                DEFAULT_BUFFER_TIME_MS,
                rate,
            )?,
            None => CpalOutput::new(
                out_format,
                ResamplerQuality::default(),
                DEFAULT_BUFFER_TIME_MS,
            )?,
        };
        // Wire the visualizer tap before `start()` builds the cpal stream
        // closures below — they capture a clone of this slot at build
        // time, so it must be set first.
        #[cfg(feature = "visualizer")]
        output.set_viz_tap(ctx.pcm_buffer.clone());
        // Fade ramp: runs in the device callback (see `fade.rs`). Armed
        // here, before `start()` captures it. A pending request from
        // `play()` makes THIS stream start silent and fade in; format-
        // change rebuilds during gapless advances start at unity.
        let fade_in = ctx.fade.take_fade_in_pending();
        ctx.fade.arm_stream_start(fade_in);
        output.set_fade(ctx.fade.clone());
        output.start()?;

        let channels =
            NonZero::new(format.channels.max(1) as u16).expect("channels.max(1) is never zero");
        Ok(Self {
            eq: EqFilter::new(channels, ctx.eq_coeffs.clone(), ctx.eq_bypass.clone()),
            volume: VolumeFilter::new(ctx.volume.clone()),
            output,
            paused: false,
            fade: ctx.fade.clone(),
            stop_flag: ctx.stop_flag.clone(),
            graceful_stop: ctx.graceful_stop.clone(),
            pause_fade_until: None,
        })
    }

    /// Apply EQ, volume, and write — for a buffer that already carries its
    /// final gain (e.g. an already-blended crossfade chunk, or a plain
    /// decode chunk after `process` scales it). `ctx` isn't read here
    /// anymore (the visualizer tap moved to `CpalOutput`'s callback — see
    /// the struct docs above) but stays a parameter so every call site
    /// looks the same regardless.
    fn finish(&mut self, buf: &mut [f32], ctx: &ThreadContext) -> Result<()> {
        self.eq.apply(buf);
        self.volume.apply(buf);
        let _ = ctx;
        self.output.write(buf).map(|_| ())
    }

    /// Apply ReplayGain, then EQ/volume/tap/write via [`Self::finish`].
    fn process(&mut self, buf: &mut [f32], gain: f32, ctx: &ThreadContext) -> Result<()> {
        for s in buf.iter_mut() {
            *s *= gain;
        }
        self.finish(buf, ctx)
    }

    /// Poll the shared run state; pauses/resumes the hardware stream on a
    /// transition. Returns `true` when the caller should skip decoding this
    /// iteration (currently paused).
    ///
    /// With fades enabled a pause first ramps the gain to silence (in the
    /// device callback, so it is audible at once) while decoding carries
    /// on; only when the ramp has finished is the stream really paused.
    fn poll_pause(&mut self, ctx: &ThreadContext) -> bool {
        let is_paused = ctx.run_state.load(Ordering::Acquire) == RUN_PAUSED;
        if is_paused {
            if !self.paused {
                if self.fade.enabled() {
                    match self.pause_fade_until {
                        None => {
                            self.fade.fade_out();
                            self.pause_fade_until =
                                Some(Instant::now() + Duration::from_secs_f32(self.fade.secs()));
                            return false;
                        }
                        Some(deadline) if Instant::now() < deadline => return false,
                        Some(_) => {}
                    }
                }
                let _ = self.output.pause();
                self.paused = true;
                self.pause_fade_until = None;
            }
            thread::sleep(Duration::from_millis(100));
            true
        } else {
            let was_fading = self.pause_fade_until.take().is_some();
            if self.paused {
                let _ = self.output.resume();
                self.paused = false;
                // The gain is at silence after a faded pause; ramp back up
                // (an instant jump when fades have since been disabled).
                self.fade.fade_in();
            } else if was_fading {
                // Resumed before the pause-fade finished.
                self.fade.fade_in();
            }
            false
        }
    }

    /// Close the output. When this thread was retired gracefully (see
    /// `PlaybackEngine::retire_gracefully`) the buffered audio is faded to
    /// silence first. Capped below the output's buffer depth since nothing
    /// is decoded during this wait.
    fn stop(&mut self) {
        if self.fade.enabled()
            && !self.paused
            && self.stop_flag.load(Ordering::Acquire)
            && self.graceful_stop.load(Ordering::Acquire)
        {
            let secs = self
                .fade
                .secs()
                .min(MAX_FADE_SECS)
                .min(DEFAULT_BUFFER_TIME_MS as f32 / 1000.0 * 0.9);
            self.fade.ramp_to(0.0, secs);
            thread::sleep(Duration::from_secs_f32(secs));
        }
        let _ = self.output.stop();
    }
}

/// PCM decode loop, handling every consecutive track whose format matches
/// the one this output was opened for. Returns [`PcmOutcome::Advance`] (with
/// the output already torn down) the moment a transition needs a different
/// output — a format change, or the next track being DSD — so the caller
/// can rebuild appropriately; returns internally via `continue 'buf` for
/// same-format gapless/crossfade transitions, which is what keeps the
/// device open (no click) across an ordinary track change.
fn run_pcm(
    mut decoder: SymphoniaDecoder,
    mut gain: f32,
    dsd_target_rate: Option<u32>,
    live_retry: Option<&LiveRetry>,
    command_rx: &Receiver<EngineCommand>,
    ctx: &ThreadContext,
) -> PcmOutcome {
    let format = decoder.format();
    let mut sink = match PcmSink::new(format, dsd_target_rate, ctx) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to open PCM output: {e}");
            return PcmOutcome::Done;
        }
    };

    ctx.status.probing.store(false, Ordering::Release);
    ctx.status
        .duration_nanos
        .store(secs_to_nanos(decoder.duration()), Ordering::Release);

    let mut buffer = vec![0f32; BUFFER_SIZE];
    let mut total_samples: u64 = 0;
    let samples_per_second = format.sample_rate as u64 * format.channels.max(1) as u64;
    // Owned, reassignable copy of the caller's reconnect ingredients:
    // cleared the moment playback moves on to a different (pre-queued)
    // track within this same call, so a later hiccup on THAT track never
    // mistakenly retries the original live stream's URL instead.
    let mut live_retry: Option<LiveRetry> = live_retry.cloned();
    'buf: loop {
        if ctx.stop_flag.load(Ordering::Acquire) {
            sink.stop();
            return PcmOutcome::Done;
        }

        if let Ok(EngineCommand::Seek(pos)) = command_rx.try_recv() {
            apply_seek(
                &mut decoder,
                pos,
                samples_per_second,
                &mut total_samples,
                ctx,
            );
            sink.eq.reset_states();
        }

        if sink.poll_pause(ctx) {
            continue 'buf;
        }

        // Crossfade look-ahead (report §7). Dormant when crossfade_secs ==
        // 0 (the default): this whole block is skipped and behaviour is
        // identical to plain gapless.
        let crossfade_secs = f32::from_bits(ctx.crossfade_bits.load(Ordering::Acquire));
        if crossfade_secs > 0.0
            && let Some(duration) = decoder.duration()
        {
            let cf_start =
                ((duration - crossfade_secs as f64) * samples_per_second as f64).max(0.0) as u64;
            if total_samples >= cf_start
                && let Some(pending) = ctx.next_track.lock().take()
            {
                match open_pending(pending) {
                    Some((next_decoder, next_gain))
                        if !next_decoder.is_dsd() && next_decoder.format() == format =>
                    {
                        let window = crossfade::window_samples_secs(
                            format.sample_rate,
                            format.channels,
                            crossfade_secs,
                        ) as u64;
                        match run_crossfade(
                            &mut decoder,
                            next_decoder,
                            gain,
                            next_gain,
                            window,
                            &mut buffer,
                            &mut sink,
                            command_rx,
                            ctx,
                        ) {
                            CrossfadeOutcome::Transitioned {
                                decoder: nd,
                                gain: ng,
                                samples_played,
                            } => {
                                ctx.status.track_finished.store(true, Ordering::Release);
                                live_retry = None;
                                decoder = nd;
                                gain = ng;
                                total_samples = samples_played;
                                sink.eq.reset_states();
                                ctx.status
                                    .duration_nanos
                                    .store(secs_to_nanos(decoder.duration()), Ordering::Release);
                                ctx.status.position_nanos.store(
                                    units_to_nanos(total_samples, samples_per_second),
                                    Ordering::Release,
                                );
                                continue 'buf;
                            }
                            CrossfadeOutcome::Abandoned => continue 'buf,
                            CrossfadeOutcome::Stopped => {
                                sink.stop();
                                return PcmOutcome::Done;
                            }
                        }
                    }
                    Some((next_decoder, next_gain)) => {
                        // Either side is DSD, or the format differs: never
                        // blend — hard-cut now instead of discarding an
                        // already-opened decoder and waiting for a literal
                        // EOS that would just hard-cut a few seconds later
                        // anyway.
                        ctx.status.track_finished.store(true, Ordering::Release);
                        sink.stop();
                        return PcmOutcome::Advance(next_decoder, next_gain);
                    }
                    None => {}
                }
            }
        }

        let samples_read = match decoder.read(&mut buffer) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("decode error: {e}");
                // A read error on an ordinary source is fatal. On a live
                // radio stream it's almost always a transient network
                // hiccup — reconnect (same URL, fresh connection) instead
                // of treating the whole track as finished.
                if let Some(outcome) = try_reconnect(live_retry.as_ref(), format, gain, ctx) {
                    match outcome {
                        ReconnectOutcome::SameFormat(nd) => {
                            decoder = nd;
                            total_samples = 0;
                            sink.eq.reset_states();
                            ctx.status.position_nanos.store(0, Ordering::Release);
                            ctx.status
                                .duration_nanos
                                .store(secs_to_nanos(decoder.duration()), Ordering::Release);
                            continue 'buf;
                        }
                        ReconnectOutcome::FormatChanged(nd, ng) => {
                            sink.stop();
                            return PcmOutcome::Advance(nd, ng);
                        }
                    }
                }
                sink.stop();
                return PcmOutcome::Done;
            }
        };

        if samples_read == 0 {
            // EOS: claim a pre-fetched next track regardless of format/DSD
            // -ness — unlike rmpd, aulos rebuilds the output on a format
            // change instead of refusing the gapless advance outright.
            let gapless_next = ctx.next_track.lock().take().and_then(open_pending);

            match gapless_next {
                Some((next_decoder, next_gain))
                    if !next_decoder.is_dsd() && next_decoder.format() == format =>
                {
                    ctx.status.track_finished.store(true, Ordering::Release);
                    live_retry = None;
                    decoder = next_decoder;
                    gain = next_gain;
                    total_samples = 0;
                    sink.eq.reset_states();
                    ctx.status.position_nanos.store(0, Ordering::Release);
                    ctx.status
                        .duration_nanos
                        .store(secs_to_nanos(decoder.duration()), Ordering::Release);
                    continue 'buf;
                }
                Some((next_decoder, next_gain)) => {
                    ctx.status.track_finished.store(true, Ordering::Release);
                    sink.stop();
                    return PcmOutcome::Advance(next_decoder, next_gain);
                }
                None => {
                    // No explicit next track queued. For an ordinary
                    // finite source this really is the end. For live
                    // radio, though, a bare EOS almost always means the
                    // connection dropped rather than the "track" having
                    // a natural end — internet radio has no such thing.
                    // Try to reconnect BEFORE ever setting
                    // `track_finished`: it's read by the UI thread's
                    // ~500ms poll (`Player::is_finished`), and a
                    // reconnect attempt (with backoff) can take longer
                    // than that poll interval, so setting the flag first
                    // — then clearing it again on success — would leave
                    // a window where the UI could see a spurious
                    // "finished" and advance/stop the queue anyway.
                    if let Some(outcome) = try_reconnect(live_retry.as_ref(), format, gain, ctx) {
                        match outcome {
                            ReconnectOutcome::SameFormat(nd) => {
                                decoder = nd;
                                total_samples = 0;
                                sink.eq.reset_states();
                                ctx.status.position_nanos.store(0, Ordering::Release);
                                ctx.status
                                    .duration_nanos
                                    .store(secs_to_nanos(decoder.duration()), Ordering::Release);
                                continue 'buf;
                            }
                            ReconnectOutcome::FormatChanged(nd, ng) => {
                                ctx.status.track_finished.store(true, Ordering::Release);
                                sink.stop();
                                return PcmOutcome::Advance(nd, ng);
                            }
                        }
                    }
                    ctx.status.track_finished.store(true, Ordering::Release);
                    sink.stop();
                    return PcmOutcome::Done;
                }
            }
        }

        let chunk = &mut buffer[..samples_read];
        if let Err(e) = sink.process(chunk, gain, ctx) {
            tracing::warn!("output write failed: {e}");
            sink.stop();
            return PcmOutcome::Done;
        }

        total_samples += samples_read as u64;
        ctx.status.position_nanos.store(
            units_to_nanos(total_samples, samples_per_second),
            Ordering::Release,
        );
    }
}

enum CrossfadeOutcome {
    /// The blend completed (window exhausted, or the outgoing track ended
    /// inside it) — caller should switch to `decoder`.
    Transitioned {
        decoder: SymphoniaDecoder,
        gain: f32,
        samples_played: u64,
    },
    /// A seek (or the incoming track being shorter than the window)
    /// aborted the blend; `next_decoder` is dropped and the caller
    /// continues with the current decoder untouched.
    Abandoned,
    /// The output disconnected; caller should stop entirely.
    Stopped,
}

/// Equal-power crossfade blend loop (report §7): reads matched-length
/// chunks from both decoders, mixes with
/// [`crossfade::equal_power_gains`]/[`crossfade::mix_into`], and writes the
/// blended chunk to `sink`. PCM-only by construction — the caller only
/// invokes this once it has already confirmed neither side is DSD and both
/// share a format.
#[allow(clippy::too_many_arguments)]
fn run_crossfade(
    decoder: &mut SymphoniaDecoder,
    mut next_decoder: SymphoniaDecoder,
    gain: f32,
    next_gain: f32,
    window: u64,
    cur_buf: &mut [f32],
    sink: &mut PcmSink,
    command_rx: &Receiver<EngineCommand>,
    ctx: &ThreadContext,
) -> CrossfadeOutcome {
    let mut next_buf = vec![0f32; cur_buf.len()];
    let mut overlap_done: u64 = 0;
    let mut next_played: u64 = 0;

    loop {
        if ctx.stop_flag.load(Ordering::Acquire) {
            return CrossfadeOutcome::Stopped;
        }

        if let Ok(EngineCommand::Seek(pos)) = command_rx.try_recv() {
            if let Err(e) = decoder.seek(pos.as_secs_f64()) {
                tracing::error!("seek failed during crossfade: {e}");
            } else {
                ctx.status
                    .position_nanos
                    .store(pos.as_nanos() as u64, Ordering::Release);
            }
            sink.eq.reset_states();
            // next_decoder is dropped here; the caller's next_track slot is
            // already empty so a subsequent crossfade window needs a fresh
            // `queue_next()` from the controlling side.
            return CrossfadeOutcome::Abandoned;
        }

        if sink.poll_pause(ctx) {
            continue;
        }

        if overlap_done >= window {
            return CrossfadeOutcome::Transitioned {
                decoder: next_decoder,
                gain: next_gain,
                samples_played: next_played,
            };
        }

        let n_cur = match decoder.read(cur_buf) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("decode error during crossfade: {e}");
                0
            }
        };
        if n_cur == 0 {
            // Outgoing ended inside the window — switch fully now.
            return CrossfadeOutcome::Transitioned {
                decoder: next_decoder,
                gain: next_gain,
                samples_played: next_played,
            };
        }

        let n_next = match next_decoder.read(&mut next_buf[..n_cur]) {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("decode error on incoming crossfade track: {e}");
                0
            }
        };
        if n_next == 0 {
            // Incoming track shorter than the crossfade window: play the
            // remaining outgoing tail unmodified and abandon the blend.
            let tail = &mut cur_buf[..n_cur];
            if sink.process(tail, gain, ctx).is_err() {
                return CrossfadeOutcome::Stopped;
            }
            return CrossfadeOutcome::Abandoned;
        }

        let n_mix = n_next;
        let progress = (overlap_done as f32 / window.max(1) as f32).clamp(0.0, 1.0);
        let (g_out, g_in) = crossfade::equal_power_gains(progress);

        let mix = &mut cur_buf[..n_mix];
        for s in mix.iter_mut() {
            *s *= gain * g_out;
        }
        crossfade::mix_into(mix, &next_buf[..n_mix], 1.0, next_gain * g_in);

        if sink.finish(mix, ctx).is_err() {
            return CrossfadeOutcome::Stopped;
        }

        overlap_done += n_mix as u64;
        next_played += n_mix as u64;
        // Position keeps tracking the OUTGOING track's sample count during
        // the blend — the UI's "current track" identity only switches once
        // `Transitioned` is returned, matching where `track_finished` flips.
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn apply_seek(
    decoder: &mut SymphoniaDecoder,
    position: Duration,
    samples_per_second: u64,
    total_samples: &mut u64,
    ctx: &ThreadContext,
) {
    match decoder.seek(position.as_secs_f64()) {
        Ok(()) => {
            *total_samples = (position.as_secs_f64() * samples_per_second as f64).round() as u64;
            ctx.status
                .position_nanos
                .store(position.as_nanos() as u64, Ordering::Release);
        }
        Err(e) => tracing::error!("seek failed: {e}"),
    }
}

fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Convert a running unit count (samples or DSD bytes) at `units_per_second`
/// into nanoseconds, without overflowing for very long tracks.
fn units_to_nanos(units: u64, units_per_second: u64) -> u64 {
    if units_per_second == 0 {
        return 0;
    }
    (u128::from(units) * 1_000_000_000u128 / u128::from(units_per_second)) as u64
}

fn secs_to_nanos(secs: Option<f64>) -> u64 {
    secs.map(|s| (s.max(0.0) * 1_000_000_000.0) as u64)
        .unwrap_or(0)
}
