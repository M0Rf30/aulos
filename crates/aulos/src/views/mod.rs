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

/// Starts whatever loading the page of `route` needs.
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
