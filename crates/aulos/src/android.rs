// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The Android entry: `cargo apk` packages this crate's `cdylib` as a
//! `NativeActivity`, whose glue calls [`android_main`] on its own thread.

use android_activity::AndroidApp;

/// The JavaVM and the activity object of this process, set once by
/// [`android_main`] (raw pointers kept as integers so they are `Sync`).
static HOST: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();

/// The attached-thread JNI handles of [`HOST`].
fn host() -> Option<(jni::JavaVM, jni::objects::JObject<'static>)> {
    let (vm, activity) = *HOST.get()?;
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

/// The system bars and display cutout, in pixels: `[left, top, right, bottom]`.
/// The activity draws edge to edge, so the app keeps its content out of them.
/// The deprecated `getSystemWindowInset*` getters are the ones that exist on
/// every API level from 26. Asked of the window at most twice a second; `None` until it has insets.
pub fn system_insets() -> Option<[f32; 4]> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    static CACHE: Mutex<Option<(Instant, Option<[f32; 4]>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().ok()?;
    if let Some((at, value)) = *cache
        && at.elapsed() < Duration::from_millis(500)
    {
        return value;
    }
    let (vm, activity) = host()?;
    let mut env = vm.attach_current_thread().ok()?;
    let read = (|| -> jni::errors::Result<Option<[f32; 4]>> {
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
        Ok(Some([
            side("getSystemWindowInsetLeft")?,
            side("getSystemWindowInsetTop")?,
            side("getSystemWindowInsetRight")?,
            side("getSystemWindowInsetBottom")?,
        ]))
    })();
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

/// Starts the app inside the activity: private directories, logcat, a tokio
/// runtime, then `eframe` on the activity's window.
#[unsafe(no_mangle)]
fn android_main(android: AndroidApp) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let _ = HOST.set((
        android.vm_as_ptr() as usize,
        android.activity_as_ptr() as usize,
    ));
    let base = android.internal_data_path();
    if let Some(base) = &base {
        aulos_core::paths::set_android_base(base.clone());
    }
    if let Ok(logcat) = tracing_android::layer("Aulos") {
        tracing_subscriber::registry()
            .with(tracing_subscriber::EnvFilter::new("info"))
            .with(logcat)
            .init();
    }
    ensure_audio_permission(base.as_deref());
    aulos_core::i18n::init_from_system();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
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
