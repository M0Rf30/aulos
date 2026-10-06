// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

/// Bus name Aulos's MPRIS server claims once running (see
/// `aulos::mpris::mpris_stream`). Used both to detect an already-running
/// instance and to address it directly over D-Bus.
const MPRIS_BUS_NAME: &str = "org.mpris.MediaPlayer2.Aulos";

fn main() -> cosmic::iced::Result {
    #[cfg(feature = "tokio-console")]
    console_subscriber::init();

    #[cfg(not(feature = "tokio-console"))]
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Files passed on the command line -- via a file manager's "Open
    // With" (`Exec=aulos %U` in the desktop entry) or directly by the
    // user. Nonexistent paths are dropped here rather than failing later
    // inside the scanner.
    let open_paths = parse_args();

    // If another Aulos instance already owns the MPRIS bus name, hand the
    // files to it and exit instead of starting a second player -- every
    // playback route lives on a single `AppModel`/`Player`, so two
    // processes would each think they own the audio device.
    if !open_paths.is_empty() && hand_off_to_running_instance(&open_paths) {
        return Ok(());
    }

    // Carry over data/config/cache from the pre-rename "Lyra" install
    // before anything opens the library database or reads the config.
    aulos::migrate::migrate_legacy_dirs();

    // Get the system's preferred languages.
    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();

    // Enable localizations to be applied.
    aulos::i18n::init(&requested_languages);

    // Settings for configuring the application window and iced runtime.
    let settings = cosmic::app::Settings::default().size_limits(
        cosmic::iced::Limits::NONE
            .min_width(900.0)
            .min_height(600.0),
    );

    let flags = aulos::app::AppFlags { open_paths };

    // Starts the application's event loop.
    cosmic::app::run::<aulos::app::AppModel>(settings, flags)
}

/// Parses `argv[1..]` into existing filesystem paths, accepting both
/// plain paths and `file://` URIs (as passed by `Exec=aulos %U` per the
/// Desktop Entry Specification, or by some file managers' "Open With").
fn parse_args() -> Vec<PathBuf> {
    std::env::args_os()
        .skip(1)
        .filter_map(|arg| {
            let arg = arg.to_string_lossy().into_owned();
            let path = aulos::file_uri_to_path(&arg).unwrap_or_else(|| PathBuf::from(&arg));
            if path.exists() {
                Some(path)
            } else {
                tracing::warn!("Ignoring nonexistent file argument: {}", path.display());
                None
            }
        })
        .collect()
}

/// Hands `paths` to an already-running Aulos instance over D-Bus
/// (`org.mpris.MediaPlayer2.Player.OpenUri`) and raises its window,
/// instead of starting a second process. Returns `true` only when every
/// step succeeded; any failure (no running instance, no session bus, a
/// rejected call, ...) returns `false` so `main` falls through to a
/// normal cold start.
///
/// zbus's `tokio` integration needs a running Tokio reactor, which
/// doesn't exist yet this early in `main` (the real one only starts
/// inside `cosmic::app::run`) -- so this spins up and tears down a
/// throwaway single-threaded runtime just for the handoff.
fn hand_off_to_running_instance(paths: &[PathBuf]) -> bool {
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

    // Past this point another instance provably owns the bus name, so every
    // outcome returns `Ok(true)`: falling back to a cold start here would
    // spawn a duplicate player, possibly after some files were already
    // delivered. A failed call is logged and dropped instead.
    for path in paths {
        if let Err(e) = connection
            .call_method(
                Some(MPRIS_BUS_NAME),
                "/org/mpris/MediaPlayer2",
                Some("org.mpris.MediaPlayer2.Player"),
                "OpenUri",
                &(path_to_file_uri(path),),
            )
            .await
        {
            tracing::warn!("MPRIS OpenUri failed for {}: {e}", path.display());
        }
    }

    // Best-effort: the running instance may legitimately refuse to raise.
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

/// Builds a `file://` URI for `path`, canonicalizing it first so a
/// relative CLI argument still resolves correctly in the receiving
/// (already-running) process, which may have a different working
/// directory.
fn path_to_file_uri(path: &std::path::Path) -> String {
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let encoded = absolute
        .to_string_lossy()
        .split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/");
    format!("file://{encoded}")
}
