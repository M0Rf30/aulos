// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Directory-hierarchy index over the library's tracks, for browsing by
//! on-disk (or on-provider) folder structure instead of by tags.
//!
//! Complements the tag-driven views (Albums, Artists, Genres) for
//! collections whose metadata is sparse or wrong. The tree is built once
//! from the in-memory track list — never rescanned, no filesystem I/O, no
//! database query — and stores only track *indices* into the caller's
//! slice, so it stays cheap to rebuild on every library reload.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Track;
use crate::fl;
use crate::format::format_duration_coarse;

/// Tracks inspected per folder while looking for a cached cover.
const COVER_SCAN_BUDGET: usize = 96;

/// One directory's contents in the folder tree.
#[derive(Debug, Clone, Default)]
struct FolderNode {
    /// Immediate subdirectories, sorted for stable display order.
    children: Vec<PathBuf>,
    /// Indices into the track slice `FolderTree::build` was given, for
    /// tracks stored directly in this directory (not its subdirectories).
    track_indices: Vec<usize>,
}

/// Directory-hierarchy index over a track slice.
///
/// Built once per library load with a single pass over the tracks; every
/// navigation (open a child, list children, list tracks) is then an O(1)
/// map lookup, never a rescan. Stores `usize` indices into the caller's
/// slice, never cloned `Track`s — the tree is a pure lookup structure over
/// `all_tracks`, cheap to throw away and rebuild whenever the library
/// changes.
#[derive(Debug, Clone, Default)]
pub struct FolderTree {
    nodes: HashMap<PathBuf, FolderNode>,
}

/// Every directory nests under the empty path, used as a synthetic root.
///
/// `Track::path` is a real filesystem path for local tracks but a
/// provider-relative path or URI for MPD/Subsonic tracks. Both are handled
/// with the same plain string-based `Path::parent()` semantics — no
/// filesystem access, so remote providers form a sensible tree too.
/// Anything whose parent can't be resolved that way (a bare filename, an
/// absolute path's own root, an unparseable URI tail) is grouped directly
/// under this synthetic root instead of being dropped.
fn root() -> PathBuf {
    PathBuf::new()
}

/// `path`'s parent, or the synthetic root when it has none (or an empty
/// one — `Path::parent()` on a single-component path returns `Some("")`).
fn parent_or_root(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => root(),
    }
}

impl FolderTree {
    /// Builds the tree from `tracks` in one pass: each track's directory
    /// (and every ancestor up to the root) becomes a node, and the
    /// track's index is recorded against its immediate directory.
    #[must_use]
    pub fn build(tracks: &[Arc<Track>]) -> FolderTree {
        let mut nodes: HashMap<PathBuf, FolderNode> = HashMap::new();
        nodes.entry(root()).or_default();

        for (index, track) in tracks.iter().enumerate() {
            let dir = parent_or_root(&track.path);
            link_ancestors(&mut nodes, &dir);
            nodes.entry(dir).or_default().track_indices.push(index);
        }

        for node in nodes.values_mut() {
            node.children.sort();
            node.children.dedup();
        }

        FolderTree { nodes }
    }

    /// The tree's synthetic root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        Path::new("")
    }

    /// Whether `dir` is a known directory in this tree.
    #[must_use]
    pub fn contains(&self, dir: &Path) -> bool {
        self.nodes.contains_key(dir)
    }

    /// Whether the tree has never been built — `true` only for a
    /// `Default`-constructed tree that `build` hasn't populated yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Immediate subdirectories of `dir`, sorted; empty if `dir` is
    /// unknown or has none.
    #[must_use]
    pub fn child_dirs(&self, dir: &Path) -> &[PathBuf] {
        self.nodes
            .get(dir)
            .map(|node| node.children.as_slice())
            .unwrap_or(&[])
    }

    /// Indices of tracks stored directly in `dir` (not its subdirectories).
    #[must_use]
    pub fn direct_tracks(&self, dir: &Path) -> &[usize] {
        self.nodes
            .get(dir)
            .map(|node| node.track_indices.as_slice())
            .unwrap_or(&[])
    }

    /// Indices of tracks in `dir`; when `recursive`, also every track
    /// beneath its subdirectories, depth-first (this directory's own
    /// tracks, then each child directory in sorted order) — so playing a
    /// parent directory plays everything below it in path order.
    #[must_use]
    pub fn tracks_in(&self, dir: &Path, recursive: bool) -> Vec<usize> {
        let mut out = Vec::new();
        self.collect_tracks(dir, recursive, &mut out);
        out
    }

    /// Count of tracks in `dir`; when `recursive`, also every track
    /// beneath its subdirectories. Equivalent to
    /// `tracks_in(dir, recursive).len()` but never allocates the index
    /// list — the folder view calls this once per visible subdirectory row
    /// on every render, purely to display a count.
    #[must_use]
    pub fn track_count_in(&self, dir: &Path, recursive: bool) -> usize {
        let Some(node) = self.nodes.get(dir) else {
            return 0;
        };
        let mut count = node.track_indices.len();
        if recursive {
            for child in &node.children {
                count += self.track_count_in(child, recursive);
            }
        }
        count
    }

    fn collect_tracks(&self, dir: &Path, recursive: bool, out: &mut Vec<usize>) {
        let Some(node) = self.nodes.get(dir) else {
            return;
        };
        out.extend_from_slice(&node.track_indices);
        if recursive {
            for child in &node.children {
                self.collect_tracks(child, recursive, out);
            }
        }
    }

    /// Follows a chain of directories that hold no tracks themselves and
    /// have exactly one child, returning the first directory that branches
    /// or holds tracks (or `dir` itself when it already does).
    #[must_use]
    pub fn collapse(&self, dir: &Path) -> PathBuf {
        let mut cursor = dir.to_path_buf();
        while let Some(node) = self.nodes.get(&cursor) {
            match node.children.as_slice() {
                [only] if node.track_indices.is_empty() => cursor = only.clone(),
                _ => break,
            }
        }
        cursor
    }

    /// Visits track indices under `dir` in the same order as `tracks_in`,
    /// stopping early once `visit` returns `false`. Returns `false` if it
    /// was stopped.
    pub fn visit_tracks(
        &self,
        dir: &Path,
        recursive: bool,
        visit: &mut dyn FnMut(usize) -> bool,
    ) -> bool {
        let Some(node) = self.nodes.get(dir) else {
            return true;
        };
        for &index in &node.track_indices {
            if !visit(index) {
                return false;
            }
        }
        if recursive {
            for child in &node.children {
                if !self.visit_tracks(child, recursive, visit) {
                    return false;
                }
            }
        }
        true
    }

    /// Total playing time, in seconds, of every track at or beneath `dir`
    /// (indices that fall outside `tracks` are ignored).
    #[must_use]
    pub fn total_secs_in(&self, dir: &Path, tracks: &[Arc<Track>]) -> u64 {
        let mut total = 0u64;
        self.visit_tracks(dir, true, &mut |i| {
            if let Some(track) = tracks.get(i) {
                total += track.duration.as_secs();
            }
            true
        });
        total
    }

    /// First cached album cover among the tracks under `dir` (depth-first,
    /// bounded by [`COVER_SCAN_BUDGET`] so huge folders stay cheap to
    /// render). `covers` is the caller's cover cache keyed by
    /// [`Track::cover_key`]; the value type is the UI's own handle.
    #[must_use]
    pub fn first_cover<'a, V>(
        &self,
        dir: &Path,
        tracks: &[Arc<Track>],
        covers: &'a HashMap<String, V>,
    ) -> Option<&'a V> {
        if covers.is_empty() {
            return None;
        }
        let mut budget = COVER_SCAN_BUDGET;
        let mut found = None;
        self.visit_tracks(dir, true, &mut |i| {
            if let Some(track) = tracks.get(i)
                && !track.album.is_empty()
                && let Some(handle) = covers.get(&track.cover_key())
            {
                found = Some(handle);
                return false;
            }
            budget -= 1;
            budget > 0
        });
        found
    }
}

/// Registers `dir` and every ancestor up to the root as tree nodes, and
/// links each parent to its immediate child.
///
/// Called once per track in `FolderTree::build`, so directories shared by
/// many tracks get visited (and their child link pushed) repeatedly; that's
/// deliberately cheap and left for `build`'s post-pass `sort`+`dedup`
/// rather than tracked here.
fn link_ancestors(nodes: &mut HashMap<PathBuf, FolderNode>, dir: &Path) {
    nodes.entry(dir.to_path_buf()).or_default();
    if dir.as_os_str().is_empty() {
        return;
    }
    let parent = parent_or_root(dir);
    if parent.as_os_str() == dir.as_os_str() {
        return;
    }
    nodes
        .entry(parent.clone())
        .or_default()
        .children
        .push(dir.to_path_buf());
    link_ancestors(nodes, &parent);
}

/// Current browse position within a `FolderTree`.
///
/// The breadcrumb trail is *not* stored separately — it's just `current`'s
/// path components down to the tree root, recomputed on demand by
/// `breadcrumbs()` — so there is exactly one source of truth for "where am
/// I" and it can never drift out of sync with `current`.
#[derive(Debug, Clone, Default)]
pub struct FolderState {
    tree: FolderTree,
    current: PathBuf,
}

impl FolderState {
    /// Installs a freshly built tree and resets the browse position to its
    /// root. Called whenever the library (re)loads.
    pub fn set_tree(&mut self, tree: FolderTree) {
        self.current = tree.root().to_path_buf();
        self.tree = tree;
    }

    /// The tree backing this state.
    #[must_use]
    pub fn tree(&self) -> &FolderTree {
        &self.tree
    }

    /// The directory currently being browsed.
    #[must_use]
    pub fn current(&self) -> &Path {
        &self.current
    }

    /// Whether `set_tree` has ever installed a built tree.
    #[must_use]
    pub fn is_populated(&self) -> bool {
        !self.tree.is_empty()
    }

    /// Path segments from the tree root down to `current`, each paired
    /// with its display label — root first, `current` last. Drives the
    /// breadcrumb bar.
    #[must_use]
    pub fn breadcrumbs(&self) -> Vec<(PathBuf, String)> {
        let mut segments = Vec::new();
        let mut cursor = self.current.clone();
        loop {
            let is_root = cursor.as_os_str().is_empty();
            let label = if is_root {
                fl!("folders-root")
            } else {
                cursor
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| cursor.to_string_lossy().into_owned())
            };
            segments.push((cursor.clone(), label));
            if is_root {
                break;
            }
            cursor = parent_or_root(&cursor);
        }
        segments.reverse();
        segments
    }

    /// Descends into `dir`, if it's a directory this tree actually knows
    /// about.
    pub fn open(&mut self, dir: PathBuf) {
        if self.tree.contains(&dir) {
            self.current = dir;
        }
    }

    /// Moves up to the parent of `current`; a no-op at the root.
    pub fn up(&mut self) {
        self.current = parent_or_root(&self.current);
    }

    /// Jumps to the breadcrumb segment at `index` (see `breadcrumbs`).
    pub fn go_to(&mut self, index: usize) {
        if let Some((path, _)) = self.breadcrumbs().get(index) {
            self.current = path.clone();
        }
    }

    /// The folder browsing starts in: the root with any chain of
    /// single-child, track-less directories collapsed away, so an
    /// absolute-path library opens at its music folder rather than `/`.
    #[must_use]
    pub fn base(&self) -> PathBuf {
        self.tree.collapse(Path::new(""))
    }

    /// The directory the view should actually present: `current`, except
    /// that a position above `base()` (the freshly reset root, or a
    /// collapsed ancestor) is shown as `base()`.
    #[must_use]
    pub fn effective_current(&self) -> PathBuf {
        let base = self.base();
        if base != self.current && base.starts_with(&self.current) {
            base
        } else {
            self.current.clone()
        }
    }

    /// Breadcrumb trail for the view: from `base()` (labelled with its own
    /// folder name, or the library label when it has none) down to
    /// `effective_current()`. Each entry carries the directory to open.
    #[must_use]
    pub fn visible_breadcrumbs(&self) -> Vec<(PathBuf, String)> {
        let base = self.base();
        let mut cursor = self.effective_current();
        let mut out = Vec::new();
        loop {
            let is_base = cursor == base;
            let name = cursor
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            let label = match name {
                Some(name) => name,
                None if is_base || cursor.as_os_str().is_empty() => fl!("folders-root"),
                None => cursor.to_string_lossy().into_owned(),
            };
            out.push((cursor.clone(), label));
            if is_base || cursor.as_os_str().is_empty() {
                break;
            }
            cursor = parent_or_root(&cursor);
        }
        out.reverse();
        out
    }
}

/// Display name for the chain from `parent` down to `end`
/// (`"home / user / Music"`), dropping the filesystem root component unless
/// it is all there is.
#[must_use]
pub fn chain_label(parent: &Path, end: &Path) -> String {
    let relative = end.strip_prefix(parent).unwrap_or(end);
    let names: Vec<String> = relative
        .components()
        .filter(|c| !matches!(c, std::path::Component::RootDir))
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if names.is_empty() {
        end.to_string_lossy().into_owned()
    } else {
        names.join(" / ")
    }
}

/// Localized "N tracks" label used on folder tiles and the header.
#[must_use]
pub fn track_count_label(count: usize) -> String {
    if count == 1 {
        fl!("folder-track-count-one", count = count.to_string())
    } else {
        fl!("folder-track-count-other", count = count.to_string())
    }
}

/// Localized "N folders" label.
#[must_use]
pub fn subfolder_count_label(count: usize) -> String {
    if count == 1 {
        fl!("folder-subfolder-count-one", count = count.to_string())
    } else {
        fl!("folder-subfolder-count-other", count = count.to_string())
    }
}

/// One-line folder summary: "N folders · M tracks · 1h 20m". The folder
/// count is omitted when there are no subfolders and the duration when it
/// is zero (the tiles pass `0` to show counts only).
#[must_use]
pub fn summary(subfolders: usize, tracks: usize, total_secs: u64) -> String {
    let mut parts: Vec<String> = Vec::new();
    if subfolders > 0 {
        parts.push(subfolder_count_label(subfolders));
    }
    parts.push(track_count_label(tracks));
    if total_secs > 0 {
        parts.push(format_duration_coarse(total_secs));
    }
    parts.join(" \u{b7} ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// Minimal `Track` carrying only the field this module reads (`path`).
    fn track(path: &str) -> Arc<Track> {
        Arc::new(Track {
            id: 0,
            path: PathBuf::from(path),
            title: String::new(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 0,
            duration: Duration::ZERO,
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from("test"),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
            bit_depth: 0,
            release_type: String::new(),
            added_at: 0,
            favorited_at: 0,
        })
    }

    #[test]
    fn direct_tracks_are_recorded_against_their_own_directory() {
        let tracks = [
            track("music/a/1.flac"),
            track("music/a/2.flac"),
            track("music/b/3.flac"),
        ];
        let tree = FolderTree::build(&tracks);

        assert_eq!(tree.direct_tracks(Path::new("music/a")), &[0, 1]);
        assert_eq!(tree.direct_tracks(Path::new("music/b")), &[2]);
        // `music` holds no tracks itself, only subdirectories.
        assert!(tree.direct_tracks(Path::new("music")).is_empty());
    }

    #[test]
    fn child_dirs_are_sorted_immediate_children_only() {
        let tracks = [
            track("music/b/1.flac"),
            track("music/a/2.flac"),
            track("music/a/x/3.flac"),
        ];
        let tree = FolderTree::build(&tracks);

        assert_eq!(
            tree.child_dirs(Path::new("music")),
            &[PathBuf::from("music/a"), PathBuf::from("music/b")]
        );
        // Grandchildren belong to their own parent, not to `music`.
        assert_eq!(
            tree.child_dirs(Path::new("music/a")),
            &[PathBuf::from("music/a/x")]
        );
    }

    #[test]
    fn recursive_tracks_in_are_depth_first_in_sorted_path_order() {
        // Deliberately out of path order so the result proves ordering comes
        // from the tree, not from the input slice.
        let tracks = [
            track("music/b/3.flac"),
            track("music/1.flac"),
            track("music/a/2.flac"),
        ];
        let tree = FolderTree::build(&tracks);

        // Own tracks first, then each child directory in sorted order.
        assert_eq!(tree.tracks_in(Path::new("music"), true), vec![1, 2, 0]);
    }

    #[test]
    fn non_recursive_tracks_in_skips_subdirectories() {
        let tracks = [track("music/1.flac"), track("music/a/2.flac")];
        let tree = FolderTree::build(&tracks);

        assert_eq!(tree.tracks_in(Path::new("music"), false), vec![0]);
    }

    #[test]
    fn absolute_paths_stay_reachable_from_the_synthetic_root() {
        let tracks = [track("/home/u/Music/1.flac")];
        let tree = FolderTree::build(&tracks);

        // `/`'s parent is None, so it must nest under the synthetic root;
        // otherwise an absolute-path library would be unreachable when
        // browsing starts at the root.
        assert_eq!(tree.child_dirs(tree.root()), &[PathBuf::from("/")]);
        assert_eq!(tree.tracks_in(tree.root(), true), vec![0]);
    }

    #[test]
    fn a_bare_filename_lands_directly_under_the_root() {
        let tracks = [track("loose.flac")];
        let tree = FolderTree::build(&tracks);

        assert_eq!(tree.direct_tracks(tree.root()), &[0]);
    }

    #[test]
    fn unknown_directory_yields_no_tracks_and_no_children() {
        let tree = FolderTree::build(&[track("music/1.flac")]);

        assert!(tree.tracks_in(Path::new("nope"), true).is_empty());
        assert!(tree.child_dirs(Path::new("nope")).is_empty());
        assert!(!tree.contains(Path::new("nope")));
    }

    #[test]
    fn open_ignores_a_directory_the_tree_does_not_know() {
        let mut state = FolderState::default();
        state.set_tree(FolderTree::build(&[track("music/1.flac")]));

        state.open(PathBuf::from("nope"));
        assert_eq!(state.current(), Path::new(""));

        state.open(PathBuf::from("music"));
        assert_eq!(state.current(), Path::new("music"));
    }

    #[test]
    fn up_at_the_root_is_a_no_op() {
        let mut state = FolderState::default();
        state.set_tree(FolderTree::build(&[track("music/a/1.flac")]));

        state.open(PathBuf::from("music/a"));
        state.up();
        assert_eq!(state.current(), Path::new("music"));
        state.up();
        assert_eq!(state.current(), Path::new(""));
        state.up();
        assert_eq!(state.current(), Path::new(""));
    }

    #[test]
    fn breadcrumbs_run_from_the_root_down_to_the_current_directory() {
        let mut state = FolderState::default();
        state.set_tree(FolderTree::build(&[track("music/a/1.flac")]));
        state.open(PathBuf::from("music/a"));

        let crumbs = state.breadcrumbs();
        let paths: Vec<&Path> = crumbs.iter().map(|(p, _)| p.as_path()).collect();
        assert_eq!(
            paths,
            vec![Path::new(""), Path::new("music"), Path::new("music/a")]
        );
        // Root gets the localized library label; deeper segments use the
        // directory's own file name.
        assert_eq!(crumbs[0].1, fl!("folders-root"));
        assert_eq!(crumbs[1].1, "music");
        assert_eq!(crumbs[2].1, "a");
    }

    #[test]
    fn go_to_jumps_to_the_indexed_breadcrumb_segment() {
        let mut state = FolderState::default();
        state.set_tree(FolderTree::build(&[track("music/a/1.flac")]));
        state.open(PathBuf::from("music/a"));

        state.go_to(1);
        assert_eq!(state.current(), Path::new("music"));
        // Out-of-range index leaves the position untouched.
        state.go_to(99);
        assert_eq!(state.current(), Path::new("music"));
    }

    #[test]
    fn base_collapses_single_child_chain_from_the_root() {
        let mut state = FolderState::default();
        state.set_tree(FolderTree::build(&[
            track("/home/u/Music/a/1.flac"),
            track("/home/u/Music/b/2.flac"),
        ]));

        assert_eq!(state.base(), PathBuf::from("/home/u/Music"));
        // Freshly reset position (the synthetic root) presents as the base.
        assert_eq!(state.effective_current(), PathBuf::from("/home/u/Music"));
        let crumbs = state.visible_breadcrumbs();
        assert_eq!(crumbs.len(), 1);
        assert_eq!(crumbs[0].1, "Music");

        state.open(PathBuf::from("/home/u/Music/a"));
        let labels: Vec<_> = state
            .visible_breadcrumbs()
            .into_iter()
            .map(|c| c.1)
            .collect();
        assert_eq!(labels, vec!["Music".to_string(), "a".to_string()]);
    }
}
