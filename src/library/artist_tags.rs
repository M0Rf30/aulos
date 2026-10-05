// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Multi-artist tag splitting.
//!
//! Library scanners store whatever the file's artist tag says, verbatim —
//! `"Daft Punk feat. Pharrell Williams"`, `"Simon & Garfunkel"`,
//! `"A; B; C"`. Treated as one opaque string, each variant becomes its own
//! `Artist` entry, fragmenting a collaborator's discography across
//! near-duplicate names. This module splits such tags into individual
//! artist names at *aggregation* time (when albums/tracks are grouped into
//! `Artist` entries for display) rather than at scan time, so the raw tag
//! stored in the database is never touched and toggling the setting never
//! requires a rescan.

/// Default delimiters tried when splitting a raw artist tag, in
/// precedence order.
///
/// Ordered longest/most-specific first so e.g. `" feat. "` is preferred
/// over a shorter delimiter that happens to start matching at the same
/// position. The last two entries — `", "` and `"/"` — are the riskiest:
/// they also occur inside legitimate single-artist names (`"Earth, Wind &
/// Fire"`, `"AC/DC"`), so they're placed last (lowest precedence) and are
/// exactly why this list is user-configurable and editable rather than
/// hard-coded.
pub const DEFAULT_DELIMITERS: &[&str] = &[
    "; ",
    ";",
    " feat. ",
    " feat ",
    " ft. ",
    " ft ",
    " featuring ",
    " & ",
    " / ",
    "/",
    " vs. ",
    " vs ",
    " with ",
    ", ",
];

/// Split a raw artist tag into individual artist names.
///
/// At every position, tries every delimiter in `delimiters` and picks the
/// earliest match; ties (multiple delimiters matching at the same start
/// position) are broken by preferring the longest one, so a more specific
/// separator (`" featuring "`) wins over a shorter one that happens to be
/// a prefix of it (`" f"`). Parts are trimmed and empty parts dropped;
/// duplicates are removed case-insensitively (ASCII fold) while keeping
/// the first spelling encountered and the original left-to-right order.
///
/// Returns borrowed slices of `raw` — this runs over every track on every
/// library load/rebuild, so it must not allocate a `String` per part.
/// When `delimiters` is empty or none match, returns a single-element vec
/// containing the trimmed input, or an empty vec if `raw` is blank.
pub fn split<'a>(raw: &'a str, delimiters: &[String]) -> Vec<&'a str> {
    let mut raw_parts: Vec<&'a str> = Vec::new();
    let mut rest = raw;

    loop {
        // Find the earliest delimiter match; among matches tied on start
        // position, prefer the longest delimiter.
        let mut best: Option<(usize, usize)> = None; // (start, len)
        for delim in delimiters {
            if delim.is_empty() {
                continue;
            }
            if let Some(start) = rest.find(delim.as_str()) {
                let len = delim.len();
                let better = match best {
                    Some((best_start, best_len)) => {
                        start < best_start || (start == best_start && len > best_len)
                    }
                    None => true,
                };
                if better {
                    best = Some((start, len));
                }
            }
        }

        match best {
            Some((start, len)) => {
                raw_parts.push(&rest[..start]);
                rest = &rest[start + len..];
            }
            None => {
                raw_parts.push(rest);
                break;
            }
        }
    }

    dedup_trimmed(raw_parts)
}

/// Trim each part, drop empties, and de-duplicate case-insensitively
/// (ASCII fold) while keeping the first spelling and original order.
///
/// Uses a pairwise `eq_ignore_ascii_case` scan instead of a lowercased
/// hash set so no owned `String` is allocated per part.
fn dedup_trimmed(parts: Vec<&str>) -> Vec<&str> {
    let mut result: Vec<&str> = Vec::with_capacity(parts.len());
    for part in parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !result.iter().any(|seen| seen.eq_ignore_ascii_case(trimmed)) {
            result.push(trimmed);
        }
    }
    result
}

/// The first split part of a raw artist tag — the artist an album should
/// be attributed to for the Albums view. Splitting widens the *artist
/// index* only; an album keeps one primary attribution.
pub fn primary<'a>(raw: &'a str, delimiters: &[String]) -> &'a str {
    split(raw, delimiters)
        .into_iter()
        .next()
        .unwrap_or_else(|| raw.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delims(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    fn default_delims() -> Vec<String> {
        delims(DEFAULT_DELIMITERS)
    }

    #[test]
    fn splits_on_feat() {
        assert_eq!(split("A feat. B", &default_delims()), vec!["A", "B"]);
    }

    #[test]
    fn splits_on_semicolon_multiple_parts() {
        assert_eq!(split("A; B; C", &default_delims()), vec!["A", "B", "C"]);
    }

    #[test]
    fn ampersand_split_is_configurable() {
        let with_amp = delims(&[" & "]);
        assert_eq!(split("A & B", &with_amp), vec!["A", "B"]);

        let without_amp = delims(&["; "]);
        assert_eq!(split("A & B", &without_amp), vec!["A & B"]);
    }

    #[test]
    fn duplicate_parts_dedup_case_insensitively_keeping_first_spelling() {
        let with_slash = delims(&[" / "]);
        assert_eq!(split("A / a", &with_slash), vec!["A"]);
    }

    #[test]
    fn empty_input_returns_empty_vec() {
        // Documented contract: blank input (after trimming) yields an
        // empty vec, not a vec containing one empty string.
        assert_eq!(split("", &default_delims()), Vec::<&str>::new());
        assert_eq!(split("   ", &default_delims()), Vec::<&str>::new());
    }

    #[test]
    fn substring_that_is_not_the_delimiter_is_left_intact() {
        // "Defeat Nothing" contains "feat" as a substring but never as the
        // space-delimited token " feat " or " feat. ", so it must not split.
        assert_eq!(
            split("Defeat Nothing", &default_delims()),
            vec!["Defeat Nothing"]
        );
    }

    #[test]
    fn longest_match_wins_over_shorter_prefix_delimiter() {
        // " f" is a prefix-position match inside " featuring " at the same
        // start index; the longer, more specific delimiter must win so
        // "A featuring B" splits into ["A", "B"], not ["A", "eaturing B"].
        let tricky = delims(&[" f", " featuring "]);
        assert_eq!(split("A featuring B", &tricky), vec!["A", "B"]);
    }

    #[test]
    fn no_delimiters_returns_single_trimmed_part() {
        assert_eq!(split("  Daft Punk  ", &[]), vec!["Daft Punk"]);
    }

    #[test]
    fn primary_returns_first_split_part() {
        assert_eq!(
            primary("Daft Punk feat. Pharrell", &default_delims()),
            "Daft Punk"
        );
        assert_eq!(primary("Solo Artist", &default_delims()), "Solo Artist");
    }
}
