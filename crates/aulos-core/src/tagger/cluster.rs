// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Picard-style clusters: loaded files grouped by album artist and album.
//!
//! Clusters are derived from the tags *on disk* (`TagFile::original`), not
//! from the edited ones, so a file doesn't jump between groups while its
//! album field is being typed; after a save the new tags become the on-disk
//! ones and the files regroup.

use super::file::{FileId, TagFile};
use crate::library::tagio::{TagSet, keys, model::split_position};

/// What makes two files part of one album: the normalised album artist (empty
/// when the files carry none) and album. An empty album means "unclustered".
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

impl ClusterKey {
    /// The key of a file with these on-disk tags.
    #[must_use]
    pub fn of(tags: &TagSet) -> Self {
        let album = normalize(tags.get(keys::ALBUM).unwrap_or_default());
        if album.is_empty() {
            return Self::unclustered();
        }
        Self {
            album_artist: normalize(tags.get(keys::ALBUMARTIST).unwrap_or_default()),
            album,
        }
    }

    /// The key of files that have no album.
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
    /// The album name as written on the first file (empty when unclustered).
    pub album: String,
    /// The album artist, or the common track artist, or "Various Artists"
    /// text left empty for the view to localise when `various` is set.
    pub artist: String,
    /// The files carry no album artist and several different track artists.
    pub various: bool,
    /// Files in disc, track, file-name order.
    pub files: Vec<FileId>,
}

fn number(tags: &TagSet, key: &str) -> u32 {
    tags.get(key).and_then(|v| split_position(v).0).unwrap_or(0)
}

/// Groups `files` into clusters: albums alphabetically by artist then
/// title, the unclustered files last.
#[must_use]
pub fn group(files: &[TagFile]) -> Vec<Cluster> {
    struct Bucket<'a> {
        key: ClusterKey,
        members: Vec<&'a TagFile>,
    }

    let mut buckets: Vec<Bucket<'_>> = Vec::new();
    for file in files {
        let key = ClusterKey::of(&file.original);
        match buckets.iter_mut().find(|b| b.key == key) {
            Some(bucket) => bucket.members.push(file),
            None => buckets.push(Bucket {
                key,
                members: vec![file],
            }),
        }
    }

    let mut clusters: Vec<Cluster> = buckets
        .into_iter()
        .map(|mut bucket| {
            bucket.members.sort_by(|a, b| {
                number(&a.original, keys::DISCNUMBER)
                    .cmp(&number(&b.original, keys::DISCNUMBER))
                    .then(
                        number(&a.original, keys::TRACKNUMBER)
                            .cmp(&number(&b.original, keys::TRACKNUMBER)),
                    )
                    .then_with(|| a.path.cmp(&b.path))
            });
            let first = bucket.members[0];
            let album = first
                .original
                .get(keys::ALBUM)
                .unwrap_or_default()
                .trim()
                .to_owned();
            let (artist, various) = cluster_artist(&bucket.members);
            Cluster {
                key: bucket.key,
                album,
                artist,
                various,
                files: bucket.members.iter().map(|f| f.id).collect(),
            }
        })
        .collect();

    clusters.sort_by(|a, b| {
        a.key
            .is_unclustered()
            .cmp(&b.key.is_unclustered())
            .then_with(|| normalize(&a.artist).cmp(&normalize(&b.artist)))
            .then_with(|| a.key.album.cmp(&b.key.album))
    });
    clusters
}

/// The artist to show for a cluster: its album artist, else the track
/// artist every file shares, else "various" (empty text).
fn cluster_artist(members: &[&TagFile]) -> (String, bool) {
    let first = |tags: &TagSet, key: &str| tags.get(key).unwrap_or_default().trim().to_owned();
    if let Some(album_artist) = members
        .iter()
        .map(|f| first(&f.original, keys::ALBUMARTIST))
        .find(|a| !a.is_empty())
    {
        return (album_artist, false);
    }
    let mut artists = members
        .iter()
        .map(|f| first(&f.original, keys::ARTIST))
        .filter(|a| !a.is_empty());
    let Some(artist) = artists.next() else {
        return (String::new(), false);
    };
    if artists.all(|a| normalize(&a) == normalize(&artist)) {
        (artist, false)
    } else {
        (String::new(), true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::file::testing::*;
    use super::*;

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
    fn albums_are_sorted_by_artist_then_title_with_unclustered_last() {
        let files = vec![
            file(1, &[(keys::TITLE, "Loose")]),
            track(2, "Zed", "A", 1, "x"),
            track(3, "Abe", "B", 1, "y"),
            track(4, "Abe", "A", 1, "z"),
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
                (String::new(), String::new()),
            ]
        );
    }

    #[test]
    fn files_without_an_album_are_unclustered_together() {
        let files = vec![
            file(1, &[(keys::TITLE, "a"), (keys::ARTIST, "X")]),
            file(2, &[(keys::TITLE, "b"), (keys::ARTIST, "Y")]),
        ];
        let clusters = group(&files);
        assert_eq!(clusters.len(), 1);
        assert!(clusters[0].key.is_unclustered());
        assert_eq!(clusters[0].files.len(), 2);
    }

    #[test]
    fn multi_disc_albums_sort_by_disc_then_track() {
        let mut a = track(1, "A", "Box", 1, "d2t1");
        a.original.set_one(keys::DISCNUMBER, "2/2");
        let mut b = track(2, "A", "Box", 2, "d1t2");
        b.original.set_one(keys::DISCNUMBER, "1/2");
        let mut c = track(3, "A", "Box", 1, "d1t1");
        c.original.set_one(keys::DISCNUMBER, "1/2");
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
        f.edited.set_one(keys::ALBUM, "New");
        let clusters = group(&[f]);
        assert_eq!(clusters[0].album, "Old");
    }
}
