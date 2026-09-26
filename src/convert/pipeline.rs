// SPDX-License-Identifier: GPL-3.0

//! Standalone decode → resample → encode pipeline for local file
//! conversion, transcoding, and CUE-sheet splitting.
//!
//! Decoding uses the same probe/track-selection pattern as
//! [`crate::player::engine::decoder`], but against a plain `File` /
//! `MediaSourceStream` — no DSD, no HTTP streaming, since this pipeline
//! never touches playback. Because symphonia's probe is content-based, this
//! transparently "rips" the first audio track out of video containers
//! (mp4/mkv) too, with no container-specific special-casing.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use crate::library::tags::{self as track_tags, Picture};
use crate::player::engine::resampler::{ResamplerQuality, StreamResampler};

use super::cue;
use super::encoder;
use super::tag_writer::{self, WriteTags};
use super::{ConvertError, ConvertJob, JobId, JobKind, JobSettings};

/// The settings `job` is running with. Panics if called before
/// [`super::ConvertJob::start`] set them — every job passed to `run` here
/// has already transitioned `Queued` -> `Running`, which always sets them.
fn settings(job: &ConvertJob) -> &JobSettings {
    job.settings
        .as_ref()
        .expect("pipeline::run called on a job without settings (not yet started)")
}

/// Runs `job` to completion: decodes, optionally resamples, encodes, and
/// tags the output(s). Checks `job.cancel` between packets. Encoding
/// happens on a scratch file next to the real output; on cancellation, a
/// decode error, or an encode error the scratch file is deleted and the
/// real output path is never touched, so nothing partial is ever left
/// behind at the name the caller asked for.
pub fn run(job: &ConvertJob) -> Result<(), ConvertError> {
    std::fs::create_dir_all(&settings(job).out_dir)?;
    match job.kind {
        JobKind::Convert => {
            let stem = job.source.file_stem().and_then(|s| s.to_str()).unwrap_or("track");
            let out_path = unique_out_path(&settings(job).out_dir, stem, settings(job).format.extension());
            transcode(job, &job.source, &out_path, None, None, 0, 1000)?;
            copy_tags(&job.source, &out_path, settings(job).format);
            Ok(())
        }
        JobKind::CueSplit => cue_split(job),
    }
}

/// Splits the audio file referenced by a `.cue` sheet (`job.source`) into
/// one tagged output file per track. Each track's slice of the overall
/// `job.progress` permille range is fixed up front by
/// [`track_progress_range`] (equal-weight per track), so progress climbs
/// monotonically across the whole rip instead of resetting to ~0 at every
/// track boundary — `transcode` alone can't know it's one of several calls
/// sharing a single job's progress bar.
fn cue_split(job: &ConvertJob) -> Result<(), ConvertError> {
    let cue_text = std::fs::read_to_string(&job.source)?;
    let tracks = cue::parse(&cue_text).map_err(|e| ConvertError::Cue(e.to_string()))?;
    let file_name = cue::parse_file_name(&cue_text)
        .ok_or_else(|| ConvertError::Cue("no FILE line in CUE sheet".to_owned()))?;
    let audio_path = job
        .source
        .parent()
        .map(|dir| dir.join(&file_name))
        .unwrap_or_else(|| PathBuf::from(&file_name));

    // Read the whole album's tag once so every track can carry over the
    // fields the CUE sheet itself doesn't encode (album, genre, date, cover
    // art), rather than each track ending up untagged beyond its title.
    let src_tags = track_tags::probe(&audio_path, true).map(|p| p.tags);

    let track_count = tracks.len();
    for (i, track) in tracks.iter().enumerate() {
        if job.cancel.load(Ordering::Relaxed) {
            return Err(ConvertError::Cancelled);
        }

        let stem = format!("{:02} - {}", track.number, sanitize_filename(&track.title));
        let out_path = unique_out_path(&settings(job).out_dir, &stem, settings(job).format.extension());
        let start = track.start.as_secs_f64();
        let end = track.end.map(|d| d.as_secs_f64());
        let (progress_base, progress_span) = track_progress_range(i, track_count);
        transcode(job, &audio_path, &out_path, Some(start), end, progress_base, progress_span)?;

        let write = WriteTags {
            title: Some(track.title.as_str()),
            artist: Some(track.performer.as_str()),
            track_number: Some(track.number),
            track_total: Some(track_count as u32),
            ..src_tags.as_ref().map(shared_tags).unwrap_or_default()
        };
        write_output_tags(&out_path, settings(job).format, &write);
    }
    Ok(())
}

/// Computes the `(progress_base, progress_span)` permille slice that CUE
/// track `index` (0-based) of `total` tracks owns within the job's overall
/// progress bar. Allocates equal weight per track rather than by duration —
/// exact duration-weighting would need a separate full-file probe pass,
/// which isn't worth the complexity here. Integer division keeps the spans
/// exact: they sum to 1000 with no drift, for any `total >= 1`.
fn track_progress_range(index: usize, total: usize) -> (u32, u32) {
    let total = total.max(1) as u32;
    let index = index as u32;
    let base = 1000 * index / total;
    let next_base = 1000 * (index + 1) / total;
    (base, next_base - base)
}

/// Decodes `[start, end)` seconds of `source_path` (the whole file when
/// both are `None`) and encodes it to `out_path` per `job.format` /
/// `job.target_rate`. `progress_base`/`progress_span` place this call's
/// own 0-1000 permille progress within a larger slice of `job.progress` —
/// `[progress_base, progress_base + progress_span]`, i.e. `(0, 1000)` for
/// a plain whole-file conversion, or a per-track slice when `cue_split`
/// calls this once per track and needs the job's progress to climb
/// monotonically across all of them instead of restarting at each track.
///
/// Encodes into a same-directory scratch file and installs it with an
/// atomic rename only once it's fully written — see [`TempFileGuard`].
fn transcode(
    job: &ConvertJob,
    source_path: &Path,
    out_path: &Path,
    start: Option<f64>,
    end: Option<f64>,
    progress_base: u32,
    progress_span: u32,
) -> Result<(), ConvertError> {
    let mut source = AudioSource::open(source_path)?;
    if let Some(start) = start.filter(|&s| s > 0.0) {
        source.seek(start)?;
    }

    let src_rate = source.sample_rate;
    let channels = source.channels;
    let dst_rate = settings(job).target_rate.unwrap_or(src_rate);
    let mut resampler = (dst_rate != src_rate)
        .then(|| StreamResampler::new(src_rate, dst_rate, channels as usize, ResamplerQuality::SincMedium))
        .flatten();

    let tmp_path = temp_out_path(out_path, job.id);
    let mut tmp_guard = TempFileGuard::new(tmp_path.clone());
    let mut sink = encoder::create_sink(
        settings(job).format,
        &tmp_path,
        channels,
        dst_rate,
        source.bits_per_sample,
        settings(job).flac_options,
        settings(job).lossy_options,
    )?;

    // Frame budget for the `[start, end)` window, in source-domain frames.
    let max_frames = end.map(|e| ((e - start.unwrap_or(0.0)).max(0.0) * f64::from(src_rate)).round() as u64);

    const CHUNK_FRAMES: usize = 8192;
    let mut buf = vec![0f32; CHUNK_FRAMES * channels.max(1) as usize];
    let mut frames_done: u64 = 0;

    loop {
        if job.cancel.load(Ordering::Relaxed) {
            return Err(ConvertError::Cancelled);
        }

        let mut want = buf.len();
        if let Some(max) = max_frames {
            let remaining = max.saturating_sub(frames_done) as usize * channels.max(1) as usize;
            if remaining == 0 {
                break;
            }
            want = want.min(remaining);
        }

        let n = source.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        frames_done += (n / channels.max(1) as usize) as u64;

        let chunk = &buf[..n];
        match resampler.as_mut() {
            Some(rs) => sink.write(&rs.process(chunk))?,
            None => sink.write(chunk)?,
        }

        job.progress.store(
            progress_base + source.progress_permille(frames_done) * progress_span / 1000,
            Ordering::Relaxed,
        );
    }

    // `StreamResampler::process` only emits output once a full internal
    // chunk of input has accumulated, so it's always holding back a
    // fractional chunk plus its filter's group delay. Without draining
    // that here, every resampled conversion loses its true tail — for a
    // clip shorter than one resampler chunk, the entire output is silently
    // empty.
    if let Some(rs) = resampler.as_mut() {
        let tail = rs.flush();
        if !tail.is_empty() {
            sink.write(&tail)?;
        }
    }

    sink.finish()?;
    std::fs::rename(&tmp_path, out_path)?;
    tmp_guard.disarm();
    job.progress.store(progress_base + progress_span, Ordering::Relaxed);
    Ok(())
}

/// Same-directory scratch path for `out_path`'s encode, suffixed with
/// `job_id` so two concurrently-running jobs never collide on the same
/// temp file.
fn temp_out_path(out_path: &Path, job_id: JobId) -> PathBuf {
    let file_name = out_path.file_name().and_then(|f| f.to_str()).unwrap_or("output");
    out_path.with_file_name(format!(".{file_name}.{job_id}.part"))
}

/// Deletes its file on drop unless [`disarm`](Self::disarm) was called
/// first. `transcode` disarms it only after `sink.finish()` and the
/// rename into the real output path both succeed, so every early return
/// (cancellation, a decode error, an encode error) deletes the scratch
/// file instead of leaving a partial result on disk.
struct TempFileGuard {
    path: PathBuf,
    keep: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }

    fn disarm(&mut self) {
        self.keep = true;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Copies title/artist/track/disc plus the shared album/genre/date/cover
/// art fields (see [`shared_tags`]) from `src` to `dst`, best-effort — a
/// missing source tag or an unwritable field is skipped, never a hard
/// error, since the encoded output is already valid without it.
fn copy_tags(src: &Path, dst: &Path, format: encoder::OutputFormat) {
    let Some(src_tags) = track_tags::probe(src, true).map(|p| p.tags) else {
        return;
    };

    let write = WriteTags {
        title: src_tags.title.as_deref(),
        artist: src_tags.artist.as_deref(),
        track_number: src_tags.track_number,
        disc_number: src_tags.disc_number,
        ..shared_tags(&src_tags)
    };
    write_output_tags(dst, format, &write);
}

/// Builds the release-level fields both `copy_tags` (whole-file convert)
/// and `cue_split` (per-track rip) need from the same source tag — album,
/// genre, release date, and front-cover artwork — but neither title,
/// artist, nor track number, since those differ per output (CUE tracks get
/// their own title/artist/number from the cue sheet, not the source tag).
fn shared_tags(src_tags: &track_tags::AudioTags) -> WriteTags<'_> {
    WriteTags {
        album: src_tags.album.as_deref(),
        genre: src_tags.genre.as_deref(),
        date: src_tags.date.as_deref(),
        picture: front_cover(&src_tags.pictures),
        ..Default::default()
    }
}

/// Returns `pictures`'s front-cover picture, falling back to the first
/// embedded picture if none is explicitly typed as the front cover — most
/// rips only embed a single (untyped-as-front) picture, and that's still
/// the one users expect to see as artwork.
fn front_cover(pictures: &[Picture]) -> Option<&Picture> {
    pictures.iter().find(|p| p.is_front_cover).or_else(|| pictures.first())
}

/// Dispatches to the right tag writer for `format`'s output container:
/// `super::tag_writer` (FLAC/WAV, this crate's own writers), a no-op for
/// AIFF (no tag chunk writer exists yet), or `super::ffmpeg` (the five
/// `ffmpeg`-backed formats, tagged via a `-metadata` remux pass since none
/// of them are containers this crate writes natively).
fn write_output_tags(path: &Path, format: encoder::OutputFormat, tags: &WriteTags<'_>) {
    match format {
        encoder::OutputFormat::Flac => tag_writer::write_flac_tags(path, tags),
        encoder::OutputFormat::Wav16 | encoder::OutputFormat::Wav24 | encoder::OutputFormat::Wav32Float => {
            tag_writer::write_wav_tags(path, tags)
        }
        // No native AIFF tag chunk is written (yet): AIFF has a standard
        // `ID3 `/`NAME`/`AUTH`/`(c) ` chunk convention, but round-tripping
        // it isn't implemented — matches the "best-effort, never a hard
        // error" contract by simply skipping rather than half-implementing
        // a reader-incompatible tag.
        encoder::OutputFormat::Aiff16 | encoder::OutputFormat::Aiff24 => {}
        encoder::OutputFormat::Mp3
        | encoder::OutputFormat::Aac
        | encoder::OutputFormat::Opus
        | encoder::OutputFormat::OggVorbis
        | encoder::OutputFormat::Alac => super::ffmpeg::write_tags(path, tags),
    }
}

/// Strips characters that are awkward, invalid, or path-traversing in
/// filenames (path separators, NUL, other control characters); strips a
/// leading `-` that could be misread as a flag by anything that later
/// shells out on the name; trims surrounding `.`/whitespace so a name
/// that's entirely dots can't resolve to `.`/`..` as a path component; and
/// caps the result's byte length so a pathological tag can't exceed
/// common filesystem name limits.
fn sanitize_filename(name: &str) -> String {
    const MAX_BYTES: usize = 150;

    let mut cleaned = String::new();
    for c in name.trim().chars() {
        if c == '\0' || c.is_control() {
            continue;
        }
        let c = if "/\\:*?\"<>|".contains(c) { '_' } else { c };
        if cleaned.len() + c.len_utf8() > MAX_BYTES {
            break;
        }
        cleaned.push(c);
    }

    while cleaned.starts_with('-') {
        cleaned.remove(0);
    }
    let cleaned = cleaned.trim_matches('.').trim();

    if cleaned.is_empty() { "track".to_owned() } else { cleaned.to_owned() }
}

/// Builds `dir/stem.ext`, appending ` (N)` before the extension if that
/// path already exists, so repeated conversions never clobber each other.
fn unique_out_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let candidate = dir.join(format!("{stem}.{ext}"));
    if !candidate.exists() {
        return candidate;
    }
    for n in 1..1000 {
        let candidate = dir.join(format!("{stem} ({n}).{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}.{ext}"))
}

/// Wraps a `File` so decode progress can be tracked by bytes consumed, for
/// formats/sources where symphonia can't report `num_frames` up front.
struct CountingFile {
    inner: File,
    byte_len: Option<u64>,
    read_bytes: Arc<AtomicU64>,
}

impl Read for CountingFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read_bytes.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl Seek for CountingFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl MediaSource for CountingFile {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

/// A single open, probed audio track ready for streaming PCM reads.
struct AudioSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    /// Bits per decoded sample, if the codec reports one (only consulted
    /// for picking a FLAC output bit depth).
    bits_per_sample: Option<u32>,
    /// Total frames if known from container metadata; falls back to
    /// `file_len`/`read_bytes` for progress when unknown.
    total_frames: Option<u64>,
    file_len: Option<u64>,
    read_bytes: Arc<AtomicU64>,
    sample_buf: Vec<f32>,
    sample_pos: usize,
}

impl AudioSource {
    fn open(path: &Path) -> Result<Self, ConvertError> {
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let file = File::open(path)?;
        let file_len = file.metadata().ok().map(|m| m.len());
        let read_bytes = Arc::new(AtomicU64::new(0));
        let counting = CountingFile {
            inner: file,
            byte_len: file_len,
            read_bytes: Arc::clone(&read_bytes),
        };
        let mss = MediaSourceStream::new(Box::new(counting), Default::default());

        let reader = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .map_err(|e| ConvertError::Decode(format!("probe failed: {e}")))?;

        let track = reader.default_track(TrackType::Audio).ok_or(ConvertError::NoAudioTrack)?;
        let track_id = track.id;

        let audio = match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(audio)) => audio.clone(),
            _ => return Err(ConvertError::NoAudioTrack),
        };
        let sample_rate = audio
            .sample_rate
            .ok_or_else(|| ConvertError::Decode("source has no sample rate".to_owned()))?;
        let channels = audio.channels.as_ref().map_or(2, |c| c.count() as u16);
        let bits_per_sample = audio.bits_per_sample;
        let total_frames = track.num_frames;

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&audio, &AudioDecoderOptions::default())
            .map_err(|e| ConvertError::Decode(format!("no decoder available: {e}")))?;

        Ok(Self {
            reader,
            decoder,
            track_id,
            sample_rate,
            channels,
            bits_per_sample,
            total_frames,
            file_len,
            read_bytes,
            sample_buf: Vec::new(),
            sample_pos: 0,
        })
    }

    /// Seeks near `secs` seconds from the start (used for CUE track
    /// boundaries).
    fn seek(&mut self, secs: f64) -> Result<(), ConvertError> {
        let time = Time::try_from_secs_f64(secs.max(0.0))
            .ok_or_else(|| ConvertError::Decode("invalid seek position".to_owned()))?;
        self.reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Time { time, track_id: Some(self.track_id) },
            )
            .map_err(|e| ConvertError::Decode(format!("seek failed: {e}")))?;
        self.decoder.reset();
        self.sample_buf.clear();
        self.sample_pos = 0;
        Ok(())
    }

    /// Reads decoded, interleaved `f32` PCM into `buffer`, returning how
    /// many samples were written (0 only at end-of-stream).
    fn read(&mut self, buffer: &mut [f32]) -> Result<usize, ConvertError> {
        let mut written = 0;

        while written < buffer.len() {
            if self.sample_pos < self.sample_buf.len() {
                let available = self.sample_buf.len() - self.sample_pos;
                let to_copy = (buffer.len() - written).min(available);
                buffer[written..written + to_copy]
                    .copy_from_slice(&self.sample_buf[self.sample_pos..self.sample_pos + to_copy]);
                written += to_copy;
                self.sample_pos += to_copy;
                if written >= buffer.len() {
                    break;
                }
            }

            let packet = match self.reader.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(ConvertError::Decode(format!("failed to read packet: {e}"))),
            };

            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => return Err(ConvertError::Decode(format!("failed to decode packet: {e}"))),
            };

            if decoded.frames() == 0 {
                continue;
            }

            decoded.copy_to_vec_interleaved(&mut self.sample_buf);
            self.sample_pos = 0;
        }

        Ok(written)
    }

    /// Progress in permille (0-1000), from decoded source frames when the
    /// total is known, else from bytes consumed out of the source file.
    fn progress_permille(&self, frames_done: u64) -> u32 {
        if let Some(total) = self.total_frames.filter(|&t| t > 0) {
            ((frames_done.min(total) * 1000) / total) as u32
        } else if let Some(total) = self.file_len.filter(|&t| t > 0) {
            let done = self.read_bytes.load(Ordering::Relaxed);
            ((done.min(total) * 1000) / total) as u32
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    /// Writes a mono 44.1kHz sine WAV of `num_frames` samples (no tags —
    /// used only by tests that don't care about tag round-tripping).
    fn write_test_wav_frames(path: &Path, num_frames: u32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..num_frames {
            let s = (TAU * 440.0 * i as f32 / 44_100.0).sin();
            writer.write_sample((s * f32::from(i16::MAX)) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    /// One second of `write_test_wav_frames`, used by tests that don't care
    /// about the exact clip length or about tags.
    fn write_test_wav(path: &Path) {
        write_test_wav_frames(path, 44_100);
    }

    /// Writes a mono 44.1kHz sine FLAC of `num_frames` samples, untagged —
    /// used as a fixture for tests that then tag it themselves via
    /// `tag_writer::write_flac_tags`, exercising the same read-then-write
    /// path a real source file goes through (`crate::library::tags::probe`
    /// for reading, since FLAC — unlike this fork's WAV reader, see
    /// `tag_writer`'s module docs — round-trips tags end to end).
    fn write_test_flac_frames(path: &Path, num_frames: u32) {
        let mut sink = encoder::create_sink(
            encoder::OutputFormat::Flac,
            path,
            1,
            44_100,
            Some(16),
            encoder::FlacOptions::default(),
            encoder::LossyOptions::default(),
        )
        .unwrap();
        let samples: Vec<f32> =
            (0..num_frames).map(|i| (TAU * 440.0 * i as f32 / 44_100.0).sin()).collect();
        sink.write(&samples).unwrap();
        sink.finish().unwrap();
    }

    /// One second of `write_test_flac_frames`.
    fn write_test_flac(path: &Path) {
        write_test_flac_frames(path, 44_100);
    }

    #[test]
    fn run_converts_and_copies_tags_end_to_end() {
        let dir = std::env::temp_dir().join(format!("lyra-pipeline-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.flac");
        write_test_flac(&source);
        tag_writer::write_flac_tags(
            &source,
            &WriteTags { title: Some("Pipeline Test Track"), ..Default::default() },
        );

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings {
            format: encoder::OutputFormat::Flac,
            target_rate: None,
            out_dir: out_dir.clone(),
            flac_options: encoder::FlacOptions::default(),
            lossy_options: encoder::LossyOptions::default(),
        });

        run(&job).expect("conversion job should succeed");

        let out_path = out_dir.join("source.flac");
        assert!(out_path.exists(), "expected {out_path:?} to exist");

        let probed = track_tags::probe(&out_path, false).expect("probe should succeed");
        assert!(
            (probed.properties.duration.as_secs_f64() - 1.0).abs() < 0.05,
            "expected ~1s, got {:?}",
            probed.properties.duration
        );
        assert_eq!(probed.tags.title.as_deref(), Some("Pipeline Test Track"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_copies_date_and_cover_art_end_to_end() {
        let dir = std::env::temp_dir().join(format!("lyra-pipeline-art-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.flac");
        write_test_flac(&source);

        // Tag the source with exactly the fields `shared_tags` is
        // responsible for carrying over: release date and a front-cover
        // picture (title/artist/track number are deliberately left unset,
        // since those come from elsewhere for CUE tracks and shouldn't be
        // conflated with the shared fields this test exercises).
        let cover_bytes = vec![0xFFu8, 0xD8, 0xFF, 0xD9]; // minimal fake JPEG payload
        let picture =
            Picture { mime_type: "image/jpeg".to_string(), is_front_cover: true, data: cover_bytes.clone() };
        tag_writer::write_flac_tags(
            &source,
            &WriteTags { date: Some("2024"), picture: Some(&picture), ..Default::default() },
        );

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings {
            format: encoder::OutputFormat::Flac,
            target_rate: None,
            out_dir: out_dir.clone(),
            flac_options: encoder::FlacOptions::default(),
            lossy_options: encoder::LossyOptions::default(),
        });
        run(&job).expect("conversion job should succeed");

        let probed = track_tags::probe(&out_dir.join("source.flac"), true).expect("probe should succeed");
        assert_eq!(probed.tags.date.as_deref(), Some("2024"), "release date should carry over");
        assert_eq!(probed.tags.pictures.len(), 1, "expected exactly one carried-over picture");
        assert_eq!(probed.tags.pictures[0].data, cover_bytes, "cover art bytes should be preserved");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn track_progress_range_allocates_equal_monotonic_spans() {
        for total in [1usize, 2, 3, 7] {
            let mut prev_end = 0u32;
            for index in 0..total {
                let (base, span) = track_progress_range(index, total);
                assert_eq!(base, prev_end, "track {index}/{total} should start where the previous one ended");
                assert!(span > 0, "track {index}/{total} got a zero-width progress span");
                prev_end = base + span;
            }
            assert_eq!(prev_end, 1000, "spans for {total} tracks should sum to exactly 1000");
        }
    }

    #[test]
    fn sanitize_filename_blocks_traversal_and_dashes_and_caps_length() {
        assert_eq!(sanitize_filename(".."), "track");
        assert_eq!(sanitize_filename("..."), "track");
        assert!(!sanitize_filename("../../.bashrc").contains('/'));
        assert!(!sanitize_filename("-rf --no-preserve-root").starts_with('-'));
        assert_eq!(sanitize_filename("a\0b").find('\0'), None);

        let long = "x".repeat(1000);
        assert!(sanitize_filename(&long).len() <= 150);
    }

    #[test]
    fn cancelled_conversion_leaves_no_output_file() {
        let dir = std::env::temp_dir().join(format!("lyra-pipeline-cancel-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.wav");
        write_test_wav(&source);

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings {
            format: encoder::OutputFormat::Wav16,
            target_rate: None,
            out_dir: out_dir.clone(),
            flac_options: encoder::FlacOptions::default(),
            lossy_options: encoder::LossyOptions::default(),
        });
        job.cancel();
        let result = run(&job);
        assert!(matches!(result, Err(ConvertError::Cancelled)), "expected Cancelled, got {result:?}");

        let entries: Vec<_> = std::fs::read_dir(&out_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        assert!(entries.is_empty(), "expected no leftover files in {out_dir:?}, found {entries:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_with_resample_does_not_drop_a_short_clip() {
        let dir = std::env::temp_dir().join(format!("lyra-pipeline-resample-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.wav");
        // Shorter than the resampler's internal processing chunk (1024
        // source-rate frames), so without draining its buffered tail via
        // `flush()` the whole clip would come out empty.
        write_test_wav_frames(&source, 441);

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings {
            format: encoder::OutputFormat::Flac,
            target_rate: Some(48_000),
            out_dir: out_dir.clone(),
            flac_options: encoder::FlacOptions::default(),
            lossy_options: encoder::LossyOptions::default(),
        });
        run(&job).expect("resampled conversion job should succeed");

        let out_path = out_dir.join("source.flac");
        let probed = track_tags::probe(&out_path, false).expect("probe should succeed");
        assert!(
            probed.properties.duration.as_secs_f64() > 0.0,
            "resampled short clip should not be flushed away entirely, got {:?}",
            probed.properties.duration
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// End-to-end smoke test of the `ffmpeg` fallback path through the
    /// real `pipeline::run`, not just `ffmpeg::spawn_encoder`'s argument
    /// building — skips (rather than fails) when `ffmpeg` isn't on
    /// `$PATH`, since CI/dev environments aren't guaranteed to have it.
    #[test]
    fn run_encodes_mp3_via_ffmpeg_when_available() {
        if !crate::convert::ffmpeg::detect() {
            eprintln!("skipping: ffmpeg not found on $PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("lyra-pipeline-mp3-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.wav");
        write_test_wav(&source);
        tag_writer::write_wav_tags(&source, &WriteTags { title: Some("MP3 Smoke Test"), ..Default::default() });

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings {
            format: encoder::OutputFormat::Mp3,
            target_rate: None,
            out_dir: out_dir.clone(),
            flac_options: encoder::FlacOptions::default(),
            lossy_options: encoder::LossyOptions::default(),
        });

        run(&job).expect("mp3 conversion job should succeed");

        let out_path = out_dir.join("source.mp3");
        assert!(out_path.exists(), "expected {out_path:?} to exist");

        let probed = track_tags::probe(&out_path, false).expect("probe should succeed decoding the mp3");
        assert!(
            probed.properties.duration.as_secs_f64() > 0.5,
            "expected ~1s of decoded mp3 audio, got {:?}",
            probed.properties.duration
        );
        assert_eq!(
            probed.tags.title.as_deref(),
            Some("MP3 Smoke Test"),
            "title tag should have survived the ffmpeg -metadata remux"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
