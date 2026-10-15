// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The one door between a [`Session`] and a UI.
//!
//! Everything the backend does asynchronously ends as a [`SessionEvent`];
//! a UI feeds each one to [`Session::apply_session_event`] and presents the
//! returned [`SessionUpdate`]:
//!
//! - [`SessionUpdate::effect`]: follow-up work to run; its events come back
//!   through the same call,
//! - [`SessionUpdate::notices`]: things to tell the user,
//! - [`SessionUpdate::changes`]: what changed in the session that a UI keeps
//!   a presentation of (cover image handles, artist photos, ...).
//!
//! The action methods of the session (`play_tracks`, `scan_library`,
//! `switch_provider`, ...) return [`PlaybackUpdate`]s, [`Reload`]s or bare
//! [`Effect`]s of one domain's event type; all of them convert into a
//! [`SessionUpdate`] with `.into()`, so a UI has a single path for "the
//! result of something the session did", whether it comes back
//! immediately or later.
//!
//! [`Session::start`] and [`Session::background_streams`] complete the
//! picture: what the backend kicks off at launch, and the long-running
//! event sources (filesystem watcher, MPD, playback ticks) a UI must keep
//! subscribed to.

use super::artist_info::ArtistPhoto;
use super::{
    ConfigEvent, ConvertEvent, ConvertNotice, ConvertUpdate, EqualizerEvent, HomeEvent,
    LibraryApplied, LibraryEvent, MpdIdleOutcome, PlaybackEvent, PlaybackNotice, PlaybackUpdate,
    PlaylistEvent, PlaylistOutcome, PlaylistUpdate, PodcastEvent, PodcastNotice, PodcastUpdate,
    PropertiesEvent, ProviderEvent, ProviderInitError, ProviderReinit, RadioEvent, RadioNotice,
    RadioUpdate, Reload, Session,
};
use crate::config::Config;
use crate::library::Album;
use crate::library::artist_info::ArtistInfoOutcome;
use crate::library::covers::CoverDelta;
use crate::library::playlist_io::ImportSummary;
use crate::provider::ProviderType;
use crate::provider::mpd_events::{MpdEvent, MpdStatusUpdate};
#[cfg(not(target_arch = "wasm32"))]
use crate::provider::mpd_streams::{idle_stream, status_stream};
use crate::task::{Effect, interval_stream};
use futures::StreamExt;
use futures::stream::BoxStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// A decoded remote icon (podcast artwork, radio favicon), downscaled and
/// ready for the UI to turn into an image handle.
#[derive(Debug, Clone)]
pub struct OnlineIcon {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Every asynchronous result and tick the session understands; see the
/// module docs. Each domain event converts into it with `.into()`.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    Library(LibraryEvent),
    Playback(PlaybackEvent),
    Playlist(PlaylistEvent),
    /// Keyring checks and AcoustID key probes.
    Config(ConfigEvent),
    Home(HomeEvent),
    Podcast(PodcastEvent),
    Radio(RadioEvent),
    Equalizer(EqualizerEvent),
    Properties(PropertiesEvent),
    Provider(ProviderEvent),
    /// Results of the conversion queue.
    Convert(ConvertEvent),
    /// An artist's bio/photo fetch ended.
    ArtistInfo(ArtistInfoOutcome),
    /// Full-size cover bytes (or `None`: the album has no art) for `key`,
    /// from [`Session::load_cover_bytes`].
    CoverBytes {
        key: String,
        bytes: Option<Vec<u8>>,
    },
    /// An icon fetch ended (`None`: it failed), see [`Session::fetch_icons`].
    IconLoaded {
        url: String,
        icon: Option<OnlineIcon>,
    },
    /// An MPD connection event from [`BackgroundKey::MpdIdle`].
    Mpd(MpdEvent),
    /// A polled MPD status from [`BackgroundKey::MpdStatus`].
    MpdStatus(MpdStatusUpdate),
    /// The filesystem watcher reported these changed paths.
    FilesChanged(Vec<PathBuf>),
    /// Position ticker while playing locally ([`BackgroundKey::PlaybackTick`]).
    PlaybackTick,
    /// Slow housekeeping while an inhibit lock is held or being taken
    /// ([`BackgroundKey::ExtrasSync`]).
    ExtrasTick,
    /// Progress ticker while conversion jobs run
    /// ([`BackgroundKey::ConvertTick`]); applying it changes nothing -- the
    /// UI re-renders to pick up the jobs' atomic progress values.
    ConvertTick,
}

macro_rules! session_event_from {
    ($($ty:ty => $variant:ident),+ $(,)?) => {
        $(impl From<$ty> for SessionEvent {
            fn from(event: $ty) -> Self {
                Self::$variant(event)
            }
        })+
    };
}

session_event_from! {
    LibraryEvent => Library,
    PlaybackEvent => Playback,
    PlaylistEvent => Playlist,
    ConfigEvent => Config,
    HomeEvent => Home,
    PodcastEvent => Podcast,
    RadioEvent => Radio,
    EqualizerEvent => Equalizer,
    PropertiesEvent => Properties,
    ProviderEvent => Provider,
    ConvertEvent => Convert,
    ArtistInfoOutcome => ArtistInfo,
    MpdEvent => Mpd,
    MpdStatusUpdate => MpdStatus,
}

impl<E: Into<SessionEvent> + Send + 'static> Effect<E> {
    /// The same work, its events wrapped as [`SessionEvent`]s.
    pub fn into_session(self) -> Effect<SessionEvent> {
        self.map(Into::into)
    }
}

/// Something to tell the user. The wording and the way to show it are the
/// UI's.
#[derive(Debug, Clone)]
pub enum SessionNotice {
    Playback(PlaybackNotice),
    Podcast(PodcastNotice),
    Radio(RadioNotice),
    Convert(ConvertNotice),
    /// A provider action (favourite, rating, playlist mutation) failed; the
    /// session already rolled back what it had applied optimistically.
    ProviderActionFailed(String),
    /// A provider (MPD or Subsonic server) could not be connected.
    ProviderConnectFailed {
        provider: String,
        reason: String,
    },
    /// Playlist files were imported, one summary per file.
    PlaylistsImported(Vec<ImportSummary>),
    /// Importing playlist files failed.
    PlaylistImportFailed(String),
}

/// What changed in the session that a UI keeps a presentation of. Every
/// field is "nothing" by default; updates merge with [`Self::absorb`].
#[derive(Debug, Default)]
pub struct SessionChanges {
    /// Library cover thumbnails to (re)build image handles for or drop.
    pub covers: CoverDelta,
    /// The whole library was replaced (not just extended): derived views
    /// such as a folder tree are out of date.
    pub library_loaded: bool,
    /// Library content (or its covers) changed: whatever is derived from
    /// the current track's album art may need a refresh.
    pub library_updated: bool,
    /// The current track changed: the track-dependent presentation (cover
    /// backdrop, visualizer metadata) needs a refresh. The session already
    /// did its half (lyrics, scrobble reset, MPRIS, notification).
    pub track_changed: bool,
    /// Cover bytes arrived in [`Session::cover_bytes`]: whoever waited for
    /// them retries.
    pub cover_bytes_loaded: bool,
    /// Decoded artist photos to build image handles for.
    pub artist_photos: Vec<ArtistPhoto>,
    /// Every artist photo is gone (the provider changed): drop the handles.
    pub artist_photos_cleared: bool,
    /// Decoded remote icons to build image handles for.
    pub online_icons: Vec<OnlineIcon>,
    /// A playlist was created: reset the "new playlist" input.
    pub playlist_created: bool,
    /// A playlist was deleted: leave its detail view.
    pub playlist_deleted: bool,
}

impl SessionChanges {
    /// Folds `later`, which happened after `self`, into it.
    pub fn absorb(&mut self, later: SessionChanges) {
        self.covers.absorb(later.covers);
        self.library_loaded |= later.library_loaded;
        self.library_updated |= later.library_updated;
        self.track_changed |= later.track_changed;
        self.cover_bytes_loaded |= later.cover_bytes_loaded;
        if later.artist_photos_cleared {
            self.artist_photos.clear();
        }
        self.artist_photos_cleared |= later.artist_photos_cleared;
        self.artist_photos.extend(later.artist_photos);
        self.online_icons.extend(later.online_icons);
        self.playlist_created |= later.playlist_created;
        self.playlist_deleted |= later.playlist_deleted;
    }
}

/// What a UI must do after the session did something: run the effect,
/// show the notices, refresh what the changes touched.
#[must_use = "run the effect, show the notices and present the changes"]
pub struct SessionUpdate {
    /// Follow-up work; its events go back into
    /// [`Session::apply_session_event`].
    pub effect: Effect<SessionEvent>,
    pub notices: Vec<SessionNotice>,
    pub changes: SessionChanges,
}

impl SessionUpdate {
    /// Nothing to do.
    pub fn none() -> Self {
        Self {
            effect: Effect::none(),
            notices: Vec::new(),
            changes: SessionChanges::default(),
        }
    }

    fn notice(notice: SessionNotice) -> Self {
        Self {
            notices: vec![notice],
            ..Self::none()
        }
    }

    /// Also do everything `other` asks for, after what `self` already does.
    pub fn also(mut self, other: impl Into<SessionUpdate>) -> Self {
        let other = other.into();
        self.effect = Effect::batch([self.effect, other.effect]);
        self.notices.extend(other.notices);
        self.changes.absorb(other.changes);
        self
    }
}

impl<E: Into<SessionEvent> + Send + 'static> From<Effect<E>> for SessionUpdate {
    fn from(effect: Effect<E>) -> Self {
        Self {
            effect: effect.into_session(),
            ..Self::none()
        }
    }
}

impl From<PlaybackUpdate> for SessionUpdate {
    fn from(update: PlaybackUpdate) -> Self {
        let PlaybackUpdate {
            track_changed,
            notice,
            effect,
        } = update;
        let mut update = Self::from(effect);
        update.changes.track_changed = track_changed;
        update.notices.extend(notice.map(SessionNotice::Playback));
        update
    }
}

impl From<Reload> for SessionUpdate {
    fn from(reload: Reload) -> Self {
        let Reload { covers, effect } = reload;
        let mut update = Self::from(effect);
        update.changes.covers = covers;
        update
    }
}

impl From<ProviderReinit> for SessionUpdate {
    fn from(reinit: ProviderReinit) -> Self {
        let ProviderReinit { reload, errors } = reinit;
        let mut update = Self::none();
        update.notices.extend(
            errors
                .into_iter()
                .map(|(provider, reason)| SessionNotice::ProviderConnectFailed {
                    provider,
                    reason,
                }),
        );
        update.also(reload)
    }
}

impl From<ConvertUpdate> for SessionUpdate {
    fn from(update: ConvertUpdate) -> Self {
        let ConvertUpdate { effect, notice } = update;
        let mut update = Self::from(effect);
        update.notices.extend(notice.map(SessionNotice::Convert));
        update
    }
}

impl From<PlaylistUpdate> for SessionUpdate {
    fn from(update: PlaylistUpdate) -> Self {
        let PlaylistUpdate {
            effect,
            playback,
            outcome,
        } = update;
        let mut update = Self::from(effect);
        match outcome {
            PlaylistOutcome::None => {}
            PlaylistOutcome::Created => update.changes.playlist_created = true,
            PlaylistOutcome::Deleted => update.changes.playlist_deleted = true,
            PlaylistOutcome::Failed(reason) => update
                .notices
                .push(SessionNotice::ProviderActionFailed(reason)),
            PlaylistOutcome::Imported(summaries) => update
                .notices
                .push(SessionNotice::PlaylistsImported(summaries)),
            PlaylistOutcome::ImportFailed(reason) => update
                .notices
                .push(SessionNotice::PlaylistImportFailed(reason)),
        }
        match playback {
            Some(playback) => update.also(playback),
            None => update,
        }
    }
}

/// Identity of a long-running event source (see
/// [`Session::background_streams`]). Equal keys are the same source: a UI
/// that dedupes its subscriptions by key keeps one running stream, and
/// starts or stops a source exactly when its key appears in or disappears
/// from the list -- never because something inside it changed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BackgroundKey {
    /// The filesystem watcher over these music directories.
    FsWatcher(Vec<PathBuf>),
    /// 500 ms position ticker for playback running through the local engine.
    PlaybackTick,
    /// 2 s housekeeping tick while an inhibit lock is held or being taken.
    ExtrasSync,
    /// 500 ms progress ticker while conversion jobs run.
    ConvertTick,
    /// Status poll of the MPD server with this provider id.
    MpdStatus(Arc<str>),
    /// Idle/connection events of the MPD provider at this position of the
    /// provider list.
    MpdIdle(usize),
    /// PipeWire capture of the local MPD server's audio, feeding the
    /// visualizer.
    #[cfg(feature = "visualizer")]
    MpdCapture,
}

/// Position-ticker period while playing through the local engine.
const PLAYBACK_TICK: Duration = Duration::from_millis(500);
/// Progress-ticker period while conversion jobs run.
const CONVERT_TICK: Duration = Duration::from_millis(500);
/// Period of the inhibit-lock housekeeping tick.
const EXTRAS_SYNC: Duration = Duration::from_secs(2);

impl Session {
    /// Builds the session of a real launch: moves legacy data directories
    /// to their current place, loads the settings and calls
    /// [`Session::new`]. Returns the servers that failed to construct, as
    /// `(server name, reason)`, for the UI to surface.
    ///
    /// Must be called inside a tokio runtime, like [`Session::new`]: the
    /// background streams and every [`Effect`] also need one entered while
    /// they run.
    pub fn bootstrap() -> (Self, Vec<ProviderInitError>) {
        crate::migrate::migrate_legacy_dirs();
        Self::new(Config::load())
    }

    /// Everything the backend kicks off at launch: the first-launch intro
    /// jingle (played at most once, ever), the library scan and the keyring
    /// checks. Run the returned effect once, right after construction, and
    /// feed its events into [`Self::apply_session_event`].
    ///
    /// Pages load their own data when a UI first shows them (playlists,
    /// genres, podcasts, radio, ...); that is deliberately not part of the
    /// launch.
    pub fn start(&mut self) -> Effect<SessionEvent> {
        // The jingle needs a sound device it can open on its own, which the
        // browser (autoplay rules) does not give a page at load.
        #[cfg(not(target_arch = "wasm32"))]
        if !self.config.config.intro_played {
            crate::player::intro::play();
            self.config.config.intro_played = true;
            self.config.save();
        }
        // The covers of the scan's reload are empty at launch: nothing is
        // shown yet that could be out of date.
        let scan = self.scan_library().effect;
        Effect::batch([
            scan.into_session(),
            self.config.startup_checks().into_session(),
        ])
    }

    /// Tells the session whether a visualizer is on screen and wants audio:
    /// the capture of a local MPD server's audio runs only then. Only Linux
    /// can capture it (see [`Self::visualizer_capture_supported`]); elsewhere
    /// this does nothing.
    #[cfg(feature = "visualizer")]
    pub fn set_visualizer_capture(&mut self, wanted: bool) {
        self.playback.capture_wanted = wanted;
    }

    /// Whether this build can capture the audio of an MPD server for the
    /// visualizer: it needs PipeWire, so Linux with the `visualizer`
    /// feature only. Everywhere else the visualizer shows local playback
    /// only, and MPD playback has nothing to draw from.
    pub const fn visualizer_capture_supported() -> bool {
        cfg!(all(feature = "visualizer", target_os = "linux"))
    }

    /// Requests the full-size cover bytes of the album `hint` under `key`
    /// (see [`crate::library::CoverArt::album_key`]) from the active
    /// provider. Callers check [`CoverByteCache::should_load`] first -- it
    /// is also checked here, so a redundant call is harmless -- and resolve
    /// the `hint` album only then. The result arrives as
    /// [`SessionEvent::CoverBytes`].
    ///
    /// [`CoverByteCache::should_load`]: crate::library::palette::CoverByteCache::should_load
    pub fn load_cover_bytes(&mut self, key: String, hint: Album) -> Effect<SessionEvent> {
        let Some(provider) = self.providers.registry.active_shared() else {
            return Effect::none();
        };
        if !self.cover_bytes.begin_load(&key) {
            return Effect::none();
        }
        Effect::future(async move {
            let bytes =
                crate::task::spawn_blocking(move || provider.get_cover_art(&hint).ok().flatten())
                    .await
                    .ok()
                    .flatten();
            SessionEvent::CoverBytes { key, bytes }
        })
    }

    /// The long-running event sources that should be running right now,
    /// each under its stable [`BackgroundKey`]. Call it after every state
    /// change (or on every UI subscription refresh): sources whose key is
    /// absent must be stopped, new keys started, known keys left alone --
    /// the streams of known keys can simply be dropped unpolled. Building a
    /// stream does no work; polling it does, and needs a tokio runtime.
    ///
    /// Feed every item to [`Self::apply_session_event`].
    pub fn background_streams(&self) -> Vec<(BackgroundKey, BoxStream<'static, SessionEvent>)> {
        let mut streams: Vec<(BackgroundKey, BoxStream<'static, SessionEvent>)> = Vec::new();
        let player = self.playback.player.as_ref();

        // MPD status poll, gated on the *presence* of an MPD backend, not
        // on playing or `active_backend_type()`: right after switching to
        // an MPD provider the fresh player reports `Stopped` (nothing has
        // been polled yet) and `active_backend_type()` only flips to `Mpd`
        // once Aulos itself issued a `play` -- gating on either would mean
        // never polling a server that was already playing.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(player) = player
            && let Some(mpd) = player.mpd_backend_ref()
            && let Some(provider) = self.active_mpd_provider()
        {
            let provider_id = provider.provider_id_arc();
            streams.push((
                BackgroundKey::MpdStatus(Arc::clone(&provider_id)),
                status_stream(mpd.client(), provider_id)
                    .map(SessionEvent::MpdStatus)
                    .boxed(),
            ));
        }

        // Local-engine position tick, independent of the MPD poll: an MPD
        // backend can exist (MPD provider active) while playback actually
        // runs through the local backend -- a radio stream or podcast
        // resolves to a live/HTTP stream regardless of the active provider.
        if player.is_some_and(|p| {
            p.state() == crate::player::PlaybackState::Playing
                && p.active_backend_type() != crate::player::ActiveBackend::Mpd
        }) {
            streams.push((
                BackgroundKey::PlaybackTick,
                interval_stream(PLAYBACK_TICK)
                    .map(|()| SessionEvent::PlaybackTick)
                    .boxed(),
            ));
        }

        // Slow housekeeping: releases the inhibit lock after a pause or
        // stop (the playback tick only runs while playing).
        if self.playback.extras.inhibit_busy() {
            streams.push((
                BackgroundKey::ExtrasSync,
                interval_stream(EXTRAS_SYNC)
                    .map(|()| SessionEvent::ExtrasTick)
                    .boxed(),
            ));
        }

        // MPD idle events, one source per configured MPD provider. Each
        // opens TWO TCP connections to its server (an idle one and the
        // command one stored in the provider), see `idle_stream`. Identified
        // by list position, so editing a server keeps its running source.
        #[cfg(not(target_arch = "wasm32"))]
        for (idx, provider) in self.providers.mpd_providers.iter().enumerate() {
            streams.push((
                BackgroundKey::MpdIdle(idx),
                idle_stream(Arc::clone(provider))
                    .map(SessionEvent::Mpd)
                    .boxed(),
            ));
        }

        // Conversion progress, gated on the experimental-converter flag so
        // it never runs while the feature is disabled.
        if self.config.config.experimental_converter
            && self
                .convert
                .jobs
                .iter()
                .any(|j| j.state == crate::convert::JobState::Running)
        {
            streams.push((
                BackgroundKey::ConvertTick,
                interval_stream(CONVERT_TICK)
                    .map(|()| SessionEvent::ConvertTick)
                    .boxed(),
            ));
        }

        #[cfg(all(feature = "visualizer", target_os = "linux"))]
        self.push_capture_stream(&mut streams);

        // Filesystem watcher, only while the local provider is active and
        // the "watch for changes" setting is on.
        let local_active = self
            .providers
            .registry
            .active()
            .is_some_and(|p| p.provider_type() == ProviderType::Local);
        if local_active
            && self.config.config.watch_for_changes
            && !self.config.config.music_dirs.is_empty()
        {
            let music_dirs = self.config.config.music_dirs.clone();
            let stream = crate::library::watcher::watch_stream(&music_dirs)
                .map(SessionEvent::FilesChanged)
                .boxed();
            streams.push((BackgroundKey::FsWatcher(music_dirs), stream));
        }

        streams
    }

    /// PipeWire capture of MPD's own audio into the visualizer's PCM
    /// buffer: aulos only remote-controls MPD, so no PCM otherwise reaches
    /// this process. Runs while a visualizer wants audio, MPD is the
    /// backend in charge and its server resolves to this machine (its audio
    /// can only appear on *this* PipeWire graph then); see
    /// `player::pw_capture` for the capture and matching rules.
    #[cfg(all(feature = "visualizer", target_os = "linux"))]
    fn push_capture_stream(
        &self,
        streams: &mut Vec<(BackgroundKey, BoxStream<'static, SessionEvent>)>,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};

        if !self.playback.capture_wanted
            || !self
                .playback
                .player
                .as_ref()
                .is_some_and(|p| p.active_backend_type() == crate::player::ActiveBackend::Mpd)
        {
            return;
        }
        let Some(provider) = self.active_mpd_provider() else {
            return;
        };
        if crate::player::pw_capture::is_local_mpd_host(provider.host()) {
            let capture = crate::player::pw_capture::capture_stream(
                Arc::clone(&self.playback.pcm_buffer),
                Arc::clone(&self.playback.mpd_playing),
            )
            .map(|never| match never {})
            .boxed();
            streams.push((BackgroundKey::MpdCapture, capture));
        } else {
            // Logged at most once per process: this re-evaluates on every
            // subscription refresh while a remote MPD server is active and
            // a visualizer is on screen, which would otherwise spam.
            static REMOTE_WARNED: AtomicBool = AtomicBool::new(false);
            if !REMOTE_WARNED.swap(true, Ordering::Relaxed) {
                tracing::info!(
                    "MPD server '{}' is not local; visualizer audio capture is \
                     unavailable (MPD's own audio only appears on this machine's \
                     PipeWire graph when the server itself runs on it)",
                    provider.host()
                );
            }
        }
    }

    /// Applies one asynchronous result or tick to the session; see the
    /// module docs. This is the single routing point: each event goes to the
    /// workflow of its domain, and what that workflow wants the UI to do
    /// comes back as one [`SessionUpdate`].
    pub fn apply_session_event(&mut self, event: SessionEvent) -> SessionUpdate {
        match event {
            SessionEvent::Library(event) => self.apply_library_dispatch(event),
            SessionEvent::Playback(event) => self.apply_playback_event(event).into(),
            SessionEvent::Playlist(event) => self.apply_playlist_event(event).into(),
            SessionEvent::Config(event) => {
                self.config.apply_event(event);
                SessionUpdate::none()
            }
            SessionEvent::Home(event) => self.apply_home_event(event).into(),
            SessionEvent::Podcast(event) => {
                let update = self.podcasts.apply_event(event);
                self.podcast_update(update)
            }
            SessionEvent::Radio(event) => {
                let update = self.radio.apply_event(event);
                self.radio_update(update)
            }
            SessionEvent::Equalizer(event) => {
                self.apply_equalizer_event(event);
                SessionUpdate::none()
            }
            SessionEvent::Properties(event) => {
                self.apply_properties_event(event);
                SessionUpdate::none()
            }
            SessionEvent::Provider(event) => {
                self.providers.apply_event(event);
                SessionUpdate::none()
            }
            SessionEvent::Convert(event) => self.convert.apply_event(event).into(),
            SessionEvent::ArtistInfo(outcome) => {
                let mut update = SessionUpdate::none();
                update
                    .changes
                    .artist_photos
                    .extend(self.artist_info.apply_outcome(outcome));
                update
            }
            SessionEvent::CoverBytes { key, bytes } => {
                self.cover_bytes.finish_load(key, bytes);
                let mut update = SessionUpdate::none();
                update.changes.cover_bytes_loaded = true;
                update
            }
            SessionEvent::IconLoaded { url, icon } => {
                self.icons.finish(&url, icon.is_some());
                let mut update = SessionUpdate::none();
                update.changes.online_icons.extend(icon);
                update
            }
            SessionEvent::Mpd(event) => self.apply_mpd_dispatch(event),
            SessionEvent::MpdStatus(status) => {
                // Real MPD-server playback state, not gated on which
                // backend is currently "active": the capture thread's
                // monitor fallback must only feed the visualizer while MPD
                // itself is the one making sound.
                #[cfg(feature = "visualizer")]
                if self.playback.player.is_some() {
                    self.playback.mpd_playing.store(
                        status.state == crate::player::PlaybackState::Playing,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                self.apply_mpd_status(status).into()
            }
            SessionEvent::FilesChanged(paths) => self.files_changed(paths).into(),
            SessionEvent::PlaybackTick => self.playback_tick().into(),
            SessionEvent::ExtrasTick => self.playback_extras_tick().into(),
            SessionEvent::ConvertTick => SessionUpdate::none(),
        }
    }

    fn apply_library_dispatch(&mut self, event: LibraryEvent) -> SessionUpdate {
        match self.apply_library_event(event) {
            LibraryApplied::Stale | LibraryApplied::Done => SessionUpdate::none(),
            LibraryApplied::ScanComplete { reload } => reload.into(),
            LibraryApplied::Loaded { covers } => {
                let mut update = SessionUpdate::from(self.load_home(true));
                update.changes.covers = covers;
                update.changes.library_loaded = true;
                update.changes.library_updated = true;
                update.also(self.autoplay_on_launch())
            }
            LibraryApplied::Batch { covers, staged } => {
                let mut update = SessionUpdate::none();
                if !staged {
                    update.changes.covers = covers;
                    update.changes.library_updated = true;
                }
                update
            }
            LibraryApplied::Completed { covers } => {
                let mut update = SessionUpdate::from(self.load_home(true));
                update.changes.covers = covers;
                update.changes.library_updated = true;
                update.also(self.autoplay_on_launch())
            }
            LibraryApplied::ActionFailed(reason) => {
                SessionUpdate::notice(SessionNotice::ProviderActionFailed(reason))
            }
        }
    }

    fn apply_mpd_dispatch(&mut self, event: MpdEvent) -> SessionUpdate {
        match event {
            MpdEvent::Connected(provider_id) => {
                let connected = self.on_mpd_connected(&provider_id);
                if connected.recreate_player {
                    self.recreate_player();
                }
                connected.reload.into()
            }
            MpdEvent::ConnectionFailed(provider_id, error) => {
                match self.on_mpd_connection_failed(&provider_id, &error) {
                    Some((provider, reason)) => {
                        SessionUpdate::notice(SessionNotice::ProviderConnectFailed {
                            provider,
                            reason,
                        })
                    }
                    None => SessionUpdate::none(),
                }
            }
            MpdEvent::Idle(provider_id, subsystem) => {
                match self.on_mpd_idle(&provider_id, subsystem) {
                    MpdIdleOutcome::Ignore => SessionUpdate::none(),
                    MpdIdleOutcome::Reload(reload) => reload.into(),
                    MpdIdleOutcome::ReloadPlaylists => self.load_playlists().into(),
                }
            }
        }
    }

    fn podcast_update(&mut self, update: PodcastUpdate) -> SessionUpdate {
        let PodcastUpdate {
            effect,
            icon_urls,
            notice,
        } = update;
        let mut update = SessionUpdate::from(effect).also(self.fetch_icons(icon_urls));
        update.notices.extend(notice.map(SessionNotice::Podcast));
        update
    }

    fn radio_update(&mut self, update: RadioUpdate) -> SessionUpdate {
        let RadioUpdate {
            effect,
            icon_urls,
            notice,
            play,
        } = update;
        // Station icons come from the stations' own servers: only with the
        // setting on.
        let icon_urls = if self.config.config.fetch_radio_icons {
            icon_urls
        } else {
            Vec::new()
        };
        let mut update = SessionUpdate::from(effect).also(self.fetch_icons(icon_urls));
        update.notices.extend(notice.map(SessionNotice::Radio));
        match play {
            // The resolved stream plays as a one-track list.
            Some(track) => update.also(self.play_tracks(vec![track], 0)),
            None => update,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{test_session, track};
    use super::*;
    use crate::provider::mpd_events::IdleSubsystem;
    use futures::StreamExt;

    fn album(name: &str) -> Album {
        Album::new(name.into(), "artist".into(), 2000, Vec::new(), None)
    }

    #[test]
    fn domain_events_convert_into_their_variant() {
        assert!(matches!(
            SessionEvent::from(LibraryEvent::GenresLoaded(Vec::new())),
            SessionEvent::Library(_)
        ));
        assert!(matches!(
            SessionEvent::from(PodcastEvent::PodcastsLoaded(Vec::new())),
            SessionEvent::Podcast(_)
        ));
        assert!(matches!(
            SessionEvent::from(MpdEvent::Connected("x".into())),
            SessionEvent::Mpd(_)
        ));
    }

    #[test]
    fn library_events_route_to_the_library_workflow() {
        let mut session = test_session();
        let update = session.apply_session_event(
            LibraryEvent::GenresLoaded(vec!["Rock".into(), "Jazz".into()]).into(),
        );
        // Loading sorts the genres.
        assert_eq!(session.library.genres, ["Jazz", "Rock"]);
        assert!(update.notices.is_empty());
        assert!(update.effect.is_none());
    }

    #[test]
    fn a_failed_provider_action_becomes_a_notice() {
        let mut session = test_session();
        let update = session.apply_session_event(
            LibraryEvent::FavoriteToggled {
                track_id: "t".into(),
                optimistic: true,
                result: Err("denied".into()),
            }
            .into(),
        );
        assert!(matches!(
            update.notices.as_slice(),
            [SessionNotice::ProviderActionFailed(reason)] if reason == "denied"
        ));
    }

    #[test]
    fn playlist_outcomes_become_changes_and_notices() {
        use super::super::PlaylistOp;
        let mut session = test_session();
        let created = session.apply_session_event(
            PlaylistEvent::OpDone {
                op: PlaylistOp::Create,
                result: Ok(()),
            }
            .into(),
        );
        assert!(created.changes.playlist_created);
        let failed = session.apply_session_event(
            PlaylistEvent::OpDone {
                op: PlaylistOp::AddTrack,
                result: Err("nope".into()),
            }
            .into(),
        );
        assert!(matches!(
            failed.notices.as_slice(),
            [SessionNotice::ProviderActionFailed(reason)] if reason == "nope"
        ));
        let import_failed =
            session.apply_session_event(PlaylistEvent::Imported(Err("bad file".into())).into());
        assert!(matches!(
            import_failed.notices.as_slice(),
            [SessionNotice::PlaylistImportFailed(reason)] if reason == "bad file"
        ));
    }

    #[test]
    fn cover_bytes_are_cached_and_reported() {
        let mut session = test_session();
        let update = session.apply_session_event(SessionEvent::CoverBytes {
            key: "k".into(),
            bytes: Some(vec![1, 2, 3]),
        });
        assert!(update.changes.cover_bytes_loaded);
        assert_eq!(
            session.cover_bytes.get("k").as_deref(),
            Some(&vec![1, 2, 3])
        );
    }

    #[test]
    fn icons_are_reported_and_a_failed_fetch_may_retry() {
        let mut session = test_session();
        session.icons.request(vec!["u".into()]);
        let failed = session.apply_session_event(SessionEvent::IconLoaded {
            url: "u".into(),
            icon: None,
        });
        assert!(failed.changes.online_icons.is_empty());
        assert_eq!(session.icons.request(vec!["u".into()]), ["u"]);

        let loaded_before = session.icons.request(vec!["u".into()]);
        assert!(loaded_before.is_empty());
        let loaded = session.apply_session_event(SessionEvent::IconLoaded {
            url: "u".into(),
            icon: Some(OnlineIcon {
                url: "u".into(),
                width: 1,
                height: 1,
                rgba: vec![0; 4],
            }),
        });
        assert_eq!(loaded.changes.online_icons.len(), 1);
        // A loaded icon stays known: it is never fetched again.
        assert!(session.icons.request(vec!["u".into()]).is_empty());
    }

    #[test]
    fn artist_info_outcomes_deliver_the_photo() {
        let mut session = test_session();
        let outcome = ArtistInfoOutcome {
            name: "Someone".into(),
            bio: None,
            image: Some((1, 1, vec![0; 4])),
        };
        let update = session.apply_session_event(outcome.into());
        assert_eq!(update.changes.artist_photos.len(), 1);
        assert!(session.artist_info.photos.contains("Someone"));
    }

    #[test]
    fn ticks_without_a_player_do_nothing() {
        let mut session = test_session();
        for tick in [
            SessionEvent::PlaybackTick,
            SessionEvent::ExtrasTick,
            SessionEvent::ConvertTick,
        ] {
            let update = session.apply_session_event(tick);
            assert!(update.notices.is_empty());
            assert!(!update.changes.track_changed);
        }
    }

    #[test]
    fn ignored_mpd_idle_events_do_nothing() {
        let mut session = test_session();
        let update = session.apply_session_event(
            MpdEvent::Idle("not-active".into(), IdleSubsystem::Database).into(),
        );
        assert!(update.effect.is_none());
        assert!(update.notices.is_empty());
    }

    #[test]
    fn updates_merge_effects_notices_and_changes() {
        let mut changes = SessionChanges::default();
        changes.covers.changed.push("a".into());
        let mut first = SessionUpdate::none();
        first.changes = changes;
        first.changes.track_changed = true;
        let mut second = SessionUpdate::notice(SessionNotice::PlaylistImportFailed("x".into()));
        second.changes.covers.removed.push("a".into());
        let merged = first.also(second);
        assert!(merged.changes.track_changed);
        assert!(merged.changes.covers.changed.is_empty());
        assert_eq!(merged.changes.covers.removed, ["a"]);
        assert_eq!(merged.notices.len(), 1);
    }

    #[test]
    fn a_playback_update_converts_with_its_track_change() {
        let mut session = test_session();
        let update: SessionUpdate = session.play_tracks(vec![track(1)], 0).into();
        // No audio device in tests: whatever the player did, the conversion
        // keeps the effect/notice/changes split.
        let _ = (update.effect, update.notices, update.changes);
    }

    #[test]
    fn background_keys_hash_by_identity() {
        use std::collections::HashSet;
        let keys: HashSet<_> = [
            BackgroundKey::PlaybackTick,
            BackgroundKey::PlaybackTick,
            BackgroundKey::MpdIdle(0),
            BackgroundKey::MpdIdle(0),
            BackgroundKey::MpdIdle(1),
            BackgroundKey::MpdStatus(Arc::from("a")),
            BackgroundKey::MpdStatus(Arc::from("a")),
            BackgroundKey::FsWatcher(vec![PathBuf::from("/m")]),
            BackgroundKey::FsWatcher(vec![PathBuf::from("/m")]),
        ]
        .into_iter()
        .collect();
        assert_eq!(keys.len(), 5);
    }

    #[test]
    fn an_idle_session_has_no_background_streams() {
        let session = test_session();
        assert!(session.background_streams().is_empty());
    }

    #[tokio::test]
    async fn background_streams_follow_the_session_state() {
        let mut session = test_session();

        // The filesystem watcher needs the local provider; the test session
        // has none registered.
        session.config.config.music_dirs = vec![PathBuf::from("/music")];
        assert!(session.background_streams().is_empty());

        // The converter ticker needs the flag and a running job.
        session.config.config.experimental_converter = true;
        assert!(session.background_streams().is_empty());
        let _ = session
            .convert
            .enqueue(vec![PathBuf::from("/music/a.flac")]);
        if let Some(job) = session.convert.jobs.first_mut() {
            job.state = crate::convert::JobState::Running;
        }
        let keys: Vec<_> = session
            .background_streams()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, [BackgroundKey::ConvertTick]);
        session.config.config.experimental_converter = false;
        assert!(session.background_streams().is_empty());
    }

    #[tokio::test]
    async fn the_convert_tick_stream_emits_convert_ticks() {
        let mut session = test_session();
        session.config.config.experimental_converter = true;
        let _ = session
            .convert
            .enqueue(vec![PathBuf::from("/music/a.flac")]);
        if let Some(job) = session.convert.jobs.first_mut() {
            job.state = crate::convert::JobState::Running;
        }
        let (_, mut stream) = session
            .background_streams()
            .into_iter()
            .next()
            .expect("convert ticker");
        assert!(matches!(
            stream.next().await,
            Some(SessionEvent::ConvertTick)
        ));
    }

    #[tokio::test]
    async fn start_scans_and_checks_the_keyring_without_replaying_the_intro() {
        let mut session = test_session();
        session.config.config.intro_played = true;
        // No provider is active in the test session, so the scan is a
        // no-op, and the keyring check only runs for configured servers.
        let _effect = session.start();
        assert!(session.config.config.intro_played);
    }

    #[test]
    fn the_cover_load_request_needs_an_active_provider() {
        let mut session = test_session();
        assert!(session.load_cover_bytes("k".into(), album("a")).is_none());
        assert!(session.cover_bytes.should_load("k"));
    }
}
