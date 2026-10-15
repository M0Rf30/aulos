// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Cover art of local tracks in the browser: what
//! [`CoverArt`](super::CoverArt) and [`ThumbStamp`] do with `std::fs` on the
//! desktop, through [`crate::web_fs`].

use std::hash::{Hash, Hasher};
use std::path::Path;

use super::covers::{CoverThumb, ThumbStamp};
use super::{CoverArt, pick_cover_file, web_scanner};
use crate::web_fs;

/// The encoded cover of the track `track_path`: its embedded picture, else
/// the folder's cover image.
pub async fn cover_bytes(track_path: &Path) -> Option<Vec<u8>> {
    let from_tags = async {
        let file = web_fs::open_file(track_path).await.ok()?;
        let probed = web_scanner::probe_file(track_path, &file, true)
            .await
            .ok()?;
        CoverArt::embedded_picture(probed)
    };
    if let Some(bytes) = from_tags.await {
        return Some(bytes);
    }
    let dir = track_path.parent()?;
    let names = web_fs::list_dir(dir).await.ok()?;
    let image = dir.join(pick_cover_file(&names)?);
    web_fs::read_file(&image).await.ok()
}

/// The thumbnail of the track's cover, decoded and downscaled.
pub async fn cover_thumb(track_path: &Path) -> Option<CoverThumb> {
    CoverThumb::from_bytes(&cover_bytes(track_path).await?)
}

/// Whether the file of `path` can be inspected now: its folder was granted
/// (or its files picked again) in this session.
#[must_use]
pub fn files_available(path: &Path) -> bool {
    web_fs::status(path) == web_fs::RootStatus::Ready
}

/// The stamp of the track file as it is now, for the current thumbnail size.
pub async fn thumb_stamp(track_path: &Path) -> Option<ThumbStamp> {
    let (size, mtime) = web_fs::file_info(track_path).await.ok()?;
    Some(ThumbStamp {
        path: track_path.to_string_lossy().into_owned(),
        mtime_ns: mtime.checked_mul(1_000_000_000)?,
        size,
        max_dim: CoverThumb::max_dim(),
    })
}

/// The stamp that vouches for "this album has no cover". A page cannot ask a
/// folder for its modification time, so the stamp's `size` is a hash of the
/// folder's file names instead: adding a `cover.jpg` changes it.
pub async fn coverless_stamp(track_path: &Path) -> Option<ThumbStamp> {
    let mut stamp = thumb_stamp(track_path).await?;
    let mut names = web_fs::list_dir(track_path.parent()?).await.ok()?;
    names.sort_unstable();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    names.hash(&mut hasher);
    stamp.size = hasher.finish();
    Some(stamp)
}
