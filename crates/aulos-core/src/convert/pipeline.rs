// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Standalone decode → resample → encode pipeline for local file
//! conversion, transcoding, and CUE-sheet splitting.
//!
//! Decoding uses the same probe/track-selection pattern as
//! [`crate::player::engine::decoder`], but against a plain `File` /
//! `MediaSourceStream` — no DSD, no HTTP streaming, since this pipeline
//! never touches playback. Because symphonia's probe is content-based, this
//! transparently "rips" the first audio track out of video containers
//! (mp4/mkv) too, with no container-specific special-casing.
//!
//! # Pausing
//!
//! Every packet-sized step of the decode → mix → resample → encode loop
//! first passes [`checkpoint`], which blocks while the job's shared `pause`
//! flag is set (and still reacts to cancellation). Pausing therefore needs
//! no cooperation from the encoder: the pure-Rust sinks simply stop being
//! fed, and the `ffmpeg` child stops receiving PCM on its stdin, blocks on
//! the empty pipe and idles at zero CPU until the feed resumes — no
//! `SIGSTOP`, which would be Unix-only and could leave the child stopped if
//! Aulos died while paused.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatReader, SeekMode, SeekTo, SeekedTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;
use symphonia::core::units::{Time, TimeBase};

use crate::library::tagio::{self, TagSet, keys, picture_type};
use crate::player::engine::decoder::{
    MAX_SEEK_RESETS, ResettableSeek, SeekSkip, chain_duration_secs, local_file_format_options,
    seek_with_resets,
};
use crate::player::engine::resampler::{ResamplerQuality, StreamResampler};

use super::channels::Mixer;
use super::cue;
use super::encoder;
use super::naming::{self, NameFields};
use super::source::cue_audio_path;
use super::{ConvertError, ConvertJob, JobId, JobKind, JobSettings};

/// How often a paused job re-checks its pause/cancel flags.
const PAUSE_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// The settings `job` is running with. Panics if called before
/// [`super::ConvertJob::start`] set them — every job passed to `run` here
/// has already transitioned `Queued` -> `Running`, which always sets them.
fn settings(job: &ConvertJob) -> &JobSettings {
    job.settings
        .as_ref()
        .expect("pipeline::run called on a job without settings (not yet started)")
}

/// Runs `job` to completion: decodes, optionally mixes channels and
/// resamples, encodes, and tags the output(s). Returns the total size in
/// bytes of the files it produced. Checks `job.cancel` between packets.
/// Encoding happens on a scratch file next to the real output; on
/// cancellation, a decode error, or an encode error the scratch file is
/// deleted and the real output path is never touched, so nothing partial is
/// ever left behind at the name the caller asked for.
pub fn run(job: &ConvertJob) -> Result<u64, ConvertError> {
    std::fs::create_dir_all(&settings(job).out_dir)?;
    match job.kind {
        JobKind::Convert => convert_file(job, &job.source),
        JobKind::CueSplit if settings(job).split_cue => cue_split(job),
        JobKind::CueSplit => {
            // Splitting is off: treat the sheet as a pointer to the one
            // audio file it describes and convert that whole.
            let cue_text = std::fs::read_to_string(&job.source)?;
            let audio_path = cue_audio_path(&job.source, &cue_text)
                .ok_or_else(|| ConvertError::Cue("no FILE line in CUE sheet".to_owned()))?;
            convert_file(job, &audio_path)
        }
    }
}

/// Converts the whole of `source` to one output named by the job's file name
/// pattern; returns its size.
fn convert_file(job: &ConvertJob, source: &Path) -> Result<u64, ConvertError> {
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("track");
    let mut tags = read_source_tags(source);
    let fields = NameFields::from_tags(tags.as_ref(), stem);
    let out_path = output_path(settings(job), &fields)?;
    transcode(job, source, &out_path, None, None, 0, 1000)?;
    if settings(job).copy_tags
        && let Some(tags) = tags.as_mut()
    {
        keep_front_cover(tags);
        write_output_tags(&out_path, tags);
    }
    Ok(file_len(&out_path))
}

/// Size of `path` in bytes; 0 if it can't be read.
fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

/// The not-yet-existing output path for a file with `fields`: the job's
/// pattern expanded under the output folder (its folders created), made
/// unique, with the format's extension.
fn output_path(settings: &JobSettings, fields: &NameFields) -> Result<PathBuf, ConvertError> {
    let mut segments = naming::expand(&settings.name_pattern, fields);
    let stem = segments
        .pop()
        .expect("naming::expand always returns at least one segment");
    let dir = segments
        .iter()
        .fold(settings.out_dir.clone(), |dir, segment| dir.join(segment));
    std::fs::create_dir_all(&dir)?;
    Ok(unique_out_path(&dir, &stem, settings.format.extension()))
}

/// Splits the audio file referenced by a `.cue` sheet (`job.source`) into
/// one tagged output file per track. Each track's slice of the overall
/// `job.progress` permille range is fixed up front by
/// [`track_progress_range`] (equal-weight per track), so progress climbs
/// monotonically across the whole rip instead of resetting to ~0 at every
/// track boundary — `transcode` alone can't know it's one of several calls
/// sharing a single job's progress bar. Returns the total output size.
fn cue_split(job: &ConvertJob) -> Result<u64, ConvertError> {
    let cue_text = std::fs::read_to_string(&job.source)?;
    let tracks = cue::parse(&cue_text).map_err(|e| ConvertError::Cue(e.to_string()))?;
    let audio_path = cue_audio_path(&job.source, &cue_text)
        .ok_or_else(|| ConvertError::Cue("no FILE line in CUE sheet".to_owned()))?;
    let audio_stem = audio_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("track")
        .to_owned();

    // Read the whole album's tag once so every track can carry over the
    // release-level fields the CUE sheet itself doesn't encode (album,
    // genre, date, MBIDs, cover art), rather than each track ending up
    // untagged beyond its title.
    let album_tags = album_tags(&audio_path);

    let track_count = tracks.len();
    job.cue_total.store(
        u32::try_from(track_count).unwrap_or(u32::MAX),
        Ordering::Relaxed,
    );
    let mut total_bytes = 0;
    for (i, track) in tracks.iter().enumerate() {
        checkpoint(job)?;
        job.cue_track
            .store(u32::try_from(i + 1).unwrap_or(u32::MAX), Ordering::Relaxed);

        // The track's own fields on top of the release-level ones: they
        // name the file, and are what gets written with it.
        let cue_tags = cue_track_tags(track, track_count);
        let mut named = album_tags.clone().unwrap_or_default();
        for (key, values) in cue_tags.iter() {
            named.set(key, values.to_vec());
        }
        let mut fields = NameFields::from_tags(Some(&named), &audio_stem);
        if fields.title.is_empty() {
            fields.title = format!("Track {:02}", track.number);
        }
        let out_path = output_path(settings(job), &fields)?;

        let start = track.start.as_secs_f64();
        let end = track.end.map(|d| d.as_secs_f64());
        let (progress_base, progress_span) = track_progress_range(i, track_count);
        transcode(
            job,
            &audio_path,
            &out_path,
            Some(start),
            end,
            progress_base,
            progress_span,
        )?;

        write_output_tags(
            &out_path,
            if settings(job).copy_tags {
                &named
            } else {
                &cue_tags
            },
        );
        total_bytes += file_len(&out_path);
    }
    Ok(total_bytes)
}

/// The tags a CUE sheet itself provides for `track`.
fn cue_track_tags(track: &cue::CueTrack, track_count: usize) -> TagSet {
    let mut tags = TagSet::new();
    tags.set_one(keys::TITLE, track.title.as_str());
    tags.set_one(keys::ARTIST, track.performer.as_str());
    tags.set_one(keys::TRACKNUMBER, track.number.to_string());
    tags.set_one(keys::TOTALTRACKS, track_count.to_string());
    tags
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
    let src_channels = source.channels;
    let channels = settings(job).channels.output_channels(src_channels);
    let mixer = Mixer::new(src_channels, channels);
    let mut mixed = Vec::new();
    let dst_rate = settings(job).target_rate.unwrap_or(src_rate);
    let mut resampler = (dst_rate != src_rate)
        .then(|| {
            StreamResampler::new(
                src_rate,
                dst_rate,
                channels as usize,
                ResamplerQuality::SincMedium,
            )
        })
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
    let max_frames =
        end.map(|e| ((e - start.unwrap_or(0.0)).max(0.0) * f64::from(src_rate)).round() as u64);

    const CHUNK_FRAMES: usize = 8192;
    let mut buf = vec![0f32; CHUNK_FRAMES * src_channels.max(1) as usize];
    let mut frames_done: u64 = 0;

    loop {
        checkpoint(job)?;

        let mut want = buf.len();
        if let Some(max) = max_frames {
            let remaining = max.saturating_sub(frames_done) as usize * src_channels.max(1) as usize;
            if remaining == 0 {
                break;
            }
            want = want.min(remaining);
        }

        let n = source.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        frames_done += (n / src_channels.max(1) as usize) as u64;

        let chunk: &[f32] = match &mixer {
            Some(mixer) => {
                mixer.process(&buf[..n], &mut mixed);
                &mixed
            }
            None => &buf[..n],
        };
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
    job.progress
        .store(progress_base + progress_span, Ordering::Relaxed);
    Ok(())
}

/// Blocks while the job is paused; fails with [`ConvertError::Cancelled`]
/// as soon as cancellation is requested (also while paused).
fn checkpoint(job: &ConvertJob) -> Result<(), ConvertError> {
    loop {
        if job.cancel.load(Ordering::Relaxed) {
            return Err(ConvertError::Cancelled);
        }
        if !job.pause.load(Ordering::Relaxed) {
            return Ok(());
        }
        std::thread::sleep(PAUSE_POLL);
    }
}

/// Same-directory scratch path for `out_path`'s encode, suffixed with
/// `job_id` so two concurrently-running jobs never collide on the same
/// temp file.
fn temp_out_path(out_path: &Path, job_id: JobId) -> PathBuf {
    let file_name = out_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("output");
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

/// The source file's full tag set (every field `tagio` reads, so MBIDs,
/// ReplayGain, credits, … survive a copy), best-effort: an unreadable source
/// is logged and yields `None`, never a hard error, since the encoded output
/// is valid without tags.
fn read_source_tags(src: &Path) -> Option<TagSet> {
    match tagio::read(src) {
        Ok(file) => Some(file.tags),
        Err(e) => {
            tracing::warn!("couldn't read tags from {}: {e}", src.display());
            None
        }
    }
}

/// Reduces `tags.pictures` to the front cover, falling back to the first
/// embedded picture if none is explicitly typed as the front cover — most
/// rips only embed a single (untyped-as-front) picture, and that's still
/// the one users expect to see as artwork.
fn keep_front_cover(tags: &mut TagSet) {
    let idx = tags
        .pictures
        .iter()
        .position(|p| p.kind == picture_type::FRONT_COVER)
        .unwrap_or(0);
    if idx < tags.pictures.len() {
        let cover = tags.pictures.swap_remove(idx);
        tags.pictures = vec![cover];
    }
}

/// Keys describing one specific recording/track rather than the release;
/// dropped when an album-wide source tag is split into CUE tracks, which
/// get their own title/artist/number from the cue sheet.
const TRACK_LEVEL_KEYS: &[&str] = &[
    keys::TITLE,
    keys::ARTIST,
    keys::ARTISTS,
    keys::TRACKNUMBER,
    keys::TOTALTRACKS,
    keys::DISCNUMBER,
    keys::TITLESORT,
    keys::ARTISTSORT,
    keys::LYRICS,
    keys::ISRC,
    keys::BPM,
    keys::MUSICBRAINZ_RECORDINGID,
    keys::MUSICBRAINZ_TRACKID,
    keys::ACOUSTID_ID,
    keys::ACOUSTID_FINGERPRINT,
    keys::REPLAYGAIN_TRACK_GAIN,
    keys::REPLAYGAIN_TRACK_PEAK,
];

/// Release-level tags of an album-wide source, for CUE-split tracks.
fn album_tags(src: &Path) -> Option<TagSet> {
    let mut tags = read_source_tags(src)?;
    for key in TRACK_LEVEL_KEYS {
        tags.remove(key);
    }
    keep_front_cover(&mut tags);
    Some(tags)
}

/// Writes `tags` into the finished output via `tagio`, best-effort: a
/// failure (including a container `tagio` can't write yet) is logged, never
/// fails the job.
fn write_output_tags(path: &Path, tags: &TagSet) {
    if tags.is_empty() {
        return;
    }
    if let Err(e) = tagio::write(path, tags) {
        tracing::warn!("failed to write tags to {}: {e}", path.display());
    }
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
    time_base: Option<TimeBase>,
    /// Pending sample-accurate seek (frames before the target are discarded).
    seek_skip: Option<SeekSkip>,
}

impl ResettableSeek for AudioSource {
    fn try_seek(&mut self, time: Time) -> Result<SeekedTo, SymphoniaError> {
        // The track id is read on every attempt: it changes when a reset switched links.
        self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        )
    }

    fn rebuild_after_reset(&mut self) -> Result<(), SymphoniaError> {
        self.reinit_after_reset()
    }
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
            .probe(
                &hint,
                mss,
                // A local WavPack `.wv` picks up a sibling `.wvc` (hybrid lossless).
                local_file_format_options(path),
                MetadataOptions::default(),
            )
            .map_err(|e| ConvertError::Decode(format!("probe failed: {e}")))?;

        let track = reader
            .default_track(TrackType::Audio)
            .ok_or(ConvertError::NoAudioTrack)?;
        let track_id = track.id;

        let audio = match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(audio)) => audio.clone(),
            _ => return Err(ConvertError::NoAudioTrack),
        };
        let sample_rate = audio
            .sample_rate
            .ok_or_else(|| ConvertError::Decode("source has no sample rate".to_owned()))?;
        let channels = audio.channels.as_ref().map_or(2, |c| c.count() as u16);
        let bits_per_sample = crate::library::tech_info::stream_bit_depth(&audio);
        let total_frames = track.num_frames;
        let time_base = track.time_base;

        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&audio, &AudioDecoderOptions::default())
            .map_err(|e| ConvertError::Decode(format!("no decoder available: {e}")))?;

        // The decoder may report a different output rate than the container (explicitly
        // signalled HE-AAC decodes at twice the AAC core rate).
        let sample_rate = decoder.codec_params().sample_rate.unwrap_or(sample_rate);

        // A chained Ogg stream reports the whole chain's duration at the media level.
        let total_frames = chain_duration_secs(reader.format_info().format, reader.media_info())
            .map(|secs| (secs * f64::from(sample_rate)).round() as u64)
            .or(total_frames);

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
            time_base,
            seek_skip: None,
        })
    }

    /// Seeks near `secs` seconds from the start (used for CUE track
    /// boundaries).
    fn seek(&mut self, secs: f64) -> Result<(), ConvertError> {
        let time = Time::try_from_secs_f64(secs.max(0.0))
            .ok_or_else(|| ConvertError::Decode("invalid seek position".to_owned()))?;
        self.seek_skip = None;
        let seeked = seek_with_resets(self, time, MAX_SEEK_RESETS)
            .map_err(|e| ConvertError::Decode(format!("seek failed: {e}")))?;
        self.decoder.reset();
        self.sample_buf.clear();
        self.sample_pos = 0;
        // The demuxer lands at or before the target (long pre-roll for Opus/MKV): the frames
        // before the requested timestamp are discarded by `read`.
        self.seek_skip = SeekSkip::new(
            seeked.required_ts,
            seeked.actual_ts,
            self.time_base,
            self.sample_rate,
        );
        Ok(())
    }

    /// Number of leading frames of the just-decoded `packet` to discard to honour a pending
    /// sample-accurate seek.
    fn seek_skip_frames(&mut self, packet: &Packet, frames: usize, rate: u32) -> usize {
        let Some(tb) = self.time_base else {
            self.seek_skip = None;
            return 0;
        };
        let Some(skip) = self.seek_skip.as_mut() else {
            return 0;
        };
        let valid_start = packet.pts.saturating_add(packet.trim_start);
        let (n, done) = skip.advance(valid_start, frames, tb, rate);
        if done {
            self.seek_skip = None;
        }
        n
    }

    /// Re-read the default audio track and re-create the decoder after the demuxer returned
    /// `ResetRequired` (a new link of a chained Ogg stream has a new track id). The output
    /// format negotiated for the first link is kept.
    fn reinit_after_reset(&mut self) -> Result<(), SymphoniaError> {
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
        self.decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&audio, &AudioDecoderOptions::default())?;
        self.track_id = track_id;
        self.time_base = time_base;
        self.sample_buf.clear();
        self.sample_pos = 0;
        self.seek_skip = None;
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
                    self.reinit_after_reset().map_err(|e| {
                        ConvertError::Decode(format!("failed to reinitialise after reset: {e}"))
                    })?;
                    continue;
                }
                Err(SymphoniaError::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    break;
                }
                Err(e) => return Err(ConvertError::Decode(format!("failed to read packet: {e}"))),
            };

            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(decoded) => decoded,
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(e) => {
                    return Err(ConvertError::Decode(format!(
                        "failed to decode packet: {e}"
                    )));
                }
            };

            if decoded.frames() == 0 {
                continue;
            }

            let frames = decoded.frames();
            let rate = decoded.spec().rate();
            let channel_count = decoded.spec().channels().count();
            decoded.copy_to_vec_interleaved(&mut self.sample_buf);
            // After a seek, drop the frames in front of the requested position.
            let skip_frames = self.seek_skip_frames(&packet, frames, rate);
            self.sample_pos = (skip_frames * channel_count).min(self.sample_buf.len());
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
    /// ALAC states its depth only in the codec's magic cookie; FLAC "Auto"
    /// needs it to keep a 16-bit source at 16 bits instead of padding to 24.
    #[test]
    fn alac_sources_report_their_bit_depth() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/probe");
        for (name, depth) in [("alac16.m4a", 16), ("alac24.m4a", 24)] {
            let source = AudioSource::open(&dir.join(name)).expect(name);
            assert_eq!(source.bits_per_sample, Some(depth), "{name}");
        }
    }

    use super::*;
    use crate::library::tagio::EditPicture;
    use crate::library::tags as track_tags;
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
            writer
                .write_sample((s * f32::from(i16::MAX)) as i16)
                .unwrap();
        }
        writer.finalize().unwrap();
    }

    /// One second of `write_test_wav_frames`, used by tests that don't care
    /// about the exact clip length or about tags.
    fn write_test_wav(path: &Path) {
        write_test_wav_frames(path, 44_100);
    }

    /// Tags `path` through `tagio`, as a real tagged source file would be.
    fn tag_source(path: &Path, fields: &[(&str, &str)], picture: Option<EditPicture>) {
        let mut tags = TagSet::new();
        for (k, v) in fields {
            tags.set_one(k, *v);
        }
        tags.pictures.extend(picture);
        tagio::write(path, &tags).expect("tag source fixture");
    }

    /// Writes a mono 44.1kHz sine FLAC of `num_frames` samples, untagged.
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
        let samples: Vec<f32> = (0..num_frames)
            .map(|i| (TAU * 440.0 * i as f32 / 44_100.0).sin())
            .collect();
        sink.write(&samples).unwrap();
        sink.finish().unwrap();
    }

    /// One second of `write_test_flac_frames`.
    fn write_test_flac(path: &Path) {
        write_test_flac_frames(path, 44_100);
    }

    #[test]
    fn run_converts_and_copies_tags_end_to_end() {
        let dir = std::env::temp_dir().join(format!("aulos-pipeline-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.flac");
        write_test_flac(&source);
        tag_source(
            &source,
            &[
                (keys::TITLE, "Pipeline Test Track"),
                (
                    keys::MUSICBRAINZ_ALBUMID,
                    "11111111-2222-3333-4444-555555555555",
                ),
                (keys::REPLAYGAIN_TRACK_GAIN, "-6.50 dB"),
            ],
            None,
        );

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings::for_test(
            encoder::OutputFormat::Flac,
            None,
            out_dir.clone(),
        ));

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
        let out_tags = tagio::read(&out_path).expect("tagio read").tags;
        assert_eq!(
            out_tags.get(keys::MUSICBRAINZ_ALBUMID),
            Some("11111111-2222-3333-4444-555555555555"),
            "MBIDs should survive conversion"
        );
        assert_eq!(out_tags.get(keys::REPLAYGAIN_TRACK_GAIN), Some("-6.50 dB"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_copies_date_and_cover_art_end_to_end() {
        let dir =
            std::env::temp_dir().join(format!("aulos-pipeline-art-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.flac");
        write_test_flac(&source);

        // Tag the source with exactly the fields `shared_tags` is
        // responsible for carrying over: release date and a front-cover
        // picture (title/artist/track number are deliberately left unset,
        // since those come from elsewhere for CUE tracks and shouldn't be
        // conflated with the shared fields this test exercises).
        let cover_bytes = vec![0xFFu8, 0xD8, 0xFF, 0xD9]; // minimal fake JPEG payload
        let picture = EditPicture {
            kind: picture_type::FRONT_COVER,
            mime: "image/jpeg".to_string(),
            description: String::new(),
            data: cover_bytes.clone(),
        };
        tag_source(&source, &[(keys::DATE, "2024")], Some(picture));

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings::for_test(
            encoder::OutputFormat::Flac,
            None,
            out_dir.clone(),
        ));
        run(&job).expect("conversion job should succeed");

        let probed =
            track_tags::probe(&out_dir.join("source.flac"), true).expect("probe should succeed");
        assert_eq!(
            probed.tags.date.as_deref(),
            Some("2024"),
            "release date should carry over"
        );
        assert_eq!(
            probed.tags.pictures.len(),
            1,
            "expected exactly one carried-over picture"
        );
        assert_eq!(
            probed.tags.pictures[0].data, cover_bytes,
            "cover art bytes should be preserved"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn track_progress_range_allocates_equal_monotonic_spans() {
        for total in [1usize, 2, 3, 7] {
            let mut prev_end = 0u32;
            for index in 0..total {
                let (base, span) = track_progress_range(index, total);
                assert_eq!(
                    base, prev_end,
                    "track {index}/{total} should start where the previous one ended"
                );
                assert!(
                    span > 0,
                    "track {index}/{total} got a zero-width progress span"
                );
                prev_end = base + span;
            }
            assert_eq!(
                prev_end, 1000,
                "spans for {total} tracks should sum to exactly 1000"
            );
        }
    }

    #[test]
    fn cancelled_conversion_leaves_no_output_file() {
        let dir =
            std::env::temp_dir().join(format!("aulos-pipeline-cancel-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.wav");
        write_test_wav(&source);

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings::for_test(
            encoder::OutputFormat::Wav16,
            None,
            out_dir.clone(),
        ));
        job.cancel();
        let result = run(&job);
        assert!(
            matches!(result, Err(ConvertError::Cancelled)),
            "expected Cancelled, got {result:?}"
        );

        let entries: Vec<_> = std::fs::read_dir(&out_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        assert!(
            entries.is_empty(),
            "expected no leftover files in {out_dir:?}, found {entries:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_with_resample_does_not_drop_a_short_clip() {
        let dir = std::env::temp_dir().join(format!(
            "aulos-pipeline-resample-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.wav");
        // Shorter than the resampler's internal processing chunk (1024
        // source-rate frames), so without draining its buffered tail via
        // `flush()` the whole clip would come out empty.
        write_test_wav_frames(&source, 441);

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings::for_test(
            encoder::OutputFormat::Flac,
            Some(48_000),
            out_dir.clone(),
        ));
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

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aulos-pipeline-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn run_names_the_output_by_pattern_and_reports_its_size() {
        let dir = temp_dir("pattern");
        let source = dir.join("source.flac");
        write_test_flac(&source);
        tag_source(
            &source,
            &[
                (keys::TITLE, "Salt Glass"),
                (keys::ARTIST, "Mira Okafor"),
                (keys::ALBUM, "Low Tide Tapes"),
                (keys::DATE, "2024-03-01"),
                (keys::TRACKNUMBER, "6"),
            ],
            None,
        );

        let out_dir = dir.join("out");
        let mut settings =
            JobSettings::for_test(encoder::OutputFormat::Flac, None, out_dir.clone());
        settings.name_pattern = "{artist}/{year} - {album}/{track:02} {title}".to_owned();
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(settings);

        let bytes = run(&job).expect("conversion should succeed");

        let out_path = out_dir
            .join("Mira Okafor")
            .join("2024 - Low Tide Tapes")
            .join("06 Salt Glass.flac");
        assert!(out_path.exists(), "expected {out_path:?}");
        assert_eq!(bytes, std::fs::metadata(&out_path).unwrap().len());
        assert!(bytes > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_falls_back_to_the_source_name_without_tags() {
        let dir = temp_dir("untagged");
        let source = dir.join("My Song.wav");
        write_test_wav(&source);
        let out_dir = dir.join("out");
        let mut settings =
            JobSettings::for_test(encoder::OutputFormat::Wav16, None, out_dir.clone());
        settings.name_pattern = naming::DEFAULT_PATTERN.to_owned();
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(settings);
        run(&job).expect("conversion should succeed");
        assert_eq!(file_names(&out_dir), ["My Song.wav"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_without_copy_tags_leaves_the_output_untagged() {
        let dir = temp_dir("no-tags");
        let source = dir.join("source.flac");
        write_test_flac(&source);
        tag_source(&source, &[(keys::TITLE, "Should Not Carry Over")], None);

        let out_dir = dir.join("out");
        let mut settings =
            JobSettings::for_test(encoder::OutputFormat::Flac, None, out_dir.clone());
        settings.copy_tags = false;
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(settings);
        run(&job).expect("conversion should succeed");

        let probed = track_tags::probe(&out_dir.join("source.flac"), false).unwrap();
        assert_eq!(probed.tags.title, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_upmixes_mono_to_stereo_and_downmixes_back() {
        let dir = temp_dir("channels");
        let source = dir.join("source.wav");
        write_test_wav(&source);

        for (mode, expected) in [
            (super::super::ChannelMode::Stereo, 2),
            (super::super::ChannelMode::Mono, 1),
            (super::super::ChannelMode::Source, 1),
        ] {
            let out_dir = dir.join(format!("out-{expected}-{mode:?}"));
            let mut settings =
                JobSettings::for_test(encoder::OutputFormat::Wav16, None, out_dir.clone());
            settings.channels = mode;
            let mut job = ConvertJob::new(1, source.clone(), JobKind::Convert);
            job.start(settings);
            run(&job).expect("conversion should succeed");

            let reader = hound::WavReader::open(out_dir.join("source.wav")).unwrap();
            assert_eq!(reader.spec().channels, expected, "{mode:?}");
            assert_eq!(reader.duration(), 44_100, "{mode:?} keeps the length");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    fn write_cue_fixture(dir: &Path) -> PathBuf {
        write_test_wav(&dir.join("album.wav"));
        let cue_path = dir.join("album.cue");
        std::fs::write(
            &cue_path,
            "PERFORMER \"Mira\"\nFILE \"album.wav\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"One\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"Two\"\n    INDEX 01 00:00:50\n",
        )
        .unwrap();
        cue_path
    }

    #[test]
    fn cue_sheet_is_split_into_pattern_named_tracks() {
        let dir = temp_dir("cue-split");
        let cue_path = write_cue_fixture(&dir);
        let out_dir = dir.join("out");
        let mut settings =
            JobSettings::for_test(encoder::OutputFormat::Wav16, None, out_dir.clone());
        settings.name_pattern = naming::DEFAULT_PATTERN.to_owned();
        let mut job = ConvertJob::new(1, cue_path, JobKind::CueSplit);
        job.start(settings);

        let bytes = run(&job).expect("cue split should succeed");

        assert_eq!(file_names(&out_dir), ["01 One.wav", "02 Two.wav"]);
        let sizes: u64 = file_names(&out_dir)
            .iter()
            .map(|n| std::fs::metadata(out_dir.join(n)).unwrap().len())
            .sum();
        assert_eq!(bytes, sizes);
        assert_eq!(job.cue_total.load(Ordering::Relaxed), 2);
        assert_eq!(job.cue_track.load(Ordering::Relaxed), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cue_sheet_converts_whole_when_splitting_is_off() {
        let dir = temp_dir("cue-whole");
        let cue_path = write_cue_fixture(&dir);
        let out_dir = dir.join("out");
        let mut settings =
            JobSettings::for_test(encoder::OutputFormat::Wav16, None, out_dir.clone());
        settings.split_cue = false;
        let mut job = ConvertJob::new(1, cue_path, JobKind::CueSplit);
        job.start(settings);

        run(&job).expect("whole conversion should succeed");

        assert_eq!(file_names(&out_dir), ["album.wav"]);
        let reader = hound::WavReader::open(out_dir.join("album.wav")).unwrap();
        assert_eq!(reader.duration(), 44_100);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn paused_job_holds_until_resumed() {
        let dir = temp_dir("pause");
        let source = dir.join("source.wav");
        write_test_wav(&source);
        let out_dir = dir.join("out");

        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings::for_test(
            encoder::OutputFormat::Wav16,
            None,
            out_dir.clone(),
        ));
        job.pause.store(true, Ordering::Relaxed);
        let worker_job = job.clone();
        let worker = std::thread::spawn(move || run(&worker_job));

        std::thread::sleep(std::time::Duration::from_millis(350));
        assert!(!worker.is_finished(), "a paused job must not finish");
        assert_eq!(job.progress_permille(), 0);

        job.pause.store(false, Ordering::Relaxed);
        worker.join().unwrap().expect("resumed job should finish");
        assert_eq!(job.progress_permille(), 1000);
        assert_eq!(file_names(&out_dir), ["source.wav"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancelling_a_paused_job_stops_it_without_output() {
        let dir = temp_dir("pause-cancel");
        let source = dir.join("source.wav");
        write_test_wav(&source);
        let out_dir = dir.join("out");

        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings::for_test(
            encoder::OutputFormat::Wav16,
            None,
            out_dir.clone(),
        ));
        job.pause.store(true, Ordering::Relaxed);
        let worker_job = job.clone();
        let worker = std::thread::spawn(move || run(&worker_job));

        std::thread::sleep(std::time::Duration::from_millis(150));
        job.cancel();
        let result = worker.join().unwrap();
        assert!(matches!(result, Err(ConvertError::Cancelled)), "{result:?}");
        assert!(file_names(&out_dir).is_empty());
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

        let dir =
            std::env::temp_dir().join(format!("aulos-pipeline-mp3-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.wav");
        write_test_wav(&source);
        tag_source(&source, &[(keys::TITLE, "MP3 Smoke Test")], None);

        let out_dir = dir.join("out");
        let mut job = ConvertJob::new(1, source, JobKind::Convert);
        job.start(JobSettings::for_test(
            encoder::OutputFormat::Mp3,
            None,
            out_dir.clone(),
        ));

        run(&job).expect("mp3 conversion job should succeed");

        let out_path = out_dir.join("source.mp3");
        assert!(out_path.exists(), "expected {out_path:?} to exist");

        let probed =
            track_tags::probe(&out_path, false).expect("probe should succeed decoding the mp3");
        assert!(
            probed.properties.duration.as_secs_f64() > 0.5,
            "expected ~1s of decoded mp3 audio, got {:?}",
            probed.properties.duration
        );
        assert_eq!(
            probed.tags.title.as_deref(),
            Some("MP3 Smoke Test"),
            "title tag should have been written by tagio"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
