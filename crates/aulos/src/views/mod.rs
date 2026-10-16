//! Views: the window chrome plus one module per page / sheet / overlay.
//!
//! Every page exposes `State` (its UI-only state, a field of [`State`]),
//! `enter(app)` (called when the route is entered) and
//! `show(app, ui)` (draws into the central area).

pub mod albums;
pub mod artists;
pub mod chrome;
pub mod converter;
pub mod expanded;
pub mod folders;
pub mod genres;
pub mod home;
pub mod menus;
pub mod mini;
pub mod now_bar;
pub mod overlays;
pub mod playlists;
pub mod podcasts;
pub mod properties;
pub mod queue;
pub mod radio;
pub mod settings;
pub mod sidebar;
pub mod songs;
pub mod tabbar;
pub mod tagger;
pub mod toasts;
pub mod toolbar;
#[cfg(feature = "visualizer")]
pub mod visualizer;

use crate::app::{App, Route};

/// UI-only state of every view.
#[derive(Default)]
pub struct State {
    pub home: home::State,
    pub albums: albums::State,
    pub artists: artists::State,
    pub songs: songs::State,
    pub genres: genres::State,
    pub folders: folders::State,
    pub playlists: playlists::State,
    pub podcasts: podcasts::State,
    pub radio: radio::State,
    pub tagger: tagger::State,
    pub converter: converter::State,
    pub settings: settings::State,
    pub queue: queue::State,
    pub expanded: expanded::State,
    pub mini: mini::State,
    pub overlays: overlays::State,
    pub menus: menus::State,
    #[cfg(feature = "visualizer")]
    pub visualizer: visualizer::State,
}

/// Where a side sheet (queue, properties) sits: `(width, top, bottom)` in
/// screen points. Desktop sheets float at the right edge between the toolbar
/// and the now bar; phone sheets span the width and stop above the player
/// strip and tab bar.
pub fn sheet_extent(app: &App, content: egui::Rect, top_gap: f32) -> (f32, f32, f32) {
    use crate::theme::size;
    if app.compact() {
        (
            content.width() - 16.0,
            content.top() + 8.0,
            // The full-screen player has no bars under it.
            if app.expanded {
                content.bottom() - 8.0
            } else {
                content.bottom() - size::MINI_BAR - size::TAB_BAR - 8.0
            },
        )
    } else {
        (
            if app.narrow() { 340.0 } else { 360.0 },
            content.top() + size::TOOLBAR + top_gap,
            content.bottom() - size::NOW_BAR - 8.0,
        )
    }
}

/// Starts whatever loading the page of `route` needs.
/// The index a dragged row ends up at when it is dropped into insert `slot`
/// (the gap before row `slot`; `n` is the gap after the last row), or `None`
/// when the drop leaves it where it was. Removing the row first shifts every
/// later slot down by one.
pub fn drop_target(from: usize, slot: usize) -> Option<usize> {
    let to = if slot > from { slot - 1 } else { slot };
    (to != from).then_some(to)
}

pub fn enter(app: &mut App, route: &Route) {
    match route {
        Route::Home => home::enter(app),
        Route::Albums | Route::Album(_) => albums::enter(app),
        Route::Artists | Route::Artist(_) => artists::enter(app),
        Route::Songs => songs::enter(app),
        Route::Genres | Route::Genre(_) => genres::enter(app),
        Route::Folders => folders::enter(app),
        Route::Playlists => playlists::enter(app),
        Route::Podcasts => podcasts::enter(app),
        Route::Radio => radio::enter(app),
        Route::Tagger => tagger::enter(app),
        Route::Converter => converter::enter(app),
        Route::Settings => settings::enter(app),
    }
}

/// Draws the page of the current route into `ui`.
pub fn show_page(app: &mut App, ui: &mut egui::Ui) {
    match app.route.clone() {
        Route::Home => home::show(app, ui),
        Route::Albums | Route::Album(_) => albums::show(app, ui),
        Route::Artists | Route::Artist(_) => artists::show(app, ui),
        Route::Songs => songs::show(app, ui),
        Route::Genres | Route::Genre(_) => genres::show(app, ui),
        Route::Folders => folders::show(app, ui),
        Route::Playlists => playlists::show(app, ui),
        Route::Podcasts => podcasts::show(app, ui),
        Route::Radio => radio::show(app, ui),
        Route::Tagger => tagger::show(app, ui),
        Route::Converter => converter::show(app, ui),
        Route::Settings => settings::show(app, ui),
    }
}

#[cfg(test)]
mod tests {
    use super::drop_target;

    /// Applies the move the way `Vec`-backed lists (queue, playlists) do.
    fn dropped(rows: &[char], from: usize, slot: usize) -> Vec<char> {
        let mut v = rows.to_vec();
        if let Some(to) = drop_target(from, slot) {
            let row = v.remove(from);
            v.insert(to, row);
        }
        v
    }

    #[test]
    fn dropping_into_a_slot_lands_the_row_there() {
        let rows = ['a', 'b', 'c', 'd'];
        assert_eq!(dropped(&rows, 0, 3), ['b', 'c', 'a', 'd']);
        assert_eq!(dropped(&rows, 0, 4), ['b', 'c', 'd', 'a']);
        assert_eq!(dropped(&rows, 3, 0), ['d', 'a', 'b', 'c']);
        assert_eq!(dropped(&rows, 2, 1), ['a', 'c', 'b', 'd']);
    }

    #[test]
    fn dropping_beside_itself_is_not_a_move() {
        assert_eq!(drop_target(2, 2), None);
        assert_eq!(drop_target(2, 3), None);
        assert_eq!(drop_target(0, 0), None);
    }
}
