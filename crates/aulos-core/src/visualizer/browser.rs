// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Preset browser list model: search filtering and category-grouped row
//! flattening, independent of how the rows are drawn.

use super::presets::PresetEntry;
use std::path::Path;

/// One line of the flattened, filtered list: a category heading or a
/// preset, both identified by their index into the full entries slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Heading for the category of `entries[i]`.
    Category(usize),
    Preset(usize),
}

/// Whether `entry` matches every whitespace-separated word of `query`
/// (case-insensitive substring of `"category name"`). An empty/blank query
/// matches everything.
fn matches_query(entry: &PresetEntry, words: &[String]) -> bool {
    words.iter().all(|w| entry.search_key.contains(w.as_str()))
}

/// Flattens `entries` (already sorted by category) into display rows,
/// dropping non-matching presets and emitting a heading whenever the
/// category changes between two *shown* presets.
pub fn build_rows(entries: &[PresetEntry], query: &str) -> Vec<Row> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut rows = Vec::new();
    let mut last_category: Option<&str> = None;
    for (i, entry) in entries.iter().enumerate() {
        if !matches_query(entry, &words) {
            continue;
        }
        if last_category != Some(entry.category.as_str()) {
            rows.push(Row::Category(i));
            last_category = Some(entry.category.as_str());
        }
        rows.push(Row::Preset(i));
    }
    rows
}

/// Position in `rows` of the preset whose *path* is `current` — never its
/// name: the same stem exists in several category directories.
pub fn current_row(rows: &[Row], entries: &[PresetEntry], current: Option<&Path>) -> Option<usize> {
    let current = current?;
    rows.iter()
        .position(|row| matches!(row, Row::Preset(i) if entries[*i].path == current))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(category: &str, name: &str) -> PresetEntry {
        PresetEntry::from_path(PathBuf::from(format!(
            "/presets/presets_{category}/{name}.milk"
        )))
    }

    /// Same stem in three categories, like the stock MilkDrop preset pack.
    fn sample() -> Vec<PresetEntry> {
        vec![
            entry("milkdrop", "Geiss - Eddies 2"),
            entry("milkdrop", "Rovastar - Fractal"),
            entry("stock", "Geiss - Eddies 2"),
            entry("tryptonaut", "Geiss - Eddies 2"),
            entry("tryptonaut", "Zylot - Aurora"),
        ]
    }

    fn presets(rows: &[Row]) -> Vec<usize> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Preset(i) => Some(*i),
                Row::Category(_) => None,
            })
            .collect()
    }

    #[test]
    fn empty_query_lists_everything_with_one_heading_per_category() {
        let entries = sample();
        let rows = build_rows(&entries, "  ");
        assert_eq!(presets(&rows), vec![0, 1, 2, 3, 4]);
        let headings: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Category(i) => Some(*i),
                Row::Preset(_) => None,
            })
            .collect();
        assert_eq!(headings, vec![0, 2, 3]);
        assert_eq!(rows[0], Row::Category(0));
        assert_eq!(rows[1], Row::Preset(0));
    }

    #[test]
    fn query_is_case_insensitive_and_every_word_must_match() {
        let entries = sample();
        assert_eq!(presets(&build_rows(&entries, "GEISS")), vec![0, 2, 3]);
        // Words may appear in any order and across category + name.
        assert_eq!(presets(&build_rows(&entries, "eddies stock")), vec![2]);
        assert_eq!(presets(&build_rows(&entries, "stock eddies")), vec![2]);
        assert!(build_rows(&entries, "geiss fractal").is_empty());
    }

    #[test]
    fn category_names_match_and_headings_follow_only_shown_presets() {
        let entries = sample();
        let rows = build_rows(&entries, "tryptonaut");
        assert_eq!(presets(&rows), vec![3, 4]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], Row::Category(3));

        // A match that skips a category's first preset still gets a heading.
        let rows = build_rows(&entries, "aurora");
        assert_eq!(rows, vec![Row::Category(4), Row::Preset(4)]);
    }

    #[test]
    fn current_row_matches_by_path_not_by_name() {
        let entries = sample();
        let rows = build_rows(&entries, "");
        // entries[2] and entries[3] share the stem of entries[0].
        let playing = entries[3].path.clone();
        let pos = current_row(&rows, &entries, Some(&playing)).unwrap();
        assert_eq!(rows[pos], Row::Preset(3));
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, Row::Preset(i) if entries[*i].path == playing))
                .count(),
            1
        );
        assert_eq!(current_row(&rows, &entries, None), None);
        assert_eq!(
            current_row(&rows, &entries, Some(Path::new("/elsewhere.milk"))),
            None
        );
    }

    #[test]
    fn current_row_is_none_when_the_playing_preset_is_filtered_out() {
        let entries = sample();
        let rows = build_rows(&entries, "zylot");
        assert_eq!(current_row(&rows, &entries, Some(&entries[0].path)), None);
    }
}
