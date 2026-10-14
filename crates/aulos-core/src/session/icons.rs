// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Remote icon fetching (podcast artwork, radio station favicons): which
//! URLs were already asked for, so the same image is never fetched twice.

use super::Session;
use super::events::{OnlineIcon, SessionEvent};
use crate::task::Effect;
use std::collections::HashSet;

/// The icon URLs that are fetched or being fetched.
#[derive(Debug, Default)]
pub struct IconState {
    requested: HashSet<String>,
}

impl IconState {
    /// Marks every non-empty, not yet requested URL of `urls` as requested
    /// and returns those.
    pub(super) fn request(&mut self, urls: Vec<String>) -> Vec<String> {
        urls.into_iter()
            .filter(|url| !url.is_empty() && self.requested.insert(url.clone()))
            .collect()
    }

    /// A fetch ended; a failed one is forgotten so a later request retries.
    pub(super) fn finish(&mut self, url: &str, loaded: bool) {
        if !loaded {
            self.requested.remove(url);
        }
    }
}

impl Session {
    /// Fetches each icon of `urls` that was not requested before; each
    /// result arrives as [`SessionEvent::IconLoaded`] and, when it decoded,
    /// is reported in [`SessionChanges::online_icons`](super::SessionChanges).
    /// The podcast and radio workflows call this themselves; a UI only
    /// needs it for icons it learns about elsewhere.
    pub fn fetch_icons(&mut self, urls: Vec<String>) -> Effect<SessionEvent> {
        Effect::batch(self.icons.request(urls).into_iter().map(|url| {
            Effect::future(async move {
                let icon =
                    crate::online::fetch_icon(url.clone())
                        .await
                        .map(|(width, height, rgba)| OnlineIcon {
                            url: url.clone(),
                            width,
                            height,
                            rgba,
                        });
                SessionEvent::IconLoaded { url, icon }
            })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_skips_empty_and_known_urls() {
        let mut icons = IconState::default();
        let first = icons.request(vec!["a".into(), String::new(), "b".into(), "a".into()]);
        assert_eq!(first, ["a", "b"]);
        assert!(icons.request(vec!["a".into()]).is_empty());
    }

    #[test]
    fn a_failed_fetch_may_be_requested_again() {
        let mut icons = IconState::default();
        icons.request(vec!["a".into(), "b".into()]);
        icons.finish("a", false);
        icons.finish("b", true);
        assert_eq!(icons.request(vec!["a".into(), "b".into()]), ["a"]);
    }
}
