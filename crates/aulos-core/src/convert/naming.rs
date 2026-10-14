// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Output file naming for the converter.
//!
//! A *pattern* such as `{track:02} {title}` or
//! `{album_artist}/{year} - {album}/{track:02} {title}` is expanded from a
//! source's tags into a relative path (without extension): `/` separates
//! folders, everything else is literal text plus `{token}` placeholders.
//!
//! Tokens: `{artist}`, `{album_artist}`, `{album}`, `{title}`, `{track}`,
//! `{disc}`, `{year}`, `{genre}` and `{filename}` (the source file's stem).
//! `{track}` and `{disc}` accept a zero-padding width, e.g. `{track:02}`.
//! A missing tag expands to nothing — except `{title}`, which falls back to
//! the source file's stem, and `{album_artist}`, which falls back to the
//! artist — and an unknown `{token}` is kept as literal text. Every path
//! segment is sanitized (see [`sanitize_filename`]) and empty folder
//! segments are dropped, so a tag value can never escape the output folder.

use crate::library::tagio::{TagSet, keys, split_position};

/// Pattern used when none is configured.
pub const DEFAULT_PATTERN: &str = "{track:02} {title}";

/// The tag values a pattern can reference.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameFields {
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub title: String,
    pub genre: String,
    pub year: String,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    /// The source file's name without its extension.
    pub stem: String,
}

impl NameFields {
    /// Fields read from `tags` (`None` for an unreadable source) of the file
    /// whose stem is `stem`.
    #[must_use]
    pub fn from_tags(tags: Option<&TagSet>, stem: &str) -> Self {
        let mut fields = Self {
            stem: stem.to_owned(),
            ..Self::default()
        };
        let Some(tags) = tags else {
            return fields;
        };
        let text = |key: &str| tags.get(key).unwrap_or_default().trim().to_owned();
        fields.artist = text(keys::ARTIST);
        fields.album_artist = text(keys::ALBUMARTIST);
        fields.album = text(keys::ALBUM);
        fields.title = text(keys::TITLE);
        fields.genre = text(keys::GENRE);
        fields.year = year_of(tags.get(keys::DATE).unwrap_or_default());
        fields.track = tags
            .get(keys::TRACKNUMBER)
            .and_then(|t| split_position(t).0)
            .filter(|&n| n > 0);
        fields.disc = tags
            .get(keys::DISCNUMBER)
            .and_then(|t| split_position(t).0)
            .filter(|&n| n > 0);
        fields
    }

    fn value(&self, token: &str, width: usize) -> Option<String> {
        let number = |n: Option<u32>| n.map(|n| format!("{n:0width$}")).unwrap_or_default();
        Some(match token {
            "artist" => self.artist.clone(),
            "album_artist" if self.album_artist.is_empty() => self.artist.clone(),
            "album_artist" => self.album_artist.clone(),
            "album" => self.album.clone(),
            "title" if self.title.is_empty() => self.stem.clone(),
            "title" => self.title.clone(),
            "genre" => self.genre.clone(),
            "year" => self.year.clone(),
            "track" => number(self.track),
            "disc" => number(self.disc),
            "filename" => self.stem.clone(),
            _ => return None,
        })
    }
}

/// The leading four-digit year of a tag date (`2024`, `2024-05-01`, …), or
/// nothing when it doesn't start with one.
fn year_of(date: &str) -> String {
    let digits: String = date
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.len() >= 4 {
        digits[..4].to_owned()
    } else {
        String::new()
    }
}

/// Expands `pattern` into sanitized path segments: the folders followed by
/// the file stem (no extension). Never empty — an empty result falls back
/// to the source's stem, then to `"track"`.
#[must_use]
pub fn expand(pattern: &str, fields: &NameFields) -> Vec<String> {
    let mut segments: Vec<String> = pattern
        .split('/')
        .filter_map(|segment| clean_segment(&expand_segment(segment, fields)))
        .collect();
    if segments.is_empty() {
        segments.push(sanitize_filename(&fields.stem));
    }
    segments
}

/// Expands the `{token}`s of one path segment.
fn expand_segment(segment: &str, fields: &NameFields) -> String {
    let mut out = String::new();
    let mut rest = segment;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            // Unterminated brace: literal.
            out.push_str(&rest[open..]);
            return out;
        };
        let body = &after[..close];
        let (name, spec) = body.split_once(':').unwrap_or((body, ""));
        let width = spec
            .trim_start_matches('0')
            .parse::<usize>()
            .ok()
            .filter(|_| spec.starts_with('0'))
            .unwrap_or(0);
        match fields.value(name.trim(), width) {
            Some(value) => out.push_str(&value),
            None => {
                out.push('{');
                out.push_str(body);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Sanitizes one expanded segment; `None` when nothing printable remains.
/// Leading/trailing spaces and dashes are trimmed first so a missing tag
/// next to a separator (`" - Title"`) doesn't leave the separator behind.
fn clean_segment(segment: &str) -> Option<String> {
    let trimmed = segment.trim_matches(|c: char| c.is_whitespace() || c == '-');
    sanitize_segment(trimmed)
}

/// Strips characters that are awkward, invalid, or path-traversing in
/// filenames (path separators, NUL, other control characters); strips a
/// leading `-` that could be misread as a flag by anything that later
/// shells out on the name; trims surrounding `.`/whitespace so a name
/// that's entirely dots can't resolve to `.`/`..` as a path component; and
/// caps the result's byte length so a pathological tag can't exceed
/// common filesystem name limits. `None` when nothing is left.
fn sanitize_segment(name: &str) -> Option<String> {
    const MAX_BYTES: usize = 150;

    let mut cleaned = String::new();
    for c in name.trim().chars() {
        if c == '\0' || c.is_control() {
            continue;
        }
        let c = if "/\\:*?\"<>|".contains(c) { '_' } else { c };
        if cleaned.len() + c.len_utf8() > MAX_BYTES {
            break;
        }
        cleaned.push(c);
    }

    while cleaned.starts_with('-') {
        cleaned.remove(0);
    }
    let cleaned = cleaned.trim_matches('.').trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

/// [`sanitize_segment`], with `"track"` for a name that sanitizes to nothing.
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    sanitize_segment(name).unwrap_or_else(|| "track".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> NameFields {
        NameFields {
            artist: "Mira Okafor".into(),
            album_artist: String::new(),
            album: "Low Tide Tapes".into(),
            title: "Pier Song".into(),
            genre: "Ambient".into(),
            year: "2024".into(),
            track: Some(3),
            disc: Some(1),
            stem: "03 - source".into(),
        }
    }

    #[test]
    fn default_pattern_pads_the_track_number() {
        assert_eq!(expand(DEFAULT_PATTERN, &fields()), ["03 Pier Song"]);
    }

    #[test]
    fn slashes_make_folders_and_tokens_fill_each_segment() {
        assert_eq!(
            expand(
                "{album_artist}/{year} - {album}/{track:02} {title}",
                &fields()
            ),
            ["Mira Okafor", "2024 - Low Tide Tapes", "03 Pier Song"]
        );
    }

    #[test]
    fn unpadded_and_wide_track_numbers() {
        assert_eq!(expand("{track}", &fields()), ["3"]);
        assert_eq!(expand("{track:03}", &fields()), ["003"]);
        assert_eq!(expand("{disc}-{track:02}", &fields()), ["1-03"]);
    }

    #[test]
    fn missing_title_falls_back_to_the_file_stem() {
        let mut f = fields();
        f.title.clear();
        assert_eq!(expand("{title}", &f), ["03 - source"]);
    }

    #[test]
    fn untagged_source_yields_the_stem() {
        let f = NameFields::from_tags(None, "My Song");
        assert_eq!(expand(DEFAULT_PATTERN, &f), ["My Song"]);
        assert_eq!(expand("{album}", &f), ["My Song"]);
    }

    #[test]
    fn missing_values_do_not_leave_separators_behind() {
        let mut f = fields();
        f.track = None;
        assert_eq!(expand("{track} - {title}", &f), ["Pier Song"]);
        f.year.clear();
        assert_eq!(
            expand("{album}/{year}-{title}", &f),
            ["Low Tide Tapes", "Pier Song"]
        );
    }

    #[test]
    fn empty_folder_segments_are_dropped() {
        let mut f = fields();
        f.album.clear();
        assert_eq!(expand("{album}/{title}", &f), ["Pier Song"]);
        assert_eq!(expand("//{title}//", &f), ["Pier Song"]);
    }

    #[test]
    fn album_artist_falls_back_to_the_artist() {
        assert_eq!(expand("{album_artist}", &fields()), ["Mira Okafor"]);
        let mut f = fields();
        f.album_artist = "Various Artists".into();
        assert_eq!(expand("{album_artist}", &f), ["Various Artists"]);
    }

    #[test]
    fn unknown_tokens_and_stray_braces_are_literal() {
        assert_eq!(expand("{bogus} {title}", &fields()), ["{bogus} Pier Song"]);
        assert_eq!(expand("{title} {", &fields()), ["Pier Song {"]);
    }

    #[test]
    fn tag_values_cannot_traverse_or_inject_folders() {
        let mut f = fields();
        f.title = "../../etc/passwd".into();
        f.album = "..".into();
        let segments = expand("{album}/{title}", &f);
        assert_eq!(segments.len(), 1, "{segments:?}");
        assert!(!segments[0].contains('/'));
        assert!(segments.iter().all(|s| s != ".." && s != "."));
    }

    #[test]
    fn filename_token_is_the_source_stem() {
        assert_eq!(expand("{filename}", &fields()), ["03 - source"]);
    }

    #[test]
    fn fields_are_read_from_tags() {
        let mut tags = TagSet::new();
        tags.set_one(keys::TITLE, " Salt Glass ");
        tags.set_one(keys::ARTIST, "Mira");
        tags.set_one(keys::ALBUM, "Tapes");
        tags.set_one(keys::DATE, "2019-05-01");
        tags.set_one(keys::TRACKNUMBER, "6/11");
        tags.set_one(keys::DISCNUMBER, "2");
        let f = NameFields::from_tags(Some(&tags), "stem");
        assert_eq!(f.title, "Salt Glass");
        assert_eq!(f.year, "2019");
        assert_eq!(f.track, Some(6));
        assert_eq!(f.disc, Some(2));
        assert_eq!(
            expand("{year}/{track:02} {title}", &f),
            ["2019", "06 Salt Glass"]
        );
    }

    #[test]
    fn short_or_non_numeric_dates_give_no_year() {
        assert_eq!(year_of("99"), "");
        assert_eq!(year_of("n/a"), "");
        assert_eq!(year_of("1999"), "1999");
    }

    #[test]
    fn sanitize_filename_blocks_traversal_and_dashes_and_caps_length() {
        assert_eq!(sanitize_filename(".."), "track");
        assert_eq!(sanitize_filename("..."), "track");
        assert!(!sanitize_filename("../../.bashrc").contains('/'));
        assert!(!sanitize_filename("-rf --no-preserve-root").starts_with('-'));
        assert_eq!(sanitize_filename("a\0b").find('\0'), None);

        let long = "x".repeat(1000);
        assert!(sanitize_filename(&long).len() <= 150);
    }
}
