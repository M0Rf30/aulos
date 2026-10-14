// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Party mode and auto-play continuation (Lollypop's `auto_random` /
//! `auto_similar` repeat modes and party mode), as pure selection logic over
//! the in-memory library — no audio, no UI.
//!
//! The app keeps the play queue topped up *before* it runs dry
//! ([`plan_top_up`] decides when, [`continuation`] decides with what), so
//! the engine's gapless look-ahead carries on into the appended tracks
//! seamlessly instead of the queue ever "ending".
//!
//! Similarity is deliberately local and cheap (like Lollypop's
//! `similars_local.py`): same artist, shared genre tag and nearby release
//! year, scored per candidate and aggregated per album.
//!
//! Also hosts the process-wide mirror of the player's "stop after this
//! track" flag (see [`stop_after_flag`]) so the now-playing views can render
//! the toggle's state without threading another parameter through every
//! layout function.

use crate::config::{AutoPlayMode, RepeatMode};
use crate::library::Track;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// Score for sharing an artist (track artist or album artist).
const ARTIST_WEIGHT: u32 = 4;
/// Score for sharing at least one genre tag.
const GENRE_WEIGHT: u32 = 3;
/// Score for a release year within [`NEAR_YEARS`] of the seed's.
const NEAR_YEAR_WEIGHT: u32 = 2;
/// Score for a release year in the same decade as the seed's.
const SAME_DECADE_WEIGHT: u32 = 1;
/// "Nearby" release year distance.
const NEAR_YEARS: u32 = 2;
/// A candidate must reach this score (artist or genre match) to count as
/// similar at all; era alone never qualifies.
pub const MIN_SIMILAR_SCORE: u32 = 3;
/// How many of the best-scoring albums the weighted pick chooses from.
const SIMILAR_POOL: usize = 8;
/// Party mode refills when fewer than this many tracks are queued after
/// the current one...
pub const PARTY_LOOKAHEAD: usize = 2;
/// ...adding this many random tracks at a time.
pub const PARTY_BATCH: usize = 5;

// ---------------------------------------------------------------------------
// "Stop after this track" mirror
// ---------------------------------------------------------------------------

static STOP_AFTER_CURRENT: AtomicBool = AtomicBool::new(false);

/// Whether the player is set to stop once the current track finishes.
/// Written only by `Player`; read by the views.
pub fn stop_after_flag() -> bool {
    STOP_AFTER_CURRENT.load(Ordering::Relaxed)
}

pub(super) fn publish_stop_after(on: bool) {
    STOP_AFTER_CURRENT.store(on, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// RNG
// ---------------------------------------------------------------------------

/// Tiny xorshift64* PRNG (aulos has no `rand` dependency).
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seeded from the clock plus per-process hasher entropy.
    pub fn seeded() -> Self {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        use std::time::{SystemTime, UNIX_EPOCH};

        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        let hash = RandomState::new().build_hasher().finish();
        Self::from_seed(time ^ hash.rotate_left(23))
    }

    /// Deterministic generator (tests).
    pub fn from_seed(seed: u64) -> Self {
        Self(if seed == 0 {
            0x2545_F491_4F6C_DD1D
        } else {
            seed
        })
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish value in `0..n` (`0` when `n == 0`).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

// ---------------------------------------------------------------------------
// Eligibility / keys
// ---------------------------------------------------------------------------

/// Radio stations and podcast episodes are never auto-queued.
fn is_music(track: &Track) -> bool {
    !matches!(&*track.provider_id, "radio" | "podcast")
}

/// Genre tags split on `;`, `/` and `,`, lowercased and trimmed.
pub fn genre_tokens(genre: &str) -> Vec<String> {
    genre
        .split([';', '/', ','])
        .map(|g| g.trim().to_lowercase())
        .filter(|g| !g.is_empty())
        .collect()
}

/// Whether `track_genre` shares a tag with any of the (already
/// lowercased) `wanted` tokens.
fn shares_genre(track_genre: &str, wanted: &[String]) -> bool {
    if wanted.is_empty() || track_genre.trim().is_empty() {
        return false;
    }
    genre_tokens(track_genre).iter().any(|g| wanted.contains(g))
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Identity of the album a track belongs to. Tracks with no album tag are
/// each their own "album".
fn album_key(track: &Track) -> (String, String) {
    if track.album.trim().is_empty() {
        return (format!("\u{0}{}", track.id), String::new());
    }
    let artist = if track.album_artist.trim().is_empty() {
        &track.artist
    } else {
        &track.album_artist
    };
    (norm(artist), norm(&track.album))
}

/// What a seed track is compared against.
#[derive(Debug, Clone)]
pub struct SeedProfile {
    artists: Vec<String>,
    genres: Vec<String>,
    year: u32,
}

impl SeedProfile {
    pub fn from_track(seed: &Track) -> Self {
        let mut artists = Vec::new();
        for a in [&seed.artist, &seed.album_artist] {
            let a = norm(a);
            if !a.is_empty() && !artists.contains(&a) {
                artists.push(a);
            }
        }
        Self {
            artists,
            genres: genre_tokens(&seed.genre),
            year: seed.year,
        }
    }

    /// Similarity of `cand` to the seed (`0` = unrelated).
    pub fn score(&self, cand: &Track) -> u32 {
        let mut score = 0;
        if !self.artists.is_empty() {
            let a = norm(&cand.artist);
            let aa = norm(&cand.album_artist);
            if self.artists.iter().any(|s| *s == a || *s == aa) {
                score += ARTIST_WEIGHT;
            }
        }
        if shares_genre(&cand.genre, &self.genres) {
            score += GENRE_WEIGHT;
        }
        if self.year > 0 && cand.year > 0 {
            if self.year.abs_diff(cand.year) <= NEAR_YEARS {
                score += NEAR_YEAR_WEIGHT;
            } else if self.year / 10 == cand.year / 10 {
                score += SAME_DECADE_WEIGHT;
            }
        }
        score
    }
}

/// All of an album's tracks in disc/track order.
fn album_tracks(library: &[Track], indices: &[usize]) -> Vec<Track> {
    let mut tracks: Vec<Track> = indices.iter().map(|&i| library[i].clone()).collect();
    Track::sort_by_disc_and_track(&mut tracks);
    tracks
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Pick one random album from the library (Lollypop's auto-random): a
/// random eligible track, expanded to its whole album. `genres` (lowercase
/// tokens) restricts the pool; empty = anywhere. Tracks in `exclude` (and
/// albums they belong to) are skipped, unless that would leave nothing.
pub fn select_random_album(
    library: &[Track],
    exclude: &HashSet<i64>,
    genres: &[String],
    rng: &mut Rng,
) -> Vec<Track> {
    let pool = |skip_excluded: bool| -> Vec<usize> {
        library
            .iter()
            .enumerate()
            .filter(|(_, t)| is_music(t))
            .filter(|(_, t)| !(skip_excluded && exclude.contains(&t.id)))
            .filter(|(_, t)| genres.is_empty() || shares_genre(&t.genre, genres))
            .map(|(i, _)| i)
            .collect()
    };
    let mut eligible = pool(true);
    let skip_excluded = !eligible.is_empty();
    if !skip_excluded {
        eligible = pool(false);
    }
    if eligible.is_empty() {
        return Vec::new();
    }
    let pick = eligible[rng.below(eligible.len())];
    let key = album_key(&library[pick]);
    let indices: Vec<usize> = library
        .iter()
        .enumerate()
        .filter(|(_, t)| is_music(t))
        .filter(|(_, t)| !(skip_excluded && exclude.contains(&t.id)))
        .filter(|(_, t)| album_key(t) == key)
        .map(|(i, _)| i)
        .collect();
    album_tracks(library, &indices)
}

/// Pick the next album for auto-similar playback: albums sharing the
/// seed's artist / genre / era, never the seed's own album and never an
/// album already queued, chosen by a score-weighted draw among the best
/// few. Falls back to [`select_random_album`] when nothing is similar.
pub fn select_similar_album(
    seed: &Track,
    library: &[Track],
    exclude: &HashSet<i64>,
    rng: &mut Rng,
) -> Vec<Track> {
    let profile = SeedProfile::from_track(seed);
    let seed_album = album_key(seed);

    // album key -> (best track score, member track indices)
    let mut albums: HashMap<(String, String), (u32, Vec<usize>)> = HashMap::new();
    for (i, t) in library.iter().enumerate() {
        if !is_music(t) || exclude.contains(&t.id) || t.id == seed.id {
            continue;
        }
        let key = album_key(t);
        if key == seed_album {
            continue;
        }
        let score = profile.score(t);
        let entry = albums.entry(key).or_insert((0, Vec::new()));
        entry.0 = entry.0.max(score);
        entry.1.push(i);
    }

    let mut ranked: Vec<((String, String), u32, Vec<usize>)> = albums
        .into_iter()
        .filter(|(_, (score, _))| *score >= MIN_SIMILAR_SCORE)
        .map(|(key, (score, idx))| (key, score, idx))
        .collect();
    if ranked.is_empty() {
        return select_random_album(library, exclude, &[], rng);
    }
    // Best first; ties broken by key so the order is deterministic.
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked.truncate(SIMILAR_POOL);

    let total: u64 = ranked.iter().map(|(_, s, _)| u64::from(*s)).sum();
    let mut roll = rng.next_u64() % total.max(1);
    let mut chosen = 0;
    for (i, (_, score, _)) in ranked.iter().enumerate() {
        let w = u64::from(*score);
        if roll < w {
            chosen = i;
            break;
        }
        roll -= w;
    }
    album_tracks(library, &ranked[chosen].2)
}

/// `count` random tracks for party mode, drawn from `genres` (lowercase
/// tokens; empty = the whole library) and avoiding `exclude`. Falls back
/// to repeating already-queued tracks only when the pool is exhausted.
pub fn select_party_tracks(
    library: &[Track],
    genres: &[String],
    exclude: &HashSet<i64>,
    count: usize,
    rng: &mut Rng,
) -> Vec<Track> {
    let pool = |skip_excluded: bool| -> Vec<usize> {
        library
            .iter()
            .enumerate()
            .filter(|(_, t)| is_music(t))
            .filter(|(_, t)| !(skip_excluded && exclude.contains(&t.id)))
            .filter(|(_, t)| genres.is_empty() || shares_genre(&t.genre, genres))
            .map(|(i, _)| i)
            .collect()
    };
    let mut eligible = pool(true);
    if eligible.is_empty() {
        eligible = pool(false);
    }
    // Partial Fisher-Yates: only the first `take` slots are needed.
    let take = count.min(eligible.len());
    for i in 0..take {
        let j = i + rng.below(eligible.len() - i);
        eligible.swap(i, j);
    }
    eligible[..take]
        .iter()
        .map(|&i| library[i].clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

/// What kind of refill the queue needs right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopUp {
    Party,
    Random,
    Similar,
}

/// Decide whether (and how) to extend the queue. `upcoming` is the number
/// of tracks queued after the current one. Looping repeat modes never
/// run out, so they never need a refill.
pub fn plan_top_up(
    upcoming: usize,
    repeat: RepeatMode,
    party: bool,
    auto: AutoPlayMode,
) -> Option<TopUp> {
    if repeat != RepeatMode::None {
        return None;
    }
    if party {
        return (upcoming < PARTY_LOOKAHEAD).then_some(TopUp::Party);
    }
    if upcoming > 0 {
        return None;
    }
    match auto {
        AutoPlayMode::Off => None,
        AutoPlayMode::Random => Some(TopUp::Random),
        AutoPlayMode::Similar => Some(TopUp::Similar),
    }
}

/// The tracks to append for a refill of kind `kind`. `seed` is the track
/// now playing (similarity anchor); `party_genres` the raw configured
/// genre names.
pub fn continuation(
    kind: TopUp,
    seed: Option<&Track>,
    library: &[Track],
    party_genres: &[String],
    exclude: &HashSet<i64>,
    rng: &mut Rng,
) -> Vec<Track> {
    match kind {
        TopUp::Party => {
            let genres: Vec<String> = party_genres.iter().map(|g| norm(g)).collect();
            select_party_tracks(library, &genres, exclude, PARTY_BATCH, rng)
        }
        TopUp::Random => select_random_album(library, exclude, &[], rng),
        TopUp::Similar => match seed {
            Some(seed) => select_similar_album(seed, library, exclude, rng),
            None => select_random_album(library, exclude, &[], rng),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[allow(clippy::too_many_arguments)]
    fn track(
        id: i64,
        artist: &str,
        album: &str,
        genre: &str,
        year: u32,
        track_number: u32,
    ) -> Track {
        Track {
            id,
            path: PathBuf::from(format!("/m/{id}.flac")),
            title: format!("t{id}"),
            artist: artist.to_string(),
            album_artist: artist.to_string(),
            album: album.to_string(),
            genre: genre.to_string(),
            track_number,
            disc_number: 1,
            year,
            duration: Duration::from_secs(200),
            bitrate: 0,
            sample_rate: 44_100,
            provider_id: "local".into(),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
        }
    }

    fn ids(tracks: &[Track]) -> Vec<i64> {
        tracks.iter().map(|t| t.id).collect()
    }

    /// Seed: Artist A, "A1", Rock, 1995. Library: A's other album, a Rock
    /// album by B (same era), a Jazz album by C, and the seed's own album.
    fn library() -> Vec<Track> {
        vec![
            track(1, "A", "A1", "Rock", 1995, 1),
            track(2, "A", "A1", "Rock", 1995, 2),
            track(3, "A", "A2", "Pop", 2010, 1),
            track(4, "A", "A2", "Pop", 2010, 2),
            track(5, "B", "B1", "Rock", 1996, 1),
            track(6, "B", "B1", "Rock", 1996, 2),
            track(7, "C", "C1", "Jazz", 1960, 2),
            track(8, "C", "C1", "Jazz", 1960, 1),
        ]
    }

    #[test]
    fn similarity_prefers_artist_then_genre_then_era() {
        let lib = library();
        let seed = &lib[0];
        let p = SeedProfile::from_track(seed);
        let same_artist_other_genre = p.score(&lib[2]); // A, Pop, 2010
        let same_genre_near_year = p.score(&lib[4]); // B, Rock, 1996
        let unrelated = p.score(&lib[6]); // C, Jazz, 1960
        assert_eq!(same_artist_other_genre, ARTIST_WEIGHT);
        assert_eq!(same_genre_near_year, GENRE_WEIGHT + NEAR_YEAR_WEIGHT);
        assert_eq!(unrelated, 0);
        assert!(same_genre_near_year > same_artist_other_genre);
    }

    #[test]
    fn era_alone_is_not_similar() {
        let seed = track(1, "A", "A1", "Rock", 1995, 1);
        let p = SeedProfile::from_track(&seed);
        let other = track(2, "Z", "Z1", "Jazz", 1996, 1);
        assert_eq!(p.score(&other), NEAR_YEAR_WEIGHT);
        assert!(p.score(&other) < MIN_SIMILAR_SCORE);
    }

    #[test]
    fn genre_tokens_split_and_normalise() {
        assert_eq!(
            genre_tokens(" Rock ; Indie/Pop,  "),
            vec!["rock", "indie", "pop"]
        );
        let seed = track(1, "A", "A1", "Alt Rock; Indie", 2000, 1);
        let cand = track(2, "B", "B1", "indie", 2001, 1);
        assert!(SeedProfile::from_track(&seed).score(&cand) >= GENRE_WEIGHT);
    }

    #[test]
    fn similar_never_returns_seed_album_or_unrelated() {
        let lib = library();
        for seed_value in 0..50u64 {
            let mut rng = Rng::from_seed(seed_value + 1);
            let picked = select_similar_album(&lib[0], &lib, &HashSet::new(), &mut rng);
            assert!(!picked.is_empty());
            let got = ids(&picked);
            assert!(
                got == vec![3, 4] || got == vec![5, 6],
                "picked an unrelated/seed album: {got:?}"
            );
        }
    }

    #[test]
    fn similar_returns_a_whole_album_in_track_order() {
        let lib = library();
        // Only the Jazz album is eligible, and it is stored out of order.
        let exclude: HashSet<i64> = [1, 2, 3, 4, 5, 6].into_iter().collect();
        let seed = track(100, "C", "Other", "Jazz", 1960, 1);
        let mut rng = Rng::from_seed(7);
        let picked = select_similar_album(&seed, &lib, &exclude, &mut rng);
        assert_eq!(ids(&picked), vec![8, 7]);
    }

    #[test]
    fn similar_skips_queued_albums_and_falls_back_to_random() {
        let lib = library();
        // Both similar albums are already queued: nothing scores, so the
        // random fallback still yields *something* not already queued.
        let exclude: HashSet<i64> = [1, 2, 3, 4, 5, 6].into_iter().collect();
        let mut rng = Rng::from_seed(11);
        let picked = select_similar_album(&lib[0], &lib, &exclude, &mut rng);
        assert_eq!(ids(&picked), vec![8, 7]);
    }

    #[test]
    fn similar_weighting_favours_stronger_matches() {
        let lib = library();
        let mut strong = 0;
        let runs = 400;
        let mut rng = Rng::from_seed(42);
        for _ in 0..runs {
            // [5,6] scores genre+era (5), [3,4] scores artist only (4).
            if ids(&select_similar_album(
                &lib[0],
                &lib,
                &HashSet::new(),
                &mut rng,
            )) == vec![5, 6]
            {
                strong += 1;
            }
        }
        assert!(
            strong > runs * 45 / 100,
            "strong album picked {strong}/{runs}"
        );
        assert!(strong < runs, "weak album must still be reachable");
    }

    #[test]
    fn random_album_is_whole_album_and_respects_exclusions() {
        let lib = library();
        let exclude: HashSet<i64> = [1, 2, 3, 4].into_iter().collect();
        for s in 1..40u64 {
            let mut rng = Rng::from_seed(s);
            let got = ids(&select_random_album(&lib, &exclude, &[], &mut rng));
            assert!(got == vec![5, 6] || got == vec![8, 7], "got {got:?}");
        }
        // Everything excluded: still returns an album rather than nothing.
        let all: HashSet<i64> = (1..=8).collect();
        let mut rng = Rng::from_seed(3);
        assert!(!select_random_album(&lib, &all, &[], &mut rng).is_empty());
        assert!(select_random_album(&[], &HashSet::new(), &[], &mut rng).is_empty());
    }

    #[test]
    fn radio_and_podcasts_are_never_auto_queued() {
        let mut lib = library();
        for t in &mut lib {
            t.provider_id = "radio".into();
        }
        let mut rng = Rng::from_seed(1);
        assert!(select_random_album(&lib, &HashSet::new(), &[], &mut rng).is_empty());
        assert!(select_party_tracks(&lib, &[], &HashSet::new(), 3, &mut rng).is_empty());
    }

    #[test]
    fn party_tracks_filter_by_genre_without_duplicates() {
        let lib = library();
        let mut rng = Rng::from_seed(9);
        let genres = vec!["rock".to_string()];
        let got = select_party_tracks(&lib, &genres, &HashSet::new(), 10, &mut rng);
        let mut got_ids = ids(&got);
        got_ids.sort_unstable();
        assert_eq!(
            got_ids,
            vec![1, 2, 5, 6],
            "only rock, each once, capped at pool size"
        );

        let exclude: HashSet<i64> = [1, 2].into_iter().collect();
        let got = select_party_tracks(&lib, &genres, &exclude, 2, &mut rng);
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|t| t.id == 5 || t.id == 6));
    }

    #[test]
    fn party_empty_genre_list_means_whole_library() {
        let lib = library();
        let mut rng = Rng::from_seed(5);
        let got = select_party_tracks(&lib, &[], &HashSet::new(), 8, &mut rng);
        assert_eq!(got.len(), 8);
    }

    #[test]
    fn plan_top_up_rules() {
        use AutoPlayMode::*;
        // Nothing configured: never.
        assert_eq!(plan_top_up(0, RepeatMode::None, false, Off), None);
        // Auto modes only at the very end of the queue.
        assert_eq!(
            plan_top_up(0, RepeatMode::None, false, Random),
            Some(TopUp::Random)
        );
        assert_eq!(
            plan_top_up(0, RepeatMode::None, false, Similar),
            Some(TopUp::Similar)
        );
        assert_eq!(plan_top_up(1, RepeatMode::None, false, Similar), None);
        // Looping repeat modes make a refill pointless.
        assert_eq!(plan_top_up(0, RepeatMode::All, false, Random), None);
        assert_eq!(plan_top_up(0, RepeatMode::One, true, Random), None);
        // Party refills ahead of time and wins over the auto mode.
        assert_eq!(
            plan_top_up(1, RepeatMode::None, true, Similar),
            Some(TopUp::Party)
        );
        assert_eq!(
            plan_top_up(PARTY_LOOKAHEAD, RepeatMode::None, true, Off),
            None
        );
    }

    #[test]
    fn continuation_dispatches_per_kind() {
        let lib = library();
        let mut rng = Rng::from_seed(13);
        let seed = &lib[0];
        let sim = continuation(
            TopUp::Similar,
            Some(seed),
            &lib,
            &[],
            &HashSet::new(),
            &mut rng,
        );
        assert!(!sim.is_empty());
        let rnd = continuation(TopUp::Random, None, &lib, &[], &HashSet::new(), &mut rng);
        assert!(!rnd.is_empty());
        let party = continuation(
            TopUp::Party,
            None,
            &lib,
            &["Jazz".to_string()],
            &HashSet::new(),
            &mut rng,
        );
        assert_eq!(ids(&party).len(), 2);
        assert!(party.iter().all(|t| t.genre == "Jazz"));
        // Similar without a seed degrades to random.
        let no_seed = continuation(TopUp::Similar, None, &lib, &[], &HashSet::new(), &mut rng);
        assert!(!no_seed.is_empty());
    }

    #[test]
    fn rng_is_deterministic_and_bounded() {
        let mut a = Rng::from_seed(99);
        let mut b = Rng::from_seed(99);
        for _ in 0..50 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut r = Rng::from_seed(0);
        for n in 1..20 {
            assert!(r.below(n) < n);
        }
        assert_eq!(r.below(0), 0);
    }
}
