// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Symphonia-based audio decoder.
//!
//! One decoder implementation only — no MPD-style `DecoderPlugin` registry,
//! since a compile-time registry only earns its keep with multiple
//! interchangeable implementations. There's also no ICY "now playing" title
//! support for internet radio streams, since lyra doesn't need it.
//!
//! [`SymphoniaDecoder::open`] (local files) and
//! [`SymphoniaDecoder::open_reader`] (arbitrary readers — used for HTTP
//! streaming from lyra's Subsonic/Navidrome remote libraries via
//! [`crate::player::http_range_reader::HttpRangeReader`]) both funnel into
//! [`SymphoniaDecoder::from_media_source`] so the probe/track-selection logic
//! is written once regardless of where the bytes come from.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{
    AudioCodecId, AudioDecoder, AudioDecoderOptions, BitOrder, ChannelDataLayout,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};
// DSD codec type (from the Symphonia fork's `dsd` feature).
use symphonia::default::formats::CODEC_TYPE_DSD;

use crate::player::backend::PlayerError;
use crate::player::http_range_reader::HttpRangeReader;

pub type Result<T, E = PlayerError> = std::result::Result<T, E>;

/// File extensions this decoder recognizes, for use by a file-picker filter
/// or library scanner. Symphonia's probe is content-based and doesn't
/// strictly require a matching extension — this list only ever feeds a
/// [`Hint`], never gates whether a file is attempted.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "ogg", "oga", "opus", "wav", "wave", "aiff", "aif", "m4a", "mp4", "aac", "alac",
    "ape", "wv", "mpc", "dsf", "dff", "webm", "mka", "caf",
];

/// Minimal audio format descriptor. Dependency-free by design — lyra has no
/// shared "song"/media domain crate to pull a richer type from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u32,
}

/// Adapts any `Read + Seek` source into a Symphonia [`MediaSource`], so
/// [`SymphoniaDecoder::open_reader`] can probe/decode from something other
/// than a local file (Symphonia's own `impl MediaSource for std::fs::File`
/// covers the local-file case directly). `byte_len` is supplied by the
/// caller up front since arbitrary readers have no cheap, uniform way to
/// report their total length the way a file's metadata does.
struct ReadSeekMediaSource<R> {
    inner: R,
    byte_len: Option<u64>,
}

impl<R> ReadSeekMediaSource<R> {
    fn new(inner: R, byte_len: Option<u64>) -> Self {
        Self { inner, byte_len }
    }
}

impl<R: Read> Read for ReadSeekMediaSource<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for ReadSeekMediaSource<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl<R: Read + Seek + Send + Sync> MediaSource for ReadSeekMediaSource<R> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

/// Adapts a non-seekable `Read` source (e.g. an internet radio stream) into
/// a Symphonia [`MediaSource`]. Unlike [`ReadSeekMediaSource`], seeking is
/// never supported: `is_seekable()` is always `false` and the `Seek` impl
/// always errors, matching what a live stream can actually do.
struct StreamMediaSource<R> {
    inner: R,
}

impl<R> StreamMediaSource<R> {
    fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R: Read> Read for StreamMediaSource<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R> Seek for StreamMediaSource<R> {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "seeking is not supported on live streams",
        ))
    }
}

impl<R: Read + Send + Sync> MediaSource for StreamMediaSource<R> {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// Symphonia-based audio decoder.
pub struct SymphoniaDecoder {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    codec_id: AudioCodecId,
    sample_rate: u32,
    channels: Option<u8>,
    total_duration: Option<f64>,
    sample_buf: Vec<f32>,
    sample_pos: usize,
    current_bitrate: Option<u32>,
    time_base: Option<TimeBase>,
    channel_data_layout: Option<ChannelDataLayout>,
    bit_order: Option<BitOrder>,
    uses_pcm_conversion: bool,
}

const MAX_CONSECUTIVE_DSD_RESETS: usize = 1024;

enum DsdPacketEvent<T> {
    Packet { track_id: u32, packet: T },
    Reset,
    End,
}

fn next_dsd_packet<T>(
    track_id: u32,
    mut next_event: impl FnMut() -> Result<DsdPacketEvent<T>>,
    mut reset: impl FnMut(),
) -> Result<Option<T>> {
    let mut consecutive_resets = 0;

    loop {
        match next_event()? {
            DsdPacketEvent::Packet {
                track_id: packet_track_id,
                packet,
            } => {
                consecutive_resets = 0;
                if packet_track_id == track_id {
                    return Ok(Some(packet));
                }
            }
            DsdPacketEvent::Reset => {
                consecutive_resets += 1;
                if consecutive_resets > MAX_CONSECUTIVE_DSD_RESETS {
                    return Err(PlayerError(
                        "Too many consecutive DSD decoder resets".to_owned(),
                    ));
                }
                reset();
            }
            DsdPacketEvent::End => return Ok(None),
        }
    }
}

impl SymphoniaDecoder {
    /// Open a local file for decoding.
    pub fn open(path: &Path) -> Result<Self> {
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let file = std::fs::File::open(path)
            .map_err(|e| PlayerError(format!("Failed to open file: {e}")))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        Self::from_media_source(mss, hint)
    }

    /// Open an arbitrary seekable byte source for decoding — e.g. a remote
    /// track streamed through lyra's [`HttpRangeReader`]. `byte_len` should
    /// be the total content length in bytes if known (enables trailing-tag
    /// probing and lets Symphonia seek accurately near the end of the
    /// stream); `hint_extension` should be the file extension of the
    /// underlying resource, if known, to speed up format probing. Both are
    /// optional — Symphonia's probe falls back to content sniffing when
    /// `hint_extension` is `None`, and simply skips trailing-tag probing
    /// when `byte_len` is `None`.
    pub fn open_reader<R>(
        reader: R,
        byte_len: Option<u64>,
        hint_extension: Option<&str>,
    ) -> Result<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let mut hint = Hint::new();
        if let Some(ext) = hint_extension {
            hint.with_extension(ext);
        }

        let source = ReadSeekMediaSource::new(reader, byte_len);
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        Self::from_media_source(mss, hint)
    }

    /// Convenience wrapper over [`Self::open_reader`] for lyra's own
    /// Subsonic/Navidrome remote-library use case: opens a decoder directly
    /// against an [`HttpRangeReader`], translating its `content_length()`
    /// convention (`0` = unknown) into the `Option<u64>` `open_reader` wants.
    pub fn open_http_range(reader: HttpRangeReader, hint_extension: Option<&str>) -> Result<Self> {
        let byte_len = match reader.content_length() {
            0 => None,
            len => Some(len),
        };
        Self::open_reader(reader, byte_len, hint_extension)
    }

    /// Open a non-seekable byte source for decoding — e.g. an internet
    /// radio (Shoutcast/Icecast) live stream. Unlike [`Self::open_reader`],
    /// the source only needs `Read + Send + Sync`: seeking is never
    /// supported (see [`StreamMediaSource`]), matching an unbounded live
    /// stream's actual capabilities.
    pub fn open_stream<R>(reader: R, hint_extension: Option<&str>) -> Result<Self>
    where
        R: Read + Send + Sync + 'static,
    {
        let mut hint = Hint::new();
        if let Some(ext) = hint_extension {
            hint.with_extension(ext);
        }

        let source = StreamMediaSource::new(reader);
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        Self::from_media_source(mss, hint)
    }

    /// Shared probe + track-selection + decoder-construction logic used by
    /// both [`Self::open`] and [`Self::open_reader`], so it's written once
    /// regardless of where the bytes come from.
    fn from_media_source(mss: MediaSourceStream<'static>, hint: Hint) -> Result<Self> {
        // Probe the media source.
        let reader = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| PlayerError(format!("Failed to probe format: {e}")))?;

        // Find the default audio track.
        let track = reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| PlayerError("No audio tracks found".to_owned()))?;

        let track_id = track.id;
        let time_base = track.time_base;

        // Get the audio codec parameters.
        let audio = match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(audio)) => audio,
            _ => return Err(PlayerError("No audio codec parameters".to_owned())),
        };

        // Store codec id for DSD detection.
        let codec_id = audio.codec;

        let sample_rate = audio
            .sample_rate
            .ok_or_else(|| PlayerError("Sample rate not available".to_owned()))?;

        // Channels might not be available until after decoding starts.
        let channels = audio.channels.as_ref().map(|ch| ch.count() as u8);

        // DSD metadata if available.
        let channel_data_layout = audio.channel_data_layout;
        let bit_order = audio.bit_order;

        // Calculate total duration from the track frame count and timebase.
        let total_duration = match (track.num_frames, time_base) {
            (Some(n_frames), Some(tb)) => tb
                .calc_time(Timestamp::new(n_frames as i64))
                .map(|t| t.as_secs_f64()),
            _ => None,
        };

        // Create decoder in pass-through mode (no PCM conversion).
        // PCM conversion can be enabled later if needed.
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(audio, &AudioDecoderOptions::default())
            .map_err(|e| PlayerError(format!("Failed to create decoder: {e}")))?;

        Ok(Self {
            reader,
            decoder,
            track_id,
            codec_id,
            sample_rate,
            channels,
            total_duration,
            sample_buf: Vec::new(),
            sample_pos: 0,
            current_bitrate: None,
            time_base,
            channel_data_layout,
            bit_order,
            uses_pcm_conversion: false,
        })
    }

    /// Check if this is a DSD file.
    pub fn is_dsd(&self) -> bool {
        self.codec_id == CODEC_TYPE_DSD
    }

    /// Enable PCM conversion for DSD (can be called multiple times with different rates).
    pub fn enable_pcm_conversion(&mut self, output_rate: u32) -> Result<()> {
        if self.codec_id != CODEC_TYPE_DSD {
            return Ok(()); // Not DSD, nothing to do
        }

        // If already enabled at the same rate, nothing to do.
        if self.uses_pcm_conversion && self.sample_rate == output_rate {
            return Ok(());
        }

        // Get the current track's audio codec parameters.
        let track = self
            .reader
            .tracks()
            .iter()
            .find(|t| t.id == self.track_id)
            .ok_or_else(|| PlayerError("Track not found".to_owned()))?;

        let audio = match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(audio)) => audio,
            _ => return Err(PlayerError("No audio codec parameters".to_owned())),
        };
        let input_rate = audio
            .sample_rate
            .ok_or_else(|| PlayerError("Sample rate not available".to_owned()))?;

        // Clone params and add PCM conversion mode via extra_data.
        let mut params_with_pcm = audio.clone();
        params_with_pcm.extra_data = Some(output_rate.to_le_bytes().to_vec().into_boxed_slice());

        tracing::info!(
            "enabling DSD-to-PCM conversion: {} Hz DSD -> {} Hz PCM",
            input_rate,
            output_rate
        );

        // Create new decoder with PCM conversion.
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params_with_pcm, &AudioDecoderOptions::default())
            .map_err(|e| PlayerError(format!("Failed to create PCM decoder: {e}")))?;

        // Get actual output sample rate from decoder.
        let actual_sample_rate = decoder
            .codec_params()
            .sample_rate
            .ok_or_else(|| PlayerError("Decoder sample rate not available".to_owned()))?;

        // Replace decoder.
        self.decoder = decoder;
        self.sample_rate = actual_sample_rate;
        self.uses_pcm_conversion = true;

        Ok(())
    }

    /// Read decoded, interleaved `f32` PCM samples into `buffer`, returning
    /// how many were written (may be less than `buffer.len()` only at
    /// end-of-stream). For a DSD file this yields the PCM decimation set up
    /// by [`Self::enable_pcm_conversion`], not raw DSD bits — see
    /// [`Self::read_dsd_raw`] for that.
    pub fn read(&mut self, buffer: &mut [f32]) -> Result<usize> {
        let mut samples_written = 0;

        while samples_written < buffer.len() {
            // Drain any buffered interleaved samples first.
            if self.sample_pos < self.sample_buf.len() {
                let available = self.sample_buf.len() - self.sample_pos;
                let to_copy = (buffer.len() - samples_written).min(available);
                buffer[samples_written..samples_written + to_copy]
                    .copy_from_slice(&self.sample_buf[self.sample_pos..self.sample_pos + to_copy]);
                samples_written += to_copy;
                self.sample_pos += to_copy;
                if samples_written >= buffer.len() {
                    break;
                }
            }

            // Read the next packet.
            let packet = match self.reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break, // End of stream.
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(e) => {
                    tracing::error!("failed to read packet: {}", e);
                    return Err(PlayerError(format!("Failed to read packet: {e}")));
                }
            };

            // Skip packets from other tracks.
            if packet.track_id != self.track_id {
                continue;
            }

            // Calculate instantaneous bitrate from the packet.
            if let Some(tb) = self.time_base
                && let Some(time) = tb.calc_time(Timestamp::new(packet.dur.get() as i64))
            {
                let duration_secs = time.as_secs_f64();
                if duration_secs > 0.0 {
                    let bitrate_bps = (packet.data.len() as f64 * 8.0) / duration_secs;
                    self.current_bitrate = Some((bitrate_bps / 1000.0) as u32);
                }
            }

            // Decode the packet.
            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => {
                    return Err(PlayerError(format!("Failed to decode packet: {e}")));
                }
            };

            // For DSD with PCM conversion, the decoder must return F32.
            if self.uses_pcm_conversion && !matches!(decoded, GenericAudioBufferRef::F32(_)) {
                tracing::error!("DSD-to-PCM decoder returned a non-F32 buffer");
                return Err(PlayerError(
                    "DSD decoder returned wrong sample format".to_owned(),
                ));
            }

            // Skip empty packets (can happen with metadata or padding).
            if decoded.frames() == 0 {
                continue;
            }

            // Update channels if not yet known.
            if self.channels.is_none() {
                self.channels = Some(decoded.spec().channels().count() as u8);
            }

            // Copy decoded audio as interleaved f32 into the reusable buffer.
            decoded.copy_to_vec_interleaved(&mut self.sample_buf);
            self.sample_pos = 0;
        }

        Ok(samples_written)
    }

    /// Seek to `position` seconds from the start of the track.
    pub fn seek(&mut self, position: f64) -> Result<()> {
        if position < 0.0 {
            return Err(PlayerError("Invalid seek position".to_owned()));
        }

        let time = Time::try_from_secs_f64(position)
            .ok_or_else(|| PlayerError("Invalid seek position".to_owned()))?;

        self.reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|e| PlayerError(format!("Seek failed: {e}")))?;

        self.decoder.reset();
        self.sample_buf.clear();
        self.sample_pos = 0;

        Ok(())
    }

    pub fn format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.sample_rate,
            channels: self.channels.unwrap_or(2), // Default to stereo if not yet known
            bits_per_sample: 16, // Symphonia decodes to f32; 16 is a display-only default
        }
    }

    pub fn duration(&self) -> Option<f64> {
        self.total_duration
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u8 {
        self.channels.unwrap_or(2) // Default to stereo if not yet known
    }

    /// Get the current instantaneous bitrate in kbps (for VBR files this changes during playback).
    pub fn current_bitrate(&self) -> Option<u32> {
        self.current_bitrate
    }

    /// Get channel data layout (planar vs interleaved) for DSD files.
    pub fn channel_data_layout(&self) -> Option<ChannelDataLayout> {
        self.channel_data_layout
    }

    /// Get bit order (LSB-first vs MSB-first) for DSD files.
    pub fn bit_order(&self) -> Option<BitOrder> {
        self.bit_order
    }

    /// Read raw DSD data (for DoP encoding).
    /// Returns raw DSD bytes without conversion.
    pub fn read_dsd_raw(&mut self, buffer: &mut Vec<u8>) -> Result<usize> {
        buffer.clear();

        let track_id = self.track_id;
        let packet = {
            let reader = &mut self.reader;
            let decoder = &mut self.decoder;
            next_dsd_packet(
                track_id,
                || match reader.next_packet() {
                    Ok(Some(packet)) => Ok(DsdPacketEvent::Packet {
                        track_id: packet.track_id,
                        packet,
                    }),
                    Ok(None) => Ok(DsdPacketEvent::End),
                    Err(SymphoniaError::IoError(e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        Ok(DsdPacketEvent::End)
                    }
                    Err(SymphoniaError::ResetRequired) => Ok(DsdPacketEvent::Reset),
                    Err(e) => Err(PlayerError(format!("Failed to read DSD packet: {e}"))),
                },
                || decoder.reset(),
            )
        }?;

        let Some(packet) = packet else {
            return Ok(0);
        };

        // For DSD, the packet buffer contains raw DSD data.
        // Copy it directly without decoding.
        buffer.extend_from_slice(&packet.data);

        Ok(buffer.len())
    }
}

/// Object-safe wrapper trait for the ordinary PCM decode path. DSD-specific
/// methods (`is_dsd`/`enable_pcm_conversion`/`read_dsd_raw`/etc.) are
/// deliberately not part of this trait, so DSD/DoP handling always goes
/// through the concrete [`SymphoniaDecoder`] type rather than a trait object.
pub trait Decoder: Send {
    fn read(&mut self, buffer: &mut [f32]) -> Result<usize>;
    fn seek(&mut self, position: f64) -> Result<()>;
    fn format(&self) -> AudioFormat;
    fn duration(&self) -> Option<f64>;
}

impl Decoder for SymphoniaDecoder {
    fn read(&mut self, buffer: &mut [f32]) -> Result<usize> {
        self.read(buffer)
    }
    fn seek(&mut self, position: f64) -> Result<()> {
        self.seek(position)
    }
    fn format(&self) -> AudioFormat {
        self.format()
    }
    fn duration(&self) -> Option<f64> {
        self.duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsd_packet_skip_loop_handles_many_events() {
        let mut skipped = 100_000;
        let packet = next_dsd_packet(
            1,
            || {
                if skipped == 0 {
                    Ok(DsdPacketEvent::Packet {
                        track_id: 1,
                        packet: 7,
                    })
                } else {
                    skipped -= 1;
                    Ok(DsdPacketEvent::Packet {
                        track_id: 2,
                        packet: 0,
                    })
                }
            },
            || {},
        )
        .unwrap();

        assert_eq!(packet, Some(7));
    }

    #[test]
    fn dsd_packet_skip_loop_rejects_repeated_resets() {
        let error = next_dsd_packet(
            1,
            || Ok::<_, PlayerError>(DsdPacketEvent::<()>::Reset),
            || {},
        )
        .unwrap_err();

        assert_eq!(error.0, "Too many consecutive DSD decoder resets");
    }


    /// Manual repro harness (network-dependent, not run in CI): opens a
    /// real public Icecast/Shoutcast live stream exactly the way
    /// `PlaySource::LiveStream::open_decoder` does (blocking GET +
    /// `Icy-MetaData: 1` header, wrapped in `icy_metadata::IcyMetadataReader`
    /// when the server advertises a metadata interval, then
    /// `SymphoniaDecoder::open_stream`), then calls `read()` in a loop for
    /// ~25 real seconds, logging every `Read::read` call the underlying
    /// socket sees (via a counting wrapper) alongside every `decoder.read()`
    /// result. Run with:
    /// `cargo test --package lyra --lib player::engine::decoder::tests::repro_live_stream_stops_after_a_few_seconds -- --ignored --nocapture`
    #[test]
    #[ignore = "network-dependent manual repro harness"]
    fn repro_live_stream_stops_after_a_few_seconds() {
        use std::time::{Duration, Instant};

        struct CountingReader<R> {
            inner: R,
            start: Instant,
            total: u64,
        }
        impl<R: std::io::Read> std::io::Read for CountingReader<R> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let t0 = Instant::now();
                let result = self.inner.read(buf);
                let elapsed = t0.elapsed();
                match &result {
                    Ok(n) => {
                        self.total += *n as u64;
                        eprintln!(
                            "[{:>7.3}s] socket.read() -> Ok({n}) after {:>6.1}ms (total={} bytes)",
                            self.start.elapsed().as_secs_f64(),
                            elapsed.as_secs_f64() * 1000.0,
                            self.total
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "[{:>7.3}s] socket.read() -> Err({e:?}) [kind={:?}] after {:>6.1}ms",
                            self.start.elapsed().as_secs_f64(),
                            e.kind(),
                            elapsed.as_secs_f64() * 1000.0
                        );
                    }
                }
                result
            }
        }

        let url = std::env::var("LYRA_TEST_RADIO_URL")
            .unwrap_or_else(|_| "http://ice1.somafm.com/groovesalad-128-mp3".to_string());
        eprintln!("connecting to {url}");

        // Mirrors `PlaySource::LiveStream::open_decoder` exactly, including
        // the client (no explicit timeout — same as `LocalBackend`'s
        // `reqwest::blocking::Client::new()`).
        let client = reqwest::blocking::Client::new();
        let response = client
            .get(&url)
            .header("Icy-MetaData", "1")
            .send()
            .expect("connect");
        eprintln!("status={} headers={:#?}", response.status(), response.headers());
        let metadata_interval =
            icy_metadata::IcyHeaders::parse_from_headers(response.headers()).metadata_interval();
        eprintln!("icy metadata_interval={metadata_interval:?}");

        let counting = CountingReader {
            inner: response,
            start: Instant::now(),
            total: 0,
        };

        let mut decoder = match metadata_interval {
            Some(interval) => {
                let reader = icy_metadata::IcyMetadataReader::new(
                    counting,
                    Some(interval),
                    |metadata| {
                        if let Ok(m) = metadata {
                            eprintln!("ICY title: {:?}", m.stream_title());
                        }
                    },
                );
                SymphoniaDecoder::open_stream(reader, None).expect("open_stream")
            }
            None => SymphoniaDecoder::open_stream(counting, None).expect("open_stream"),
        };

        eprintln!("opened decoder: duration={:?}", decoder.duration());

        let start = Instant::now();
        let mut buffer = vec![0f32; 4096];
        let mut total_samples: u64 = 0;
        let mut iterations: u64 = 0;
        while start.elapsed() < Duration::from_secs(25) {
            iterations += 1;
            match decoder.read(&mut buffer) {
                Ok(0) => {
                    eprintln!(
                        "[{:>7.3}s] decoder.read() -> Ok(0) — EOS after {iterations} calls, {total_samples} samples",
                        start.elapsed().as_secs_f64()
                    );
                    panic!(
                        "live stream reported end-of-stream after only {:.1}s — this is the bug",
                        start.elapsed().as_secs_f64()
                    );
                }
                Ok(n) => {
                    total_samples += n as u64;
                }
                Err(e) => {
                    eprintln!("[{:>7.3}s] decoder.read() -> Err({e})", start.elapsed().as_secs_f64());
                    panic!("live stream decode error after {:.1}s: {e}", start.elapsed().as_secs_f64());
                }
            }
        }
        eprintln!(
            "OK: still streaming after {:.1}s ({iterations} read() calls, {total_samples} samples)",
            start.elapsed().as_secs_f64()
        );
    }
}
