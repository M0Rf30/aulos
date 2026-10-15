// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Preset discovery for the lactea visualizer: `.milk` / `.milk2` file
//! scanning, search directories and display metadata.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// One `.milk` / `.milk2` preset file discovered by [`scan_presets`].
///
/// The same stem routinely exists in several category directories (the
/// stock MilkDrop pack ships ~940 duplicated names), so an entry is
/// identified by its full `path`, never by `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetEntry {
    /// File stem (filename without the `.milk`/`.milk2` extension) — display only.
    pub name: String,
    /// The preset's immediate parent directory name, prettified by
    /// stripping the MilkDrop pack convention `presets_` prefix (e.g.
    /// `presets_milkdrop` -> `milkdrop`).
    pub category: String,
    /// Full path: the preset's identity. Passed to `VizCommand::LoadPreset`
    /// when selected and compared with the render thread's current preset
    /// to highlight the active row.
    pub path: PathBuf,
    /// Lowercased `"category name"`, matched against the browser's search
    /// tokens (precomputed: the filter runs on every view rebuild).
    pub search_key: String,
}

impl PresetEntry {
    pub fn from_path(path: PathBuf) -> Self {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let category = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| prettify_category(&s.to_string_lossy()))
            .unwrap_or_default();
        let search_key = format!("{category} {name}").to_lowercase();
        Self {
            name,
            category,
            path,
            search_key,
        }
    }
}

/// Where users put their own presets: `<data dir>/aulos/presets`, i.e.
/// `~/.local/share/aulos/presets` on Linux,
/// `%APPDATA%\aulos\presets` on Windows and
/// `~/Library/Application Support/aulos/presets` on macOS. `None` when the
/// platform has no data directory (the web build).
pub fn user_preset_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|data| data.join("aulos").join("presets"))
}

/// Returns the full ordered list of preset search directories: the
/// caller-supplied `user_dir` (if any) first, then Aulos's own
/// [`user_preset_dir`], the system-wide MilkDrop preset install locations of
/// the platform (distro packages on Linux, Homebrew on macOS), and the
/// lactea/projectM folders under `dirs::data_dir()` (the Flatpak location on
/// Linux). Presets and their textures are both looked up here (see
/// [`super::source`]) so the two always agree on where they live.
pub fn preset_search_dirs(user_dir: Option<PathBuf>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(user_dir);
    dirs.extend(user_preset_dir());
    if cfg!(target_os = "linux") {
        dirs.extend([
            PathBuf::from("/usr/share/lactea/presets"),
            PathBuf::from("/usr/share/projectM/presets"),
            PathBuf::from("/usr/local/share/projectM/presets"),
            PathBuf::from("/usr/share/projectm/presets"),
        ]);
    } else if cfg!(target_os = "macos") {
        dirs.extend([
            PathBuf::from("/opt/homebrew/share/projectM/presets"),
            PathBuf::from("/usr/local/share/projectM/presets"),
        ]);
    }
    if let Some(data) = dirs::data_dir() {
        dirs.push(data.join("lactea").join("presets"));
        dirs.push(data.join("projectM").join("presets"));
    }
    dirs
}

/// Strips the MilkDrop pack convention `presets_` prefix from a category
/// directory name (e.g. `presets_milkdrop` -> `milkdrop`); other names are
/// left untouched.
fn prettify_category(raw: &str) -> String {
    raw.strip_prefix("presets_").unwrap_or(raw).to_string()
}

/// Recursively scans `dirs` for `.milk` and MilkDrop 3 `.milk2` (double
/// preset) files (case-insensitive extension) off the render thread — no
/// GPU needed.
/// Returns one entry per file, sorted by `(category, name)` and
/// deduplicated by path (in case two search dirs alias the same tree).
pub fn scan_presets(dirs: &[PathBuf]) -> Vec<PresetEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        for entry in WalkDir::new(dir).follow_links(true) {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            // Non-UTF-8 paths are skipped: preset identity and display
            // names are UTF-8 throughout the UI.
            if !is_preset_file(path) || path.to_str().is_none() {
                continue;
            }
            let path = path.to_path_buf();
            if !seen.insert(path.clone()) {
                continue;
            }
            entries.push(PresetEntry::from_path(path));
        }
    }
    sort_entries(&mut entries);
    entries
}

/// Whether `path` names a MilkDrop preset: a `.milk` or MilkDrop 3 `.milk2`
/// file (case-insensitive extension).
pub fn is_preset_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("milk") || ext.eq_ignore_ascii_case("milk2"))
}

/// Orders a preset list for display: by category, then name, both
/// case-insensitive, with the path as a total-order tiebreak so the list
/// (and therefore every row's position) is stable between scans.
pub fn sort_entries(entries: &mut [PresetEntry]) {
    entries.sort_by_cached_key(|e| {
        (
            e.category.to_lowercase(),
            e.name.to_lowercase(),
            e.path.clone(),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use web_time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aulos-viz-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Nested dirs, noise files, and mixed-case extensions all in one
    /// tree: only the `.milk`/`.milk2` files should surface, each attributed
    /// to its immediate parent directory (prettified), sorted by
    /// `(category, name)`.
    #[test]
    fn scan_presets_finds_nested_case_insensitive_milk_files_grouped_by_category() {
        let root = temp_root("nested");
        let milkdrop_dir = root.join("presets_milkdrop");
        let nested_dir = milkdrop_dir.join("subdir");
        let stock_dir = root.join("presets_stock");
        fs::create_dir_all(&nested_dir).unwrap();
        fs::create_dir_all(&stock_dir).unwrap();

        fs::write(milkdrop_dir.join("Cool - Preset.milk"), b"").unwrap();
        fs::write(milkdrop_dir.join("noise.txt"), b"").unwrap();
        fs::write(nested_dir.join("Deep.MILK"), b"").unwrap();
        fs::write(stock_dir.join("Basic.milk"), b"").unwrap();
        fs::write(stock_dir.join("Double.Milk2"), b"").unwrap();
        fs::write(root.join("readme.txt"), b"").unwrap();

        let entries = scan_presets(std::slice::from_ref(&root));

        assert_eq!(
            entries
                .iter()
                .map(|e| (e.category.as_str(), e.name.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("milkdrop", "Cool - Preset"),
                ("stock", "Basic"),
                ("stock", "Double"),
                ("subdir", "Deep"),
            ]
        );

        fs::remove_dir_all(root).unwrap();
    }

    /// The same directory reachable twice in the search-dir list (e.g. a
    /// duplicated config entry) must not double up the preset it contains.
    #[test]
    fn scan_presets_dedupes_when_the_same_dir_is_listed_twice() {
        let root = temp_root("dedup");
        fs::write(root.join("Solo.milk"), b"").unwrap();

        let entries = scan_presets(&[root.clone(), root.clone()]);

        assert_eq!(entries.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preset_entry_from_path_derives_name_category_and_lowercase_key() {
        let e = PresetEntry::from_path(PathBuf::from("/x/presets_milkdrop/Geiss - Eddies 2.milk"));
        assert_eq!(e.name, "Geiss - Eddies 2");
        assert_eq!(e.category, "milkdrop");
        assert_eq!(e.search_key, "milkdrop geiss - eddies 2");
    }

    /// Same stem in two categories stays two distinct, path-identified
    /// entries, and ordering ignores case (`alpha` before `Zeta`).
    #[test]
    fn scan_presets_keeps_duplicate_stems_and_sorts_case_insensitively() {
        let root = temp_root("dupes");
        for dir in ["presets_b", "presets_a"] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join("Same.milk"), b"").unwrap();
        }
        fs::write(root.join("presets_a").join("alpha.milk"), b"").unwrap();
        fs::write(root.join("presets_a").join("Zeta.milk"), b"").unwrap();

        let entries = scan_presets(std::slice::from_ref(&root));

        let order: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| (e.category.as_str(), e.name.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![("a", "alpha"), ("a", "Same"), ("a", "Zeta"), ("b", "Same")]
        );
        assert_ne!(entries[1].path, entries[3].path);
        fs::remove_dir_all(root).unwrap();
    }
}
