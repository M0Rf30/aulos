// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Android's media session and playback notification: the track on the lock
//! screen, in the shade and on Bluetooth/headset controls, a foreground
//! service that keeps the process (and its audio) alive in the background,
//! a partial wake lock while playing, and the transport commands coming back.
//!
//! The Android counterpart of `media_session.rs` (web) and the desktop MPRIS
//! backends: [`sync`] runs once per frame from `App::logic`, publishes what
//! the session's [`MprisSnapshot`] says and returns the [`MprisCommand`]s the
//! user gave through the system controls.
//!
//! Almost everything is plain JNI against framework classes (`MediaSession`,
//! `Notification.MediaStyle`, `PowerManager`). Only what needs real Java
//! subclasses lives in `crates/aulos/android/classes.dex` (see its generator):
//! `MediaService`, a foreground service, and `MediaCallback`, a
//! `MediaSession.Callback`. Both forward transport commands to
//! [`native_command`], registered here with `RegisterNatives`. Without the
//! dex (a plain `cargo apk build`) the layer logs a warning and stays off.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock, PoisonError};

use aulos_core::mpris::{MprisCommand, MprisSnapshot, MprisStatus, changed_beyond_ticking};
use aulos_core::session::Session;
use jni::errors::Result as JniResult;
use jni::objects::{GlobalRef, JClass, JObject, JValue};
use jni::sys::{jint, jlong};
use jni::{JNIEnv, NativeMethod};

const SERVICE_CLASS: &str = "io.github.m0rf30.aulos.MediaService";
const CALLBACK_CLASS: &str = "io.github.m0rf30.aulos.MediaCallback";
const CHANNEL_ID: &str = "playback";
/// What `MediaService.onStartCommand` passes to `startForeground`.
const NOTIFICATION_ID: jint = 1;
/// The longest side of the cover handed to the system, in pixels.
const ART_SIZE: i32 = 512;
/// The longest the wake lock may be held without being renewed: frames stop
/// while the app is in the background, so a lock that nothing releases (the
/// queue stalls there, see AGENTS.md) must still run out.
const WAKE_CAP_MS: i64 = 4 * 3_600_000;
/// Added to the time left in the track before the wake lock lapses.
const WAKE_SLACK_MS: i64 = 60_000;

// `MediaService.nativeCommand` codes (see `build_dex.py`).
const CMD_PLAY: jint = 1;
const CMD_PAUSE: jint = 2;
const CMD_STOP: jint = 3;
const CMD_NEXT: jint = 4;
const CMD_PREVIOUS: jint = 5;
const CMD_SEEK: jint = 6;

/// Commands the system sent, waiting for the next frame.
static COMMANDS: Mutex<VecDeque<MprisCommand>> = Mutex::new(VecDeque::new());
/// Wakes the UI when a command arrives.
static REPAINT: OnceLock<egui::Context> = OnceLock::new();
static STATE: Mutex<State> = Mutex::new(State::new());

/// `MediaService.nativeCommand`, called on the Android main thread by the
/// service (notification buttons) and the session callback (everything else).
extern "system" fn native_command(_env: JNIEnv, _class: JClass, command: jint, arg: jlong) {
    let command = match command {
        CMD_PLAY => MprisCommand::Play,
        CMD_PAUSE => MprisCommand::Pause,
        CMD_STOP => MprisCommand::Stop,
        CMD_NEXT => MprisCommand::Next,
        CMD_PREVIOUS => MprisCommand::Previous,
        // The position arrives in milliseconds.
        CMD_SEEK => MprisCommand::SetPosition(arg.saturating_mul(1000)),
        _ => return,
    };
    COMMANDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push_back(command);
    if let Some(ctx) = REPAINT.get() {
        ctx.request_repaint();
    }
}

/// Publishes the session's playback state to Android and returns the
/// commands the system controls sent since the last call.
pub fn sync(ctx: &egui::Context, session: &mut Session) -> Vec<MprisCommand> {
    REPAINT.get_or_init(|| ctx.clone());
    STATE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .publish(session);
    COMMANDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .drain(..)
        .collect()
}

/// Ends the media session: removes the notification, stops the service and
/// releases the wake lock. Called once the app has left its event loop.
pub fn shutdown() {
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    let Backend::Ready(handles) = std::mem::replace(&mut state.backend, Backend::Unavailable)
    else {
        return;
    };
    let Some((vm, activity)) = super::host() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let result = env.with_local_frame(32, |env| -> JniResult<()> {
        handles.stop_playback(env, &activity)?;
        call_void(env, handles.session.as_obj(), "release", "()V", &[])
    });
    clear_on_error(&mut env, "ending the media session", result);
}

/// What was built for the system and what has been published to it.
struct State {
    backend: Backend,
    /// The snapshot last published (even when publishing failed, so a broken
    /// call is not retried every frame).
    last: Option<MprisSnapshot>,
    /// Whether the foreground service is running.
    service_up: bool,
    /// Whether the notification permission was already asked this install.
    asked_permission: bool,
}

enum Backend {
    /// Nothing was ever playable, so nothing was created.
    Untried,
    Ready(Handles),
    /// The dex is missing or JNI failed: the layer stays off.
    Unavailable,
}

/// The Java objects the layer keeps.
struct Handles {
    session: GlobalRef,
    /// `Class<MediaService>`.
    service_class: GlobalRef,
    notifications: GlobalRef,
    wake_lock: GlobalRef,
    /// The status-bar glyph (`res/drawable/ic_stat_aulos.xml`).
    small_icon: jint,
}

impl State {
    const fn new() -> Self {
        Self {
            backend: Backend::Untried,
            last: None,
            service_up: false,
            asked_permission: false,
        }
    }

    fn publish(&mut self, session: &mut Session) {
        if matches!(self.backend, Backend::Unavailable) {
            return;
        }
        let art_key = session
            .playback
            .current_track
            .as_ref()
            .map(|track| track.cover_key())
            .filter(|key| session.cover_bytes.contains(key));
        let snapshot = session.media_snapshot(art_key.clone());
        let previous = self.last.as_ref();

        let meta = previous.is_none_or(|p| {
            (
                &p.title,
                &p.artist,
                &p.album,
                p.track_id,
                p.length_us,
                &p.art_url,
            ) != (
                &snapshot.title,
                &snapshot.artist,
                &snapshot.album,
                snapshot.track_id,
                snapshot.length_us,
                &snapshot.art_url,
            )
        });
        let controls = previous.is_none_or(|p| {
            (
                p.status,
                p.can_go_next,
                p.can_go_previous,
                p.can_seek,
                p.can_play,
            ) != (
                snapshot.status,
                snapshot.can_go_next,
                snapshot.can_go_previous,
                snapshot.can_seek,
                snapshot.can_play,
            )
        });
        // A seek, or a position that does not follow the clock. Volume,
        // shuffle and repeat do not concern the system's controls.
        let moved = previous.is_none_or(|p| {
            let mut probe = snapshot.clone();
            probe.volume = p.volume;
            probe.shuffle = p.shuffle;
            probe.loop_mode = p.loop_mode;
            changed_beyond_ticking(p, &probe)
        });
        if !(meta || controls || moved) {
            return;
        }
        let first = self.last.is_none();
        self.last = Some(snapshot.clone());
        if first && snapshot.is_idle() {
            return;
        }
        let art = art_key.and_then(|key| session.cover_bytes.get(&key));
        self.apply(
            &snapshot,
            art.as_deref().map(Vec::as_slice),
            Change {
                meta,
                controls,
                moved,
            },
        );
    }

    fn apply(&mut self, snapshot: &MprisSnapshot, art: Option<&[u8]>, change: Change) {
        let Some((vm, activity)) = super::host() else {
            return;
        };
        let Ok(mut env) = vm.attach_current_thread() else {
            return;
        };
        if matches!(self.backend, Backend::Untried) {
            match Handles::new(&mut env, &activity) {
                Ok(handles) => self.backend = Backend::Ready(handles),
                Err(error) => {
                    tracing::warn!("Android media session unavailable: {error}");
                    let _ = env.exception_clear();
                    self.backend = Backend::Unavailable;
                    return;
                }
            }
        }
        let Backend::Ready(handles) = &self.backend else {
            return;
        };
        if snapshot.status == MprisStatus::Playing && !self.asked_permission {
            self.asked_permission = true;
            super::ask_notification_permission();
        }
        let service_up = &mut self.service_up;
        let result = env.with_local_frame(96, |env| {
            handles.apply(env, &activity, snapshot, art, change, service_up)
        });
        clear_on_error(&mut env, "publishing the media session", result);
    }
}

/// What the layer needs to know about a snapshot beyond its fields.
trait SnapshotExt {
    /// Nothing is playing or paused: no track, or playback stopped.
    fn is_idle(&self) -> bool;
}

impl SnapshotExt for MprisSnapshot {
    fn is_idle(&self) -> bool {
        !self.can_play || self.status == MprisStatus::Stopped
    }
}

/// Which parts of the snapshot changed.
#[derive(Clone, Copy)]
struct Change {
    /// Title, artist, album, length or cover: the metadata and the notification text.
    meta: bool,
    /// Play state or available transport buttons.
    controls: bool,
    /// Anything that moves the position: the playback state is republished.
    moved: bool,
}

impl Handles {
    /// Loads the dex classes, registers the native command, and creates the
    /// notification channel, the media session and the wake lock.
    fn new(env: &mut JNIEnv, activity: &JObject) -> JniResult<Self> {
        env.with_local_frame(48, |env| {
            let loader = call(
                env,
                activity,
                "getClassLoader",
                "()Ljava/lang/ClassLoader;",
                &[],
            )?;
            // `FindClass` from this native thread only sees the boot class
            // path: the app's own classes come from its class loader.
            let mut load = |name: &str| -> JniResult<JObject> {
                let name = env.new_string(name)?;
                call(
                    env,
                    &loader,
                    "loadClass",
                    "(Ljava/lang/String;)Ljava/lang/Class;",
                    &[JValue::Object(&name)],
                )
            };
            let service_class = load(SERVICE_CLASS)?;
            let callback_class = load(CALLBACK_CLASS)?;
            let service_jclass = JClass::from(env.new_local_ref(&service_class)?);
            env.register_native_methods(
                &service_jclass,
                &[NativeMethod {
                    name: "nativeCommand".into(),
                    sig: "(IJ)V".into(),
                    fn_ptr: native_command as *mut c_void,
                }],
            )?;

            let notifications = system_service(env, activity, "notification")?;
            let name = env.new_string(aulos_core::fl!("settings-playback"))?;
            let id = env.new_string(CHANNEL_ID)?;
            let channel = env.new_object(
                "android/app/NotificationChannel",
                "(Ljava/lang/String;Ljava/lang/CharSequence;I)V",
                // IMPORTANCE_LOW: no sound, still in the shade.
                &[JValue::Object(&id), JValue::Object(&name), JValue::Int(2)],
            )?;
            call_void(env, &channel, "setShowBadge", "(Z)V", &[false.into()])?;
            call_void(
                env,
                &notifications,
                "createNotificationChannel",
                "(Landroid/app/NotificationChannel;)V",
                &[JValue::Object(&channel)],
            )?;

            let tag = env.new_string("Aulos")?;
            let session = env.new_object(
                "android/media/session/MediaSession",
                "(Landroid/content/Context;Ljava/lang/String;)V",
                &[JValue::Object(activity), JValue::Object(&tag)],
            )?;
            let callback = env.new_object(JClass::from(callback_class), "()V", &[])?;
            // Callbacks run on the main thread: this thread has no looper.
            let main_looper = env
                .call_static_method(
                    "android/os/Looper",
                    "getMainLooper",
                    "()Landroid/os/Looper;",
                    &[],
                )?
                .l()?;
            let handler = env.new_object(
                "android/os/Handler",
                "(Landroid/os/Looper;)V",
                &[JValue::Object(&main_looper)],
            )?;
            call_void(
                env,
                &session,
                "setCallback",
                "(Landroid/media/session/MediaSession$Callback;Landroid/os/Handler;)V",
                &[JValue::Object(&callback), JValue::Object(&handler)],
            )?;

            let power = system_service(env, activity, "power")?;
            let tag = env.new_string("Aulos:playback")?;
            let wake_lock = call(
                env,
                &power,
                "newWakeLock",
                "(ILjava/lang/String;)Landroid/os/PowerManager$WakeLock;",
                // PARTIAL_WAKE_LOCK
                &[JValue::Int(1), JValue::Object(&tag)],
            )?;
            call_void(
                env,
                &wake_lock,
                "setReferenceCounted",
                "(Z)V",
                &[false.into()],
            )?;

            let resources = call(
                env,
                activity,
                "getResources",
                "()Landroid/content/res/Resources;",
                &[],
            )?;
            let package = call(env, activity, "getPackageName", "()Ljava/lang/String;", &[])?;
            let icon_name = env.new_string("ic_stat_aulos")?;
            let kind = env.new_string("drawable")?;
            let small_icon = env
                .call_method(
                    &resources,
                    "getIdentifier",
                    "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)I",
                    &[
                        JValue::Object(&icon_name),
                        JValue::Object(&kind),
                        JValue::Object(&package),
                    ],
                )?
                .i()?;

            Ok(Self {
                session: env.new_global_ref(&session)?,
                service_class: env.new_global_ref(&service_class)?,
                notifications: env.new_global_ref(&notifications)?,
                wake_lock: env.new_global_ref(&wake_lock)?,
                small_icon,
            })
        })
    }

    fn apply(
        &self,
        env: &mut JNIEnv,
        activity: &JObject,
        snapshot: &MprisSnapshot,
        art: Option<&[u8]>,
        change: Change,
        service_up: &mut bool,
    ) -> JniResult<()> {
        // Nothing to control: no notification, no service, no wake lock.
        if snapshot.is_idle() {
            self.set_state(env, snapshot, 0)?;
            call_void(
                env,
                self.session.as_obj(),
                "setActive",
                "(Z)V",
                &[false.into()],
            )?;
            *service_up = false;
            return self.stop_playback(env, activity);
        }
        call_void(
            env,
            self.session.as_obj(),
            "setActive",
            "(Z)V",
            &[true.into()],
        )?;
        // The notification is rebuilt whole whenever the track or the buttons
        // change, so it needs the cover on both.
        let bitmap = match art {
            Some(bytes) if change.meta || change.controls => decode_cover(env, bytes)
                .unwrap_or_else(|error| {
                    tracing::warn!("cover for the media session unusable: {error}");
                    let _ = env.exception_clear();
                    JObject::null()
                }),
            _ => JObject::null(),
        };
        if change.meta {
            self.set_metadata(env, snapshot, &bitmap)?;
        }
        if change.moved || change.meta || change.controls {
            let state = match snapshot.status {
                MprisStatus::Playing => 3,
                MprisStatus::Paused => 2,
                MprisStatus::Stopped => 1,
            };
            self.set_state(env, snapshot, state)?;
        }
        if change.meta || change.controls {
            self.show_notification(env, activity, snapshot, &bitmap, service_up)?;
        }
        self.hold_wake_lock(env, snapshot)
    }

    fn set_metadata(
        &self,
        env: &mut JNIEnv,
        snapshot: &MprisSnapshot,
        bitmap: &JObject,
    ) -> JniResult<()> {
        let builder = env.new_object("android/media/MediaMetadata$Builder", "()V", &[])?;
        let put_string = |env: &mut JNIEnv, key: &str, value: &str| -> JniResult<()> {
            let key = env.new_string(key)?;
            let value = env.new_string(value)?;
            call(
                env,
                &builder,
                "putString",
                "(Ljava/lang/String;Ljava/lang/String;)Landroid/media/MediaMetadata$Builder;",
                &[JValue::Object(&key), JValue::Object(&value)],
            )
            .map(drop)
        };
        put_string(env, "android.media.metadata.TITLE", &snapshot.title)?;
        put_string(env, "android.media.metadata.ARTIST", &snapshot.artist)?;
        put_string(env, "android.media.metadata.ALBUM", &snapshot.album)?;
        if snapshot.length_us > 0 {
            let key = env.new_string("android.media.metadata.DURATION")?;
            call(
                env,
                &builder,
                "putLong",
                "(Ljava/lang/String;J)Landroid/media/MediaMetadata$Builder;",
                &[
                    JValue::Object(&key),
                    JValue::Long(snapshot.length_us / 1000),
                ],
            )?;
        }
        if !bitmap.is_null() {
            for key in [
                "android.media.metadata.ALBUM_ART",
                "android.media.metadata.ART",
            ] {
                let key = env.new_string(key)?;
                call(
                    env,
                    &builder,
                    "putBitmap",
                    "(Ljava/lang/String;Landroid/graphics/Bitmap;)Landroid/media/MediaMetadata$Builder;",
                    &[JValue::Object(&key), JValue::Object(bitmap)],
                )?;
            }
        }
        let metadata = call(
            env,
            &builder,
            "build",
            "()Landroid/media/MediaMetadata;",
            &[],
        )?;
        call_void(
            env,
            self.session.as_obj(),
            "setMetadata",
            "(Landroid/media/MediaMetadata;)V",
            &[JValue::Object(&metadata)],
        )
    }

    /// `state` is a `PlaybackState.STATE_*` constant.
    fn set_state(&self, env: &mut JNIEnv, snapshot: &MprisSnapshot, state: jint) -> JniResult<()> {
        // ACTION_STOP | ACTION_PAUSE | ACTION_PLAY | ACTION_PLAY_PAUSE
        let mut actions: i64 = 0x1 | 0x2 | 0x4 | 0x200;
        if snapshot.can_go_previous {
            actions |= 0x10;
        }
        if snapshot.can_go_next {
            actions |= 0x20;
        }
        if snapshot.can_seek {
            actions |= 0x100;
        }
        if state == 0 {
            actions = 0;
        }
        let builder = env.new_object("android/media/session/PlaybackState$Builder", "()V", &[])?;
        call(
            env,
            &builder,
            "setActions",
            "(J)Landroid/media/session/PlaybackState$Builder;",
            &[JValue::Long(actions)],
        )?;
        let now = env
            .call_static_method("android/os/SystemClock", "elapsedRealtime", "()J", &[])?
            .j()?;
        call(
            env,
            &builder,
            "setState",
            "(IJFJ)Landroid/media/session/PlaybackState$Builder;",
            &[
                JValue::Int(state),
                JValue::Long((snapshot.position_us / 1000).max(0)),
                JValue::Float(if state == 3 { 1.0 } else { 0.0 }),
                JValue::Long(now),
            ],
        )?;
        let built = call(
            env,
            &builder,
            "build",
            "()Landroid/media/session/PlaybackState;",
            &[],
        )?;
        call_void(
            env,
            self.session.as_obj(),
            "setPlaybackState",
            "(Landroid/media/session/PlaybackState;)V",
            &[JValue::Object(&built)],
        )
    }

    /// Builds the media notification and either starts the foreground service
    /// with it or, when the service already runs, replaces the shown one.
    fn show_notification(
        &self,
        env: &mut JNIEnv,
        activity: &JObject,
        snapshot: &MprisSnapshot,
        bitmap: &JObject,
        service_up: &mut bool,
    ) -> JniResult<()> {
        let service_class = JClass::from(env.new_local_ref(self.service_class.as_obj())?);
        let playing = snapshot.status == MprisStatus::Playing;

        let id = env.new_string(CHANNEL_ID)?;
        let builder = env.new_object(
            "android/app/Notification$Builder",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[JValue::Object(activity), JValue::Object(&id)],
        )?;
        let set = |env: &mut JNIEnv, name: &str, sig: &str, arg: JValue| -> JniResult<()> {
            call(env, &builder, name, sig, &[arg]).map(drop)
        };
        const BUILDER: &str = "Landroid/app/Notification$Builder;";
        set(
            env,
            "setSmallIcon",
            &format!("(I){BUILDER}"),
            JValue::Int(self.small_icon),
        )?;
        let title = env.new_string(&snapshot.title)?;
        set(
            env,
            "setContentTitle",
            &format!("(Ljava/lang/CharSequence;){BUILDER}"),
            JValue::Object(&title),
        )?;
        let artist = env.new_string(&snapshot.artist)?;
        set(
            env,
            "setContentText",
            &format!("(Ljava/lang/CharSequence;){BUILDER}"),
            JValue::Object(&artist),
        )?;
        let album = env.new_string(&snapshot.album)?;
        set(
            env,
            "setSubText",
            &format!("(Ljava/lang/CharSequence;){BUILDER}"),
            JValue::Object(&album),
        )?;
        if !bitmap.is_null() {
            set(
                env,
                "setLargeIcon",
                &format!("(Landroid/graphics/Bitmap;){BUILDER}"),
                JValue::Object(bitmap),
            )?;
        }
        let open = open_app_intent(env, activity)?;
        set(
            env,
            "setContentIntent",
            &format!("(Landroid/app/PendingIntent;){BUILDER}"),
            JValue::Object(&open),
        )?;
        set(
            env,
            "setOnlyAlertOnce",
            &format!("(Z){BUILDER}"),
            true.into(),
        )?;
        set(env, "setShowWhen", &format!("(Z){BUILDER}"), false.into())?;
        // A paused notification may be swiped away.
        set(env, "setOngoing", &format!("(Z){BUILDER}"), playing.into())?;
        // VISIBILITY_PUBLIC: the lock screen shows the track.
        set(
            env,
            "setVisibility",
            &format!("(I){BUILDER}"),
            JValue::Int(1),
        )?;
        let category = env.new_string("transport")?;
        set(
            env,
            "setCategory",
            &format!("(Ljava/lang/String;){BUILDER}"),
            JValue::Object(&category),
        )?;

        let mut buttons = Vec::new();
        if snapshot.can_go_previous {
            buttons.push((
                "ic_media_previous",
                aulos_core::fl!("previous"),
                CMD_PREVIOUS,
            ));
        }
        if playing {
            buttons.push(("ic_media_pause", aulos_core::fl!("pause"), CMD_PAUSE));
        } else {
            buttons.push(("ic_media_play", aulos_core::fl!("play"), CMD_PLAY));
        }
        if snapshot.can_go_next {
            buttons.push(("ic_media_next", aulos_core::fl!("next"), CMD_NEXT));
        }
        for (drawable, label, command) in &buttons {
            let action =
                notification_action(env, activity, &service_class, drawable, label, *command)?;
            set(
                env,
                "addAction",
                &format!("(Landroid/app/Notification$Action;){BUILDER}"),
                JValue::Object(&action),
            )?;
        }

        let style = env.new_object("android/app/Notification$MediaStyle", "()V", &[])?;
        let token = call(
            env,
            self.session.as_obj(),
            "getSessionToken",
            "()Landroid/media/session/MediaSession$Token;",
            &[],
        )?;
        call(
            env,
            &style,
            "setMediaSession",
            "(Landroid/media/session/MediaSession$Token;)Landroid/app/Notification$MediaStyle;",
            &[JValue::Object(&token)],
        )?;
        let compact: Vec<jint> = (0..buttons.len().min(3) as jint).collect();
        let indices = env.new_int_array(compact.len() as i32)?;
        env.set_int_array_region(&indices, 0, &compact)?;
        call(
            env,
            &style,
            "setShowActionsInCompactView",
            "([I)Landroid/app/Notification$MediaStyle;",
            &[JValue::Object(&indices)],
        )?;
        set(
            env,
            "setStyle",
            &format!("(Landroid/app/Notification$Style;){BUILDER}"),
            JValue::Object(&style),
        )?;
        let notification = call(env, &builder, "build", "()Landroid/app/Notification;", &[])?;

        // `MediaService.onStartCommand` reads it from here.
        env.set_static_field(
            &service_class,
            (&service_class, "notification", "Landroid/app/Notification;"),
            JValue::Object(&notification),
        )?;
        if *service_up {
            return call_void(
                env,
                self.notifications.as_obj(),
                "notify",
                "(ILandroid/app/Notification;)V",
                &[JValue::Int(NOTIFICATION_ID), JValue::Object(&notification)],
            );
        }
        let intent = service_intent(env, activity, &service_class)?;
        call(
            env,
            activity,
            "startForegroundService",
            "(Landroid/content/Intent;)Landroid/content/ComponentName;",
            &[JValue::Object(&intent)],
        )?;
        *service_up = true;
        Ok(())
    }

    /// Holds the wake lock while playing, for as long as the track can last
    /// (renewed on every state change), and releases it otherwise.
    fn hold_wake_lock(&self, env: &mut JNIEnv, snapshot: &MprisSnapshot) -> JniResult<()> {
        let lock = self.wake_lock.as_obj();
        if snapshot.status != MprisStatus::Playing {
            return call_void(env, lock, "release", "()V", &[]);
        }
        let left_ms = if snapshot.length_us > 0 {
            ((snapshot.length_us - snapshot.position_us) / 1000).max(0) + WAKE_SLACK_MS
        } else {
            WAKE_CAP_MS
        };
        call_void(
            env,
            lock,
            "acquire",
            "(J)V",
            &[JValue::Long(left_ms.min(WAKE_CAP_MS))],
        )
    }

    /// Stops the service (and with it its notification) and releases the wake lock.
    fn stop_playback(&self, env: &mut JNIEnv, activity: &JObject) -> JniResult<()> {
        let service_class = JClass::from(env.new_local_ref(self.service_class.as_obj())?);
        let intent = service_intent(env, activity, &service_class)?;
        call(
            env,
            activity,
            "stopService",
            "(Landroid/content/Intent;)Z",
            &[JValue::Object(&intent)],
        )?;
        let lock = self.wake_lock.as_obj();
        if env.call_method(lock, "isHeld", "()Z", &[])?.z()? {
            call_void(env, lock, "release", "()V", &[])?;
        }
        Ok(())
    }
}

/// `new Intent(activity, service)`.
fn service_intent<'l>(
    env: &mut JNIEnv<'l>,
    activity: &JObject,
    service_class: &JClass,
) -> JniResult<JObject<'l>> {
    env.new_object(
        "android/content/Intent",
        "(Landroid/content/Context;Ljava/lang/Class;)V",
        &[JValue::Object(activity), JValue::Object(service_class)],
    )
}

/// `FLAG_IMMUTABLE | FLAG_UPDATE_CURRENT`.
const PENDING_FLAGS: jint = 0x0400_0000 | 0x0800_0000;

/// A notification button: the foreground service gets the command as an extra.
fn notification_action<'l>(
    env: &mut JNIEnv<'l>,
    activity: &JObject,
    service_class: &JClass,
    drawable: &str,
    label: &str,
    command: jint,
) -> JniResult<JObject<'l>> {
    let intent = service_intent(env, activity, service_class)?;
    let key = env.new_string("cmd")?;
    call(
        env,
        &intent,
        "putExtra",
        "(Ljava/lang/String;I)Landroid/content/Intent;",
        &[JValue::Object(&key), JValue::Int(command)],
    )?;
    // The command is also the request code: extras do not tell two pending
    // intents apart, request codes do.
    let pending = env
        .call_static_method(
            "android/app/PendingIntent",
            "getService",
            "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
            &[
                JValue::Object(activity),
                JValue::Int(command),
                JValue::Object(&intent),
                JValue::Int(PENDING_FLAGS),
            ],
        )?
        .l()?;
    let resource = env
        .get_static_field("android/R$drawable", drawable, "I")?
        .i()?;
    let icon = env
        .call_static_method(
            "android/graphics/drawable/Icon",
            "createWithResource",
            "(Landroid/content/Context;I)Landroid/graphics/drawable/Icon;",
            &[JValue::Object(activity), JValue::Int(resource)],
        )?
        .l()?;
    let label = env.new_string(label)?;
    let builder = env.new_object(
        "android/app/Notification$Action$Builder",
        "(Landroid/graphics/drawable/Icon;Ljava/lang/CharSequence;Landroid/app/PendingIntent;)V",
        &[
            JValue::Object(&icon),
            JValue::Object(&label),
            JValue::Object(&pending),
        ],
    )?;
    call(
        env,
        &builder,
        "build",
        "()Landroid/app/Notification$Action;",
        &[],
    )
}

/// A pending intent that brings the app to the front (a tap on the notification).
fn open_app_intent<'l>(env: &mut JNIEnv<'l>, activity: &JObject) -> JniResult<JObject<'l>> {
    let class = env.get_object_class(activity)?;
    let intent = env.new_object(
        "android/content/Intent",
        "(Landroid/content/Context;Ljava/lang/Class;)V",
        &[JValue::Object(activity), JValue::Object(&class)],
    )?;
    // FLAG_ACTIVITY_NEW_TASK | FLAG_ACTIVITY_SINGLE_TOP
    call(
        env,
        &intent,
        "addFlags",
        "(I)Landroid/content/Intent;",
        &[JValue::Int(0x1000_0000 | 0x2000_0000)],
    )?;
    env.call_static_method(
        "android/app/PendingIntent",
        "getActivity",
        "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
        &[
            JValue::Object(activity),
            JValue::Int(0),
            JValue::Object(&intent),
            JValue::Int(PENDING_FLAGS),
        ],
    )?
    .l()
}

/// Decodes cover bytes to a `Bitmap` of at most about [`ART_SIZE`] pixels on
/// its longer side (sampled while decoding, so a huge cover is never fully
/// expanded). A null object when the bytes are no image.
fn decode_cover<'l>(env: &mut JNIEnv<'l>, bytes: &[u8]) -> JniResult<JObject<'l>> {
    let data = env.byte_array_from_slice(bytes)?;
    let length = bytes.len() as jint;
    let options = env.new_object("android/graphics/BitmapFactory$Options", "()V", &[])?;
    env.set_field(&options, "inJustDecodeBounds", "Z", true.into())?;
    decode_with(env, &data, length, &options)?;
    let width = env.get_field(&options, "outWidth", "I")?.i()?;
    let height = env.get_field(&options, "outHeight", "I")?.i()?;
    if width <= 0 || height <= 0 {
        return Ok(JObject::null());
    }
    let mut sample = 1;
    while width.max(height) / (sample * 2) >= ART_SIZE {
        sample *= 2;
    }
    env.set_field(&options, "inJustDecodeBounds", "Z", false.into())?;
    env.set_field(&options, "inSampleSize", "I", JValue::Int(sample))?;
    decode_with(env, &data, length, &options)
}

/// `BitmapFactory.decodeByteArray(data, 0, len, options)`.
fn decode_with<'l>(
    env: &mut JNIEnv<'l>,
    data: &JObject,
    length: jint,
    options: &JObject,
) -> JniResult<JObject<'l>> {
    env.call_static_method(
        "android/graphics/BitmapFactory",
        "decodeByteArray",
        "([BIILandroid/graphics/BitmapFactory$Options;)Landroid/graphics/Bitmap;",
        &[
            JValue::Object(data),
            JValue::Int(0),
            JValue::Int(length),
            JValue::Object(options),
        ],
    )?
    .l()
}

fn system_service<'l>(
    env: &mut JNIEnv<'l>,
    activity: &JObject,
    name: &str,
) -> JniResult<JObject<'l>> {
    let name = env.new_string(name)?;
    call(
        env,
        activity,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[JValue::Object(&name)],
    )
}

fn call<'l>(
    env: &mut JNIEnv<'l>,
    object: &JObject,
    name: &str,
    sig: &str,
    args: &[JValue],
) -> JniResult<JObject<'l>> {
    env.call_method(object, name, sig, args)?.l()
}

fn call_void(
    env: &mut JNIEnv,
    object: &JObject,
    name: &str,
    sig: &str,
    args: &[JValue],
) -> JniResult<()> {
    env.call_method(object, name, sig, args)?.v()
}

/// Logs a failed JNI sequence and clears the pending Java exception.
fn clear_on_error(env: &mut JNIEnv, what: &str, result: JniResult<()>) {
    if let Err(error) = result {
        tracing::warn!("{what} failed: {error}");
        let _ = env.exception_clear();
    }
}
