// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Desktop notifications on track change, with cover art
//! (`org.freedesktop.Notifications` over the session bus; Lollypop's
//! `notification.py`).
//!
//! Only shown while the Aulos window isn't focused — see
//! [`should_notify`]. Successive notifications replace the previous one
//! (`replaces_id`) instead of stacking up.

use crate::library::Track;
use std::collections::HashMap;
use std::path::PathBuf;
use zbus::zvariant::Value;

const APP_NAME: &str = "Aulos";
const DESKTOP_ENTRY: &str = "io.github.m0rf30.Aulos";
/// How long the bubble stays up (ms); the server may override.
const EXPIRE_MS: i32 = 5000;

/// Whether to pop a notification for a track change.
///
/// Only when the user wants them, music is actually playing, the window
/// isn't in front of the user already, and it's a different track from the
/// one last announced (repeat-one must not re-notify every loop).
pub fn should_notify(
    enabled: bool,
    window_focused: bool,
    playing: bool,
    track_key: &str,
    last_notified: Option<&str>,
) -> bool {
    enabled && playing && !window_focused && last_notified != Some(track_key)
}

/// Identity of a track for notification de-duplication (library id plus
/// source, so distinct radio stations/streams sharing id 0 still differ).
pub fn track_key(track: &Track) -> String {
    format!("{}|{}|{}", track.id, track.provider_id, track.source_uri)
}

/// `(summary, body)` for a track: the title on top, then
/// "artist — album" (either part may be missing).
pub fn notification_text(track: &Track) -> (String, String) {
    let summary = if track.title.trim().is_empty() {
        APP_NAME.to_string()
    } else {
        track.title.clone()
    };
    let artist = track.artist.trim();
    let album = track.album.trim();
    let body = match (artist.is_empty(), album.is_empty()) {
        (false, false) => format!("{artist} — {album}"),
        (false, true) => artist.to_string(),
        (true, false) => album.to_string(),
        (true, true) => String::new(),
    };
    (summary, body)
}

/// Escape the characters the notification spec's body markup treats
/// specially (servers may render a markup subset).
pub fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// File extension for raw cover bytes.
fn image_ext(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else {
        "jpg"
    }
}

/// Make a cover image available on disk for the notification server and
/// return its path: the already-loaded album cover bytes when we have them
/// (any provider), else the file's embedded art (local files only).
/// Blocking file I/O — call from `spawn_blocking`.
pub fn prepare_cover(track: &Track, cover_bytes: Option<&[u8]>) -> Option<PathBuf> {
    if let Some(bytes) = cover_bytes.filter(|b| !b.is_empty()) {
        let dir = dirs::cache_dir()?.join("aulos").join("notify");
        std::fs::create_dir_all(&dir).ok()?;
        // Keyed by content so the file can be reused and never half-read:
        // a different cover is simply a different file.
        let digest = md5::compute(bytes);
        let path = dir.join(format!("{digest:x}.{}", image_ext(bytes)));
        if !path.exists() {
            std::fs::write(&path, bytes).ok()?;
        }
        return Some(path);
    }
    let url = crate::mpris::extract_art_url(track.id, &track.path)?;
    crate::file_uri_to_path(&url)
}

/// Send (or replace, when `replaces_id != 0`) a track notification.
/// Returns the server-assigned id to pass as `replaces_id` next time.
pub async fn send(
    summary: &str,
    body: &str,
    image: Option<&std::path::Path>,
    replaces_id: u32,
) -> zbus::Result<u32> {
    let conn = zbus::Connection::session().await?;

    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("desktop-entry", Value::from(DESKTOP_ENTRY));
    // Don't pile into the notification history/tray for every track.
    hints.insert("transient", Value::from(true));
    hints.insert("category", Value::from("x-gnome.music"));
    let image_path = image.map(|p| p.to_string_lossy().into_owned());
    if let Some(path) = image_path.as_deref() {
        hints.insert("image-path", Value::from(path));
    }

    let reply = conn
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                APP_NAME,
                replaces_id,
                DESKTOP_ENTRY, // app_icon: falls back to the app's own icon
                summary,
                escape_markup(body),
                Vec::<&str>::new(),
                hints,
                EXPIRE_MS,
            ),
        )
        .await?;
    reply.body().deserialize::<u32>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn track(title: &str, artist: &str, album: &str) -> Track {
        Track {
            id: 1,
            path: PathBuf::from("/m/a.flac"),
            title: title.into(),
            artist: artist.into(),
            album_artist: String::new(),
            album: album.into(),
            genre: String::new(),
            track_number: 1,
            disc_number: 1,
            year: 0,
            duration: Duration::from_secs(1),
            bitrate: 0,
            sample_rate: 0,
            provider_id: "local".into(),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    #[test]
    fn notifies_only_when_unfocused_playing_enabled_and_new() {
        assert!(should_notify(true, false, true, "5", None));
        assert!(should_notify(true, false, true, "5", Some("4")));
        assert!(!should_notify(false, false, true, "5", None), "disabled");
        assert!(
            !should_notify(true, true, true, "5", None),
            "window focused"
        );
        assert!(!should_notify(true, false, false, "5", None), "not playing");
        assert!(
            !should_notify(true, false, true, "5", Some("5")),
            "same track again"
        );
    }

    #[test]
    fn text_combines_artist_and_album() {
        assert_eq!(
            notification_text(&track("Song", "Artist", "Album")),
            ("Song".to_string(), "Artist — Album".to_string())
        );
        assert_eq!(notification_text(&track("Song", "Artist", "")).1, "Artist");
        assert_eq!(notification_text(&track("Song", "", "Album")).1, "Album");
        assert_eq!(notification_text(&track("Song", " ", " ")).1, "");
        assert_eq!(notification_text(&track("  ", "A", "B")).0, "Aulos");
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(
            escape_markup("AC/DC <live> & more"),
            "AC/DC &lt;live&gt; &amp; more"
        );
    }

    #[test]
    fn image_extension_sniffs_png() {
        assert_eq!(image_ext(b"\x89PNG\r\n\x1a\nrest"), "png");
        assert_eq!(image_ext(b"\xff\xd8\xff"), "jpg");
    }

    #[test]
    fn prepared_cover_bytes_land_in_a_content_addressed_file() {
        let t = track("S", "A", "B");
        let bytes = b"\x89PNG\r\n\x1a\nfake-image-data-for-test";
        let p1 = prepare_cover(&t, Some(bytes)).expect("cache dir available");
        let p2 = prepare_cover(&t, Some(bytes)).expect("second call reuses");
        assert_eq!(p1, p2);
        assert_eq!(std::fs::read(&p1).unwrap(), bytes);
        assert_eq!(p1.extension().and_then(|e| e.to_str()), Some("png"));
        let _ = std::fs::remove_file(p1);
    }
}
