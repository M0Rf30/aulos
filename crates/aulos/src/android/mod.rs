// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The Android entry: `cargo apk` packages this crate's `cdylib` as a
//! `NativeActivity`, whose glue calls [`android_main`] on its own thread.

pub mod media;

use android_activity::AndroidApp;

/// The JavaVM and the activity object of this process, overwritten by every
/// [`android_main`] (a relaunch in the same process gets a new activity; raw
/// pointers kept as integers so they are `Sync`).
static HOST: std::sync::Mutex<Option<(usize, usize)>> = std::sync::Mutex::new(None);

/// The app's private files directory, set once by [`android_main`].
static BASE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// The attached-thread JNI handles of [`HOST`].
fn host() -> Option<(jni::JavaVM, jni::objects::JObject<'static>)> {
    let (vm, activity) = (*HOST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))?;
    // SAFETY: both pointers come from `android-activity`, which keeps the VM
    // and the activity alive for the whole process.
    unsafe {
        Some((
            jni::JavaVM::from_raw(vm as *mut _).ok()?,
            jni::objects::JObject::from_raw(activity as *mut _),
        ))
    }
}

/// How long the first launch waits for the audio permission dialog.
const PERMISSION_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// Makes sure the shared Music folder is readable: when the audio permission
/// is missing, asks for it, and on the first launch waits (at most
/// [`PERMISSION_WAIT`]) so the first library scan already sees the files.
/// Any JNI failure only means the user grants it in the system settings.
fn ensure_audio_permission(base: Option<&std::path::Path>) {
    use jni::objects::JValue;
    let asked = base.map(|b| b.join("permission_asked"));
    let first_time = asked.as_ref().is_some_and(|f| !f.exists());
    let Some((vm, activity)) = host() else { return };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let result = (|| -> jni::errors::Result<()> {
        let sdk = env
            .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
            .i()?;
        let name = if sdk >= 33 {
            "android.permission.READ_MEDIA_AUDIO"
        } else {
            "android.permission.READ_EXTERNAL_STORAGE"
        };
        let permission = env.new_string(name)?;
        let granted = |env: &mut jni::JNIEnv| -> jni::errors::Result<bool> {
            Ok(env
                .call_method(
                    &activity,
                    "checkSelfPermission",
                    "(Ljava/lang/String;)I",
                    &[JValue::Object(&permission)],
                )?
                .i()?
                == 0)
        };
        if granted(&mut env)? {
            return Ok(());
        }
        let class = env.find_class("java/lang/String")?;
        let list = env.new_object_array(1, class, &permission)?;
        env.call_method(
            &activity,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[JValue::Object(&list), JValue::Int(1)],
        )?;
        if !first_time {
            return Ok(());
        }
        let deadline = std::time::Instant::now() + PERMISSION_WAIT;
        while std::time::Instant::now() < deadline && !granted(&mut env)? {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!("audio permission request failed: {error}");
        let _ = env.exception_clear();
    }
    if let Some(file) = asked {
        let _ = std::fs::write(file, b"");
    }
}

/// Asks, once per install, for the notification permission (Android 13+; before
/// that notifications need none), so the playback notification shows. Called
/// when playback first starts, so the dialog explains itself. Never waits: the
/// service plays either way, only its notification stays hidden when denied.
fn ask_notification_permission() {
    use jni::objects::JValue;
    let asked = BASE
        .get()
        .map(|base| base.join("notification_permission_asked"));
    if asked.as_ref().is_none_or(|file| file.exists()) {
        return;
    }
    let Some((vm, activity)) = host() else { return };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let result = (|| -> jni::errors::Result<()> {
        let sdk = env
            .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
            .i()?;
        if sdk < 33 {
            return Ok(());
        }
        let permission = env.new_string("android.permission.POST_NOTIFICATIONS")?;
        let granted = env
            .call_method(
                &activity,
                "checkSelfPermission",
                "(Ljava/lang/String;)I",
                &[JValue::Object(&permission)],
            )?
            .i()?
            == 0;
        if granted {
            return Ok(());
        }
        let class = env.find_class("java/lang/String")?;
        let list = env.new_object_array(1, class, &permission)?;
        env.call_method(
            &activity,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[JValue::Object(&list), JValue::Int(2)],
        )?;
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!("notification permission request failed: {error}");
        let _ = env.exception_clear();
    }
    if let Some(file) = asked {
        let _ = std::fs::write(file, b"");
    }
}

/// The text on the system clipboard. winit's Android backend has no system
/// clipboard (egui keeps a private one), so a copy made in another app only
/// reaches a text field through this. `None` when the clipboard is empty or
/// holds no text.
pub fn clipboard_text() -> Option<String> {
    use jni::objects::{JObject, JString, JValue};
    let (vm, activity) = host()?;
    let mut env = vm.attach_current_thread().ok()?;
    let read = env.with_local_frame(16, |env| -> jni::errors::Result<Option<String>> {
        let service = env.new_string("clipboard")?;
        let manager = env
            .call_method(
                &activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service)],
            )?
            .l()?;
        let clip = env
            .call_method(
                &manager,
                "getPrimaryClip",
                "()Landroid/content/ClipData;",
                &[],
            )?
            .l()?;
        if clip.is_null() || env.call_method(&clip, "getItemCount", "()I", &[])?.i()? == 0 {
            return Ok(None);
        }
        let item = env
            .call_method(
                &clip,
                "getItemAt",
                "(I)Landroid/content/ClipData$Item;",
                &[JValue::Int(0)],
            )?
            .l()?;
        let text = env
            .call_method(
                &item,
                "coerceToText",
                "(Landroid/content/Context;)Ljava/lang/CharSequence;",
                &[JValue::Object(&activity)],
            )?
            .l()?;
        if text.is_null() {
            return Ok(None);
        }
        let text: JObject = env
            .call_method(&text, "toString", "()Ljava/lang/String;", &[])?
            .l()?;
        Ok(Some(env.get_string(&JString::from(text))?.into()))
    });
    match read {
        Ok(text) => text.filter(|t| !t.is_empty()),
        Err(error) => {
            tracing::warn!("reading the clipboard failed: {error}");
            let _ = env.exception_clear();
            None
        }
    }
}

/// Puts `text` on the system clipboard, so a copy made in Aulos can be pasted
/// into other apps.
pub fn set_clipboard_text(text: &str) {
    use jni::objects::JValue;
    let Some((vm, activity)) = host() else { return };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let result = env.with_local_frame(16, |env| -> jni::errors::Result<()> {
        let service = env.new_string("clipboard")?;
        let manager = env
            .call_method(
                &activity,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&service)],
            )?
            .l()?;
        let label = env.new_string("Aulos")?;
        let body = env.new_string(text)?;
        let clip = env
            .call_static_method(
                "android/content/ClipData",
                "newPlainText",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                &[JValue::Object(&label), JValue::Object(&body)],
            )?
            .l()?;
        env.call_method(
            &manager,
            "setPrimaryClip",
            "(Landroid/content/ClipData;)V",
            &[JValue::Object(&clip)],
        )?;
        Ok(())
    });
    if let Err(error) = result {
        tracing::warn!("writing the clipboard failed: {error}");
        let _ = env.exception_clear();
    }
}

/// Opens `url` in the user's browser with an `ACTION_VIEW` intent.
pub fn open_url(url: &str) -> Result<(), String> {
    use jni::objects::JValue;

    let (vm, activity) = host().ok_or("Could not open the browser: no activity")?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("Could not open the browser: {e}"))?;
    let result = env.with_local_frame(16, |env| -> jni::errors::Result<()> {
        let action = env.new_string("android.intent.action.VIEW")?;
        let uri_text = env.new_string(url)?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&uri_text)],
            )?
            .l()?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        env.call_method(
            &activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    });
    result.map_err(|e| {
        let _ = env.exception_clear();
        format!("Could not open the browser: {e}")
    })
}

/// The system bars and display cutout, in pixels: `[left, top, right, bottom]`.
/// The activity draws edge to edge, so the app keeps its content out of them.
/// `None` until the window has insets. See [`window_insets`].
pub fn system_insets() -> Option<[f32; 4]> {
    window_insets().map(|insets| insets.system)
}

/// The height the soft keyboard covers at the bottom of the window, in pixels
/// (0 while it is hidden or the window has no insets yet).
pub fn ime_bottom() -> f32 {
    window_insets().map_or(0.0, |insets| insets.ime_bottom)
}

/// What the window reports it is covered by, in pixels.
#[derive(Clone, Copy)]
struct WindowInsets {
    /// `[left, top, right, bottom]` of the bars and the cutout (the
    /// deprecated `getSystemWindowInset*` getters are the ones that exist on
    /// every API level from 26).
    system: [f32; 4],
    /// The soft keyboard's height: `getInsets(Type.ime())` on API 30+, before
    /// that what the system bottom inset adds to the stable one.
    ime_bottom: f32,
}

/// Asked of the window at most a few times a second (the keyboard shows and
/// hides within that time).
fn window_insets() -> Option<WindowInsets> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    static CACHE: Mutex<Option<(Instant, Option<WindowInsets>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().ok()?;
    if let Some((at, value)) = *cache
        && at.elapsed() < Duration::from_millis(150)
    {
        return value;
    }
    let (vm, activity) = host()?;
    let mut env = vm.attach_current_thread().ok()?;
    // This runs on the permanent UI thread: the frame frees the local
    // references of every poll.
    let read = env.with_local_frame(16, |env| -> jni::errors::Result<Option<WindowInsets>> {
        let window = env
            .call_method(&activity, "getWindow", "()Landroid/view/Window;", &[])?
            .l()?;
        let decor = env
            .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?
            .l()?;
        let insets = env
            .call_method(
                &decor,
                "getRootWindowInsets",
                "()Landroid/view/WindowInsets;",
                &[],
            )?
            .l()?;
        if insets.is_null() {
            return Ok(None);
        }
        let mut side = |name: &str| -> jni::errors::Result<f32> {
            Ok(env.call_method(&insets, name, "()I", &[])?.i()? as f32)
        };
        let system = [
            side("getSystemWindowInsetLeft")?,
            side("getSystemWindowInsetTop")?,
            side("getSystemWindowInsetRight")?,
            side("getSystemWindowInsetBottom")?,
        ];
        let stable_bottom = side("getStableInsetBottom")?;
        let sdk = env
            .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
            .i()?;
        let ime_bottom = if sdk >= 30 {
            let ime = env
                .call_static_method("android/view/WindowInsets$Type", "ime", "()I", &[])?
                .i()?;
            let ime = env
                .call_method(
                    &insets,
                    "getInsets",
                    "(I)Landroid/graphics/Insets;",
                    &[jni::objects::JValue::Int(ime)],
                )?
                .l()?;
            env.get_field(&ime, "bottom", "I")?.i()? as f32
        } else {
            (system[3] - stable_bottom).max(0.0)
        };
        Ok(Some(WindowInsets { system, ime_bottom }))
    });
    let value = match read {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("window insets unavailable: {error}");
            let _ = env.exception_clear();
            None
        }
    };
    *cache = Some((Instant::now(), value));
    value
}

/// Unpacks the MilkDrop presets bundled in the APK (`assets/presets`, the
/// files of `aulos-core/assets/presets`) into the user preset folder, where
/// the visualizer scans. A file is rewritten only when it is missing or its
/// size differs, so a launch costs a directory listing once everything is in
/// place, and presets the user added next to them are never touched. Failures
/// are logged: the visualizer then just has fewer presets.
#[cfg(feature = "visualizer")]
fn install_presets(android: &AndroidApp) {
    let Some(dest) = aulos_core::visualizer::presets::user_preset_dir() else {
        return;
    };
    let assets = android.asset_manager();
    let Some(names) = assets.open_dir(c"presets") else {
        tracing::warn!("the APK has no bundled presets");
        return;
    };
    if let Err(error) = std::fs::create_dir_all(&dest) {
        tracing::warn!("preset folder {}: {error}", dest.display());
        return;
    }
    for name in names {
        let Ok(file) = name.to_str() else { continue };
        let target = dest.join(file);
        let Ok(path) = std::ffi::CString::new(format!("presets/{file}")) else {
            continue;
        };
        let Some(mut asset) = assets.open(&path) else {
            continue;
        };
        let Ok(bytes) = asset.buffer() else { continue };
        let current = std::fs::metadata(&target).map(|m| m.len()).ok();
        if current == Some(bytes.len() as u64) {
            continue;
        }
        if let Err(error) = std::fs::write(&target, bytes) {
            tracing::warn!("preset {}: {error}", target.display());
        }
    }
}

/// Starts the app inside the activity: private directories, logcat, a tokio
/// runtime, then `eframe` on the activity's window.
#[unsafe(no_mangle)]
fn android_main(android: AndroidApp) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    *HOST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((
        android.vm_as_ptr() as usize,
        android.activity_as_ptr() as usize,
    ));
    let base = android.internal_data_path();
    if let Some(base) = &base {
        aulos_core::paths::set_android_base(base.clone());
        let _ = BASE.set(base.clone());
    }
    if let Ok(logcat) = tracing_android::layer("Aulos") {
        tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("info"))
            .with(logcat)
            .try_init()
            // A relaunch reuses the process, whose subscriber is already set.
            .ok();
    }
    ensure_audio_permission(base.as_deref());
    aulos_core::desktop::browser::set_url_opener(open_url);
    #[cfg(feature = "visualizer")]
    install_presets(&android);
    aulos_core::i18n::init_from_system();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    {
        let _guard = runtime.enter();
        let spawner = aulos_core::task::Spawner::new(runtime.handle().clone());

        let options = eframe::NativeOptions {
            android_app: Some(android),
            wgpu_options: crate::low_memory_wgpu(None),
            ..Default::default()
        };
        if let Err(error) = eframe::run_native(
            "Aulos",
            options,
            Box::new(|cc| Ok(Box::new(crate::app::App::new(cc, spawner)))),
        ) {
            tracing::error!("Aulos failed to start: {error}");
        }
    }
    // The activity is gone: the notification and the foreground service must
    // not outlive it (nothing would be left to drive playback).
    media::shutdown();
    // Dropping the runtime would wait for running scan/convert/tag jobs.
    runtime.shutdown_timeout(crate::RUNTIME_SHUTDOWN);
    // The activity pointer is dead from here on.
    *HOST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}
