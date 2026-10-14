//! The application's subscriptions.
//!
//! The backend's long-running event sources (filesystem watcher, MPD,
//! playback and housekeeping tickers, MPD audio capture) come from
//! `Session::background_streams`: each is run under its stable key and its
//! events go to `Message::Session`. What is built here is UI-only: the
//! cosmic config watcher, MPRIS, window-close requests, the visualizer
//! renderer, and the mouse/keyboard listeners.

use super::{AppModel, Message};
use aulos_core::config::Config;
use aulos_core::session::{BackgroundKey, SessionEvent};
use cosmic::Application;
use cosmic::iced::Subscription;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
#[cfg(feature = "visualizer")]
use futures_util::{SinkExt, Stream};
use std::hash::{Hash, Hasher};
#[cfg(feature = "visualizer")]
use std::sync::Arc;
use std::sync::Mutex;
#[cfg(feature = "visualizer")]
use std::time::Duration;

/// One of the session's background streams, as the data of a
/// `Subscription::run_with`: identified by its key alone (iced keeps the
/// subscription running while the hash is unchanged), carrying the stream
/// until the subscription actually starts and takes it. Streams do no work
/// until polled, so building one that iced then discards costs nothing.
struct BackgroundSlot {
    key: BackgroundKey,
    stream: Mutex<Option<BoxStream<'static, SessionEvent>>>,
}

impl Hash for BackgroundSlot {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

/// Starts a [`BackgroundSlot`]: its events become `Message::Session`.
fn background_stream(slot: &BackgroundSlot) -> BoxStream<'static, Message> {
    match slot.stream.lock().ok().and_then(|mut stream| stream.take()) {
        Some(stream) => stream.map(Message::Session).boxed(),
        None => futures_util::stream::empty().boxed(),
    }
}

impl AppModel {
    pub(super) fn build_subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            // Watch config changes
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| {
                    Message::Session(SessionEvent::ConfigChanged(Box::new(update.config)))
                }),
        ];

        // MPRIS2 D-Bus server — unconditional and keyed by the zero-argument
        // `mpris_stream` function, so it starts once and never restarts.
        subs.push(Subscription::run(aulos_core::mpris::mpris_stream).map(Message::Mpris));

        // Window-close requests (background playback).
        subs.push(self.playback_extras_subscription());

        // The backend's own event sources: filesystem watcher, MPD status
        // and idle events, playback/housekeeping/convert tickers and the
        // MPD audio capture (see `Session::background_streams` for what
        // gates each).
        subs.extend(
            self.session
                .background_streams()
                .into_iter()
                .map(|(key, stream)| {
                    Subscription::run_with(
                        BackgroundSlot {
                            key,
                            stream: Mutex::new(Some(stream)),
                        },
                        background_stream,
                    )
                }),
        );

        // Visualizer render subscription (~30fps, only when active and expanded)
        //
        // The lactea `Engine` is not `Send`, and its readback blocks on the
        // GPU, so the actual render loop runs on a dedicated
        // `std::thread::spawn` OS thread instead of a tokio worker. The iced
        // subscription channel only relays "frame ready" notifications from
        // that thread back to the UI.
        #[cfg(feature = "visualizer")]
        if self.visualizer_active && self.expand_progress > 0.0 {
            let pcm = Arc::clone(&self.session.playback.pcm_buffer);
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

/// Identity key for the lactea visualizer render subscription. Hashes to
/// a constant so there's only ever one active instance regardless of the
/// captured Arc contents.
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
