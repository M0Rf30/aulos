// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Startup preferences: which section Aulos opens on, and which provider is
//! active at launch. Holds the stable page ↔ key mapping (also used to
//! persist the last used section in `Config::last_view`), the fallback
//! rules, and the "Startup" block of the Settings drawer.

use super::{AppModel, Page};
use crate::config::Config;
use crate::fl;
use cosmic::Task;
use cosmic::widget;

/// Stable, persisted key of a page.
pub fn page_key(page: &Page) -> &'static str {
    match page {
        Page::Home => "home",
        Page::Albums => "albums",
        Page::Artists => "artists",
        Page::Songs => "songs",
        Page::Playlists => "playlists",
        Page::SmartPlaylists => "smart_playlists",
        Page::Genres => "genres",
        Page::Folders => "folders",
        Page::Podcasts => "podcasts",
        Page::Radio => "radio",
        Page::Convert => "convert",
    }
}

/// Inverse of [`page_key`]; `None` for unknown keys.
pub fn page_from_key(key: &str) -> Option<Page> {
    Some(match key.trim() {
        "home" => Page::Home,
        "albums" => Page::Albums,
        "artists" => Page::Artists,
        "songs" => Page::Songs,
        "playlists" => Page::Playlists,
        "smart_playlists" => Page::SmartPlaylists,
        "genres" => Page::Genres,
        "folders" => Page::Folders,
        "podcasts" => Page::Podcasts,
        "radio" => Page::Radio,
        "convert" => Page::Convert,
        _ => return None,
    })
}

/// Page to open at startup. `startup` is `Config::startup_page` (`"home"`,
/// `"last"`, or a page key), `last_view` the persisted last used page, and
/// `available` tells whether a page is currently in the sidebar (e.g. the
/// experimental Convert page may be disabled). Anything unknown or
/// unavailable falls back to Home.
pub fn resolve_startup_page(
    startup: &str,
    last_view: &str,
    available: impl Fn(&Page) -> bool,
) -> Page {
    let wanted = match startup.trim() {
        "last" => page_from_key(last_view),
        key => page_from_key(key),
    };
    match wanted {
        Some(page) if page == Page::Home || available(&page) => page,
        _ => Page::Home,
    }
}

/// Options of the "Start on" dropdown, in display order: Home, Last used,
/// then every page that can be chosen as a startup section.
pub const STARTUP_OPTIONS: [&str; 11] = [
    "home",
    "last",
    "albums",
    "artists",
    "songs",
    "playlists",
    "smart_playlists",
    "genres",
    "folders",
    "podcasts",
    "radio",
];

/// Index of `startup_page` in [`STARTUP_OPTIONS`] (unknown → Home).
pub fn startup_option_index(startup_page: &str) -> usize {
    STARTUP_OPTIONS
        .iter()
        .position(|k| *k == startup_page.trim())
        .unwrap_or(0)
}

fn option_label(key: &str) -> String {
    match key {
        "last" => fl!("startup-last-used"),
        "home" => fl!("home"),
        "albums" => fl!("albums"),
        "artists" => fl!("artists"),
        "songs" => fl!("songs"),
        "playlists" => fl!("playlists"),
        "smart_playlists" => fl!("smart-playlists"),
        "genres" => fl!("genres"),
        "folders" => fl!("folders"),
        "podcasts" => fl!("podcasts"),
        "radio" => fl!("radio"),
        other => other.to_string(),
    }
}

/// Options of the "Provider at startup" dropdown as `(value, label)`:
/// `"last"`, `"local"`, then every configured MPD and Subsonic server.
pub fn provider_options(config: &Config) -> Vec<(String, String)> {
    let mut options = vec![
        ("last".to_string(), fl!("startup-last-used")),
        ("local".to_string(), fl!("startup-provider-local")),
    ];
    options.extend(
        config
            .mpd_servers
            .iter()
            .map(|s| (s.id.clone(), s.name.clone())),
    );
    options.extend(
        config
            .subsonic_servers
            .iter()
            .map(|s| (s.id.clone(), s.name.clone())),
    );
    options
}

/// Index of the saved `startup_provider` among `options` (a server that was
/// removed from the config reads as "Last used").
pub fn provider_option_index(options: &[(String, String)], saved: &str) -> usize {
    let saved = if saved.trim().is_empty() {
        "last"
    } else {
        saved
    };
    options.iter().position(|(v, _)| v == saved).unwrap_or(0)
}

/// Messages of the Startup settings block (indices into the option lists).
#[derive(Debug, Clone)]
pub enum StartupMessage {
    SetPage(usize),
    SetProvider(usize),
}

impl AppModel {
    /// The "Startup" block of the Settings drawer.
    pub(super) fn startup_settings(&self) -> cosmic::Element<'_, StartupMessage> {
        let page_labels: Vec<String> = STARTUP_OPTIONS.iter().map(|k| option_label(k)).collect();
        let provider_opts = provider_options(&self.config);
        let provider_labels: Vec<String> = provider_opts.iter().map(|(_, l)| l.clone()).collect();
        let provider_selected =
            provider_option_index(&provider_opts, &self.config.startup_provider);

        widget::settings::section()
            .title(fl!("settings-startup"))
            .add(
                widget::settings::item::builder(fl!("startup-page"))
                    .description(fl!("startup-page-description"))
                    .control(widget::dropdown(
                        page_labels,
                        Some(startup_option_index(&self.config.startup_page)),
                        StartupMessage::SetPage,
                    )),
            )
            .add(
                widget::settings::item::builder(fl!("startup-provider"))
                    .description(fl!("startup-provider-description"))
                    .control(widget::dropdown(
                        provider_labels,
                        Some(provider_selected),
                        StartupMessage::SetProvider,
                    )),
            )
            .into()
    }

    /// Apply a change made in the Startup settings block.
    pub(super) fn update_startup(
        &mut self,
        msg: StartupMessage,
    ) -> Task<cosmic::Action<super::Message>> {
        match msg {
            StartupMessage::SetPage(i) => {
                if let Some(key) = STARTUP_OPTIONS.get(i) {
                    self.config.startup_page = (*key).to_string();
                    self.save_config();
                }
            }
            StartupMessage::SetProvider(i) => {
                if let Some((value, _)) = provider_options(&self.config).get(i) {
                    self.config.startup_provider = value.clone();
                    self.save_config();
                }
            }
        }
        Task::none()
    }

    /// Persist the active page as `last_view`, only when it changed.
    pub(super) fn remember_page(&mut self) {
        let Some(page) = self.nav.active_data::<Page>() else {
            return;
        };
        let key = page_key(page);
        if self.config.last_view != key {
            self.config.last_view = key.to_string();
            self.save_config();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_PAGES: [Page; 11] = [
        Page::Home,
        Page::Albums,
        Page::Artists,
        Page::Songs,
        Page::Playlists,
        Page::SmartPlaylists,
        Page::Genres,
        Page::Folders,
        Page::Podcasts,
        Page::Radio,
        Page::Convert,
    ];

    #[test]
    fn page_keys_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for page in ALL_PAGES {
            let key = page_key(&page);
            assert!(seen.insert(key), "duplicate key {key}");
            assert_eq!(page_from_key(key), Some(page));
        }
        assert_eq!(page_from_key("nope"), None);
        assert_eq!(page_from_key(""), None);
    }

    #[test]
    fn startup_options_are_valid_keys_with_home_and_last_first() {
        assert_eq!(STARTUP_OPTIONS[0], "home");
        assert_eq!(STARTUP_OPTIONS[1], "last");
        for key in &STARTUP_OPTIONS[2..] {
            let page = page_from_key(key).expect("option is a page key");
            assert_ne!(page, Page::Convert);
            assert_eq!(page_key(&page), *key);
        }
        assert_eq!(startup_option_index("radio"), 10);
        assert_eq!(startup_option_index("last"), 1);
        assert_eq!(startup_option_index("garbage"), 0);
    }

    fn all(_: &Page) -> bool {
        true
    }

    #[test]
    fn default_and_explicit_pages_resolve() {
        assert_eq!(resolve_startup_page("home", "songs", all), Page::Home);
        assert_eq!(resolve_startup_page("albums", "songs", all), Page::Albums);
        assert_eq!(
            resolve_startup_page("smart_playlists", "", all),
            Page::SmartPlaylists
        );
    }

    #[test]
    fn last_restores_last_view_with_home_fallback() {
        assert_eq!(resolve_startup_page("last", "genres", all), Page::Genres);
        assert_eq!(resolve_startup_page("last", "garbage", all), Page::Home);
        assert_eq!(resolve_startup_page("last", "", all), Page::Home);
        assert_eq!(resolve_startup_page("last", "home", all), Page::Home);
    }

    #[test]
    fn unknown_or_unavailable_pages_fall_back_to_home() {
        assert_eq!(resolve_startup_page("bogus", "albums", all), Page::Home);
        let no_convert = |p: &Page| *p != Page::Convert;
        assert_eq!(
            resolve_startup_page("last", "convert", no_convert),
            Page::Home
        );
        assert_eq!(resolve_startup_page("convert", "", no_convert), Page::Home);
        assert_eq!(resolve_startup_page("convert", "", all), Page::Convert);
        let none = |_: &Page| false;
        assert_eq!(resolve_startup_page("albums", "", none), Page::Home);
    }

    #[test]
    fn provider_option_index_handles_missing_and_empty() {
        let options = vec![
            ("last".to_string(), "Last used".to_string()),
            ("local".to_string(), "Local".to_string()),
            ("mpd-home".to_string(), "Home MPD".to_string()),
        ];
        assert_eq!(provider_option_index(&options, "last"), 0);
        assert_eq!(provider_option_index(&options, ""), 0);
        assert_eq!(provider_option_index(&options, "local"), 1);
        assert_eq!(provider_option_index(&options, "mpd-home"), 2);
        assert_eq!(provider_option_index(&options, "removed-server"), 0);
    }
}
