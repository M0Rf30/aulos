// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Cover Art Archive front covers (<https://musicbrainz.org/doc/Cover_Art_Archive/API>).

use super::{http_client, require_mbid};
use crate::library::tagio::{EditPicture, picture_type};

const CAA_BASE: &str = "https://coverartarchive.org";
/// A 500 px thumbnail is tens of KB and a scan of the original rarely
/// exceeds a few MB; the cap only guards against a broken server.
const MAX_COVER_BYTES: u64 = 20 * 1024 * 1024;

/// Downloads one image variant (`front-500`, `front`). `Ok(None)` for a
/// 404 (the release has no such image).
fn download(release_id: &str, variant: &str) -> Result<Option<Vec<u8>>, String> {
    // The CAA answers with a redirect to archive.org, which reqwest follows.
    let response = http_client()
        .get(format!("{CAA_BASE}/release/{release_id}/{variant}"))
        .send()
        .map_err(|e| format!("Cover Art Archive request failed: {e}"))?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!(
            "Cover Art Archive error (HTTP {})",
            status.as_u16()
        ));
    }
    crate::online::read_capped_body(response, MAX_COVER_BYTES).map(Some)
}

/// Wraps downloaded bytes as a front-cover picture, rejecting anything
/// that isn't a recognisable image (an HTML error page, say).
fn front_cover_picture(data: Vec<u8>) -> Result<EditPicture, String> {
    let mime = crate::library::tags::infer_mime(&data);
    if !mime.starts_with("image/") {
        return Err("Cover Art Archive returned something that is not an image".to_owned());
    }
    Ok(EditPicture {
        kind: picture_type::FRONT_COVER,
        mime: mime.to_owned(),
        description: String::new(),
        data,
    })
}

/// The release's front cover: the 500 px rendition if it exists, else the
/// original. `Ok(None)` when the release has no front cover.
pub fn fetch_front_cover(release_id: &str) -> Result<Option<EditPicture>, String> {
    require_mbid("release", release_id)?;
    for variant in ["front-500", "front"] {
        if let Some(data) = download(release_id, variant)? {
            return front_cover_picture(data).map(Some);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_images_as_front_covers() {
        let jpeg = b"\xFF\xD8\xFF\xE0rest".to_vec();
        let pic = front_cover_picture(jpeg.clone()).unwrap();
        assert_eq!(pic.kind, picture_type::FRONT_COVER);
        assert_eq!(pic.mime, "image/jpeg");
        assert_eq!(pic.data, jpeg);
        let png = b"\x89PNG\r\n\x1a\nxxxx".to_vec();
        assert_eq!(front_cover_picture(png).unwrap().mime, "image/png");
    }

    #[test]
    fn rejects_non_images() {
        assert!(front_cover_picture(b"<html>404</html>".to_vec()).is_err());
        assert!(front_cover_picture(Vec::new()).is_err());
    }

    #[test]
    fn rejects_malformed_release_ids_before_any_request() {
        assert!(fetch_front_cover("../x").is_err());
    }
}
