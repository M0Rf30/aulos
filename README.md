<div align="center">

<img src="resources/icons/hicolor/scalable/apps/io.github.m0rf30.Aulos.svg" width="120" height="120" alt="Aulos">

# Aulos

**A modern music player for the COSMIC desktop**

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-2024-orange.svg)](https://www.rust-lang.org/)
[![COSMIC](https://img.shields.io/badge/COSMIC-Desktop-purple.svg)](https://github.com/pop-os/cosmic)
[![Ko-fi](https://img.shields.io/badge/Ko--fi-Support%20Me-ff5e5b?logo=ko-fi&logoColor=white)](https://ko-fi.com/W7W61U8IUL)

</div>

---

Aulos is a sleek, native music player designed specifically for the [COSMIC desktop environment](https://github.com/pop-os/cosmic). Named after the constellation and the ancient lyre, it brings together elegant design with powerful music management capabilities.

## Features

- **Multiple Sources** — Connect to MPD servers, Subsonic/OpenSubsonic servers, or play local files
- **Library Management** — SQLite-backed library with automatic metadata extraction via lofty
- **Browse Your Way** — View your collection by albums, artists, or individual tracks
- **Lyrics Support** — Automatic lyrics fetching so you can sing along
- **Cover Art** — Beautiful album artwork display
- **Real-time Updates** — File system watching keeps your library in sync
- **Visualizer Ready** — Optional ProjectM visualizer support for immersive audio experiences

## Installation

### Prebuilt Binaries

Download the archive for your architecture from the [GitHub Releases](https://github.com/M0Rf30/aulos/releases) page:

- `aulos-<version>-x86_64-unknown-linux-gnu.tar.gz` — x86_64 Linux
- `aulos-<version>-aarch64-unknown-linux-gnu.tar.gz` — aarch64 Linux

Verify the checksum, extract, and install:

```bash
sha256sum -c aulos-<version>-<target>.tar.gz.sha256
tar xzf aulos-<version>-<target>.tar.gz
cd aulos-<version>-<target>
sudo just install
```

The extracted directory contains the `aulos` binary, `LICENSE`, `README.md`, the `justfile` and `resources/` (desktop entry, AppStream metainfo, icons). If you don't have [just](https://github.com/casey/just), copy those files into place by hand.

### Runtime Requirements

- COSMIC desktop environment (or any compatible Wayland desktop)
- PipeWire client library (`libpipewire-0.3`) and OpenGL/EGL drivers — prebuilt binaries ship with the ProjectM visualizer enabled
- Optional: `ffmpeg` for lossy audio conversion

### Building from Source

Requires the Rust toolchain (2024 edition) and [just](https://github.com/casey/just):

```bash
git clone https://github.com/M0Rf30/aulos
cd aulos
just build-release
sudo just install
```

Or build with plain cargo:

```bash
cargo build --release
```

### Optional Features

Prebuilt binaries are built with `--features visualizer`. From source:

```bash
# Enable visualizer support (same as the release binaries)
cargo build --release --features visualizer

# Enable tokio-console for debugging (replaces normal log output)
cargo build --release --features tokio-console
```

## Usage

Launch Aulos from your application menu or run:

```bash
cargo run --release
```

### Adding Music Sources

1. **Local Library** — Your `~/Music` folder is automatically indexed
2. **MPD Server** — Connect to any MPD instance (local or remote)
3. **Subsonic Server** — Stream from your self-hosted music server

## Architecture

Aulos is built with a modern Rust stack:

- **UI Framework**: [libcosmic](https://github.com/pop-os/libcosmic) — Native COSMIC toolkit
- **Audio Playback**: [rodio](https://github.com/RustAudio/rodio) with symphonia for broad format support
- **Async Runtime**: [tokio](https://tokio.rs/) for responsive, non-blocking I/O
- **Database**: SQLite via [rusqlite](https://github.com/rusqlite/rusqlite) for efficient library queries
- **Protocols**: Native MPD and OpenSubsonic clients for server connectivity

## Support

If you enjoy Aulos and want to support its development, consider buying me a coffee:

<a href='https://ko-fi.com/W7W61U8IUL' target='_blank'><img height='36' style='border:0px;height:36px;' src='https://storage.ko-fi.com/cdn/kofi6.png?v=6' border='0' alt='Buy Me a Coffee at ko-fi.com' /></a>

## License

Aulos is free software released under the GNU General Public License v3.0. See [LICENSE](LICENSE) for details.

---

<div align="center">

Made with ♪ for the COSMIC ecosystem

</div>
