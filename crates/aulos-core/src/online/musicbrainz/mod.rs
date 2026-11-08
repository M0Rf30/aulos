// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! MusicBrainz for the tag editor: release search, release lookup and Cover
//! Art Archive downloads (all done by `tagcore`, with its process-wide
//! throttle), plus "add release" seeding and MusicBrainz page links.
//!
//! Every function here is blocking; call them from a worker thread.

mod seed;

use tagcore::mb::is_mbid;
use tagcore::settings::MetadataSettings;
use tagcore::{CoverImage, Release, ReleaseCandidate, ReleaseQuery};

pub use seed::{SeedInput, seed_fields, seed_html, seed_release_add};

const SITE_BASE: &str = "https://musicbrainz.org";
/// Releases returned per search; the web service's default is 25.
pub const SEARCH_LIMIT: u32 = 25;

fn require_mbid(what: &str, s: &str) -> Result<(), String> {
    if is_mbid(s) {
        Ok(())
    } else {
        Err(format!("'{s}' is not a valid MusicBrainz {what} id"))
    }
}

/// Searches releases; every non-empty field of `query` narrows the search.
pub fn search_releases(query: &ReleaseQuery) -> Result<Vec<ReleaseCandidate>, String> {
    tagcore::mb::search_releases(query, SEARCH_LIMIT).map_err(|e| e.to_string())
}

/// Fetches a release with Picard's default metadata mapping.
pub fn release(mbid: &str) -> Result<Release, String> {
    require_mbid("release", mbid)?;
    tagcore::mb::lookup_release(mbid, &MetadataSettings::default(), None).map_err(|e| e.to_string())
}

/// The front cover of release `release_id` from the Cover Art Archive, or
/// `None` when it has none.
pub fn fetch_front_cover(release_id: &str) -> Result<Option<CoverImage>, String> {
    require_mbid("release", release_id)?;
    tagcore::caa::fetch_front_cover(release_id).map_err(|e| e.to_string())
}

/// `https://musicbrainz.org/release/<mbid>`.
#[must_use]
pub fn release_url(mbid: &str) -> String {
    format!("{SITE_BASE}/release/{mbid}")
}

/// `https://musicbrainz.org/recording/<mbid>`.
#[must_use]
pub fn recording_url(mbid: &str) -> String {
    format!("{SITE_BASE}/recording/{mbid}")
}

/// Opens a recording's MusicBrainz page in the default browser.
pub fn open_recording_page(mbid: &str) -> Result<(), String> {
    require_mbid("recording", mbid)?;
    crate::desktop::browser::open_in_browser(&recording_url(mbid))
}

/// Opens a release's MusicBrainz page in the default browser.
pub fn open_release_page(mbid: &str) -> Result<(), String> {
    require_mbid("release", mbid)?;
    crate::desktop::browser::open_in_browser(&release_url(mbid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_links_need_a_valid_mbid() {
        assert!(open_release_page("nope").is_err());
        assert!(open_recording_page("").is_err());
        assert_eq!(
            release_url("00398749-1816-4c98-8a39-377627740365"),
            "https://musicbrainz.org/release/00398749-1816-4c98-8a39-377627740365"
        );
    }
}
