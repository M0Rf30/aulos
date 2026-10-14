// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Compilation ("Various Artists") detection and grouping.
//!
//! A compilation is an album whose tracks come from many different artists.
//! Three signals mark one, mirroring Lollypop's behaviour:
//!
//! 1. the album artist is a well-known "Various Artists" placeholder;
//! 2. the file carries a compilation flag and no album artist (applied at
//!    scan time, see `LibraryScanner::read_metadata`);
//! 3. tracks sharing an album name come from many distinct artists with no
//!    explicit album artist (the scanner copies the track artist into
//!    `album_artist` in that case, so each track would otherwise end up in a
//!    one-track "album" of its own) — handled by [`group_compilations`].
//!
//! All three resolve to `album_artist == VARIOUS_ARTISTS`, so the rest of the
//! app only needs [`is_various_artists`].

use super::Track;
use std::collections::{HashMap, HashSet};

/// Canonical album-artist name given to detected compilations.
pub const VARIOUS_ARTISTS: &str = "Various Artists";

/// Distinct track artists required to treat same-named albums as one
/// compilation when no album-artist tag is present.
pub const MIN_COMPILATION_ARTISTS: usize = 4;

/// Whether `name` is a "Various Artists" style album-artist placeholder.
pub fn is_various_artists(name: &str) -> bool {
    let n = name.trim();
    if n.is_empty() {
        return false;
    }
    const NAMES: &[&str] = &[
        "various artists",
        "various",
        "va",
        "v.a.",
        "v.a",
        "v/a",
        "various artist",
        "varios artistas",
        "varios",
        "verschiedene interpreten",
        "verschiedene",
        "artisti vari",
        "artistes divers",
        "divers",
        "diversos",
        "diverse artiesten",
    ];
    NAMES.iter().any(|v| n.eq_ignore_ascii_case(v))
}

/// Decade (e.g. `1990`) of a release year; `None` for unknown (`0`) years.
pub fn decade_of(year: u32) -> Option<u32> {
    (year > 0).then(|| year / 10 * 10)
}

/// Rewrites the album artist of tracks belonging to detected compilations to
/// [`VARIOUS_ARTISTS`] and, if anything changed, restores the
/// `album_artist, album, disc, track` ordering expected by album grouping.
///
/// An album name qualifies when it is non-empty, has at least
/// [`MIN_COMPILATION_ARTISTS`] distinct track artists, every one of its
/// tracks still has `album_artist == artist` (no explicit album artist) or
/// is already "Various Artists", and — for absolute local paths — all tracks
/// live in one directory (so unrelated "Greatest Hits" albums of different
/// artists are not merged).
pub fn group_compilations(tracks: &mut [Track]) {
    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, t) in tracks.iter().enumerate() {
        if !t.album.trim().is_empty() {
            groups.entry(t.album.as_str()).or_default().push(i);
        }
    }

    let mut to_rewrite: Vec<usize> = Vec::new();
    for idxs in groups.values() {
        if idxs.len() < MIN_COMPILATION_ARTISTS {
            continue;
        }
        let mut artists: HashSet<&str> = HashSet::new();
        let mut explicit_album_artist = false;
        for &i in idxs {
            let t = &tracks[i];
            artists.insert(t.artist.as_str());
            if t.album_artist != t.artist && !is_various_artists(&t.album_artist) {
                explicit_album_artist = true;
            }
        }
        if explicit_album_artist || artists.len() < MIN_COMPILATION_ARTISTS {
            continue;
        }
        if !same_local_directory(idxs.iter().map(|&i| &tracks[i])) {
            continue;
        }
        to_rewrite.extend(
            idxs.iter()
                .copied()
                .filter(|&i| !is_various_artists(&tracks[i].album_artist)),
        );
    }

    if to_rewrite.is_empty() {
        return;
    }
    for i in to_rewrite {
        tracks[i].album_artist = VARIOUS_ARTISTS.to_string();
    }
    tracks.sort_by(|a, b| {
        a.album_artist
            .cmp(&b.album_artist)
            .then_with(|| a.album.cmp(&b.album))
            .then(a.disc_number.cmp(&b.disc_number))
            .then(a.track_number.cmp(&b.track_number))
    });
}

/// `true` unless the tracks have absolute paths spread over several
/// directories (non-absolute paths — remote providers — are not checked).
fn same_local_directory<'a>(mut tracks: impl Iterator<Item = &'a Track>) -> bool {
    let Some(first) = tracks.next() else {
        return true;
    };
    if !first.path.is_absolute() {
        return true;
    }
    let dir = first.path.parent();
    tracks.all(|t| !t.path.is_absolute() || t.path.parent() == dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn track(id: i64, dir: &str, artist: &str, album_artist: &str, album: &str, no: u32) -> Track {
        Track {
            id,
            path: PathBuf::from(format!("{dir}/{id}.flac")),
            title: format!("t{id}"),
            artist: artist.into(),
            album_artist: album_artist.into(),
            album: album.into(),
            genre: String::new(),
            track_number: no,
            disc_number: 1,
            year: 2000,
            duration: Duration::from_secs(60),
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("local"),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    #[test]
    fn various_artists_names() {
        assert!(is_various_artists("Various Artists"));
        assert!(is_various_artists("  various artists "));
        assert!(is_various_artists("VA"));
        assert!(is_various_artists("V/A"));
        assert!(!is_various_artists("Vanilla Ice"));
        assert!(!is_various_artists(""));
    }

    #[test]
    fn decades() {
        assert_eq!(decade_of(1994), Some(1990));
        assert_eq!(decade_of(2000), Some(2000));
        assert_eq!(decade_of(0), None);
    }

    #[test]
    fn merges_many_artists_in_one_dir() {
        let mut v: Vec<Track> = ["A", "B", "C", "D"]
            .iter()
            .enumerate()
            .map(|(i, a)| track(i as i64, "/m/hits", a, a, "Hits", 4 - i as u32))
            .collect();
        group_compilations(&mut v);
        assert!(v.iter().all(|t| t.album_artist == VARIOUS_ARTISTS));
        let order: Vec<u32> = v.iter().map(|t| t.track_number).collect();
        assert_eq!(order, vec![1, 2, 3, 4]);
    }

    #[test]
    fn keeps_explicit_album_artist_and_split_dirs() {
        let mut v: Vec<Track> = ["A", "B", "C", "D"]
            .iter()
            .enumerate()
            .map(|(i, a)| track(i as i64, "/m/x", a, "Boss", "Hits", i as u32))
            .collect();
        group_compilations(&mut v);
        assert!(v.iter().all(|t| t.album_artist == "Boss"));

        let mut w: Vec<Track> = ["A", "B", "C", "D"]
            .iter()
            .enumerate()
            .map(|(i, a)| track(i as i64, &format!("/m/{a}"), a, a, "Hits", 1))
            .collect();
        group_compilations(&mut w);
        assert!(w.iter().all(|t| t.album_artist != VARIOUS_ARTISTS));
    }

    #[test]
    fn few_artists_not_a_compilation() {
        let mut v: Vec<Track> = ["A", "B"]
            .iter()
            .enumerate()
            .map(|(i, a)| track(i as i64, "/m/x", a, a, "Split", 1 + i as u32))
            .collect();
        group_compilations(&mut v);
        assert!(v.iter().all(|t| t.album_artist != VARIOUS_ARTISTS));
    }
}
