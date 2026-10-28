about = About
file = File
albums = Albums
artists = Artists
songs = Songs
equalizer = Equalizer
lyrics = Lyrics
queue = Queue
add-music-folder = Add Music Folder
unknown-album = Unknown album
play = Play
pause = Pause
next = Next
previous = Previous
shuffle = Shuffle
repeat = Repeat
volume = Volume
search = Search
settings = Settings
save = Save
remove = Remove
connected = Connected
connection-failed = Connection Failed
playlists = Playlists
genres = Genres
delete-playlist = Delete
rename-playlist = Rename
play-all = Play All
crossfade = Crossfade
crossfade-duration = Crossfade Duration
replay-gain = Replay Gain
replay-gain-off = Off
replay-gain-track = Track
replay-gain-album = Album
replay-gain-auto = Auto

# SettingsView
settings-library = Library
settings-playback = Playback
settings-about = About
settings-startup = Startup
startup-page = Start on
startup-page-description = The section shown when Aulos opens
startup-provider = Provider at startup
startup-provider-description = The music source selected when Aulos opens

# RowFoundation
favorite-add = Add to favorites
favorite-remove = Remove from favorites

equalizer-section-preset = Preset
equalizer-section-preamp = Preamp
equalizer-preamp-value = { $db } dB

play-album = Play Album
back-to-albums = Back to albums

artist-album-count-one = { $count } album
artist-album-count-other = { $count } albums
artist-track-count-one = { $count } track
artist-track-count-other = { $count } tracks
genre-track-count-one = { $count } track
genre-track-count-other = { $count } tracks
genre-label = Genre
playlist-track-count-one = { $count } track
playlist-track-count-other = { $count } tracks

# SongsTable
songs-column-number = #
songs-track-count-one = { $count } track
songs-track-count-other = { $count } tracks

# PodcastsView
podcasts = Podcasts
subscribe = Subscribe
subscriptions = Subscriptions

# ConvertView
convert = Convert

viz-beat-sensitivity = Beat sensitivity

# --- folders ---
folders = Folders
folders-root = Library
folder-empty = Empty folder
folder-empty-hint = This folder has no tracks or subfolders.
play-folder = Play Folder
folder-track-count-one = { $count } track
folder-track-count-other = { $count } tracks
folder-subfolder-count-one = { $count } folder
folder-subfolder-count-other = { $count } folders

# --- artist tags ---
split-artist-tags = Split multi-artist tags
split-artist-tags-description = Split tags like "Artist A feat. B" into individual artists so each gets its own entry and collaborations are found under any contributor.
artist-tag-delimiters = Delimiters
artist-tag-delimiters-description = Separators used to split artist tags, tried longest-first. Edit as a " | "-separated list.

new-smart-playlist = New Smart Playlist
add-rule = Add Rule
remove-rule-tooltip = Remove rule
smart-playlist-value-placeholder = Value...
smart-playlist-limit = Limit to
smart-playlist-field-title = Title
smart-playlist-field-artist = Artist
smart-playlist-field-album-artist = Album Artist
smart-playlist-field-album = Album
smart-playlist-field-genre = Genre
smart-playlist-field-year = Year
smart-playlist-field-rating = Rating
smart-playlist-field-favorite = Favorite
smart-playlist-field-duration = Duration (seconds)
smart-playlist-field-bitrate = Bitrate
smart-playlist-field-sample-rate = Sample Rate
smart-playlist-field-play-count = Play Count
smart-playlist-field-days-since-played = Days Since Last Played
smart-playlist-field-quality = Quality
smart-playlist-quality-hires = Hi-Res
smart-playlist-quality-cd = CD
smart-playlist-quality-lossy = Lossy
smart-playlist-op-is = is
smart-playlist-op-is-not = is not
smart-playlist-op-contains = contains
smart-playlist-op-not-contains = does not contain
smart-playlist-op-starts-with = starts with
smart-playlist-op-ends-with = ends with
smart-playlist-op-greater-than = is greater than
smart-playlist-op-less-than = is less than
smart-playlist-op-between = is between
smart-playlist-order-title = Title
smart-playlist-order-artist = Artist
smart-playlist-order-album = Album
smart-playlist-order-year = Year
smart-playlist-order-rating = Rating
smart-playlist-order-duration = Duration
smart-playlist-order-random = Random
smart-playlist-order-recently-added = Recently Added
smart-playlist-order-play-count = Play Count
smart-playlist-order-last-played = Last Played

## Converter
convert-add-files = Add Files
convert-output-section = Output
convert-dir-change = Change…
convert-format = Format
convert-sample-rate = Sample Rate
convert-rate-source = Source Rate
convert-rate-hz = { $hz } Hz
convert-cancel-all = Cancel All
convert-clear-finished = Clear Finished
convert-kind-convert = Convert
convert-kind-cuesplit = Split CUE
convert-format-flac = FLAC
convert-format-wav16 = WAV (16-bit)
convert-format-wav24 = WAV (24-bit)
convert-format-wav32float = WAV (32-bit float)
convert-state-queued = Queued
convert-state-running = Converting…
convert-state-done = Done
convert-state-failed-chip = Failed
convert-state-cancelled = Cancelled
convert-cancel-tooltip = Cancel
convert-retry-tooltip = Retry

## Player UI
stop = Stop
now-playing-live = LIVE
queue-now-playing-label = Now playing
queue-up-next-one = Up next · { $count } track · { $duration }
queue-up-next-other = Up next · { $count } tracks · { $duration }
queue-clear = Clear
queue-play-next = Play next
queue-add = Add to queue

## Context menus (track rows, album cards)
properties = Properties
menu-go-to-album = Go to album
menu-go-to-artist = Go to artist
menu-add-to-playlist = Add to playlist
menu-edit-tags = Edit tags…
menu-edit-album-tags = Edit album tags…
menu-lookup-musicbrainz = Look up on MusicBrainz
menu-scan-acoustid = Scan with AcoustID

## Radio
radio = Radio
radio-add-station = Add Station
radio-tab-discover = Discover
radio-add-url-required = Enter a stream URL
radio-add-url-invalid = URL must start with http:// or https://
radio-add-by-url = Add by URL
radio-no-filter-matches = No matching stations
radio-no-filter-matches-hint = Try a different search term, or clear the filter
radio-preset-popular = Popular
radio-preset-top-voted = Top Voted
radio-retry = Retry
radio-no-results = No stations found
radio-no-results-hint = Try a different search, tag or country
radio-sort-most-played = Most played
radio-sort-most-voted = Most voted
radio-sort-name = Name
radio-sort-bitrate = Bitrate

## Converter
settings-experimental = Experimental
experimental-converter = Audio Converter
experimental-converter-description = Convert, transcode, or rip local audio/video files and CUE sheets. Experimental: enable to add a "Convert" page to the navigation.
convert-format-aiff16 = AIFF (16-bit)
convert-format-aiff24 = AIFF (24-bit)
convert-format-mp3 = MP3
convert-format-aac = AAC (M4A)
convert-format-opus = Opus
convert-format-vorbis = Ogg Vorbis
convert-format-alac = ALAC (M4A)
convert-format-needs-ffmpeg = { $format } (needs ffmpeg)
convert-flac-bitdepth = Bit Depth
convert-flac-bitdepth-auto = Auto
convert-flac-bitdepth-16 = 16-bit
convert-flac-bitdepth-24 = 24-bit
convert-mp3-mode = Quality
convert-mp3-vbr = VBR V{ $q }
convert-mp3-cbr = CBR { $kbps } kbps
convert-bitrate = Bitrate
convert-rate-hz-kbps = { $kbps } kbps
convert-vorbis-quality = Quality

## Podcasts
podcast-tab-discover = Discover
podcast-add-by-url = Add by URL
podcast-cancel = Cancel
podcast-add-url-required = Enter a feed URL
podcast-add-url-invalid = URL must start with http:// or https://
podcast-unplayed-badge = { $count } unplayed
podcast-updated-at = Updated { $when }
podcast-never-updated = Never updated
podcast-retry = Retry
podcast-no-results = No shows found
podcast-subscribed-badge = Subscribed
podcast-refresh = Refresh
podcast-unsubscribe = Unsubscribe
podcast-filter-all = All
podcast-refreshing = Refreshing…
podcast-episode-count =
    { $count ->
        [one] { $count } episode
       *[other] { $count } episodes
    }
podcast-time-left = { $time } left

## Artists
settings-artist-info = Artist Info
fetch-artist-info = Fetch artist images and info online
fetch-artist-info-description = Look up artist photos (via Deezer) and biography text (via Wikipedia) when browsing in Local or MPD mode. Off by default. Ignored in Subsonic/Navidrome mode, which always shows the server's own artist info instead.
artist-bio-more = Show more
artist-bio-less = Show less

## Home
home = Home
home-greeting-morning = Good morning
home-greeting-afternoon = Good afternoon
home-greeting-evening = Good evening
home-greeting-named = { $greeting }, { $name }
home-resume =
    You were { $count ->
        [one] 1 track
       *[other] { $count } tracks
    } into { $album }.
home-subtitle =
    { $count ->
        [one] 1 album in your library
       *[other] { $count } albums in your library
    }
home-welcome-title = Welcome to Aulos
home-open-settings = Open settings
home-browse-radio = Browse radio
home-no-history-hint = Start listening and this page fills up with what you played recently, your favorites of the month and albums to rediscover.
home-most-played-month = Most played this month
home-top-artists-month = Top artists this month
home-recently-added = Recently added
home-rediscover = Rediscover
home-rediscover-hint = Albums you haven't played in a while
home-random-picks = Random picks
home-shuffle-picks = Shuffle picks
home-plays =
    { $count ->
        [one] 1 play
       *[other] { $count } plays
    }
home-decade-title = The { $decade }s
home-decade-albums =
    { $count ->
        [one] 1 album
       *[other] { $count } albums
    }
home-decade-play = Shuffle play
home-decade-empty-hint = No albums from this decade in your library
home-back = Back

viz-toggle = Visualizer

## Drawers (settings, providers, equalizer, lyrics, queue)
settings-library-description = Folders Aulos scans for music.
settings-artist-tags = Artist Tags
settings-artist-tags-description = How multi-artist tags are read from your files.
settings-playback-description = Transitions and loudness.
crossfade-description = Fade between tracks for smoother transitions.
replay-gain-description = Even out loudness between tracks or albums.
settings-appearance = Appearance
settings-appearance-description = How the library looks.
grid-size = Grid size
grid-size-description = Size of album, artist and other cards.
settings-about-description = Version, license and links
queue-history-label = Recently played

## Scrobbling
scrobbling = Scrobbling
scrobble-connect = Connect
scrobble-disconnect = Disconnect
scrobble-auth-hint = Approve Aulos in the browser window that just opened, then come back and press "Complete connection".
scrobble-error = Error: { $message }
scrobble-error-no-keys = Enter your Last.fm API key and secret first.
scrobble-error-no-token = Press "Connect" first.
scrobble-import-cancel = Cancel import
scrobble-import-done = Imported { $imported } plays: { $matched } of { $fetched } listens matched your library ({ $skipped } already there)
scrobble-import-cancelled = Import cancelled after { $fetched } listens: { $imported } plays added
scrobble-import-failed = Import stopped: { $message } ({ $imported } plays added so far)
home-because-you-listened = Because you listened to { $artist }
home-in-library = In your library
settings-search-placeholder = Search settings

## Playlist import / export (M3U)
settings-playlists = Playlists
settings-playlists-description = How playlists are exported to M3U files.
m3u-relative-paths = Use relative paths in exported playlists
m3u-relative-paths-description = Write track paths relative to the playlist file so the playlist keeps working if you move the folder. Turn off to write absolute paths.

## Mini player
mini-player = Mini player

## Compilations and album filters
settings-compilations = Compilations
settings-compilations-description = Albums by various artists are grouped under “Various Artists”.
show-compilations-in-artists = Show compilations in artist view
show-compilations-in-artists-description = List “Various Artists” in the Artists page. Compilations always stay available from the Albums page.
filter-albums = Albums
filter-compilations = Compilations
filter-eps = EPs
filter-singles = Singles
filter-live = Live
filter-favourites = Favourites
release-kind-album = Album
release-kind-ep = EP
release-kind-single = Single
release-kind-live = Live
release-kind-compilation = Compilation
release-count-album =
    { $count ->
        [one] { $count } album
       *[other] { $count } albums
    }
release-count-ep =
    { $count ->
        [one] { $count } EP
       *[other] { $count } EPs
    }
release-count-single =
    { $count ->
        [one] { $count } single
       *[other] { $count } singles
    }
release-count-live =
    { $count ->
        [one] { $count } live album
       *[other] { $count } live albums
    }
release-count-compilation =
    { $count ->
        [one] { $count } compilation
       *[other] { $count } compilations
    }
sort-recently-added = Recently added
sort-added = Added
sort-title = Title
sort-artist = Artist
sort-year = Year
scan-progress-title = Scanning { $root }

## Playback extras: stop after track, fades, party mode, auto-play, desktop integration
party-mode = Party mode
party-mode-description = Endless random playback from the genres below (the whole library when none is selected).
party-genres = Party genres
auto-play = Auto-play
auto-play-description = What to play when the queue runs out.
auto-play-off = Off
auto-play-random = Random
auto-play-similar = Similar
fade-duration = Fade in and out
fade-description = Smoothly fade the sound when playing, pausing, stopping or skipping tracks.
settings-playback-extras = Fades and continuous playback
settings-playback-extras-description = Fade transitions and what plays when the queue ends.
settings-desktop = Desktop
settings-desktop-description = Notifications, power management and window behaviour.
notify-track-change = Track change notifications
notify-track-change-description = Show the cover art and track details when the window is in the background.
inhibit-suspend = Prevent sleep while playing
inhibit-suspend-description = Keep the computer awake and the screen from idling while music plays.
background-playback = Keep playing when the window is closed
background-playback-description = Closing the window minimizes Aulos while music plays instead of quitting.

## Properties drawer
album-properties = Album properties
props-provider-unavailable = The server for this track isn't connected.
props-not-applicable = n/a
props-artist = Artist
props-album = Album
props-year = Year
props-date = Date
props-source = Source
props-source-local = Local library
props-source-radio = Internet radio
props-source-podcast = Podcast
props-codec = Codec
props-bitrate = Bitrate
props-sample-rate = Sample rate
props-bit-depth = Bit depth
props-channels = Channels
props-duration = Duration
props-encoder = Encoder
props-tag-formats = Tags
props-mode-cbr = constant (CBR)
props-mode-vbr = variable (VBR)
props-mode-abr = average (ABR)
props-mode-lossless = lossless
props-nominal = nominal { $value }
props-audio-data = { $size } of audio
props-tracks = Tracks
props-formats = Formats
props-sample-rates = Sample rates
props-bit-depths = Bit depths

## Tag editor settings
settings-tagger = Tag editor
settings-tagger-description = Online lookups and fingerprinting for the built-in tag editor.
settings-tagger-app-key = AcoustID application key
settings-tagger-user-key = Your AcoustID API key
settings-tagger-caa-cover = Use the Cover Art Archive front cover

## Tag editor
tagger-title = Tag editor
tagger-loading =
    { $count ->
        [one] Reading { $count } file…
       *[other] Reading { $count } files…
    }

## Tag editor: toolbar
tagger-add-files = Add files…
tagger-add-folder = Add folder…
tagger-submit = Submit AcoustIDs
tagger-submit-count = Submit AcoustIDs ({ $count })
tagger-save = Save
tagger-saving = Saving { $done } of { $total }…
tagger-revert = Revert
tagger-revert-all = Revert all
tagger-remove = Remove
tagger-clear = Clear
tagger-add-as-release-tooltip = Add these files as a new release on MusicBrainz

## Tag editor: banners
tagger-dismiss = Dismiss
tagger-discard = Discard
tagger-cancel = Cancel
tagger-open-settings = Open Settings
tagger-scan-no-match = AcoustID found no release for these files ({ $identified } of { $total } identified).
tagger-scan-failures =
    { $count ->
        [one] One file could not be fingerprinted or looked up.
       *[other] { $count } files could not be fingerprinted or looked up.
    }
tagger-scan-cancel = Cancel scan
tagger-submitting = Submitting fingerprints to AcoustID…

## Tag editor: files
tagger-files-count =
    { $count ->
        [one] { $count } file
       *[other] { $count } files
    }
tagger-selected-count = { $count } selected
tagger-select-all = Select all
tagger-unclustered = Files without an album
tagger-various-artists = Various artists
tagger-unknown-artist = Unknown artist
tagger-cluster-select = Select this album
tagger-cluster-lookup = Look up this album on MusicBrainz
tagger-cluster-scan = Scan this album with AcoustID
tagger-cluster-add-release = Add this album as a release on MusicBrainz
tagger-empty-title = No files yet
tagger-empty-hint = Add audio files or a folder to edit their tags. Files can also be sent here from the library with “Edit tags…”.
tagger-status-modified = Unsaved changes
tagger-status-saving = Saving…
tagger-status-saved = Saved
tagger-status-error = Could not save: { $error }
tagger-assignment = → { $number } { $title } { $score }%
tagger-unmatched = No matching track on the release
tagger-acoustid-open = Open this AcoustID on acoustid.org
tagger-recording-open = Open the recording on MusicBrainz
tagger-submission-pending = Submitting…
tagger-submission-pending-status = Submitted, waiting to be processed
tagger-submission-imported-status = Submitted and imported
tagger-submission-submitted = Submitted ({ $status })
tagger-submission-failed = Submission failed: { $error }

## Tag editor: metadata
tagger-pane-files = Files
tagger-metadata-title = Metadata
tagger-metadata-selection =
    { $count ->
        [one] 1 file selected
       *[other] { $count } files selected
    }
tagger-metadata-empty-title = Nothing selected
tagger-metadata-empty-hint = Select one or more files to see and edit their tags.
tagger-metadata-readonly = The selected files can't be edited, so their tags are shown read-only.
tagger-different-values = (different values)
tagger-multiline-tooltip = This value spans several lines and can't be edited here.
tagger-multi-hint = Multiple values: separate them with “; ”. Only list fields such as genre or artists are split.
tagger-revert-field = Put this field back to what is on disk
tagger-delete-field = Remove this field
tagger-add-field-custom = Custom field name
tagger-add-field-value = Value
tagger-add-field = Add
tagger-add-field-known = Choose a field…
tagger-add-field-error-no-selection = Select a file that can be edited first.
tagger-add-field-error-no-key = Choose a field or type a field name.
tagger-add-field-error-invalid-key = A field name may only use visible ASCII characters and no “=”.
tagger-add-field-error-no-value = Type a value for the field.

## Tag editor: cover art
tagger-cover-empty = No embedded pictures.
tagger-cover-revert = Revert pictures
tagger-cover-remove = Remove this picture
tagger-cover-in-files = in { $count } of { $total } files
tagger-cover-placeholder = Decoding…
tagger-cover-details = { $mime } · { $size }
tagger-picture-front = Front cover
tagger-picture-back = Back cover
tagger-picture-leaflet = Leaflet
tagger-picture-media = Media
tagger-picture-artist = Artist
tagger-picture-other = Other
tagger-picture-type = Type { $kind }
tagger-size-kb = { $value } KB
tagger-size-mb = { $value } MB

## Tag editor: MusicBrainz lookup
tagger-lookup-title = MusicBrainz lookup
tagger-lookup-files =
    { $count ->
        [one] 1 file
       *[other] { $count } files
    }
tagger-lookup-artist = Artist
tagger-lookup-album = Album or release title
tagger-lookup-search = Search
tagger-lookup-idle = Type an artist and an album, then search.
tagger-lookup-searching = Searching MusicBrainz…
tagger-lookup-failed = The search failed: { $error }
tagger-lookup-none = No release found. Adjust the artist or album and search again.
tagger-lookup-tracks =
    { $count ->
        [one] 1 track
       *[other] { $count } tracks
    }
tagger-lookup-loading-release = Loading the release…
tagger-lookup-release-failed = The release could not be loaded: { $error }
tagger-lookup-acoustid-summary = AcoustID matched { $matched } of { $total } files to { $release }
tagger-lookup-matched = Matched { $matched } of { $total } files
tagger-lookup-unused =
    { $count ->
        [one] 1 track of the release has no file.
       *[other] { $count } tracks of the release have no file.
    }
tagger-lookup-unmatched = Not matched: { $files }
tagger-lookup-unmatched-more = Not matched: { $files } and { $more } more
tagger-lookup-open = Open on MusicBrainz
tagger-lookup-discard = Discard release
tagger-lookup-close = Close
tagger-lookup-cover-loading = Fetching the front cover…
tagger-lookup-cover-none = The Cover Art Archive has no front cover for this release.
tagger-lookup-cover-failed = The front cover could not be fetched: { $error }
tagger-lookup-cover-found = Front cover found on the Cover Art Archive.

## Tag editor: field names
tagger-field-title = Title
tagger-field-artist = Artist
tagger-field-album = Album
tagger-field-albumartist = Album artist
tagger-field-date = Date
tagger-field-genre = Genre
tagger-field-tracknumber = Track number
tagger-field-totaltracks = Total tracks
tagger-field-discnumber = Disc number
tagger-field-totaldiscs = Total discs
tagger-field-discsubtitle = Disc subtitle
tagger-field-compilation = Compilation
tagger-field-comment = Comment
tagger-field-lyrics = Lyrics
tagger-field-artists = Artists
tagger-field-composer = Composer
tagger-field-lyricist = Lyricist
tagger-field-conductor = Conductor
tagger-field-arranger = Arranger
tagger-field-producer = Producer
tagger-field-engineer = Engineer
tagger-field-mixer = Mixer
tagger-field-remixer = Remixer
tagger-field-originaldate = Original release date
tagger-field-originalyear = Original release year
tagger-field-label = Label
tagger-field-catalognumber = Catalog number
tagger-field-barcode = Barcode
tagger-field-isrc = ISRC
tagger-field-asin = ASIN
tagger-field-media = Media
tagger-field-releasecountry = Release country
tagger-field-releasestatus = Release status
tagger-field-releasetype = Release type
tagger-field-script = Script
tagger-field-language = Language
tagger-field-bpm = BPM
tagger-field-key = Musical key
tagger-field-mood = Mood
tagger-field-grouping = Grouping
tagger-field-copyright = Copyright
tagger-field-license = License
tagger-field-encodedby = Encoded by
tagger-field-encodersettings = Encoder settings
tagger-field-website = Website
tagger-field-titlesort = Title sort order
tagger-field-artistsort = Artist sort order
tagger-field-albumsort = Album sort order
tagger-field-albumartistsort = Album artist sort order
tagger-field-composersort = Composer sort order
tagger-field-musicbrainz-recordingid = MusicBrainz recording ID
tagger-field-musicbrainz-trackid = MusicBrainz track ID
tagger-field-musicbrainz-albumid = MusicBrainz release ID
tagger-field-musicbrainz-artistid = MusicBrainz artist ID
tagger-field-musicbrainz-albumartistid = MusicBrainz release artist ID
tagger-field-musicbrainz-releasegroupid = MusicBrainz release group ID
tagger-field-musicbrainz-workid = MusicBrainz work ID
tagger-field-musicbrainz-discid = MusicBrainz disc ID
tagger-field-musicbrainz-labelid = MusicBrainz label ID
tagger-field-acoustid-id = AcoustID
tagger-field-acoustid-fingerprint = AcoustID fingerprint
tagger-field-replaygain-track-gain = ReplayGain track gain
tagger-field-replaygain-track-peak = ReplayGain track peak
tagger-field-replaygain-album-gain = ReplayGain album gain
tagger-field-replaygain-album-peak = ReplayGain album peak

## Tag editor page (navigation and notices)
tagger-notice-no-audio = No audio files were found in that folder.
tagger-notice-no-user-key = Save your AcoustID API key in Settings to submit fingerprints.
tagger-notice-keyring-failed = Couldn't read your AcoustID key from the keyring: { $error }

language = Language
language-desc = Language of the interface. Changes apply immediately.
language-system = System default

# --- browse.ftl ---
# props (views/properties.rs)
props-tab-track = Track
props-tab-album = Album
props-empty-title = Nothing selected
props-empty-hint = Open Properties from a track or album menu.
props-reading-file = Reading file…
props-reading-files = Reading files…
props-read-file-failed = Could not read the file: { $error }
props-read-files-failed = Could not read the files: { $error }
props-container = Container
props-size = Size
props-replaygain-track = ReplayGain track
props-replaygain-album = ReplayGain album
props-location = Location
props-not-read = Not read
props-not-read-count =
    { $count ->
        [one] { $count } track
       *[other] { $count } tracks
    }
props-album-not-in-library = { $album } is not in the library

# songs (views/songs.rs)
songs-total-days-hours = { $days } d { $hours } h
songs-total-hours-minutes = { $hours } h { $minutes } m
songs-total-minutes = { $minutes } m
songs-shuffle-all = Shuffle all
songs-empty-title = No songs yet
songs-empty-body = Add a music folder in Settings and Aulos will fill this list.
songs-no-search-matches-title = No matching songs
songs-no-search-matches-body = Nothing in your library matches “{ $query }”.

# genres (views/genres.rs)
genres-song-count =
    { $count ->
        [one] { $count } song
       *[other] { $count } songs
    }
genres-album-count =
    { $count ->
        [one] { $count } album
       *[other] { $count } albums
    }
genres-summary = { $songs } · { $albums } · { $duration }
genres-scanning = Scanning your library…
genres-empty-title = No genres yet
genres-empty-body = Genres come from the tags of your music. Songs without a genre tag are not listed here.
genres-no-matches-title = No matching genres
genres-no-matches-body = No genre matches “{ $query }”.
genres-not-found-title = Genre not found
genres-not-found-body = There are no songs tagged “{ $name }” in this library.
genres-all = All genres
genres-detail-empty-title = No songs in this genre
genres-detail-empty-body = Nothing tagged with this genre could be loaded from the active source.
genres-play-in-order = Play in order
genres-top-artists = Top artists

# folders (views/folders.rs)
folders-empty-title = No folders yet
folders-empty-body = Add a music folder in Settings and its directory structure will show up here.
folders-song-count =
    { $count ->
        [one] { $count } song
       *[other] { $count } songs
    }

# menus (views/menus.rs)
menus-go-to-genre = Go to genre
menus-new-playlist = New playlist…
menus-no-playlists = No playlists yet
menus-album-not-in-library = { $album } is not in the library
menus-no-tracks-for = No tracks for { $name }
menus-new-playlist-title = New playlist
menus-prompt-name = Name
menus-prompt-name-hint = Playlist name
menus-prompt-create = Create
menus-key-enter = Enter

# artists (views/artists.rs)
artists-filter-album-artists = Album artists
artists-filter-all-credited = All credited
artists-photos-hint = Photos appear when Online data › Artist photos is on. Until then artists get initials on a tint.
artists-no-matches-title = No matching artists
artists-no-matches-body = Nothing in your artists matches “{ $query }”.
artists-no-favourites-title = No favourite artists
artists-no-favourites-body = Heart a few songs and the artists behind them show up here.
artists-empty-title = No artists yet
artists-empty-body = Add a music folder or connect an MPD or Subsonic server. Artists appear as soon as the first files are scanned.
artists-column-artist = Artist
artists-column-length = Length
artists-loading = Loading your library…
artists-not-found-title = Artist not found
artists-not-found-body = “{ $name }” is not in your library any more.
artists-all = All artists
artists-summary = { $albums } · { $songs } · { $duration }
artists-song-count =
    { $count ->
        [one] { $count } song
       *[other] { $count } songs
    }
artists-more = More
artists-top-tracks = Top tracks
artists-about = About
artists-bio-source = via MusicBrainz · Wikipedia
artists-no-bio = No biography found for this artist.
artists-online-opt-in = Online data is opt-in · Settings › Online data

# expanded (views/expanded.rs)
expanded-nothing-playing = Nothing playing
expanded-collapse-tip = Collapse (Esc)
expanded-heading-album = Playing from album
expanded-heading-now-playing = Now playing
expanded-mute = Mute
expanded-unmute = Unmute
expanded-more = More
expanded-replaygain-chip = RG { $gain } dB
expanded-up-next = Up next
expanded-nothing-queued = Nothing queued
expanded-more-queued = { $count } more · { $duration }
expanded-lyrics-searching = Looking for lyrics…
expanded-lyrics-not-found = No lyrics found
expanded-lyrics-not-found-hint = Nothing embedded in the file or published online.
expanded-lyrics-none = No lyrics
expanded-lyrics-none-hint = None embedded in the file or in a .lrc next to it.
expanded-search-online = Search online

# --- library.ftl ---
# albums (views/albums.rs)
albums-toast-playing-next = Playing next
albums-toast-added-to-queue = Added to queue
albums-no-match-search-title = No albums match
albums-no-match-search-body = Nothing in your library matches this search.
albums-no-match-search-clear = Clear search
albums-no-match-filters-title = No albums match these filters
albums-no-match-filters-body = Try a different kind, decade or genre.
albums-no-match-filters-clear = Clear filters
albums-filter-all = All
albums-filter-eps = EPs
albums-filter-singles = Singles
albums-filter-compilations = Compilations
albums-filter-live = Live
albums-filter-decade = Decade
albums-filter-any-decade = Any decade
albums-filter-any-genre = Any genre
albums-filter-favourites = Favourites
albums-search-match-summary = of { $total } albums match “{ $query }”
albums-search-artist-count =
    { $count ->
        [one] { $count } artist
       *[other] { $count } artists
    }
albums-search-song-count =
    { $count ->
        [one] { $count } song
       *[other] { $count } songs
    }
albums-search-clears = clears
albums-scanning = Scanning your library…
albums-empty-title = Nothing on the shelves yet
albums-empty-body = Add a music folder or connect an MPD or Subsonic server. Scanning runs in the background — play the first album as soon as it lands.
albums-empty-add-folder = Add music folder
albums-empty-connect-server = Connect a server
albums-playing-badge = Playing
albums-column-album = Album
albums-column-year = Year
albums-column-kind = Kind
albums-column-tracks = Tracks
albums-column-length = Length
albums-column-quality = Quality
albums-kind-album = Album
albums-kind-compilation = Compilation
albums-duration-hours = { $hours } h { $minutes } min
albums-duration-minutes = { $minutes } min
albums-detail-loading-title = Loading album…
albums-detail-loading-body = The library is still being scanned.
albums-detail-missing-title = Album not found
albums-detail-missing-body = It is no longer in your library.
albums-disc = Disc { $number }
albums-song-count =
    { $count ->
        [one] { $count } song
       *[other] { $count } songs
    }

# home (views/home.rs)
home-similar-hint = Artists you may like
home-not-in-library = Not in your library
home-empty-scanning-body = Scanning your library… Suggestions will appear as soon as your music is indexed.
home-empty-body = Add a music folder or connect a server in Settings. Internet radio and podcasts work without any library.
home-shuffle-everything = Shuffle everything
home-browse-albums = Browse albums
home-see-all = See all
home-open-on-site = Open { $name } on { $site }
home-favourites = Favourites
home-favourites-of-month = Favourites of { $month }
home-song-count =
    { $count ->
        [one] 1 song
       *[other] { $count } songs
    }
home-decades = Decades
home-decade-empty-title = No albums
home-month-1 = January
home-month-2 = February
home-month-3 = March
home-month-4 = April
home-month-5 = May
home-month-6 = June
home-month-7 = July
home-month-8 = August
home-month-9 = September
home-month-10 = October
home-month-11 = November
home-month-12 = December

# playlists (views/playlists.rs)
playlists-duration-hm = { $hours } h { $minutes } m
playlists-duration-m = { $minutes } m
playlists-duration-s = { $seconds } s
playlists-song-count =
    { $count ->
        [one] 1 song
       *[other] { $count } songs
    }
playlists-meta-duration = { $songs } · { $duration }
playlists-meta-rules = { $rules } · { $songs }
playlists-matching-songs = { $count } of { $total } matching songs
playlists-smart-meta = { $count } · updates live
playlists-smart-meta-duration = { $count } · { $duration } · updates live
playlists-new = New playlist
playlists-import-tooltip = Import playlist file
playlists-section-smart = Smart
playlists-menu-rename = Rename…
playlists-menu-export = Export…
playlists-menu-delete = Delete…
playlists-empty-title = No playlists yet
playlists-empty-body = Create a playlist, build a smart playlist from rules, or import an M3U / PLS file.
playlists-pick-title = Pick a playlist
playlists-pick-body = Select a playlist on the left to see its songs.
playlists-kind-playlist = Playlist
playlists-kind-smart = Smart playlist
playlists-kind-smart-label = Smart playlist
playlists-edit-rules = Edit rules
playlists-export = Export
playlists-no-songs = No songs yet. Add songs from the menu of any track.
playlists-no-songs-smart = No songs match these rules.
playlists-order-random = at random
playlists-order-asc = by { $field }, ascending
playlists-order-desc = by { $field }, descending
playlists-editor-name = Name
playlists-name-placeholder = Playlist name
playlists-editor-match = Match
playlists-editor-match-all = all
playlists-editor-match-any = any
playlists-editor-of-rules = of these rules
playlists-editor-from = from
playlists-editor-and = and
playlists-editor-to = to
playlists-editor-songs = songs
playlists-editor-picked = picked
playlists-editor-matches-tail =
    { $count ->
        [one] song matches
       *[other] songs match
    }
playlists-editor-counting = Counting matches…
playlists-editor-save = Save rules
playlists-cancel = Cancel
playlists-create-hint-smart = A smart playlist fills itself from rules you set next.
playlists-create-hint-plain = An empty playlist; add songs from any track's menu.
playlists-create = Create
playlists-rename-title = Rename playlist
playlists-delete-title = Delete “{ $name }”?
playlists-delete-body = This can't be undone. The songs themselves stay in your library.
playlists-export-empty = This playlist is empty, nothing to export
playlists-export-done = Exported { $count } songs to { $file }
playlists-export-done-skipped = Exported { $count } songs to { $file } ({ $skipped } not local files were skipped)
playlists-export-failed = Playlist export failed: { $reason }
playlists-filter-m3u = M3U playlist
playlists-filter-import = Playlists (M3U, M3U8, PLS)
playlists-export-dialog-title = Export playlist
playlists-import-dialog-title = Import playlists

# --- media.ftl ---
# ---- radio view
radio-mode-name = Name
radio-mode-tag = Tag
radio-mode-country = Country
radio-tab-saved = Saved · { $count }
radio-add-name-placeholder = Station name (optional)
radio-search-by-name = Search stations by name
radio-search-by-tag = Search by tag, e.g. jazz
radio-search-by-country = Country code, e.g. IT
radio-filter-saved = Filter saved stations
radio-sort-label = Sort: { $order }
radio-saved-empty-title = No saved stations
radio-saved-empty-body = Find stations in Discover and tap the heart, or add a stream URL.
radio-open-discover = Open Discover
radio-unreachable-title = Station directory is unreachable
radio-unreachable-body = { $count ->
    [one] Discover needs the online station directory. Your { $count } saved station still plays.
   *[other] Discover needs the online station directory. Your { $count } saved stations still play.
}
radio-go-to-saved = Go to Saved
radio-votes = { $votes } votes
radio-on-air = ON AIR
radio-now = ♪ Now: { $title }
radio-remove-from-saved = Remove from saved
radio-save-station = Save station
radio-rename = Rename
radio-removed-toast = Removed { $name } from saved stations
radio-adding-toast = Adding station…

# ---- podcasts view
podcasts-all-shows = All shows
podcasts-refresh-all-feeds = Refresh all feeds
podcasts-refresh-failed-title = { $count ->
    [one] Couldn’t refresh { $count } feed
   *[other] Couldn’t refresh { $count } feeds
}
podcasts-names-two = { $a } and { $b }
podcasts-names-many = { $a }, { $b } and { $more } more
podcasts-refresh-failed-detail = { $names }: { $reason }.
podcasts-refresh-failed-unreachable = { $names } could not be reached.
podcasts-cached-ago = Showing episodes cached { $age } ago.
podcasts-dismiss = Dismiss
podcasts-retrying = Retrying…
podcasts-age-minutes = { $count ->
    [one] { $count } minute
   *[other] { $count } minutes
}
podcasts-age-hours = { $count ->
    [one] { $count } hour
   *[other] { $count } hours
}
podcasts-age-days = { $count ->
    [one] { $count } day
   *[other] { $count } days
}
podcasts-none-title = No subscriptions yet
podcasts-none-body = Find shows in Discover, or paste an RSS feed URL. New episodes show up here and in Home.
podcasts-open-discover = Open Discover
podcasts-menu-open = Open
podcasts-menu-unsubscribe = Unsubscribe…
podcasts-filter-unplayed-count = Unplayed · { $count }
podcasts-filter-downloaded-count = Downloaded · { $count }
podcasts-filter-episodes = Filter episodes
podcasts-no-episodes-title = No episodes
podcasts-no-episodes-body = This feed has no episodes yet.
podcasts-no-episodes-match = No episodes match the filter
podcasts-show-caps = SHOW
podcasts-continue = Continue
podcasts-latest = Latest
podcasts-unsubscribe-hover = Also deletes downloaded episodes and playback positions
podcasts-refresh-this-feed = Refresh this feed
podcasts-new-badge = NEW
podcasts-ep-cancelling = Cancelling…
podcasts-ep-downloading-pct = Downloading · { $pct }%
podcasts-ep-downloading = Downloading…
podcasts-ep-played = Played
podcasts-ep-in-progress = In progress
podcasts-ep-unplayed-dur = Unplayed · { $time }
podcasts-ep-unplayed = Unplayed
podcasts-cancel-download = Cancel download
podcasts-downloaded-remove = Downloaded — click to remove
podcasts-download-offline = Download for offline
podcasts-play-pause = Play / pause
podcasts-mark-unplayed = Mark as unplayed
podcasts-mark-played = Mark as played
podcasts-remove-download = Remove download
podcasts-download = Download
podcasts-search-hint = Search podcasts or paste a feed URL
podcasts-search-failed = Couldn’t search podcasts
podcasts-no-shows-body = Try a different name, or paste the feed URL directly.
podcasts-discover-title = Discover podcasts
podcasts-discover-body = Search the directory by show name, or paste an RSS feed URL and press Enter.
podcasts-cadence-daily = Daily
podcasts-cadence-twice-weekly = Twice weekly
podcasts-cadence-weekly = Weekly
podcasts-cadence-biweekly = Biweekly
podcasts-cadence-monthly = Monthly
podcasts-dur-hm = { $h } h { $m } min
podcasts-dur-m = { $m } min
podcasts-dur-s = { $s } s
podcasts-subscribing-toast = Subscribing…

# ---- converter view
converter-title = Converter
converter-lab-badge = LAB
converter-experimental-note = Experimental. Originals are never modified; output goes to
converter-dir-error = Could not use the output folder: { $error }
converter-change-folder = Change folder
converter-ffmpeg-missing = { $format } needs ffmpeg, which was not found on this system. Install it to convert to this format.
converter-checking-ffmpeg = Checking for ffmpeg…
converter-pane-output = Output
converter-queue-title = Queue
converter-summary = { $jobs ->
    [one] { $jobs } job · { $done } done
   *[other] { $jobs } jobs · { $done } done
}
converter-summary-failed = { $failed } failed
converter-empty-title = Nothing to convert
converter-empty-body = Add audio files, video files or a CUE sheet — or drop them here. Originals are never modified.
converter-resume = Resume
converter-resume-tooltip = Resume the queue
converter-pause-tooltip = Pause the queue
converter-file-fallback = FILE
converter-done-size = Done · { $size }
converter-meta-convert = { $source } → { $target }
converter-meta-cue-one-file = CUE, one file → { $target }
converter-meta-cue-track = CUE split · track { $current } of { $total } → { $target }
converter-meta-cue-tracks = { $total ->
    [one] CUE split · { $total } track → { $target }
   *[other] CUE split · { $total } tracks → { $target }
}
converter-meta-cue-split = CUE split → { $target }
converter-meta-khz = { $khz } kHz
converter-meta-mono = mono
converter-meta-stereo = stereo
converter-show-in-folder = Show in folder
converter-remove-from-queue = Remove from queue
converter-remove-from-list = Remove from list
converter-paused-percent = Paused · { $percent }%
converter-progress-cue = { $percent }% · { $current }/{ $total }
converter-progress-eta = { $percent }% · { $time } left
converter-split-cue = Split by CUE sheet
converter-copy-tags = Copy tags & cover
converter-copy-tags-hint = Tags, ReplayGain and the front cover are carried over
converter-nothing-queued = Nothing queued
converter-queued-files = { $count ->
    [one] { $count } file
   *[other] { $count } files
}
converter-queue-estimate = { $files } · ~{ $from } → ~{ $to }
converter-queue-target = { $files } → { $target }
converter-more-formats = More formats…
converter-depth-na = n/a for { $format }
converter-channels = Channels
converter-compression = Compression
converter-compression-level = Level { $level }
converter-quality-value = Quality { $q }
converter-lossless = Lossless
converter-channels-keep = Keep
converter-channels-mono = Mono
converter-channels-stereo = Stereo
converter-pattern-label = File name pattern
converter-pattern-hint = { $tokens } — / makes folders
converter-cue-hint = Applies to .cue sheets added to the queue
converter-cue-found = { $count ->
    [one] { $name } · { $count } track found
   *[other] { $name } · { $count } tracks found
}
converter-output-folder = Output folder
converter-open = Open
converter-filter-name = Audio, video and CUE files
converter-pick-files-title = Select audio or video files, or a CUE sheet
converter-pick-folder-title = Select the output folder

# --- settings-.ftl ---
# Settings page: sections and navigation
settings-sec-sources = Sources
settings-sec-online = Online data
settings-sec-tagger = Tag editor keys
settings-sec-startup = Startup page
settings-sec-desktop = Background playback
settings-no-match-title = No settings match
settings-no-match-hint = Try another word, like playback or scrobble.
settings-badge-network = NETWORK

# Settings page: notices and toasts
settings-import-failed = Import from { $service } failed: { $reason }
settings-import-cancelled = Import from { $service } cancelled
settings-import-done =
    { $count ->
        [one] Imported 1 play from { $service }
       *[other] Imported { $count } plays from { $service }
    }
settings-loved-synced = Synced loved tracks: { $added } added here, { $pushed } sent to Last.fm
settings-pick-folder-title = Select Music Directory
settings-add-folder-failed = Could not add the folder: { $reason }
settings-folders-need-access-subject = Music folders
settings-folders-need-access-suffix = need your permission to be read again
settings-folders-repick-notice = Pick your music folder again in Settings › Library to play your files

# Settings page: library
settings-library-scanning = Scanning…
settings-library-note = Music folders are scanned into the local database
settings-folders-empty = No music folders yet. Add one to build your library.
settings-folder-remove-tip = Remove folder
settings-folder-rescan = Rescan
settings-folder-allow = Allow access
settings-folder-pick-again = Pick again
settings-folder-tracks =
    { $count ->
        [one] 1 track
       *[other] { $count } tracks
    }
settings-folder-tracks-allow =
    { $count ->
        [one] 1 track · allow access to play it
       *[other] { $count } tracks · allow access to play them
    }
settings-folder-tracks-repick =
    { $count ->
        [one] 1 track · pick the folder again to play it
       *[other] { $count } tracks · pick the folder again to play them
    }
settings-folder-unavailable = Not available · skipped while offline, not removed
settings-folder-add = Add folder
settings-folder-path-hint = …or type a path
settings-folder-add-path = Add
settings-watch-title = Watch for changes
settings-watch-desc = Rescan automatically when files in your music folders change.
settings-artist-tags-heading = Artist tags
settings-artist-tags-note = How collaboration credits are grouped
settings-split-title = Split collaboration credits
settings-split-desc = “A feat. B” and “A; B” appear as separate artists. Your tags are never changed.
settings-delimiters-title = Credit delimiters
settings-delimiters-desc = Separators credits are split on (when splitting is on), divided by |.
settings-reset = Reset
settings-compilations-heading = Compilations and playlists
settings-show-va-title = Show “Various Artists” in Artists
settings-show-va-desc = Compilations stay reachable from Albums either way.
settings-m3u-relative-title = Relative paths in exported M3U playlists
settings-m3u-relative-desc = Write track paths relative to the playlist file instead of absolute.

# Settings page: sources
settings-sources-note = MPD and Subsonic / OpenSubsonic servers
settings-sources-empty = No servers yet. Your local library is always available.
settings-add-mpd = Add MPD server
settings-add-subsonic = Add Subsonic server
settings-card-testing = Testing…
settings-card-active = Active source
settings-card-untested = Not tested
settings-card-testing-btn = Testing
settings-retry = Retry
settings-test = Test
settings-edit = Edit
settings-editor-mpd-title = MPD server
settings-editor-subsonic-title = Subsonic / OpenSubsonic server
settings-field-name = Name
settings-field-host = Host
settings-field-port = Port
settings-field-password-optional = Password (optional)
settings-field-server-url = Server URL
settings-field-username = Username
settings-field-password-or-key = Password or API key
settings-field-mpd-name-hint = Living room MPD
settings-web-tls-note = The browser validates TLS certificates itself, so self-signed servers only work once your browser trusts their certificate. The server must also allow requests from this site (CORS) and use https when this page is served over https.
settings-web-password-note = The password is stored unencrypted in this browser's localStorage for this site; the web has no system keyring.
settings-accept-invalid-certs = Accept invalid TLS certificates (self-signed servers)
settings-testing-connection = Testing connection…
settings-test-connection = Test connection
settings-cancel = Cancel
settings-err-name-host = Give the server a name and a host.
settings-err-port = The port must be a number between 1 and 65535.
settings-err-name-url = Give the server a name and a URL.
settings-err-url-scheme = The URL must start with http:// or https://.
settings-err-username = Enter the username.
settings-server-saved = Server saved

# Settings page: playback
settings-off = Off
settings-seconds = { $value } s
settings-gapless-title = Gapless playback
settings-gapless-desc = Prepare the next track so consecutive tracks follow without a pause.
settings-stop-after-title = Stop after current track
settings-stop-after-desc = Playback stops at the end of every track instead of continuing with the queue.
settings-crossfade-title = Crossfade
settings-crossfade-desc = Overlap the end of one track with the next. Album tracks stay gapless.
settings-fade-title = Fade on play, pause and stop
settings-fade-desc = A short volume ramp instead of a hard cut.
settings-replaygain-desc = Album mode keeps the dynamics between tracks of one album.
settings-autoplay-title = When the queue ends
settings-autoplay-desc = Keep playing random albums or music similar to what just played.
settings-party-title = Party mode
settings-party-desc = Endless random queue from the genres below (the whole library when none).
settings-party-no-more-genres = No more genres in the library

# Settings page: equalizer
settings-eq-note = Ten bands, applied to local playback
settings-eq-enabled-desc = Bypass it without losing your settings.
settings-eq-preset-desc = Built-in shapes and your saved ones.
settings-eq-modified = { $name } (modified)
settings-eq-custom = Custom
settings-eq-preamp-desc = Lower it when boosted bands start to clip.
settings-eq-reset-flat = Reset to flat
settings-eq-save-preset = Save preset
settings-eq-delete-preset = Delete preset
settings-eq-name-hint = Save current settings as…
settings-eq-save-as = Save as
settings-autoeq-title = AutoEQ headphone profiles
settings-autoeq-desc = Correction curves for your headphones, downloaded from the AutoEQ project.
settings-autoeq-loading-list = Loading list…
settings-autoeq-load-list = Load profile list
settings-autoeq-search-hint = Search headphones (2+ letters)
settings-autoeq-no-match = No profile matches
settings-autoeq-keep-typing = Keep typing to narrow the list.
settings-autoeq-applying = Applying AutoEQ profile

# Settings page: scrobbling
settings-scrobbling-note = A play counts after 50% or 4 minutes
settings-scrobble-streams-title = Scrobble radio and podcasts
settings-scrobble-streams-desc = Streams are off by default.
settings-love-title = Love tracks on Last.fm
settings-love-desc = Favoriting a track loves it there too.
settings-sync-loved-title = Sync loved tracks
settings-sync-loved-desc = Loved tracks become favorites here, favorites become loved there.
settings-sync-now = Sync now
settings-import-on-connect-title = Import history after connecting
settings-import-on-connect-desc = Reads the service's listening history once a service is connected.
settings-svc-connected = Connected as { $user }
settings-svc-connected-paused = Connected as { $user } · paused
settings-svc-connected-queued =
    { $count ->
        [one] Connected as { $user } · 1 waiting to send
       *[other] Connected as { $user } · { $count } waiting to send
    }
settings-svc-not-connected = Not connected
settings-import-history = Import history
settings-complete-connection = Complete connection
settings-checking = Checking…
settings-lb-token-hint = Paste your ListenBrainz user token
settings-lb-token-help = Find the token on listenbrainz.org under Settings → User token.
settings-lastfm-keys-note = Last.fm needs your own API key and shared secret (last.fm/api/account/create).
settings-api-key = API key
settings-shared-secret = Shared secret
settings-save-keys = Save keys
settings-import-progress-title = Importing listening history from { $service }
settings-import-read = { $fetched } read
settings-import-summary = { $matched } matched in your library · { $imported } added to the history. Matched plays power Home’s “rediscover” shelf.

# Settings page: online data
settings-online-note = Off by default. Nothing leaves your machine until you turn it on.
settings-bio-title = Artist biographies
settings-bio-desc = Wikipedia text for Local and MPD libraries. Subsonic servers use their own.
settings-photos-title = Artist photos
settings-photos-desc = Deezer images for Local and MPD libraries. Subsonic servers use their own.
settings-radio-icons-title = Radio station icons
settings-radio-icons-desc = Downloaded from each station's own server.
settings-because-title = “Because you listened to…” on Home
settings-because-desc = Similar-artist shelves from your connected scrobbling service.

# Settings page: tag editor keys
settings-acoustid-app-title = AcoustID application key
settings-acoustid-app-desc = Required for fingerprint lookups. Not a secret.
settings-acoustid-app-hint = Application key
settings-acoustid-user-title = AcoustID user key
settings-acoustid-user-desc-stored = Your own key is stored in the system keyring. Enter a new one to replace it, or save an empty field to delete it.
settings-acoustid-user-desc-optional = Optional. Lets you submit fingerprints. Stored in the system keyring, never in the settings file.
settings-acoustid-user-desc-checking = Checking the system keyring…
settings-acoustid-user-hint = User key
settings-caa-title = Prefer Cover Art Archive covers
settings-caa-desc = When applying a MusicBrainz release in the tag editor.

# Settings page: appearance
settings-theme-title = Theme
settings-theme-desc = System follows your desktop's dark or light setting.
settings-theme-system = System
settings-theme-dark = Dark
settings-theme-light = Light
settings-tint-title = Tint the interface with cover colour
settings-tint-desc = The accent follows the dominant colour of the playing album.
settings-reduce-motion-title = Reduce motion
settings-reduce-motion-desc = Turns off fades, slides and other interface animations.
settings-ui-scale-title = Interface size
settings-ui-scale-desc = Scales the whole window. Ctrl + = / Ctrl + − / Ctrl + 0 change it anywhere.

# Settings page: startup
settings-startup-last-visited = Last visited page
settings-display-name-title = Display name
settings-display-name-desc = Greets you on the Home page (“Good evening, Sam”). Leave empty for no name.
settings-display-name-hint = Your name
settings-startup-page-title = Startup page
settings-startup-page-desc = Opened when Aulos launches.
settings-startup-last-source = Last used source
settings-startup-source-title = Startup source
settings-startup-source-desc = The library Aulos opens with.
settings-autoplay-launch-title = Start playing on launch
settings-autoplay-launch-desc = Plays your library as soon as it has loaded.

# Settings page: desktop
settings-desktop-note = Desktop integration
settings-background-title = Keep playing when the window is closed
settings-background-desc = The window minimizes instead of quitting while music plays; MPRIS and media keys keep working.
settings-notify-title = Notify on track change
settings-notify-desc = A desktop notification with the cover while the window isn't focused.
settings-inhibit-title = Prevent suspend while playing
settings-inhibit-desc = Holds a desktop inhibit lock during playback.
settings-close-tray-title = Close to the system tray
settings-close-tray-desc = Closing the window hides Aulos in the tray instead of quitting; quit from the tray menu. Needs a desktop with tray support.
settings-minimize-tray-title = Minimize to the system tray
settings-minimize-tray-desc = Minimizing the window hides it in the tray; click the tray icon to bring it back. Wayland does not tell apps when a window is minimized, so there it only works with close to tray.

# Settings page: experimental
settings-experimental-note = Features still being tested
settings-converter-title = File converter
settings-converter-desc = Shows the Converter page: transcode and rip local files.

# Settings page: about
settings-version = Version { $version }
settings-about-blurb = A music player for your own library. Licensed under GPL-3.0-only.
settings-project-page = Project page
settings-author = Author: Gianluca Boiano
settings-author-page = Author on GitHub

# Settings page: extra search terms (separate terms with |)
settings-kw-library = Watch for changes | auto rescan | file watcher
settings-kw-sources = Sources | MPD | Subsonic | OpenSubsonic | Navidrome | server | provider | connection
settings-kw-playback = Gapless | Stop after current track | stop after every track
settings-kw-equalizer = Equalizer | preamp | preset | AutoEQ | bands | headphone
settings-kw-scrobbling = Scrobbling | Last.fm | ListenBrainz | Libre.fm | scrobble | listening history | import | love
settings-kw-online = Artist biographies | Artist photos | Radio station icons | Online data | network | suggestions | Because you listened to
settings-kw-tagger = AcoustID | Cover Art Archive
settings-kw-appearance = Theme | Dark | Light | System | Tint | cover colour | Reduce motion | animations
settings-kw-startup = Start playing on launch | auto-play | autoplay | Display name | greeting | username
settings-kw-desktop = Desktop | tray | window close
settings-kw-about = version | license | GPL

# --- shell.ftl ---
# app: toasts and notices
app-toast-undo = Undo
app-toast-allow = Allow
app-toast-queued =
    { $count ->
        [one] Added 1 track to the queue
       *[other] Added { $count } tracks to the queue
    }
app-toast-open-files-failed = Could not open those files
app-toast-playback-failed = Playback failed: { $reason }
app-toast-action-failed = Action failed: { $reason }
app-toast-playlists-imported =
    { $count ->
        [one] Imported 1 playlist file
       *[other] Imported { $count } playlist files
    }
app-toast-playlist-import-failed = Playlist import failed: { $reason }
app-toast-converted =
    { $count ->
        [one] Converted 1 file
       *[other] Converted { $count } files
    }
app-toast-converted-failed =
    { $count ->
        [one] Converted 1 file, { $failed } failed
       *[other] Converted { $count } files, { $failed } failed
    }
app-toast-subscribed = Subscribed to { $name }
app-toast-subscribe-failed = Could not subscribe: { $reason }
app-toast-unsubscribe-failed = Could not unsubscribe: { $reason }
app-toast-download-failed = Download failed: { $reason }
app-toast-station-saved = Saved { $name }
app-toast-station-already-saved = { $name } is already saved
app-toast-station-save-failed = Could not save the station: { $reason }
app-toast-station-play-failed = Could not play the station: { $reason }
app-toast-visualizer-needs-gpu = The visualizer needs the GPU renderer
app-web-start-failed = Aulos could not start: this browser needs WebGL2 or WebGPU.

# now bar
nowbar-nothing-playing = Nothing playing
nowbar-pick-something = Pick an album or a song
nowbar-favorite-remove = Remove from favorites
nowbar-favorite-add = Add to favorites
nowbar-shuffle = Shuffle
nowbar-previous = Previous
nowbar-pause = Pause
nowbar-play = Play
nowbar-stop = Stop
nowbar-next = Next
nowbar-repeat = Repeat
nowbar-live = LIVE
nowbar-volume = Volume
nowbar-mute = Mute
nowbar-unmute = Unmute
nowbar-expand = Expand
nowbar-visualizer = Visualizer
nowbar-properties = Properties
nowbar-lyrics = Lyrics
nowbar-queue = Queue

# sidebar
tabbar-more = More
sidebar-home = Home
sidebar-albums = Albums
sidebar-artists = Artists
sidebar-songs = Songs
sidebar-genres = Genres
sidebar-folders = Folders
sidebar-playlists = Playlists
sidebar-podcasts = Podcasts
sidebar-radio = Radio
sidebar-settings = Settings
sidebar-tag-editor = Tag editor
sidebar-converter = Converter
sidebar-lab-badge = LAB
sidebar-section-library = LIBRARY
sidebar-section-listen = LISTEN
sidebar-section-playlists = PLAYLISTS
sidebar-collapse = Collapse sidebar
sidebar-expand = Expand sidebar
sidebar-new-playlist = New playlist

# visualizer
viz-start-failed = The visualizer could not start: { $reason }
viz-starting = Starting the visualizer…
viz-presets-folder = the presets folder
viz-no-presets = No MilkDrop presets found. Put .milk / .milk2 files in { $dir }
viz-close = Close (Esc)
viz-preset-locked-tip = Preset locked (L)
viz-lock-preset-tip = Lock the preset (L)
viz-preset-locked = Preset locked
viz-beat = Beat
viz-previous-preset = Previous preset (←)
viz-random-preset = Random preset
viz-next-preset = Next preset (→)
viz-preset-browser = Preset browser (P)
viz-leave-fullscreen = Leave fullscreen (F)
viz-fullscreen = Fullscreen (F)
viz-search-presets =
    { $count ->
        [one] Search 1 preset
       *[other] Search { $count } presets
    }
viz-no-match = No preset matches

# overlays
overlays-connect-title = Can't reach { $provider }
overlays-connect-attempts =
    { $count ->
        [one] 1 attempt failed
       *[other] { $count } attempts failed
    }
overlays-retry = Retry
overlays-edit-server = Edit server
overlays-local-only = Show local only
overlays-auth-title = Sign in to { $name }
overlays-auth-rejected = The server rejected these credentials.
overlays-username = Username
overlays-password = Password
overlays-sign-in = Sign in
overlays-cancel = Cancel

# queue
queue-empty-meta = Empty
queue-title = Queue
queue-save-as-playlist = Save as playlist
queue-empty-title = Queue is empty
queue-empty-hint = Play an album or add tracks to see them here.
queue-group-now-playing = NOW PLAYING
queue-nothing-next-title = Nothing up next
queue-nothing-next-hint = Add tracks with Play next or Add to queue.
queue-group-playing-next = PLAYING NEXT
queue-group-added = ADDED TO QUEUE
queue-group-from-album = FROM { $album } · { $count } LEFT
queue-group-up-next = UP NEXT
queue-save-title = Save queue as playlist
queue-save-name = Name
queue-save-hint = Playlist name
queue-save-submit = Save

# toolbar
toolbar-back = Back
toolbar-forward = Forward
toolbar-search-short = Search
toolbar-search-long = Search albums, artists, songs
toolbar-all-sources = All sources

# mini player
mini-nothing-playing = Nothing playing
mini-unpin = Unpin
mini-keep-on-top = Keep on top
mini-expand = Expand

# widgets
widgets-column-title = TITLE
widgets-column-artist = ARTIST
widgets-column-album = ALBUM
widgets-column-quality = QUALITY
widgets-column-rating = RATING

# --- tagger-.ftl ---
# Tag editor view (crates/aulos/src/views/tagger.rs)

## notices
tagger-ui-add-files-failed = Couldn't add the files: { $error }
tagger-ui-add-image-failed = Couldn't add the image: { $error }
tagger-ui-scan-unrecognised = AcoustID didn't recognise enough of these files to pick a release.
tagger-ui-submitted =
    { $count ->
        [one] 1 fingerprint submitted.
       *[other] { $count } fingerprints submitted.
    }
tagger-ui-submit-partial = { $ok } submitted, { $failed } failed.
tagger-ui-open-mb-failed = Couldn't open MusicBrainz: { $error }
tagger-ui-open-page-failed = Couldn't open the page: { $error }
tagger-ui-no-files-lookup = There are no files to look up.
tagger-ui-no-files-release = There are no files to add as a release.
tagger-ui-key-missing-notice = Set an AcoustID application key in Settings to identify files by their audio.
tagger-ui-no-files-scan = There are no files to scan.
tagger-ui-no-recording-ids = None of these files has a MusicBrainz recording ID to submit.
tagger-ui-no-changes = There are no changes to save.
tagger-ui-saved =
    { $count ->
        [one] Saved 1 file.
       *[other] Saved { $count } files.
    }
tagger-ui-save-partial = { $saved } saved, { $failed } failed.

## file dialogs
tagger-ui-filter-audio = Audio files
tagger-ui-pick-files = Add files to the tag editor
tagger-ui-pick-folder = Add a folder to the tag editor
tagger-ui-filter-images = Images
tagger-ui-pick-image = Choose an image

## header
tagger-ui-scan-button = AcoustID scan
tagger-ui-scan-disabled-hover = Set an AcoustID application key in Settings to scan.
tagger-ui-scan-progress = Fingerprinting { $done } / { $total }
tagger-ui-scan-loading-release = Loading the matching release…
tagger-ui-save-files =
    { $count ->
        [one] Save 1 file
       *[other] Save { $count } files
    }
tagger-ui-submit-key-hover = Save your AcoustID API key in Settings to submit fingerprints.

## banners
tagger-ui-discard-banner =
    { $count ->
        [one] Discard unsaved edits? 1 file will lose its changes.
       *[other] Discard unsaved edits? { $count } files will lose their changes.
    }
tagger-ui-album-artist-mismatch =
    { $count ->
        [one] 1 file has a different Album artist than the rest of the selection.
       *[other] { $count } files have a different Album artist than the rest of the selection.
    }
tagger-ui-show-files = Show files
tagger-ui-key-missing-banner = AcoustID needs an application key. Scanning and submitting fingerprints requires a free AcoustID application key. Add it in Settings.
tagger-ui-user-key-missing-banner = Submitting needs your AcoustID API key. Add your personal AcoustID API key in Settings to submit fingerprints.
tagger-ui-scan-identified =
    { $count ->
        [one] AcoustID identified 1 file. Loading the matching release…
       *[other] AcoustID identified { $count } files. Loading the matching release…
    }

## files panel
tagger-ui-filter-placeholder = Filter files
tagger-ui-remove-selected-tooltip = Remove the selected files from the editor
tagger-ui-clear-all-tooltip = Remove every file from the editor
tagger-ui-add-files-button = Add files
tagger-ui-add-folder-button = Add folder
tagger-ui-no-filter-match = No file matches the filter.
tagger-ui-unknown-album = Unknown album
tagger-ui-no-match = no match
tagger-ui-read-only-reason = Read-only: { $reason }

## metadata
tagger-ui-meta-nothing-title = Nothing to edit
tagger-ui-meta-nothing-hint = Add files to see and edit their tags.
tagger-ui-col-field = FIELD
tagger-ui-col-disk = ON DISK
tagger-ui-col-new = NEW VALUE
tagger-ui-edited = edited
tagger-ui-readonly-label = read-only
tagger-ui-no-files-selected = No files selected
tagger-ui-add-field-toggle = Add field

## cover art
tagger-ui-cover-caps = COVER ART
tagger-ui-cover-select = Select one or more files to see their pictures.
tagger-ui-cover-add-picture = Add picture
tagger-ui-cover-fetch = Fetch cover
tagger-ui-cover-fetch-hint = Look up a release first to fetch its front cover from the Cover Art Archive.

## lookup
tagger-ui-lookup-caps-musicbrainz = MUSICBRAINZ RELEASES
tagger-ui-lookup-caps-acoustid = ACOUSTID RESULT
tagger-ui-lookup-cover-off = The front cover is not fetched automatically (see Settings). Use “Fetch cover” below.

## acoustid card
tagger-ui-identified = Identified { $identified } of { $total } files.
tagger-ui-and-more = and { $count } more
tagger-ui-recording-link = Recording

# Text field bar (Android)
edit-paste = Paste
edit-select-all = Select all
edit-copy = Copy
edit-cut = Cut

# Browser folder access (web_fs)
web-folder-not-granted = Folder access for “{ $root }” was not granted — allow it in Settings › Library
web-folder-not-available = The files of “{ $root }” are not available — pick the folder again in Settings › Library
web-folder-empty = The folder has no files
web-file-not-in-root = { $path } is not a file of a music folder
web-file-not-in-folder = “{ $file }” is not in “{ $root }” any more
web-file-not-read = “{ $file }” could not be read: { $error }
web-file-read-failed = The file could not be read: { $error }

# System tray icon menu
tray-show = Show Aulos
tray-hide = Hide Aulos
tray-play-pause = Play / Pause
tray-next = Next
tray-previous = Previous
tray-quit = Quit
