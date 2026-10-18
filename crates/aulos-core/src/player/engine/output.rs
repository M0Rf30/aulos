// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! cpal-backed PCM audio output.
//!
//! Builds one of three near-identical `cpal::Stream` variants (F32/I16/I32)
//! fed by a [`SampleBuffer`] pull adapter; `write` resamples to the device's
//! negotiated rate when required and blocks on a bounded channel send, which
//! is the actual backpressure primitive pacing the decode thread to
//! real-time. The small `AudioOutput`/`PauseState` seam lives in this file
//! too since `engine/mod.rs` doesn't declare a separate module for it.
//!
//! With the `visualizer` feature, each of the three callback closures also
//! taps the exact samples it just handed to the device this period into
//! [`crate::player::pcm::PcmBuffer`] (see
//! [`CpalOutput::set_viz_tap`]/`tap_viz`) — deliberately *here*, in the
//! realtime device callback, rather than on the decode thread that fills
//! the ~500ms-deep `sync_channel` above: tapping on the decode side would
//! let the visualizer see (and react to) audio up to that whole buffer
//! depth before it's actually heard.

use crate::player::backend::PlayerError;
use crate::player::engine::conversion::{self, SampleBuffer};
use crate::player::engine::cpal_utils::CpalDeviceConfig;
use crate::player::engine::fade::{FadeControl, FadeRamp};
use crate::player::engine::resampler::{ResamplerQuality, StreamResampler};
#[cfg(feature = "visualizer")]
use crate::player::pcm::PcmBuffer;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::time::Duration;

pub type Result<T, E = PlayerError> = std::result::Result<T, E>;

/// What the user is told when the output device disappears or is
/// invalidated under a running stream (unplugged, reconfigured, server gone).
pub(crate) const DEVICE_LOST_MESSAGE: &str = "The audio output device was lost or changed";

/// Whether a cpal stream error means the stream can no longer deliver audio.
/// A reroute to another default device keeps the stream alive, and a
/// buffer xrun or a refused real-time priority only glitch it; anything
/// else (device gone, stream invalidated, backend failure) is permanent.
pub(crate) fn is_fatal_stream_error(kind: cpal::ErrorKind) -> bool {
    !matches!(
        kind,
        cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied
    )
}

/// The cpal stream error callback shared by the PCM and DoP outputs: logs,
/// and on a fatal error raises `failed` for the playback thread to see
/// (instead of it noticing seconds later through a stalled write).
pub(crate) fn stream_error_callback(
    label: &'static str,
    failed: Arc<AtomicBool>,
) -> impl FnMut(cpal::Error) + Send + 'static {
    move |err| {
        tracing::error!("{label} error: {err}");
        if is_fatal_stream_error(err.kind()) {
            failed.store(true, Ordering::Release);
        }
    }
}

/// The engine's shared visualizer tap slot (`ThreadContext::pcm_buffer`):
/// the outer lock is shared with the controlling side, and the inner
/// `Option` is flipped on visualizer activate/deactivate.
#[cfg(feature = "visualizer")]
pub type VizTapSlot = Arc<parking_lot::Mutex<Option<Arc<std::sync::Mutex<PcmBuffer>>>>>;

/// Minimal decoded-stream format descriptor: sample rate, channel count, and
/// bit depth. Self-contained here (no dependency on `decoder.rs`, which is a
/// sibling module ported separately); the later wiring phase reconciles this
/// with whatever shape `decoder.rs` exposes (same fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u32,
}

/// Tracks pause state for output backends with simple flag-based pausing.
///
/// Backends that need hardware-level pause (e.g. cpal stream control) should
/// override the trait methods instead of relying on these defaults.
#[derive(Debug, Default)]
pub struct PauseState {
    paused: bool,
}

impl PauseState {
    pub fn new() -> Self {
        Self { paused: false }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }
}

/// An audio output backend.
///
/// All methods are called from a blocking (non-async) thread.
pub trait AudioOutput: Send {
    /// Open the output device / file / pipe and prepare for playback.
    fn start(&mut self) -> Result<()>;

    /// Write interleaved f32 PCM samples (range −1.0 … +1.0).
    fn write(&mut self, samples: &[f32]) -> Result<()>;

    /// Stop playback and close the underlying resource.
    fn stop(&mut self) -> Result<()>;

    /// Access the embedded [`PauseState`]. Required for default
    /// `pause` / `resume` / `is_paused` implementations.
    fn pause_state(&self) -> &PauseState;

    /// Mutable access to the embedded [`PauseState`].
    fn pause_state_mut(&mut self) -> &mut PauseState;

    /// Pause: stop consuming samples (silence / no-op writes).
    fn pause(&mut self) -> Result<()> {
        self.pause_state_mut().set_paused(true);
        Ok(())
    }

    /// Resume after a pause.
    fn resume(&mut self) -> Result<()> {
        self.pause_state_mut().set_paused(false);
        Ok(())
    }

    /// Whether the output is currently paused.
    fn is_paused(&self) -> bool {
        self.pause_state().is_paused()
    }
}

/// How long [`CpalOutput::write`] waits for room in the sample channel
/// before declaring the device stalled.
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Poll interval while waiting on the sample channel.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Wait until everything queued on `sender`'s channel has been taken by the
/// device callback, within `timeout`. Returns `true` when drained, `false`
/// on timeout, disconnect or when `stop` was raised.
///
/// The sender cannot observe the receiver's backlog, so this queues `tail`
/// (if any) and then `depth` empty chunks: the channel holds at most `depth`
/// chunks, so once the last empty one is accepted every chunk queued before
/// it has been consumed. An empty chunk plays back as a single silent
/// sample, which is harmless at a stream end.
pub(crate) fn drain_channel<T>(
    sender: &SyncSender<Vec<T>>,
    tail: Option<Vec<T>>,
    depth: usize,
    timeout: Duration,
    stop: Option<&AtomicBool>,
) -> bool {
    let deadline = web_time::Instant::now() + timeout;
    let chunks = tail
        .into_iter()
        .chain(std::iter::repeat_with(Vec::new).take(depth));
    for mut chunk in chunks {
        loop {
            match sender.try_send(chunk) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return false,
                Err(TrySendError::Full(back)) => {
                    if stop.is_some_and(|flag| flag.load(Ordering::Acquire))
                        || web_time::Instant::now() >= deadline
                    {
                        return false;
                    }
                    chunk = back;
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
        }
    }
    true
}

/// Sleep for `duration` in short steps, returning early once `stop` is raised.
pub(crate) fn sleep_unless_stopped(duration: Duration, stop: Option<&AtomicBool>) {
    let deadline = web_time::Instant::now() + duration;
    while web_time::Instant::now() < deadline
        && !stop.is_some_and(|flag| flag.load(Ordering::Acquire))
    {
        std::thread::sleep(POLL_INTERVAL);
    }
}

pub struct CpalOutput {
    device: Device,
    stream: Option<Stream>,
    sample_sender: Option<SyncSender<Vec<f32>>>,
    config: StreamConfig,
    pause_state: PauseState,
    resampler: Option<StreamResampler>,
    /// Output buffer time in milliseconds; sizes the sync-channel depth.
    buffer_time_ms: u32,
    /// Depth of the live sample channel (set by [`Self::start`]); [`Self::stop`]
    /// needs it to wait for the channel to drain.
    channel_depth: usize,
    /// Length (samples, device rate) of the last chunk sent: the device
    /// callback may still be playing it when the channel reads as empty.
    last_chunk_len: usize,
    /// The engine's stop flag (see [`Self::set_stop_flag`]): aborts the
    /// blocking waits in [`Self::write`] and [`Self::stop`].
    stop_flag: Option<Arc<AtomicBool>>,
    /// Raised by the cpal error callback on a fatal stream error (device
    /// unplugged or invalidated); see [`Self::device_failed`].
    device_failed: Arc<AtomicBool>,
    /// Visualizer tap slot, wired via [`Self::set_viz_tap`] before
    /// [`Self::start`]. Doubly-`Option`-wrapped: the outer `Arc<Mutex<_>>`
    /// is `ThreadContext::pcm_buffer` itself, shared with the controlling
    /// side, so UI-side visualizer activate/deactivate (flipping the
    /// *inner* `Option`) takes effect on the very next cpal callback with
    /// no stream rebuild. `None` for a `CpalOutput` that never opts in
    /// (visualizer feature/tap not wired, or DoP/test callers).
    #[cfg(feature = "visualizer")]
    viz_tap: Option<VizTapSlot>,
    /// Fade ramp control shared with the engine (see [`Self::set_fade`]).
    fade: Option<Arc<FadeControl>>,
}

impl CpalOutput {
    pub fn new(
        format: AudioFormat,
        quality: ResamplerQuality,
        buffer_time_ms: u32,
    ) -> Result<Self> {
        Self::build(format, quality, buffer_time_ms, format.sample_rate)
    }

    /// Open the cpal stream at `target_device_rate` instead of
    /// `format.sample_rate`. The built-in resampler bridges the gap when they
    /// differ. Used by the DSD-to-PCM path to drive cpal at the device's native
    /// rate (e.g. 48000 Hz) rather than an advertised-but-resampled rate
    /// (e.g. 88200 Hz on a PipeWire 48 kHz graph), which prevents buffer
    /// underruns and keeps DSD ultrasonic shaped noise out of the audible band.
    pub fn with_target_rate(
        format: AudioFormat,
        quality: ResamplerQuality,
        buffer_time_ms: u32,
        target_device_rate: u32,
    ) -> Result<Self> {
        Self::build(format, quality, buffer_time_ms, target_device_rate)
    }

    fn build(
        format: AudioFormat,
        quality: ResamplerQuality,
        buffer_time_ms: u32,
        requested_device_rate: u32,
    ) -> Result<Self> {
        let device_config = CpalDeviceConfig::new(requested_device_rate, format.channels as u16)?;

        // If the device could not take the requested rate, CpalDeviceConfig
        // selected a supported one; resample to bridge the difference so the
        // file plays regardless of hardware constraints.
        let device_rate = device_config.config.sample_rate;
        let resampler = if device_rate != format.sample_rate {
            tracing::info!(
                "output device does not support {} Hz; resampling to {} Hz ({:?})",
                format.sample_rate,
                device_rate,
                quality
            );
            let rs = StreamResampler::new(
                format.sample_rate,
                device_rate,
                format.channels as usize,
                quality,
            );
            if rs.is_none() {
                tracing::error!(
                    "failed to build resampler {} -> {} Hz; audio may play at the wrong speed",
                    format.sample_rate,
                    device_rate
                );
            }
            rs
        } else {
            None
        };

        Ok(Self {
            device: device_config.device,
            stream: None,
            sample_sender: None,
            config: device_config.config,
            pause_state: PauseState::new(),
            resampler,
            buffer_time_ms,
            channel_depth: 0,
            last_chunk_len: 0,
            stop_flag: None,
            device_failed: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "visualizer")]
            viz_tap: None,
            fade: None,
        })
    }

    /// Whether the device reported a fatal stream error. Sticky for this
    /// output's lifetime: the playback thread polls it so a lost device
    /// ends playback at once, not after a stalled write times out.
    pub fn device_failed(&self) -> bool {
        self.device_failed.load(Ordering::Acquire)
    }

    /// Wire the shared visualizer tap slot (`ThreadContext::pcm_buffer`).
    /// Must be called before [`Self::start`]: the cpal callback closures
    /// capture a clone of this at stream-build time, so a call after
    /// `start()` has no effect on the already-built stream. Cloning only
    /// the outer `Arc` is enough — the render side flips the *inner*
    /// `Option` on visualizer activate/deactivate, which the callback
    /// observes on its very next invocation.
    #[cfg(feature = "visualizer")]
    pub fn set_viz_tap(&mut self, tap: VizTapSlot) {
        self.viz_tap = Some(tap);
    }

    /// Wire the shared fade control. Must be called before [`Self::start`]
    /// (the callback closures capture it at stream-build time). The ramp
    /// runs in the realtime device callback so a fade is audible
    /// immediately instead of ~500 ms later (the depth of the sample
    /// channel between the decode thread and the device).
    pub fn set_fade(&mut self, fade: Arc<FadeControl>) {
        self.fade = Some(fade);
    }

    /// Wire the engine's stop flag. While it is set, [`Self::write`] stops
    /// waiting for channel space and [`Self::stop`] skips draining the
    /// buffered tail (the stream is being torn down on purpose).
    pub fn set_stop_flag(&mut self, stop_flag: Arc<AtomicBool>) {
        self.stop_flag = Some(stop_flag);
    }

    fn stop_requested(&self) -> bool {
        self.stop_flag
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    /// Build the per-stream fade ramp for the callback closures. With no
    /// control wired this is a unity-gain no-op ramp.
    fn new_fade_ramp(&self) -> FadeRamp {
        let control = self.fade.clone().unwrap_or_default();
        FadeRamp::new(
            control,
            self.config.sample_rate,
            self.config.channels as usize,
        )
    }

    /// Whether the default output device natively supports `rate`. Lets callers
    /// prefer a bit-exact rate before falling back to resampling.
    pub fn supports_rate(rate: u32) -> bool {
        CpalDeviceConfig::default_device_supports_rate(rate)
    }

    /// The default output device's preferred sample rate (Hz), if known.
    pub fn default_output_rate() -> Option<u32> {
        CpalDeviceConfig::default_output_rate()
    }

    /// Feed `channels`-interleaved samples that were just handed to the
    /// device this callback period into the visualizer's ring buffer, if a
    /// tap is wired and both locks are free.
    ///
    /// This runs on the realtime cpal callback thread, which must never
    /// block or allocate in steady state: both locks are `try_lock`, so a
    /// render thread that's mid-read just costs this period's
    /// contribution rather than stalling the audio device, and callers
    /// pass in the exact slice/scratch buffer they already have — no
    /// allocation happens here.
    #[cfg(feature = "visualizer")]
    #[inline]
    fn tap_viz(tap: &Option<VizTapSlot>, samples: &[f32], channels: usize) {
        if let Some(slot) = tap
            && let Some(inner) = slot.try_lock()
            && let Some(buf) = inner.as_ref()
            && let Ok(mut pcm) = buf.try_lock()
        {
            pcm.write_interleaved(samples, channels);
        }
    }

    pub fn start(&mut self) -> Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }

        let mut device_config = CpalDeviceConfig {
            device: self.device.clone(),
            config: self.config,
            sample_format: SampleFormat::F32,
        };
        let sample_format = device_config.find_pcm_format()?;

        // Compute channel depth from buffer_time_ms. Each chunk sent over the
        // channel holds ~4096 samples across all channels (the engine's decode
        // loop writes BUFFER_SIZE = 4096 samples per iteration). We divide the
        // desired buffer by the chunk size and clamp to a minimum of 4 so the
        // device callback never starves on a cold start.
        const SAMPLES_PER_CHUNK: u64 = 4096;
        let channel_depth = if self.buffer_time_ms == 0 {
            32 // safe default if somehow zero
        } else {
            let samples_needed = (self.buffer_time_ms as u64
                * self.config.sample_rate as u64
                * self.config.channels as u64)
                / 1000;
            samples_needed.div_ceil(SAMPLES_PER_CHUNK).max(4) as usize
        };
        let (tx, rx) = sync_channel::<Vec<f32>>(channel_depth);

        let stream = match sample_format {
            SampleFormat::F32 => {
                let mut buf = SampleBuffer::new(rx);
                let mut ramp = self.new_fade_ramp();
                #[cfg(feature = "visualizer")]
                let viz_tap = self.viz_tap.clone();
                #[cfg(feature = "visualizer")]
                let viz_channels = self.config.channels as usize;
                self.device
                    .build_output_stream(
                        self.config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            ramp.begin();
                            for sample in data.iter_mut() {
                                *sample = buf.next_sample() * ramp.next_gain();
                            }
                            // Tap AFTER filling `data`: this is exactly
                            // what was just handed to the device this
                            // period, at the moment it was handed over —
                            // see the module docs for why the tap lives
                            // here now instead of on the decode thread.
                            #[cfg(feature = "visualizer")]
                            Self::tap_viz(&viz_tap, data, viz_channels);
                        },
                        stream_error_callback("pcm output", self.device_failed.clone()),
                        None,
                    )
                    .map_err(|e| PlayerError(format!("Failed to build F32 stream: {e}")))?
            }
            SampleFormat::I16 => {
                let mut buf = SampleBuffer::new(rx);
                let mut ramp = self.new_fade_ramp();
                #[cfg(feature = "visualizer")]
                let viz_tap = self.viz_tap.clone();
                #[cfg(feature = "visualizer")]
                let viz_channels = self.config.channels as usize;
                // Scratch buffer holding this period's f32 values before
                // I16 conversion, so the tap sees the same samples handed
                // to the device. Lives in the closure and only grows (via
                // `resize`, never shrinks) when the callback period
                // itself grows — no steady-state allocation.
                #[cfg(feature = "visualizer")]
                let mut viz_scratch: Vec<f32> = Vec::new();
                self.device
                    .build_output_stream(
                        self.config,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            ramp.begin();
                            #[cfg(feature = "visualizer")]
                            if viz_scratch.len() < data.len() {
                                viz_scratch.resize(data.len(), 0.0);
                            }
                            #[cfg(feature = "visualizer")]
                            let mut viz_iter = viz_scratch.iter_mut();
                            for sample in data.iter_mut() {
                                let s = buf.next_sample() * ramp.next_gain();
                                #[cfg(feature = "visualizer")]
                                if let Some(slot) = viz_iter.next() {
                                    *slot = s;
                                }
                                *sample = conversion::f32_to_i16(s);
                            }
                            #[cfg(feature = "visualizer")]
                            Self::tap_viz(&viz_tap, &viz_scratch[..data.len()], viz_channels);
                        },
                        stream_error_callback("pcm output", self.device_failed.clone()),
                        None,
                    )
                    .map_err(|e| PlayerError(format!("Failed to build I16 stream: {e}")))?
            }
            SampleFormat::I32 => {
                let mut buf = SampleBuffer::new(rx);
                let mut ramp = self.new_fade_ramp();
                #[cfg(feature = "visualizer")]
                let viz_tap = self.viz_tap.clone();
                #[cfg(feature = "visualizer")]
                let viz_channels = self.config.channels as usize;
                // See the I16 branch above for why this scratch buffer
                // exists and how its allocation is bounded.
                #[cfg(feature = "visualizer")]
                let mut viz_scratch: Vec<f32> = Vec::new();
                self.device
                    .build_output_stream(
                        self.config,
                        move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                            ramp.begin();
                            #[cfg(feature = "visualizer")]
                            if viz_scratch.len() < data.len() {
                                viz_scratch.resize(data.len(), 0.0);
                            }
                            #[cfg(feature = "visualizer")]
                            let mut viz_iter = viz_scratch.iter_mut();
                            for sample in data.iter_mut() {
                                let s = buf.next_sample() * ramp.next_gain();
                                #[cfg(feature = "visualizer")]
                                if let Some(slot) = viz_iter.next() {
                                    *slot = s;
                                }
                                *sample = conversion::f32_to_i32(s);
                            }
                            #[cfg(feature = "visualizer")]
                            Self::tap_viz(&viz_tap, &viz_scratch[..data.len()], viz_channels);
                        },
                        stream_error_callback("pcm output", self.device_failed.clone()),
                        None,
                    )
                    .map_err(|e| PlayerError(format!("Failed to build I32 stream: {e}")))?
            }
            _ => {
                return Err(PlayerError(format!(
                    "Unsupported sample format: {sample_format:?}"
                )));
            }
        };

        stream
            .play()
            .map_err(|e| PlayerError(format!("Failed to start stream: {e}")))?;

        self.stream = Some(stream);
        self.sample_sender = Some(tx);
        self.channel_depth = channel_depth;
        self.last_chunk_len = 0;
        self.pause_state.set_paused(false);

        tracing::info!(
            "pcm output started: {:?} format, {} Hz, {} channels",
            sample_format,
            self.config.sample_rate,
            self.config.channels
        );

        Ok(())
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<usize> {
        if self.device_failed() {
            return Err(PlayerError(DEVICE_LOST_MESSAGE.to_owned()));
        }
        if self.pause_state.is_paused() {
            return Ok(0);
        }

        // Resample to the device rate when required (bridges unsupported rates).
        let out = match &mut self.resampler {
            Some(rs) => rs.process(samples),
            None => samples.to_vec(),
        };
        let n = out.len();

        match &self.sample_sender {
            Some(sender) => {
                if n > 0 {
                    // A blocking `send` would hang forever if the device
                    // stops draining (unplug, xrun) and then freeze every
                    // caller that joins this thread; give up after a bound,
                    // and as soon as the engine asks this thread to stop
                    // (the caller is about to join it).
                    let deadline = web_time::Instant::now() + SEND_TIMEOUT;
                    let mut payload = out;
                    loop {
                        match sender.try_send(payload) {
                            Ok(()) => break,
                            Err(TrySendError::Disconnected(_)) => {
                                return Err(PlayerError(
                                    "Failed to send samples to output".to_owned(),
                                ));
                            }
                            Err(TrySendError::Full(back)) => {
                                if self.stop_requested() {
                                    // Dropping this buffer is fine: the
                                    // playback loop sees the flag next.
                                    return Ok(0);
                                }
                                if web_time::Instant::now() >= deadline {
                                    return Err(PlayerError(
                                        "Audio device stopped consuming samples".to_owned(),
                                    ));
                                }
                                payload = back;
                                std::thread::sleep(std::time::Duration::from_millis(2));
                            }
                        }
                    }
                    self.last_chunk_len = n;
                }
                Ok(n)
            }
            None => Err(PlayerError("Output not started".to_owned())),
        }
    }

    pub fn pause(&mut self) -> Result<()> {
        if let Some(stream) = &self.stream {
            stream
                .pause()
                .map_err(|e| PlayerError(format!("Failed to pause: {e}")))?;
            self.pause_state.set_paused(true);
        }
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        if let Some(stream) = &self.stream {
            stream
                .play()
                .map_err(|e| PlayerError(format!("Failed to resume: {e}")))?;
            self.pause_state.set_paused(false);
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        // Drain any tail still buffered inside the resampler (a short final
        // chunk, plus its own filter delay) before the stream closes, so the
        // last fraction of a resampled track isn't silently dropped.
        let tail = self
            .resampler
            .as_mut()
            .map(StreamResampler::flush)
            .filter(|tail| !tail.is_empty());
        let tail_len = tail.as_ref().map_or(0, Vec::len);

        // Dropping the stream discards whatever the device callback has not
        // played yet — up to the whole ~500 ms channel. At a queue end or a
        // format change that is the last half second of the track, so wait
        // (bounded, abortable) for the channel to drain first. Skipped when
        // the engine is stopping on purpose, when the device is gone (nothing
        // will ever drain), and while paused (a paused stream never drains).
        if let Some(sender) = &self.sample_sender
            && self.stream.is_some()
            && !self.pause_state.is_paused()
            && !self.stop_requested()
            && !self.device_failed()
        {
            let drain_timeout = Duration::from_millis(u64::from(self.buffer_time_ms) + 100);
            let drained = drain_channel(
                sender,
                tail,
                self.channel_depth,
                drain_timeout,
                self.stop_flag.as_deref(),
            );
            if drained {
                // The callback may still be playing the last chunk it took
                // off the channel (the channel reads empty once it did).
                let last = self.last_chunk_len.max(tail_len);
                let per_second =
                    u64::from(self.config.sample_rate) * u64::from(self.config.channels).max(1);
                let tail_ms = (last as u64 * 1000 / per_second.max(1)).min(250);
                sleep_unless_stopped(Duration::from_millis(tail_ms), self.stop_flag.as_deref());
            }
        }
        if let Some(stream) = self.stream.take() {
            drop(stream);
        }
        self.sample_sender = None;
        self.pause_state.set_paused(false);
        Ok(())
    }

    pub fn is_paused(&self) -> bool {
        self.pause_state.is_paused()
    }
}

impl Drop for CpalOutput {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl AudioOutput for CpalOutput {
    fn start(&mut self) -> Result<()> {
        CpalOutput::start(self)
    }
    fn write(&mut self, samples: &[f32]) -> Result<()> {
        CpalOutput::write(self, samples).map(|_| ())
    }
    fn stop(&mut self) -> Result<()> {
        CpalOutput::stop(self)
    }
    fn pause_state(&self) -> &PauseState {
        &self.pause_state
    }
    fn pause_state_mut(&mut self) -> &mut PauseState {
        &mut self.pause_state
    }
    fn pause(&mut self) -> Result<()> {
        CpalOutput::pause(self)
    }
    fn resume(&mut self) -> Result<()> {
        CpalOutput::resume(self)
    }
    fn is_paused(&self) -> bool {
        CpalOutput::is_paused(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_channel_returns_once_consumer_took_everything() {
        let (tx, rx) = sync_channel::<Vec<f32>>(4);
        tx.send(vec![1.0; 8]).unwrap();
        tx.send(vec![2.0; 8]).unwrap();
        let consumer = std::thread::spawn(move || {
            let mut taken = 0;
            while let Ok(chunk) = rx.recv() {
                taken += chunk.len();
                std::thread::sleep(Duration::from_millis(5));
            }
            taken
        });
        assert!(drain_channel(
            &tx,
            Some(vec![3.0; 4]),
            4,
            Duration::from_secs(2),
            None
        ));
        drop(tx);
        assert_eq!(consumer.join().unwrap(), 20);
    }

    #[test]
    fn drain_channel_times_out_on_stalled_consumer() {
        let (tx, _rx) = sync_channel::<Vec<f32>>(2);
        tx.send(vec![0.0]).unwrap();
        tx.send(vec![0.0]).unwrap();
        assert!(!drain_channel(
            &tx,
            None,
            2,
            Duration::from_millis(20),
            None
        ));
    }

    #[test]
    fn drain_channel_aborts_on_stop_flag() {
        let (tx, _rx) = sync_channel::<Vec<f32>>(1);
        tx.send(vec![0.0]).unwrap();
        let stop = AtomicBool::new(true);
        let started = web_time::Instant::now();
        assert!(!drain_channel(
            &tx,
            None,
            1,
            Duration::from_secs(5),
            Some(&stop)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn device_loss_errors_are_fatal_but_glitches_are_not() {
        assert!(is_fatal_stream_error(cpal::ErrorKind::DeviceNotAvailable));
        assert!(is_fatal_stream_error(cpal::ErrorKind::StreamInvalidated));
        assert!(is_fatal_stream_error(cpal::ErrorKind::BackendError));
        assert!(!is_fatal_stream_error(cpal::ErrorKind::DeviceChanged));
        assert!(!is_fatal_stream_error(cpal::ErrorKind::Xrun));
        assert!(!is_fatal_stream_error(cpal::ErrorKind::RealtimeDenied));
    }

    #[test]
    fn error_callback_raises_the_flag_only_for_fatal_errors() {
        let failed = Arc::new(AtomicBool::new(false));
        let mut callback = stream_error_callback("test", failed.clone());
        callback(cpal::Error::new(cpal::ErrorKind::Xrun));
        assert!(!failed.load(Ordering::Acquire));
        callback(cpal::Error::new(cpal::ErrorKind::DeviceNotAvailable));
        assert!(failed.load(Ordering::Acquire));
    }
}
