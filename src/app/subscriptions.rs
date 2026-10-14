// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::convert_page::ConvertEvent;
use super::{AppModel, Message};
use aulos_core::config::Config;
use aulos_core::convert::JobState;
use aulos_core::library::watcher::watch_stream;
#[cfg(feature = "visualizer")]
use aulos_core::player::pw_capture::{MpdCaptureKey, capture_stream};
use aulos_core::player::{ActiveBackend, PlaybackState};
use aulos_core::provider::mpd_events::{
    MpdEvent, MpdIdleKey, MpdPollKey, idle_stream, status_stream,
};
use aulos_core::task::interval_stream;
use cosmic::Application;
use cosmic::iced::Subscription;
#[cfg(feature = "visualizer")]
use futures_util::SinkExt;
use futures_util::{Stream, StreamExt};
#[cfg(feature = "visualizer")]
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(feature = "visualizer")]
use std::sync::Mutex;
use std::time::Duration;

impl AppModel {
    pub(super) fn build_subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            // Watch config changes
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| Message::UpdateConfig(update.config)),
        ];

        // MPRIS2 D-Bus server — unconditional and keyed by the zero-argument
        // `mpris_stream` function, so it starts once and never restarts.
        subs.push(Subscription::run(aulos_core::mpris::mpris_stream).map(Message::Mpris));

        // Window-close requests (background playback) and the slow
        // playback-extras housekeeping tick.
        subs.push(self.playback_extras_subscription());

        // Playback position ticker.
        let is_playing = self
            .session
            .playback
            .player
            .as_ref()
            .is_some_and(|p| p.state() == PlaybackState::Playing);

        // Gate the MPD status subscription on the *presence* of an MPD
        // backend, not on `is_playing` or `active_backend_type()`: right
        // after switching to an MPD provider, the freshly-created `Player`
        // always reports `Stopped` (no status has been polled yet) and
        // `active_backend_type()` only flips to `Mpd` once Aulos itself has
        // issued a `play` through it — gating on either would mean Aulos
        // never polls a server that was already playing before the switch.
        if let Some(player) = &self.session.playback.player
            && let Some(mpd) = player.mpd_backend_ref()
            && let Some(provider) = self.session.active_mpd_provider()
        {
            // MPD: poll status from the server — position/duration/state/
            // volume/current-song all come from the real MPD status.
            // `mpd_status_stream` adapts its own interval and only fetches
            // the current song on change.
            let client = mpd.client();
            let provider_id = provider.provider_id_arc();
            subs.push(Subscription::run_with(
                MpdPollKey::new(client, provider_id),
                mpd_status_stream,
            ));
        }

        // Local/Subsonic tick, independent of the MPD poll above: an MPD
        // backend can exist (MPD provider active) while playback actually
        // runs through the local backend — a radio stream or podcast
        // resolves to `LiveStream`/`HttpStream` regardless of the active
        // provider — and that playback still needs its position ticked.
        if is_playing
            && self
                .session
                .playback
                .player
                .as_ref()
                .is_some_and(|p| p.active_backend_type() != ActiveBackend::Mpd)
        {
            subs.push(Subscription::run(playback_tick_stream));
        }

        // MPD idle event subscriptions — one per configured MPD provider.
        //
        // Each subscription opens TWO separate TCP connections to the MPD server:
        // 1. Idle connection — stays in idle mode, streams events via ConnectionEvents
        // 2. Command connection — stored in MpdProvider for browse/search/playback
        //
        // This avoids protocol conflicts between idle mode and command execution.
        for (idx, provider) in self.session.providers.mpd_providers.iter().enumerate() {
            let provider = Arc::clone(provider);
            subs.push(Subscription::run_with(
                MpdIdleKey::new(idx, provider),
                mpd_idle_stream,
            ));
        }

        // Convert queue progress ticker (every 500ms while jobs are running);
        // gated on the experimental-converter flag so it never runs while
        // the feature (and its nav entry) is disabled.
        if self.session.config.config.experimental_converter
            && self
                .session
                .convert
                .jobs
                .iter()
                .any(|j| j.state == JobState::Running)
        {
            subs.push(Subscription::run(convert_tick_stream));
        }

        // Visualizer render subscription (~30fps, only when active and expanded)
        //
        // The lactea `Engine` is not `Send`, and its readback blocks on the
        // GPU, so the actual render loop runs on a dedicated
        // `std::thread::spawn` OS thread instead of a tokio worker. The iced
        // subscription channel only relays "frame ready" notifications from
        // that thread back to the UI.
        #[cfg(feature = "visualizer")]
        if self.visualizer_active
            && self.expand_progress > 0.0
            && let Some(ref pcm_buf) = self.pcm_buffer
        {
            let pcm = Arc::clone(pcm_buf);
            let cmd_rx = Arc::clone(&self.viz_cmd_rx_slot);
            let frame_buf = Arc::clone(&self.viz_frame_buf);
            let current_preset = Arc::clone(&self.viz_current_preset_shared);
            subs.push(Subscription::run_with(
                VizRenderKey {
                    pcm,
                    cmd_rx,
                    frame_buf,
                    current_preset,
                    locked: self.viz_locked,
                },
                lactea_render_stream,
            ));
        }

        // MPD PipeWire capture subscription — feeds the same PCM buffer as
        // the render subscription above, but from MPD's own PipeWire
        // playback stream instead of the local engine's tap, since aulos
        // only remote-controls MPD (no PCM otherwise reaches this
        // process). Gated on the same active+expanded condition as the
        // render subscription, further restricted to when MPD is actually
        // the backend in charge and its server resolves to this machine
        // (its own audio can only appear on *this* PipeWire graph when
        // it's local) — see `player::pw_capture` for the capture and
        // matching rules.
        #[cfg(feature = "visualizer")]
        if self.visualizer_active
            && self.expand_progress > 0.0
            && let Some(pcm_buf) = &self.pcm_buffer
            && self
                .session
                .playback
                .player
                .as_ref()
                .is_some_and(|p| p.active_backend_type() == ActiveBackend::Mpd)
            && let Some(provider) = self.session.active_mpd_provider()
        {
            if aulos_core::player::pw_capture::is_local_mpd_host(provider.host()) {
                let pcm = Arc::clone(pcm_buf);
                let mpd_playing = Arc::clone(&self.mpd_playing);
                subs.push(Subscription::run_with(
                    MpdCaptureKey::new(pcm, mpd_playing),
                    mpd_capture_stream,
                ));
            } else {
                // Logged at most once per process — this `if`
                // re-evaluates on every `update()` while a remote MPD
                // server is active and expanded/visualizing, which
                // would otherwise spam.
                static REMOTE_WARNED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !REMOTE_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::info!(
                        "MPD server '{}' is not local; visualizer audio capture is \
                         unavailable (MPD's own audio only appears on this machine's \
                         PipeWire graph when the server itself runs on it)",
                        provider.host()
                    );
                }
            }
        }

        // Filesystem watcher subscription — only when the Local provider is active.
        // Uses notify::RecommendedWatcher to watch music_dirs recursively.
        // Debounces events with a 2-second quiet timer before emitting
        // Message::FilesChanged with the collected paths.
        {
            let is_local_active =
                self.session.providers.registry.active().is_some_and(|p| {
                    p.provider_type() == aulos_core::provider::ProviderType::Local
                });

            if is_local_active && !self.session.config.config.music_dirs.is_empty() {
                let music_dirs = self.session.config.config.music_dirs.clone();
                subs.push(Subscription::run_with(music_dirs, fs_watcher_stream));
            }
        }

        // Mouse movement while the visualizer is fullscreen resets the HUD
        // control-card auto-hide idle counter (see `viz_hud_idle_frames`).
        // Scoped to `viz_fullscreen` so ordinary app usage never emits this.
        #[cfg(feature = "visualizer")]
        if self.viz_fullscreen {
            subs.push(cosmic::iced::event::listen_with(|event, _status, _id| {
                if let cosmic::iced::Event::Mouse(cosmic::iced::mouse::Event::CursorMoved {
                    ..
                }) = event
                {
                    Some(Message::VizHudActivity)
                } else {
                    None
                }
            }));
        }

        // Global keyboard shortcuts (play/pause, seek, volume, shuffle,
        // repeat, favorite, lyrics, expand/collapse, nav-page jumps,
        // search focus, escape). Replaces the old ad hoc Space/Escape/
        // Ctrl+F listeners this block used to hold. This libcosmic revision
        // exposes no `keyboard::on_key_press`, so we filter the raw event
        // stream ourselves; `event::listen_with` only yields events a
        // focused widget didn't already capture (`Status::Ignored`), which
        // is exactly the guard those hand-rolled listeners implemented.
        // `crate::keybinds::resolve` is the pure, unit-tested mapping from
        // a raw key press to a `Shortcut`; `update()` decides what each one
        // does given the current app state.
        subs.push(cosmic::iced::event::listen_with(|event, status, _id| {
            if status != cosmic::iced::event::Status::Ignored {
                return None;
            }
            match event {
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    ..
                }) => crate::keybinds::resolve(&key, modifiers).map(Message::Shortcut),
                _ => None,
            }
        }));

        Subscription::batch(subs)
    }
}

/// MPD: forwards the core status poll (see
/// `aulos_core::provider::mpd_events::status_stream`) as `MpdStatusUpdate`.
fn mpd_status_stream(key: &MpdPollKey) -> impl Stream<Item = Message> + use<> {
    status_stream(key).map(Message::MpdStatusUpdate)
}

/// Local/Subsonic: simple tick for UI updates (no captured state).
fn playback_tick_stream() -> impl Stream<Item = Message> {
    interval_stream(Duration::from_millis(500)).map(|()| Message::PlaybackTick)
}

/// Convert-queue progress ticker: while jobs are running, periodically
/// triggers a re-render so the UI picks up the latest atomic progress
/// values written by the background job tasks.
fn convert_tick_stream() -> impl Stream<Item = Message> {
    interval_stream(Duration::from_millis(500)).map(|()| Message::ConvertEvent(ConvertEvent::Tick))
}

/// MPD idle event subscription — one per configured MPD provider (see
/// `aulos_core::provider::mpd_events::idle_stream`).
fn mpd_idle_stream(key: &MpdIdleKey) -> impl Stream<Item = Message> + use<> {
    idle_stream(key).map(|event| match event {
        MpdEvent::Connected(pid) => Message::MpdConnected(pid),
        MpdEvent::ConnectionFailed(pid, error) => Message::MpdConnectionFailed(pid, error),
        MpdEvent::Idle(pid, subsystem) => Message::MpdIdleEvent(pid, subsystem),
    })
}

/// Filesystem watcher subscription — only when the Local provider is active.
/// Emits `Message::FilesChanged` with the debounced changed paths.
#[allow(clippy::ptr_arg)] // must match `fn(&D) -> S` where D = Vec<PathBuf> (Subscription::run_with)
fn fs_watcher_stream(music_dirs: &Vec<PathBuf>) -> impl Stream<Item = Message> + use<> {
    watch_stream(music_dirs).map(Message::FilesChanged)
}

/// Identity key for the lactea visualizer render subscription. Hashes to
/// a constant so there's only ever one active instance regardless of the
/// captured Arc contents (mirrors `MpdPollKey`/`MpdIdleKey`).
#[cfg(feature = "visualizer")]
struct VizRenderKey {
    pcm: Arc<Mutex<aulos_core::player::pcm::PcmBuffer>>,
    cmd_rx: Arc<Mutex<Option<std::sync::mpsc::Receiver<aulos_core::visualizer::VizCommand>>>>,
    frame_buf: Arc<Mutex<crate::views::now_playing::viz_shader::VizFrameBuffer>>,
    current_preset: Arc<Mutex<Option<std::path::PathBuf>>>,
    /// Lock state the UI shows when the subscription starts; a fresh
    /// renderer is always unlocked, so it is re-applied on startup.
    locked: bool,
}

#[cfg(feature = "visualizer")]
impl Hash for VizRenderKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "lactea-render".hash(state);
    }
}

/// Runs the lactea render loop on a dedicated OS thread and relays
/// "frame ready" notifications back to the UI through an iced subscription
/// stream. See the call site for why the render loop needs its own thread
/// (the engine is not `Send` and its readback blocks on the GPU).
#[cfg(feature = "visualizer")]
fn lactea_render_stream(key: &VizRenderKey) -> impl Stream<Item = Message> + use<> {
    let pcm = Arc::clone(&key.pcm);
    let cmd_rx_slot = Arc::clone(&key.cmd_rx);
    let frame_buf = Arc::clone(&key.frame_buf);
    let current_preset = Arc::clone(&key.current_preset);
    let initial_locked = key.locked;
    cosmic::iced::stream::channel(
        2,
        move |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            // Use a one-shot channel to know when the render thread
            // has produced a new frame so we can notify the UI.
            let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Result<(), String>>(2);

            // Spawn a dedicated OS thread for the render loop. The engine
            // created inside `VizRenderer::new` lives on this thread.
            std::thread::Builder::new()
                .name("lactea-render".into())
                .spawn(move || {
                    let preset_dir = dirs::data_dir().map(|d| d.join("lactea").join("presets"));
                    let mut renderer =
                        match crate::views::now_playing::visualizer::VizRenderer::new(preset_dir) {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::error!("Failed to create lactea renderer: {e}");
                                let _ = frame_tx.blocking_send(Err(e));
                                return;
                            }
                        };

                    // A fresh renderer starts unlocked, but the UI may still
                    // show the lock toggle on from before the visualizer was
                    // collapsed/re-opened: re-apply it. Also forget the
                    // previous renderer's preset until this one reports its own.
                    renderer.set_locked(initial_locked);
                    if let Ok(mut shared) = current_preset.lock() {
                        *shared = None;
                    }

                    // Check out the command receiver for this thread's
                    // lifetime; handed back below so a future activation
                    // (visualizer toggled off then on again) can check it
                    // out in turn — the channel itself is only ever
                    // created once, in `AppModel::init`.
                    // Quick collapse/re-expand can start this thread before
                    // the previous one has handed the receiver back (it only
                    // notices its frame channel closed on its next frame), so
                    // wait briefly rather than running with no command
                    // channel at all.
                    let mut cmd_rx = None;
                    for _ in 0..50 {
                        cmd_rx = cmd_rx_slot.lock().ok().and_then(|mut slot| slot.take());
                        if cmd_rx.is_some() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }

                    // Cursor into the shared PCM ring buffer (see
                    // `PcmBuffer::read_since`). Starting at 0 is safe even
                    // when playback is already well underway: `read_since`
                    // clamps a cursor that's fallen behind to the most
                    // recent window rather than replaying stale history,
                    // so activation just catches straight up to "now".
                    let mut pcm_cursor: u64 = 0;
                    // Wall-clock time PCM last actually arrived. Used to
                    // decay to silence — instead of freezing on the last
                    // window forever — when nothing new shows up for a
                    // while (paused, stopped, or MPD idle). `read_since`
                    // legitimately returns empty between ordinary callback
                    // periods too, so only a *sustained* gap past
                    // `PCM_STALE_TIMEOUT` triggers the silence fallback;
                    // a merely-empty-this-frame read just feeds nothing
                    // and lets lactea's own internal smoothing carry on.
                    let mut last_pcm_at = std::time::Instant::now();
                    // How long PCM may go quiet before we push explicit
                    // silence rather than just skipping this frame's feed.
                    const PCM_STALE_TIMEOUT: Duration = Duration::from_millis(100);
                    // One render period's worth of stereo silence
                    // (44.1kHz / ~30fps), fed to lactea while stale so
                    // it decays toward an idle visualization instead of
                    // replaying the last real window forever.
                    const SILENCE_FRAME_SAMPLES: usize = (44_100 / 30) * 2;
                    // Most PCM worth reading per frame: one contiguous window
                    // (576 stereo frames) so the analyser sees real audio
                    // rather than 5ms fragments ~33ms apart.
                    let max_pcm_samples = aulos_core::visualizer::PCM_FEED_SAMPLES;

                    // Fixed-deadline pacing: sleeping a flat 33 ms *after*
                    // rendering would run below 30 fps by the render time.
                    const FRAME_PERIOD: Duration = Duration::from_millis(33);
                    let mut next_frame = std::time::Instant::now() + FRAME_PERIOD;

                    loop {
                        // ~30 fps
                        let now = std::time::Instant::now();
                        if let Some(wait) = next_frame.checked_duration_since(now) {
                            std::thread::sleep(wait);
                            next_frame += FRAME_PERIOD;
                        } else {
                            // Fell behind (slow frame): resync instead of bursting.
                            next_frame = now + FRAME_PERIOD;
                        }

                        // Drain every pending command before rendering
                        // this frame.
                        if let Some(rx) = cmd_rx.as_ref() {
                            use aulos_core::visualizer::VizCommand;
                            while let Ok(cmd) = rx.try_recv() {
                                match cmd {
                                    VizCommand::NextPreset => renderer.next_preset(),
                                    VizCommand::LoadPreset(path) => {
                                        renderer.load_preset(&path);
                                    }
                                    VizCommand::SetLocked(locked) => renderer.set_locked(locked),
                                    VizCommand::SetBeatSensitivity(sensitivity) => {
                                        renderer.set_beat_sensitivity(sensitivity);
                                    }
                                }
                            }
                        }

                        // Read whatever new PCM has arrived since the last
                        // frame, newest `max_pcm_samples` at most.
                        // Empty is normal between callback periods (see
                        // the `last_pcm_at` docs above) — only sustained
                        // silence falls back to explicit zeros below.
                        let pcm_data = {
                            let fresh = pcm
                                .lock()
                                .ok()
                                .map(|buf| buf.read_since(&mut pcm_cursor, max_pcm_samples))
                                .unwrap_or_default();
                            if !fresh.is_empty() {
                                last_pcm_at = std::time::Instant::now();
                                fresh
                            } else if last_pcm_at.elapsed() >= PCM_STALE_TIMEOUT {
                                // Sustained silence: push explicit zeros so
                                // lactea's beat detection decays to idle
                                // instead of replaying the last real audio
                                // window forever.
                                vec![0.0f32; SILENCE_FRAME_SAMPLES]
                            } else {
                                Vec::new()
                            }
                        };

                        // Render a frame (GPU render + readback)
                        let rgba = renderer.render_frame(&pcm_data);
                        // Publish the preset that is now on screen — covers
                        // manual loads, "next", and automatic timer/beat
                        // switches alike (the renderer owns the rotation, so
                        // it always knows the exact file).
                        if let Some(path) = renderer.take_current_preset_change()
                            && let Ok(mut shared) = current_preset.lock()
                        {
                            *shared = Some(path);
                        }

                        // Write pixels into the shared frame buffer
                        if let Ok(mut buf) = frame_buf.lock() {
                            buf.update(rgba);
                        }

                        // Notify the async side that a frame is ready.
                        // If the channel is full or closed, the render
                        // thread is ahead of the UI — just skip.
                        if frame_tx.try_send(Ok(())).is_err() && frame_tx.is_closed() {
                            // Channel closed → subscription was dropped
                            // (visualizer deactivated or view collapsed).
                            break;
                        }
                    }

                    // Hand the receiver back so a future render-thread
                    // activation can check it out in turn.
                    if let Ok(mut slot) = cmd_rx_slot.lock() {
                        *slot = cmd_rx.take();
                    }
                })
                .expect("failed to spawn lactea-render thread");

            // Relay frame-ready signals from the render thread to iced.
            while let Some(event) = frame_rx.recv().await {
                match event {
                    Ok(()) => _ = emitter.send(Message::VisualizerFrameReady).await,
                    Err(reason) => {
                        _ = emitter.send(Message::VisualizerFailed(reason)).await;
                        break;
                    }
                }
            }
        },
    )
}

/// Captures MPD's own PipeWire playback stream into the shared PCM ring
/// buffer (see `aulos_core::player::pw_capture::capture_stream`). It never
/// yields a message; iced dropping this subscription (its gating `if` in
/// `build_subscription` going false) stops the capture.
#[cfg(feature = "visualizer")]
fn mpd_capture_stream(key: &MpdCaptureKey) -> impl Stream<Item = Message> + use<> {
    capture_stream(key).map(|never| match never {})
}
