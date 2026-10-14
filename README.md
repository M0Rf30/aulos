<div align="center">

<img src="resources/icons/hicolor/scalable/apps/io.github.m0rf30.Aulos.svg" width="120" height="120" alt="Aulos">

# Aulos

**A music player for Linux**

*Aulos: it really pipes the satyr's ass.*

[![CI](https://github.com/M0Rf30/aulos/actions/workflows/ci.yml/badge.svg)](https://github.com/M0Rf30/aulos/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/M0Rf30/aulos)](https://github.com/M0Rf30/aulos/releases/latest)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-Support%20Me-ff5e5b?logo=ko-fi&logoColor=white)](https://ko-fi.com/W7W61U8IUL)

</div>

---

Aulos is a music player for Linux, written in Rust. Its backend (`aulos-core`) plays your local library, drives MPD servers, streams from Subsonic/OpenSubsonic servers, and also handles internet radio, podcasts and tag editing. The user interface is a native [egui](https://github.com/emilk/egui) app.

> **Status: early prototype.** The backend is feature-rich, but the egui front end is new and exposes only part of it: it loads and browses the local library, searches, and plays through the queue. Much of what is described below is implemented in `aulos-core` and is not reachable from the UI yet.

The name comes from the *aulos*, the double pipe of ancient Greece.

## Features

Everything below describes what the backend implements. The egui front end does not expose all of it yet (see the status note above).

### Music sources

| | Local files | MPD | Subsonic / OpenSubsonic |
|---|:-:|:-:|:-:|
| Browse albums, artists, songs, genres | ✓ | ✓ | ✓ |
| Search | ✓ | ✓ | ✓ |
| Playlists (create, rename, delete, add) | ✓ | ✓ | ✓ |
| Favorites and 1–5 star ratings | ✓ | ✓ (via MPD stickers) | ✓ |
| Scrobbling | | | ✓ |
| Server-side transcoding | | | ✓ (max bitrate and format) |

- **Local library**: your music folder (`~/Music` by default) is scanned into an SQLite database at startup. A file watcher then keeps it up to date as files are added, changed or removed.
- **MPD**: Aulos works as a remote control. Playback happens on the MPD server, and shuffle, repeat, crossfade and ReplayGain settings are sent to it.
- **Subsonic**: streams over HTTP with seeking. It also works with self-signed certificates, which you have to allow explicitly.
- **Passwords** for MPD and Subsonic are kept in the system keyring: the kernel keyutils first, falling back to the Secret Service (GNOME Keyring, KWallet).

### Playback engine

The engine uses [Symphonia](https://github.com/pdeljanov/Symphonia) (a DSD-capable fork) to decode and [cpal](https://github.com/RustAudio/cpal) for output.

- **Formats**: FLAC, MP3, Ogg Vorbis, Opus (including multichannel family 3), AAC/M4A (including AAC-LD/ELD and raw ADIF), ALAC, WAV, AIFF, APE, WavPack (including hybrid lossless with a `.wvc` correction file), Musepack, CAF and DSF/DFF, plus the audio in MP4, WebM, MKA, FLV, MPEG-TS and MPEG-PS files. Seeking is sample-accurate and chained Ogg streams report their full duration.
- **DSD**: sent bit-exact as DoP (DSD over PCM) to DACs that support it, otherwise converted to PCM.
- **Gapless** playback and **crossfade** with an adjustable length.
- **Fade in/out** on play, pause, stop and skip, with an adjustable length (off by default). Not applied to DSD played as DoP, which stays bit-exact.
- **Stop after this track**, **party mode** (endless random playback, optionally limited to chosen genres) and **auto-play** when the queue ends: random albums, or albums by the same artist, genre and era from your library.
- **ReplayGain**: Off, Track, Album or Auto mode, read from file tags.
- **10-band equalizer** with a preamp, built-in and user presets, and headphone correction profiles from [AutoEq](https://github.com/jaakkopasanen/AutoEq).
- Automatic **resampling** when the output device doesn't support a file's sample rate.

### Library and browsing

- **Pages**: Albums, Artists, Songs, Genres, Folders, Playlists and Smart Playlists. Albums, Artists and Genres can be shown as a grid or a list.
- **Smart playlists** built from rules on fields such as title, artist, album, genre, year, rating, favorite, duration, bitrate and sample rate. Rules can require all or any to match, and results can be sorted, randomised or ordered by date added.
- **Quality badges**: Lossy, CD, Hi-Res or DSD.
- **Cover art** from embedded pictures or image files in the album folder (`cover`, `folder`, `front` and similar). The colour of the current cover tints the interface.
- **Artist images and biographies** from Deezer and Wikipedia. This is **opt-in** and turned off by default.
- **Lyrics** from tags or `.lrc` files next to the track. Synced lyrics scroll with playback. Online lookup from [LRCLIB](https://lrclib.net) or your Subsonic server is on demand.

### Radio and podcasts

- **Internet radio**: search the [radio-browser.info](https://www.radio-browser.info) directory by name, tag or country, or browse the most-clicked and most-voted stations. The title of the song currently playing is read from the stream. PLS/M3U links are supported, and dropped streams reconnect automatically.
- **Podcasts**: find shows through the iTunes directory or subscribe to any RSS/Atom feed. Episodes can be streamed or downloaded for offline listening.

### Desktop integration

- **MPRIS**: works with media keys and desktop media widgets, including cover art and seeking.
- **Notifications** with cover art on track change, only while the window isn't focused.
- **Sleep inhibit** while music plays, through the desktop portal (falling back to systemd-logind).

### Visualizer

The [lactea](https://github.com/M0Rf30/lactea) visualizer (Milkdrop-style presets) reacts to what is actually playing: the engine's own output for local playback, and MPD's audio output captured through PipeWire. It has a preset browser and search, preset locking and beat sensitivity. Presets (`.milk`, and MilkDrop 3 `.milk2` double presets) are loaded from `~/.local/share/lactea/presets`, `/usr/share/lactea/presets`, `/usr/share/projectM/presets`, `/usr/local/share/projectM/presets` and `~/.local/share/projectM/presets`; user textures are read from the same directories and their sibling `textures` folders.

### Tag editor

A Picard-style editor for the tags of local files, opened from the **Tag Editor** page or from the right-click menu of a track or album ("Edit tags…").

- **Editing**: files are grouped into albums, and a metadata table shows each field on disk next to the new value. Selecting several files edits them together, with "(different values)" where they disagree. Multi-valued fields (genre, artists…) are written as `Rock; Electronic`. Fields can be reverted, deleted or added, and cover art can be replaced, retyped, removed or added.
- **MusicBrainz**: look up the release of an album, pick it from the search results and apply it. Files are matched to tracks by recording id, then by title, number and duration. Fields MusicBrainz doesn't provide (comment, lyrics, ReplayGain, custom fields) are kept, and the front cover can come from the Cover Art Archive. An album can also be sent to MusicBrainz as a new release.
- **AcoustID**: identify files by their audio fingerprint, and submit fingerprints for files that have a MusicBrainz recording id. This needs an AcoustID application key (and, to submit, your own API key) in Settings.
- **Saving**: writes MP3, FLAC, Ogg (Vorbis, Opus, FLAC, Speex), MP4/M4A, WAV, AIFF, DSF, WavPack, APE and Musepack tags. The library and its covers refresh as soon as the files are written.

### File converter (experimental)

The converter is off by default; turn it on in Settings.

- **Outputs**: converts to FLAC, WAV or AIFF without any external tools, and to MP3, AAC, Opus, Ogg Vorbis or ALAC through the system `ffmpeg`.
- **Processing**: all tags and the cover are copied to every output format, the sample rate can be changed, and CUE sheets can be split into per-track files.

## Installation

### Arch Linux (AUR)

```bash
paru -S aulos-bin   # prebuilt release
paru -S aulos-git   # build from the latest commit
```

### Prebuilt binaries

Each [GitHub release](https://github.com/M0Rf30/aulos/releases) ships `aulos-<version>-x86_64-unknown-linux-gnu.tar.gz` and `aulos-<version>-aarch64-unknown-linux-gnu.tar.gz`, each with a `.sha256` checksum. Release binaries include the visualizer.

```bash
sha256sum -c aulos-<version>-<target>.tar.gz.sha256
tar xzf aulos-<version>-<target>.tar.gz
cd aulos-<version>-<target>
sudo just install
```

The archive contains the `aulos` binary, the `justfile` and `resources/` (desktop entry, AppStream metainfo, icon). Without [just](https://github.com/casey/just), copy those into `/usr/bin` and `/usr/share` by hand.

**Runtime requirements:**
- A Wayland or X11 desktop, with a Vulkan or GL driver.
- ALSA (`alsa-lib`).
- `libpipewire-0.3` and a Vulkan or GL driver, for the visualizer.
- Optional: `ffmpeg`, for lossy formats in the converter.
- Optional: MilkDrop (`.milk` / `.milk2`) presets.

### From source

Aulos runs on Linux only. You need a recent stable Rust toolchain (edition 2024), `just`, and the development packages for xkbcommon, Wayland and ALSA. On Debian or Ubuntu:

```bash
sudo apt install pkg-config libxkbcommon-dev libwayland-dev libasound2-dev
git clone https://github.com/M0Rf30/aulos
cd aulos
just build-release
sudo just install
```

To build with the visualizer, as the release binaries are, also install `libpipewire-0.3-dev libspa-0.2-dev`, then run:

```bash
just build-release --features visualizer
```

### Code layout

The repository is a Cargo workspace with two crates:

- `crates/aulos-core` is the backend: library and tag I/O, the playback engine, MPD/Subsonic providers, online services (scrobbling, MusicBrainz/AcoustID, podcasts, radio), the tag editor, settings, MPRIS and desktop integration. Its `Session` type holds the application state and workflows (play, queue, rescan, switch server, subscribe…); background work comes back as events the front end feeds into it. It has no UI toolkit dependency, and CI fails if it gains one.
- `crates/aulos` is the egui/eframe front end (the `aulos` binary). It maps user input to `Session` calls and draws the results.

`cargo build`, `cargo test` and `cargo clippy` at the repository root cover both crates.

## Usage

Start Aulos from your application menu, or from a terminal with `aulos`. The music folder is read from the settings file (see below); the egui front end has no settings page yet.

On the very first launch Aulos greets you with its intro jingle — a 4-second, 128 kbps MP3 in honour of Winamp's llama — and never again after that.

### Where data lives

| What | Path |
|---|---|
| Library database, artist info, podcast downloads | `~/.local/share/aulos/` |
| AutoEq profiles, MPRIS cover art | `~/.cache/aulos/` |
| Equalizer presets | `~/.config/aulos/eq_presets/` |
| Settings | `~/.config/aulos/settings.ron` |

## Building blocks

[egui](https://github.com/emilk/egui) (UI) · [Symphonia](https://github.com/M0Rf30/Symphonia) and [cpal](https://github.com/RustAudio/cpal) (audio) · [rubato](https://github.com/HEnquist/rubato) (resampling) · [rusqlite](https://github.com/rusqlite/rusqlite) (library) · [mpd_client](https://github.com/elomatreb/mpd_client) (MPD) · [opensubsonic](https://github.com/M0Rf30/opensubsonic-rs) (Subsonic) · [mpris-server](https://github.com/SeaDve/mpris-server) and [zbus](https://github.com/dbus2/zbus) (D-Bus) · [lactea](https://github.com/M0Rf30/lactea) (visualizer) · [tokio](https://tokio.rs) (async)

## Support

If you enjoy Aulos and want to support its development:

<a href='https://ko-fi.com/W7W61U8IUL' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi6.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

## License

Aulos is free software released under the [GNU General Public License v3.0](LICENSE).
