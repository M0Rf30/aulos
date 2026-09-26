// SPDX-License-Identifier: GPL-3.0

//! Output encoders for the local file converter. Pure Rust only: WAV via
//! `hound`, FLAC via `flacenc`. No lossy format is offered since no
//! pure-Rust lossy encoder exists.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use super::ConvertError;

/// Output container/codec choices. FLAC always writes 16-bit for 16-bit (or
/// unknown) sources and 24-bit otherwise (see [`flac_bit_depth`]) — good
/// enough fidelity without needlessly doubling the size of a 16-bit rip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OutputFormat {
    #[default]
    Flac,
    Wav16,
    Wav24,
    Wav32Float,
}

impl OutputFormat {
    pub const ALL: [OutputFormat; 4] = [Self::Flac, Self::Wav16, Self::Wav24, Self::Wav32Float];

    /// File extension for this format, used to build output filenames.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Wav16 | Self::Wav24 | Self::Wav32Float => "wav",
        }
    }
}

/// Sink for interleaved `f32` PCM frames, writing an encoded output file.
/// Implementations buffer as needed internally; [`SampleSink::finish`]
/// flushes and finalizes the file.
pub trait SampleSink: Send {
    fn write(&mut self, interleaved: &[f32]) -> Result<(), ConvertError>;
    fn finish(self: Box<Self>) -> Result<(), ConvertError>;
}

/// Picks the FLAC bits-per-sample to encode at, from the source's reported
/// bit depth (`None` for formats symphonia doesn't expose one for, e.g.
/// lossy sources being transcoded to a lossless container).
fn flac_bit_depth(source_bits: Option<u32>) -> u32 {
    match source_bits {
        Some(bits) if bits <= 16 => 16,
        _ => 24,
    }
}

/// Scales a `[-1.0, 1.0]` sample to a signed `bits`-wide integer, clamping
/// out-of-range input rather than wrapping. No dither is added: plain
/// round-to-nearest quantization noise is inaudible at the 16-24 bit
/// depths this converter targets, so the added complexity isn't worth it.
fn f32_to_int(sample: f32, bits: u32) -> i32 {
    let scale = (1i64 << (bits - 1)) as f64;
    let max = scale - 1.0;
    let min = -scale;
    (f64::from(sample.clamp(-1.0, 1.0)) * scale).round().clamp(min, max) as i32
}

/// Creates the [`SampleSink`] for `format` at `path`. `source_bits_hint` is
/// only consulted for [`OutputFormat::Flac`].
pub fn create_sink(
    format: OutputFormat,
    path: &Path,
    channels: u16,
    sample_rate: u32,
    source_bits_hint: Option<u32>,
) -> Result<Box<dyn SampleSink>, ConvertError> {
    match format {
        OutputFormat::Flac => Ok(Box::new(FlacSink {
            samples: Vec::new(),
            channels,
            bits_per_sample: flac_bit_depth(source_bits_hint),
            sample_rate,
            path: path.to_owned(),
        })),
        OutputFormat::Wav16 | OutputFormat::Wav24 | OutputFormat::Wav32Float => {
            let depth = match format {
                OutputFormat::Wav16 => WavDepth::I16,
                OutputFormat::Wav24 => WavDepth::I24,
                _ => WavDepth::F32,
            };
            let spec = hound::WavSpec {
                channels,
                sample_rate,
                bits_per_sample: match depth {
                    WavDepth::I16 => 16,
                    WavDepth::I24 => 24,
                    WavDepth::F32 => 32,
                },
                sample_format: match depth {
                    WavDepth::F32 => hound::SampleFormat::Float,
                    WavDepth::I16 | WavDepth::I24 => hound::SampleFormat::Int,
                },
            };
            let writer = hound::WavWriter::create(path, spec)
                .map_err(|e| ConvertError::Encode(format!("cannot create WAV file: {e}")))?;
            Ok(Box::new(WavSink { writer, depth }))
        }
    }
}

#[derive(Clone, Copy)]
enum WavDepth {
    I16,
    I24,
    F32,
}

struct WavSink {
    writer: hound::WavWriter<BufWriter<File>>,
    depth: WavDepth,
}

impl SampleSink for WavSink {
    fn write(&mut self, interleaved: &[f32]) -> Result<(), ConvertError> {
        for &sample in interleaved {
            let result = match self.depth {
                WavDepth::I16 => self.writer.write_sample(f32_to_int(sample, 16) as i16),
                WavDepth::I24 => self.writer.write_sample(f32_to_int(sample, 24)),
                WavDepth::F32 => self.writer.write_sample(sample),
            };
            result.map_err(|e| ConvertError::Encode(format!("WAV write failed: {e}")))?;
        }
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(), ConvertError> {
        self.writer
            .finalize()
            .map_err(|e| ConvertError::Encode(format!("WAV finalize failed: {e}")))
    }
}

struct FlacSink {
    samples: Vec<i32>,
    channels: u16,
    bits_per_sample: u32,
    sample_rate: u32,
    path: PathBuf,
}

impl SampleSink for FlacSink {
    fn write(&mut self, interleaved: &[f32]) -> Result<(), ConvertError> {
        let bits = self.bits_per_sample;
        self.samples.extend(interleaved.iter().map(|&s| f32_to_int(s, bits)));
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(), ConvertError> {
        use flacenc::component::BitRepr;
        use flacenc::error::Verify;

        // `multithread` defaults to on (the `par` feature): its worker
        // split appears to mis-number frames on some inputs (the reference
        // `flac` decoder tolerates it with a warning, but symphonia's
        // stricter demuxer rejects the stream outright). Job-level
        // concurrency is already capped elsewhere, so single-threaded FLAC
        // encoding costs nothing here and sidesteps the bug entirely.
        let mut encoder_config = flacenc::config::Encoder::default();
        encoder_config.multithread = false;
        let config = encoder_config
            .into_verified()
            .map_err(|(_, e)| ConvertError::Encode(format!("invalid FLAC encoder config: {e}")))?;
        let source = flacenc::source::MemSource::from_samples(
            &self.samples,
            self.channels as usize,
            self.bits_per_sample as usize,
            self.sample_rate as usize,
        );
        let block_size = config.block_size;
        let mut stream = flacenc::encode_with_fixed_block_size(&config, source, block_size)
            .map_err(|e| ConvertError::Encode(format!("FLAC encode failed: {e}")))?;

        // `Stream::add_frame` lets a shorter last block lower
        // `StreamInfo::min_block_size` below `block_size`. The reference
        // `flac` encoder never does this (it always reports
        // `min_block_size == max_block_size == block_size`), and at least
        // one symphonia decoder infers "variable blocksize stream" from
        // `min != max` — misreading every frame's fixed-blocksize frame
        // number as a sample offset and failing to sync. Restoring the
        // libFLAC-style min/max keeps the (fully spec-legal) short last
        // frame decodable everywhere; per-frame headers already encode
        // each frame's true size independently, so this touches only
        // informational metadata, never the audio data.
        stream.stream_info_mut().set_block_sizes(block_size, block_size).ok();

        let mut sink = flacenc::bitsink::ByteSink::new();
        stream
            .write(&mut sink)
            .map_err(|e| ConvertError::Encode(format!("FLAC bitstream write failed: {e}")))?;
        std::fs::write(&self.path, sink.as_slice())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    /// One second of 440 Hz sine at 44.1 kHz mono, as interleaved `f32`.
    fn sine_1s() -> Vec<f32> {
        let sample_rate = 44_100;
        (0..sample_rate)
            .map(|i| (TAU * 440.0 * i as f32 / sample_rate as f32).sin() * 0.5)
            .collect()
    }

    /// Probes `path` with symphonia and returns the decoded frame count.
    fn probe_frame_count(path: &Path) -> u64 {
        use symphonia::core::codecs::audio::AudioDecoderOptions;
        use symphonia::core::formats::{FormatOptions, TrackType};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;

        let file = File::open(path).expect("reopen encoded file");
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut reader = symphonia::default::get_probe()
            .probe(
                &Default::default(),
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("probe encoded file");
        let track = reader
            .default_track(TrackType::Audio)
            .expect("audio track in encoded file");
        let track_id = track.id;
        let symphonia::core::codecs::CodecParameters::Audio(audio) =
            track.codec_params.as_ref().expect("audio codec params")
        else {
            panic!("expected audio codec params");
        };
        let mut decoder = symphonia::default::get_codecs()
            .make_audio_decoder(audio, &AudioDecoderOptions::default())
            .expect("make decoder");

        let mut frames = 0u64;
        loop {
            let packet = match reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(_) => break,
            };
            if packet.track_id != track_id {
                continue;
            }
            if let Ok(decoded) = decoder.decode(&packet) {
                frames += decoded.frames() as u64;
            }
        }
        frames
    }

    #[test]
    fn wav16_roundtrip_preserves_frame_count() {
        let dir = std::env::temp_dir().join(format!("lyra-convert-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wav16.wav");

        let samples = sine_1s();
        let mut sink = create_sink(OutputFormat::Wav16, &path, 1, 44_100, None).unwrap();
        sink.write(&samples).unwrap();
        sink.finish().unwrap();

        let frames = probe_frame_count(&path);
        assert!(
            frames.abs_diff(samples.len() as u64) <= 1,
            "expected ~{} frames, got {frames}",
            samples.len()
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn flac_roundtrip_preserves_frame_count() {
        let dir = std::env::temp_dir().join(format!("lyra-convert-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sine.flac");

        let samples = sine_1s();
        let mut sink = create_sink(OutputFormat::Flac, &path, 1, 44_100, Some(16)).unwrap();
        sink.write(&samples).unwrap();
        sink.finish().unwrap();

        let frames = probe_frame_count(&path);
        assert!(
            frames.abs_diff(samples.len() as u64) <= 1,
            "expected ~{} frames, got {frames}",
            samples.len()
        );
        std::fs::remove_file(&path).ok();
    }
}
