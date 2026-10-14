// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! M3U / M3U8 / PLS playlist import and M3U export.
//!
//! Pure text/path logic (no I/O except [`read_playlist_file`]): parsing into
//! [`PlaylistEntry`] values, rendering a track list as an extended M3U, and
//! matching imported entries against library tracks — by resolved file path
//! first, then by artist + title, then by a unique file-name tail.

use super::Track;
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

/// File extensions recognised as importable playlists.
pub const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "m3u8", "pls"];

/// One line of an imported playlist.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaylistEntry {
    /// Raw location as written in the file (path, relative path, or URL).
    pub location: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_secs: Option<i64>,
}

/// Whether `path` has a playlist extension (`m3u`, `m3u8`, `pls`).
pub fn is_playlist_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        PLAYLIST_EXTENSIONS
            .iter()
            .any(|p| e.eq_ignore_ascii_case(p))
    })
}

/// Decode playlist bytes: UTF-8 (BOM stripped), falling back to Latin-1,
/// which is what legacy `.m3u` files written by Windows players use.
pub fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

/// Reads and parses a playlist file, returning `(name, entries)` where the
/// name is the file stem.
pub fn read_playlist_file(path: &Path) -> Result<(String, Vec<PlaylistEntry>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = decode_text(&bytes);
    let is_pls = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pls"));
    let entries = if is_pls {
        parse_pls(&text)
    } else {
        parse_m3u(&text)
    };
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Playlist")
        .to_string();
    Ok((name, entries))
}

/// Parse an (extended) M3U body.
pub fn parse_m3u(text: &str) -> Vec<PlaylistEntry> {
    let mut out = Vec::new();
    let mut pending: Option<PlaylistEntry> = None;
    for raw in text.lines() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = strip_prefix_ci(line, "#EXTINF:") {
            pending = Some(parse_extinf(rest));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let mut entry = pending.take().unwrap_or_default();
        entry.location = line.to_string();
        out.push(entry);
    }
    out
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Parse the part after `#EXTINF:` — `<secs>[ attrs],<display>`.
fn parse_extinf(rest: &str) -> PlaylistEntry {
    let (head, display) = match rest.split_once(',') {
        Some((h, d)) => (h, d.trim()),
        None => (rest, ""),
    };
    let duration_secs = head
        .trim()
        .split(|c: char| c.is_whitespace())
        .next()
        .and_then(|n| n.parse::<f64>().ok())
        .map(|d| d as i64)
        .filter(|d| *d >= 0);
    let (artist, title) = split_display(display);
    PlaylistEntry {
        location: String::new(),
        title,
        artist,
        duration_secs,
    }
}

/// Split `"Artist - Title"` into its parts; without a separator the whole
/// text is the title.
fn split_display(display: &str) -> (Option<String>, Option<String>) {
    let display = display.trim();
    if display.is_empty() {
        return (None, None);
    }
    match display.split_once(" - ") {
        Some((a, t)) if !a.trim().is_empty() && !t.trim().is_empty() => {
            (Some(a.trim().to_string()), Some(t.trim().to_string()))
        }
        _ => (None, Some(display.to_string())),
    }
}

/// Parse a PLS body (`FileN=`, `TitleN=`, `LengthN=`), ordered by `N`.
pub fn parse_pls(text: &str) -> Vec<PlaylistEntry> {
    let mut by_index: BTreeMap<u32, PlaylistEntry> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        // `get` (not slicing) so multi-byte keys can never split a char.
        let Some(prefix_end) = key.find(|c: char| c.is_ascii_digit()) else {
            continue;
        };
        let (name, num) = key.split_at(prefix_end);
        let Ok(n) = num.parse::<u32>() else {
            continue;
        };
        let entry = by_index.entry(n).or_default();
        if name.eq_ignore_ascii_case("file") {
            entry.location = value.to_string();
        } else if name.eq_ignore_ascii_case("title") {
            let (artist, title) = split_display(value);
            entry.artist = artist;
            entry.title = title;
        } else if name.eq_ignore_ascii_case("length") {
            entry.duration_secs = value.parse::<i64>().ok().filter(|d| *d >= 0);
        }
    }
    by_index
        .into_values()
        .filter(|e| !e.location.is_empty())
        .collect()
}

/// `path` expressed relative to the directory `base` (both absolute).
/// Returns `None` when the two share no root (e.g. different drives).
pub fn relative_path(path: &Path, base: &Path) -> Option<PathBuf> {
    let path = normalize(path);
    let base = normalize(base);
    if !path.is_absolute() || !base.is_absolute() {
        return None;
    }
    let p: Vec<_> = path.components().collect();
    let b: Vec<_> = base.components().collect();
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return None;
    }
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &p[common..] {
        out.push(c.as_os_str());
    }
    Some(out)
}

/// Lexically normalise `path`: drop `.` components and fold `..`.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Render `tracks` as an extended M3U. With `base_dir`, paths are written
/// relative to it (falling back to absolute when no relative form exists).
/// Tracks that are not local files are skipped.
pub fn write_m3u(tracks: &[Track], base_dir: Option<&Path>) -> String {
    let mut out = String::from("#EXTM3U\n");
    for t in tracks {
        if t.provider_id.as_ref() != "local" || !t.path.is_absolute() {
            continue;
        }
        let secs = t.duration.as_secs();
        let display = match (t.artist.trim().is_empty(), t.title.trim().is_empty()) {
            (false, false) => format!("{} - {}", t.artist.trim(), t.title.trim()),
            (true, false) => t.title.trim().to_string(),
            (false, true) => t.artist.trim().to_string(),
            (true, true) => String::new(),
        };
        out.push_str(&format!("#EXTINF:{secs},{}\n", one_line(&display)));
        let location = base_dir
            .and_then(|b| relative_path(&t.path, b))
            .unwrap_or_else(|| t.path.clone());
        out.push_str(&location.to_string_lossy());
        out.push('\n');
    }
    out
}

fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// Number of tracks [`write_m3u`] will actually write.
pub fn exportable_count(tracks: &[Track]) -> usize {
    tracks
        .iter()
        .filter(|t| t.provider_id.as_ref() == "local" && t.path.is_absolute())
        .count()
}

/// Outcome of [`match_entries`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchResult {
    /// Library track ids, in playlist order.
    pub matched: Vec<i64>,
    pub unmatched: usize,
}

fn text_key(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Turn a playlist location into a filesystem path: strips `file://`,
/// percent-decodes, converts Windows separators, and resolves relative
/// locations against `playlist_dir`. `None` for network URLs.
pub fn resolve_location(location: &str, playlist_dir: &Path) -> Option<PathBuf> {
    let loc = location.trim();
    let loc = if let Some(rest) = strip_prefix_ci(loc, "file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        urlencoding::decode(rest)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| rest.to_string())
    } else if loc.contains("://") {
        return None;
    } else {
        loc.to_string()
    };
    // Windows-style paths ("C:\\Music\\a.mp3", "..\\a.mp3").
    let loc = if loc.contains('\\') && !loc.contains('/') {
        loc.replace('\\', "/")
    } else {
        loc
    };
    let p = PathBuf::from(loc);
    Some(normalize(&if p.is_absolute() {
        p
    } else {
        playlist_dir.join(p)
    }))
}

/// The last two path components, lower-cased, used as a loose location key.
fn tail_key(path: &Path) -> Option<String> {
    let comps: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    let n = comps.len();
    (n > 0).then(|| comps[n.saturating_sub(2)..].join("/"))
}

/// Match `entries` against `library`. Resolution order per entry: exact
/// resolved path; artist + title (case-insensitive); unique `dir/file` tail.
pub fn match_entries(
    entries: &[PlaylistEntry],
    playlist_dir: &Path,
    library: &[Track],
) -> MatchResult {
    let mut by_path: HashMap<PathBuf, i64> = HashMap::with_capacity(library.len());
    let mut by_meta: HashMap<(String, String), i64> = HashMap::new();
    let mut by_tail: HashMap<String, Option<i64>> = HashMap::new();
    for t in library {
        by_path.entry(normalize(&t.path)).or_insert(t.id);
        by_meta
            .entry((text_key(&t.artist), text_key(&t.title)))
            .or_insert(t.id);
        if let Some(k) = tail_key(&t.path) {
            by_tail
                .entry(k)
                .and_modify(|v| *v = None) // ambiguous
                .or_insert(Some(t.id));
        }
    }

    let mut result = MatchResult::default();
    for e in entries {
        let resolved = resolve_location(&e.location, playlist_dir);
        let by_location = resolved.as_ref().and_then(|p| by_path.get(p).copied());
        let by_tags = || {
            let title = e.title.as_deref()?;
            let artist = e.artist.as_deref().unwrap_or("");
            by_meta.get(&(text_key(artist), text_key(title))).copied()
        };
        let by_filename = || {
            let key = tail_key(resolved.as_deref()?)?;
            by_tail.get(&key).copied().flatten()
        };
        match by_location.or_else(by_tags).or_else(by_filename) {
            Some(id) => result.matched.push(id),
            None => result.unmatched += 1,
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn track(id: i64, path: &str, artist: &str, title: &str, secs: u64) -> Track {
        Track {
            id,
            path: PathBuf::from(path),
            title: title.into(),
            artist: artist.into(),
            album_artist: artist.into(),
            album: "Album".into(),
            genre: String::new(),
            track_number: 1,
            disc_number: 1,
            year: 2000,
            duration: Duration::from_secs(secs),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("local"),
            source_uri: path.into(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    #[test]
    fn parses_extended_m3u() {
        let text = "\u{feff}#EXTM3U\n#EXTINF:215,Daft Punk - One More Time\nmusic/a.flac\n\n#comment\nplain.mp3\n#EXTINF:-1,Only Title\nhttp://x/stream\n";
        let e = parse_m3u(text);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].location, "music/a.flac");
        assert_eq!(e[0].artist.as_deref(), Some("Daft Punk"));
        assert_eq!(e[0].title.as_deref(), Some("One More Time"));
        assert_eq!(e[0].duration_secs, Some(215));
        assert_eq!(e[1].location, "plain.mp3");
        assert_eq!(e[1].title, None);
        assert_eq!(e[2].duration_secs, None);
        assert_eq!(e[2].title.as_deref(), Some("Only Title"));
    }

    #[test]
    fn parses_pls_in_index_order() {
        let text = "[playlist]\nFile2=/b.mp3\nTitle2=B - Two\nFile1=/a.mp3\nLength1=120\nNumberOfEntries=2\n";
        let e = parse_pls(text);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].location, "/a.mp3");
        assert_eq!(e[0].duration_secs, Some(120));
        assert_eq!(e[1].location, "/b.mp3");
        assert_eq!(e[1].artist.as_deref(), Some("B"));
    }

    #[test]
    fn decodes_latin1_fallback() {
        assert_eq!(decode_text(&[0x63, 0x61, 0x66, 0xE9]), "café");
        assert_eq!(decode_text("ok".as_bytes()), "ok");
    }

    #[test]
    fn relative_paths() {
        let r = relative_path(Path::new("/m/a/b.flac"), Path::new("/m/pl")).unwrap();
        assert_eq!(r, PathBuf::from("../a/b.flac"));
        let r = relative_path(Path::new("/m/pl/x/b.flac"), Path::new("/m/pl")).unwrap();
        assert_eq!(r, PathBuf::from("x/b.flac"));
        assert!(relative_path(Path::new("rel"), Path::new("/m")).is_none());
    }

    #[test]
    fn writes_absolute_and_relative() {
        let tracks = vec![
            track(1, "/m/a/b.flac", "Art", "Song", 61),
            track(2, "/m/c.mp3", "", "Lonely", 3),
        ];
        let abs = write_m3u(&tracks, None);
        assert_eq!(
            abs,
            "#EXTM3U\n#EXTINF:61,Art - Song\n/m/a/b.flac\n#EXTINF:3,Lonely\n/m/c.mp3\n"
        );
        let rel = write_m3u(&tracks, Some(Path::new("/m/pl")));
        assert!(rel.contains("\n../a/b.flac\n"));
        assert!(rel.contains("\n../c.mp3\n"));
    }

    #[test]
    fn skips_non_local_tracks_on_export() {
        let mut t = track(1, "/m/a.flac", "A", "T", 1);
        t.provider_id = Arc::from("navidrome");
        assert_eq!(write_m3u(&[t.clone()], None), "#EXTM3U\n");
        assert_eq!(exportable_count(&[t]), 0);
    }

    #[test]
    fn roundtrip_matches_by_path() {
        let lib = vec![
            track(1, "/m/a/b.flac", "Art", "Song", 61),
            track(2, "/m/c.mp3", "X", "Y", 3),
        ];
        let text = write_m3u(&lib, Some(Path::new("/m/pl")));
        let entries = parse_m3u(&text);
        let r = match_entries(&entries, Path::new("/m/pl"), &lib);
        assert_eq!(r.matched, vec![1, 2]);
        assert_eq!(r.unmatched, 0);
    }

    #[test]
    fn matches_by_tags_then_tail_and_counts_unmatched() {
        let lib = vec![
            track(1, "/music/Artist/Album/01 Song.flac", "Artist", "Song", 1),
            track(2, "/music/Other/Album/02 Tune.flac", "Other", "Tune", 1),
        ];
        let entries = vec![
            // Different machine path, but tags match.
            PlaylistEntry {
                location: "C:\\Users\\me\\Song.mp3".into(),
                artist: Some("ARTIST".into()),
                title: Some("song".into()),
                ..Default::default()
            },
            // No tags; unique directory/file tail matches.
            PlaylistEntry {
                location: "/mnt/old/Album/02 Tune.flac".into(),
                ..Default::default()
            },
            PlaylistEntry {
                location: "/nowhere/missing.mp3".into(),
                ..Default::default()
            },
            PlaylistEntry {
                location: "http://radio.example/stream".into(),
                ..Default::default()
            },
        ];
        let r = match_entries(&entries, Path::new("/pl"), &lib);
        assert_eq!(r.matched, vec![1, 2]);
        assert_eq!(r.unmatched, 2);
    }

    #[test]
    fn resolves_file_uris_and_relative() {
        assert_eq!(
            resolve_location("file:///m/a%20b.mp3", Path::new("/pl")),
            Some(PathBuf::from("/m/a b.mp3"))
        );
        assert_eq!(
            resolve_location("../x/y.mp3", Path::new("/pl/sub")),
            Some(PathBuf::from("/pl/x/y.mp3"))
        );
        assert_eq!(resolve_location("https://h/a.m3u8", Path::new("/")), None);
    }

    #[test]
    fn playlist_extension_detection() {
        assert!(is_playlist_path(Path::new("/a/b.M3U8")));
        assert!(is_playlist_path(Path::new("x.pls")));
        assert!(!is_playlist_path(Path::new("x.mp3")));
    }
}
