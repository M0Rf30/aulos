// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The page header, the toolbar and the slim banners under it.

use cosmic::iced::{Alignment, Length};
use cosmic::widget;

use aulos_core::fl;
use aulos_core::tagger::{
    Confirm, Notice, ScanPhase, ScanState, TaggerMessage as Msg, TaggerState,
};

use super::TaggerViewProps;
use super::style;
use aulos_core::tagger::Facts;
use aulos_core::tagger::labels::Tone;

type Element<'a> = cosmic::Element<'a, Msg>;

/// Title, subtitle and the page-wide status chips.
pub fn header<'a>(facts: &Facts, state: &TaggerState) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let mut status = widget::Row::new()
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center);
    if state.loading() > 0 {
        status = status.push(style::chip(
            Some("emblem-synchronizing-symbolic"),
            fl!("tagger-loading", count = state.loading()),
            Tone::Neutral,
        ));
    }
    let unsaved = facts.unsaved;
    if unsaved > 0 {
        status = status.push(style::chip(
            Some("document-edit-symbolic"),
            fl!("tagger-unsaved-chip", count = unsaved),
            Tone::Accent,
        ));
    }

    widget::Row::new()
        .push(
            widget::Column::new()
                .push(widget::text::title3(fl!("tagger-title")))
                .push(style::muted(fl!("tagger-subtitle")))
                .spacing(2),
        )
        .push(widget::Space::new().width(Length::Fill))
        .push(status)
        .spacing(spacing.space_s)
        .align_y(Alignment::Center)
        .into()
}

/// How much of the toolbar's labels fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Density {
    /// Every button carries its label.
    Full,
    /// Only the buttons of the common flow keep their labels.
    Mixed,
    /// Icons with tooltips, except Save.
    Icons,
}

/// Toolbar width from which every label fits on one row.
const FULL_WIDTH: f32 = 1400.0;
/// Toolbar width from which the primary labels fit on one row.
const MIXED_WIDTH: f32 = 1120.0;
/// Below this width the groups wrap onto further rows.
const WRAP_WIDTH: f32 = 700.0;

fn density(width: f32) -> Density {
    if width >= FULL_WIDTH {
        Density::Full
    } else if width >= MIXED_WIDTH {
        Density::Mixed
    } else {
        Density::Icons
    }
}

/// A toolbar button: labelled when `labelled`, else an icon with the label
/// as tooltip. `badge` is a count shown next to the icon of the icon-only
/// form (the labelled form carries it in `label`).
fn tool_button<'a>(
    label: String,
    icon: &'static str,
    on_press: Option<Msg>,
    labelled: bool,
    badge: Option<usize>,
) -> Element<'a> {
    if labelled {
        return widget::button::standard(label)
            .leading_icon(widget::icon::from_name(icon).handle())
            .on_press_maybe(on_press)
            .into();
    }
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut content = widget::Row::new()
        .push(widget::icon::from_name(icon).size(16))
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center);
    if let Some(count) = badge.filter(|c| *c > 0) {
        content = content.push(widget::text::caption(count.to_string()));
    }
    widget::tooltip(
        widget::button::custom(content)
            .padding([spacing.space_xs, spacing.space_s])
            .class(cosmic::theme::Button::Standard)
            .on_press_maybe(on_press),
        widget::text::caption(label),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// A rounded tray holding the buttons of one task.
fn group<'a>(items: Vec<Element<'a>>) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    widget::container(
        widget::Row::with_children(items)
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center),
    )
    .padding(spacing.space_xxs)
    .class(cosmic::theme::Container::Card)
    .into()
}

/// [Add files, Add folder, Remove, Clear] | [Look up, Scan, Submit, Add as
/// release] | [Save, Revert]. One row from [`WRAP_WIDTH`] up, with labels
/// dropped as the width shrinks; below it the trays wrap.
pub fn toolbar<'a>(
    state: &TaggerState,
    props: TaggerViewProps,
    facts: &Facts,
    width: f32,
) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let busy = facts.busy;
    let empty = state.is_empty();
    let density = density(width);
    let primary = density != Density::Icons;
    let secondary = density == Density::Full;

    let files = group(vec![
        tool_button(
            fl!("tagger-add-files"),
            "list-add-symbolic",
            Some(Msg::AddFiles),
            primary,
            None,
        ),
        tool_button(
            fl!("tagger-add-folder"),
            "folder-open-symbolic",
            Some(Msg::AddFolder),
            primary,
            None,
        ),
        tool_button(
            fl!("tagger-remove"),
            "list-remove-symbolic",
            (!busy && state.selected_count() > 0).then_some(Msg::RemoveSelected),
            secondary,
            None,
        ),
        tool_button(
            fl!("tagger-clear"),
            "edit-clear-all-symbolic",
            (!busy && !empty).then_some(Msg::ClearAll),
            secondary,
            None,
        ),
    ]);

    let can_submit = !busy
        && facts.submittable > 0
        && props.acoustid_key_set
        && props.acoustid_user_key_saved == Some(true);
    let submit_label = if facts.submittable > 0 {
        fl!("tagger-submit-count", count = facts.submittable)
    } else {
        fl!("tagger-submit")
    };
    let online = group(vec![
        tool_button(
            fl!("tagger-lookup"),
            "edit-find-symbolic",
            (!busy && facts.has_target).then_some(Msg::Lookup),
            primary,
            None,
        ),
        tool_button(
            fl!("tagger-scan"),
            "audio-input-microphone-symbolic",
            (!busy && facts.has_target && props.acoustid_key_set).then_some(Msg::Scan),
            primary,
            None,
        ),
        tool_button(
            submit_label,
            "mail-send-symbolic",
            can_submit.then_some(Msg::SubmitAcoustIds),
            secondary,
            Some(facts.submittable),
        ),
        style::icon_button(
            "list-add-symbolic",
            fl!("tagger-add-as-release-tooltip"),
            (!empty && !busy).then_some(Msg::AddAsRelease(None)),
        ),
    ]);

    let saving = state.save_progress();
    let save_label = match saving {
        Some((done, total)) => fl!("tagger-saving", done = done, total = total),
        None if facts.savable > 0 => {
            fl!("tagger-save-count", count = facts.savable)
        }
        None => fl!("tagger-save"),
    };
    let save = widget::button::suggested(save_label)
        .leading_icon(widget::icon::from_name("document-save-symbolic").handle())
        .on_press_maybe((!state.is_saving() && facts.savable > 0).then_some(Msg::Save));
    let edits = group(vec![
        save.into(),
        tool_button(
            fl!("tagger-revert"),
            "edit-undo-symbolic",
            (!state.is_saving() && facts.selected_dirty).then_some(Msg::RevertSelected),
            secondary,
            None,
        ),
    ]);

    let trays = vec![files, online, edits];
    if width >= WRAP_WIDTH {
        widget::Row::with_children(trays)
            .spacing(spacing.space_s)
            .align_y(Alignment::Center)
            .into()
    } else {
        widget::flex_row(trays)
            .row_spacing(spacing.space_xs)
            .column_spacing(spacing.space_s)
            .into()
    }
}

fn notice_banner<'a>(notice: &Notice) -> Element<'a> {
    let (icon, tone) = if notice.error {
        ("dialog-error-symbolic", Tone::Danger)
    } else {
        ("dialog-information-symbolic", Tone::Accent)
    };
    style::banner(
        icon,
        notice.text.clone(),
        Vec::new(),
        vec![style::icon_button(
            "window-close-symbolic",
            fl!("tagger-dismiss"),
            Some(Msg::DismissNotice),
        )],
        tone,
    )
}

fn confirm_banner<'a>(state: &TaggerState, facts: &Facts, confirm: &Confirm) -> Element<'a> {
    let unsaved = match confirm {
        Confirm::ClearAll => facts.unsaved,
        Confirm::Remove(ids) => ids
            .iter()
            .filter_map(|id| state.file(*id))
            .filter(|f| f.has_unsaved_edits())
            .count(),
    };
    style::banner(
        "dialog-warning-symbolic",
        fl!("tagger-confirm-title"),
        vec![style::hint(fl!("tagger-confirm-body", count = unsaved)).into()],
        vec![
            widget::button::destructive(fl!("tagger-discard"))
                .on_press(Msg::ConfirmDiscard)
                .into(),
            widget::button::standard(fl!("tagger-cancel"))
                .on_press(Msg::CancelDiscard)
                .into(),
        ],
        Tone::Warning,
    )
}

fn open_settings_button<'a>() -> Element<'a> {
    widget::button::standard(fl!("tagger-open-settings"))
        .leading_icon(widget::icon::from_name("preferences-system-symbolic").handle())
        .on_press(Msg::OpenSettings)
        .into()
}

fn scan_banner<'a>(scan: &ScanState) -> Option<Element<'a>> {
    let total = scan.files.len();
    let failures: Vec<Element<'a>> = if scan.failures.is_empty() {
        Vec::new()
    } else {
        vec![style::hint(fl!("tagger-scan-failures", count = scan.failures.len())).into()]
    };
    let cancel = || -> Element<'a> {
        widget::button::standard(fl!("tagger-scan-cancel"))
            .on_press(Msg::CancelScan)
            .into()
    };
    match scan.phase {
        ScanPhase::Running => {
            let fraction = if total == 0 {
                0.0
            } else {
                scan.done as f32 / total as f32
            };
            let mut body: Vec<Element<'a>> = vec![
                widget::progress_bar::determinate_linear(fraction)
                    .girth(4)
                    .width(Length::Fill)
                    .into(),
            ];
            body.extend(failures);
            Some(style::banner(
                "audio-input-microphone-symbolic",
                fl!("tagger-scan-running", done = scan.done, total = total),
                body,
                vec![cancel()],
                Tone::Accent,
            ))
        }
        ScanPhase::Resolving => Some(style::banner(
            "audio-input-microphone-symbolic",
            fl!("tagger-scan-resolving", identified = scan.identified),
            vec![
                widget::progress_bar::indeterminate_linear()
                    .girth(4)
                    .width(Length::Fill)
                    .into(),
            ],
            vec![cancel()],
            Tone::Accent,
        )),
        ScanPhase::NoMatch => Some(style::banner(
            "dialog-warning-symbolic",
            fl!(
                "tagger-scan-no-match",
                identified = scan.identified,
                total = total
            ),
            failures,
            Vec::new(),
            Tone::Warning,
        )),
        ScanPhase::Done | ScanPhase::Cancelled => {
            if failures.is_empty() {
                None
            } else {
                Some(style::banner(
                    "dialog-warning-symbolic",
                    fl!("tagger-scan-failures", count = scan.failures.len()),
                    Vec::new(),
                    Vec::new(),
                    Tone::Warning,
                ))
            }
        }
    }
}

/// Everything that needs the user's attention or shows a running job. Each
/// banner is slim; with none showing the column is empty.
pub fn banners<'a>(state: &TaggerState, props: TaggerViewProps, facts: &Facts) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut column = widget::Column::new().spacing(spacing.space_xs);

    if let Some(notice) = state.notice() {
        column = column.push(notice_banner(notice));
    }
    if let Some(confirm) = state.confirm() {
        column = column.push(confirm_banner(state, facts, confirm));
    }
    if !props.acoustid_key_set && !state.is_empty() {
        column = column.push(style::banner(
            "dialog-warning-symbolic",
            fl!("tagger-key-missing-title"),
            vec![style::hint(fl!("tagger-key-missing-body")).into()],
            vec![open_settings_button()],
            Tone::Warning,
        ));
    } else if props.acoustid_user_key_saved == Some(false) && facts.submittable > 0 {
        column = column.push(style::banner(
            "dialog-warning-symbolic",
            fl!("tagger-user-key-missing-title"),
            vec![style::hint(fl!("tagger-user-key-missing-body")).into()],
            vec![open_settings_button()],
            Tone::Warning,
        ));
    }
    if let Some(banner) = state.scan().and_then(scan_banner) {
        column = column.push(banner);
    }
    if state.is_submitting() {
        column = column.push(style::banner(
            "mail-send-symbolic",
            fl!("tagger-submitting"),
            vec![
                widget::progress_bar::indeterminate_linear()
                    .girth(4)
                    .width(Length::Fill)
                    .into(),
            ],
            Vec::new(),
            Tone::Accent,
        ));
    }
    if let Some((done, total)) = state.save_progress() {
        let fraction = if total == 0 {
            0.0
        } else {
            done as f32 / total as f32
        };
        column = column.push(style::banner(
            "document-save-symbolic",
            fl!("tagger-saving", done = done, total = total),
            vec![
                widget::progress_bar::determinate_linear(fraction)
                    .girth(4)
                    .width(Length::Fill)
                    .into(),
            ],
            Vec::new(),
            Tone::Accent,
        ));
    }
    column.into()
}
