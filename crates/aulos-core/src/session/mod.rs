// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The domain state of one running Aulos: settings, providers, the library,
//! playback, playlists, podcasts, radio, equalizer, artist info, conversion
//! jobs, the tag editor and the properties drawer.
//!
//! A UI owns exactly one [`Session`] next to its own view state (navigation,
//! text inputs, image handles, animation...). Every sub-state is a plain
//! struct with public fields so views can read them directly.
//!
//! Results come back through one door: [`SessionEvent`] into
//! [`Session::apply_session_event`], answered by a [`SessionUpdate`]; see
//! the `events` module. [`Session::bootstrap`] builds the session of a real
//! launch, [`Session::start`] kicks off the launch work and
//! [`Session::background_streams`] lists the long-running event sources.

mod artist_info;
mod config;
mod convert;
mod credential_check;
mod equalizer;
mod events;
mod home;
mod icons;
mod library;
mod playback;
mod playlists;
mod podcasts;
mod preferences;
mod properties;
mod providers;
mod radio;
#[cfg(test)]
mod test_support;
mod transport;

pub use artist_info::{ARTIST_INFO_CONCURRENCY, ArtistInfoState, ArtistPhoto};
pub use config::{ConfigEvent, ConfigState};
pub use convert::{ConvertEvent, ConvertNotice, ConvertState, ConvertUpdate};
pub use credential_check::{CredentialKind, CredentialUpdate};
pub use equalizer::{EqualizerEvent, EqualizerState};
pub use events::{
    BackgroundKey, OnlineIcon, SessionChanges, SessionEvent, SessionNotice, SessionUpdate,
};
pub use home::HomeEvent;
pub use icons::IconState;
pub use library::{LibraryApplied, LibraryEvent, Reload};
pub use playback::{PlaybackEvent, PlaybackNotice, PlaybackState, PlaybackUpdate, QueuePlacement};
pub use playlists::{
    PlaylistEditing, PlaylistEvent, PlaylistOp, PlaylistOutcome, PlaylistUpdate, PlaylistsState,
    export_playlist, import_playlist_files,
};
pub use podcasts::{EpisodePlayback, PodcastEvent, PodcastNotice, PodcastUpdate, PodcastsState};
pub use properties::PropertiesEvent;
pub use providers::{
    MpdConnected, MpdIdleOutcome, ProviderEvent, ProviderInitError, ProviderReinit, ProviderStatus,
    ProvidersState,
};
pub use radio::{RadioEvent, RadioNotice, RadioState, RadioUpdate};
pub use transport::{SEEK_STEP, VOLUME_STEP};

use crate::config::Config;
use crate::library::home_state::HomeState;
use crate::library::palette::CoverByteCache;
use crate::library::properties::PropertiesState;
use crate::library::state::LibraryState;
use crate::tagger::TaggerState;

/// All domain state of a running Aulos; see the module docs.
pub struct Session {
    pub config: ConfigState,
    pub providers: ProvidersState,
    /// Library lists, reload bookkeeping, header-search results and the
    /// scan/folder-tree flags.
    pub library: LibraryState,
    pub playback: PlaybackState,
    /// Full-size cover bytes of recently shown albums (current track,
    /// detail hero, notifications), loaded lazily; never the whole library.
    pub cover_bytes: CoverByteCache,
    pub playlists: PlaylistsState,
    /// Home page shelves, decade grid and play-history tracker.
    pub home: HomeState,
    pub podcasts: PodcastsState,
    pub radio: RadioState,
    pub equalizer: EqualizerState,
    pub artist_info: ArtistInfoState,
    /// Which remote icons were requested, see [`Session::fetch_icons`].
    pub icons: IconState,
    pub convert: ConvertState,
    /// The tag editor page's files, selection and running jobs.
    pub tagger: TaggerState,
    /// State of the Track/Album Properties drawer.
    pub properties: PropertiesState,
}

impl Session {
    /// Builds the session from the loaded settings: spawns the background
    /// config writer, opens the library database and registers the local /
    /// MPD / Subsonic providers, creates the audio player and the stores.
    ///
    /// Must be called inside a tokio runtime (provider construction takes a
    /// runtime handle). Also returns the Subsonic servers that failed to
    /// construct, as `(server name, reason)`, for the UI to surface.
    ///
    /// # Panics
    /// If the equalizer presets directory cannot be created.
    pub fn new(config: Config) -> (Self, Vec<ProviderInitError>) {
        let (providers, init_errors) = ProvidersState::new(&config);
        let music_dirs_present = config.music_dirs.iter().any(|d| d.is_dir());
        let mut session = Self {
            providers,
            library: LibraryState::default(),
            playback: PlaybackState::new(&config),
            cover_bytes: CoverByteCache::new(),
            playlists: PlaylistsState::default(),
            home: HomeState::default(),
            podcasts: PodcastsState::default(),
            radio: RadioState::new(),
            equalizer: EqualizerState::new(&config),
            artist_info: ArtistInfoState::new(),
            icons: IconState::default(),
            convert: ConvertState::new(),
            tagger: TaggerState::new(),
            properties: PropertiesState::default(),
            config: ConfigState::new(Some(Config::path()), config),
        };
        session.library.music_dirs_present = music_dirs_present;
        (session, init_errors)
    }
}
