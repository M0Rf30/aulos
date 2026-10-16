//! Aulos: an egui player on top of `aulos-core`. All behaviour lives in the
//! core [`Session`](aulos_core::session::Session); this crate runs the
//! effects and background streams it hands out on a [`Spawner`] (a tokio
//! runtime natively, the browser's event loop on the web), feeds every
//! resulting event back into it from `App::logic`, and paints the result.
//!
//! Two entry points: a desktop window (`eframe::run_native`) and, on
//! `wasm32`, a canvas in a web page (`eframe::WebRunner`), built with Trunk.

// A release build on Windows is a GUI program: no console window behind it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod dialogs;
#[cfg(target_arch = "wasm32")]
mod media_session;
mod smoke;
mod theme;
mod views;
#[cfg(feature = "visualizer")]
mod viz;
mod widgets;

/// Caps glibc's per-thread malloc arenas and returns freed memory sooner:
/// decode bursts otherwise leave hundreds of MB resident.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn tune_allocator() {
    unsafe extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
    }
    const M_TRIM_THRESHOLD: i32 = -1;
    const M_MMAP_THRESHOLD: i32 = -3;
    const M_ARENA_MAX: i32 = -8;
    // SAFETY: plain glibc tunables, called before any other thread exists.
    unsafe {
        mallopt(M_ARENA_MAX, 2);
        mallopt(M_TRIM_THRESHOLD, 128 * 1024);
        mallopt(M_MMAP_THRESHOLD, 256 * 1024);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    tune_allocator();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    aulos_core::i18n::init_from_system();

    // `Session::bootstrap` builds providers that need a runtime handle; the
    // guard keeps one entered on this (the UI) thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        // The async work is I/O; CPU-bound work uses the blocking pool.
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let _guard = runtime.enter();
    let handle = aulos_core::task::Spawner::new(runtime.handle().clone());

    let size = smoke::window_size().unwrap_or([1280.0, 800.0]);
    let run = |backends| {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(size)
                .with_min_inner_size([420.0, 190.0])
                .with_title("Aulos")
                .with_app_id("io.github.m0rf30.Aulos")
                // X11 and desktops that don't resolve the icon from the
                // desktop file via the app id need it on the window itself.
                .with_icon(
                    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png"))
                        .unwrap_or_default(),
                ),
            // wgpu's default reserves 128-256 MB blocks; ~10 MB is ever used.
            wgpu_options: low_memory_wgpu(backends),
            ..Default::default()
        };
        let handle = handle.clone();
        eframe::run_native(
            "Aulos",
            options,
            Box::new(|cc| Ok(Box::new(app::App::new(cc, handle)))),
        )
    };
    // Vulkan alone keeps Mesa's GL stack (libgallium, LLVM; ~50 MB) from
    // loading; fall back to every backend when it has no adapter.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WGPU_BACKEND").is_none() {
        match run(Some(eframe::wgpu::Backends::VULKAN)) {
            Err(eframe::Error::Wgpu(err)) => {
                tracing::warn!("Vulkan unavailable ({err}); retrying with all backends");
            }
            other => return other,
        }
    }
    run(None)
}

#[cfg(not(target_arch = "wasm32"))]
/// Default wgpu setup (restricted to `backends` when given) with small memory blocks (`MemoryHints::MemoryUsage`):
/// about 160 MB less shared GPU memory than the performance default.
fn low_memory_wgpu(
    backends: Option<eframe::wgpu::Backends>,
) -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
    let mut config = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        if let Some(backends) = backends {
            setup.instance_descriptor.backends = backends;
        }
        let previous = setup.device_descriptor.clone();
        setup.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut descriptor = previous(adapter);
            descriptor.memory_hints = eframe::wgpu::MemoryHints::MemoryUsage;
            descriptor
        });
    }
    config
}

/// The `<canvas>` of `index.html` the app paints on.
#[cfg(target_arch = "wasm32")]
const CANVAS_ID: &str = "aulos_canvas";

/// The web entry: logs to the browser console, then starts the app on the
/// canvas of the page. The page's loading text is replaced by the error when
/// start-up fails (no WebGL/WebGPU, say).
#[cfg(target_arch = "wasm32")]
fn main() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::LevelFilter::INFO)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .without_time()
                .with_writer(tracing_web::MakeWebConsoleWriter::new()),
        )
        .init();
    aulos_core::i18n::init_from_system();

    wasm_bindgen_futures::spawn_local(async {
        use wasm_bindgen::JsCast;
        // The saved library and music folders come back before the session
        // opens its database.
        aulos_core::web_fs::init().await;
        // So do the presets the user imported for the visualizer.
        #[cfg(feature = "visualizer")]
        aulos_core::visualizer::source::restore().await;
        let document = web_sys::window()
            .and_then(|window| window.document())
            .expect("a page with a document");
        let canvas = document
            .get_element_by_id(CANVAS_ID)
            .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("the page has the canvas of index.html");
        let started = eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|cc| {
                    Ok(Box::new(app::App::new(
                        cc,
                        aulos_core::task::Spawner::browser(),
                    )))
                }),
            )
            .await;
        let loading = document.get_element_by_id("loading");
        match started {
            Ok(()) => {
                if let Some(loading) = loading {
                    loading.remove();
                }
            }
            Err(error) => {
                tracing::error!("Aulos failed to start: {error:?}");
                if let Some(loading) = loading {
                    loading.set_inner_html(&aulos_core::fl!("app-web-start-failed"));
                }
            }
        }
    });
}
