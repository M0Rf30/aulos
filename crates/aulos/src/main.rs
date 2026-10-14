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

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    aulos_core::i18n::init_from_system();

    // `Session::bootstrap` builds providers that need a runtime handle; the
    // guard keeps one entered on this (the UI) thread.
    let runtime = tokio::runtime::Builder::new_multi_thread()
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
        ..Default::default()
    };
    eframe::run_native(
        "Aulos",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, handle)))),
    )
}
