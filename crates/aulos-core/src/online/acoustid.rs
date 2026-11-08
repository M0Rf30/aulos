// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! AcoustID links for the tag editor. Fingerprint lookups and submissions
//! are done by `tagcore::acoustid`.

/// `https://acoustid.org/track/<id>`.
#[must_use]
pub fn track_url(acoustid: &str) -> String {
    format!("https://acoustid.org/track/{acoustid}")
}

/// Opens an AcoustID's page in the default browser.
pub fn open_track_page(acoustid: &str) -> Result<(), String> {
    crate::desktop::browser::open_in_browser(&track_url(acoustid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_url_points_at_the_acoustid_page() {
        assert_eq!(
            track_url("9ff43b6a-4f16-427c-93c2-92307ca505e0"),
            "https://acoustid.org/track/9ff43b6a-4f16-427c-93c2-92307ca505e0"
        );
    }
}
