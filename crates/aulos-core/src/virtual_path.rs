// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The paths of local files in the browser, which has none.
//!
//! A music folder the user picks becomes a *root* named after the folder, and
//! each file in it gets the *virtual path* `/<root>/<relative path>`; that is
//! what `Track::path`, `Track::source_uri` and `Config::music_dirs` hold on
//! the web. The names are plain strings, so this module (pure, and tested
//! natively) has nothing to do with the browser; `web_fs` maps them to files.

use std::path::{Component, Path, PathBuf};

/// The root name for a folder the browser reported as `name`: the name with
/// `/` and `\` replaced, and `Music` when nothing is left.
#[must_use]
pub fn root_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "Music".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// `wanted`, or `wanted (2)`, `wanted (3)`, … — the first name for which
/// `is_taken` is false.
#[must_use]
pub fn unique_root_name(wanted: &str, is_taken: impl Fn(&str) -> bool) -> String {
    if !is_taken(wanted) {
        return wanted.to_owned();
    }
    (2..)
        .map(|n| format!("{wanted} ({n})"))
        .find(|name| !is_taken(name))
        .expect("an unbounded range has a free name")
}

/// The virtual path of the root `name`: `/<name>`.
#[must_use]
pub fn root_path(name: &str) -> PathBuf {
    PathBuf::from(format!("/{name}"))
}

/// The virtual path of `relative` (components separated by `/`) under the
/// root `root`.
#[must_use]
pub fn join(root: &str, relative: &str) -> PathBuf {
    let relative = relative.trim_matches('/');
    if relative.is_empty() {
        root_path(root)
    } else {
        PathBuf::from(format!("/{root}/{relative}"))
    }
}

/// A virtual path taken apart: the root's name and the components below it.
/// `None` when `path` is not absolute or names no root.
#[must_use]
pub fn split(path: &Path) -> Option<(String, Vec<String>)> {
    let mut parts = path.components();
    if parts.next()? != Component::RootDir {
        return None;
    }
    let Component::Normal(root) = parts.next()? else {
        return None;
    };
    let mut relative = Vec::new();
    for part in parts {
        match part {
            Component::Normal(name) => relative.push(name.to_str()?.to_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some((root.to_str()?.to_owned(), relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_into_root_and_components() {
        assert_eq!(
            split(Path::new("/Music/Artist/Album/01 Song.flac")),
            Some((
                "Music".to_owned(),
                vec![
                    "Artist".to_owned(),
                    "Album".to_owned(),
                    "01 Song.flac".to_owned()
                ]
            ))
        );
        assert_eq!(
            split(Path::new("/Music")),
            Some(("Music".to_owned(), vec![]))
        );
        assert_eq!(split(Path::new("/")), None);
        assert_eq!(split(Path::new("Music/a.flac")), None);
        assert_eq!(split(Path::new("/Music/../a.flac")), None);
    }

    #[test]
    fn join_is_the_inverse_of_split() {
        let path = join("My Music", "A/B/c.mp3");
        assert_eq!(path, Path::new("/My Music/A/B/c.mp3"));
        assert_eq!(
            split(&path),
            Some((
                "My Music".to_owned(),
                vec!["A".to_owned(), "B".to_owned(), "c.mp3".to_owned()]
            ))
        );
        assert_eq!(join("Music", ""), root_path("Music"));
        assert_eq!(join("Music", "/x.mp3"), Path::new("/Music/x.mp3"));
    }

    #[test]
    fn root_names_are_clean_and_unique() {
        assert_eq!(root_name("Music"), "Music");
        assert_eq!(root_name("a/b"), "a_b");
        assert_eq!(root_name("  "), "Music");
        assert_eq!(root_name(".."), "Music");

        let taken = ["Music", "Music (2)"];
        assert_eq!(
            unique_root_name("Music", |n| taken.contains(&n)),
            "Music (3)"
        );
        assert_eq!(unique_root_name("Jazz", |n| taken.contains(&n)), "Jazz");
    }
}
