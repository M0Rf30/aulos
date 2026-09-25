// SPDX-License-Identifier: GPL-3.0

//! Captures MPD's own PipeWire playback stream so the projectM visualizer
//! reacts to audio when MPD is the active backend.
//!
//! Lyra only remote-controls MPD (see `mpd_backend`/`provider::mpd`): no PCM
//! passes through the in-process decode/output engine that feeds the
//! visualizer tap for local playback (`engine::engine::tap_visualizer`).
//! Instead, when MPD is playing, this module opens its own PipeWire client,
//! watches the registry for MPD's own audio output stream, and links an
//! input stream directly to it — the same trick `pw-record --target`/
//! `qpwgraph` use to record a specific running application regardless of
//! which sink it plays into.
//!
//! # Target selection
//! A registry `global` is treated as MPD's stream (see [`is_mpd_output_stream`])
//! when it is a `Node` whose `media.class` is `Stream/Output/Audio` **and**
//! any of:
//! - `application.process.binary` is exactly `mpd` or `rmpd` (case-insensitive)
//! - `application.name` contains "music player daemon" or "mpd"
//!   (case-insensitive; also covers the `rmpd` fork since it contains "mpd")
//! - `node.name` starts with "mpd" (case-insensitive)
//!
//! If no such node is present, capture falls back to the default sink's
//! *monitor* ports (`stream.capture.sink = true`, no explicit target) —
//! this is the only way to get *some* signal when MPD hasn't been observed
//! on the graph yet (e.g. it hasn't started playing since PipeWire booted).
//! Because the monitor mixes in every other application's audio too,
//! fallback-mode writes into the PCM buffer are gated on the caller-supplied
//! `mpd_playing` flag so the visualizer doesn't react to unrelated audio
//! while MPD itself is paused or stopped.
//!
//! # Lifecycle
//! [`PwCapture::spawn`] starts a dedicated OS thread running a PipeWire
//! `MainLoop` (PipeWire objects are `!Send`/`!Sync` and must live on one
//! thread) and blocks until that thread has either connected to the
//! PipeWire daemon or failed to, surfacing the failure as an `Err` instead
//! of panicking (e.g. when PipeWire isn't running at all). A registry
//! listener watches for MPD's node appearing/disappearing (MPD may close
//! its output on pause/stop) and rebuilds the capture stream accordingly.
//! Dropping the returned [`PwCapture`] sends a shutdown message over a
//! `pipewire::channel`, which the mainloop thread's attached receiver turns
//! into `loop_.quit()`, then joins the thread — so capture starts and stops
//! exactly when the owning subscription is alive.

use crate::views::now_playing::visualizer::PcmBuffer;
use pipewire as pw;
use pw::spa;
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;
use spa::utils::dict::DictRef;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Error starting or running the PipeWire capture thread.
#[derive(Debug)]
pub struct PwCaptureError(pub String);

impl std::fmt::Display for PwCaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PwCaptureError {}

impl From<String> for PwCaptureError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Message sent from the owning thread to ask the capture's mainloop thread
/// to quit. Carries no data — `pipewire::channel` needs a concrete type,
/// and "shut down" is the only message this capture ever needs to send.
struct Terminate;

/// Handle to a running MPD PipeWire capture.
///
/// Holding this alive keeps the dedicated PipeWire mainloop thread (and
/// whatever stream it currently has connected) running. Dropping it asks
/// that thread to quit its loop and joins it, so capture stops
/// deterministically rather than being left to a detached thread's fate.
pub struct PwCapture {
    shutdown: Option<pw::channel::Sender<Terminate>>,
    thread: Option<JoinHandle<()>>,
}

impl PwCapture {
    /// Spawn the capture thread and wait for it to either connect to
    /// PipeWire or report why it couldn't.
    ///
    /// `pcm` is the shared ring buffer the visualizer render loop reads
    /// from (see `views::now_playing::visualizer::PcmBuffer`); `mpd_playing`
    /// gates writes made while capturing from the default-sink monitor
    /// fallback (see the module docs).
    pub fn spawn(
        pcm: Arc<Mutex<PcmBuffer>>,
        mpd_playing: Arc<AtomicBool>,
    ) -> Result<Self, PwCaptureError> {
        let (cmd_tx, cmd_rx) = pw::channel::channel::<Terminate>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

        let thread = std::thread::Builder::new()
            .name("lyra-pw-capture".into())
            .spawn(move || {
                if let Err(e) = run_capture_loop(pcm, mpd_playing, cmd_rx, ready_tx.clone()) {
                    tracing::warn!("MPD PipeWire capture stopped: {e}");
                    // If startup itself failed, `ready_tx` hasn't been sent
                    // yet — make sure `spawn`'s `recv()` still unblocks. A
                    // send after a successful startup (thread ending later,
                    // e.g. mainloop.run() returning) harmlessly fails since
                    // the receiver was already dropped; ignore that.
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| PwCaptureError(format!("failed to spawn capture thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                shutdown: Some(cmd_tx),
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(PwCaptureError(e))
            }
            Err(_) => {
                // The thread panicked or exited without ever sending
                // readiness; surface that instead of hanging.
                let _ = thread.join();
                Err(PwCaptureError(
                    "capture thread exited before signalling readiness".to_string(),
                ))
            }
        }
    }
}

impl Drop for PwCapture {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(Terminate);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs entirely on the dedicated capture thread: connects to PipeWire,
/// watches the registry, keeps a capture stream connected to the best
/// available target, and blocks in `mainloop.run()` until asked to quit.
///
/// Sends exactly one readiness result on `ready_tx` — `Ok(())` once
/// connected and the initial (fallback) stream is up, or `Err` if any setup
/// step fails (most commonly: PipeWire isn't running).
fn run_capture_loop(
    pcm: Arc<Mutex<PcmBuffer>>,
    mpd_playing: Arc<AtomicBool>,
    cmd_rx: pw::channel::Receiver<Terminate>,
    ready_tx: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| format!("main loop: {e}"))?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).map_err(|e| format!("context: {e}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("connect to PipeWire (is it running?): {e}"))?;
    let registry = core
        .get_registry_rc()
        .map_err(|e| format!("registry: {e}"))?;

    let state = Rc::new(RefCell::new(CaptureState::new(core, pcm, mpd_playing)));

    // Start from the fallback (default-sink monitor) stream; `reconcile()`
    // below switches to a specific MPD node the moment the registry
    // reports one, including any that already existed before this
    // client connected (the registry replays its full current state to
    // every new client).
    state.borrow_mut().rebuild_stream(None);

    let state_for_global = Rc::clone(&state);
    let state_for_remove = Rc::clone(&state);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = global.props else {
                return;
            };
            state_for_global
                .borrow_mut()
                .on_global_node(global.id, props);
        })
        .global_remove(move |id| {
            state_for_remove.borrow_mut().on_global_removed(id);
        })
        .register();

    let mainloop_for_quit = mainloop.clone();
    let _cmd_listener = cmd_rx.attach(mainloop.loop_(), move |Terminate| {
        mainloop_for_quit.quit();
    });

    // Startup succeeded; let `PwCapture::spawn` return.
    let _ = ready_tx.send(Ok(()));

    mainloop.run();
    Ok(())
}

/// Mutable state owned by the capture thread: which MPD output-stream
/// nodes currently exist on the graph, and the currently-connected stream
/// (targeting one of them, or the default-sink monitor fallback).
struct CaptureState {
    core: pw::core::CoreRc,
    pcm: Arc<Mutex<PcmBuffer>>,
    mpd_playing: Arc<AtomicBool>,
    /// Registry id -> `target.object` value (the node's `object.serial`,
    /// falling back to its `node.name` on ancient PipeWire without serials)
    /// for every currently-live node identified as MPD's output stream.
    mpd_nodes: HashMap<u32, String>,
    /// Registry id of the node the active stream is currently targeting,
    /// or `None` while using the default-sink monitor fallback.
    current_target_id: Option<u32>,
    active: Option<ActiveStream>,
}

impl CaptureState {
    fn new(
        core: pw::core::CoreRc,
        pcm: Arc<Mutex<PcmBuffer>>,
        mpd_playing: Arc<AtomicBool>,
    ) -> Self {
        Self {
            core,
            pcm,
            mpd_playing,
            mpd_nodes: HashMap::new(),
            current_target_id: None,
            active: None,
        }
    }

    /// Registry `global` callback for `Node` globals: records the node if
    /// it matches [`is_mpd_output_stream`], then reconciles the active
    /// stream's target.
    fn on_global_node(&mut self, id: u32, props: &DictRef) {
        if !is_mpd_output_stream(
            props.get("media.class"),
            props.get("application.process.binary"),
            props.get("application.name"),
            props.get("node.name"),
        ) {
            return;
        }
        let target = props
            .get("object.serial")
            .or_else(|| props.get("node.name"))
            .map(str::to_string);
        let Some(target) = target else {
            tracing::warn!(
                "MPD PipeWire stream (node {id}) matched but has neither \
                 object.serial nor node.name; cannot target it"
            );
            return;
        };
        self.mpd_nodes.insert(id, target);
        self.reconcile();
    }

    /// Registry `global_remove` callback: MPD may close its PipeWire
    /// output stream on pause/stop, so any tracked node can disappear at
    /// any time — reconcile back to the fallback (or another remaining
    /// MPD node) when that happens.
    fn on_global_removed(&mut self, id: u32) {
        if self.mpd_nodes.remove(&id).is_some() {
            self.reconcile();
        }
    }

    /// Recomputes the desired capture target from `mpd_nodes` and rebuilds
    /// the stream only if it actually changed, so a `global`/`global_remove`
    /// pair that doesn't affect the *chosen* target (e.g. a second MPD
    /// instance appearing while the first one is already targeted) doesn't
    /// needlessly tear down a perfectly good stream.
    fn reconcile(&mut self) {
        let desired = self.mpd_nodes.iter().min_by_key(|&(&id, _)| id);
        let desired_id = desired.map(|(&id, _)| id);
        if desired_id == self.current_target_id {
            return;
        }
        let desired = desired.map(|(&id, target)| (id, target.clone()));
        self.rebuild_stream(desired);
    }

    /// Tears down the current stream (if any — dropping [`ActiveStream`]
    /// disconnects and destroys it) and connects a new one, either
    /// targeting a specific MPD node or the default-sink monitor fallback.
    fn rebuild_stream(&mut self, target: Option<(u32, String)>) {
        self.current_target_id = target.as_ref().map(|(id, _)| *id);
        let target_str = target.map(|(_, t)| t);
        match build_stream(
            &self.core,
            target_str.as_deref(),
            Arc::clone(&self.pcm),
            Arc::clone(&self.mpd_playing),
        ) {
            Ok(active) => {
                if let Some(t) = &target_str {
                    tracing::debug!("MPD visualizer capture targeting PipeWire node ({t})");
                } else {
                    tracing::debug!("MPD visualizer capture using default-sink monitor fallback");
                }
                self.active = Some(active);
            }
            Err(e) => {
                tracing::warn!("Failed to (re)build MPD visualizer capture stream: {e}");
                self.active = None;
            }
        }
    }
}

/// A connected capture stream plus the listener keeping its callbacks
/// registered. Dropping this disconnects and destroys the underlying
/// PipeWire stream (`StreamRc`'s `Drop` calls `pw_stream_destroy`, which
/// implicitly disconnects) and unregisters its callbacks.
struct ActiveStream {
    _stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<StreamUserData>,
}

/// Per-stream state visible to the `param_changed`/`process` callbacks.
struct StreamUserData {
    format: spa::param::audio::AudioInfoRaw,
    pcm: Arc<Mutex<PcmBuffer>>,
    mpd_playing: Arc<AtomicBool>,
    /// `true` for the default-sink-monitor fallback stream, where writes
    /// must be gated on `mpd_playing` so other applications' audio doesn't
    /// drive the visualizer while MPD itself isn't the one playing.
    require_mpd_playing: bool,
    /// Reused across `process` calls to avoid steady-state allocation
    /// (only grows if a negotiated buffer is larger than any seen so far).
    scratch: Vec<f32>,
}

/// Builds and connects one capture stream, either targeting `target`
/// (`target.object`, an MPD node's `object.serial`/`node.name`) or, when
/// `None`, the default sink's monitor ports.
fn build_stream(
    core: &pw::core::CoreRc,
    target: Option<&str>,
    pcm: Arc<Mutex<PcmBuffer>>,
    mpd_playing: Arc<AtomicBool>,
) -> Result<ActiveStream, String> {
    let mut props = pw::properties::properties! {
        "media.type" => "Audio",
        "media.category" => "Capture",
        "media.role" => "Music",
        "node.name" => "lyra-visualizer",
    };
    let require_mpd_playing = if let Some(target) = target {
        props.insert("target.object", target);
        false
    } else {
        // Redirect the default target from the default *source* (likely a
        // microphone — the module docs explain why we must not just
        // autoconnect blindly) to the default sink's monitor ports.
        props.insert("stream.capture.sink", "true");
        true
    };

    let stream = pw::stream::StreamRc::new(core.clone(), "lyra-visualizer", props)
        .map_err(|e| format!("stream create: {e}"))?;

    let user_data = StreamUserData {
        format: spa::param::audio::AudioInfoRaw::new(),
        pcm,
        mpd_playing,
        require_mpd_playing,
        scratch: Vec::new(),
    };

    let listener = stream
        .add_local_listener_with_user_data(user_data)
        .param_changed(|_stream, data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                return;
            };
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            if data.format.parse(param).is_err() {
                tracing::warn!("MPD visualizer capture: failed to parse negotiated audio format");
                return;
            }
            tracing::debug!(
                "MPD visualizer capture negotiated rate={} channels={}",
                data.format.rate(),
                data.format.channels()
            );
        })
        .process(|stream, data| {
            // Always dequeue (and let it re-queue on drop below) even if
            // we end up not using it, or PipeWire's buffer pool stalls.
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            if data.require_mpd_playing && !data.mpd_playing.load(Ordering::Relaxed) {
                return;
            }
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let channels = data.format.channels().max(1) as usize;
            let chunk_size = datas[0].chunk().size() as usize;
            let Some(raw) = datas[0].data() else {
                return;
            };
            let valid_bytes = chunk_size.min(raw.len());
            let n_samples = valid_bytes / std::mem::size_of::<f32>();

            data.scratch.clear();
            data.scratch.extend(
                raw[..n_samples * std::mem::size_of::<f32>()]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );

            // `try_lock`, never block: this runs on PipeWire's realtime
            // data thread, same rule as the cpal output callback.
            if let Ok(mut pcm) = data.pcm.try_lock() {
                pcm.write_interleaved(&data.scratch, channels);
            }
        })
        .register()
        .map_err(|e| format!("listener register: {e}"))?;

    // Request raw interleaved F32LE; leave rate/channels unset so the
    // stream accepts whatever the target node/graph is natively running
    // at, read back from `param_changed` above.
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|_| "failed to serialize format param".to_string())?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or("invalid format pod")?];

    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| format!("stream connect: {e}"))?;

    Ok(ActiveStream {
        _stream: stream,
        _listener: listener,
    })
}

/// Pure, unit-tested predicate for whether a registry `Node` global's
/// properties describe MPD's (or the `rmpd` fork's) own PipeWire playback
/// stream. Kept free of any PipeWire types so it's testable with plain
/// string fixtures — see the module docs for the exact rules.
fn is_mpd_output_stream(
    media_class: Option<&str>,
    process_binary: Option<&str>,
    application_name: Option<&str>,
    node_name: Option<&str>,
) -> bool {
    if media_class != Some("Stream/Output/Audio") {
        return false;
    }
    if let Some(bin) = process_binary
        && (bin.eq_ignore_ascii_case("mpd") || bin.eq_ignore_ascii_case("rmpd"))
    {
        return true;
    }
    if let Some(name) = application_name {
        let lower = name.to_ascii_lowercase();
        if lower.contains("music player daemon") || lower.contains("mpd") {
            return true;
        }
    }
    if let Some(name) = node_name
        && name.to_ascii_lowercase().starts_with("mpd")
    {
        return true;
    }
    false
}

/// Pure predicate: does `host` (as configured for an MPD server connection,
/// `config::MpdConfigEntry::host` / `provider::mpd::MpdConfig::host`) refer
/// to this same machine? MPD's own PipeWire stream only shows up on the
/// local PipeWire graph when the MPD server itself runs locally, so this
/// gates whether the capture subscription is even worth starting.
///
/// `machine_hostname` is injected so this stays pure/unit-testable; real
/// callers go through [`is_local_mpd_host`], which supplies it from
/// [`local_hostname`].
pub(crate) fn is_local_host(host: &str, machine_hostname: Option<&str>) -> bool {
    let host = host.trim();
    if host.is_empty() {
        return false;
    }
    if host.starts_with('/') {
        // A unix-domain socket path is inherently local.
        return true;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => v4.octets()[0] == 127,
            std::net::IpAddr::V6(v6) => v6.is_loopback(),
        };
    }
    machine_hostname.is_some_and(|name| host.eq_ignore_ascii_case(name.trim()))
}

/// Reads this machine's hostname from `/proc/sys/kernel/hostname`.
///
/// Lyra is Linux/COSMIC-specific, so reading this virtual file avoids
/// pulling in a whole crate (`hostname`/`gethostname`) for a single
/// `libc::gethostname()` call.
fn local_hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Convenience wrapper combining [`is_local_host`] with this machine's real
/// hostname; the one call sites outside tests should use.
pub(crate) fn is_local_mpd_host(host: &str) -> bool {
    is_local_host(host, local_hostname().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- is_mpd_output_stream --

    #[test]
    fn matches_mpd_process_binary() {
        assert!(is_mpd_output_stream(
            Some("Stream/Output/Audio"),
            Some("mpd"),
            None,
            Some("alsa_output.mpd"),
        ));
    }

    #[test]
    fn matches_rmpd_process_binary_case_insensitive() {
        assert!(is_mpd_output_stream(
            Some("Stream/Output/Audio"),
            Some("RMPD"),
            None,
            None,
        ));
    }

    #[test]
    fn matches_application_name_music_player_daemon() {
        assert!(is_mpd_output_stream(
            Some("Stream/Output/Audio"),
            None,
            Some("Music Player Daemon"),
            None,
        ));
    }

    #[test]
    fn matches_application_name_containing_mpd() {
        assert!(is_mpd_output_stream(
            Some("Stream/Output/Audio"),
            None,
            Some("mpd"),
            None,
        ));
    }

    #[test]
    fn matches_node_name_prefix_case_insensitive() {
        assert!(is_mpd_output_stream(
            Some("Stream/Output/Audio"),
            None,
            None,
            Some("MPD-output"),
        ));
    }

    #[test]
    fn rejects_wrong_media_class() {
        assert!(!is_mpd_output_stream(
            Some("Stream/Input/Audio"),
            Some("mpd"),
            None,
            None,
        ));
    }

    #[test]
    fn rejects_missing_media_class() {
        assert!(!is_mpd_output_stream(None, Some("mpd"), None, None));
    }

    #[test]
    fn rejects_unrelated_process() {
        assert!(!is_mpd_output_stream(
            Some("Stream/Output/Audio"),
            Some("firefox"),
            Some("Firefox"),
            Some("firefox-output"),
        ));
    }

    #[test]
    fn rejects_process_binary_only_containing_mpd() {
        // `application.process.binary` must be exactly "mpd"/"rmpd", not
        // merely contain it — unlike the `application.name`/`node.name`
        // checks.
        assert!(!is_mpd_output_stream(
            Some("Stream/Output/Audio"),
            Some("mpdfoo"),
            None,
            None,
        ));
    }

    #[test]
    fn rejects_node_name_containing_but_not_starting_with_mpd() {
        assert!(!is_mpd_output_stream(
            Some("Stream/Output/Audio"),
            None,
            None,
            Some("music-mpd-relay"),
        ));
    }

    #[test]
    fn no_properties_at_all_does_not_match() {
        assert!(!is_mpd_output_stream(
            Some("Stream/Output/Audio"),
            None,
            None,
            None,
        ));
    }

    // -- is_local_host --

    #[test]
    fn localhost_is_local() {
        assert!(is_local_host("localhost", None));
        assert!(is_local_host("LOCALHOST", None));
    }

    #[test]
    fn loopback_v4_range_is_local() {
        assert!(is_local_host("127.0.0.1", None));
        assert!(is_local_host("127.5.6.7", None));
    }

    #[test]
    fn loopback_v6_is_local() {
        assert!(is_local_host("::1", None));
    }

    #[test]
    fn unix_socket_path_is_local() {
        assert!(is_local_host("/run/mpd/socket", None));
    }

    #[test]
    fn matching_machine_hostname_is_local() {
        assert!(is_local_host("my-desktop", Some("my-desktop")));
        assert!(is_local_host("MY-DESKTOP", Some("my-desktop")));
    }

    #[test]
    fn remote_ip_is_not_local() {
        assert!(!is_local_host("8.8.8.8", Some("my-desktop")));
    }

    #[test]
    fn remote_hostname_is_not_local() {
        assert!(!is_local_host("other-machine", Some("my-desktop")));
    }

    #[test]
    fn empty_host_is_not_local() {
        assert!(!is_local_host("", Some("my-desktop")));
    }

    // -- live PipeWire capture (requires a running PipeWire daemon) --

    #[test]
    #[ignore = "requires a running PipeWire daemon; run with `--ignored`"]
    fn capture_starts_and_shuts_down_cleanly_against_monitor_fallback() {
        let pcm = Arc::new(Mutex::new(PcmBuffer::new(8192)));
        let mpd_playing = Arc::new(AtomicBool::new(true));

        let capture = PwCapture::spawn(Arc::clone(&pcm), Arc::clone(&mpd_playing))
            .expect("PwCapture::spawn should succeed against a running PipeWire daemon");

        std::thread::sleep(std::time::Duration::from_secs(1));

        drop(capture); // must join cleanly without panicking or hanging
    }
}
