//! Desktop and web entry; everything lives in the library (`lib.rs`).

// A release build on Windows is a GUI program: no console window behind it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    aulos_app::run()
}

#[cfg(target_arch = "wasm32")]
fn main() {
    aulos_app::run_web();
}
