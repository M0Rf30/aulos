// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Fallback encoder for the formats with no usable pure-Rust encoder (see
//! `super::encoder`'s module docs for why): shells out to the system
//! `ffmpeg` binary, piping the same decoded/resampled `f32le` interleaved
//! PCM `pipeline::transcode` feeds every pure-Rust [`super::encoder::SampleSink`]
//! straight to `ffmpeg`'s stdin. This is simpler and more robust than
//! trying to hand `ffmpeg` the original source file directly: it already
//! goes through `pipeline`'s single decode path regardless of container
//! (video files, DSD-adjacent oddities symphonia handles, CUE-sheet time
//! ranges, resampling) — piping keeps that one path in charge of every
//! format instead of forking a second "pass the file straight through"
//! code path that would only work for the subset of jobs with no
//! resampling and no CUE split.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use super::ConvertError;
use super::encoder::{LossyOptions, Mp3Mode, OutputFormat, SampleSink};
use super::tag_writer::WriteTags;

/// Whether `ffmpeg` was found on `$PATH`, detected once (a cheap
/// `ffmpeg -version` probe) and cached for the process's lifetime.
///
/// `OnceLock` rather than `LazyLock`: [`cached`] needs to *peek* whether
/// detection has already run without ever triggering it itself (the UI
/// must never block on a fresh probe mid-render) — `LazyLock` has no
/// stable "is it initialized yet" query that doesn't force it, so the
/// two-API (`get_or_init` here in [`detect`], plain `get` in [`cached`])
/// shape `OnceLock` gives directly is the right tool here, not a
/// leftover `once_cell`-era pattern.
static FFMPEG_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Runs the (cheap, well under a second) availability probe and caches
/// the result. Blocking — callers on the UI thread must run this inside
/// `spawn_blocking`/`cosmic::task::future` (see
/// `crate::app::convert_page::AppModel::detect_ffmpeg_once`), which also
/// only fires it once per process by checking [`cached`] first.
pub fn detect() -> bool {
    *FFMPEG_AVAILABLE.get_or_init(|| {
        Command::new("ffmpeg")
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    })
}

/// The cached result of a previous [`detect`] call, or `None` if it's
/// never been run yet. Used by the UI to gate/label the `ffmpeg`-only
/// formats without blocking on a fresh probe every render.
pub fn cached() -> Option<bool> {
    FFMPEG_AVAILABLE.get().copied()
}

/// Spawns `ffmpeg` to encode `format` (must be one of
/// [`OutputFormat::requires_ffmpeg`]'s formats) at `out_path`, ready to
/// receive interleaved `f32le` PCM on its stdin via [`SampleSink::write`].
pub fn spawn_encoder(
    format: OutputFormat,
    out_path: &Path,
    channels: u16,
    sample_rate: u32,
    lossy: &LossyOptions,
) -> Result<FfmpegSink, ConvertError> {
    if !detect() {
        return Err(ConvertError::Encode(
            "ffmpeg is required for this output format but wasn't found on $PATH".to_owned(),
        ));
    }

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg("f32le")
        .arg("-ar")
        .arg(sample_rate.to_string())
        .arg("-ac")
        .arg(channels.to_string())
        .arg("-i")
        .arg("-");

    match format {
        OutputFormat::Mp3 => {
            cmd.arg("-c:a").arg("libmp3lame");
            match lossy.mp3_mode {
                Mp3Mode::Vbr(q) => {
                    cmd.arg("-q:a").arg(q.to_string());
                }
                Mp3Mode::Cbr(kbps) => {
                    cmd.arg("-b:a").arg(format!("{kbps}k"));
                }
            }
        }
        OutputFormat::Aac => {
            cmd.arg("-c:a").arg("aac").arg("-b:a").arg(format!("{}k", lossy.aac_bitrate_kbps));
        }
        OutputFormat::Opus => {
            cmd.arg("-c:a").arg("libopus").arg("-b:a").arg(format!("{}k", lossy.opus_bitrate_kbps));
        }
        OutputFormat::OggVorbis => {
            cmd.arg("-c:a").arg("libvorbis").arg("-q:a").arg(format!("{}", lossy.vorbis_quality));
        }
        OutputFormat::Alac => {
            cmd.arg("-c:a").arg("alac");
        }
        _ => unreachable!("spawn_encoder is only ever called for requires_ffmpeg formats"),
    }

    // Explicit output muxer: the real target extension (`.mp3`/`.m4a`/...)
    // isn't on the scratch path ffmpeg actually writes to (`pipeline`
    // encodes into a same-directory `.name.ext.<job-id>.part` file, only
    // renamed into place after a successful `finish`), so ffmpeg's usual
    // extension-based muxer autodetection can't run — without this it
    // fails immediately ("Broken pipe" writing PCM to a dead process; a
    // .part-suffixed path has no muxer ffmpeg can guess).
    let muxer = match format {
        OutputFormat::Mp3 => "mp3",
        OutputFormat::Aac | OutputFormat::Alac => "ipod",
        OutputFormat::Opus => "opus",
        OutputFormat::OggVorbis => "ogg",
        _ => unreachable!("spawn_encoder is only ever called for requires_ffmpeg formats"),
    };
    cmd.arg("-f").arg(muxer);
    cmd.arg(out_path);
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());

    let mut child =
        cmd.spawn().map_err(|e| ConvertError::Encode(format!("failed to spawn ffmpeg: {e}")))?;
    let stdin = child.stdin.take();
    let mut stderr = child.stderr.take();

    // Drained on a background thread rather than left piped-but-unread:
    // if ffmpeg ever writes enough to its stderr pipe to fill the kernel
    // buffer (verbose warnings despite `-loglevel error`, or an outright
    // crash dump) while nothing is reading it, it would block writing
    // more of *our* PCM to its stdin — a classic two-pipe deadlock.
    // Reading eagerly also means the captured text is available for
    // `finish`'s error message on failure.
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.read_to_string(&mut buf);
        }
        buf
    });

    Ok(FfmpegSink { child, stdin, stderr_thread: Some(stderr_thread), finished: false })
}

/// [`SampleSink`] that pipes interleaved `f32le` PCM to an `ffmpeg` child
/// process's stdin. Dropping it without calling [`SampleSink::finish`]
/// (e.g. `pipeline::transcode` returning early on cancellation) kills the
/// child rather than leaving it running in the background — see the
/// [`Drop`] impl.
pub struct FfmpegSink {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    stderr_thread: Option<std::thread::JoinHandle<String>>,
    /// Set by `finish` once the child has been waited on, so `Drop`
    /// doesn't redundantly try to kill an already-exited process.
    finished: bool,
}

impl SampleSink for FfmpegSink {
    fn write(&mut self, interleaved: &[f32]) -> Result<(), ConvertError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| ConvertError::Encode("ffmpeg stdin is unexpectedly closed".to_owned()))?;

        let mut buf = Vec::with_capacity(interleaved.len() * 4);
        for &sample in interleaved {
            buf.extend_from_slice(&sample.to_le_bytes());
        }
        stdin
            .write_all(&buf)
            .map_err(|e| ConvertError::Encode(format!("writing PCM to ffmpeg failed: {e}")))?;
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), ConvertError> {
        // Close stdin first so ffmpeg sees EOF and starts flushing/exiting.
        drop(self.stdin.take());

        let status = self
            .child
            .wait()
            .map_err(|e| ConvertError::Encode(format!("waiting for ffmpeg failed: {e}")))?;
        self.finished = true;

        let stderr_output =
            self.stderr_thread.take().and_then(|handle| handle.join().ok()).unwrap_or_default();

        if !status.success() {
            let detail = stderr_output.trim();
            return Err(ConvertError::Encode(if detail.is_empty() {
                format!("ffmpeg exited with {status}")
            } else {
                format!("ffmpeg exited with {status}: {detail}")
            }));
        }
        Ok(())
    }
}

impl Drop for FfmpegSink {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Best-effort: re-mux `path`'s `ffmpeg`-encoded container to embed
/// `tags`'s text fields via a `-c copy -metadata` pass (fast and
/// lossless — only the container's tag frames change, the audio stream
/// itself is never re-encoded). No-op if `ffmpeg` isn't available or
/// `tags` has nothing to write.
///
/// Cover art isn't attempted here: embedding a picture needs a different
/// `ffmpeg` invocation per container (ID3 `APIC` for MP3, MP4 `covr` for
/// AAC/ALAC, a `METADATA_BLOCK_PICTURE` comment for Opus/Vorbis), which
/// isn't worth the added complexity for a best-effort tag copy — matches
/// `super::tag_writer`'s "tagging never fails the job" contract by simply
/// skipping what it can't do rather than erroring.
pub fn write_tags(path: &Path, tags: &WriteTags<'_>) {
    if tags.is_empty() || !detect() {
        return;
    }

    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return;
    };
    let tmp_path = path.with_extension(format!("tagtmp.{ext}"));

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner").arg("-loglevel").arg("error").arg("-y").arg("-i").arg(path).arg("-c").arg("copy");

    if let Some(v) = tags.title {
        cmd.arg("-metadata").arg(format!("title={v}"));
    }
    if let Some(v) = tags.artist {
        cmd.arg("-metadata").arg(format!("artist={v}"));
    }
    if let Some(v) = tags.album {
        cmd.arg("-metadata").arg(format!("album={v}"));
    }
    if let Some(v) = tags.genre {
        cmd.arg("-metadata").arg(format!("genre={v}"));
    }
    if let Some(v) = tags.date {
        cmd.arg("-metadata").arg(format!("date={v}"));
    }
    if let Some(n) = tags.track_number {
        let track = match tags.track_total {
            Some(total) => format!("{n}/{total}"),
            None => n.to_string(),
        };
        cmd.arg("-metadata").arg(format!("track={track}"));
    }
    if let Some(n) = tags.disc_number {
        cmd.arg("-metadata").arg(format!("disc={n}"));
    }

    cmd.arg(&tmp_path);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());

    match cmd.status() {
        Ok(status) if status.success() => {
            if let Err(e) = std::fs::rename(&tmp_path, path) {
                tracing::warn!("ffmpeg tag remux: failed to install tagged output for {path:?}: {e}");
                let _ = std::fs::remove_file(&tmp_path);
            }
        }
        Ok(status) => {
            tracing::warn!("ffmpeg tag remux exited with {status} for {path:?}");
            let _ = std::fs::remove_file(&tmp_path);
        }
        Err(e) => {
            tracing::warn!("ffmpeg tag remux failed to run for {path:?}: {e}");
            let _ = std::fs::remove_file(&tmp_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `spawn_encoder` builds the right `-c:a`/quality flags per format —
    /// checked by asserting on `Command::get_args()` rather than actually
    /// spawning `ffmpeg` (which may not be installed in the test
    /// environment, and `detect()`'s process-wide cache would make that
    /// non-deterministic across tests anyway).
    fn args_of(format: OutputFormat, lossy: &LossyOptions) -> Vec<String> {
        // Mirrors `spawn_encoder`'s argument-building exactly, without the
        // `detect()` gate or actually spawning anything.
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-f")
            .arg("f32le")
            .arg("-ar")
            .arg("44100")
            .arg("-ac")
            .arg("2")
            .arg("-i")
            .arg("-");
        match format {
            OutputFormat::Mp3 => {
                cmd.arg("-c:a").arg("libmp3lame");
                match lossy.mp3_mode {
                    Mp3Mode::Vbr(q) => {
                        cmd.arg("-q:a").arg(q.to_string());
                    }
                    Mp3Mode::Cbr(kbps) => {
                        cmd.arg("-b:a").arg(format!("{kbps}k"));
                    }
                }
            }
            OutputFormat::Aac => {
                cmd.arg("-c:a").arg("aac").arg("-b:a").arg(format!("{}k", lossy.aac_bitrate_kbps));
            }
            OutputFormat::Opus => {
                cmd.arg("-c:a").arg("libopus").arg("-b:a").arg(format!("{}k", lossy.opus_bitrate_kbps));
            }
            OutputFormat::OggVorbis => {
                cmd.arg("-c:a").arg("libvorbis").arg("-q:a").arg(format!("{}", lossy.vorbis_quality));
            }
            OutputFormat::Alac => {
                cmd.arg("-c:a").arg("alac");
            }
            _ => unreachable!(),
        }
        cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn mp3_vbr_uses_q_a_flag() {
        let lossy = LossyOptions { mp3_mode: Mp3Mode::Vbr(0), ..LossyOptions::default() };
        let args = args_of(OutputFormat::Mp3, &lossy);
        assert!(args.windows(2).any(|w| w == ["-c:a", "libmp3lame"]));
        assert!(args.windows(2).any(|w| w == ["-q:a", "0"]));
        assert!(!args.iter().any(|a| a == "-b:a"));
    }

    #[test]
    fn mp3_cbr_uses_b_a_flag_in_kbps() {
        let lossy = LossyOptions { mp3_mode: Mp3Mode::Cbr(320), ..LossyOptions::default() };
        let args = args_of(OutputFormat::Mp3, &lossy);
        assert!(args.windows(2).any(|w| w == ["-b:a", "320k"]));
        assert!(!args.iter().any(|a| a == "-q:a"));
    }

    #[test]
    fn aac_uses_native_aac_codec_and_bitrate() {
        let lossy = LossyOptions { aac_bitrate_kbps: 256, ..LossyOptions::default() };
        let args = args_of(OutputFormat::Aac, &lossy);
        assert!(args.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert!(args.windows(2).any(|w| w == ["-b:a", "256k"]));
    }

    #[test]
    fn opus_uses_libopus_and_bitrate() {
        let lossy = LossyOptions { opus_bitrate_kbps: 128, ..LossyOptions::default() };
        let args = args_of(OutputFormat::Opus, &lossy);
        assert!(args.windows(2).any(|w| w == ["-c:a", "libopus"]));
        assert!(args.windows(2).any(|w| w == ["-b:a", "128k"]));
    }

    #[test]
    fn vorbis_uses_libvorbis_and_q_a_quality() {
        let lossy = LossyOptions { vorbis_quality: 8.0, ..LossyOptions::default() };
        let args = args_of(OutputFormat::OggVorbis, &lossy);
        assert!(args.windows(2).any(|w| w == ["-c:a", "libvorbis"]));
        assert!(args.windows(2).any(|w| w == ["-q:a", "8"]));
    }

    #[test]
    fn alac_uses_alac_codec_with_no_quality_flag() {
        let args = args_of(OutputFormat::Alac, &LossyOptions::default());
        assert!(args.windows(2).any(|w| w == ["-c:a", "alac"]));
        assert!(!args.iter().any(|a| a == "-b:a" || a == "-q:a"));
    }

    #[test]
    fn every_pcm_invocation_specifies_f32le_input_format() {
        for format in
            [OutputFormat::Mp3, OutputFormat::Aac, OutputFormat::Opus, OutputFormat::OggVorbis, OutputFormat::Alac]
        {
            let args = args_of(format, &LossyOptions::default());
            assert!(args.windows(2).any(|w| w == ["-f", "f32le"]), "format {format:?} missing -f f32le");
        }
    }
}
