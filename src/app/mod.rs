// SPDX-License-Identifier: GPL-3.0

use crate::config::Config;
use crate::convert::ConvertJob;
use crate::library::{Album, Artist, Lyrics, Track};
use crate::online::podcast::PodcastSearchResult;
use crate::online::radio::StationSearchResult;
use crate::online::store::{Episode, OnlineStore, Podcast, RadioStation};
use crate::player::Player;
use crate::provider::ProviderRegistry;
use crate::provider::mpd::MpdProvider;
use crate::provider::subsonic::SubsonicProvider;
use crate::views::{providers, songs};
use cosmic::cosmic_config;
use cosmic::widget::{self, about::About, menu, nav_bar};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(feature = "visualizer")]
use std::sync::Mutex;
use std::time::Duration;

mod application;
mod helpers;
mod init;
mod message;
mod subscriptions;
mod tasks;
mod update;
mod view;

pub use message::Message;

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const APP_ICON: &[u8] =
    include_bytes!("../../resources/icons/hicolor/scalable/apps/io.github.m0rf30.Lyra.svg");

/// Widget id for the header library-search input, used to programmatically
/// focus it when the search bar is activated.
const SEARCH_INPUT_ID: &str = "lyra-library-search";

/// Frames of no mouse movement (at the visualizer's ~30fps render cadence)
/// before the fullscreen HUD control card auto-hides. ~3 seconds.
#[cfg(feature = "visualizer")]
const VIZ_HUD_HOLD_FRAMES: u32 = 90;

/// Shared blocking HTTP client for all radio/podcast requests (search,
/// feed fetch, stream resolution, episode downloads). Built once so
/// requests reuse the connection pool and TLS session cache instead of
/// paying a fresh handshake on every call, with an explicit timeout so a
/// stalled server can't hang the blocking task forever.
static HTTP_CLIENT: std::sync::LazyLock<reqwest::blocking::Client> =
    std::sync::LazyLock::new(|| {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new())
    });

/// Main application model.
pub struct AppModel {
    core: cosmic::Core,
    nav: nav_bar::Model,
    key_binds: HashMap<menu::KeyBind, MenuAction>,
    about: About,
    config: Config,
    /// Cached cosmic-config context to avoid repeated D-Bus watcher creation attempts.
    config_context: Option<cosmic_config::Config>,
    context_page: ContextPage,

    // Notifications
    /// Toast notifications (e.g. provider connection failures).
    toasts: widget::toaster::Toasts<Message>,

    // Providers
    registry: ProviderRegistry,
    /// Shared references to MPD providers for idle event subscriptions.
    mpd_providers: Vec<Arc<MpdProvider>>,
    /// Ordered list of (provider_id, display_name) for the selector dropdown.
    provider_list: Vec<(String, String)>,
    /// Index of the active provider in `provider_list`.
    active_provider_index: Option<usize>,

    // Library data
    all_tracks: Vec<Track>,
    all_albums: Vec<Album>,
    all_artists: Vec<Artist>,
    library_scanning: bool,
    /// Monotonically increasing generation counter for library
    /// reloads/scans. Bumped before every new reload/scan so in-flight
    /// async results tagged with an older generation (or a different
    /// provider id) can be detected and ignored as stale.
    reload_generation: u64,

    // Library search (header search bar)
    /// Current search query (case-insensitive substring match against the
    /// active page's fields).
    library_search: String,
    /// Whether the header search input is visible/active.
    search_active: bool,
    /// Albums matching `library_search`; cached in the model (not built
    /// fresh inside `view()`) because view functions borrow `&'a [T]`
    /// slices that must live as long as `&self` — a `Vec` built locally
    /// inside `view()` would not satisfy that lifetime.
    filtered_albums: Vec<Album>,
    /// Maps `filtered_albums[i]` back to its index in `all_albums`, so
    /// index-carrying view messages (select/play) resolve against the
    /// real, unfiltered data.
    filtered_album_map: Vec<usize>,
    filtered_artists: Vec<Artist>,
    filtered_artist_map: Vec<usize>,
    filtered_tracks: Vec<Track>,
    filtered_track_map: Vec<usize>,
    filtered_playlists: Vec<crate::library::Playlist>,
    filtered_playlist_map: Vec<usize>,
    filtered_genres: Vec<String>,
    filtered_genre_map: Vec<usize>,

    // Podcasts
    podcasts: Vec<Podcast>,
    selected_podcast: Option<usize>,
    podcast_episodes: Vec<Episode>,
    podcast_search_query: String,
    podcast_search_results: Vec<PodcastSearchResult>,
    podcast_search_loading: bool,
    podcast_add_url: String,
    /// The episode id currently playing, if the current track came from a
    /// podcast subscription — used to persist playback position.
    current_podcast_episode_id: Option<i64>,
    /// Last whole-second position persisted for `current_podcast_episode_id`,
    /// so the tick handler only writes to the DB roughly every 5 seconds.
    last_saved_podcast_position_secs: u64,
    /// Episode ids currently being downloaded for offline playback.
    downloading_episodes: std::collections::HashSet<i64>,

    // Radio
    radio_stations: Vec<RadioStation>,
    radio_search_query: String,
    radio_search_results: Vec<StationSearchResult>,
    radio_search_loading: bool,
    radio_add_name: String,
    radio_add_url: String,

    /// Podcast artwork / radio favicon bytes, keyed by their source URL.
    /// Shared between both views since the icons are the same kind of
    /// small, best-effort raster image loaded from a remote URL.
    online_icons: HashMap<String, widget::icon::Handle>,

    // Player
    player: Option<Player>,
    playback_position: Duration,
    current_track: Option<Track>,
    /// MPRIS2 D-Bus handle, once the session-bus server has started (see
    /// `crate::mpris::mpris_stream`). `None` before `Ready` arrives or if no
    /// session bus is available — media-key integration degrades quietly.
    mpris: Option<crate::mpris::MprisHandle>,
    /// While the user is dragging the seek slider, holds the preview fraction
    /// (0.0–1.0). `None` when not dragging. The actual backend seek happens
    /// only on release (`SeekCommit`).
    seeking_preview: Option<f32>,
    /// Volume level captured when the mute shortcut silenced playback, so
    /// unmuting restores it instead of a fixed default. `None` whenever
    /// audio is not muted-by-shortcut.
    pre_mute_volume: Option<f32>,

    // Scrobble state (Subsonic)
    /// Whether a "now playing" notification has been sent for the current track.
    scrobble_now_playing_sent: bool,
    /// Whether the current track has been scrobbled (to avoid duplicates).
    scrobble_sent: bool,

    // View state
    selected_album: Option<usize>,
    selected_artist: Option<usize>,
    songs_sort: songs::SortField,
    /// When true, the Songs list column headers show a descending arrow.
    songs_sort_descending: bool,
    /// When true, the Songs view shows only favorite tracks.
    favorites_filter: bool,
    /// When set, the Songs view shows only tracks matching this genre.
    genre_filter: Option<String>,
    /// Available playlists for the Playlists view.
    playlists: Vec<crate::library::Playlist>,
    /// Currently selected playlist index (for detail view).
    selected_playlist: Option<usize>,
    /// Text input for new playlist name.
    new_playlist_name: String,
    /// Live-edited text for the rename field in `playlist_detail_view`,
    /// seeded from the playlist's current name when its detail view opens.
    rename_playlist_input: String,
    /// Saved smart (rule-based) playlists.
    smart_playlists: Vec<crate::library::smart_playlist::SmartPlaylist>,
    /// Currently selected smart playlist index (for detail/editor view).
    selected_smart_playlist: Option<usize>,
    /// Resolved tracks for the currently viewed smart playlist.
    smart_playlist_tracks: Vec<Track>,
    /// In-progress rules-editor state; `Some` shows the editor instead of
    /// the list/detail view.
    smart_playlist_editor: Option<crate::views::smart_playlists::EditorState>,
    /// All distinct genres from the active provider.
    all_genres: Vec<String>,
    /// Currently selected genre index (for detail view).
    selected_genre: Option<usize>,
    /// Tracks filtered by the currently selected genre.
    genre_tracks: Vec<Track>,
    /// In-memory directory-hierarchy browse state for the Folders view;
    /// rebuilt from `all_tracks` whenever the page is opened or the
    /// library reloads (see `FolderTree::build`).
    folder_state: crate::views::folders::FolderState,
    cover_images: HashMap<String, widget::icon::Handle>,
    artist_avatars: HashMap<String, widget::icon::Handle>,

    // Keyboard input state
    /// Tracks whether any text input field currently has keyboard focus.
    /// When true, space bar should type a space character instead of toggling playback.
    text_input_focused: bool,

    // Lyrics
    lyrics_text: Option<Lyrics>,
    lyrics_loading: bool,
    /// When true and the expanded now-playing view is active, lyrics render
    /// as an in-view overlay (over the cover art / visualizer) instead of
    /// opening the generic context-drawer sidebar, keeping the immersive
    /// full view intact.
    lyrics_overlay_active: bool,

    // Equalizer
    eq_preset: Option<crate::player::equalizer::EqPreset>,
    preset_manager: crate::player::eq_presets::EqPresetManager,
    all_presets: Vec<crate::player::equalizer::EqPresetData>,
    active_preset_name: Option<String>,
    eq_dirty: bool,
    save_as_name: String,

    // AutoEQ
    /// AutoEQ profiles loaded from GitHub, available in the preset dropdown.
    autoeq_profiles: Vec<crate::autoeq::AutoEQProfileMetadata>,
    autoeq_loading: bool,
    /// Current search query for filtering AutoEQ profiles in the dropdown.
    autoeq_search: String,

    // Settings — multi-artist tag splitting
    /// Live-edited text for the delimiter list editor in the Settings
    /// drawer, seeded from `config.artist_tag_delimiters.join(" | ")` and
    /// only committed into `config` on submit.
    artist_tag_delimiters_input: String,

    // Provider settings (editing state)
    mpd_edit_states: Vec<providers::MpdEditState>,
    mpd_connection_status: Vec<Option<String>>,
    subsonic_edit_states: Vec<providers::SubsonicEditState>,
    subsonic_connection_status: Vec<Option<String>>,
    /// Shared references to Subsonic providers for scrobbling.
    subsonic_providers: Vec<Arc<SubsonicProvider>>,

    // Expanded now-playing view
    /// Raw cover art bytes keyed by album_key, for blur processing.
    cover_art_bytes: crate::library::palette::CoverByteCache,
    /// Cached blurred cover art for the current album.
    blurred_cover: Option<widget::icon::Handle>,
    /// Album key for the cached blurred cover.
    blurred_cover_key: Option<String>,
    /// Accent colour extracted from the current track's cover art via
    /// `library::palette::extract`, computed alongside the blur (same
    /// bytes, same trigger — see `maybe_update_blurred_cover`). `None`
    /// when there is no current cover or extraction found no legible
    /// dominant hue; consumers fall back to the theme accent.
    accent: Option<crate::library::palette::Accent>,
    /// 0.0 = fully collapsed (compact bar), 1.0 = fully expanded.
    expand_progress: f32,
    /// Animation target: 0.0 for collapsing, 1.0 for expanding. None when idle.
    expand_target: Option<f32>,
    /// Timestamp when the current animation started.
    expand_anim_start: Option<std::time::Instant>,
    /// Progress value when the current animation started (for reversals).
    expand_anim_from: f32,

    // ProjectM visualizer (behind feature flag)
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
    #[cfg(feature = "visualizer")]
    pcm_buffer: Option<Arc<Mutex<crate::views::now_playing::visualizer::PcmBuffer>>>,
    /// Whether MPD itself is currently reporting `Playing` state — set from
    /// `Message::MpdStatusUpdate`. Read by the MPD PipeWire capture thread
    /// (`player::pw_capture`) to gate writes made while it's using the
    /// default-sink-monitor fallback (no MPD stream found on the graph
    /// yet), so other applications' audio doesn't drive the visualizer
    /// while MPD itself is paused/stopped. `Arc<AtomicBool>` rather than a
    /// plain field so the capture thread (spawned fresh by the MPD capture
    /// subscription each time it (re)activates) can share it without
    /// touching `AppModel` from off the UI thread.
    #[cfg(feature = "visualizer")]
    mpd_playing: Arc<std::sync::atomic::AtomicBool>,
    /// Sender half of the command channel to the render thread (see
    /// `VizCommand`); the `Receiver` lives in `viz_cmd_rx_slot`.
    #[cfg(feature = "visualizer")]
    viz_cmd_tx: std::sync::mpsc::Sender<crate::views::now_playing::visualizer::VizCommand>,
    /// Holds the render thread's command `Receiver` between activations —
    /// see the comment where it's created in `AppModel::init`.
    #[cfg(feature = "visualizer")]
    viz_cmd_rx_slot: Arc<
        Mutex<Option<std::sync::mpsc::Receiver<crate::views::now_playing::visualizer::VizCommand>>>,
    >,
    /// Preset name the render thread most recently loaded/switched to.
    /// `None` after a playlist-driven `NextPreset` switch, since the
    /// playlist API exposes no way to read back which preset it landed
    /// on; set again on the next explicit `LoadPreset`.
    #[cfg(feature = "visualizer")]
    viz_current_preset_shared: Arc<Mutex<Option<String>>>,
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
    /// Queued/running/finished conversion jobs, oldest first.
    convert_jobs: Vec<ConvertJob>,
    /// Output directory for new conversion jobs.
    convert_out_dir: PathBuf,
    /// Selected index into `OutputFormat::ALL`.
    convert_format_index: usize,
    /// Selected index into `views::convert::SAMPLE_RATE_OPTIONS`.
    convert_rate_index: usize,
    /// Monotonic id source for new jobs.
    convert_next_id: u64,
    /// Caps concurrently-running conversion jobs at 2, shared across every
    /// in-flight job future.
    convert_semaphore: Arc<tokio::sync::Semaphore>,
    /// Whether the preset browser overlay is currently open.
    #[cfg(feature = "visualizer")]
    viz_browser_open: bool,
    /// Discovered `.milk` presets, populated lazily on first browser open.
    #[cfg(feature = "visualizer")]
    viz_preset_entries: Vec<crate::views::now_playing::visualizer::PresetEntry>,
    /// Set once the background preset scan has been kicked off, so
    /// reopening the browser doesn't rescan every time.
    #[cfg(feature = "visualizer")]
    viz_presets_scan_started: bool,
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
    /// UI-local mirror of `viz_current_preset_shared`, updated
    /// optimistically on `LoadVizPreset` and resynced from the render
    /// thread on every `VisualizerFrameReady`.
    #[cfg(feature = "visualizer")]
    viz_current_preset_name: Option<String>,
}

/// Navigation pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Page {
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
}

/// Context drawer pages.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ContextPage {
    #[default]
    About,
    Equalizer,
    Lyrics,
    Providers,
    Settings,
}

/// Menu bar actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuAction {
    About,
    Equalizer,
    Providers,
    Settings,
    ScanLibrary,
    AddMusicDir,
    Search,
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
            MenuAction::ScanLibrary => Message::ScanLibrary,
            MenuAction::AddMusicDir => Message::AddMusicDir,
            MenuAction::Search => Message::ToggleLibrarySearch,
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
    key_binds
}

/// Translates a position in a search-filtered list back to its index in
/// the corresponding unfiltered library vector. Passthrough when `map` is
/// `None` (search inactive or the query is empty).
fn unfilter_index(map: Option<&[usize]>, i: usize) -> usize {
    map.map_or(i, |m| m[i])
}

/// Parse the delimiter-list text box (delimiters joined by `" | "`) back
/// into the `Vec<String>` stored in `Config::artist_tag_delimiters`,
/// trimming each entry and dropping empties.
fn parse_delimiters_input(text: &str) -> Vec<String> {
    text.split(" | ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Pure staleness check for an async library reload result.
///
/// `true` when the result's `(generation, provider_id)` no longer matches
/// the currently active reload — i.e. it was superseded by a later
/// reload/scan or a provider switch — and must be discarded without
/// mutating library data or `library_scanning`.
fn reload_result_is_stale(
    current_generation: u64,
    current_provider_id: &str,
    result_generation: u64,
    result_provider_id: &str,
) -> bool {
    result_generation != current_generation || result_provider_id != current_provider_id
}

/// Path to the shared library database (also used by `OnlineStore` for
/// podcasts/radio — same file, same schema, opened via its own connection).
fn online_db_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lyra")
        .join("library.db")
}

/// Open the online store at the shared library database path.
fn open_online_store() -> Result<OnlineStore, String> {
    OnlineStore::open(&online_db_path())
}

/// Current Unix time in whole seconds.
fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Flags passed into `AppModel::init` at startup -- currently just the
/// audio files (if any) the process was launched or handed off to open,
/// via `Exec=lyra %U`, a bare CLI argument, or another running
/// instance's MPRIS `OpenUri` forwarded through `main`.
#[derive(Debug, Clone, Default)]
pub struct AppFlags {
    pub open_paths: Vec<PathBuf>,
}

#[cfg(test)]
mod reload_generation_tests {
    use super::reload_result_is_stale;

    #[test]
    fn matching_generation_and_provider_is_not_stale() {
        assert!(!reload_result_is_stale(3, "mpd-home", 3, "mpd-home"));
    }

    #[test]
    fn result_from_a_superseded_reload_is_stale() {
        // A second reload bumped the generation before this result arrived.
        assert!(reload_result_is_stale(3, "mpd-home", 2, "mpd-home"));
    }

    #[test]
    fn result_from_a_different_provider_is_stale_even_at_current_generation() {
        // The generation counter alone can't catch a switch back to the
        // same numeric generation on a different provider, so identity
        // must be checked independently.
        assert!(reload_result_is_stale(3, "mpd-home", 3, "subsonic-server"));
    }
}
