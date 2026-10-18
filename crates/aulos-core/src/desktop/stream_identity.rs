// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! How Aulos shows up in PipeWire / PulseAudio mixers (pavucontrol, Helvum,
//! the desktop volume panel). cpal opens the sound card through ALSA, and the
//! ALSA PipeWire and PulseAudio plugins name the stream after the executable
//! ("ALSA plug-in [aulos]") with no icon. Both plugins let the environment
//! override that, so [`apply`] sets the application name and icon there.

/// Display name of the stream.
#[cfg(target_os = "linux")]
const NAME: &str = "Aulos";

/// Makes the audio streams this process opens appear as "Aulos" with the app
/// icon. Variables the user already set are left alone.
///
/// Must run at startup, before any thread exists and before the first audio
/// stream is opened (the plugins read the environment when a stream opens).
/// A no-op outside Linux.
pub fn apply() {
    #[cfg(target_os = "linux")]
    {
        let icon = crate::config::APP_ID;
        // PipeWire's own ALSA plugin: SPA-JSON properties.
        set_unless_present(
            "PIPEWIRE_ALSA",
            &format!(
                "{{ application.name = {NAME} application.icon-name = {icon} \
                 node.name = {NAME} node.description = {NAME} }}"
            ),
        );
        // alsa-plugins' PulseAudio plugin (and any libpulse client).
        set_unless_present(
            "PULSE_PROP_OVERRIDE",
            &format!("application.name={NAME} application.icon_name={icon}"),
        );
    }
}

#[cfg(target_os = "linux")]
fn set_unless_present(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: `apply` is documented to run before the process spawns
        // threads, so nothing reads or writes the environment concurrently.
        unsafe { std::env::set_var(key, value) };
    }
}
