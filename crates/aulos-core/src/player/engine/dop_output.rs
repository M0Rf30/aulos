// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! DoP-specific audio output using integer samples.
//!
//! DoP requires exact bit patterns, so this uses I32/I24 format instead of
//! F32 and never implements [`crate::player::engine::output::AudioOutput`] —
//! it is a deliberately separate, structurally isolated path so DoP samples
//! can never accidentally pass through a stage designed for f32 PCM (gain,
//! volume, resampling). Includes the primer-silence-after-open and
//! zero-sample-burst-before-close sequences: real DACs need those to
//! reliably latch onto (and release) the DoP marker pattern.

use crate::player::backend::PlayerError;
use crate::player::engine::conversion::SampleBuffer;
use crate::player::engine::cpal_utils::CpalDeviceConfig;
use crate::player::engine::output::{DEVICE_LOST_MESSAGE, drain_channel, stream_error_callback};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{Device, SampleFormat, Stream, StreamConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::time::Duration;
use web_time::Instant;

pub type Result<T, E = PlayerError> = std::result::Result<T, E>;

/// Depth of the sample channel between the decode thread and the device.
const CHANNEL_DEPTH: usize = 32;

/// Upper bound on how long [`DopOutput::stop`] waits for the channel to
/// drain (~the buffered audio, 32 chunks of a few ms each, plus slack).
const DRAIN_TIMEOUT: Duration = Duration::from_millis(600);

/// Outcome of a bounded send on the DoP sample channel.
enum SendOutcome {
    Sent,
    TimedOut,
    Disconnected,
}

/// Send on the bounded channel, waiting up to `timeout` for space via
/// non-blocking `try_send` retries. `std::sync::mpsc` has no stable timed send,
/// and a plain blocking `send` would hang forever if the output callback stalls
/// (device xrun/disconnect) — leaking the exclusive ALSA device.
fn send_bounded(
    sender: &SyncSender<Vec<i32>>,
    mut payload: Vec<i32>,
    timeout: Duration,
    stop_flag: Option<&AtomicBool>,
) -> SendOutcome {
    let deadline = Instant::now() + timeout;
    loop {
        match sender.try_send(payload) {
            Ok(()) => return SendOutcome::Sent,
            Err(TrySendError::Disconnected(_)) => return SendOutcome::Disconnected,
            Err(TrySendError::Full(returned)) => {
                if Instant::now() >= deadline
                    || stop_flag.is_some_and(|flag| flag.load(Ordering::Acquire))
                {
                    return SendOutcome::TimedOut;
                }
                payload = returned;
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// DoP marker sample for the priming/reset bursts: alternates 0x05/0xFA
/// every frame, matching the real encoder's per-frame marker cycle (see
/// `dop.rs::DopEncoder::encode`).
fn dop_primer_sample(frame: usize) -> i32 {
    let marker: i32 = if frame.is_multiple_of(2) { 0x05 } else { 0xFA };
    marker << 24
}

pub struct DopOutput {
    device: Device,
    stream: Option<Stream>,
    sample_sender: Option<SyncSender<Vec<i32>>>,
    config: StreamConfig,
    is_paused: bool,
    /// The engine's stop flag (see [`Self::set_stop_flag`]): skips draining
    /// the buffered tail in [`Self::stop`] when the engine is stopping on
    /// purpose.
    stop_flag: Option<Arc<AtomicBool>>,
    /// Raised by the cpal error callback on a fatal stream error (device
    /// unplugged or invalidated); see [`Self::device_failed`].
    device_failed: Arc<AtomicBool>,
}

impl DopOutput {
    /// Wire the engine's stop flag.
    pub fn set_stop_flag(&mut self, stop_flag: Arc<AtomicBool>) {
        self.stop_flag = Some(stop_flag);
    }

    /// Whether the device reported a fatal stream error. Sticky for this
    /// output's lifetime: the playback thread polls it so a lost device
    /// ends playback at once instead of looping on dropped writes.
    pub fn device_failed(&self) -> bool {
        self.device_failed.load(Ordering::Acquire)
    }

    pub fn new(sample_rate: u32, channels: u8) -> Result<Self> {
        let device_config = CpalDeviceConfig::new_dop(sample_rate, channels as u16)?;

        tracing::warn!(
            "DoP requires direct hardware access at {} Hz, ensure PipeWire/PulseAudio isn't resampling",
            sample_rate
        );

        Ok(Self {
            device: device_config.device,
            stream: None,
            sample_sender: None,
            config: device_config.config,
            is_paused: false,
            stop_flag: None,
            device_failed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn start(&mut self) -> Result<()> {
        if self.stream.is_some() {
            return Ok(());
        }

        let mut device_config = CpalDeviceConfig {
            device: self.device.clone(),
            config: self.config,
            sample_format: SampleFormat::I32,
        };
        let sample_format = device_config.find_dop_format()?;
        tracing::info!("requested sample rate: {:?} Hz", self.config.sample_rate);
        tracing::info!("requested channels: {}", self.config.channels);

        let (tx, rx) = sync_channel::<Vec<i32>>(CHANNEL_DEPTH);

        let stream = match sample_format {
            SampleFormat::I32 | SampleFormat::I24 => {
                let mut buf = SampleBuffer::new(rx);
                self.device
                    .build_output_stream(
                        self.config,
                        move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                            for sample in data.iter_mut() {
                                *sample = buf.next_sample();
                            }
                        },
                        stream_error_callback("DoP output", self.device_failed.clone()),
                        None,
                    )
                    .map_err(|e| PlayerError(format!("Failed to build DoP stream: {e}")))?
            }
            _ => {
                tracing::warn!("no I32 format available, using fallback conversion");
                let mut buf = SampleBuffer::new(rx);
                self.device
                    .build_output_stream(
                        self.config,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            for sample in data.iter_mut() {
                                let val = buf.next_sample();
                                *sample = (val as f32) / 2147483648.0;
                            }
                        },
                        stream_error_callback("DoP output", self.device_failed.clone()),
                        None,
                    )
                    .map_err(|e| PlayerError(format!("Failed to build DoP stream: {e}")))?
            }
        };

        stream
            .play()
            .map_err(|e| PlayerError(format!("Failed to start DoP stream: {e}")))?;

        self.stream = Some(stream);
        self.sample_sender = Some(tx.clone());
        self.is_paused = false;

        tracing::info!("DoP output started");

        std::thread::sleep(std::time::Duration::from_millis(50));

        tracing::info!("priming DAC with DoP-marked silence for format detection");

        let sample_rate_hz = self.config.sample_rate as usize;
        let primer_duration_ms = if sample_rate_hz <= 200000 {
            200
        } else if sample_rate_hz <= 400000 {
            100
        } else {
            50
        };
        let primer_frames = (sample_rate_hz * primer_duration_ms) / 1000;
        let mut primer_samples = Vec::with_capacity(primer_frames * self.config.channels as usize);

        for frame in 0..primer_frames {
            let dop_silence = dop_primer_sample(frame);
            primer_samples.extend(std::iter::repeat_n(
                dop_silence,
                self.config.channels as usize,
            ));
        }

        let chunk_size = self.config.sample_rate as usize / 50 * self.config.channels as usize;
        for chunk in primer_samples.chunks(chunk_size) {
            // Bounded wait so a stalled callback can't hang priming forever.
            if !matches!(
                send_bounded(&tx, chunk.to_vec(), Duration::from_millis(500), None),
                SendOutcome::Sent
            ) {
                tracing::warn!("DoP primer send stalled; continuing");
                break;
            }
        }

        tracing::info!(
            "DoP primer sent ({} frames = {}ms)",
            primer_frames,
            (primer_frames * 1000) / self.config.sample_rate as usize
        );

        Ok(())
    }

    pub fn write(&mut self, samples: &[i32]) -> Result<usize> {
        if self.device_failed() {
            return Err(PlayerError(DEVICE_LOST_MESSAGE.to_owned()));
        }
        if self.is_paused {
            return Ok(0);
        }

        let Some(sender) = &self.sample_sender else {
            return Err(PlayerError("DoP output not started".to_owned()));
        };

        // Bounded send: normally returns quickly (backpressure paces the decoder
        // to playback rate). If the output callback stalls (device xrun or
        // disconnect), don't block forever — drop this buffer so the playback
        // loop stays responsive to stop/seek and the device can be released.
        match send_bounded(
            sender,
            samples.to_vec(),
            Duration::from_millis(500),
            self.stop_flag.as_deref(),
        ) {
            SendOutcome::Sent => Ok(samples.len()),
            SendOutcome::TimedOut => Ok(0),
            SendOutcome::Disconnected => Err(PlayerError("DoP output stream closed".to_owned())),
        }
    }

    pub fn pause(&mut self) -> Result<()> {
        if let Some(stream) = &self.stream {
            stream
                .pause()
                .map_err(|e| PlayerError(format!("Failed to pause: {e}")))?;
            self.is_paused = true;
        }
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        if let Some(stream) = &self.stream {
            stream
                .play()
                .map_err(|e| PlayerError(format!("Failed to resume: {e}")))?;
            self.is_paused = false;
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        // Let the device play out what is still buffered before the reset
        // burst and the stream drop discard it (the last ~370 ms of the
        // track otherwise). Skipped when the engine is stopping on purpose,
        // while paused (a paused stream never drains), and when the device
        // is gone (nothing will ever drain).
        if let Some(sender) = &self.sample_sender
            && self.stream.is_some()
            && !self.is_paused
            && !self.device_failed()
            && !self
                .stop_flag
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            drain_channel(
                sender,
                None,
                CHANNEL_DEPTH,
                DRAIN_TIMEOUT,
                self.stop_flag.as_deref(),
            );
        }

        // A lost device cannot take the reset burst; skip it and its waits.
        if let Some(sender) = &self.sample_sender
            && !self.device_failed()
        {
            tracing::info!("sending PCM reset sequence to switch DAC back to PCM mode");

            let reset_frames = self.config.sample_rate as usize / 10;
            let reset_samples = vec![0i32; reset_frames * self.config.channels as usize];

            // Best-effort: the drain above leaves up to `CHANNEL_DEPTH` empty
            // chunks queued (each consumed within one sample), so wait
            // briefly for room, but never block shutdown if the callback
            // isn't draining.
            let _ = send_bounded(sender, reset_samples, Duration::from_millis(200), None);

            std::thread::sleep(std::time::Duration::from_millis(150));

            tracing::info!("PCM reset sequence sent");
        }

        if let Some(stream) = self.stream.take() {
            drop(stream);
            // Let ALSA/USB fully release the exclusive device before any
            // subsequent open, avoiding spurious "busy" on rapid track switches.
            std::thread::sleep(std::time::Duration::from_millis(80));
        }
        self.sample_sender = None;
        self.is_paused = false;
        Ok(())
    }

    pub fn is_paused(&self) -> bool {
        self.is_paused
    }
}

impl Drop for DopOutput {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_bounded_delivers_when_space_available() {
        let (tx, rx) = sync_channel::<Vec<i32>>(1);
        let outcome = send_bounded(&tx, vec![1, 2, 3], Duration::from_millis(50), None);
        assert!(matches!(outcome, SendOutcome::Sent));
        assert_eq!(rx.recv().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn send_bounded_times_out_when_full_and_undrained() {
        let (tx, _rx) = sync_channel::<Vec<i32>>(1);
        // Fill the one slot; keep the receiver alive but never drain it.
        tx.try_send(vec![0]).unwrap();
        let outcome = send_bounded(&tx, vec![1], Duration::from_millis(20), None);
        assert!(matches!(outcome, SendOutcome::TimedOut));
    }

    #[test]
    fn send_bounded_returns_promptly_when_stopping() {
        let (tx, _rx) = sync_channel::<Vec<i32>>(1);
        tx.try_send(vec![0]).unwrap();
        let stop = AtomicBool::new(true);
        let start = Instant::now();
        let outcome = send_bounded(&tx, vec![1], Duration::from_secs(5), Some(&stop));
        assert!(matches!(outcome, SendOutcome::TimedOut));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn send_bounded_detects_disconnect() {
        let (tx, rx) = sync_channel::<Vec<i32>>(1);
        drop(rx);
        let outcome = send_bounded(&tx, vec![1], Duration::from_millis(20), None);
        assert!(matches!(outcome, SendOutcome::Disconnected));
    }

    #[test]
    fn dop_primer_marker_alternates_every_frame() {
        let markers: Vec<i32> = (0..6).map(dop_primer_sample).collect();
        assert_eq!(
            markers,
            vec![
                0x05 << 24,
                0xFA << 24,
                0x05 << 24,
                0xFA << 24,
                0x05 << 24,
                0xFA << 24,
            ]
        );
    }
}
