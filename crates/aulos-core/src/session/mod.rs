// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The domain state of one running Aulos: settings, providers, the library,
//! playback, playlists, podcasts, radio, equalizer, artist info, conversion
//! jobs, the tag editor and the properties drawer.
//!
//! A UI owns exactly one [`Session`] next to its own view state (navigation,
//! text inputs, image handles, animation...). Every sub-state is a plain
//! struct with public fields so views can read them directly.

mod artist_info;
mod config;
mod convert;
mod credential_check;
mod equalizer;
mod home;
mod library;
mod playback;
mod playlists;
mod podcasts;
mod properties;
mod providers;
mod radio;
mod transport;

pub use artist_info::{ARTIST_INFO_CONCURRENCY, ArtistInfoState, ArtistPhoto};
pub use config::{ConfigEvent, ConfigState, ConfigUpdate};
pub use convert::{ConvertEvent, ConvertNotice, ConvertState, ConvertUpdate};
pub use credential_check::{CredentialKind, CredentialUpdate};
pub use equalizer::{EqualizerEvent, EqualizerState};
pub use home::HomeEvent;
pub use library::{
    CoverImage, CoverReset, CoverSet, CoverThumb, LibraryApplied, LibraryEvent, Reload,
};
pub use playback::{PlaybackEvent, PlaybackNotice, PlaybackState, PlaybackUpdate, QueuePlacement};
pub use playlists::{
    PlaylistEvent, PlaylistOp, PlaylistOutcome, PlaylistsState, export_playlist,
    import_playlist_files,
};
pub use podcasts::{EpisodePlayback, PodcastEvent, PodcastNotice, PodcastUpdate, PodcastsState};
pub use properties::PropertiesEvent;
pub use providers::{
    MpdConnected, MpdIdleOutcome, ProviderEvent, ProviderInitError, ProviderReinit, ProvidersState,
};
pub use radio::{RadioEvent, RadioNotice, RadioState, RadioUpdate};
pub use transport::{SEEK_STEP, VOLUME_STEP};

use crate::config::Config;
use crate::library::home_state::HomeState;
use crate::library::properties::PropertiesState;
use crate::library::state::LibraryState;
use crate::tagger::TaggerState;
use cosmic_config::Config as ConfigContext;

/// All domain state of a running Aulos; see the module docs.
pub struct Session {
    pub config: ConfigState,
    pub providers: ProvidersState,
    /// Library lists, reload bookkeeping, header-search results and the
    /// scan/folder-tree flags.
    pub library: LibraryState,
    pub playback: PlaybackState,
    pub playlists: PlaylistsState,
    /// Home page shelves, decade grid and play-history tracker.
    pub home: HomeState,
    pub podcasts: PodcastsState,
    pub radio: RadioState,
    pub equalizer: EqualizerState,
    pub artist_info: ArtistInfoState,
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
    pub fn new(context: Option<ConfigContext>, config: Config) -> (Self, Vec<ProviderInitError>) {
        let (providers, init_errors) = ProvidersState::new(&config);
        let music_dirs_present = config.music_dirs.iter().any(|d| d.is_dir());
        let mut session = Self {
            providers,
            library: LibraryState::default(),
            playback: PlaybackState::new(&config),
            playlists: PlaylistsState::default(),
            home: HomeState::default(),
            podcasts: PodcastsState::default(),
            radio: RadioState::new(),
            equalizer: EqualizerState::new(&config),
            artist_info: ArtistInfoState::new(),
            convert: ConvertState::new(),
            tagger: TaggerState::new(),
            properties: PropertiesState::default(),
            config: ConfigState::new(context, config),
        };
        session.library.music_dirs_present = music_dirs_present;
        (session, init_errors)
    }
}
