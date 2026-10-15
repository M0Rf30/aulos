// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The browser's Media Session: the track in the OS media controls, the
//! lock screen and the hardware media keys, and their commands back.
//!
//! The web counterpart of the desktop MPRIS/SMTC backends. [`sync`] runs once
//! per frame from the app's logic: it publishes what the session's
//! [`MprisSnapshot`] says (title, artist, album, cover, play state, position)
//! and returns the commands the user gave through the browser's controls, as
//! the [`MprisCommand`]s the app already handles.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aulos_core::mpris::{MprisCommand, MprisSnapshot, MprisStatus, changed_beyond_ticking};
use aulos_core::session::Session;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Blob, BlobPropertyBag, Url};

// `web-sys` only has the Media Session API behind `--cfg web_sys_unstable_apis`,
// a build-wide flag; these few bindings are all that is used of it.
#[wasm_bindgen]
extern "C" {
    /// `navigator.mediaSession`.
    type MediaSession;

    #[wasm_bindgen(method, setter, js_name = metadata)]
    fn set_metadata(this: &MediaSession, metadata: Option<&MediaMetadata>);

    #[wasm_bindgen(method, setter, js_name = playbackState)]
    fn set_playback_state(this: &MediaSession, state: &str);

    #[wasm_bindgen(method, catch, js_name = setActionHandler)]
    fn set_action_handler(
        this: &MediaSession,
        action: &str,
        handler: Option<&js_sys::Function>,
    ) -> Result<(), JsValue>;

    #[wasm_bindgen(method, catch, js_name = setPositionState)]
    fn set_position_state(this: &MediaSession, state: &JsValue) -> Result<(), JsValue>;

    type MediaMetadata;

    #[wasm_bindgen(constructor, catch)]
    fn new(init: &JsValue) -> Result<MediaMetadata, JsValue>;
}

/// Sets `object[key] = value`.
fn put(object: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(object, &key.into(), value);
}

/// A number member of an action's details object.
fn number(details: &JsValue, key: &str) -> Option<f64> {
    js_sys::Reflect::get(details, &key.into()).ok()?.as_f64()
}

/// How far the seek-backward/forward buttons jump when the browser does not
/// say (seconds).
const DEFAULT_SEEK_SECS: f64 = 10.0;

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Publishes the session's playback state to the browser and returns the
/// commands it sent since the last call.
pub fn sync(ctx: &egui::Context, session: &mut Session) -> Vec<MprisCommand> {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.publish(ctx, session);
        let commands = std::mem::take(&mut *state.commands.borrow_mut());
        commands.into()
    })
}

/// One action handler the browser can be given or have taken away.
struct Action {
    action: &'static str,
    closure: Closure<dyn FnMut(JsValue)>,
    attached: bool,
}

#[derive(Default)]
struct State {
    /// `None` until first use, then the page's media session (absent in
    /// browsers without one).
    session: Option<JsValue>,
    started: bool,
    commands: Rc<RefCell<VecDeque<MprisCommand>>>,
    actions: Vec<Action>,
    /// What was last published, to publish changes only.
    last: Option<MprisSnapshot>,
    /// The cover currently shown: its key and the object URL of its bytes.
    art: Option<(String, String)>,
}

impl State {
    /// Finds the media session and installs the handlers that are always on.
    fn start(&mut self, ctx: &egui::Context) {
        self.started = true;
        self.session = web_sys::window()
            .and_then(|w| js_sys::Reflect::get(&w.navigator(), &"mediaSession".into()).ok())
            .filter(|session| session.is_object())
            .map(JsCast::unchecked_into);
        if self.session.is_none() {
            return;
        }
        let commands = &self.commands;
        let mut add = |action: &'static str, command: fn(&JsValue) -> Option<MprisCommand>| {
            let commands = Rc::clone(commands);
            let ctx = ctx.clone();
            let closure = Closure::<dyn FnMut(JsValue)>::new(move |details: JsValue| {
                if let Some(command) = command(&details) {
                    commands.borrow_mut().push_back(command);
                    ctx.request_repaint();
                }
            });
            self.actions.push(Action {
                action,
                closure,
                attached: false,
            });
        };
        add("play", |_| Some(MprisCommand::Play));
        add("pause", |_| Some(MprisCommand::Pause));
        add("stop", |_| Some(MprisCommand::Stop));
        add("previoustrack", |_| Some(MprisCommand::Previous));
        add("nexttrack", |_| Some(MprisCommand::Next));
        add("seekbackward", |d| {
            Some(MprisCommand::Seek(-seek_offset_us(d)))
        });
        add("seekforward", |d| {
            Some(MprisCommand::Seek(seek_offset_us(d)))
        });
        add("seekto", |d| {
            number(d, "seekTime").map(|secs| MprisCommand::SetPosition((secs * 1e6) as i64))
        });
        for action in ["play", "pause", "stop"] {
            self.set_attached(action, true);
        }
    }

    /// Gives the browser the handler of `action`, or takes it away: an action
    /// without a handler is not offered by the OS controls.
    fn set_attached(&mut self, action: &'static str, attached: bool) {
        let (Some(session), Some(entry)) = (
            self.session
                .as_ref()
                .map(JsCast::unchecked_ref::<MediaSession>),
            self.actions.iter_mut().find(|a| a.action == action),
        ) else {
            return;
        };
        if entry.attached == attached {
            return;
        }
        entry.attached = attached;
        let handler = attached.then(|| entry.closure.as_ref().unchecked_ref());
        // Browsers that do not know an action throw; it is simply not offered.
        let _ = session.set_action_handler(action, handler);
    }

    fn publish(&mut self, ctx: &egui::Context, session: &mut Session) {
        if !self.started {
            self.start(ctx);
        }
        let Some(media) = self.session.clone() else {
            return;
        };
        let media: &MediaSession = media.unchecked_ref();
        let art_url = self.art_url(session);
        let snapshot = session.media_snapshot(art_url);
        if self.last.as_ref() == Some(&snapshot) {
            return;
        }
        let previous = self.last.replace(snapshot.clone());
        let before = previous.as_ref();

        let track_shown = |s: &MprisSnapshot| s.can_play;
        if before.is_none_or(|p| {
            (&p.title, &p.artist, &p.album, p.track_id, &p.art_url)
                != (
                    &snapshot.title,
                    &snapshot.artist,
                    &snapshot.album,
                    snapshot.track_id,
                    &snapshot.art_url,
                )
        }) {
            set_metadata_of(media, track_shown(&snapshot).then_some(&snapshot));
        }
        if before.is_none_or(|p| p.status != snapshot.status) {
            media.set_playback_state(match snapshot.status {
                MprisStatus::Playing => "playing",
                MprisStatus::Paused => "paused",
                MprisStatus::Stopped => "none",
            });
        }
        if before.is_none_or(|p| {
            (p.can_go_next, p.can_go_previous, p.can_seek)
                != (
                    snapshot.can_go_next,
                    snapshot.can_go_previous,
                    snapshot.can_seek,
                )
        }) {
            self.set_attached("nexttrack", snapshot.can_go_next);
            self.set_attached("previoustrack", snapshot.can_go_previous);
            for action in ["seekbackward", "seekforward", "seekto"] {
                self.set_attached(action, snapshot.can_seek);
            }
        }
        if before.is_none_or(|p| changed_beyond_ticking(p, &snapshot)) {
            set_position(media, &snapshot);
        }
    }

    /// The object URL of the current track's cover, once its bytes are
    /// loaded (the app's art pass requests them); frees the previous one.
    fn art_url(&mut self, session: &mut Session) -> Option<String> {
        let Some(track) = session.playback.current_track.as_ref() else {
            self.drop_art();
            return None;
        };
        let key = track.cover_key();
        if let Some((shown, url)) = &self.art
            && *shown == key
        {
            return Some(url.clone());
        }
        let bytes = session.cover_bytes.get(&key)?;
        self.drop_art();
        let url = object_url(&bytes)?;
        self.art = Some((key, url.clone()));
        Some(url)
    }

    fn drop_art(&mut self) {
        if let Some((_, url)) = self.art.take() {
            let _ = Url::revoke_object_url(&url);
        }
    }
}

/// How far the browser asked to seek, in microseconds.
fn seek_offset_us(details: &JsValue) -> i64 {
    (number(details, "seekOffset").unwrap_or(DEFAULT_SEEK_SECS) * 1e6) as i64
}

/// Shows `track` (or clears the metadata).
fn set_metadata_of(media: &MediaSession, track: Option<&MprisSnapshot>) {
    let Some(track) = track else {
        media.set_metadata(None);
        return;
    };
    let init = js_sys::Object::new();
    put(&init, "title", &track.title.as_str().into());
    put(&init, "artist", &track.artist.as_str().into());
    put(&init, "album", &track.album.as_str().into());
    let artwork = js_sys::Array::new();
    if let Some(url) = &track.art_url {
        let image = js_sys::Object::new();
        put(&image, "src", &url.as_str().into());
        artwork.push(&image);
    }
    put(&init, "artwork", &artwork);
    match MediaMetadata::new(&init) {
        Ok(metadata) => media.set_metadata(Some(&metadata)),
        Err(e) => tracing::warn!("media session metadata refused: {e:?}"),
    }
}

/// Tells the browser where the track is, so its controls can show and
/// extrapolate the position. Needs a known length (and a position inside it).
fn set_position(media: &MediaSession, snapshot: &MprisSnapshot) {
    if !snapshot.can_play || snapshot.length_us <= 0 || !snapshot.can_seek {
        return;
    }
    let duration = snapshot.length_us as f64 / 1e6;
    let state = js_sys::Object::new();
    put(&state, "duration", &duration.into());
    put(&state, "playbackRate", &1.0.into());
    let position = (snapshot.position_us.max(0) as f64 / 1e6).min(duration);
    put(&state, "position", &position.into());
    let _ = media.set_position_state(&state);
}

/// A `blob:` URL of the image `bytes`.
fn object_url(bytes: &[u8]) -> Option<String> {
    let parts = js_sys::Array::new();
    parts.push(&js_sys::Uint8Array::from(bytes));
    let options = BlobPropertyBag::new();
    options.set_type(image_mime(bytes));
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &options).ok()?;
    Url::create_object_url_with_blob(&blob).ok()
}

/// The MIME type of an image by its first bytes.
fn image_mime(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [b'G', b'I', b'F', ..] => "image/gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "image/webp",
        _ => "image/jpeg",
    }
}
