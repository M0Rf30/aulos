//! Aulos: an egui player on top of `aulos-core`. All behaviour lives in the
//! core [`Session`](aulos_core::session::Session); this crate runs the
//! effects and background streams it hands out on a tokio runtime, feeds every
//! resulting event back into it from `App::logic`, and paints the result.

// A release build on Windows is a GUI program: no console window behind it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod dialogs;
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
    let handle = runtime.handle().clone();

    let size = smoke::window_size().unwrap_or([1280.0, 800.0]);
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
        wgpu_options: low_memory_wgpu(),
        ..Default::default()
    };
    eframe::run_native(
        "Aulos",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, handle)))),
    )
}

/// Default wgpu setup with small memory blocks (`MemoryHints::MemoryUsage`):
/// about 160 MB less shared GPU memory than the performance default.
fn low_memory_wgpu() -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};
    let mut config = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        let previous = setup.device_descriptor.clone();
        setup.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut descriptor = previous(adapter);
            descriptor.memory_hints = eframe::wgpu::MemoryHints::MemoryUsage;
            descriptor
        });
    }
    config
}
