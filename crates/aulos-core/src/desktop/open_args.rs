// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Files handed to the process at launch ("Open With" in a file manager via
//! `Exec=aulos %U`, or the command line) and their forwarding to an Aulos
//! that is already running.

use std::ffi::OsString;
use std::path::PathBuf;

/// Parses command-line arguments (without the program name) into existing
/// paths. Plain paths and `file://` URIs are accepted; anything that does not
/// exist is dropped with a warning.
#[must_use]
pub fn parse_paths(args: impl IntoIterator<Item = OsString>) -> Vec<PathBuf> {
    args.into_iter()
        .filter_map(|arg| {
            let arg = arg.to_string_lossy().into_owned();
            let path = crate::file_uri_to_path(&arg).unwrap_or_else(|| PathBuf::from(&arg));
            if path.exists() {
                Some(path)
            } else {
                tracing::warn!("Ignoring nonexistent file argument: {}", path.display());
                None
            }
        })
        .collect()
}

/// The paths named on this process's command line.
#[must_use]
pub fn from_env() -> Vec<PathBuf> {
    parse_paths(std::env::args_os().skip(1))
}

/// Bus name the running instance's MPRIS server owns.
#[cfg(target_os = "linux")]
const MPRIS_BUS_NAME: &str = "org.mpris.MediaPlayer2.Aulos";

/// Hands `paths` to an already-running instance (MPRIS `OpenUri`, then
/// `Raise`) so a second player never starts. `true` when another instance
/// owns the bus name and took over; any failure before that point returns
/// `false` and the caller starts normally.
#[cfg(target_os = "linux")]
#[must_use]
pub fn hand_off_to_running_instance(paths: &[PathBuf]) -> bool {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    match runtime.block_on(try_hand_off(paths)) {
        Ok(handed_off) => handed_off,
        Err(e) => {
            tracing::warn!("Could not hand off files to a running Aulos instance: {e}");
            false
        }
    }
}

/// No other instance can be reached on this platform.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn hand_off_to_running_instance(_paths: &[PathBuf]) -> bool {
    false
}

#[cfg(target_os = "linux")]
async fn try_hand_off(paths: &[PathBuf]) -> zbus::Result<bool> {
    let connection = zbus::Connection::session().await?;
    let bus_name = zbus::names::BusName::from_static_str(MPRIS_BUS_NAME)
        .expect("MPRIS_BUS_NAME is a valid well-known D-Bus name");
    let owned = zbus::fdo::DBusProxy::new(&connection)
        .await?
        .name_has_owner(bus_name)
        .await?;
    if !owned {
        return Ok(false);
    }
    // Another instance provably owns the name: a failed call is logged, never
    // a reason to start a duplicate player.
    for path in paths {
        let Some(uri) = crate::path_to_file_uri(path) else {
            tracing::warn!("Cannot express {} as a URI", path.display());
            continue;
        };
        if let Err(e) = connection
            .call_method(
                Some(MPRIS_BUS_NAME),
                "/org/mpris/MediaPlayer2",
                Some("org.mpris.MediaPlayer2.Player"),
                "OpenUri",
                &(uri,),
            )
            .await
        {
            tracing::warn!("MPRIS OpenUri failed for {}: {e}", path.display());
        }
    }
    if let Err(e) = connection
        .call_method(
            Some(MPRIS_BUS_NAME),
            "/org/mpris/MediaPlayer2",
            Some("org.mpris.MediaPlayer2"),
            "Raise",
            &(),
        )
        .await
    {
        tracing::debug!("MPRIS Raise failed: {e}");
    }
    Ok(true)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn accepts_paths_and_file_uris_and_drops_missing() {
        let dir = std::env::temp_dir().join(format!("aulos-open-args-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a b.flac");
        std::fs::write(&file, b"x").unwrap();
        let uri = crate::path_to_file_uri(&file).unwrap();
        let args = [
            file.clone().into_os_string(),
            OsString::from(uri),
            OsString::from("/definitely/not/here.mp3"),
        ];
        assert_eq!(parse_paths(args), vec![file.clone(), file]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
