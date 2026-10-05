// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Directory-hierarchy browse view.
//!
//! Complements the tag-driven views (Albums, Artists, Genres) for
//! collections whose metadata is sparse or wrong: browses the library by
//! its on-disk (or on-provider) folder structure instead of by tags. The
//! tree is built once from the in-memory track list — never rescanned, no
//! filesystem I/O, no database query — and stores only track *indices*
//! into the caller's slice, so it stays cheap to rebuild on every library
//! reload.

use crate::fl;
use crate::library::Track;
use crate::views::common;
use crate::views::list_row_button_class;
use cosmic::iced::alignment::Horizontal;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    pub fn build(tracks: &[Track]) -> FolderTree {
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
}

/// Messages from the folder browse view.
#[derive(Debug, Clone)]
pub enum FolderMessage {
    /// Descend into a child directory.
    Open(PathBuf),
    /// Move up to the parent of the current directory.
    Up,
    /// Jump to a breadcrumb segment by index.
    GoTo(usize),
    /// Play a specific track (index into `all_tracks`).
    PlayTrack(usize),
    /// Play every track under the current directory, recursively.
    PlayFolder,
    /// Queue every track under the current directory, recursively.
    QueueFolder,
    /// Toggle favorite status for a track (by track ID string).
    ToggleFavorite(String),
    /// Set rating (1-5) for a track. Pass 0 to clear.
    SetRating(String, u8),
}

/// Render the folder browse view: breadcrumb bar and folder actions, then
/// subfolder rows, then the tracks stored directly in the current
/// directory.
pub fn folder_view<'a>(
    state: &'a FolderState,
    tracks: &'a [Track],
    current_track: Option<&'a Track>,
) -> cosmic::Element<'a, FolderMessage> {
    if tracks.is_empty() {
        return common::empty_state(
            "folder-symbolic",
            fl!("no-folders"),
            fl!("folders-empty-hint"),
        );
    }

    let tree = state.tree();
    let current = state.current();
    let children = tree.child_dirs(current);
    let direct = tree.direct_tracks(current);
    let has_any_here = !children.is_empty() || !direct.is_empty();

    let mut crumb_row = widget::Row::new().spacing(4).align_y(Alignment::Center);
    if !current.as_os_str().is_empty() {
        crumb_row = crumb_row.push(widget::tooltip(
            widget::button::icon(widget::icon::from_name("go-up-symbolic").size(16))
                .on_press(FolderMessage::Up),
            widget::text::caption(fl!("folders-up")),
            widget::tooltip::Position::Top,
        ));
    }
    let breadcrumbs = state.breadcrumbs();
    let last = breadcrumbs.len().saturating_sub(1);
    for (index, (_, label)) in breadcrumbs.into_iter().enumerate() {
        if index > 0 {
            crumb_row = crumb_row.push(common::cell_caption("/"));
        }
        crumb_row = crumb_row.push(
            widget::button::text(label)
                .on_press_maybe((index != last).then_some(FolderMessage::GoTo(index))),
        );
    }

    let actions_row = widget::Row::new()
        .push(
            widget::button::suggested(fl!("play-folder"))
                .on_press_maybe(has_any_here.then_some(FolderMessage::PlayFolder)),
        )
        .push(widget::tooltip(
            widget::button::icon(widget::icon::from_name("list-add-symbolic").size(16))
                .on_press_maybe(has_any_here.then_some(FolderMessage::QueueFolder)),
            widget::text::caption(fl!("queue-folder-tooltip")),
            widget::tooltip::Position::Top,
        ))
        .spacing(8);

    let header = widget::Column::new()
        .push(crumb_row)
        .push(actions_row)
        .spacing(8)
        .padding(16);

    let mut body = widget::Column::new().spacing(2);

    for child in children {
        let count = tree.track_count_in(child, true);
        let name = child
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| child.to_string_lossy().into_owned());

        let row = widget::button::custom(
            widget::Row::new()
                .push(widget::icon::from_name("folder-symbolic").size(24))
                .push(common::clipped_cell(common::cell_text(name).into()))
                .push(common::cell_caption(folder_track_count_label(count)))
                .spacing(14)
                .align_y(Alignment::Center)
                .padding([10, 8]),
        )
        .on_press(FolderMessage::Open(child.clone()))
        .width(Length::Fill)
        .class(list_row_button_class(false));

        body = body.push(row);
    }

    for &index in direct {
        if let Some(track) = tracks.get(index) {
            body = body.push(track_row(index, track, current_track));
        }
    }

    if !has_any_here {
        body = body.push(common::empty_state(
            "folder-symbolic",
            fl!("folder-empty"),
            fl!("folder-empty-hint"),
        ));
    }

    widget::scrollable(
        widget::Column::new()
            .push(header)
            .push(widget::divider::horizontal::default())
            .push(widget::container(body).padding(16))
            .spacing(8),
    )
    .height(Length::Fill)
    .into()
}

/// Build a single track row for the current directory's track list.
fn track_row<'a>(
    index: usize,
    track: &'a Track,
    current_track: Option<&'a Track>,
) -> cosmic::Element<'a, FolderMessage> {
    let track_id = track.id.to_string();
    let is_playing = current_track.map(|t| t.id) == Some(track.id);
    let rating_track_id = track_id.clone();

    let title_col = widget::container(common::clipped_cell(
        common::cell_text(track.title.as_str()).into(),
    ))
    .width(Length::FillPortion(4));
    let artist_col = widget::container(common::clipped_cell(
        common::cell_text(track.artist.as_str()).into(),
    ))
    .width(Length::FillPortion(3));

    let row = widget::button::custom(
        widget::Row::new()
            .push(common::cell_text((index + 1).to_string()).width(32))
            .push(title_col)
            .push(artist_col)
            .push(
                widget::container(common::favorite_button(
                    track.is_favorite,
                    FolderMessage::ToggleFavorite(track_id.clone()),
                ))
                .width(40)
                .align_x(Horizontal::Center),
            )
            .push(
                widget::container(common::star_rating(track.rating, move |r| {
                    FolderMessage::SetRating(rating_track_id.clone(), r)
                }))
                .width(100)
                .align_x(Horizontal::Center),
            )
            .push(common::duration_cell(track.duration.as_secs()))
            .spacing(8)
            .width(Length::Fill)
            .align_y(Alignment::Center)
            .padding([4, 8]),
    )
    .on_press(FolderMessage::PlayTrack(index))
    .width(Length::Fill)
    .class(list_row_button_class(is_playing));

    row.into()
}

/// Localized "N tracks" label used on folder rows.
fn folder_track_count_label(count: usize) -> String {
    if count == 1 {
        fl!("folder-track-count-one", count = count.to_string())
    } else {
        fl!("folder-track-count-other", count = count.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// Minimal `Track` carrying only the field this module reads (`path`).
    fn track(path: &str) -> Track {
        Track {
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
        }
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
}
