// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Embeds the application icon and version information into the Windows
//! executable, so Explorer, the taskbar and the Start menu show Aulos' icon.
//! Does nothing on other targets.
//!
//! `assets/aulos.ico` is rendered from the scalable icon in
//! `resources/icons/hicolor/scalable/apps/`; regenerate it with
//! `rsvg-convert` at 16–256 px and `magick … aulos.ico` when the artwork
//! changes.

fn main() {
    println!("cargo:rerun-if-changed=assets/aulos.ico");
    #[cfg(windows)]
    embed_resources();
}

/// Only compiled on a Windows host: `winresource` drives `rc.exe`/`windres`.
/// (A Linux-to-Windows cross build ships without the embedded icon.)
#[cfg(windows)]
fn embed_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/aulos.ico")
        .set("ProductName", "Aulos")
        .set("FileDescription", "Aulos music player");
    if let Err(e) = res.compile() {
        println!("cargo:warning=could not embed the Windows icon: {e}");
    }
}
