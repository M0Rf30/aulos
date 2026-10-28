// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The OS media-session interface: media keys, "now playing" panels and
//! shell applets.
//!
//! The platform-neutral half lives here: the [`MprisCommand`]s a client can
//! send, the [`MprisSnapshot`] of playback state the app publishes, and the
//! [`MprisHandle`] `Session` holds. Each platform has its own backend behind
//! [`mpris_stream`]:
//!
//! - Linux: an MPRIS2 D-Bus server (`org.mpris.MediaPlayer2`, via
//!   `mpris-server`), shown by the COSMIC/GNOME/KDE media applets;
//! - Windows: System Media Transport Controls; macOS: the Now Playing center
//!   and remote commands (both via `souvlaki`);
//! - anything else (the web build): no integration, the stream is empty.
//!
//! This module owns nothing about playback itself: [`mpris_stream`] starts
//! the backend and yields [`MprisEvent`]s exactly like the other background
//! streams. A backend only ever reads the last-published
//! [`MprisSnapshot`] and forwards every client request as an
//! [`MprisCommand`], which `Session::apply_mpris_command` maps onto the real
//! playback methods.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use futures::Stream;
use parking_lot::Mutex;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod backend;
#[cfg(any(target_os = "windows", target_os = "macos"))]
#[path = "native.rs"]
mod backend;
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
#[path = "none.rs"]
mod backend;

/// A command originating from an MPRIS client (media keys, shell applet,
/// `playerctl`, …), forwarded to `app.rs` for handling.
#[derive(Debug, Clone, PartialEq)]
pub enum MprisCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Relative seek, in microseconds (negative seeks backward).
    Seek(i64),
    /// Absolute seek target, in microseconds.
    SetPosition(i64),
    SetVolume(f64),
    Shuffle(bool),
    Loop(LoopMode),
    Raise,
    Quit,
    /// `OpenUri` request from an MPRIS client. Only `file://` URIs are
    /// actually playable; see `Message::OpenFiles`/`file_uri_to_path` in
    /// `app.rs`/`lib.rs`.
    OpenUri(String),
}

/// Repeat/loop mode, mirroring `mpris_server::LoopStatus` without leaking
/// that crate's type into `app.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopMode {
    #[default]
    None,
    Track,
    Playlist,
}

/// Coarse playback state, mirroring `mpris_server::PlaybackStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MprisStatus {
    #[default]
    Stopped,
    Playing,
    Paused,
}

/// Everything the D-Bus side needs to answer property getters. Rebuilt by
/// `AppModel::publish_mpris` on every relevant state change (and, cheaply,
/// on every playback tick — [`MprisHandle::publish`] diffs internally).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MprisSnapshot {
    pub status: MprisStatus,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub track_id: i64,
    pub length_us: i64,
    pub position_us: i64,
    pub art_url: Option<String>,
    pub volume: f64,
    pub shuffle: bool,
    pub loop_mode: LoopMode,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_seek: bool,
    pub can_play: bool,
}

/// Item yielded by [`mpris_stream`]: first a one-shot `Ready` carrying the
/// handle the app should hold onto, then a `Command` per incoming D-Bus
/// method call.
#[derive(Debug, Clone)]
pub enum MprisEvent {
    Ready(MprisHandle),
    Command(MprisCommand),
}

/// The native window the media session attaches to. Windows' transport
/// controls are bound to a window; the other backends ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeWindow(
    /// The raw `HWND` on Windows.
    pub usize,
);

/// A change of the published state, handed to the backend task.
#[cfg_attr(
    any(target_arch = "wasm32", target_os = "android"),
    allow(
        dead_code,
        reason = "only the desktop backends publish media-session state"
    )
)]
struct Update {
    previous: MprisSnapshot,
    next: MprisSnapshot,
}

/// State shared between the app-facing [`MprisHandle`] and the backend.
struct SharedState {
    snapshot: Mutex<MprisSnapshot>,
    updates: futures::channel::mpsc::UnboundedSender<Update>,
    /// Per-track `file://` cover art URL cache. Aulos keeps cover art as
    /// BLOBs in its library database rather than as loose files on disk, but
    /// media-session clients need an actual file to read pixels from — see
    /// [`MprisHandle::cached_art_url`]/[`MprisHandle::cache_art_url`].
    art_cache: Mutex<HashMap<String, Option<String>>>,
}

impl SharedState {
    /// A fresh shared state and the receiving end of its update channel.
    #[cfg_attr(
        any(target_arch = "wasm32", target_os = "android"),
        allow(
            dead_code,
            reason = "only the desktop backends publish media-session state"
        )
    )]
    fn new() -> (Arc<Self>, futures::channel::mpsc::UnboundedReceiver<Update>) {
        let (updates, updates_rx) = futures::channel::mpsc::unbounded();
        let shared = Arc::new(Self {
            snapshot: Mutex::new(MprisSnapshot::default()),
            updates,
            art_cache: Mutex::new(HashMap::new()),
        });
        (shared, updates_rx)
    }
}

/// Handle held by the `Session` to push fresh playback state into the
/// running media-session backend.
#[derive(Clone)]
pub struct MprisHandle {
    shared: Arc<SharedState>,
}

impl std::fmt::Debug for MprisHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MprisHandle").finish()
    }
}

impl MprisHandle {
    /// Stores `snapshot` and, only if it differs from the previously
    /// published one in more than the ticking position, hands the change to
    /// the backend. Cheap no-op when nothing changed, so callers may call
    /// this unconditionally on every UI tick.
    pub fn publish(&self, snapshot: MprisSnapshot) {
        let previous = {
            let mut guard = self.shared.snapshot.lock();
            if *guard == snapshot {
                return;
            }
            std::mem::replace(&mut *guard, snapshot.clone())
        };
        if !changed_beyond_ticking(&previous, &snapshot) {
            return;
        }
        // The receiving end lives inside the backend; if it never came up
        // (or the app is shutting down) this send is simply dropped.
        let _ = self.shared.updates.unbounded_send(Update {
            previous,
            next: snapshot,
        });
    }

    /// Returns the cached cover-art URL for `track_id`, if resolution has
    /// already completed. `Some(None)` means resolution finished but the
    /// track has no embedded art; `None` means it hasn't been resolved yet,
    /// and the caller should resolve it off-thread (see [`extract_art_url`])
    /// and record the outcome via [`Self::cache_art_url`].
    pub fn cached_art_url(&self, art_key: &str) -> Option<Option<String>> {
        self.shared.art_cache.lock().get(art_key).cloned()
    }

    /// Records a resolved cover-art URL for `art_key` (see [`art_key`]),
    /// populating the cache read by [`Self::cached_art_url`].
    pub fn cache_art_url(&self, art_key: String, url: Option<String>) {
        self.shared.art_cache.lock().insert(art_key, url);
    }
}

/// Identity of a track for the cover-art cache. Local tracks are told apart
/// by row id; every other track (remote `id: 0`, podcast episodes `id: -1`,
/// possibly with an empty path while streaming) by provider, path and
/// source URI.
#[must_use]
pub fn art_key(track: &crate::library::Track) -> String {
    if &*track.provider_id == "local" {
        format!("local\u{0}{}", track.id)
    } else {
        format!(
            "{}\u{0}{}\u{0}{}",
            track.provider_id,
            track.path.display(),
            track.source_uri
        )
    }
}

/// [`extract_art_url`] off the caller's thread (it parses tags and writes to
/// an on-disk cache synchronously); the event is the resolved URL, if any.
pub fn resolve_art_url(
    art_key: String,
    path: std::path::PathBuf,
) -> crate::task::Effect<Option<String>> {
    crate::task::Effect::future(async move {
        crate::task::spawn_blocking(move || extract_art_url(&art_key, &path))
            .await
            .unwrap_or(None)
    })
}

/// Extracts embedded cover art for `path` and writes it to a small on-disk
/// cache under the user's cache directory, returning a `file://` URL to it.
/// This is the only disk-writing cover art cache in Aulos (the library's own
/// cache stores album art as database BLOBs, which other processes can't
/// read directly) and is scoped entirely to the media session's needs.
pub fn extract_art_url(art_key: &str, path: &Path) -> Option<String> {
    let bytes = crate::library::CoverArt::extract_from_file(path)?;
    let ext = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "png"
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else {
        "jpg"
    };
    // Keyed by content as well as track key: ids can be reused (rescans,
    // other providers) and the file must not serve another cover's art.
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    };
    let dir = crate::paths::cache_dir().join("mpris");
    let file_path = dir.join(art_file_name(art_key, hash, ext));
    if !file_path.exists() {
        // Atomic, so a crash never leaves a truncated cover that `exists()`
        // would trust.
        crate::fs_atomic::write(&file_path, &bytes, false).ok()?;
    }
    crate::fs_atomic::prune_dir(&dir, &file_path, crate::fs_atomic::COVER_CACHE_KEEP);
    crate::path_to_file_uri(&file_path)
}

/// Filesystem-safe cache file name: the key is hashed, never embedded.
fn art_file_name(art_key: &str, content_hash: u64, ext: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    art_key.hash(&mut hasher);
    format!("{:016x}-{content_hash:016x}.{ext}", hasher.finish())
}

/// Whether the transition from `previous` to `next` is worth telling a
/// backend about: any field but the position changed, or the position
/// jumped (see [`seek_target`]). Plain ticks of a playing track are not:
/// the OS extrapolates them.
pub fn changed_beyond_ticking(previous: &MprisSnapshot, next: &MprisSnapshot) -> bool {
    let mut ticked = previous.clone();
    ticked.position_us = next.position_us;
    ticked != *next || seek_target(previous, next).is_some()
}

/// Decides whether a seek is warranted for the transition from `previous`
/// to `next`, returning the new position in microseconds. The MPRIS spec
/// leaves the exact trigger condition to implementations ("the track
/// position has changed in a way that is inconsistent with the current
/// playing state"); we treat any position jump beyond normal tick cadence
/// while playing, or any position change at all while paused/stopped, as an
/// explicit seek.
fn seek_target(previous: &MprisSnapshot, next: &MprisSnapshot) -> Option<i64> {
    if previous.track_id != next.track_id {
        // A new track starting somewhere other than the beginning (e.g.
        // resuming a podcast episode) is worth announcing explicitly.
        return (next.position_us > 0).then_some(next.position_us);
    }

    let delta = next.position_us - previous.position_us;
    if previous.status == MprisStatus::Playing && next.status == MprisStatus::Playing {
        // Generous slack for tick cadence (300-500ms) and rounding.
        const TOLERANCE_US: i64 = 1_500_000;
        if !(0..=TOLERANCE_US).contains(&delta) {
            return Some(next.position_us);
        }
        return None;
    }

    (delta != 0).then_some(next.position_us)
}

/// Starts the media-session backend and turns it into a stream matching
/// the other background streams. Yields exactly one `MprisEvent::Ready`
/// first, then an `MprisEvent::Command` per client request.
///
/// If the platform has no media session available (no D-Bus session bus, no
/// window handle on Windows, the web), logs and ends the stream quietly:
/// Aulos keeps running without media-key/applet integration.
///
/// `window` is the app's native window; only Windows needs it.
pub fn mpris_stream(window: Option<NativeWindow>) -> impl Stream<Item = MprisEvent> {
    backend::stream(window)
}

#[cfg(test)]
mod art_key_tests {
    use super::*;
    use crate::library::Track;
    use std::path::PathBuf;
    use std::time::Duration;

    fn track(provider: &str, id: i64, path: &str, source_uri: &str) -> Track {
        Track {
            id,
            path: PathBuf::from(path),
            title: String::new(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 0,
            duration: Duration::ZERO,
            bitrate: 0,
            sample_rate: 0,
            provider_id: provider.into(),
            source_uri: source_uri.into(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
            bit_depth: 0,
            release_type: String::new(),
            suffix: String::new(),
            added_at: 0,
            favorited_at: 0,
        }
    }

    #[test]
    fn podcast_episodes_and_remote_tracks_get_distinct_keys() {
        let e1 = track("podcast", -1, "", "https://x/1.mp3");
        let e2 = track("podcast", -1, "", "https://x/2.mp3");
        assert_ne!(art_key(&e1), art_key(&e2));
        let r1 = track("subsonic", 0, "song-1", "subsonic://s/song-1");
        let r2 = track("subsonic", 0, "song-2", "subsonic://s/song-2");
        assert_ne!(art_key(&r1), art_key(&r2));
        let l1 = track("local", 1, "/a", "/a");
        let l2 = track("local", 2, "/b", "/b");
        assert_ne!(art_key(&l1), art_key(&l2));
    }

    #[test]
    fn art_file_names_are_filesystem_safe_and_key_specific() {
        let key = "podcast\u{0}\u{0}https://x/1.mp3?a=b/c";
        let name = art_file_name(key, 7, "jpg");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-.".contains(c))
        );
        assert_ne!(name, art_file_name("other", 7, "jpg"));
        assert_eq!(name, art_file_name(key, 7, "jpg"));
    }
}
