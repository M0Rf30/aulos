// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The first-run intro jingle — Aulos's answer to Winamp's llama.
//!
//! "Aulos: it really pipes the satyr's ass." is shipped as a 128 kbps
//! joint-stereo MP3 (the format Winamp made famous), embedded in the binary
//! and played once on the very first launch. It runs on its own short-lived
//! cpal stream on a background thread, completely separate from the playback
//! engine, so it never touches the queue, now-playing state, EQ or MPRIS.

use crate::player::engine::cpal_utils::CpalDeviceConfig;
use crate::player::engine::decoder::SymphoniaDecoder;
use crate::player::engine::resampler::{ResamplerQuality, StreamResampler};
use cpal::SampleFormat;
use cpal::traits::{DeviceTrait, StreamTrait};
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// The jingle itself.
const JINGLE_MP3: &[u8] = include_bytes!("../../resources/sounds/satyr.mp3");

/// Playback gain: leave headroom so the jingle isn't startling.
const GAIN: f32 = 0.8;

/// Play the intro jingle once, without blocking. Failures (no audio device,
/// decode error) are logged and otherwise ignored — an intro must never get
/// in the way of the app starting.
pub fn play() {
    let spawned = std::thread::Builder::new()
        .name("aulos-intro".into())
        .spawn(|| {
            if let Err(e) = play_blocking() {
                tracing::debug!("intro jingle not played: {e}");
            }
        });
    if let Err(e) = spawned {
        tracing::debug!("intro jingle thread not spawned: {e}");
    }
}

/// Decode the whole (short) jingle into interleaved stereo f32 samples.
fn decode() -> Result<(Vec<f32>, u32, usize), String> {
    let mut decoder = SymphoniaDecoder::open_reader(
        Cursor::new(JINGLE_MP3),
        Some(JINGLE_MP3.len() as u64),
        Some("mp3"),
    )
    .map_err(|e| e.0)?;
    let mut samples = Vec::new();
    let mut buf = vec![0.0f32; 8192];
    loop {
        let n = decoder.read(&mut buf).map_err(|e| e.0)?;
        if n == 0 {
            break;
        }
        samples.extend_from_slice(&buf[..n]);
    }
    Ok((
        samples,
        decoder.sample_rate(),
        usize::from(decoder.channels()),
    ))
}

fn play_blocking() -> Result<(), String> {
    let (samples, src_rate, channels) = decode()?;
    if samples.is_empty() || channels == 0 {
        return Err("empty jingle".into());
    }

    let mut device = CpalDeviceConfig::new(src_rate, channels as u16).map_err(|e| e.0)?;
    let format = device.find_pcm_format().map_err(|e| e.0)?;
    let dst_rate = device.config.sample_rate;

    // Bridge the device's rate if it can't play 44.1 kHz directly.
    let samples = if dst_rate == src_rate {
        samples
    } else {
        let mut rs = StreamResampler::new(src_rate, dst_rate, channels, ResamplerQuality::SincFast)
            .ok_or("resampler unavailable")?;
        let mut out = rs.process(&samples);
        out.extend(rs.flush());
        out
    };

    let total = samples.len();
    let duration = Duration::from_secs_f64(total as f64 / f64::from(dst_rate) / channels as f64);
    let samples = Arc::new(samples);
    let pos = Arc::new(AtomicUsize::new(0));
    let next = {
        let samples = Arc::clone(&samples);
        let pos = Arc::clone(&pos);
        move || {
            let i = pos.fetch_add(1, Ordering::Relaxed);
            samples.get(i).map_or(0.0, |s| s * GAIN)
        }
    };

    let err = |e| tracing::debug!("intro jingle stream error: {e}");
    let stream = match format {
        SampleFormat::F32 => device.device.build_output_stream(
            device.config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                data.iter_mut().for_each(|s| *s = next());
            },
            err,
            None,
        ),
        SampleFormat::I16 => device.device.build_output_stream(
            device.config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                data.iter_mut()
                    .for_each(|s| *s = (next().clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16);
            },
            err,
            None,
        ),
        SampleFormat::I32 => device.device.build_output_stream(
            device.config,
            move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                data.iter_mut().for_each(|s| {
                    *s = (f64::from(next().clamp(-1.0, 1.0)) * f64::from(i32::MAX)) as i32
                });
            },
            err,
            None,
        ),
        other => return Err(format!("unsupported sample format {other:?}")),
    }
    .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    // Keep the stream alive for the jingle plus a little tail for the
    // device buffer to drain, then drop it.
    std::thread::sleep(duration + Duration::from_millis(400));
    drop(stream);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_jingle_decodes_to_about_four_seconds_of_stereo() {
        let (samples, rate, channels) = super::decode().expect("jingle decodes");
        assert_eq!(channels, 2);
        let secs = samples.len() as f64 / f64::from(rate) / channels as f64;
        assert!(
            (3.5..4.6).contains(&secs),
            "unexpected jingle length {secs}s"
        );
    }
}
