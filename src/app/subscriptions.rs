// SPDX-License-Identifier: GPL-3.0

use super::{AppModel, Message};
use crate::config::Config;
use crate::convert::JobState;
use crate::player::{ActiveBackend, PlaybackState};
use crate::provider::MusicProvider;
use crate::provider::mpd::{MpdProvider, song_to_track};
use cosmic::Application;
use cosmic::iced::Subscription;
use futures_util::{SinkExt, Stream};
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
        subs.push(Subscription::run(crate::mpris::mpris_stream).map(Message::Mpris));

        // Playback position ticker.
        let is_playing = self
            .player
            .as_ref()
            .is_some_and(|p| p.state() == PlaybackState::Playing);

        // Gate the MPD status subscription on the *presence* of an MPD
        // backend, not on `is_playing` or `active_backend_type()`: right
        // after switching to an MPD provider, the freshly-created `Player`
        // always reports `Stopped` (no status has been polled yet) and
        // `active_backend_type()` only flips to `Mpd` once Lyra itself has
        // issued a `play` through it — gating on either would mean Lyra
        // never polls a server that was already playing before the switch.
        if let Some(player) = &self.player
            && let Some(mpd) = player.mpd_backend_ref()
            && let Some(provider) = self.active_mpd_provider()
        {
            // MPD: poll status from the server — position/duration/state/
            // volume/current-song all come from the real MPD status.
            // `mpd_status_stream` adapts its own interval and only fetches
            // the current song on change.
            let client = mpd.client();
            let provider_id = provider.provider_id_arc();
            subs.push(Subscription::run_with(
                MpdPollKey(client, provider_id),
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
        for (idx, provider) in self.mpd_providers.iter().enumerate() {
            let provider = Arc::clone(provider);
            subs.push(Subscription::run_with(
                MpdIdleKey { idx, provider },
                mpd_idle_stream,
            ));
        }

        // Convert queue progress ticker (every 500ms while jobs are running)
        if self
            .convert_jobs
            .iter()
            .any(|j| j.state == JobState::Running)
        {
            subs.push(Subscription::run(convert_tick_stream));
        }

        // Expand/collapse animation tick (~60fps, only during transitions)
        if self.expand_target.is_some() {
            subs.push(
                cosmic::iced::time::every(Duration::from_millis(16))
                    .map(|_| Message::ExpandAnimTick),
            );
        }

        // Visualizer render subscription (~30fps, only when active and expanded)
        //
        // IMPORTANT: The projectM renderer requires a current EGL/GL context
        // which is thread-local. Tokio's multi-threaded runtime migrates
        // futures between OS threads across `.await` points, which would
        // lose the GL context and produce black/garbage frames (flickering)
        // and prevent PCM audio data from reaching projectM (no beat
        // reactivity). To avoid this, the actual render loop runs on a
        // dedicated `std::thread::spawn` OS thread. The iced subscription
        // channel only relays "frame ready" notifications from that thread
        // back to the UI.
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
                },
                projectm_render_stream,
            ));
        }

        // MPD PipeWire capture subscription — feeds the same PCM buffer as
        // the render subscription above, but from MPD's own PipeWire
        // playback stream instead of the local engine's tap, since lyra
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
                .player
                .as_ref()
                .is_some_and(|p| p.active_backend_type() == ActiveBackend::Mpd)
            && let Some(provider) = self.active_mpd_provider()
        {
            if crate::player::pw_capture::is_local_mpd_host(provider.host()) {
                let pcm = Arc::clone(pcm_buf);
                let mpd_playing = Arc::clone(&self.mpd_playing);
                subs.push(Subscription::run_with(
                    MpdCaptureKey { pcm, mpd_playing },
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
            let is_local_active = self
                .registry
                .active()
                .is_some_and(|p| p.provider_type() == crate::provider::ProviderType::Local);

            if is_local_active && !self.config.music_dirs.is_empty() {
                let music_dirs = self.config.music_dirs.clone();
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

/// Identity key for the MPD status-poll subscription. Hashing only the
/// fixed string (not the client/provider id) preserves the old
/// fixed-string-id behavior: a single persistent subscription while an MPD
/// backend exists, never restarted just because the client handle value
/// changes.
struct MpdPollKey(mpd_client::Client, Arc<str>);

impl Hash for MpdPollKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Include the provider id so switching to a *different* MPD server
        // restarts the poll against the new client; excluding the client
        // handle itself keeps a reconnect to the same server from
        // needlessly tearing the subscription down.
        "mpd-status-poll".hash(state);
        self.1.hash(state);
    }
}

/// MPD: poll status from the server, adapting the interval to playback
/// state — 300ms while playing (keeps the position bar smooth), ~1s
/// otherwise (an idle connection doesn't need sub-second polling) — and
/// fetching the current song only when its identity changes, so steady
/// state stays at exactly one `Status` command per tick.
fn mpd_status_stream(key: &MpdPollKey) -> impl Stream<Item = Message> + use<> {
    let client = key.0.clone();
    let provider_id = Arc::clone(&key.1);
    cosmic::iced::stream::channel(
        1,
        |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            const PLAYING_INTERVAL: Duration = Duration::from_millis(300);
            const IDLE_INTERVAL: Duration = Duration::from_secs(1);

            let mut last_song_id: Option<mpd_client::commands::SongId> = None;
            loop {
                match client.command(mpd_client::commands::Status).await {
                    Ok(status) => {
                        let state = match status.state {
                            mpd_client::responses::PlayState::Playing => PlaybackState::Playing,
                            mpd_client::responses::PlayState::Paused => PlaybackState::Paused,
                            mpd_client::responses::PlayState::Stopped => PlaybackState::Stopped,
                        };

                        // Fetch the current song only when its identity
                        // changed (including the first tick) — an unchanged
                        // song never needs a second command. While MPD is
                        // stopped there is nothing to adopt, so forget the
                        // last id: the next transition into Playing/Paused
                        // then re-fetches instead of seeing an unchanged id
                        // and staying silent forever.
                        let current_id = status.current_song.map(|(_, id)| id);
                        let song = if state == PlaybackState::Stopped {
                            last_song_id = None;
                            None
                        } else if current_id == last_song_id {
                            None
                        } else {
                            last_song_id = current_id;
                            match client.command(mpd_client::commands::CurrentSong).await {
                                Ok(Some(song_in_queue)) => {
                                    Some(song_to_track(&provider_id, &song_in_queue.song))
                                }
                                Ok(None) => None,
                                Err(e) => {
                                    tracing::warn!("MPD current-song fetch failed: {e}");
                                    None
                                }
                            }
                        };

                        let next_delay = if state == PlaybackState::Playing {
                            PLAYING_INTERVAL
                        } else {
                            IDLE_INTERVAL
                        };

                        _ = emitter
                            .send(Message::MpdStatusUpdate {
                                position: status.elapsed.unwrap_or(Duration::ZERO),
                                duration: status.duration.unwrap_or(Duration::ZERO),
                                state,
                                volume: status.volume as f32 / 100.0,
                                song,
                            })
                            .await;

                        tokio::time::sleep(next_delay).await;
                    }
                    Err(e) => {
                        tracing::warn!("MPD status poll failed: {e}");
                        tokio::time::sleep(IDLE_INTERVAL).await;
                    }
                }
            }
        },
    )
}

/// Local/Subsonic: simple tick for UI updates (no captured state).
fn playback_tick_stream() -> impl Stream<Item = Message> {
    cosmic::iced::stream::channel(
        1,
        |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
                interval.tick().await;
                _ = emitter.send(Message::PlaybackTick).await;
            }
        },
    )
}

/// Convert-queue progress ticker: while jobs are running, periodically
/// triggers a re-render so the UI picks up the latest atomic progress
/// values written by the background job tasks.
fn convert_tick_stream() -> impl Stream<Item = Message> {
    cosmic::iced::stream::channel(
        1,
        |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
                interval.tick().await;
                _ = emitter.send(Message::ConvertTick).await;
            }
        },
    )
}

/// Identity key for an MPD idle-event subscription. Hashing only `idx`
/// (not the provider) preserves the old `("mpd-idle", idx)` identity
/// semantics.
struct MpdIdleKey {
    idx: usize,
    provider: Arc<MpdProvider>,
}

impl Hash for MpdIdleKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.idx.hash(state);
    }
}

/// MPD idle event subscription — one per configured MPD provider.
///
/// Each subscription opens TWO separate TCP connections to the MPD server:
/// 1. Idle connection — stays in idle mode, streams events via ConnectionEvents
/// 2. Command connection — stored in MpdProvider for browse/search/playback
///
/// This avoids protocol conflicts between idle mode and command execution.
fn mpd_idle_stream(key: &MpdIdleKey) -> impl Stream<Item = Message> + use<> {
    let provider = Arc::clone(&key.provider);
    cosmic::iced::stream::channel(
        4,
        move |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            loop {
                let pid = provider.id().to_string();

                // Step 1: Establish the idle connection
                // We must keep `_idle_client` alive — dropping it would
                // close the background task and end the `events` stream.
                let idle_result = provider.connect_idle().await;
                let (_idle_client, mut events) = match idle_result {
                    Ok(pair) => pair,
                    Err(e) => {
                        _ = emitter
                            .send(Message::MpdConnectionFailed(pid, e.to_string()))
                            .await;
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };

                // Step 2: Establish the command connection
                if let Err(e) = provider.connect_command().await {
                    tracing::error!("MPD provider '{pid}' command connection failed: {e}");
                    _ = emitter
                        .send(Message::MpdConnectionFailed(pid, e.to_string()))
                        .await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }

                // Both connections established
                _ = emitter.send(Message::MpdConnected(pid.clone())).await;

                // Loop on idle events until the connection drops
                while let Some(_event) = events.next().await {
                    _ = emitter.send(Message::MpdIdleEvent(pid.clone())).await;
                }

                // Idle stream ended — connection lost. Disconnect command too.
                tracing::warn!("MPD provider '{pid}' connection lost, reconnecting...");
                provider.disconnect().await;

                // Backoff before reconnect
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        },
    )
}

/// Filesystem watcher subscription — only when the Local provider is active.
///
/// Uses notify::RecommendedWatcher to watch music_dirs recursively.
/// Debounces events with a 2-second quiet timer before emitting
/// Message::FilesChanged with the collected paths.
#[allow(clippy::ptr_arg)] // must match `fn(&D) -> S` where D = Vec<PathBuf> (Subscription::run_with)
fn fs_watcher_stream(music_dirs: &Vec<PathBuf>) -> impl Stream<Item = Message> + use<> {
    let music_dirs = music_dirs.clone();
    cosmic::iced::stream::channel(
        4,
        move |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            use notify::{RecursiveMode, Watcher};

            let (tx, mut rx) = tokio::sync::mpsc::channel::<PathBuf>(256);

            // Create watcher that sends changed paths through the channel.
            // IMPORTANT: Only react to content changes (Create/Modify/Remove),
            // NOT Access events. Reading files during scanning triggers Access
            // events which would create an infinite scan loop.
            let _watcher = {
                let tx = tx.clone();
                let mut watcher = match notify::RecommendedWatcher::new(
                    move |result: Result<notify::Event, notify::Error>| {
                        if let Ok(event) = result {
                            use notify::EventKind;
                            match event.kind {
                                EventKind::Create(_)
                                | EventKind::Modify(_)
                                | EventKind::Remove(_) => {
                                    for path in event.paths {
                                        let _ = tx.blocking_send(path);
                                    }
                                }
                                _ => {}
                            }
                        }
                    },
                    notify::Config::default(),
                ) {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::error!("Failed to create filesystem watcher: {e}");
                        // Keep the future alive so the subscription ID is stable.
                        std::future::pending::<()>().await;
                        return;
                    }
                };

                for dir in &music_dirs {
                    if let Err(e) = watcher.watch(dir, RecursiveMode::Recursive) {
                        tracing::warn!("Failed to watch directory {}: {e}", dir.display());
                    }
                }

                watcher // keep alive
            };

            // Debounce loop: collect paths, wait for 2s of quiet, then emit.
            let debounce_duration = Duration::from_secs(2);
            loop {
                // Wait for the first event.
                let first = match rx.recv().await {
                    Some(path) => path,
                    None => break, // channel closed
                };

                let mut changed_paths = vec![first];

                // Collect more events until 2 seconds of silence.
                loop {
                    match tokio::time::timeout(debounce_duration, rx.recv()).await {
                        Ok(Some(path)) => {
                            changed_paths.push(path);
                        }
                        Ok(None) => break, // channel closed
                        Err(_) => break,   // timeout — debounce complete
                    }
                }

                // Deduplicate paths.
                changed_paths.sort();
                changed_paths.dedup();

                tracing::debug!(
                    "Filesystem watcher: {} changed paths after debounce",
                    changed_paths.len()
                );

                _ = emitter.send(Message::FilesChanged(changed_paths)).await;
            }
        },
    )
}

/// Identity key for the projectM visualizer render subscription. Hashes to
/// a constant so there's only ever one active instance regardless of the
/// captured Arc contents (mirrors `MpdPollKey`/`MpdIdleKey`).
#[cfg(feature = "visualizer")]
struct VizRenderKey {
    pcm: Arc<Mutex<crate::views::now_playing::visualizer::PcmBuffer>>,
    cmd_rx: Arc<
        Mutex<Option<std::sync::mpsc::Receiver<crate::views::now_playing::visualizer::VizCommand>>>,
    >,
    frame_buf: Arc<Mutex<crate::views::now_playing::viz_shader::VizFrameBuffer>>,
    current_preset: Arc<Mutex<Option<String>>>,
}

#[cfg(feature = "visualizer")]
impl Hash for VizRenderKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "projectm-render".hash(state);
    }
}

/// Runs the projectM render loop on a dedicated OS thread and relays
/// "frame ready" notifications back to the UI through an iced subscription
/// stream. See the call site for why the render loop needs its own thread
/// (thread-local EGL/GL context).
#[cfg(feature = "visualizer")]
fn projectm_render_stream(key: &VizRenderKey) -> impl Stream<Item = Message> + use<> {
    let pcm = Arc::clone(&key.pcm);
    let cmd_rx_slot = Arc::clone(&key.cmd_rx);
    let frame_buf = Arc::clone(&key.frame_buf);
    let current_preset = Arc::clone(&key.current_preset);
    cosmic::iced::stream::channel(
        2,
        move |mut emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            // Use a one-shot channel to know when the render thread
            // has produced a new frame so we can notify the UI.
            let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<()>(2);

            // Spawn a dedicated OS thread for the GL render loop.
            // The EGL context created inside `ProjectMRenderer::new`
            // stays current for the lifetime of this thread.
            std::thread::Builder::new()
                .name("projectm-render".into())
                .spawn(move || {
                    let preset_dir = dirs::data_dir().map(|d| d.join("projectm").join("presets"));
                    let mut renderer =
                        match crate::views::now_playing::visualizer::ProjectMRenderer::new(
                            preset_dir,
                        ) {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::error!("Failed to create projectM renderer: {e}");
                                return;
                            }
                        };

                    // Check out the command receiver for this thread's
                    // lifetime; handed back below so a future activation
                    // (visualizer toggled off then on again) can check it
                    // out in turn — the channel itself is only ever
                    // created once, in `AppModel::init`.
                    let mut cmd_rx = cmd_rx_slot.lock().ok().and_then(|mut slot| slot.take());

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
                    // and lets projectM's own internal smoothing carry on.
                    let mut last_pcm_at = std::time::Instant::now();
                    // How long PCM may go quiet before we push explicit
                    // silence rather than just skipping this frame's feed.
                    const PCM_STALE_TIMEOUT: Duration = Duration::from_millis(100);
                    // One render period's worth of stereo silence
                    // (44.1kHz / ~30fps), fed to projectM while stale so
                    // it decays toward an idle visualization instead of
                    // replaying the last real window forever.
                    const SILENCE_FRAME_SAMPLES: usize = (44_100 / 30) * 2;
                    // projectM's own cap on samples per feed — computed
                    // once (it never changes for a given instance) rather
                    // than re-querying every frame.
                    let max_pcm_samples = projectm::core::ProjectM::pcm_get_max_samples() as usize;

                    loop {
                        // ~30 fps
                        std::thread::sleep(Duration::from_millis(33));

                        // Drain every pending command before rendering
                        // this frame.
                        if let Some(rx) = cmd_rx.as_ref() {
                            use crate::views::now_playing::visualizer::VizCommand;
                            while let Ok(cmd) = rx.try_recv() {
                                match cmd {
                                    VizCommand::NextPreset => {
                                        renderer.next_preset();
                                        // The playlist API exposes no way to
                                        // read back which preset it landed
                                        // on — clear the tracked name.
                                        if let Ok(mut name) = current_preset.lock() {
                                            *name = None;
                                        }
                                    }
                                    VizCommand::LoadPreset(path) => {
                                        renderer.load_preset(&path);
                                        let display_name = path
                                            .file_stem()
                                            .map(|s| s.to_string_lossy().into_owned());
                                        if let Ok(mut name) = current_preset.lock() {
                                            *name = display_name;
                                        }
                                    }
                                    VizCommand::SetLocked(locked) => renderer.set_locked(locked),
                                    VizCommand::SetBeatSensitivity(sensitivity) => {
                                        renderer.set_beat_sensitivity(sensitivity);
                                    }
                                }
                            }
                        }

                        // Read whatever new PCM has arrived since the last
                        // frame, capped to what projectM will accept.
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
                                // projectM's beat detection decays to idle
                                // instead of replaying the last real audio
                                // window forever.
                                vec![0.0f32; SILENCE_FRAME_SAMPLES]
                            } else {
                                Vec::new()
                            }
                        };

                        // Render a frame (GL calls, ~3-5ms)
                        let rgba = renderer.render_frame(&pcm_data);

                        // Write pixels into the shared frame buffer
                        if let Ok(mut buf) = frame_buf.lock() {
                            buf.update(rgba);
                        }

                        // Notify the async side that a frame is ready.
                        // If the channel is full or closed, the render
                        // thread is ahead of the UI — just skip.
                        if frame_tx.try_send(()).is_err() {
                            // Channel closed → subscription was dropped
                            // (visualizer deactivated or view collapsed).
                            if frame_tx.is_closed() {
                                break;
                            }
                        }
                    }

                    // Hand the receiver back so a future render-thread
                    // activation can check it out in turn.
                    if let Ok(mut slot) = cmd_rx_slot.lock() {
                        *slot = cmd_rx.take();
                    }
                })
                .expect("failed to spawn projectm-render thread");

            // Relay frame-ready signals from the render thread to iced.
            while frame_rx.recv().await.is_some() {
                _ = emitter.send(Message::VisualizerFrameReady).await;
            }
        },
    )
}

/// Identity key for the MPD PipeWire capture subscription. Hashes to a
/// constant (mirrors `VizRenderKey`) so there's only ever one active
/// instance; starting/stopping is entirely driven by whether the `if` in
/// `build_subscription` includes this subscription at all, never by the
/// key's contents changing.
#[cfg(feature = "visualizer")]
struct MpdCaptureKey {
    pcm: Arc<Mutex<crate::views::now_playing::visualizer::PcmBuffer>>,
    mpd_playing: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(feature = "visualizer")]
impl Hash for MpdCaptureKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "mpd-pw-capture".hash(state);
    }
}

/// Captures MPD's own PipeWire playback stream into the shared PCM ring
/// buffer, so the projectM visualizer reacts to audio while MPD (not the
/// in-process local engine) is the active backend — see
/// `player::pw_capture` for the capture itself, its node-matching rules,
/// and the default-sink-monitor fallback.
///
/// The `PwCapture` handle is created *inside* this async block and held
/// for as long as the returned future runs. iced drops that future (and
/// with it, the handle — whose `Drop` joins the PipeWire thread) the
/// moment this subscription's gating `if` in `build_subscription` goes
/// false, so capture starts and stops exactly when a render subscription
/// would actually want PCM data sourced from MPD.
#[cfg(feature = "visualizer")]
fn mpd_capture_stream(key: &MpdCaptureKey) -> impl Stream<Item = Message> + use<> {
    let pcm = Arc::clone(&key.pcm);
    let mpd_playing = Arc::clone(&key.mpd_playing);
    cosmic::iced::stream::channel(
        1,
        move |_emitter: cosmic::iced::futures::channel::mpsc::Sender<Message>| async move {
            let _capture = match crate::player::pw_capture::PwCapture::spawn(pcm, mpd_playing) {
                Ok(capture) => capture,
                Err(e) => {
                    tracing::warn!("MPD visualizer capture unavailable: {e}");
                    return;
                }
            };
            // Held alive by this future until iced drops it (see the doc
            // comment above) — never resolves on its own.
            futures_util::future::pending::<()>().await;
        },
    )
}
