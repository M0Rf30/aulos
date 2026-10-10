// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Path-identified, shuffled preset rotation for the projectM visualizer
//! (behind the `visualizer` feature flag).
//!
//! Replaces libprojectM's own playlist. The playlist API can neither report
//! which preset it landed on after an automatic (timer/beat) switch, nor
//! stays out of the way of a direct `projectm_load_preset_file`: a failing
//! direct load triggers the playlist's retry logic, which silently loads a
//! *different* random preset. Owning the rotation means the renderer always
//! knows the exact file on screen, so the preset browser can highlight it.

use std::path::{Path, PathBuf};

/// A shuffled, endlessly repeating rotation over a fixed list of preset
/// files. Every preset is visited once per cycle; the next cycle is
/// reshuffled so the first pick never repeats the previous cycle's last.
pub struct PresetPlaylist {
    items: Vec<PathBuf>,
    /// Permutation of `0..items.len()` for the current cycle.
    order: Vec<usize>,
    /// Position in `order` of the next preset to hand out.
    cursor: usize,
    /// xorshift64* state (never zero).
    rng: u64,
}

impl PresetPlaylist {
    pub fn new(items: Vec<PathBuf>, seed: u64) -> Self {
        let mut playlist = Self {
            order: Vec::new(),
            cursor: 0,
            rng: seed | 1,
            items,
        };
        playlist.reshuffle(None);
        playlist
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Next preset in the shuffled rotation, or `None` for an empty list.
    pub fn next_preset(&mut self) -> Option<&Path> {
        if self.items.is_empty() {
            return None;
        }
        if self.cursor >= self.order.len() {
            let last = self.order.last().copied();
            self.reshuffle(last);
        }
        let index = self.order[self.cursor];
        self.cursor += 1;
        Some(self.items[index].as_path())
    }

    fn next_random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Fisher-Yates reshuffle; when `avoid_first` is given and there is more
    /// than one preset, that index is kept out of the first slot.
    fn reshuffle(&mut self, avoid_first: Option<usize>) {
        let n = self.items.len();
        self.order = (0..n).collect();
        for i in (1..n).rev() {
            let j = (self.next_random() % (i as u64 + 1)) as usize;
            self.order.swap(i, j);
        }
        if n > 1 && avoid_first == Some(self.order[0]) {
            self.order.swap(0, n - 1);
        }
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn items(n: usize) -> Vec<PathBuf> {
        (0..n)
            .map(|i| PathBuf::from(format!("/p/{i}.milk")))
            .collect()
    }

    #[test]
    fn empty_playlist_yields_nothing() {
        let mut pl = PresetPlaylist::new(Vec::new(), 1);
        assert!(pl.is_empty());
        assert!(pl.next_preset().is_none());
    }

    #[test]
    fn every_preset_is_visited_exactly_once_per_cycle() {
        let mut pl = PresetPlaylist::new(items(50), 42);
        let cycle: Vec<PathBuf> = (0..50)
            .map(|_| pl.next_preset().unwrap().to_path_buf())
            .collect();
        assert_eq!(cycle.iter().collect::<HashSet<_>>().len(), 50);
    }

    #[test]
    fn consecutive_cycles_never_repeat_across_the_boundary() {
        for seed in 0..200u64 {
            let mut pl = PresetPlaylist::new(items(3), seed);
            let mut prev: Option<PathBuf> = None;
            for _ in 0..30 {
                let next = pl.next_preset().unwrap().to_path_buf();
                assert_ne!(prev.as_ref(), Some(&next), "seed {seed}");
                prev = Some(next);
            }
        }
    }

    #[test]
    fn single_preset_repeats_instead_of_hanging() {
        let mut pl = PresetPlaylist::new(items(1), 7);
        assert_eq!(pl.next_preset(), Some(Path::new("/p/0.milk")));
        assert_eq!(pl.next_preset(), Some(Path::new("/p/0.milk")));
    }

    #[test]
    fn rotation_is_shuffled_not_sorted() {
        let mut pl = PresetPlaylist::new(items(20), 123);
        let got: Vec<PathBuf> = (0..20)
            .map(|_| pl.next_preset().unwrap().to_path_buf())
            .collect();
        assert_ne!(got, items(20));
    }
}
