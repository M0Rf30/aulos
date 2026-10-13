// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The tag editor page, Picard-style: the loaded files grouped by album on
//! the left; on the right the MusicBrainz lookup (while one is open), the
//! metadata table of the selection and its cover art.
//!
//! Everything shown comes from [`TaggerState`]'s getters and every action is
//! a [`TaggerMessage`]; this module owns no state. The page tree keeps its
//! shape whatever the state (header, toolbar, a banner slot, the body) so
//! switching pages or jobs finishing never resets a scroll position.
//!
//! Layout: from [`WIDE_BREAKPOINT`] the two panes sit side by side, each
//! scrolling on its own; below it the panes stack in one scrolling column
//! with the file list at a fixed height. Panes narrower than
//! [`COMPACT_PANE_WIDTH`] fold the metadata table's "original" column under
//! the field names. File rows are plain widgets (no virtualization): the
//! page has no scroll offset to window by.

mod cover;
mod files;
mod labels;
mod lookup;
mod metadata;
mod style;
mod toolbar;

use cosmic::iced::Length;
use cosmic::widget;

use crate::tagger::{FileStatus, TaggerMessage, TaggerState};

/// What the page needs besides the editor state: the settings it reacts to.
#[derive(Debug, Clone, Copy)]
pub struct TaggerViewProps {
    /// An AcoustID application key is configured (needed to scan).
    pub acoustid_key_set: bool,
    /// The user's own AcoustID key is in the keyring (needed to submit);
    /// `None` while that is still being checked.
    pub acoustid_user_key_saved: Option<bool>,
    /// Applying a release also fetches its Cover Art Archive front cover.
    pub use_caa_cover: bool,
}

/// Below this width the panes stack.
const WIDE_BREAKPOINT: f32 = 960.0;
/// Panes narrower than this use the compact metadata table.
const COMPACT_PANE_WIDTH: f32 = 620.0;
/// Share of the page the files pane takes in the side-by-side layout, out
/// of `FILES_PORTION + RIGHT_PORTION`.
const FILES_PORTION: u16 = 5;
const RIGHT_PORTION: u16 = 6;

/// Numbers derived from the state that several panes need, computed once
/// per frame: some of them walk every file.
#[derive(Debug, Clone, Copy)]
struct Facts {
    /// A save, submission or scan is running.
    busy: bool,
    /// A lookup or scan would have files to work on (see
    /// [`TaggerState::target_files`]).
    has_target: bool,
    /// The selection holds a file that can be edited.
    selection_editable: bool,
    /// Files a submission would send.
    submittable: usize,
    /// Files with edits that were not saved.
    unsaved: usize,
    /// Files a save would write.
    savable: usize,
    /// A selected file has edits (or a failed save) to revert.
    selected_dirty: bool,
}

impl Facts {
    fn compute(state: &TaggerState) -> Self {
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

/// The right pane's content: the lookup (when open), then the metadata and
/// cover cards.
fn right_pane<'a>(
    state: &'a TaggerState,
    props: TaggerViewProps,
    facts: &Facts,
    compact: bool,
) -> cosmic::Element<'a, TaggerMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut column = widget::Column::new().spacing(spacing.space_s);
    if let Some(panel) = lookup::panel(state, props) {
        column = column.push(panel);
    }
    column
        .push(metadata::panel(state, facts, compact))
        .push(cover::panel(state, facts))
        .into()
}

/// The two panes for a body of `size`.
fn body<'a>(
    state: &'a TaggerState,
    props: TaggerViewProps,
    facts: &Facts,
    size: cosmic::iced::Size,
) -> cosmic::Element<'a, TaggerMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    if size.width >= WIDE_BREAKPOINT {
        let right_width = (size.width - f32::from(spacing.space_m)) * f32::from(RIGHT_PORTION)
            / f32::from(FILES_PORTION + RIGHT_PORTION);
        let compact = right_width < COMPACT_PANE_WIDTH;
        let right = widget::scrollable(
            widget::container(right_pane(state, props, facts, compact)).padding([
                0,
                spacing.space_s,
                0,
                0,
            ]),
        )
        .width(Length::FillPortion(RIGHT_PORTION))
        .height(Length::Fill);
        widget::Row::new()
            .push(
                widget::container(files::pane(state, props, facts, None))
                    .width(Length::FillPortion(FILES_PORTION))
                    .height(Length::Fill),
            )
            .push(right)
            .spacing(spacing.space_m)
            .into()
    } else {
        let compact = size.width < COMPACT_PANE_WIDTH;
        let files_height = (size.height * 0.55).clamp(260.0, 460.0);
        widget::scrollable(
            widget::Column::new()
                .push(files::pane(state, props, facts, Some(files_height)))
                .push(right_pane(state, props, facts, compact))
                .spacing(spacing.space_m)
                .padding([0, spacing.space_s, 0, 0]),
        )
        .height(Length::Fill)
        .into()
    }
}

/// The tag editor page.
pub fn view<'a>(
    state: &'a TaggerState,
    props: TaggerViewProps,
) -> cosmic::Element<'a, TaggerMessage> {
    let facts = Facts::compute(state);

    // One responsive over the whole page: the toolbar needs the page width
    // to decide how many labels fit, the panes need their own size.
    widget::responsive(move |size| {
        let spacing = cosmic::theme::active().cosmic().spacing;
        let inner_width = size.width - 2.0 * f32::from(spacing.space_m);
        let panes = widget::responsive(move |size| body(state, props, &facts, size));
        widget::Column::new()
            .push(toolbar::header(&facts, state))
            .push(toolbar::toolbar(state, props, &facts, inner_width))
            // Always-present banner slot (zero-height when empty) so the
            // page tree keeps its shape whether or not a banner is showing.
            .push(toolbar::banners(state, props, &facts))
            .push(
                widget::container(panes)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .spacing(spacing.space_s)
            .padding(spacing.space_m)
            .into()
    })
    .into()
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
