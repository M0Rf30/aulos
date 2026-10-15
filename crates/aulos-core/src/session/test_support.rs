// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Deterministic [`Session`] fixtures for unit tests: no audio device, no
//! provider, no network and nothing under the user's data or config
//! directories (scratch files go to a per-process directory below the
//! system temp dir).

use super::{
    ARTIST_INFO_CONCURRENCY, ArtistInfoState, ConfigState, ConvertState, EqualizerState, IconState,
    PlaybackState, PlaylistsState, ProvidersState, RadioState, Session,
};
use crate::config::Config;
use crate::library::Track;
use crate::library::artist_info::ArtistInfoStore;
use crate::library::history::LOCAL_PROVIDER;
use crate::library::home_state::HomeState;
use crate::library::palette::CoverByteCache;
use crate::library::properties::PropertiesState;
use crate::library::state::LibraryState;
use crate::online::scrobble::ScrobbleController;
use crate::player::eq_presets::EqPresetManager;
use crate::player::extras::PlaybackExtrasState;
use crate::provider::ProviderRegistry;
use crate::tagger::TaggerState;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Semaphore;

/// A fresh scratch directory (created) unique to the caller.
pub(super) fn scratch_dir(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aulos-session-tests-{}/{name}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A session with the default settings and nothing loaded; settings are
/// not persisted.
pub(super) fn test_session() -> Session {
    session_with(None, Config::default())
}

/// A session whose settings are persisted into a scratch directory (so
/// `config.save()` really queues a write; after `config.flush()` the file is
/// at `config.path`).
pub(super) fn test_session_persisting() -> Session {
    let path = scratch_dir("config").join("settings.ron");
    session_with(Some(path), Config::default())
}

fn session_with(config_path: Option<PathBuf>, config: Config) -> Session {
    let scratch = scratch_dir("state");
    Session {
        providers: ProvidersState {
            registry: ProviderRegistry::new(),
            mpd_providers: Vec::new(),
            subsonic_providers: Vec::new(),
            provider_list: Vec::new(),
            active_index: None,
            manually_switched: false,
            mpd_edit_states: Vec::new(),
            mpd_connection_status: Vec::new(),
            subsonic_edit_states: Vec::new(),
            subsonic_connection_status: Vec::new(),
            provider_errors: HashMap::new(),
        },
        library: LibraryState::default(),
        playback: PlaybackState {
            player: None,
            position: Duration::ZERO,
            current_track: None,
            mpris: None,
            pre_mute_volume: None,
            scrobble_now_playing_sent: false,
            scrobble_sent: false,
            scrobble: ScrobbleController::with_queue_path(&config, scratch.join("scrobbles.json")),
            lyrics: None,
            lyrics_loading: false,
            extras: PlaybackExtrasState::default(),
            window_focused: false,
            seeking: false,
            autoplay_pending: false,
            #[cfg(feature = "visualizer")]
            pcm_buffer: Arc::new(std::sync::Mutex::new(crate::player::pcm::PcmBuffer::new(
                8192,
            ))),
            #[cfg(feature = "visualizer")]
            mpd_playing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(feature = "visualizer")]
            capture_wanted: false,
        },
        cover_bytes: CoverByteCache::new(),
        playlists: PlaylistsState::default(),
        home: HomeState::default(),
        podcasts: Default::default(),
        radio: RadioState::new(),
        equalizer: EqualizerState {
            preset: None,
            preset_manager: EqPresetManager::new(scratch.join("eq_presets"))
                .expect("eq presets dir"),
            all_presets: Vec::new(),
            active_preset_name: None,
            dirty: false,
            autoeq_profiles: Vec::new(),
            autoeq_loading: false,
        },
        artist_info: ArtistInfoState {
            store: Arc::new(ArtistInfoStore::open(&scratch)),
            semaphore: Arc::new(Semaphore::new(ARTIST_INFO_CONCURRENCY)),
            bios: HashMap::new(),
            pending: HashSet::new(),
            negative: HashSet::new(),
            photos: HashSet::new(),
        },
        icons: IconState::default(),
        convert: ConvertState::new(),
        tagger: TaggerState::new(),
        properties: PropertiesState::default(),
        config: ConfigState::new(config_path, config),
    }
}

/// A local-library track with the given id.
pub(super) fn track(id: i64) -> Track {
    Track {
        id,
        path: PathBuf::from(format!("/music/{id}.flac")),
        title: format!("title {id}"),
        artist: "artist".into(),
        album_artist: String::new(),
        album: "album".into(),
        genre: String::new(),
        track_number: 1,
        disc_number: 1,
        year: 0,
        duration: Duration::from_secs(200),
        bitrate: 0,
        sample_rate: 0,
        provider_id: LOCAL_PROVIDER.into(),
        source_uri: String::new(),
        is_favorite: false,
        rating: None,
        rg_track_gain: None,
        rg_album_gain: None,
        bit_depth: 0,
        release_type: String::new(),
        added_at: 0,
        favorited_at: 0,
    }
}
