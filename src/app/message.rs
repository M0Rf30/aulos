// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::ContextPage;
use crate::config::{Config, ReplayGainMode};
use crate::library::{Album, Artist, Lyrics, Track};
use crate::player::PlaybackState;
use crate::views::radio as radio_view;
use crate::views::{albums, artists, convert, podcasts, songs};
use cosmic::widget;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// All application messages.
#[derive(Debug, Clone)]
pub enum Message {
    // Navigation / chrome
    LaunchUrl(String),
    ToggleContextPage(ContextPage),
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
    /// Toggle the header search input on/off.
    ToggleLibrarySearch,
    /// Clear the search query and deactivate the search input (Esc / clear icon).
    ClearLibrarySearch,
    // Library
    ScanLibrary,
    /// `count` is the number of tracks the scan updated; `generation`
    /// and `provider_id` identify the reload/scan this result belongs
    /// to, so stale results (superseded by a later reload or a provider
    /// switch) can be ignored.
    LibraryScanComplete {
        generation: u64,
        provider_id: String,
        count: usize,
    },
    LibraryLoaded {
        generation: u64,
        provider_id: String,
        tracks: Vec<Track>,
        albums: Vec<Album>,
        artists: Vec<Artist>,
        cover_images: HashMap<String, widget::icon::Handle>,
        /// Raw cover art bytes for blur processing.
        cover_art_bytes: HashMap<String, Vec<u8>>,
    },
    /// Filesystem watcher detected changes in music directories.
    /// Contains the deduplicated list of changed paths after debounce.
    FilesChanged(Vec<PathBuf>),

    /// Incremental batch of albums from a remote provider (e.g. Subsonic).
    /// Each batch appends albums, derives tracks/artists, and updates the UI.
    LibraryBatch {
        generation: u64,
        provider_id: String,
        albums: Vec<Album>,
        cover_images: HashMap<String, widget::icon::Handle>,
        /// Raw cover art bytes for blur processing.
        cover_art_bytes: HashMap<String, Vec<u8>>,
    },
    /// Signals that incremental loading is complete.
    LibraryLoadComplete {
        generation: u64,
        provider_id: String,
    },

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

    // Track selection
    PlayTrackIndex(usize),
    PlayAlbum(usize),
    PlayAlbumTrack(usize, usize),
    PlayArtistAlbum(usize, usize),
    PlayArtistTrack(usize, usize, usize),

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
    ArtistInfoLoaded(crate::library::artist_info::ArtistInfoOutcome),

    // Songs view
    SortSongs(songs::SortField),
    /// Toggle the favorites-only filter in the Songs view.
    ToggleFavoritesFilter,

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
    /// Playlists have been loaded from the provider.
    PlaylistsLoaded(Vec<crate::library::Playlist>),

    // Smart playlists view — a single wrapped variant keeps this feature's
    // full message vocabulary (list/detail/editor) out of the top-level enum.
    SmartPlaylists(crate::views::smart_playlists::SmartPlaylistMessage),

    // Genre filtering
    /// Filter tracks by genre name.
    FilterByGenre(String),

    // Genres view
    /// Select a genre to show its tracks.
    SelectGenre(usize),
    /// Go back from genre detail to the grid.
    BackToGenreGrid,
    /// Play a track in the genre detail view (index within genre_tracks).
    PlayGenreTrack(usize),
    /// Genres have been loaded from the provider.
    GenresLoaded(Vec<String>),
    /// Genre tracks have been loaded.
    GenreTracksLoaded(Vec<Track>),

    // Folders view
    /// Messages from the folder browse view (navigation, playback,
    /// favorite/rating delegation) — see `views::folders::FolderMessage`.
    Folders(crate::views::folders::FolderMessage),

    // Lyrics
    ShowLyrics,
    LyricsLoaded(Option<Lyrics>),
    FetchLyricsOnline,

    // Equalizer
    EqSetBand(usize, f32),
    EqSetPreset(crate::player::equalizer::EqPreset),
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
    /// AutoEQ index fetched successfully (or error).
    AutoEQIndexLoaded(Result<Vec<crate::autoeq::AutoEQProfileMetadata>, String>),
    /// Select an AutoEQ profile by path from the dropdown.
    EqSelectAutoEQ(String),
    /// AutoEQ profile fetched and ready to apply (or error).
    AutoEQProfileLoaded(Result<crate::autoeq::AutoEQProfile, String>),

    // Settings
    AddMusicDir,
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
    MpdTestResult(usize, Result<(), String>),

    // MPD provider events
    MpdConnected(String),
    MpdConnectionFailed(String, String),
    /// `provider_id`, plus which MPD subsystem changed (only
    /// `Database`/`Update`/`StoredPlaylist` trigger a reload in
    /// `update()` — see `subscriptions::idle_action`).
    MpdIdleEvent(String, super::subscriptions::IdleSubsystem),
    /// Polled status from the active MPD backend (position, duration,
    /// state, volume). `song` is `Some` only on the tick where MPD's
    /// current song identity changed (including the first tick after the
    /// subscription starts) — most ticks carry `None`.
    MpdStatusUpdate {
        position: Duration,
        duration: Duration,
        state: PlaybackState,
        volume: f32,
        song: Option<Track>,
    },
    /// An async MPD command failed — log and let the next poll self-correct.
    MpdCommandError(String),

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
    SubsonicTestResult(usize, Result<(), String>),
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
    /// Blurred cover art, accent colour, and a separately-decoded larger
    /// cover handle (for the expanded now-playing view) are ready for
    /// `album_key`. The blur/large-cover handles are `None` when their
    /// respective decode failed (still worth delivering the accent); the
    /// accent is `None` when extraction found no legible dominant hue.
    BlurReady(
        String,
        Option<widget::icon::Handle>,
        Option<crate::library::palette::Accent>,
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
    /// Load a specific preset file, bypassing the playlist, with a smooth
    /// transition.
    #[cfg(feature = "visualizer")]
    LoadVizPreset(PathBuf),
    /// Lock/unlock automatic preset transitions.
    #[cfg(feature = "visualizer")]
    SetVizLocked(bool),
    /// Adjust beat-reactivity sensitivity.
    #[cfg(feature = "visualizer")]
    SetVizBeatSensitivity(f32),
    /// Background preset scan finished.
    #[cfg(feature = "visualizer")]
    VizPresetsScanned(Vec<crate::views::now_playing::visualizer::PresetEntry>),

    // Notifications
    // Podcasts
    /// UI events from the podcasts view (see `views::podcasts::PodcastMessage`).
    Podcast(podcasts::PodcastMessage),
    /// Async results for the podcasts page (see `app::podcast_page::PodcastEvent`).
    PodcastEvent(super::podcast_page::PodcastEvent),
    /// A podcast/radio icon (artwork or favicon) finished downloading.
    /// `Some((width, height, rgba_pixels))` on success (already decoded
    /// and downscaled off-thread by `load_online_icons`, so the handler
    /// can build a `widget::icon::from_raster_pixels` handle directly);
    /// `None` when the fetch or decode failed.
    OnlineIconLoaded(String, Option<(u32, u32, Vec<u8>)>),

    // Radio
    /// UI events from the radio view (see `views::radio::RadioMessage`).
    Radio(radio_view::RadioMessage),
    /// Async results for the radio page (see `app::radio_page::RadioEvent`).
    RadioEvent(super::radio_page::RadioEvent),

    /// A toast notification was dismissed (by timeout or user action).
    CloseToast(widget::toaster::ToastId),

    // Convert / transcode / rip local files
    /// UI actions from the convert view (see `views::convert::ConvertMessage`).
    Convert(convert::ConvertMessage),
    /// Async results and ticks for the convert page (see
    /// `app::convert_page::ConvertEvent`).
    ConvertEvent(super::convert_page::ConvertEvent),

    // Application lifecycle
    Quit,
    /// An MPRIS2 D-Bus event: either the server handle becoming available,
    /// or a command relayed from a media-key/shell-applet D-Bus call.
    Mpris(crate::mpris::MprisEvent),
    /// Open ad-hoc audio files outside the library (double-clicked in a
    /// file manager via `Exec=lyra %U`, passed on the command line, or
    /// forwarded from another running instance's MPRIS `OpenUri`).
    OpenFiles(Vec<PathBuf>),
    /// The background tag-read kicked off by `OpenFiles` has finished;
    /// queues the resulting tracks for playback. Empty when none of the
    /// paths were readable audio files.
    OpenFilesScanned(Vec<Track>),
    /// Result of a background cover-art resolve kicked off by
    /// `publish_mpris` for a track not yet in the MPRIS art cache.
    MprisArtResolved(i64, Option<String>),
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
            albums::AlbumMessage::AddToPlaylist(uri, pid) => Message::AddToPlaylist(uri, pid),
            albums::AlbumMessage::ToggleViewMode => Message::ToggleAlbumsViewMode,
            albums::AlbumMessage::PlayNext(tracks) => Message::PlayNext(tracks),
            albums::AlbumMessage::AddToQueue(tracks) => Message::AddToQueue(tracks),
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
            artists::ArtistMessage::ToggleViewMode => Message::ToggleArtistsViewMode,
            artists::ArtistMessage::PlayNext(tracks) => Message::PlayNext(tracks),
            artists::ArtistMessage::AddToQueue(tracks) => Message::AddToQueue(tracks),
            artists::ArtistMessage::ToggleBioExpanded => Message::ToggleArtistBioExpanded,
        }
    }
}
