// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Picard-style clusters: loaded files grouped by album artist and album.
//!
//! Clusters are derived from the tags *on disk* (`TagFile::original`), not
//! from the edited ones, so a file doesn't jump between groups while its
//! album field is being typed; after a save the new tags become the on-disk
//! ones and the files regroup.

use super::file::{FileId, TagFile};
use tagcore::metadata::split_position;
use tagcore::{Metadata, keys};

/// What makes two files part of one album: the normalised album artist (empty
/// when the group has none) and album. An empty album means "unclustered".
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClusterKey {
    album_artist: String,
    album: String,
}

/// Lowercase, trimmed, whitespace collapsed.
fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `tagcore::cluster` names a group of files with different track artists
/// and no album artist after this.
const VARIOUS_ARTISTS: &str = "various artists";

impl ClusterKey {
    fn new(artist: &str, album: &str) -> Self {
        Self {
            album_artist: normalize(artist),
            album: normalize(album),
        }
    }

    /// The key of files no album could be derived for.
    #[must_use]
    pub fn unclustered() -> Self {
        Self {
            album_artist: String::new(),
            album: String::new(),
        }
    }

    #[must_use]
    pub fn is_unclustered(&self) -> bool {
        self.album.is_empty()
    }
}

/// One group of files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub key: ClusterKey,
    /// The album name as most files write it (empty when unclustered).
    pub album: String,
    /// The album artist, or the common track artist; left empty for the view
    /// to localise as "Various Artists" when `various` is set.
    pub artist: String,
    /// The files carry no album artist and several different track artists.
    pub various: bool,
    /// Files in disc, track, file-name order.
    pub files: Vec<FileId>,
}

fn number(tags: &Metadata, key: &str) -> u32 {
    tags.get(key).and_then(|v| split_position(v).0).unwrap_or(0)
}

/// Groups `files` into clusters with `tagcore::cluster::cluster` (fuzzy album
/// and album-artist match, the parent directory's name standing in for a
/// missing album): albums alphabetically by artist then title, the files no
/// group could be made for (no album, no directory) last.
#[must_use]
pub fn group(files: &[TagFile]) -> Vec<Cluster> {
    let tags: Vec<&Metadata> = files.iter().map(|f| &f.original).collect();
    let dirs: Vec<Option<&str>> = files
        .iter()
        .map(|f| f.directory().and_then(|d| d.to_str()))
        .collect();
    let groups = tagcore::cluster::cluster(&tags, &dirs);

    let mut grouped = vec![false; files.len()];
    let mut clusters: Vec<Cluster> = groups
        .into_iter()
        .map(|g| {
            let mut members: Vec<&TagFile> = g.members.iter().map(|&i| &files[i]).collect();
            for &i in &g.members {
                grouped[i] = true;
            }
            let various = normalize(&g.artist) == VARIOUS_ARTISTS;
            let artist = if various { String::new() } else { g.artist };
            Cluster {
                key: ClusterKey::new(&artist, &g.album),
                album: g.album,
                artist,
                various,
                files: sorted_ids(&mut members),
            }
        })
        .collect();

    let mut rest: Vec<&TagFile> = files
        .iter()
        .zip(grouped)
        .filter(|(_, g)| !g)
        .map(|(f, _)| f)
        .collect();
    if !rest.is_empty() {
        clusters.push(Cluster {
            key: ClusterKey::unclustered(),
            album: String::new(),
            artist: String::new(),
            various: false,
            files: sorted_ids(&mut rest),
        });
    }
    clusters
}

/// The ids of `members` in disc, track, file-name order.
fn sorted_ids(members: &mut [&TagFile]) -> Vec<FileId> {
    members.sort_by(|a, b| {
        number(&a.original, keys::DISCNUMBER)
            .cmp(&number(&b.original, keys::DISCNUMBER))
            .then(
                number(&a.original, keys::TRACKNUMBER).cmp(&number(&b.original, keys::TRACKNUMBER)),
            )
            .then_with(|| a.path.cmp(&b.path))
    });
    members.iter().map(|f| f.id).collect()
}

#[cfg(test)]
mod tests {
    use super::super::file::testing::*;
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn groups_by_album_artist_and_album_ignoring_case_and_spacing() {
        let files = vec![
            track(1, "Moderat", "II", 2, "Bad Kingdom"),
            track(2, "moderat ", "ii", 1, "Versions"),
            track(3, "Moderat", "Moderat", 1, "Bad Kingdom"),
            track(4, "Other", "II", 1, "Different artist, same album"),
        ];
        let clusters = group(&files);
        assert_eq!(clusters.len(), 3);
        let ii = clusters
            .iter()
            .find(|c| {
                c.album.eq_ignore_ascii_case("ii") && c.artist.eq_ignore_ascii_case("moderat")
            })
            .unwrap();
        assert_eq!(ii.files, vec![2, 1], "ordered by track number");
    }

    #[test]
    fn albums_are_sorted_by_artist_then_title() {
        let files = vec![
            track(1, "Zed", "A", 1, "x"),
            track(2, "Abe", "B", 1, "y"),
            track(3, "Abe", "A", 1, "z"),
        ];
        let order: Vec<(String, String)> = group(&files)
            .into_iter()
            .map(|c| (c.artist, c.album))
            .collect();
        assert_eq!(
            order,
            vec![
                ("Abe".to_owned(), "A".to_owned()),
                ("Abe".to_owned(), "B".to_owned()),
                ("Zed".to_owned(), "A".to_owned()),
            ]
        );
    }

    #[test]
    fn files_without_an_album_cluster_by_their_directory() {
        let files = vec![
            file(1, &[(keys::TITLE, "a"), (keys::ARTIST, "X")]),
            file(2, &[(keys::TITLE, "b"), (keys::ARTIST, "X")]),
        ];
        let clusters = group(&files);
        assert_eq!(clusters.len(), 1);
        assert!(!clusters[0].key.is_unclustered());
        assert_eq!(clusters[0].album, "music");
        assert_eq!(clusters[0].artist, "X");
        assert_eq!(clusters[0].files.len(), 2);
    }

    #[test]
    fn files_with_neither_album_nor_directory_are_unclustered_last() {
        let mut loose = file(1, &[(keys::TITLE, "Loose")]);
        loose.path = PathBuf::from("loose.flac");
        let files = vec![loose, track(2, "Abe", "A", 1, "z")];
        let clusters = group(&files);
        assert_eq!(clusters.len(), 2);
        assert!(!clusters[0].key.is_unclustered());
        assert!(clusters[1].key.is_unclustered());
        assert_eq!(clusters[1].files, vec![1]);
    }

    #[test]
    fn multi_disc_albums_sort_by_disc_then_track() {
        let mut a = track(1, "A", "Box", 1, "d2t1");
        a.original.set(keys::DISCNUMBER, "2/2");
        let mut b = track(2, "A", "Box", 2, "d1t2");
        b.original.set(keys::DISCNUMBER, "1/2");
        let mut c = track(3, "A", "Box", 1, "d1t1");
        c.original.set(keys::DISCNUMBER, "1/2");
        let clusters = group(&[a, b, c]);
        assert_eq!(clusters[0].files, vec![3, 2, 1]);
    }

    #[test]
    fn albums_without_album_artist_cluster_by_album_and_report_various_artists() {
        let mut a = file(1, &[(keys::ALBUM, "Mix"), (keys::ARTIST, "One")]);
        let mut b = file(2, &[(keys::ALBUM, "Mix"), (keys::ARTIST, "Two")]);
        a.edited = a.original.clone();
        b.edited = b.original.clone();
        let clusters = group(&[a, b]);
        assert_eq!(clusters.len(), 1);
        assert!(clusters[0].various);
        assert!(clusters[0].artist.is_empty());
    }

    #[test]
    fn clusters_ignore_unsaved_edits() {
        let mut f = track(1, "A", "Old", 1, "t");
        f.edited.set(keys::ALBUM, "New");
        let clusters = group(&[f]);
        assert_eq!(clusters[0].album, "Old");
    }
}
