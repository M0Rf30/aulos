// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::ContextPage;
use crate::views::radio as radio_view;
use crate::views::{albums, artists, convert, podcasts};
use aulos_core::config::{Config, ReplayGainMode};
use aulos_core::library::Track;
use cosmic::widget;
use std::path::PathBuf;

/// All application messages.
#[derive(Debug, Clone)]
pub enum Message {
    // Navigation / chrome
    LaunchUrl(String),
    ToggleContextPage(ContextPage),
    /// Open the Properties drawer for a track (see `AppModel::open_track_properties`).
    ShowTrackProperties(Track),
    /// Off-thread result of opening the Properties drawer.
    PropertiesProbed(aulos_core::session::PropertiesEvent),
    Properties(crate::views::track_properties::PropertiesMessage),
    /// Set the grid card size multiplier (clamped to `GRID_SCALE_RANGE`).
    SetGridScale(f32),
    /// Toggle albums view between grid and list.
    ToggleAlbumsViewMode,
    /// Toggle artists view between grid and list.
    ToggleArtistsViewMode,
    /// Toggle genres view between grid and list.
    ToggleGenresViewMode,
    /// A global keyboard shortcut resolved by `crate::keybinds::resolve`
    /// from a raw key press (`on_key_press` in `subscription()`).
    /// Interpreted contextually in `update()` -- the resolver has no
    /// access to `self`, since `on_key_press` requires a bare `fn`
    /// pointer.
    Shortcut(crate::keybinds::Shortcut),
    /// Tracks text input focus state for keyboard shortcuts.
    /// When true, text input has focus; when false, input lost focus.
    TextInputFocused(bool),

    // Library search
    /// The library search query changed (live filter as you type).
    LibrarySearchChanged(String),
    /// Debounce timer for `LibrarySearchChanged`; carries the generation
    /// it was scheduled for (ignored if a newer keystroke arrived).
    LibrarySearchDebounced(u64),
    /// Toggle the header search input on/off.
    ToggleLibrarySearch,
    /// Clear the search query and deactivate the search input (Esc / clear icon).
    ClearLibrarySearch,
    // Library
    ScanLibrary,
    /// Async result of a library workflow (scan, load, batch, genres,
    /// favourite/rating changes); see `Session::apply_library_event`.
    Library(aulos_core::session::LibraryEvent),
    /// Filesystem watcher detected changes in music directories.
    /// Contains the deduplicated list of changed paths after debounce.
    FilesChanged(Vec<PathBuf>),

    /// Full-size cover bytes for `key` were loaded on demand (blur, detail
    /// hero, notification); `None` when the album has no art.
    CoverBytesLoaded(String, Option<Vec<u8>>),
    /// Result of a background keyring job (startup credential check,
    /// AcoustID key probe/save).
    Keyring(aulos_core::session::ConfigEvent),

    // Player transport
    TogglePlayback,
    NextTrack,
    PreviousTrack,
    /// Visual-only update while dragging the seek slider (no backend seek).
    SeekPreview(f32),
    /// Performs the actual backend seek when the slider is released.
    SeekCommit,
    SetVolume(f32),
    /// Persist the current master volume to config. Emitted on slider
    /// release and on discrete volume changes (keyboard shortcuts, MPRIS
    /// `SetVolume`) so the level survives a restart without writing the
    /// config on every drag frame.
    VolumeCommit,
    ToggleShuffle,
    CycleRepeat,
    PlaybackTick,
    /// Stop playback entirely (transport button, `Stop` shortcut, MPRIS
    /// `Stop`). Keeps the queue so a later play resumes it.
    Stop,

    // Play queue ("Up Next") editing. Every index is a position in *play
    // order* — the order `Player::queue()` returns, which already reflects
    // shuffle — so the queue view can hand its row index straight back.
    /// Jump to and play the queue entry at this play-order index.
    QueueJump(usize),
    /// Remove the queue entry at this play-order index.
    QueueRemove(usize),
    /// Move a queue entry between two play-order indices.
    QueueMove {
        from: usize,
        to: usize,
    },
    /// Drop every queue entry except the one currently playing.
    QueueClear,
    /// Insert tracks right after the currently playing one.
    PlayNext(Vec<Track>),
    /// Append tracks to the end of the queue.
    AddToQueue(Vec<Track>),
    /// "Play next" (`next`) or "Add to queue" for tracks named by index;
    /// resolved in `update` so views never clone track lists.
    QueueFrom {
        source: aulos_core::library::queue_source::QueueSource,
        next: bool,
    },
    /// A virtualized album/artist/genre grid or list scrolled.
    GridScrolled(crate::views::common::ScrollArea, f32),

    // Track selection
    PlayTrackIndex(usize),
    PlayAlbum(usize),
    PlayAlbumTrack(usize, usize),
    PlayArtistAlbum(usize, usize),
    PlayArtistTrack(usize, usize, usize),
    /// Open the Properties drawer for album `.0` of the library albums (resolved
    /// in `update`, so album context menus never clone an album's tracks).
    ShowAlbumProperties(usize),
    /// Open the Properties drawer for album `.1` of artist `.0`.
    ShowArtistAlbumProperties(usize, usize),
    /// Open the tag editor on one (local) track (`AppModel::open_in_tagger`).
    TagTrack(Track, aulos_core::tagger::TagAction),
    /// Open the tag editor on album `.0` of the library albums (resolved in
    /// `update`, like `ShowAlbumProperties`).
    TagAlbum(usize, aulos_core::tagger::TagAction),
    /// Open the tag editor on album `.1` of artist `.0`.
    TagArtistAlbum(usize, usize, aulos_core::tagger::TagAction),

    // Albums view
    SelectAlbum(usize),
    BackToAlbumGrid,

    // Artists view
    SelectArtist(usize),
    BackToArtistList,
    /// Expand/collapse the selected artist's clipped biography preview.
    ToggleArtistBioExpanded,
    /// One lazily-fetched artist's bio/image result landed (see
    /// `AppModel::load_artist_info_for_visible`).
    ArtistInfoLoaded(aulos_core::library::artist_info::ArtistInfoOutcome),

    // Songs view
    SortSongs(aulos_core::library::state::SortField),
    /// Toggle the favorites-only filter in the Songs view.
    ToggleFavoritesFilter,
    /// The Songs list was scrolled to this vertical offset (px); drives
    /// which rows the virtualized list builds.
    SongsScrolled(f32),
    /// Cross-view navigation (artist/album/genre link clicked anywhere).
    Navigate(crate::views::Route),

    // Favorites & ratings
    /// Toggle favorite status for a track (by track ID string).
    ToggleFavorite(String),
    /// Set rating (1-5) for a track. Pass 0 to clear.
    SetRating(String, u8),

    // Playlist actions
    /// Add a track to a playlist. (track source_uri, playlist ID)
    AddToPlaylist(String, String),

    // Playlists view
    /// Select a playlist to show its detail view.
    SelectPlaylist(usize),
    /// Go back from playlist detail to the list.
    BackToPlaylistList,
    /// Create a new playlist with the given name.
    CreatePlaylist(String),
    /// Delete a playlist by index.
    DeletePlaylist(usize),
    /// Rename a playlist (index, new name).
    RenamePlaylist(usize, String),
    /// Play all tracks in a playlist.
    PlayPlaylist(usize),
    /// Play a specific track within a playlist (playlist idx, track idx).
    PlayPlaylistTrack(usize, usize),
    /// Remove a track from a playlist (playlist idx, track idx).
    RemovePlaylistTrack(usize, usize),
    /// The new-playlist name input changed.
    NewPlaylistNameChanged(String),
    /// The rename input changed (playlist index, new text).
    RenamePlaylistInput(usize, String),
    /// Async result of a playlist workflow (load, provider mutation, smart
    /// playlists, file import).
    PlaylistEvent(aulos_core::session::PlaylistEvent),

    // Smart playlists view — a single wrapped variant keeps this feature's
    // full message vocabulary (list/detail/editor) out of the top-level enum.
    SmartPlaylists(crate::views::smart_playlists::SmartPlaylistMessage),

    // Home page (suggested music, play history)
    Home(crate::views::home::HomeMessage),
    /// Async result of a Home workflow (shelves, decades, play history).
    HomeEvent(aulos_core::session::HomeEvent),

    // Startup preferences (Settings drawer)
    Startup(super::startup::StartupMessage),

    // Genre filtering
    /// Filter tracks by genre name.
    FilterByGenre(String),

    // Genres view
    /// Select a genre to show its tracks.
    SelectGenre(usize),
    /// Go back from genre detail to the grid.
    BackToGenreGrid,
    /// Play a track in the genre detail view (index within `LibraryState::genre_tracks`).
    PlayGenreTrack(usize),
    /// Play the selected genre's tracks in shuffled order.
    ShuffleGenre,

    // Folders view
    /// Messages from the folder browse view (navigation, playback,
    /// favorite/rating delegation) — see `views::folders::FolderMessage`.
    Folders(crate::views::folders::FolderMessage),

    // Lyrics
    ShowLyrics,
    FetchLyricsOnline,

    // Equalizer
    EqSetBand(usize, f32),
    EqSetPreset(aulos_core::player::equalizer::EqPreset),
    EqToggle(bool),
    EqSetPreamp(f32),
    /// Select a preset by name from the combined preset list.
    EqSelectPreset(String),
    /// Overwrite the currently loaded custom preset with current band/preamp values.
    EqSavePreset,
    /// Save current band/preamp values as a new named custom preset.
    EqSavePresetAs(String),
    /// Delete the currently active custom preset.
    EqDeletePreset,
    /// Reset to Flat (all bands 0, preamp 0, clear selection).
    EqResetPreset,
    /// Update the "Save As" name input.
    EqSaveAsNameChanged(String),

    // AutoEQ
    /// Update the AutoEQ search query (filters profiles in dropdown).
    AutoEQSearchChanged(String),
    /// Fetch AutoEQ index from GitHub (triggered by "Load AutoEQ..." button).
    FetchAutoEQIndex,
    /// Select an AutoEQ profile by path from the dropdown.
    EqSelectAutoEQ(String),
    /// An AutoEQ index/profile download finished.
    AutoEQEvent(aulos_core::session::EqualizerEvent),

    // Settings
    AddMusicDir,
    /// The Settings drawer's search box changed.
    SettingsSearch(String),
    /// Scrobbling settings / connect-flow message.
    Scrobble(aulos_core::online::scrobble::ScrobbleMessage),
    /// Result from the XDG Desktop Portal directory picker.
    DirPickerResult(Result<PathBuf, String>),
    RemoveMusicDir(usize),
    UpdateConfig(Config),
    /// Toggle multi-artist tag splitting on/off.
    SetSplitArtistTags(bool),
    /// Live text of the delimiter list editor changed (before submit).
    ArtistTagDelimitersInputChanged(String),
    /// Commit the edited delimiter text (parsed on the `" | "` separator).
    SubmitArtistTagDelimiters(String),
    /// Reset the delimiter list to `artist_tags::DEFAULT_DELIMITERS`.
    ResetArtistTagDelimiters,
    /// Toggle the experimental local file converter feature on/off (also
    /// live-adds/removes the Convert nav entry — see
    /// `crate::app::init::AppModel::set_convert_nav_entry`).
    SetExperimentalConverter(bool),
    /// Toggle fetching artist images/biography from online sources
    /// (Local/MPD mode only — ignored in Subsonic mode).
    SetFetchArtistInfo(bool),
    /// The AcoustID application API key was edited (persisted to config).
    SetAcoustidApiKey(String),
    /// The user's AcoustID API key text box changed (not yet stored).
    EditAcoustidUserKey(String),
    /// Store the typed AcoustID user key in the keyring (empty deletes it).
    SaveAcoustidUserKey,
    /// Toggle preferring the Cover Art Archive front cover in the tag editor.
    SetTaggerUseCaaCover(bool),

    // Provider switching
    SwitchProvider(usize),

    // MPD provider configuration
    MpdAddServer,
    MpdEditName(usize, String),
    MpdEditHost(usize, String),
    MpdEditPort(usize, String),
    MpdEditPassword(usize, String),
    MpdSaveServer(usize),
    MpdRemoveServer(usize),
    MpdTestConnection(usize),
    /// Result of a connection test of an MPD or Subsonic card.
    ProviderEvent(aulos_core::session::ProviderEvent),

    // MPD provider events
    MpdConnected(String),
    MpdConnectionFailed(String, String),
    /// `provider_id`, plus which MPD subsystem changed (only
    /// `Database`/`Update`/`StoredPlaylist` trigger a reload in
    /// `update()` — see `mpd_events::idle_action`).
    MpdIdleEvent(String, aulos_core::provider::mpd_events::IdleSubsystem),
    /// Polled status from the active MPD backend (position, duration,
    /// state, volume). `song` is `Some` only on the tick where MPD's
    /// current song identity changed (including the first tick after the
    /// subscription starts) — most ticks carry `None`.
    MpdStatusUpdate(aulos_core::provider::mpd_events::MpdStatusUpdate),
    /// Result of asynchronous playback work (MPD commands, MPRIS art,
    /// lyrics, opened files, inhibit and notification requests).
    PlaybackEvent(aulos_core::session::PlaybackEvent),

    // Subsonic provider configuration
    SubsonicAddServer,
    SubsonicEditName(usize, String),
    SubsonicEditUrl(usize, String),
    SubsonicEditUsername(usize, String),
    SubsonicEditPassword(usize, String),
    SubsonicToggleCerts(usize, bool),
    SubsonicSaveServer(usize),
    SubsonicRemoveServer(usize),
    SubsonicTestConnection(usize),
    /// Subsonic transcoding bitrate changed (server index, bitrate or None).
    SubsonicTranscodingBitrate(usize, Option<u32>),
    /// Subsonic transcoding format changed (server index, format or None).
    SubsonicTranscodingFormat(usize, Option<String>),

    // Playback settings (Tasks 107, 108, 113, 114)
    /// Set crossfade duration (seconds, 0 = disabled).
    SetCrossfade(f32),
    /// Set replay gain mode.
    SetReplayGainMode(ReplayGainMode),

    // Expanded now-playing view
    ExpandNowPlaying,
    CollapseNowPlaying,
    ExpandAnimTick,
    /// Blurred backdrop and accent for a detail page's hero header are
    /// ready for this album key.
    DetailArtReady(
        String,
        Option<widget::icon::Handle>,
        Option<aulos_core::library::palette::Accent>,
    ),
    /// Blurred cover art, accent colour, and a separately-decoded larger
    /// cover handle (for the expanded now-playing view) are ready for
    /// `album_key`. The blur/large-cover handles are `None` when their
    /// respective decode failed (still worth delivering the accent); the
    /// accent is `None` when extraction found no legible dominant hue.
    BlurReady(
        String,
        Option<widget::icon::Handle>,
        Option<aulos_core::library::palette::Accent>,
        Option<widget::icon::Handle>,
    ),

    // Visualizer (behind feature flag)
    #[cfg(feature = "visualizer")]
    ToggleVisualizer,
    #[cfg(feature = "visualizer")]
    NextVisualizerPreset,
    /// A new visualizer frame was written to the shared VizFrameBuffer.
    /// This message carries no data — it just triggers a view redraw.
    #[cfg(feature = "visualizer")]
    VisualizerFrameReady,
    /// The visualizer render thread could not start (no usable GPU adapter,
    /// device creation failed, ...); carries the reason for the user.
    #[cfg(feature = "visualizer")]
    VisualizerFailed(String),
    /// Toggle fullscreen for the visualizer window (on → off, off → on).
    #[cfg(feature = "visualizer")]
    ToggleVisualizerFullscreen,
    /// Mouse moved while the visualizer is fullscreen — resets the HUD
    /// control-card auto-hide idle counter.
    #[cfg(feature = "visualizer")]
    VizHudActivity,
    /// Cursor entered/left the fullscreen HUD control card — see
    /// `NowPlayingMessage::VizHudPointerEnter`/`VizHudPointerExit`.
    #[cfg(feature = "visualizer")]
    VizHudPointerEnter,
    #[cfg(feature = "visualizer")]
    VizHudPointerExit,
    /// Toggle the preset browser overlay on/off.
    #[cfg(feature = "visualizer")]
    TogglePresetBrowser,
    /// Preset browser search query changed.
    #[cfg(feature = "visualizer")]
    PresetSearchInput(String),
    /// Load a specific preset file outside the shuffled rotation, with a
    /// smooth transition.
    #[cfg(feature = "visualizer")]
    LoadVizPreset(PathBuf),
    /// The preset browser list scrolled to this vertical offset (px).
    #[cfg(feature = "visualizer")]
    PresetListScrolled(f32),
    /// Lock/unlock automatic preset transitions.
    #[cfg(feature = "visualizer")]
    SetVizLocked(bool),
    /// Adjust beat-reactivity sensitivity.
    #[cfg(feature = "visualizer")]
    SetVizBeatSensitivity(f32),
    /// Background preset scan finished.
    #[cfg(feature = "visualizer")]
    VizPresetsScanned(Vec<aulos_core::visualizer::presets::PresetEntry>),

    // Notifications
    // Podcasts
    /// UI events from the podcasts view (see `views::podcasts::PodcastMessage`).
    Podcast(podcasts::PodcastMessage),
    /// Async results for the podcasts page.
    PodcastEvent(aulos_core::session::PodcastEvent),
    /// A podcast/radio icon (artwork or favicon) finished downloading.
    /// `Some((width, height, rgba_pixels))` on success (already decoded
    /// and downscaled off-thread by `load_online_icons`, so the handler
    /// can build a `widget::icon::from_raster_pixels` handle directly);
    /// `None` when the fetch or decode failed.
    OnlineIconLoaded(String, Option<(u32, u32, Vec<u8>)>),

    // Radio
    /// UI events from the radio view (see `views::radio::RadioMessage`).
    Radio(radio_view::RadioMessage),
    /// Async results for the radio page.
    RadioEvent(aulos_core::session::RadioEvent),

    /// A toast notification was dismissed (by timeout or user action).
    CloseToast(widget::toaster::ToastId),

    // Convert / transcode / rip local files
    /// UI actions from the convert view (see `views::convert::ConvertMessage`).
    Convert(convert::ConvertMessage),
    /// Async results and ticks for the convert page (see
    /// `app::convert_page::ConvertEvent`).
    ConvertEvent(super::convert_page::ConvertEvent),

    // Tag editor
    /// UI actions from the tag editor view.
    Tagger(aulos_core::tagger::TaggerMessage),
    /// Results of work the tag editor started.
    TaggerEvent(aulos_core::tagger::TaggerEvent),

    // Application lifecycle
    Quit,
    /// Playback extras (stop-after, fade, party/auto-play, notifications,
    /// inhibit, background playback) — see `app::playback_extras`.
    Playback(super::playback_extras::PlaybackExtrasMessage),
    /// An MPRIS2 D-Bus event: either the server handle becoming available,
    /// or a command relayed from a media-key/shell-applet D-Bus call.
    Mpris(aulos_core::mpris::MprisEvent),
    /// Open ad-hoc audio files outside the library (double-clicked in a
    /// file manager via `Exec=aulos %U`, passed on the command line, or
    /// forwarded from another running instance's MPRIS `OpenUri`).
    OpenFiles(Vec<PathBuf>),

    // Playlist import/export, mini player, compilations and album filters
    /// M3U/M3U8/PLS playlist import and M3U export (see `app::playlist_io`).
    PlaylistIo(super::playlist_io::PlaylistIo),
    /// Mini-player mode (see `app::view_extras`).
    Mini(super::view_extras::MiniPlayerMsg),
    /// A filter chip on the Albums page was clicked.
    AlbumFilter(crate::views::album_filters::FilterMsg),
    /// Toggle the "Various Artists" entry in the Artists view.
    SetShowCompilationsInArtists(bool),
    /// Toggle relative paths in exported M3U playlists.
    SetM3uRelativePaths(bool),
}

impl From<albums::AlbumMessage> for Message {
    fn from(msg: albums::AlbumMessage) -> Self {
        match msg {
            albums::AlbumMessage::PlayAlbum(i) => Message::PlayAlbum(i),
            albums::AlbumMessage::PlayTrack(ai, ti) => Message::PlayAlbumTrack(ai, ti),
            albums::AlbumMessage::SelectAlbum(i) => Message::SelectAlbum(i),
            albums::AlbumMessage::BackToGrid => Message::BackToAlbumGrid,
            albums::AlbumMessage::ToggleFavorite(id) => Message::ToggleFavorite(id),
            albums::AlbumMessage::SetRating(id, r) => Message::SetRating(id, r),
            albums::AlbumMessage::FilterByGenre(g) => Message::FilterByGenre(g),
            albums::AlbumMessage::Navigate(route) => Message::Navigate(route),
            albums::AlbumMessage::AddToPlaylist(uri, pid) => Message::AddToPlaylist(uri, pid),
            albums::AlbumMessage::ToggleViewMode => Message::ToggleAlbumsViewMode,
            albums::AlbumMessage::PlayNext(album, track) => Message::QueueFrom {
                source: aulos_core::library::queue_source::QueueSource::Album { album, track },
                next: true,
            },
            albums::AlbumMessage::AddToQueue(album, track) => Message::QueueFrom {
                source: aulos_core::library::queue_source::QueueSource::Album { album, track },
                next: false,
            },
            albums::AlbumMessage::Scrolled(area, y) => Message::GridScrolled(area, y),
            albums::AlbumMessage::Filter(f) => Message::AlbumFilter(f),
            albums::AlbumMessage::ShowProperties(track) => Message::ShowTrackProperties(track),
            albums::AlbumMessage::ShowAlbumProperties(i) => Message::ShowAlbumProperties(i),
            albums::AlbumMessage::Tag(track, action) => Message::TagTrack(track, action),
            albums::AlbumMessage::TagAlbum(i, action) => Message::TagAlbum(i, action),
        }
    }
}

impl From<artists::ArtistMessage> for Message {
    fn from(msg: artists::ArtistMessage) -> Self {
        match msg {
            artists::ArtistMessage::PlayArtistAlbum(ai, ali) => Message::PlayArtistAlbum(ai, ali),
            artists::ArtistMessage::PlayTrack(ai, ali, ti) => Message::PlayArtistTrack(ai, ali, ti),
            artists::ArtistMessage::SelectArtist(i) => Message::SelectArtist(i),
            artists::ArtistMessage::BackToList => Message::BackToArtistList,
            artists::ArtistMessage::ToggleFavorite(id) => Message::ToggleFavorite(id),
            artists::ArtistMessage::SetRating(id, r) => Message::SetRating(id, r),
            artists::ArtistMessage::FilterByGenre(g) => Message::FilterByGenre(g),
            artists::ArtistMessage::Navigate(route) => Message::Navigate(route),
            artists::ArtistMessage::ToggleViewMode => Message::ToggleArtistsViewMode,
            artists::ArtistMessage::PlayNext(artist, album, track) => Message::QueueFrom {
                source: aulos_core::library::queue_source::QueueSource::ArtistAlbum {
                    artist,
                    album,
                    track,
                },
                next: true,
            },
            artists::ArtistMessage::AddToQueue(artist, album, track) => Message::QueueFrom {
                source: aulos_core::library::queue_source::QueueSource::ArtistAlbum {
                    artist,
                    album,
                    track,
                },
                next: false,
            },
            artists::ArtistMessage::Scrolled(area, y) => Message::GridScrolled(area, y),
            artists::ArtistMessage::ToggleBioExpanded => Message::ToggleArtistBioExpanded,
            artists::ArtistMessage::ShowProperties(track) => Message::ShowTrackProperties(track),
            artists::ArtistMessage::ShowAlbumProperties(artist, album) => {
                Message::ShowArtistAlbumProperties(artist, album)
            }
            artists::ArtistMessage::Tag(track, action) => Message::TagTrack(track, action),
            artists::ArtistMessage::TagAlbum(artist, album, action) => {
                Message::TagArtistAlbum(artist, album, action)
            }
        }
    }
}
