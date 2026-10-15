// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

use super::Session;
use crate::library::artist_info::{
    ArtistInfoOutcome, ArtistInfoStore, ArtistInfoWanted, resolve_via_agents, resolve_via_provider,
};
use crate::provider::{MusicProvider, ProviderType};
use crate::task::Effect;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Max concurrent artist-info fetches (Deezer/Wikipedia/Subsonic requests).
/// Small and fixed: this is a courtesy to keyless public APIs, not a
/// throughput knob.
pub const ARTIST_INFO_CONCURRENCY: usize = 2;

/// Cap on artists newly dispatched per page visit — a courtesy bound
/// independent of [`ARTIST_INFO_CONCURRENCY`] (which bounds how many run *at
/// once*, not how many get queued).
const MAX_PER_VISIT: usize = 40;

/// A fetched artist photo: decoded RGBA pixels, ready for the UI to turn into
/// an image handle.
#[derive(Debug, Clone)]
pub struct ArtistPhoto {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Artist biographies and the bookkeeping around fetching them. (The photos
/// are decoded image handles and stay with the UI; it receives each decoded
/// [`ArtistPhoto`] through the session's change report.)
pub struct ArtistInfoState {
    /// On-disk cache of photos and bios, see [`ArtistInfoStore`].
    pub store: Arc<ArtistInfoStore>,
    /// Bounds how many artist-info fetches run concurrently — a small, fixed
    /// number of permits so opening the Artists page never fires an
    /// unbounded burst of network requests.
    pub semaphore: Arc<Semaphore>,
    /// Cached artist biography text, keyed by artist name.
    pub bios: HashMap<String, String>,
    /// Artist names whose photo was fetched (and handed to the UI).
    pub photos: HashSet<String>,
    /// Artist names with an info fetch currently in flight, so opening the
    /// Artists page again (or re-selecting an artist) never double-dispatches
    /// a fetch that's already running.
    pub pending: HashSet<String>,
    /// Artist names confirmed (this session) to have neither a bio nor an
    /// image, so a page re-visit doesn't retry them in-memory until the
    /// on-disk negative-cache TTL expires and the app restarts.
    pub negative: HashSet<String>,
}

impl ArtistInfoState {
    pub(super) fn new() -> Self {
        Self {
            store: Arc::new(ArtistInfoStore::open(&crate::paths::data_dir())),
            semaphore: Arc::new(Semaphore::new(ARTIST_INFO_CONCURRENCY)),
            bios: HashMap::new(),
            photos: HashSet::new(),
            pending: HashSet::new(),
            negative: HashSet::new(),
        }
    }

    /// Applies a finished fetch: records the bio (or, when it found neither
    /// bio nor image, the negative result) and hands back the photo, if any,
    /// for the UI to keep.
    pub fn apply_outcome(&mut self, outcome: ArtistInfoOutcome) -> Option<ArtistPhoto> {
        self.pending.remove(&outcome.name);
        let mut resolved = false;
        if let Some(bio) = outcome.bio {
            self.bios.insert(outcome.name.clone(), bio);
            resolved = true;
        }
        let photo = outcome.image.map(|(width, height, rgba)| ArtistPhoto {
            name: outcome.name.clone(),
            width,
            height,
            rgba,
        });
        resolved |= photo.is_some();
        if photo.is_some() {
            self.photos.insert(outcome.name.clone());
        }
        if !resolved && !outcome.name.is_empty() {
            self.negative.insert(outcome.name);
        }
        photo
    }

    /// True when `name` has neither a cached bio or photo nor an in-flight
    /// or confirmed-negative lookup — i.e. still worth dispatching.
    fn wanted(&self, name: &str) -> bool {
        !self.photos.contains(name)
            && !self.bios.contains_key(name)
            && !self.negative.contains(name)
            && !self.pending.contains(name)
    }

    /// Marks `names` pending and returns one fetch per artist, each
    /// acquiring the shared semaphore before its blocking resolve (cache hit
    /// or network fetch), so the actual concurrency bound lives in one place
    /// rather than in how many futures happen to be dispatched at once.
    fn dispatch(
        &mut self,
        provider: &Arc<dyn MusicProvider>,
        is_subsonic: bool,
        wanted: ArtistInfoWanted,
        names: Vec<String>,
    ) -> Effect<ArtistInfoOutcome> {
        let provider_id = provider.id().to_string();
        let mut fetches = Vec::with_capacity(names.len());
        for name in names {
            self.pending.insert(name.clone());
            let provider = Arc::clone(provider);
            let store = Arc::clone(&self.store);
            let semaphore = Arc::clone(&self.semaphore);
            let provider_id = provider_id.clone();
            fetches.push(Effect::future(async move {
                let permit = semaphore.acquire_owned().await.ok();
                let now = crate::unix_now();
                let name_for_panic = name.clone();
                crate::task::spawn_blocking(move || {
                    let _permit = permit;
                    if is_subsonic {
                        resolve_via_provider(&store, provider.as_ref(), &provider_id, &name, now)
                    } else {
                        resolve_via_agents(&store, &name, now, wanted)
                    }
                })
                .await
                .unwrap_or_else(|_| ArtistInfoOutcome::empty(name_for_panic))
            }));
        }
        Effect::batch(fetches)
    }

    /// Forgets every fetched bio, photo and lookup bookkeeping (the UI drops
    /// its photo handles too: see `SessionChanges::artist_photos_cleared`),
    /// e.g. after the artist-info source was reconfigured.
    pub fn clear(&mut self) {
        self.bios.clear();
        self.photos.clear();
        self.pending.clear();
        self.negative.clear();
    }
}

impl Session {
    /// What the user allows fetching online for artists: the bio and photo
    /// toggles of the settings.
    pub fn artist_info_wanted(&self) -> ArtistInfoWanted {
        ArtistInfoWanted {
            bio: self.config.config.fetch_artist_bio,
            photo: self.config.config.fetch_artist_photos,
        }
    }

    /// The active provider for an artist-info fetch, with whether it is
    /// Subsonic and what to fetch. `None` when there is no provider, or in
    /// Local/MPD mode when neither `Config::fetch_artist_bio` nor
    /// `Config::fetch_artist_photos` is on; Subsonic always runs, as its
    /// data comes from the server, not the network toggles.
    fn artist_info_source(&self) -> Option<(Arc<dyn MusicProvider>, bool, ArtistInfoWanted)> {
        let provider = self.providers.registry.active_shared()?;
        let is_subsonic = provider.provider_type() == ProviderType::Subsonic;
        let wanted = if is_subsonic {
            ArtistInfoWanted::ALL
        } else {
            self.artist_info_wanted()
        };
        wanted.any().then_some((provider, is_subsonic, wanted))
    }

    /// Lazily fetches artist bio/image for the Artists page. Only artists
    /// not already resolved, not already in flight and not
    /// confirmed-negative this session are considered, capped per visit so
    /// opening the page never fires a burst of requests for a huge library;
    /// the remainder are picked up on a later visit, or immediately if
    /// individually selected (see [`Self::fetch_artist_info_for`]).
    pub fn fetch_artist_info_for_visible(&mut self) -> Effect<ArtistInfoOutcome> {
        let Some((provider, is_subsonic, wanted)) = self.artist_info_source() else {
            return Effect::none();
        };
        let names: Vec<String> = self
            .library
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .filter(|name| self.artist_info.wanted(name))
            .take(MAX_PER_VISIT)
            .map(str::to_string)
            .collect();
        if names.is_empty() {
            return Effect::none();
        }
        self.artist_info
            .dispatch(&provider, is_subsonic, wanted, names)
    }

    /// Fetches artist info for a single artist immediately (e.g. when
    /// selected from the detail view), bypassing the per-visit cap so a
    /// deep-linked or scrolled-past artist still gets its info without
    /// waiting for a later page visit.
    pub fn fetch_artist_info_for(&mut self, name: &str) -> Effect<ArtistInfoOutcome> {
        let Some((provider, is_subsonic, wanted)) = self.artist_info_source() else {
            return Effect::none();
        };
        if !self.artist_info.wanted(name) {
            return Effect::none();
        }
        self.artist_info
            .dispatch(&provider, is_subsonic, wanted, vec![name.to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(name: &str, bio: Option<&str>, image: bool) -> ArtistInfoOutcome {
        ArtistInfoOutcome {
            name: name.to_string(),
            bio: bio.map(str::to_string),
            image: image.then(|| (1, 1, vec![0; 4])),
        }
    }

    #[test]
    fn empty_outcome_is_negatively_cached_and_stops_being_wanted() {
        let mut state = ArtistInfoState::new();
        state.pending.insert("Nobody".into());
        assert!(
            state
                .apply_outcome(outcome("Nobody", None, false))
                .is_none()
        );
        assert!(state.pending.is_empty());
        assert!(state.negative.contains("Nobody"));
        assert!(!state.wanted("Nobody"));
    }

    #[test]
    fn bio_and_photo_are_both_kept_and_not_negative() {
        let mut state = ArtistInfoState::new();
        let photo = state
            .apply_outcome(outcome("Band", Some("bio"), true))
            .expect("photo");
        assert_eq!(
            (photo.name.as_str(), photo.width, photo.height),
            ("Band", 1, 1)
        );
        assert_eq!(state.bios.get("Band").map(String::as_str), Some("bio"));
        assert!(state.negative.is_empty());
        assert!(state.photos.contains("Band") && !state.wanted("Band"));
    }
}
