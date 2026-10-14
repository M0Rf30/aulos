// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Numbers derived from the editor state that several panes need.

use super::{FileStatus, TaggerState};

/// Numbers derived from the state that several panes need, computed once
/// per frame: some of them walk every file.
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    /// A save, submission or scan is running.
    pub busy: bool,
    /// A lookup or scan would have files to work on (see
    /// [`TaggerState::target_files`]).
    pub has_target: bool,
    /// The selection holds a file that can be edited.
    pub selection_editable: bool,
    /// Files a submission would send.
    pub submittable: usize,
    /// Files with edits that were not saved.
    pub unsaved: usize,
    /// Files a save would write.
    pub savable: usize,
    /// A selected file has edits (or a failed save) to revert.
    pub selected_dirty: bool,
}

impl Facts {
    /// Derives the facts from `state`.
    #[must_use]
    pub fn compute(state: &TaggerState) -> Self {
        let selection = state.selected_count() > 0;
        Self {
            busy: state.is_busy(),
            has_target: has_target(
                state
                    .files()
                    .iter()
                    .map(|f| (state.is_selected(f.id), f.is_editable())),
                selection,
            ),
            selection_editable: state.selection_editable(),
            submittable: state.submittable(),
            unsaved: state.unsaved_count(),
            savable: state.savable_count(),
            selected_dirty: state.files().iter().any(|f| {
                state.is_selected(f.id)
                    && matches!(f.status, FileStatus::Modified | FileStatus::Error(_))
            }),
        }
    }
}

/// Whether a lookup or scan has files to work on, given each file's
/// `(selected, editable)`: the selected editable files, or every editable
/// file when nothing is selected.
fn has_target(files: impl IntoIterator<Item = (bool, bool)>, selection: bool) -> bool {
    files
        .into_iter()
        .any(|(selected, editable)| editable && (selected || !selection))
}

#[cfg(test)]
mod tests {
    use super::has_target;

    #[test]
    fn nothing_selected_targets_every_editable_file() {
        assert!(has_target([(false, false), (false, true)], false));
        assert!(!has_target([(false, false)], false));
        assert!(!has_target([], false));
    }

    #[test]
    fn a_selection_targets_only_its_editable_files() {
        assert!(has_target([(true, true), (false, true)], true));
        assert!(!has_target([(true, false), (false, true)], true));
    }
}
