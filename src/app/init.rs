// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::{
    APP_ICON, AppFlags, AppModel, ContextPage, Message, Page, REPOSITORY, key_binds, radio_page,
};
use crate::config::Config;
use crate::fl;
use crate::library::LibraryDb;
use crate::player::Player;
use crate::provider::local::LocalProvider;
use crate::provider::mpd::{MpdConfig, MpdProvider};
use crate::provider::subsonic::{SubsonicConfig, SubsonicProvider};
use crate::provider::{MusicProvider, ProviderRegistry};
use crate::views::radio as radio_view;
use crate::views::{podcasts, providers, songs};
use cosmic::Application;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::prelude::*;
use cosmic::widget::{self, about::About, icon, nav_bar};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(feature = "visualizer")]
use std::sync::Mutex;
use std::time::Duration;

impl AppModel {
    pub(super) fn init_model(
        core: cosmic::Core,
        flags: AppFlags,
    ) -> (Self, Task<cosmic::Action<Message>>) {
        let mut nav = nav_bar::Model::default();

        nav.insert()
            .text(fl!("home"))
            .data::<Page>(Page::Home)
            .icon(icon::from_name("user-home-symbolic"))
            .activate();

        nav.insert()
            .text(fl!("albums"))
            .data::<Page>(Page::Albums)
            .icon(icon::from_name("media-optical-symbolic"));

        nav.insert()
            .text(fl!("artists"))
            .data::<Page>(Page::Artists)
            .icon(icon::from_name("system-users-symbolic"));

        nav.insert()
            .text(fl!("songs"))
            .data::<Page>(Page::Songs)
            .icon(icon::from_name("audio-x-generic-symbolic"));

        nav.insert()
            .text(fl!("playlists"))
            .data::<Page>(Page::Playlists)
            .icon(icon::from_name("playlist-symbolic"));

        nav.insert()
            .text(fl!("smart-playlists"))
            .data::<Page>(Page::SmartPlaylists)
            .icon(icon::from_name("folder-saved-search-symbolic"));

        nav.insert()
            .text(fl!("genres"))
            .data::<Page>(Page::Genres)
            .icon(icon::from_name("media-tape-symbolic"));

        nav.insert()
            .text(fl!("folders"))
            .data::<Page>(Page::Folders)
            .icon(icon::from_name("folder-symbolic"));

        nav.insert()
            .text(fl!("podcasts"))
            .data::<Page>(Page::Podcasts)
            .icon(icon::from_name("audio-input-microphone-symbolic"))
            .divider_above(true);

        nav.insert()
            .text(fl!("radio"))
            .data::<Page>(Page::Radio)
            .icon(icon::from_name("network-wireless-symbolic"));

        let about = About::default()
            .name(fl!("app-title"))
            .comments(fl!("app-motto"))
            .icon(widget::icon::from_svg_bytes(APP_ICON))
            .version(env!("CARGO_PKG_VERSION"))
            .links([(fl!("repository"), REPOSITORY)])
            .license("GPL-3.0");

        // Load config and cache the context to avoid repeated D-Bus watcher creation
        let config_context = cosmic_config::Config::new(Self::APP_ID, Config::VERSION).ok();
        let mut config = config_context
            .as_ref()
            .map(|context| match Config::get_entry(context) {
                Ok(config) => config,
                Err((_errors, config)) => config,
            })
            .unwrap_or_default();
        crate::views::common::set_grid_scale(config.grid_scale);

        // First launch: play the intro jingle once, Winamp-style, and
        // remember that it has been heard.
        if !config.intro_played {
            crate::player::intro::play();
            config.intro_played = true;
            // Single key (one small file), not a full `write_entry`: a
            // full rewrite costs ~0.4 s before the first frame.
            if let Some(context) = &config_context
                && let Err(e) = config.set_intro_played(context, true)
            {
                tracing::error!("Failed to save config after intro: {e:?}");
            }
        }

        if config.experimental_converter {
            insert_convert_nav_entry(&mut nav);
        }

        // Open on the configured section (Home by default); unknown or
        // unavailable choices fall back to Home.
        let start_page =
            super::startup::resolve_startup_page(&config.startup_page, &config.last_view, |page| {
                nav.iter().any(|id| nav.data::<Page>(id) == Some(page))
            });
        let start_id = nav
            .iter()
            .find(|&id| nav.data::<Page>(id) == Some(&start_page));
        if let Some(id) = start_id {
            nav.activate(id);
        }

        // Keyring work (availability probe, verifying that stored passwords
        // still exist, migrating plaintext passwords) costs secret-service
        // D-Bus round trips -- 50-500 ms, or an unlock prompt -- so it runs
        // as a startup background task (`Message::CredentialsChecked`)
        // instead of before the first frame. Providers are still constructed
        // below with whatever password they can get right now: entries
        // already in the keyring are read by `MpdConfig::from` /
        // `SubsonicConfig::from` (unavoidable, the connection needs the
        // password), plaintext ones use the config value, which stays valid
        // for this session even after the background migration moved it.
        let credential_check = needs_credential_check(&config)
            .then(|| (config.mpd_servers.clone(), config.subsonic_servers.clone()));
        // Saves from here on go through the background writer; `config`
        // is exactly what is on disk now (baseline for change detection).
        let config_writer = config_context
            .clone()
            .and_then(|ctx| super::config_writer::ConfigWriter::spawn(ctx, config.clone()));

        // Open library database and initialize provider registry
        let db_path = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("aulos")
            .join("library.db");

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let mut registry = ProviderRegistry::new();
        if let Ok(db) = LibraryDb::open(&db_path) {
            let local = LocalProvider::new(db, config.music_dirs.clone());
            registry.register(Arc::new(local));
        } else {
            tracing::error!("Failed to open library database");
        }

        // Initialize MPD providers from config.
        // Providers are registered immediately (browse returns NotConnected until
        // the subscription establishes the connection). The actual TCP connect +
        // idle-event loop runs inside a COSMIC subscription (see `subscription()`).
        let rt_handle = tokio::runtime::Handle::current();
        let mut mpd_providers = Vec::new();
        for entry in &config.mpd_servers {
            let mpd_config: MpdConfig = entry.clone().into();
            let provider = Arc::new(MpdProvider::new(mpd_config, rt_handle.clone()));
            mpd_providers.push(Arc::clone(&provider));
            registry.register(Arc::clone(&provider) as Arc<dyn MusicProvider>);
        }

        // Initialize Subsonic providers from config.
        let mut subsonic_providers = Vec::new();
        let mut subsonic_init_errors: Vec<(String, String)> = Vec::new();
        for entry in &config.subsonic_servers {
            let subsonic_config: SubsonicConfig = entry.clone().into();
            match SubsonicProvider::new(subsonic_config, rt_handle.clone()) {
                Ok(provider) => {
                    let provider = Arc::new(provider);
                    subsonic_providers.push(Arc::clone(&provider));
                    registry.register(Arc::clone(&provider) as Arc<dyn MusicProvider>);
                }
                Err(e) => {
                    tracing::error!("Failed to create Subsonic provider '{}': {e}", entry.name);
                    subsonic_init_errors.push((entry.name.clone(), e.to_string()));
                }
            }
        }

        // Restore the provider that was active when the app last exited,
        // falling back to local if it's no longer registered (server
        // removed from config, etc). All providers above are registered
        // synchronously regardless of connection state, so this already
        // reflects the final registered set even though MPD/Subsonic
        // connections themselves complete asynchronously later.
        let registered_ids: Vec<String> =
            registry.list().into_iter().map(|(id, _, _)| id).collect();
        if let Some(target) = crate::config::resolve_active_provider(
            config.startup_provider_choice(),
            &registered_ids,
        ) {
            registry.set_active(&target);
        }

        // Build editing state for MPD servers
        let mpd_edit_states: Vec<providers::MpdEditState> = config
            .mpd_servers
            .iter()
            .map(providers::MpdEditState::from_config)
            .collect();
        let mpd_connection_status: Vec<Option<String>> = vec![None; mpd_edit_states.len()];

        // Build editing state for Subsonic servers
        let subsonic_edit_states: Vec<providers::SubsonicEditState> = config
            .subsonic_servers
            .iter()
            .map(providers::SubsonicEditState::from_config)
            .collect();
        let subsonic_connection_status: Vec<Option<String>> =
            vec![None; subsonic_edit_states.len()];

        // Initialize player
        #[allow(unused_mut)]
        let mut player = match Player::new(None) {
            Ok(mut p) => {
                // Apply the persisted master volume; `Player::new` hardcodes
                // 0.8 internally, so without this every launch would ignore
                // the saved level.
                if let Err(e) = p.set_volume(config.volume) {
                    tracing::warn!("Failed to apply saved volume: {e}");
                }
                Some(p)
            }
            Err(e) => {
                tracing::error!("Failed to initialize audio player: {e}");
                None
            }
        };

        // Create shared PCM buffer for visualizer audio tapping
        #[cfg(feature = "visualizer")]
        let pcm_buffer = {
            let buf = Arc::new(Mutex::new(
                crate::views::now_playing::visualizer::PcmBuffer::new(8192),
            ));
            if let Some(ref mut p) = player {
                p.set_pcm_buffer(Arc::clone(&buf));
            }
            Some(buf)
        };

        // Tracks whether MPD itself reports `Playing`, for the PipeWire
        // capture thread's default-sink-monitor fallback gating (see
        // `AppModel::mpd_playing`'s doc comment).
        #[cfg(feature = "visualizer")]
        let mpd_playing = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Command channel to the projectM render thread — replaces the
        // old `next_preset_signal: AtomicBool` flag so the UI can also
        // request specific-preset loads, lock state, and beat sensitivity.
        // The `Sender` lives in `AppModel` for the whole app lifetime; the
        // `Receiver` is checked out of this `Mutex<Option<_>>` slot by
        // whichever render thread is currently running and handed back
        // when it stops (visualizer deactivated), so a later reactivation
        // can check it out again — the channel itself is only ever
        // created once, here.
        #[cfg(feature = "visualizer")]
        let (viz_cmd_tx, viz_cmd_rx) =
            std::sync::mpsc::channel::<crate::views::now_playing::visualizer::VizCommand>();
        #[cfg(feature = "visualizer")]
        let viz_cmd_rx_slot = Arc::new(Mutex::new(Some(viz_cmd_rx)));
        // Preset name the render thread most recently loaded/switched to;
        // written there, mirrored into `AppModel::viz_current_preset_name`
        // on every `VisualizerFrameReady`.
        #[cfg(feature = "visualizer")]
        let viz_current_preset_shared: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let artist_tag_delimiters_input = config.artist_tag_delimiters.join(" | ");
        let music_dirs_present = config.music_dirs.iter().any(|d| d.is_dir());
        let scrobble = crate::online::scrobble::ScrobbleController::new(&config);

        let mut app = AppModel {
            core,
            nav,
            key_binds: key_binds(),
            about,
            config,
            config_context: config_context.clone(),
            config_writer,
            recent_saves: Default::default(),
            context_page: ContextPage::default(),
            playback_extras: Default::default(),
            toasts: widget::toaster::Toasts::new(Message::CloseToast),
            registry,
            mpd_providers,
            provider_list: Vec::new(),
            active_provider_index: None,
            provider_manually_switched: false,
            all_tracks: Vec::new(),
            all_albums: Vec::new(),
            all_artists: Vec::new(),
            library_scanning: false,
            reload_generation: 0,
            library_reload_staging: None,
            library_gen: 0,
            search_index: Default::default(),
            search_debounce_gen: 0,
            folder_tree_gen: None,
            music_dirs_present,
            artists_filtered: false,

            library_search: String::new(),
            search_active: false,
            filtered_albums: Vec::new(),
            filtered_album_map: Vec::new(),
            filtered_artists: Vec::new(),
            filtered_artist_map: Vec::new(),
            filtered_tracks: Vec::new(),
            filtered_track_map: Vec::new(),
            filtered_playlists: Vec::new(),
            filtered_playlist_map: Vec::new(),
            filtered_genres: Vec::new(),
            filtered_genre_map: Vec::new(),
            extras: Default::default(),
            podcasts: Vec::new(),
            podcast_tab: podcasts::PodcastTab::default(),
            podcast_add_open: false,
            podcast_add_url: String::new(),
            podcast_add_error: None,
            refreshing_podcasts: std::collections::HashSet::new(),
            pending_unsubscribe_podcast: None,
            podcast_search_query: String::new(),
            podcast_search_results: Vec::new(),
            podcast_search_loading: false,
            podcast_search_error: None,
            podcast_search_generation: 0,
            selected_podcast: None,
            podcast_episodes: Vec::new(),
            podcast_episode_filter: podcasts::EpisodeFilter::default(),
            podcast_episode_text_filter: String::new(),
            podcast_description_expanded: false,
            current_podcast_episode_id: None,
            last_saved_podcast_position_secs: 0,
            downloading_episodes: std::collections::HashSet::new(),
            radio_stations: Vec::new(),
            radio_tab: radio_view::RadioTab::default(),
            radio_filter: String::new(),
            radio_add_open: false,
            radio_add_name: String::new(),
            radio_add_url: String::new(),
            radio_add_error: None,
            radio_renaming_id: None,
            radio_rename_input: String::new(),
            radio_search_query: String::new(),
            radio_search_tag: None,
            radio_search_country: String::new(),
            radio_locale_country: radio_page::locale_country_code(),
            radio_search_sort: crate::online::radio::SortOrder::default(),
            radio_search_results: Vec::new(),
            radio_search_loading: false,
            radio_search_error: None,
            radio_search_generation: 0,
            radio_now_playing_favicon: String::new(),
            radio_now_playing_key: String::new(),
            online_icons: HashMap::new(),
            player,
            playback_position: Duration::ZERO,
            current_track: None,
            mpris: None,
            seeking_preview: None,
            pre_mute_volume: None,
            scrobble_now_playing_sent: false,
            scrobble_sent: false,
            scrobble,
            settings_search: String::new(),
            selected_album: None,
            selected_artist: None,
            songs_sort: songs::SortField::Title,
            songs_sort_descending: false,
            songs_scroll_offset: 0.0,
            nav_history: Vec::new(),
            favorites_filter: false,
            genre_filter: None,
            playlists: Vec::new(),
            selected_playlist: None,
            new_playlist_name: String::new(),
            rename_playlist_input: String::new(),
            smart_playlists: Vec::new(),
            selected_smart_playlist: None,
            smart_playlist_tracks: Vec::new(),
            smart_playlist_editor: None,
            all_genres: Vec::new(),
            selected_genre: None,
            genre_tracks: Vec::new(),
            folder_state: crate::views::folders::FolderState::default(),
            home: crate::views::home::HomeState::default(),
            home_cache: Default::default(),
            cover_images: HashMap::new(),
            artist_photos: HashMap::new(),
            artist_bios: HashMap::new(),
            artist_info_pending: std::collections::HashSet::new(),
            artist_info_negative: std::collections::HashSet::new(),
            artist_bio_expanded: false,
            artist_info_store: Arc::new(crate::library::artist_info::ArtistInfoStore::open(
                &super::artist_info_data_dir(),
            )),
            artist_info_semaphore: Arc::new(tokio::sync::Semaphore::new(
                super::ARTIST_INFO_CONCURRENCY,
            )),
            text_input_focused: false,
            lyrics_text: None,
            lyrics_loading: false,
            lyrics_overlay_active: false,
            eq_preset: None,
            preset_manager: {
                let presets_dir = dirs::config_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("aulos")
                    .join("eq_presets");
                crate::player::eq_presets::EqPresetManager::new(presets_dir)
                    .expect("Failed to create EQ presets directory")
            },
            all_presets: Vec::new(), // loaded below after construction
            active_preset_name: None,
            eq_dirty: false,
            save_as_name: String::new(),
            autoeq_profiles: Vec::new(),
            autoeq_loading: false,
            autoeq_search: String::new(),
            artist_tag_delimiters_input,
            mpd_edit_states,
            mpd_connection_status,
            subsonic_edit_states,
            subsonic_connection_status,
            subsonic_providers,
            cover_art_bytes: crate::library::palette::CoverByteCache::new(),
            cover_fingerprints: HashMap::new(),
            blurred_cover: None,
            blurred_cover_key: None,
            blur_pending_key: None,
            detail_art: None,
            detail_art_pending: None,
            current_cover_large: None,
            accent: None,
            expand_progress: 0.0,
            expand_target: None,
            expand_anim_start: None,
            #[cfg(feature = "visualizer")]
            visualizer_active: false,
            #[cfg(feature = "visualizer")]
            viz_fullscreen: false,
            #[cfg(feature = "visualizer")]
            viz_prev_nav_active: true,
            #[cfg(feature = "visualizer")]
            viz_frame_buf: {
                let (w, h) = crate::views::now_playing::visualizer::ProjectMRenderer::resolution();
                Arc::new(Mutex::new(
                    crate::views::now_playing::viz_shader::VizFrameBuffer::new(w, h),
                ))
            },
            #[cfg(feature = "visualizer")]
            pcm_buffer,
            #[cfg(feature = "visualizer")]
            mpd_playing,
            #[cfg(feature = "visualizer")]
            viz_cmd_tx,
            #[cfg(feature = "visualizer")]
            viz_cmd_rx_slot,
            #[cfg(feature = "visualizer")]
            viz_current_preset_shared,
            #[cfg(feature = "visualizer")]
            viz_metadata_opacity: 0.0,
            #[cfg(feature = "visualizer")]
            viz_hud_idle_frames: 0,
            #[cfg(feature = "visualizer")]
            viz_hud_pointer_over: false,
            convert_jobs: Vec::new(),
            convert_next_id: 0,
            convert_semaphore: Arc::new(tokio::sync::Semaphore::new(crate::convert::concurrency())),
            convert_dir_error: None,
            #[cfg(feature = "visualizer")]
            viz_browser_open: false,
            #[cfg(feature = "visualizer")]
            viz_preset_entries: Vec::new(),
            #[cfg(feature = "visualizer")]
            viz_presets_scan_started: false,
            #[cfg(feature = "visualizer")]
            viz_preset_search: String::new(),
            #[cfg(feature = "visualizer")]
            viz_locked: false,
            #[cfg(feature = "visualizer")]
            viz_beat_sensitivity: 1.0,
            #[cfg(feature = "visualizer")]
            viz_current_preset_name: None,
        };

        app.rebuild_provider_list();
        app.apply_playback_extras_config();
        app.all_presets = app.preset_manager.load_all();
        // Restore active preset name from config
        if !app.config.active_eq_preset_name.is_empty() {
            app.active_preset_name = Some(app.config.active_eq_preset_name.clone());
        }
        let title_cmd = app.update_title();

        // Trigger initial library scan
        let scan_cmd = cosmic::task::message(cosmic::Action::App(Message::ScanLibrary));

        // Surface any provider construction failures collected above as toasts.
        let mut init_tasks = vec![title_cmd, scan_cmd];
        // A non-Home start page may lazy-load its data (playlists, radio…)
        // exactly like a sidebar selection would.
        if start_page != Page::Home {
            let target = app
                .nav
                .iter()
                .find(|&id| app.nav.data::<Page>(id) == Some(&start_page));
            if let Some(id) = target {
                init_tasks.push(app.select_nav(id));
            }
        }
        if let Some((mpd_servers, subsonic_servers)) = credential_check {
            init_tasks.push(cosmic::task::future(async move {
                let updates = tokio::task::spawn_blocking(move || {
                    run_credential_check(
                        &mpd_servers,
                        &subsonic_servers,
                        crate::credentials::is_keyring_available,
                        crate::credentials::retrieve_password,
                        crate::credentials::store_password,
                    )
                })
                .await
                .unwrap_or_default();
                cosmic::Action::App(Message::CredentialsChecked(updates))
            }));
        }
        for (name, reason) in subsonic_init_errors {
            init_tasks.push(app.push_toast(widget::toaster::Toast::new(fl!(
                "toast-provider-connect-failed",
                provider = name,
                reason = reason
            ))));
        }
        if !flags.open_paths.is_empty() {
            // Files passed on the command line or handed off via
            // `Exec=aulos %U` -- queue them for ad-hoc playback once tags
            // are read, bypassing the library scan/DB entirely.
            init_tasks.push(app.open_files(flags.open_paths));
        }

        (app, Task::batch(init_tasks))
    }

    /// Inserts or removes the Convert nav entry live when the
    /// experimental-converter setting is toggled. Order stays stable:
    /// re-enabling always appends it at the end (same as at startup, since
    /// `nav.insert()` always pushes onto the tail of the display order and
    /// nothing is ever inserted after Convert). If Convert was the active
    /// page when disabled, Albums is activated instead so the view never
    /// falls back to it (also guarded defensively at render time, see
    /// `view::view_page`).
    pub(super) fn set_convert_nav_entry(&mut self, enabled: bool) {
        let entities: Vec<_> = self.nav.iter().collect();
        let convert_entity = entities
            .iter()
            .copied()
            .find(|&id| self.nav.data::<Page>(id) == Some(&Page::Convert));

        if enabled {
            if convert_entity.is_none() {
                insert_convert_nav_entry(&mut self.nav);
            }
        } else if let Some(id) = convert_entity {
            let was_active = self.nav.active() == id;
            self.nav.remove(id);
            if was_active
                && let Some(albums_id) = entities
                    .iter()
                    .copied()
                    .find(|&aid| self.nav.data::<Page>(aid) == Some(&Page::Albums))
            {
                self.nav.activate(albums_id);
            }
        }
    }
}

/// Which kind of provider entry a [`CredentialUpdate`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Mpd,
    Subsonic,
}

/// A change the background keyring check wants applied to the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialUpdate {
    /// The plaintext password was moved into the keyring.
    Migrated(CredentialKind, String),
    /// The entry claimed a keyring password that no longer exists.
    Lost(CredentialKind, String),
}

/// Whether any configured server has a password the keyring check would have
/// to look at (stored in the keyring, or still plaintext). With none, the
/// keyring is not touched at startup at all.
fn needs_credential_check(config: &Config) -> bool {
    config
        .mpd_servers
        .iter()
        .any(|e| e.password_in_keyring || e.password.is_some())
        || config
            .subsonic_servers
            .iter()
            .any(|e| e.password_in_keyring || e.password.is_some())
}

/// Verify/migrate one entry's password; returns the config change, if any.
fn check_credential(
    kind: CredentialKind,
    id: &str,
    in_keyring: bool,
    plaintext: Option<&str>,
    retrieve: &impl Fn(&str) -> Result<Option<String>, String>,
    store: &impl Fn(&str, &str) -> Result<(), String>,
) -> Option<CredentialUpdate> {
    if in_keyring {
        // Verify the keyring entry still exists; reset if lost.
        match retrieve(id) {
            Ok(None) => {
                tracing::warn!(
                    "{kind:?} password for '{id}' was marked as stored in keyring \
                     but the entry is missing; resetting so user can re-enter."
                );
                Some(CredentialUpdate::Lost(kind, id.to_string()))
            }
            Err(e) => {
                tracing::warn!("Failed to verify keyring entry for {kind:?} '{id}': {e}");
                None
            }
            Ok(Some(_)) => None,
        }
    } else if let Some(pw) = plaintext {
        match store(id, pw) {
            Ok(()) => {
                tracing::info!("Migrated {kind:?} password for '{id}' to system keyring");
                Some(CredentialUpdate::Migrated(kind, id.to_string()))
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to migrate {kind:?} password for '{id}' to keyring, \
                     keeping plaintext in config: {e}"
                );
                None
            }
        }
    } else {
        None
    }
}

/// Blocking: probe the keyring, then verify/migrate every server password.
/// Run from `spawn_blocking`; the keyring operations are injected so the
/// logic is testable without a secret service.
pub(super) fn run_credential_check(
    mpd_servers: &[crate::config::MpdConfigEntry],
    subsonic_servers: &[crate::config::SubsonicConfigEntry],
    keyring_available: impl Fn() -> bool,
    retrieve: impl Fn(&str) -> Result<Option<String>, String>,
    store: impl Fn(&str, &str) -> Result<(), String>,
) -> Vec<CredentialUpdate> {
    if !keyring_available() {
        tracing::warn!(
            "System keyring is not available; passwords will remain in plaintext config"
        );
        return Vec::new();
    }
    let mpd = mpd_servers.iter().filter_map(|e| {
        check_credential(
            CredentialKind::Mpd,
            &e.id,
            e.password_in_keyring,
            e.password.as_deref(),
            &retrieve,
            &store,
        )
    });
    let subsonic = subsonic_servers.iter().filter_map(|e| {
        check_credential(
            CredentialKind::Subsonic,
            &e.id,
            e.password_in_keyring,
            e.password.as_deref(),
            &retrieve,
            &store,
        )
    });
    mpd.chain(subsonic).collect()
}

/// Apply the background check's results to `config`; `true` if anything
/// changed (the caller then persists). Each update is only applied if the
/// entry is still in the state the check saw, so a server the user edited
/// meanwhile is left alone.
pub(super) fn apply_credential_updates(config: &mut Config, updates: &[CredentialUpdate]) -> bool {
    let mut changed = false;
    for update in updates {
        match update {
            CredentialUpdate::Migrated(CredentialKind::Mpd, id) => {
                if let Some(e) = config
                    .mpd_servers
                    .iter_mut()
                    .find(|e| &e.id == id && !e.password_in_keyring && e.password.is_some())
                {
                    e.password_in_keyring = true;
                    e.password = None;
                    changed = true;
                }
            }
            CredentialUpdate::Migrated(CredentialKind::Subsonic, id) => {
                if let Some(e) = config
                    .subsonic_servers
                    .iter_mut()
                    .find(|e| &e.id == id && !e.password_in_keyring && e.password.is_some())
                {
                    e.password_in_keyring = true;
                    e.password = None;
                    changed = true;
                }
            }
            CredentialUpdate::Lost(CredentialKind::Mpd, id) => {
                if let Some(e) = config
                    .mpd_servers
                    .iter_mut()
                    .find(|e| &e.id == id && e.password_in_keyring)
                {
                    e.password_in_keyring = false;
                    changed = true;
                }
            }
            CredentialUpdate::Lost(CredentialKind::Subsonic, id) => {
                if let Some(e) = config
                    .subsonic_servers
                    .iter_mut()
                    .find(|e| &e.id == id && e.password_in_keyring)
                {
                    e.password_in_keyring = false;
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Appends the Convert nav entry at the end of `nav`'s display order —
/// shared between the initial (config-gated) build in `init_model` and
/// `AppModel::set_convert_nav_entry`'s live re-enable path.
fn insert_convert_nav_entry(nav: &mut nav_bar::Model) {
    nav.insert()
        .text(fl!("convert"))
        .data::<Page>(Page::Convert)
        .icon(icon::from_name("document-save-as-symbolic"))
        .divider_above(true);
}
#[cfg(test)]
mod credential_tests {
    use super::*;
    use crate::config::{MpdConfigEntry, SubsonicConfigEntry};
    use std::cell::RefCell;

    fn mpd(id: &str, in_keyring: bool, password: Option<&str>) -> MpdConfigEntry {
        MpdConfigEntry {
            id: id.into(),
            password_in_keyring: in_keyring,
            password: password.map(String::from),
            ..Default::default()
        }
    }

    fn subsonic(id: &str, in_keyring: bool, password: Option<&str>) -> SubsonicConfigEntry {
        SubsonicConfigEntry {
            id: id.into(),
            password_in_keyring: in_keyring,
            password: password.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn no_passwords_means_no_keyring_access() {
        let mut config = Config::default();
        assert!(!needs_credential_check(&config));
        config.mpd_servers.push(mpd("a", false, None));
        assert!(!needs_credential_check(&config));
        config
            .subsonic_servers
            .push(subsonic("b", false, Some("pw")));
        assert!(needs_credential_check(&config));
    }

    #[test]
    fn unavailable_keyring_changes_nothing() {
        let stored = RefCell::new(Vec::new());
        let updates = run_credential_check(
            &[mpd("a", false, Some("pw"))],
            &[],
            || false,
            |_| Ok(None),
            |id, _| {
                stored.borrow_mut().push(id.to_string());
                Ok(())
            },
        );
        assert!(updates.is_empty());
        assert!(stored.borrow().is_empty());
    }

    #[test]
    fn migrates_plaintext_and_flags_lost_entries() {
        let updates = run_credential_check(
            &[mpd("plain", false, Some("pw")), mpd("ok", true, None)],
            &[
                subsonic("gone", true, None),
                subsonic("fail", false, Some("x")),
            ],
            || true,
            |id| Ok((id != "gone").then(|| "secret".to_string())),
            |id, _| {
                if id == "fail" {
                    Err("locked".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            updates,
            vec![
                CredentialUpdate::Migrated(CredentialKind::Mpd, "plain".into()),
                CredentialUpdate::Lost(CredentialKind::Subsonic, "gone".into()),
            ]
        );
    }

    #[test]
    fn updates_apply_only_to_entries_still_in_the_checked_state() {
        let mut config = Config::default();
        config.mpd_servers.push(mpd("plain", false, Some("pw")));
        config.mpd_servers.push(mpd("edited", true, None));
        config.subsonic_servers.push(subsonic("gone", true, None));
        let updates = [
            CredentialUpdate::Migrated(CredentialKind::Mpd, "plain".into()),
            // User re-saved this one into the keyring meanwhile: no plaintext
            // left, so a stale "migrated" result must not be applied.
            CredentialUpdate::Migrated(CredentialKind::Mpd, "edited".into()),
            CredentialUpdate::Lost(CredentialKind::Subsonic, "gone".into()),
            CredentialUpdate::Lost(CredentialKind::Subsonic, "unknown".into()),
        ];
        assert!(apply_credential_updates(&mut config, &updates));
        assert!(config.mpd_servers[0].password_in_keyring);
        assert!(config.mpd_servers[0].password.is_none());
        assert!(config.mpd_servers[1].password_in_keyring);
        assert!(!config.subsonic_servers[0].password_in_keyring);
        assert!(!apply_credential_updates(&mut config, &updates));
    }
}
