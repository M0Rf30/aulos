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
//! [`crate::views::now_playing::visualizer::PcmBuffer`] (see
//! [`CpalOutput::set_viz_tap`]/`tap_viz`) — deliberately *here*, in the
//! realtime device callback, rather than on the decode thread that fills
//! the ~500ms-deep `sync_channel` above: tapping on the decode side would
//! let the visualizer see (and react to) audio up to that whole buffer
//! depth before it's actually heard.

use crate::player::backend::PlayerError;
use crate::player::engine::conversion::{self, SampleBuffer};
use crate::player::engine::cpal_utils::CpalDeviceConfig;
use crate::player::engine::resampler::{ResamplerQuality, StreamResampler};
#[cfg(feature = "visualizer")]
use crate::views::now_playing::visualizer::PcmBuffer;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
#[cfg(feature = "visualizer")]
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, sync_channel};

pub type Result<T, E = PlayerError> = std::result::Result<T, E>;

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

pub struct CpalOutput {
    device: Device,
    stream: Option<Stream>,
    sample_sender: Option<SyncSender<Vec<f32>>>,
    config: StreamConfig,
    pause_state: PauseState,
    resampler: Option<StreamResampler>,
    /// Output buffer time in milliseconds; sizes the sync-channel depth.
    buffer_time_ms: u32,
    /// Visualizer tap slot, wired via [`Self::set_viz_tap`] before
    /// [`Self::start`]. Doubly-`Option`-wrapped: the outer `Arc<Mutex<_>>`
    /// is `ThreadContext::pcm_buffer` itself, shared with the controlling
    /// side, so UI-side visualizer activate/deactivate (flipping the
    /// *inner* `Option`) takes effect on the very next cpal callback with
    /// no stream rebuild. `None` for a `CpalOutput` that never opts in
    /// (visualizer feature/tap not wired, or DoP/test callers).
    #[cfg(feature = "visualizer")]
    viz_tap: Option<VizTapSlot>,
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
            #[cfg(feature = "visualizer")]
            viz_tap: None,
        })
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
                #[cfg(feature = "visualizer")]
                let viz_tap = self.viz_tap.clone();
                #[cfg(feature = "visualizer")]
                let viz_channels = self.config.channels as usize;
                self.device
                    .build_output_stream(
                        self.config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            for sample in data.iter_mut() {
                                *sample = buf.next_sample();
                            }
                            // Tap AFTER filling `data`: this is exactly
                            // what was just handed to the device this
                            // period, at the moment it was handed over —
                            // see the module docs for why the tap lives
                            // here now instead of on the decode thread.
                            #[cfg(feature = "visualizer")]
                            Self::tap_viz(&viz_tap, data, viz_channels);
                        },
                        |err| {
                            tracing::error!("pcm output error: {}", err);
                        },
                        None,
                    )
                    .map_err(|e| PlayerError(format!("Failed to build F32 stream: {e}")))?
            }
            SampleFormat::I16 => {
                let mut buf = SampleBuffer::new(rx);
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
                            #[cfg(feature = "visualizer")]
                            if viz_scratch.len() < data.len() {
                                viz_scratch.resize(data.len(), 0.0);
                            }
                            #[cfg(feature = "visualizer")]
                            let mut viz_iter = viz_scratch.iter_mut();
                            for sample in data.iter_mut() {
                                let s = buf.next_sample();
                                #[cfg(feature = "visualizer")]
                                if let Some(slot) = viz_iter.next() {
                                    *slot = s;
                                }
                                *sample = conversion::f32_to_i16(s);
                            }
                            #[cfg(feature = "visualizer")]
                            Self::tap_viz(&viz_tap, &viz_scratch[..data.len()], viz_channels);
                        },
                        |err| {
                            tracing::error!("pcm output error: {}", err);
                        },
                        None,
                    )
                    .map_err(|e| PlayerError(format!("Failed to build I16 stream: {e}")))?
            }
            SampleFormat::I32 => {
                let mut buf = SampleBuffer::new(rx);
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
                            #[cfg(feature = "visualizer")]
                            if viz_scratch.len() < data.len() {
                                viz_scratch.resize(data.len(), 0.0);
                            }
                            #[cfg(feature = "visualizer")]
                            let mut viz_iter = viz_scratch.iter_mut();
                            for sample in data.iter_mut() {
                                let s = buf.next_sample();
                                #[cfg(feature = "visualizer")]
                                if let Some(slot) = viz_iter.next() {
                                    *slot = s;
                                }
                                *sample = conversion::f32_to_i32(s);
                            }
                            #[cfg(feature = "visualizer")]
                            Self::tap_viz(&viz_tap, &viz_scratch[..data.len()], viz_channels);
                        },
                        |err| {
                            tracing::error!("pcm output error: {}", err);
                        },
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
                    sender
                        .send(out)
                        .map_err(|_| PlayerError("Failed to send samples to output".to_owned()))?;
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
        if let Some(rs) = &mut self.resampler {
            let tail = rs.flush();
            if !tail.is_empty()
                && let Some(sender) = &self.sample_sender
            {
                // Best-effort: never block shutdown if the callback isn't draining.
                let _ = sender.try_send(tail);
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
