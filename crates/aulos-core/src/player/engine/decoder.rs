// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Symphonia-based audio decoder.
//!
//! One decoder implementation only — no MPD-style `DecoderPlugin` registry,
//! since a compile-time registry only earns its keep with multiple
//! interchangeable implementations. There's also no ICY "now playing" title
//! support for internet radio streams, since aulos doesn't need it.
//!
//! [`SymphoniaDecoder::open`] (local files) and
//! [`SymphoniaDecoder::open_reader`] (arbitrary readers — used for HTTP
//! streaming from aulos's Subsonic/Navidrome remote libraries via
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
use symphonia::core::formats::well_known::FORMAT_ID_OGG;
use symphonia::core::formats::{
    FormatId, FormatOptions, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, TrackType,
};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;
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

/// Minimal audio format descriptor. Dependency-free by design — aulos has no
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
    /// Pending sample-accurate seek: decoded frames before the requested timestamp are
    /// discarded.
    seek_skip: Option<SeekSkip>,
}

const MAX_CONSECUTIVE_DSD_RESETS: usize = 1024;

enum DsdPacketEvent<T> {
    Packet { track_id: u32, packet: T },
    Reset,
    End,
}

/// Pull the next packet of the current DSD track, handling resets via `reset` (which may
/// change the track id, hence it is re-read from `state` for every packet).
fn next_dsd_packet<S, T>(
    state: &mut S,
    track_id: impl Fn(&S) -> u32,
    mut next_event: impl FnMut(&mut S) -> Result<DsdPacketEvent<T>>,
    mut reset: impl FnMut(&mut S) -> Result<()>,
) -> Result<Option<T>> {
    let mut consecutive_resets = 0;

    loop {
        match next_event(state)? {
            DsdPacketEvent::Packet {
                track_id: packet_track_id,
                packet,
            } => {
                consecutive_resets = 0;
                if packet_track_id == track_id(state) {
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
                reset(state)?;
            }
            DsdPacketEvent::End => return Ok(None),
        }
    }
}

/// `FormatOptions` for probing the local file `path`: a WavPack `.wv` gets its sibling `.wvc`
/// correction file attached (hybrid lossless); everything else gets the defaults. Never use
/// for streams/remote sources.
pub fn local_file_format_options(path: &Path) -> FormatOptions {
    let opts = FormatOptions::default();
    let is_wv = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wv"));
    if is_wv {
        symphonia::default::formats::wavpack_with_sibling_correction(path, opts)
    } else {
        opts
    }
}

/// How often a seek is repeated after the demuxer returned `ResetRequired` (a time seek that
/// crosses into another link of a chained Ogg stream switches links and asks for a reset; the
/// repeated seek then completes inside the new link).
pub(crate) const MAX_SEEK_RESETS: u32 = 2;

/// Something that can seek and be rebuilt after a reset (the decoder; a mock in tests).
pub(crate) trait ResettableSeek {
    fn try_seek(&mut self, time: Time) -> std::result::Result<SeekedTo, SymphoniaError>;
    fn rebuild_after_reset(&mut self) -> std::result::Result<(), SymphoniaError>;
}

impl ResettableSeek for SymphoniaDecoder {
    fn try_seek(&mut self, time: Time) -> std::result::Result<SeekedTo, SymphoniaError> {
        // The track id is read on every attempt: it changes when the reset switched links.
        self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        )
    }

    fn rebuild_after_reset(&mut self) -> std::result::Result<(), SymphoniaError> {
        self.reinit_after_reset()
    }
}

/// Seek, and on `ResetRequired` rebuild and repeat the same seek, at most `max_resets` times.
pub(crate) fn seek_with_resets<T: ResettableSeek + ?Sized>(
    target: &mut T,
    time: Time,
    max_resets: u32,
) -> std::result::Result<SeekedTo, SymphoniaError> {
    let mut resets = 0;
    loop {
        match target.try_seek(time) {
            Err(SymphoniaError::ResetRequired) if resets < max_resets => {
                resets += 1;
                target.rebuild_after_reset()?;
            }
            other => return other,
        }
    }
}

/// Duration in seconds of a whole chained Ogg stream, if the reader describes one (media info
/// with a nanosecond timebase). `None` for anything else, so the per-track duration is used.
#[must_use]
pub fn chain_duration_secs(format: FormatId, media_info: &MediaInfo) -> Option<f64> {
    if format != FORMAT_ID_OGG {
        return None;
    }
    let tb = media_info.time_base?;
    if tb.numer.get() != 1 || tb.denom.get() != 1_000_000_000 {
        return None;
    }
    tb.calc_duration(media_info.duration?)
        .map(|t| t.as_secs_f64())
}

/// Convert a span of `ticks` of timebase `tb` into frames at `rate` Hz (rounded to nearest).
fn ticks_to_frames(ticks: u64, tb: TimeBase, rate: u32) -> u64 {
    let denom = u128::from(tb.denom.get());
    let num = u128::from(ticks) * u128::from(tb.numer.get()) * u128::from(rate);
    u64::try_from((num + denom / 2) / denom).unwrap_or(u64::MAX)
}

/// Frames to drop after a seek so playback starts exactly at the requested timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekSkip {
    /// The timestamp playback must start at, in the track timebase.
    required: Timestamp,
    /// Upper bound of frames to discard: the announced distance plus one second.
    budget: u64,
}

impl SeekSkip {
    /// `None` when nothing has to be discarded (landed exactly on, or after, the target).
    pub(crate) fn new(
        required: Timestamp,
        actual: Timestamp,
        tb: Option<TimeBase>,
        rate: u32,
    ) -> Option<Self> {
        let tb = tb?;
        let gap = required.duration_from(actual)?;
        if gap.is_zero() {
            return None;
        }
        let budget = ticks_to_frames(gap.get(), tb, rate).saturating_add(u64::from(rate));
        Some(Self { required, budget })
    }

    /// Account for one decoded buffer of `decoded_frames` frames whose first valid frame is at
    /// `valid_start`. Returns how many leading frames to discard and whether the target has
    /// been reached.
    pub(crate) fn advance(
        &mut self,
        valid_start: Timestamp,
        decoded_frames: usize,
        tb: TimeBase,
        rate: u32,
    ) -> (usize, bool) {
        let gap = match self.required.duration_from(valid_start) {
            Some(gap) if !gap.is_zero() => gap,
            _ => return (0, true),
        };
        let gap_frames = ticks_to_frames(gap.get(), tb, rate);
        let skip = gap_frames.min(decoded_frames as u64).min(self.budget);
        self.budget -= skip;
        let reached = gap_frames <= decoded_frames as u64 || self.budget == 0;
        (skip as usize, reached)
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

        // A local WavPack `.wv` picks up a sibling `.wvc` (hybrid lossless).
        Self::from_media_source(mss, hint, local_file_format_options(path))
    }

    /// Open an arbitrary seekable byte source for decoding — e.g. a remote
    /// track streamed through aulos's [`HttpRangeReader`]. `byte_len` should
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

        Self::from_media_source(mss, hint, FormatOptions::default())
    }

    /// Convenience wrapper over [`Self::open_reader`] for aulos's own
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

        Self::from_media_source(mss, hint, FormatOptions::default())
    }

    /// Shared probe + track-selection + decoder-construction logic used by
    /// both [`Self::open`] and [`Self::open_reader`], so it's written once
    /// regardless of where the bytes come from.
    fn from_media_source(
        mss: MediaSourceStream<'static>,
        hint: Hint,
        format_opts: FormatOptions,
    ) -> Result<Self> {
        // Probe the media source.
        let reader = symphonia::default::get_probe()
            .probe(&hint, mss, format_opts, MetadataOptions::default())
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

        // Total duration. A chained Ogg stream reports the whole chain's duration at the media
        // level (the track only describes the first link); otherwise use the track frame
        // count and timebase.
        let total_duration = chain_duration_secs(reader.format_info().format, reader.media_info())
            .or_else(|| match (track.num_frames, time_base) {
                (Some(n_frames), Some(tb)) => tb
                    .calc_time(Timestamp::new(n_frames as i64))
                    .map(|t| t.as_secs_f64()),
                _ => None,
            });

        // Create decoder in pass-through mode (no PCM conversion).
        // PCM conversion can be enabled later if needed.
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(audio, &AudioDecoderOptions::default())
            .map_err(|e| PlayerError(format!("Failed to create decoder: {e}")))?;

        // The decoder may report a different output rate than the container (explicitly
        // signalled HE-AAC decodes at twice the AAC core rate). Raw DSD keeps the DSD rate.
        let sample_rate = if codec_id == CODEC_TYPE_DSD {
            sample_rate
        } else {
            decoder.codec_params().sample_rate.unwrap_or(sample_rate)
        };

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
            seek_skip: None,
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
                    self.reinit_after_reset().map_err(|e| {
                        PlayerError(format!("Failed to reinitialise after stream change: {e}"))
                    })?;
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

            // Calculate instantaneous bitrate from the packet (full block duration, not the
            // trimmed one, so trimmed edge packets don't spike the figure).
            if let Some(tb) = self.time_base
                && let Some(time) = tb.calc_time(Timestamp::new(packet.block_dur().get() as i64))
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
            let frames = decoded.frames();
            let rate = decoded.spec().rate();
            let channel_count = decoded.spec().channels().count();
            decoded.copy_to_vec_interleaved(&mut self.sample_buf);
            // After a seek, drop the frames in front of the requested position.
            let skip_frames = self.seek_skip_frames(&packet, frames, rate);
            self.sample_pos = (skip_frames * channel_count).min(self.sample_buf.len());
        }

        Ok(samples_written)
    }

    /// Seek to `position` seconds from the start of the track. Sample-accurate: the demuxer
    /// lands at or before the target (Opus/MKV even start a pre-roll earlier) and the
    /// frames before the requested timestamp are discarded by [`Self::read`].
    pub fn seek(&mut self, position: f64) -> Result<()> {
        if position < 0.0 {
            return Err(PlayerError("Invalid seek position".to_owned()));
        }

        let time = Time::try_from_secs_f64(position)
            .ok_or_else(|| PlayerError("Invalid seek position".to_owned()))?;

        // The demuxer may need a reset to complete the seek (a time seek into another link of
        // a chained Ogg stream): re-read the tracks, re-create the decoder, repeat the seek.
        self.seek_skip = None;
        let seeked = seek_with_resets(self, time, MAX_SEEK_RESETS)
            .map_err(|e| PlayerError(format!("Seek failed: {e}")))?;

        self.decoder.reset();
        self.sample_buf.clear();
        self.sample_pos = 0;

        // Raw (pass-through) DSD is not decoded here, so there is nothing to discard.
        let raw_dsd = self.codec_id == CODEC_TYPE_DSD && !self.uses_pcm_conversion;
        self.seek_skip = (!raw_dsd).then_some(seeked).and_then(|s| {
            SeekSkip::new(s.required_ts, s.actual_ts, self.time_base, self.sample_rate)
        });

        Ok(())
    }

    /// Number of leading frames of the just-decoded `packet` to discard to honour a pending
    /// sample-accurate seek, updating (and eventually clearing) the seek state.
    fn seek_skip_frames(&mut self, packet: &Packet, frames: usize, rate: u32) -> usize {
        let Some(tb) = self.time_base else {
            self.seek_skip = None;
            return 0;
        };
        let Some(skip) = self.seek_skip.as_mut() else {
            return 0;
        };
        // `pts` is the start of the decoded block; `trim_start` frames (encoder delay /
        // pre-roll flagged by the demuxer) are already removed from the decoded buffer.
        let valid_start = packet.pts.saturating_add(packet.trim_start);
        let (n, done) = skip.advance(valid_start, frames, tb, rate);
        if done {
            self.seek_skip = None;
        }
        n
    }

    /// Re-read the track list after the demuxer returned `ResetRequired` (a new link of a
    /// chained Ogg stream, ...) and re-create the decoder for the default audio track.
    fn reinit_after_reset(&mut self) -> std::result::Result<(), SymphoniaError> {
        let (track_id, time_base, audio) =
            {
                let track = self.reader.default_track(TrackType::Audio).ok_or(
                    SymphoniaError::Unsupported("no audio track after stream reset"),
                )?;
                let audio = match track.codec_params.as_ref() {
                    Some(CodecParameters::Audio(audio)) => audio.clone(),
                    _ => {
                        return Err(SymphoniaError::Unsupported(
                            "no audio codec parameters after stream reset",
                        ));
                    }
                };
                (track.id, track.time_base, audio)
            };

        let is_dsd = audio.codec == CODEC_TYPE_DSD;
        let keep_pcm = self.uses_pcm_conversion && is_dsd;
        let mut params = audio.clone();
        if keep_pcm {
            // `sample_rate` is the PCM output rate while DSD-to-PCM conversion is active.
            params.extra_data = Some(self.sample_rate.to_le_bytes().to_vec().into_boxed_slice());
        }
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())?;

        // Pass-through DSD keeps the container (DSD) rate, which DoP needs.
        let new_rate = if keep_pcm {
            self.sample_rate
        } else if is_dsd {
            audio.sample_rate.unwrap_or(self.sample_rate)
        } else {
            decoder
                .codec_params()
                .sample_rate
                .or(audio.sample_rate)
                .unwrap_or(self.sample_rate)
        };
        let new_channels = audio.channels.as_ref().map(|c| c.count() as u8);
        if new_rate != self.sample_rate || (new_channels.is_some() && new_channels != self.channels)
        {
            // The output was opened for the first link's format and cannot follow.
            tracing::warn!(
                "stream format changed mid-stream: {} Hz/{:?} ch -> {} Hz/{:?} ch",
                self.sample_rate,
                self.channels,
                new_rate,
                new_channels
            );
        }

        self.decoder = decoder;
        self.track_id = track_id;
        self.time_base = time_base;
        self.codec_id = audio.codec;
        self.sample_rate = new_rate;
        self.channels = new_channels;
        self.channel_data_layout = audio.channel_data_layout;
        self.bit_order = audio.bit_order;
        self.uses_pcm_conversion = keep_pcm;
        self.sample_buf.clear();
        self.sample_pos = 0;
        self.seek_skip = None;
        self.current_bitrate = None;
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

        let packet = next_dsd_packet(
            self,
            |d| d.track_id,
            |d| match d.reader.next_packet() {
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
            |d| {
                d.reinit_after_reset().map_err(|e| {
                    PlayerError(format!("Failed to reinitialise after stream change: {e}"))
                })
            },
        )?;

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
            &mut skipped,
            |_| 1,
            |skipped| {
                if *skipped == 0 {
                    Ok(DsdPacketEvent::Packet {
                        track_id: 1,
                        packet: 7,
                    })
                } else {
                    *skipped -= 1;
                    Ok(DsdPacketEvent::Packet {
                        track_id: 2,
                        packet: 0,
                    })
                }
            },
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(packet, Some(7));
    }

    #[test]
    fn dsd_packet_skip_loop_rejects_repeated_resets() {
        let error = next_dsd_packet(
            &mut (),
            |_| 1,
            |_| Ok::<_, PlayerError>(DsdPacketEvent::<()>::Reset),
            |_| Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.0, "Too many consecutive DSD decoder resets");
    }

    #[test]
    fn dsd_reset_can_change_the_track_id() {
        // After the reset the track id is 5; packets of the old id 1 must be skipped.
        let mut state = (1u32, 0u32);
        let packet = next_dsd_packet(
            &mut state,
            |s| s.0,
            |s| {
                s.1 += 1;
                Ok(match s.1 {
                    1 => DsdPacketEvent::Reset,
                    2 => DsdPacketEvent::Packet {
                        track_id: 1,
                        packet: 0,
                    },
                    _ => DsdPacketEvent::Packet {
                        track_id: 5,
                        packet: 9,
                    },
                })
            },
            |s| {
                s.0 = 5;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(packet, Some(9));
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
    /// `cargo test --package aulos --lib player::engine::decoder::tests::repro_live_stream_stops_after_a_few_seconds -- --ignored --nocapture`
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

        let url = std::env::var("AULOS_TEST_RADIO_URL")
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
        eprintln!(
            "status={} headers={:#?}",
            response.status(),
            response.headers()
        );
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
                let reader =
                    icy_metadata::IcyMetadataReader::new(counting, Some(interval), |metadata| {
                        if let Ok(m) = metadata {
                            eprintln!("ICY title: {:?}", m.stream_title());
                        }
                    });
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
                    eprintln!(
                        "[{:>7.3}s] decoder.read() -> Err({e})",
                        start.elapsed().as_secs_f64()
                    );
                    panic!(
                        "live stream decode error after {:.1}s: {e}",
                        start.elapsed().as_secs_f64()
                    );
                }
            }
        }
        eprintln!(
            "OK: still streaming after {:.1}s ({iterations} read() calls, {total_samples} samples)",
            start.elapsed().as_secs_f64()
        );
    }
}

#[cfg(test)]
mod seek_tests {
    use super::*;
    use std::collections::VecDeque;
    use symphonia::core::errors::SeekErrorKind;

    type SeekResult = std::result::Result<SeekedTo, SymphoniaError>;

    fn seeked(required: i64, actual: i64) -> SeekedTo {
        SeekedTo {
            track_id: 0,
            required_ts: Timestamp::new(required),
            actual_ts: Timestamp::new(actual),
        }
    }

    /// Scripted reader: each seek pops the next result; records the targets and rebuilds.
    struct Mock {
        results: VecDeque<SeekResult>,
        seeks: Vec<Time>,
        rebuilds: u32,
        rebuild_fails: bool,
    }

    impl Mock {
        fn new(results: Vec<SeekResult>) -> Self {
            Self {
                results: results.into(),
                seeks: Vec::new(),
                rebuilds: 0,
                rebuild_fails: false,
            }
        }
    }

    impl ResettableSeek for Mock {
        fn try_seek(&mut self, time: Time) -> SeekResult {
            self.seeks.push(time);
            self.results.pop_front().expect("unexpected extra seek")
        }

        fn rebuild_after_reset(&mut self) -> std::result::Result<(), SymphoniaError> {
            self.rebuilds += 1;
            if self.rebuild_fails {
                Err(SymphoniaError::Unsupported("no track"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn seek_without_reset_does_not_rebuild() {
        let mut m = Mock::new(vec![Ok(seeked(10, 8))]);
        let t = Time::from_millis(1500);
        let got = seek_with_resets(&mut m, t, MAX_SEEK_RESETS).unwrap();
        assert_eq!(got.actual_ts, Timestamp::new(8));
        assert_eq!(m.rebuilds, 0);
        assert_eq!(m.seeks, vec![t]);
    }

    #[test]
    fn reset_rebuilds_and_repeats_the_same_seek() {
        let mut m = Mock::new(vec![Err(SymphoniaError::ResetRequired), Ok(seeked(10, 10))]);
        let t = Time::from_millis(90_000);
        assert!(seek_with_resets(&mut m, t, MAX_SEEK_RESETS).is_ok());
        assert_eq!(m.rebuilds, 1);
        assert_eq!(m.seeks, vec![t, t], "the very same target must be retried");
    }

    #[test]
    fn endless_resets_are_bounded() {
        let mut m = Mock::new(vec![
            Err(SymphoniaError::ResetRequired),
            Err(SymphoniaError::ResetRequired),
            Err(SymphoniaError::ResetRequired),
        ]);
        let err = seek_with_resets(&mut m, Time::from_millis(1), 2).unwrap_err();
        assert!(matches!(err, SymphoniaError::ResetRequired));
        assert_eq!(m.rebuilds, 2);
        assert_eq!(m.seeks.len(), 3);
    }

    #[test]
    fn other_errors_pass_through_without_rebuild() {
        let mut m = Mock::new(vec![Err(SymphoniaError::SeekError(
            SeekErrorKind::OutOfRange,
        ))]);
        let err = seek_with_resets(&mut m, Time::from_millis(1), 2).unwrap_err();
        assert!(matches!(
            err,
            SymphoniaError::SeekError(SeekErrorKind::OutOfRange)
        ));
        assert_eq!(m.rebuilds, 0);
    }

    #[test]
    fn rebuild_failure_aborts_the_seek() {
        let mut m = Mock::new(vec![Err(SymphoniaError::ResetRequired)]);
        m.rebuild_fails = true;
        let err = seek_with_resets(&mut m, Time::from_millis(1), 2).unwrap_err();
        assert!(matches!(err, SymphoniaError::Unsupported(_)));
        assert_eq!(m.seeks.len(), 1);
    }

    fn tb(denom: u32) -> TimeBase {
        TimeBase::try_new(1, denom).unwrap()
    }

    #[test]
    fn skips_the_seek_pre_roll_exactly() {
        // Matroska audio (timebase 1/rate): 200 ms pre-roll before the requested timestamp.
        let rate = 48_000;
        let required = 5 * 48_000 + 123;
        let actual = required - 9_600;
        let mut skip = SeekSkip::new(
            Timestamp::new(required),
            Timestamp::new(actual),
            Some(tb(rate)),
            rate,
        )
        .unwrap();
        let mut skipped = 0;
        let mut reached = false;
        for i in 0..64 {
            let pts = Timestamp::new(actual + i * 1152);
            let (n, done) = skip.advance(pts, 1152, tb(rate), rate);
            skipped += n;
            if done {
                reached = true;
                break;
            }
        }
        assert_eq!(skipped, 9_600, "exactly required - actual frames");
        assert!(reached);
    }

    #[test]
    fn skips_within_a_packet_after_a_coarse_landing() {
        let rate = 44_100;
        let mut skip = SeekSkip::new(
            Timestamp::new(10_000),
            Timestamp::new(9_000),
            Some(tb(rate)),
            rate,
        )
        .unwrap();
        assert_eq!(
            skip.advance(Timestamp::new(9_000), 4096, tb(rate), rate),
            (1_000, true)
        );
    }

    #[test]
    fn nothing_to_skip_when_the_seek_landed_on_or_after_the_target() {
        let t = Some(tb(48_000));
        assert!(SeekSkip::new(Timestamp::new(100), Timestamp::new(100), t, 48_000).is_none());
        assert!(SeekSkip::new(Timestamp::new(100), Timestamp::new(200), t, 48_000).is_none());
        assert!(SeekSkip::new(Timestamp::new(100), Timestamp::new(0), None, 48_000).is_none());
    }

    #[test]
    fn unreliable_timestamps_cannot_swallow_the_stream() {
        let rate = 48_000;
        let mut skip = SeekSkip::new(
            Timestamp::new(48_000),
            Timestamp::new(38_400),
            Some(tb(rate)),
            rate,
        )
        .unwrap();
        let mut skipped = 0usize;
        for _ in 0..1000 {
            let (n, done) = skip.advance(Timestamp::new(0), 1024, tb(rate), rate);
            skipped += n;
            if done {
                break;
            }
        }
        assert!(skipped as u64 <= 9_600 + u64::from(rate));
    }

    fn ns_media_info(secs: u64) -> MediaInfo {
        let mut mi = MediaInfo::new();
        mi.with_time_base(TimeBase::try_new(1, 1_000_000_000).unwrap());
        mi.with_duration(symphonia::core::units::Duration::new(secs * 1_000_000_000));
        mi
    }

    #[test]
    fn ogg_chain_duration_comes_from_media_info() {
        let d = chain_duration_secs(FORMAT_ID_OGG, &ns_media_info(95)).unwrap();
        assert!((d - 95.0).abs() < 1e-9);
    }

    #[test]
    fn chain_duration_is_ogg_and_ns_timebase_only() {
        use symphonia::core::formats::well_known::FORMAT_ID_FLAC;
        assert!(chain_duration_secs(FORMAT_ID_FLAC, &ns_media_info(95)).is_none());
        let mut mi = MediaInfo::new();
        mi.with_time_base(TimeBase::try_new(1, 48_000).unwrap());
        mi.with_duration(symphonia::core::units::Duration::new(48_000));
        assert!(chain_duration_secs(FORMAT_ID_OGG, &mi).is_none());
        assert!(chain_duration_secs(FORMAT_ID_OGG, &MediaInfo::new()).is_none());
    }
}
