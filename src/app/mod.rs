// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use aulos_core::library::state::SortField;
use cosmic::widget::{self, about::About, menu, nav_bar};
use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(feature = "visualizer")]
use std::sync::{Arc, Mutex};

mod application;
mod convert_page;
mod helpers;
mod home;
mod init;
mod library_glue;
mod message;
mod navigation;
pub mod playback_extras;
mod playlist_io;
mod podcast_page;
mod properties;
mod radio_page;
mod scrobble_glue;
pub mod startup;
mod subscriptions;
mod tagger_page;
mod tasks;
mod update;
mod view;
mod view_extras;

pub use message::Message;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const APP_ICON: &[u8] =
    include_bytes!("../../resources/icons/hicolor/scalable/apps/io.github.m0rf30.Aulos.svg");

/// Widget id for the header library-search input, used to programmatically
/// focus it when the search bar is activated.
const SEARCH_INPUT_ID: &str = "aulos-library-search";

/// Frames of no mouse movement (at the visualizer's ~30fps render cadence)
/// before the fullscreen HUD control card auto-hides. ~3 seconds.
#[cfg(feature = "visualizer")]
const VIZ_HUD_HOLD_FRAMES: u32 = 90;

/// Main application model.
pub struct AppModel {
    core: cosmic::Core,
    nav: nav_bar::Model,
    key_binds: HashMap<menu::KeyBind, MenuAction>,
    about: About,
    context_page: ContextPage,
    /// All domain state (settings, providers, library, playback, playlists,
    /// podcasts, radio, equalizer, artist info, conversion, tag editor,
    /// properties); the rest of this struct is view state.
    session: aulos_core::session::Session,

    // Notifications
    /// Toast notifications (e.g. provider connection failures).
    toasts: widget::toaster::Toasts<Message>,

    // Library search (header search bar)
    /// Whether the header search input is visible/active.
    search_active: bool,
    /// Mini-player mode and album-filter chip state (see `view_extras`).
    extras: view_extras::ViewExtras,

    // Podcasts
    /// Which of the two Podcasts tabs is shown; kept for the session (not
    /// persisted to disk).
    podcast_tab: aulos_core::online::podcast::PodcastTab,
    /// Whether the inline add-by-URL card is open.
    podcast_add_open: bool,
    podcast_add_url: String,
    /// Inline validation error for the add-by-URL form.
    podcast_add_error: Option<String>,
    /// Podcast id awaiting an inline unsubscribe confirmation, if any.
    pending_unsubscribe_podcast: Option<i64>,

    // Podcast discovery (Discover tab)
    podcast_search_query: String,

    // Podcast show detail (the open show itself is
    // `Session::podcasts.selected`)
    podcast_episode_filter: aulos_core::online::podcast::EpisodeFilter,
    podcast_episode_text_filter: String,
    /// Whether the show detail's description is expanded past its clipped
    /// preview.
    podcast_description_expanded: bool,

    // Radio
    /// Which of the two Radio tabs is shown; kept for the session (not
    /// persisted to disk).
    radio_tab: aulos_core::online::radio::RadioTab,
    /// Live filter text over `radio_stations` in the "My stations" tab.
    radio_filter: String,
    /// Whether the inline add-by-URL card is open.
    radio_add_open: bool,
    radio_add_name: String,
    radio_add_url: String,
    /// Inline validation error for the add-by-URL form.
    radio_add_error: Option<String>,
    /// Station id currently being renamed, and its live-edited name.
    radio_renaming_id: Option<i64>,
    radio_rename_input: String,

    // Radio discovery (Discover tab)
    radio_search_query: String,
    /// Selected quick-tag chip, if any.
    radio_search_tag: Option<&'static str>,
    /// Country-code filter; empty means "any country". Toggled between
    /// `""` and `radio_locale_country`.
    radio_search_country: String,
    radio_search_sort: aulos_core::online::radio::SortOrder,

    /// Podcast artwork / radio favicon bytes, keyed by their source URL.
    /// Shared between both views since the icons are the same kind of
    /// small, best-effort raster image loaded from a remote URL.
    online_icons: HashMap<String, widget::icon::Handle>,

    // Player
    /// While the user is dragging the seek slider, holds the preview fraction
    /// (0.0–1.0). `None` when not dragging. The actual backend seek happens
    /// only on release (`SeekCommit`).
    seeking_preview: Option<f32>,

    // Scrobble state (Subsonic)
    /// Text of the Settings drawer's search box.
    settings_search: String,

    // View state
    selected_album: Option<usize>,
    selected_artist: Option<usize>,
    songs_sort: SortField,
    /// When true, the Songs list column headers show a descending arrow.
    songs_sort_descending: bool,
    /// Current vertical scroll offset of the Songs list (virtualization).
    songs_scroll_offset: f32,
    /// Locations to return to when leaving a detail view that was reached
    /// through a cross-view link (album → artist → …). Cleared whenever
    /// the user picks a page from the sidebar.
    nav_history: Vec<Location>,
    /// When true, the Songs view shows only favorite tracks.
    favorites_filter: bool,
    /// When set, the Songs view shows only tracks matching this genre.
    genre_filter: Option<String>,
    /// Currently selected playlist index (for detail view).
    selected_playlist: Option<usize>,
    /// Text input for new playlist name.
    new_playlist_name: String,
    /// Live-edited text for the rename field in `playlist_detail_view`,
    /// seeded from the playlist's current name when its detail view opens.
    rename_playlist_input: String,
    /// Currently selected smart playlist index (for detail/editor view).
    selected_smart_playlist: Option<usize>,
    /// In-memory directory-hierarchy browse state for the Folders view;
    /// rebuilt from the library tracks whenever the page is opened or the
    /// library reloads (see `FolderTree::build`).
    folder_state: aulos_core::library::folder_tree::FolderState,
    cover_images: HashMap<String, widget::icon::Handle>,
    /// Cached real artist photos (from Subsonic's own artist data or, in
    /// Local/MPD mode, Deezer via `aulos_core::library::artist_info` when
    /// `Config::fetch_artist_info` is on), keyed by artist name. Missing
    /// entries fall back to the deterministic-color initials avatar
    /// built entirely from widgets — see `crate::views::common::artist_avatar`.
    artist_photos: HashMap<String, widget::image::Handle>,
    /// Whether the selected artist's biography preview is expanded past
    /// its clipped preview; reset on selection/back navigation.
    artist_bio_expanded: bool,

    // Keyboard input state
    /// Tracks whether any text input field currently has keyboard focus.
    /// When true, space bar should type a space character instead of toggling playback.
    text_input_focused: bool,

    // Lyrics
    /// When true and the expanded now-playing view is active, lyrics render
    /// as an in-view overlay (over the cover art / visualizer) instead of
    /// opening the generic context-drawer sidebar, keeping the immersive
    /// full view intact.
    lyrics_overlay_active: bool,

    // Equalizer
    save_as_name: String,

    // AutoEQ
    /// Current search query for filtering AutoEQ profiles in the dropdown.
    autoeq_search: String,

    // Settings — multi-artist tag splitting
    /// Live-edited text for the delimiter list editor in the Settings
    /// drawer, seeded from `config.artist_tag_delimiters.join(" | ")` and
    /// only committed into `config` on submit.
    artist_tag_delimiters_input: String,

    // Settings — tag editor
    /// Live text of the user's AcoustID API key box (never pre-filled: the
    /// key lives only in the keyring).
    acoustid_user_key_input: String,

    // Expanded now-playing view
    /// Cached blurred cover art for the current album.
    blurred_cover: Option<widget::icon::Handle>,
    /// Album key for the cached blurred cover.
    blurred_cover_key: Option<String>,
    /// Album key of a blur+accent computation currently in flight (task
    /// spawned, `BlurReady` not yet received). Guards
    /// `maybe_update_blurred_cover` against spawning a second identical
    /// job every time it's called again before the first one finishes
    /// (e.g. once per cover-art batch arrival) — each spawn would
    /// otherwise build a brand-new handle even though the result is
    /// identical.
    blur_pending_key: Option<String>,
    /// Blurred backdrop + accent for the album/artist detail page hero
    /// header (keyed by album key), and the key of a computation in flight.
    detail_art: Option<DetailArt>,
    detail_art_pending: Option<String>,
    /// Larger, separately decoded cover handle for the current track's
    /// album, used by the expanded now-playing view so it doesn't have
    /// to reuse the smaller grid-thumbnail handle from `cover_images`.
    /// Computed off-thread alongside the blur (see
    /// `maybe_update_blurred_cover`) and keyed by album key: `view()`
    /// only uses it while that key still matches the current track, so a
    /// track without cover bytes (radio, untagged files) never inherits
    /// the previous album's art. Otherwise callers fall back to the
    /// regular `cover_images` handle.
    current_cover_large: Option<(String, widget::icon::Handle)>,
    /// Accent colour extracted from the current track's cover art via
    /// `library::palette::extract`, computed alongside the blur (same
    /// bytes, same trigger — see `maybe_update_blurred_cover`). `None`
    /// when there is no current cover or extraction found no legible
    /// dominant hue; consumers fall back to the theme accent.
    accent: Option<aulos_core::library::palette::Accent>,
    /// Whether the expanded now-playing sheet is mounted: 1.0 while it is
    /// open or animating, 0.0 once a collapse has finished. The slide
    /// itself is animated at draw time by `views::now_playing::sheet`.
    expand_progress: f32,
    /// Transition in flight: 0.0 collapsing, 1.0 expanding. None when idle.
    expand_target: Option<f32>,
    /// When the current transition started.
    expand_anim_start: Option<std::time::Instant>,

    // lactea visualizer (behind feature flag)
    #[cfg(feature = "visualizer")]
    visualizer_active: bool,
    /// Whether the visualizer is currently fullscreen.
    #[cfg(feature = "visualizer")]
    viz_fullscreen: bool,
    /// Nav-bar active state saved when entering visualizer fullscreen, so it
    /// can be restored on exit (the user may have collapsed it beforehand).
    #[cfg(feature = "visualizer")]
    viz_prev_nav_active: bool,
    /// Shared frame buffer for the shader-based visualizer widget.
    /// The render subscription writes RGBA pixels here; the Shader widget
    /// reads them in its `prepare()` method via `queue.write_texture()`.
    #[cfg(feature = "visualizer")]
    viz_frame_buf: Arc<Mutex<crate::views::now_playing::viz_shader::VizFrameBuffer>>,
    /// Sender half of the command channel to the render thread (see
    /// `VizCommand`); the `Receiver` lives in `viz_cmd_rx_slot`.
    #[cfg(feature = "visualizer")]
    viz_cmd_tx: std::sync::mpsc::Sender<aulos_core::visualizer::VizCommand>,
    /// Holds the render thread's command `Receiver` between activations —
    /// see the comment where it's created in `AppModel::init`.
    #[cfg(feature = "visualizer")]
    viz_cmd_rx_slot:
        Arc<Mutex<Option<std::sync::mpsc::Receiver<aulos_core::visualizer::VizCommand>>>>,
    /// Preset file the render thread most recently put on screen — set
    /// after manual loads, "next", and automatic timer/beat switches alike.
    /// `None` until the first preset of a render thread is up.
    #[cfg(feature = "visualizer")]
    viz_current_preset_shared: Arc<Mutex<Option<std::path::PathBuf>>>,
    /// Opacity of the visualizer metadata overlay (0.0 = hidden, 1.0 = fully visible).
    /// Decays to 0 over ~4 seconds after a track change.
    #[cfg(feature = "visualizer")]
    viz_metadata_opacity: f32,
    /// Frames elapsed (~30fps) since the mouse last moved while the
    /// visualizer is fullscreen. Drives HUD control-card auto-hide; see
    /// `VIZ_HUD_HOLD_FRAMES`.
    #[cfg(feature = "visualizer")]
    viz_hud_idle_frames: u32,
    /// Whether the cursor is currently over the fullscreen HUD control
    /// card. While true, the card stays visible regardless of
    /// `viz_hud_idle_frames` (the user may be resting the pointer on a
    /// slider/button without moving it).
    #[cfg(feature = "visualizer")]
    viz_hud_pointer_over: bool,

    // Local file conversion / transcoding / CUE-ripping
    /// Drawable thumbnails of the tag editor's pictures.
    tagger_thumbs: crate::views::tagger::Thumbs,
    /// Whether the preset browser overlay is currently open.
    #[cfg(feature = "visualizer")]
    viz_browser_open: bool,
    /// Discovered `.milk` presets, refreshed by a background scan every time
    /// the browser opens (so presets installed meanwhile show up).
    #[cfg(feature = "visualizer")]
    viz_preset_entries: Vec<aulos_core::visualizer::presets::PresetEntry>,
    /// Live search filter text for the preset browser.
    #[cfg(feature = "visualizer")]
    viz_preset_search: String,
    /// Whether automatic preset transitions are locked (see
    /// `VizCommand::SetLocked`).
    #[cfg(feature = "visualizer")]
    viz_locked: bool,
    /// Beat-reactivity sensitivity (see `VizCommand::SetBeatSensitivity`).
    #[cfg(feature = "visualizer")]
    viz_beat_sensitivity: f32,
    /// UI-local mirror of `viz_current_preset_shared`, resynced from the
    /// render thread on every `VisualizerFrameReady`. Never written
    /// optimistically: the render thread is the only authority on what is
    /// really playing (a load can fail, and automatic switches replace it).
    #[cfg(feature = "visualizer")]
    viz_current_preset: Option<std::path::PathBuf>,
    /// Whether the background preset scan has delivered its result (an
    /// empty `viz_preset_entries` before that means "scanning", not "none").
    #[cfg(feature = "visualizer")]
    viz_presets_scanned: bool,
    /// Vertical scroll offset of the preset browser list, driving which
    /// rows are built (the list is virtualized: thousands of presets).
    #[cfg(feature = "visualizer")]
    viz_preset_scroll: f32,
}

/// Navigation pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
    /// Suggestions landing page (first sidebar entry).
    Home,
    Albums,
    Artists,
    Songs,
    Playlists,
    SmartPlaylists,
    Genres,
    Folders,
    Podcasts,
    Radio,
    Convert,
    Tagger,
}

/// A restorable place in the library UI, for link-navigation history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Location {
    page: Page,
    album: Option<usize>,
    artist: Option<usize>,
    genre: Option<usize>,
}

/// Hero-header artwork derived from one album cover.
#[derive(Clone, Debug)]
pub(crate) struct DetailArt {
    key: String,
    blurred: Option<widget::icon::Handle>,
    accent: Option<aulos_core::library::palette::Accent>,
}

/// Context drawer pages.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ContextPage {
    #[default]
    About,
    Equalizer,
    Lyrics,
    Providers,
    /// The play queue ("Up Next").
    Queue,
    /// Technical properties of one track (`Session::properties`).
    TrackProperties,
    /// Aggregate properties of one album (`Session::properties`).
    AlbumProperties,
    Settings,
}

/// Menu bar actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuAction {
    About,
    Equalizer,
    Providers,
    Settings,
    Queue,
    ScanLibrary,
    AddMusicDir,
    Search,
    MiniPlayer,
    /// Toggle "stop after the current track".
    StopAfterTrack,
    /// Toggle party mode.
    PartyMode,
    /// Pick the auto-play mode (what plays when the queue runs out).
    AutoPlay(aulos_core::config::AutoPlayMode),
    Quit,
}

impl menu::action::MenuAction for MenuAction {
    type Message = Message;

    fn message(&self) -> Self::Message {
        match self {
            MenuAction::About => Message::ToggleContextPage(ContextPage::About),
            MenuAction::Equalizer => Message::ToggleContextPage(ContextPage::Equalizer),
            MenuAction::Providers => Message::ToggleContextPage(ContextPage::Providers),
            MenuAction::Settings => Message::ToggleContextPage(ContextPage::Settings),
            MenuAction::Queue => Message::ToggleContextPage(ContextPage::Queue),
            MenuAction::ScanLibrary => Message::ScanLibrary,
            MenuAction::AddMusicDir => Message::AddMusicDir,
            MenuAction::Search => Message::ToggleLibrarySearch,
            MenuAction::MiniPlayer => Message::Mini(view_extras::MiniPlayerMsg::Toggle),
            MenuAction::StopAfterTrack => {
                Message::Playback(playback_extras::PlaybackExtrasMessage::ToggleStopAfter)
            }
            MenuAction::PartyMode => {
                Message::Playback(playback_extras::PlaybackExtrasMessage::TogglePartyMode)
            }
            MenuAction::AutoPlay(mode) => Message::Playback(
                playback_extras::PlaybackExtrasMessage::SetAutoPlayMode(*mode),
            ),
            MenuAction::Quit => Message::Quit,
        }
    }
}

/// Builds the map of global keyboard shortcuts to menu actions.
///
/// Consumed by the menu bar to display the shortcut label next to each
/// item. Runtime shortcut handling lives in `crate::keybinds::resolve` /
/// `Message::Shortcut` instead (see the `on_key_press` subscription in
/// `subscription()`), so this map only carries an entry where the
/// corresponding `Shortcut` also has a `MenuAction` counterpart worth
/// labelling -- currently just `Search` (`Ctrl+F` doubles as
/// `Shortcut::FocusSearch`).
fn key_binds() -> HashMap<menu::KeyBind, MenuAction> {
    let mut key_binds = HashMap::new();
    key_binds.insert(
        menu::KeyBind {
            modifiers: vec![menu::key_bind::Modifier::Ctrl],
            key: cosmic::iced::keyboard::Key::Character("f".into()),
        },
        MenuAction::Search,
    );
    key_binds.insert(
        menu::KeyBind {
            modifiers: vec![menu::key_bind::Modifier::Ctrl],
            key: cosmic::iced::keyboard::Key::Character("m".into()),
        },
        MenuAction::MiniPlayer,
    );
    key_binds
}

/// Flags passed into `AppModel::init` at startup -- currently just the
/// audio files (if any) the process was launched or handed off to open,
/// via `Exec=aulos %U`, a bare CLI argument, or another running
/// instance's MPRIS `OpenUri` forwarded through `main`.
#[derive(Debug, Clone, Default)]
pub struct AppFlags {
    pub open_paths: Vec<PathBuf>,
}
