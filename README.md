<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="resources/logo/aulos-wordmark-dark.svg">
  <img src="resources/logo/aulos-wordmark-light.svg" width="260" alt="Aulos">
</picture>

**A modern music player**

*Aulos: it really pipes the satyr's ass.*

[![CI](https://github.com/M0Rf30/aulos/actions/workflows/ci.yml/badge.svg)](https://github.com/M0Rf30/aulos/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/M0Rf30/aulos)](https://github.com/M0Rf30/aulos/releases/latest)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![Try it in your browser](https://img.shields.io/badge/Try%20it-in%20your%20browser-E07A5F?logo=webassembly&logoColor=white)](https://m0rf30.github.io/aulos/)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-Support%20Me-ff5e5b?logo=ko-fi&logoColor=white)](https://ko-fi.com/W7W61U8IUL)

</div>

---

Aulos is a modern music player for Linux, Windows and macOS, written in Rust. It plays your local library, drives MPD servers and streams from Subsonic/OpenSubsonic servers, with internet radio, podcasts, synced lyrics, a 10-band equalizer, a MilkDrop visualizer and a Picard-style tag editor. The interface is a native [egui](https://github.com/emilk/egui) app: dark by default, tinted by the colour of the cover that is playing.

The name comes from the *aulos*, the double pipe of ancient Greece; the logo's two slanted reeds are a nod to it.

## Try it in your browser

**[m0rf30.github.io/aulos](https://m0rf30.github.io/aulos/)**: the same egui interface, compiled to WebAssembly. It runs entirely client-side (WebGPU, with a WebGL2 fallback) and nothing is sent to a server.

The browser build is a preview. Pick a music folder (Chromium keeps it across reloads and asks permission again; Firefox and Safari need the folder picked again after a reload) or connect a Subsonic/OpenSubsonic server, and play it with Web Audio and media keys. Podcast and radio directories, feeds and station playlists work over `fetch` (a feed must allow CORS), but live radio audio and MPD are not available yet.

| Feature | Desktop | Browser |
|---|---|---|
| Local library | ✅ scans your folders | ✅ pick folders with the File System Access API (`<input webkitdirectory>` fallback), kept in IndexedDB |
| Playback | ✅ cpal, gapless, crossfade, DSD | ✅ Web Audio, gapless, EQ; no crossfade or DSD |
| Subsonic / OpenSubsonic | ✅ | ✅ over `fetch`; the server must allow CORS and be served over HTTPS |
| MPD | ✅ direct TCP | 🚧 through a WebSocket bridge such as websockify |
| Podcasts and internet radio | ✅ | 🚧 search, subscribe and podcast playback over `fetch` (the feed and audio hosts must allow CORS and be served over HTTPS); live radio audio is not played yet |
| Tag editor | ✅ | ❌ |
| File converter | ✅ | ❌ |
| Visualizer | ✅ | ❌ |
| MPRIS, media keys, notifications | ✅ | ✅ Media Session API; notifications ❌ |
| Credential storage | ✅ system keyring | ✅ `localStorage` (not encrypted) |
| Settings | ✅ | ✅ `localStorage` |

✅ available, 🚧 planned, ❌ not available.

## Features

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
- **Passwords** for MPD and Subsonic are kept in the system keyring: on Linux the kernel keyutils first, falling back to the Secret Service (GNOME Keyring, KWallet); on Windows the Credential Manager; on macOS the Keychain.

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

### Android

A universal APK (arm64-v8a and x86_64, Android 8.0+) is attached to every release. Library scanning reads the shared `Music` folder (the app asks for the audio permission on first launch); Subsonic/OpenSubsonic servers work as on the desktop. MPD, the tag editor's file dialogs and the converter's pickers are desktop-only for now.

[<img src="resources/badges/badge_obtainium.png" alt="Get it on Obtainium" height="48">](https://apps.obtainium.imranr.dev/redirect?r=obtainium://app/%7B%22id%22%3A%22io.github.m0rf30.aulos%22%2C%22url%22%3A%22https%3A%2F%2Fgithub.com%2FM0Rf30%2Faulos%22%2C%22author%22%3A%22M0Rf30%22%2C%22name%22%3A%22Aulos%22%7D)

Verification info:

- Package ID: `io.github.m0rf30.aulos`
- SHA-256 hash of the signing certificate:
  ```text
  8F:45:99:C9:AE:A2:26:37:47:7A:53:90:01:61:B4:1F:C6:2F:F4:1F:EC:69:69:87:B9:8A:0C:A2:1F:BD:BA:06
  ```
  (Check with `apksigner verify --print-certs aulos-<version>-android.apk`; the `.sha256` file next to the APK covers the file itself.)

### Languages

English, Italian, German, French, Spanish and Brazilian Portuguese. The interface follows the system language; change it in Settings → Appearance → Language. Translations live in `i18n/<tag>/aulos.ftl` (Fluent); fixes and new languages are welcome.

### Desktop integration

- **Media keys and now playing**: MPRIS on Linux, System Media Transport Controls on Windows, Now Playing on macOS, including cover art and seeking.
- **Notifications** with cover art on track change, only while the window isn't focused.
- **Sleep inhibit** while music plays: through the desktop portal on Linux (falling back to systemd-logind), natively on Windows and macOS.

### Visualizer

The [lactea](https://github.com/M0Rf30/lactea) visualizer (MilkDrop-style presets) renders on the GPU in a full-window view and reacts to what is actually playing: the engine's own output for local playback, and, on Linux, MPD's audio output captured through PipeWire (on Windows and macOS the MPD capture reports unavailable). A HUD fades in on mouse movement with preset navigation (previous, random, next), preset locking, beat sensitivity, a searchable preset browser and fullscreen. Presets (`.milk`, and MilkDrop 3 `.milk2` double presets) are loaded from `~/.local/share/aulos/presets`, `~/.local/share/lactea/presets`, `/usr/share/lactea/presets`, `/usr/share/projectM/presets`, `/usr/local/share/projectM/presets` and `~/.local/share/projectM/presets` on Linux, and `%APPDATA%\aulos\presets` (Windows) or `~/Library/Application Support/aulos/presets` and Homebrew's `/opt/homebrew/share/projectM/presets` (macOS); user textures are read from the same directories and their sibling `textures` folders.

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

## Platform support

Aulos runs on Linux, Windows 10+ and macOS 11+ and needs a GPU with Vulkan, DirectX 12 or Metal. Most features are the same everywhere; these differ:

| Feature | Linux | Windows | macOS |
|---|---|---|---|
| Media keys / now playing | MPRIS (D-Bus) | System Media Transport Controls | Now Playing |
| File dialogs | XDG desktop portal | Native | Native |
| Notifications | D-Bus | Native | Native |
| Sleep inhibit | Portal / systemd-logind | Native | Native |
| Password storage | keyutils / Secret Service | Credential Manager | Keychain |
| Visualizer for MPD audio | ✓ (PipeWire) | Not available | Not available |
| Visualizer for local playback | ✓ | ✓ | ✓ |
| `just install`, desktop entry, AppStream metainfo | ✓ | n/a | n/a |

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

Windows releases are a `.zip` (`aulos.exe`, `LICENSE`, `README.md`) and macOS releases a `.tar.gz` per architecture (`aarch64-apple-darwin`, `x86_64-apple-darwin`) with the `aulos` binary, `LICENSE` and `README.md`. Unzip or untar and run it. The macOS binary is not code-signed, so Gatekeeper may ask you to allow it.

### From source

You need a recent stable Rust toolchain (edition 2024).

#### Linux

You also need `just` and the development packages for xkbcommon, Wayland and ALSA. On Debian or Ubuntu:

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

#### Windows

Install [rustup](https://rustup.rs) with the MSVC toolchain (Visual Studio Build Tools, "Desktop development with C++"), then:

```powershell
git clone https://github.com/M0Rf30/aulos
cd aulos
cargo build --release --features visualizer
```

The binary is `target\release\aulos.exe`.

#### macOS

Install the Xcode command line tools and a Rust toolchain, then:

```bash
xcode-select --install
git clone https://github.com/M0Rf30/aulos
cd aulos
cargo build --release --features visualizer
```

The binary is `target/release/aulos`. To build an `.app` bundle, install [cargo-bundle](https://github.com/burtonageo/cargo-bundle) and run it from the repository root (the bundle metadata is in `crates/aulos/Cargo.toml`):

```bash
cargo install cargo-bundle
cargo bundle --release -p aulos
```

### Code layout

The repository is a Cargo workspace with two crates:

- `crates/aulos-core` is the backend: library and tag I/O, the playback engine, MPD/Subsonic providers, online services (scrobbling, MusicBrainz/AcoustID, podcasts, radio), the tag editor, settings, MPRIS and desktop integration. Its `Session` type holds the application state and workflows (play, queue, rescan, switch server, subscribe…); background work comes back as events the front end feeds into it. It has no UI toolkit dependency, and CI fails if it gains one.
- `crates/aulos` is the egui/eframe front end (the `aulos` binary). It maps user input to `Session` calls and draws the results.

`cargo build`, `cargo test` and `cargo clippy` at the repository root cover both crates.

## Usage

Start Aulos from your application menu (Start menu or Applications on Windows and macOS), or from a terminal with `aulos`. Add your music folders, MPD and Subsonic servers and everything else in **Settings**. The interface follows your display's scaling; **Settings → Appearance → Interface size** scales it further (75–200%).

On the very first launch Aulos greets you with its intro jingle — a 4-second, 128 kbps MP3 in honour of Winamp's llama — and never again after that.

### Keyboard shortcuts

| Key | Action |
|---|---|
| `Space` | Play / pause |
| `/` | Search |
| `1`–`8` | Jump to a page |
| `Alt+←` / `Alt+→` | Back / forward |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Bigger / smaller / reset interface size |
| `Esc` | Clear search, close sheets and the expanded view |

In the visualizer: `←` / `→` previous / next preset, `L` lock, `P` preset browser, `F` or double-click fullscreen, `Esc` close.

### Where data lives

| What | Linux | Windows | macOS |
|---|---|---|---|
| Config: settings (`settings.ron`), equalizer presets (`eq_presets/`) | `~/.config/aulos/` | `%APPDATA%\aulos\` | `~/Library/Application Support/aulos/` |
| Data: library database, artist info, podcast downloads | `~/.local/share/aulos/` | `%LOCALAPPDATA%\aulos\` | `~/Library/Application Support/aulos/` |
| Cache: AutoEq profiles, cover art for media controls | `~/.cache/aulos/` | `%LOCALAPPDATA%\aulos\cache\` | `~/Library/Caches/aulos/` |

On Windows the user MilkDrop preset folder is `%APPDATA%\aulos\presets`; on macOS it is `~/Library/Application Support/aulos/presets`.

## Building blocks

[egui](https://github.com/emilk/egui) (UI) · [Symphonia](https://github.com/M0Rf30/Symphonia) and [cpal](https://github.com/RustAudio/cpal) (audio) · [rubato](https://github.com/HEnquist/rubato) (resampling) · [rusqlite](https://github.com/rusqlite/rusqlite) (library) · [mpd_client](https://github.com/elomatreb/mpd_client) (MPD) · [opensubsonic](https://github.com/M0Rf30/opensubsonic-rs) (Subsonic) · [mpris-server](https://github.com/SeaDve/mpris-server) and [zbus](https://github.com/dbus2/zbus) (D-Bus), [souvlaki](https://github.com/Sinono3/souvlaki) (Windows/macOS media controls) · [lactea](https://github.com/M0Rf30/lactea) (visualizer) · [tokio](https://tokio.rs) (async)

## Support

If you enjoy Aulos and want to support its development:

<a href='https://ko-fi.com/W7W61U8IUL' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi6.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

## License

Aulos is free software released under the [GNU General Public License v3.0](LICENSE).
