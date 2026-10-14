// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::{APP_ICON, AppFlags, AppModel, ContextPage, Message, Page, REPOSITORY, key_binds};
use aulos_core::fl;
use aulos_core::library::state::SortField;
use aulos_core::session::Session;
use cosmic::prelude::*;
use cosmic::widget::{self, about::About, icon, nav_bar};
use std::collections::HashMap;
#[cfg(feature = "visualizer")]
use std::sync::{Arc, Mutex};

impl AppModel {
    pub(super) fn init_model(
        core: cosmic::Core,
        flags: AppFlags,
    ) -> (Self, Task<cosmic::Action<Message>>) {
        // Settings, library database, providers, player, stores: the domain
        // state of a real launch (needs this runtime), with the servers that
        // failed to construct.
        let (session, provider_init_errors) = Session::bootstrap();
        crate::views::common::set_grid_scale(session.config.config.grid_scale);

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

        nav.insert()
            .text(fl!("tagger-nav"))
            .data::<Page>(Page::Tagger)
            .icon(icon::from_name("document-edit-symbolic"))
            .divider_above(true);

        let about = About::default()
            .name(fl!("app-title"))
            .comments(fl!("app-motto"))
            .icon(widget::icon::from_svg_bytes(APP_ICON))
            .version(env!("CARGO_PKG_VERSION"))
            .links([(fl!("repository"), REPOSITORY)])
            .license("GPL-3.0");

        if session.config.config.experimental_converter {
            insert_convert_nav_entry(&mut nav);
        }

        // Open on the configured section (Home by default); unknown or
        // unavailable choices fall back to Home.
        let start_page = super::startup::resolve_startup_page(
            &session.config.config.startup_page,
            &session.config.config.last_view,
            |page| nav.iter().any(|id| nav.data::<Page>(id) == Some(page)),
        );
        let start_id = nav
            .iter()
            .find(|&id| nav.data::<Page>(id) == Some(&start_page));
        if let Some(id) = start_id {
            nav.activate(id);
        }

        // Command channel to the lactea render thread — replaces the
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
            std::sync::mpsc::channel::<aulos_core::visualizer::VizCommand>();
        #[cfg(feature = "visualizer")]
        let viz_cmd_rx_slot = Arc::new(Mutex::new(Some(viz_cmd_rx)));
        // Preset file the render thread most recently put on screen;
        // written there, mirrored into `AppModel::viz_current_preset`
        // on every `VisualizerFrameReady`.
        #[cfg(feature = "visualizer")]
        let viz_current_preset_shared: Arc<Mutex<Option<std::path::PathBuf>>> =
            Arc::new(Mutex::new(None));

        let artist_tag_delimiters_input = session.config.config.artist_tag_delimiters.join(" | ");

        let mut app = AppModel {
            core,
            nav,
            key_binds: key_binds(),
            about,
            context_page: ContextPage::default(),
            session,
            toasts: widget::toaster::Toasts::new(Message::CloseToast),
            search_active: false,
            extras: Default::default(),
            podcast_tab: aulos_core::online::podcast::PodcastTab::default(),
            podcast_add_open: false,
            podcast_add_url: String::new(),
            podcast_add_error: None,
            pending_unsubscribe_podcast: None,
            podcast_search_query: String::new(),
            podcast_episode_filter: aulos_core::online::podcast::EpisodeFilter::default(),
            podcast_episode_text_filter: String::new(),
            podcast_description_expanded: false,
            radio_tab: aulos_core::online::radio::RadioTab::default(),
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
            radio_search_sort: aulos_core::online::radio::SortOrder::default(),
            online_icons: HashMap::new(),
            seeking_preview: None,
            settings_search: String::new(),
            selected_album: None,
            selected_artist: None,
            songs_sort: SortField::Title,
            songs_sort_descending: false,
            songs_scroll_offset: 0.0,
            nav_history: Vec::new(),
            favorites_filter: false,
            genre_filter: None,
            selected_playlist: None,
            new_playlist_name: String::new(),
            rename_playlist_input: String::new(),
            selected_smart_playlist: None,
            folder_state: aulos_core::library::folder_tree::FolderState::default(),
            cover_images: HashMap::new(),
            artist_photos: HashMap::new(),
            artist_bio_expanded: false,
            text_input_focused: false,
            lyrics_overlay_active: false,
            save_as_name: String::new(),
            autoeq_search: String::new(),
            artist_tag_delimiters_input,
            acoustid_user_key_input: String::new(),
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
                let (w, h) = crate::views::now_playing::visualizer::VizRenderer::resolution();
                Arc::new(Mutex::new(
                    crate::views::now_playing::viz_shader::VizFrameBuffer::new(w, h),
                ))
            },
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
            tagger_thumbs: crate::views::tagger::Thumbs::default(),
            #[cfg(feature = "visualizer")]
            viz_browser_open: false,
            #[cfg(feature = "visualizer")]
            viz_preset_entries: Vec::new(),
            #[cfg(feature = "visualizer")]
            viz_preset_search: String::new(),
            #[cfg(feature = "visualizer")]
            viz_locked: false,
            #[cfg(feature = "visualizer")]
            viz_beat_sensitivity: 1.0,
            #[cfg(feature = "visualizer")]
            viz_current_preset: None,
            #[cfg(feature = "visualizer")]
            viz_presets_scanned: false,
            #[cfg(feature = "visualizer")]
            viz_preset_scroll: 0.0,
        };

        let title_cmd = app.update_title();

        // What the backend kicks off at launch: the first-launch jingle, the
        // library scan and the keyring checks (their results arrive as
        // session events).
        let start_cmd = super::tasks::session_effect(app.session.start());

        let mut init_tasks = vec![title_cmd, start_cmd];
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
        // Surface any provider construction failures as toasts.
        for (name, reason) in provider_init_errors {
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

/// Appends the Convert nav entry at the end of `nav`'s display order (right
/// after the Tag Editor, which already carries the divider) — shared
/// between the initial (config-gated) build in `init_model` and
/// `AppModel::set_convert_nav_entry`'s live re-enable path.
fn insert_convert_nav_entry(nav: &mut nav_bar::Model) {
    nav.insert()
        .text(fl!("convert"))
        .data::<Page>(Page::Convert)
        .icon(icon::from_name("document-save-as-symbolic"));
}
