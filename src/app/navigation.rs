// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Cross-view navigation: resolving [`Route`]s (artist / album / genre links
//! clicked anywhere in the UI) to a page + detail selection, and the
//! history that lets "Back" return to where the link was clicked.

use super::{AppModel, Location, Message, Page};
use crate::views::Route;
use cosmic::widget;
use cosmic::widget::nav_bar;
use cosmic::{Application, Task};

impl AppModel {
    /// Album key whose cover drives the hero header of the detail page
    /// currently shown, if any: the album itself, or an artist's first
    /// album that has cover bytes.
    fn detail_cover_key(&mut self) -> Option<String> {
        match self.nav.active_data::<Page>()? {
            Page::Albums => {
                let album = self.all_albums.get(self.selected_album?)?;
                Some(crate::library::CoverArt::album_key(
                    &album.artist,
                    &album.name,
                ))
            }
            Page::Artists => {
                let artist = self.all_artists.get(self.selected_artist?)?;
                let keys: Vec<String> = artist
                    .albums
                    .iter()
                    .map(|a| crate::library::CoverArt::album_key(&artist.name, &a.name))
                    .collect();
                // `cover_images` holds a handle exactly for the albums that
                // have art, so it identifies the first one with a cover
                // without loading any full-size bytes.
                keys.into_iter().find(|k| self.cover_images.contains_key(k))
            }
            _ => None,
        }
    }

    /// Lightweight album for the detail page's cover `key` (the selected
    /// album, or the artist's album whose key matches).
    fn detail_cover_hint(&self, key: &str) -> Option<crate::library::Album> {
        match self.nav.active_data::<Page>()? {
            Page::Albums => self.all_albums.get(self.selected_album?),
            Page::Artists => {
                let artist = self.all_artists.get(self.selected_artist?)?;
                artist
                    .albums
                    .iter()
                    .find(|a| crate::library::CoverArt::album_key(&artist.name, &a.name) == key)
            }
            _ => None,
        }
        .map(crate::library::Album::cover_hint)
    }

    /// Kick off (once per album) the blur + accent extraction for the
    /// visible detail page's hero header. Cheap no-op otherwise, so it is
    /// safe to call after every `update`.
    pub(super) fn maybe_update_detail_art(&mut self) -> Task<cosmic::Action<Message>> {
        let Some(key) = self.detail_cover_key() else {
            return Task::none();
        };
        if self.detail_art.as_ref().is_some_and(|d| d.key == key)
            || self.detail_art_pending.as_ref() == Some(&key)
        {
            return Task::none();
        }
        let Some(bytes) = self.cover_art_bytes.get(&key) else {
            // Not cached: load the full-size bytes lazily. `CoverBytesLoaded`
            // re-runs this (it is evaluated after every update).
            if !self.cover_art_bytes.should_load(&key) {
                return Task::none();
            }
            let hint = self.detail_cover_hint(&key);
            return self.request_cover_bytes(key, |_| hint);
        };
        self.detail_art_pending = Some(key.clone());
        cosmic::task::future(async move {
            let (blurred, accent) = tokio::task::spawn_blocking(move || {
                (
                    crate::views::now_playing::blur::compute_blurred_cover(&bytes),
                    crate::library::palette::extract(&bytes),
                )
            })
            .await
            .unwrap_or((None, None));
            let handle = blurred.map(|(w, h, px)| widget::icon::from_raster_pixels(w, h, px));
            cosmic::Action::App(Message::DetailArtReady(key, handle, accent))
        })
    }

    pub(super) fn apply_detail_art(
        &mut self,
        key: String,
        blurred: Option<widget::icon::Handle>,
        accent: Option<crate::library::palette::Accent>,
    ) {
        if self.detail_art_pending.as_ref() == Some(&key) {
            self.detail_art_pending = None;
        }
        self.detail_art = Some(super::DetailArt {
            key,
            blurred,
            accent: accent.map(|a| a.legible(cosmic::theme::active().cosmic().is_dark)),
        });
    }

    /// Hero artwork for the detail page being drawn, if it is ready and
    /// belongs to `key`.
    pub(super) fn detail_hero(
        &self,
        key: &str,
    ) -> Option<(
        Option<&widget::icon::Handle>,
        Option<&crate::library::palette::Accent>,
    )> {
        self.detail_art
            .as_ref()
            .filter(|d| d.key == key)
            .map(|d| (d.blurred.as_ref(), d.accent.as_ref()))
    }
}

impl AppModel {
    /// What is on screen, for detecting that a page/detail/layout change
    /// remounted the scrollables (see [`Self::restore_scroll_positions`]).
    pub(super) fn view_signature(&self) -> (Location, [crate::config::ViewMode; 3], bool) {
        (
            self.current_location(),
            [
                self.config.albums_view_mode,
                self.config.artists_view_mode,
                self.config.genres_view_mode,
            ],
            self.home.decade.is_some(),
        )
    }

    /// Scroll every virtualized list back to its remembered offset.
    ///
    /// The virtual grids/lists build only the rows around the stored
    /// offset, but a freshly mounted scrollable starts at the top: without
    /// this, returning to a page you had scrolled would show blank space
    /// until the next scroll event. Targets that aren't on screen are
    /// simply not found, so it's safe to send all of them.
    pub(super) fn restore_scroll_positions(&self) -> Task<cosmic::Action<Message>> {
        use cosmic::iced::widget::scrollable::{AbsoluteOffset, scroll_to};
        let to = |y: f32| AbsoluteOffset {
            x: None,
            y: Some(y),
        };
        let mut tasks: Vec<Task<cosmic::Action<Message>>> = self
            .extras
            .grid_scroll
            .iter()
            .map(|(area, y)| scroll_to(area.id(), to(y)))
            .collect();
        tasks.push(scroll_to(
            crate::views::common::songs_scroll_id(),
            to(self.songs_scroll_offset),
        ));
        Task::batch(tasks)
    }
}

/// Upper bound on remembered locations; old entries are dropped first.
const MAX_HISTORY: usize = 64;

impl AppModel {
    /// Where the user is right now.
    pub(super) fn current_location(&self) -> Location {
        Location {
            page: self
                .nav
                .active_data::<Page>()
                .cloned()
                .unwrap_or(Page::Albums),
            album: self.selected_album,
            artist: self.selected_artist,
            genre: self.selected_genre,
        }
    }

    /// Sidebar entity showing `page`, if that page is currently in the nav.
    fn page_entity(&self, page: &Page) -> Option<nav_bar::Id> {
        self.nav
            .iter()
            .find(|&id| self.nav.data::<Page>(id).is_some_and(|p| p == page))
    }

    /// Switch the visible page without the sidebar side effects of
    /// `select_nav` beyond resetting detail selections and collapsing the
    /// now-playing sheet — history is kept intact.
    fn show_page(&mut self, page: &Page) -> Task<cosmic::Action<Message>> {
        let Some(id) = self.page_entity(page) else {
            return Task::none();
        };
        let history = std::mem::take(&mut self.nav_history);
        let task = self.select_nav(id);
        self.nav_history = history;
        task
    }

    /// Resolve and open `route`, remembering the current location so the
    /// destination's Back button returns here.
    pub(super) fn navigate(&mut self, route: Route) -> Task<cosmic::Action<Message>> {
        let here = self.current_location();
        let task = match route {
            Route::Artist(name) => match self.find_artist(&name) {
                Some(idx) => {
                    let show = self.show_page(&Page::Artists);
                    show.chain(self.update(Message::SelectArtist(idx)))
                }
                None => return self.not_found(&name),
            },
            Route::Album { artist, album } => match self.find_album(&artist, &album) {
                Some(idx) => {
                    let show = self.show_page(&Page::Albums);
                    self.selected_album = Some(idx);
                    show
                }
                None => return self.not_found(&album),
            },
            Route::Genre(name) => {
                match self
                    .all_genres
                    .iter()
                    .position(|g| g.eq_ignore_ascii_case(&name))
                {
                    Some(idx) => {
                        let show = self.show_page(&Page::Genres);
                        show.chain(self.update(Message::SelectGenre(idx)))
                    }
                    // Genre list not loaded (or provider without genre
                    // support): fall back to the genre-filtered Songs view.
                    None => self.update(Message::FilterByGenre(name)),
                }
            }
        };
        if self.current_location() != here {
            if self.nav_history.len() >= MAX_HISTORY {
                self.nav_history.remove(0);
            }
            self.nav_history.push(here);
        }
        task
    }

    /// Return to the location a link was followed from. Returns `None` when
    /// there is no history, so callers fall back to their plain "up one
    /// level" behaviour.
    pub(super) fn go_back(&mut self) -> Option<Task<cosmic::Action<Message>>> {
        let loc = self.nav_history.pop()?;
        let mut task = self.show_page(&loc.page);
        self.selected_album = loc.album;
        if let Some(idx) = loc.artist {
            task = task.chain(self.update(Message::SelectArtist(idx)));
        }
        if let Some(idx) = loc.genre {
            task = task.chain(self.update(Message::SelectGenre(idx)));
        }
        Some(task)
    }

    /// Forget link history — the user chose a page from the sidebar.
    pub(super) fn clear_nav_history(&mut self) {
        self.nav_history.clear();
    }

    fn find_artist(&self, name: &str) -> Option<usize> {
        let name = name.trim();
        let exact = |n: &str| {
            self.all_artists
                .iter()
                .position(|a| a.name.trim().eq_ignore_ascii_case(n))
        };
        exact(name).or_else(|| {
            // "A feat. B", "A & B", "A, B"… → try the primary artist.
            primary_artist(name).and_then(exact)
        })
    }

    fn find_album(&self, artist: &str, album: &str) -> Option<usize> {
        let eq = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
        self.all_albums
            .iter()
            .position(|a| eq(&a.name, album) && eq(&a.artist, artist))
            .or_else(|| self.all_albums.iter().position(|a| eq(&a.name, album)))
    }

    fn not_found(&mut self, what: &str) -> Task<cosmic::Action<Message>> {
        tracing::debug!("navigation target not in library: {what}");
        Task::none()
    }
}

/// The leading artist of a collaboration credit, if the credit has one.
fn primary_artist(name: &str) -> Option<&str> {
    const SEPARATORS: [&str; 8] = [
        " feat. ",
        " feat ",
        " ft. ",
        " featuring ",
        " & ",
        ", ",
        "; ",
        " / ",
    ];
    // ASCII-only lowering keeps byte offsets valid for slicing `name`.
    let lower = name.to_ascii_lowercase();
    SEPARATORS
        .iter()
        .filter_map(|sep| lower.find(sep))
        .min()
        .map(|pos| name[..pos].trim())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::primary_artist;

    #[test]
    fn primary_artist_splits_common_credits() {
        assert_eq!(
            primary_artist("Daft Punk feat. Pharrell"),
            Some("Daft Punk")
        );
        assert_eq!(primary_artist("Simon & Garfunkel"), Some("Simon"));
        assert_eq!(primary_artist("A, B; C"), Some("A"));
        assert_eq!(primary_artist("Solo"), None);
    }
}
