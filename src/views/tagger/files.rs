// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The left pane: the loaded files grouped by album, with the selection
//! controls and the per-album actions.

use std::collections::{HashMap, HashSet};

use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::{Alignment, Length};
use cosmic::widget;

use crate::views::common;
use aulos_core::fl;
use aulos_core::library::tagio::keys;
use aulos_core::tagger::{
    Cluster, FileId, FileStatus, SubmissionState, TagFile, TaggerMessage as Msg, TaggerState,
};

use super::TaggerViewProps;
use super::style;
use aulos_core::tagger::Facts;
use aulos_core::tagger::labels;
use aulos_core::tagger::labels::Tone;

type Element<'a> = cosmic::Element<'a, Msg>;

/// Width reserved for the file's status glyph.
const STATUS_WIDTH: f32 = 20.0;
/// Width of the track number column.
const TRACK_WIDTH: f32 = 28.0;
/// Room kept on the right of the list for the scrollbar.
const SCROLLBAR_CLEARANCE: u16 = 12;

/// What the rows need to know beyond their file.
struct RowContext {
    /// A release is applied, so files without a track are flagged.
    release_applied: bool,
    /// The files the applied release has no track for.
    unmatched: HashSet<FileId>,
}

fn status_slot<'a>(file: &TagFile) -> Element<'a> {
    let glyph: Element<'a> = match &file.status {
        FileStatus::Unchanged => widget::Space::new().into(),
        FileStatus::Modified => tooltip(
            style::tone_icon("media-record-symbolic", 12, Tone::Accent),
            fl!("tagger-status-modified"),
        ),
        FileStatus::Saving => tooltip(style::spinner(14.0), fl!("tagger-status-saving")),
        FileStatus::Saved => tooltip(
            style::tone_icon("object-select-symbolic", 14, Tone::Success),
            fl!("tagger-status-saved"),
        ),
        FileStatus::Error(message) => tooltip(
            style::tone_icon("dialog-error-symbolic", 16, Tone::Danger),
            fl!("tagger-status-error", error = message.clone()),
        ),
    };
    widget::container(glyph)
        .width(Length::Fixed(STATUS_WIDTH))
        .align_x(Horizontal::Center)
        .into()
}

fn tooltip<'a>(content: Element<'a>, text: String) -> Element<'a> {
    widget::tooltip(
        content,
        widget::text::caption(text),
        widget::tooltip::Position::Top,
    )
    .into()
}

fn file_row<'a>(state: &TaggerState, ctx: &RowContext, file: &'a TagFile) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let id = file.id;
    let selected = state.is_selected(id);

    let title = file
        .edited
        .get(keys::TITLE)
        .filter(|t| !t.trim().is_empty())
        .map_or_else(|| file.file_name(), str::to_owned);

    let mut text = widget::Column::new()
        .push(common::cell_text(title).font(cosmic::font::semibold()))
        .push(style::muted(file.file_name()))
        .spacing(1);
    if let Some(assignment) = &file.assignment {
        text = text.push(style::tone_caption(
            fl!(
                "tagger-assignment",
                number = assignment.number.clone(),
                title = assignment.title.clone(),
                score = labels::percent(assignment.score)
            ),
            Tone::Accent,
        ));
    } else if ctx.release_applied && ctx.unmatched.contains(&id) {
        text = text.push(style::tone_caption(fl!("tagger-unmatched"), Tone::Warning));
    }
    if let Some(reason) = file.read_only_reason() {
        text = text.push(style::tone_caption(reason, Tone::Warning));
    }
    if let Some((message, tone)) = labels::submission_text(&file.submission) {
        text = text.push(style::tone_caption(message, tone));
    }

    let duration: Element<'a> = match file.duration_secs {
        Some(secs) => common::duration_cell(secs.max(0.0).round() as u64),
        None => widget::Space::new()
            .width(Length::Fixed(common::DURATION_WIDTH))
            .into(),
    };

    let content = widget::Row::new()
        .push(status_slot(file))
        .push(
            widget::container(style::muted(labels::track_label(&file.edited)))
                .width(Length::Fixed(TRACK_WIDTH))
                .align_x(Horizontal::Right),
        )
        .push(common::clipped_cell(text.into()))
        .push(duration)
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center);

    let mut row = widget::Row::new()
        .push(widget::checkbox(selected).on_toggle(move |_| Msg::ToggleFile(id)))
        .push(
            widget::button::custom(content)
                .on_press(Msg::SelectFile(id))
                .width(Length::Fill)
                .padding([spacing.space_xxs, spacing.space_xs])
                .class(style::quiet_button_class()),
        )
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center);

    if !file.is_editable() {
        row = row.push(tooltip(
            style::chip(
                Some("changes-prevent-symbolic"),
                fl!("tagger-readonly"),
                Tone::Warning,
            ),
            file.read_only_reason().unwrap_or_default(),
        ));
    }
    if let Some(hit) = &file.acoustid {
        row = row.push(tooltip(
            common::link(
                widget::text::caption(fl!("tagger-acoustid-link")),
                true,
                Msg::OpenAcoustIdPage(hit.acoustid.clone()),
            ),
            fl!("tagger-acoustid-open"),
        ));
    }
    if let Some(recording) = file.known_recording_id() {
        row = row.push(style::icon_button(
            "web-browser-symbolic",
            fl!("tagger-recording-open"),
            Some(Msg::OpenRecordingPage(recording)),
        ));
    }
    if matches!(file.submission, SubmissionState::Pending) {
        row = row.push(style::spinner(14.0));
    }

    widget::container(row)
        .padding([0, spacing.space_xxs])
        .width(Length::Fill)
        .class(style::row_class(selected))
        .into()
}

fn cluster_header<'a>(
    state: &TaggerState,
    props: TaggerViewProps,
    facts: &Facts,
    cluster: &Cluster,
    editable: bool,
) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let key = cluster.key.clone();
    let unclustered = key.is_unclustered();

    let title = if unclustered {
        fl!("tagger-unclustered")
    } else {
        cluster.album.clone()
    };
    let artist = if unclustered {
        String::new()
    } else if cluster.various {
        fl!("tagger-various-artists")
    } else if cluster.artist.is_empty() {
        fl!("tagger-unknown-artist")
    } else {
        cluster.artist.clone()
    };
    let subtitle = labels::dotted([
        artist,
        fl!("tagger-files-count", count = cluster.files.len()),
    ]);

    let heading = widget::Column::new()
        .push(common::cell_text(title).font(cosmic::font::semibold()))
        .push(style::muted(subtitle))
        .spacing(1);
    let select = widget::tooltip(
        widget::button::custom(common::clipped_cell(heading.into()))
            .on_press(Msg::SelectCluster(key.clone()))
            .width(Length::Fill)
            .padding([spacing.space_xxs, spacing.space_xs])
            .class(style::quiet_button_class()),
        widget::text::caption(fl!("tagger-cluster-select")),
        widget::tooltip::Position::Top,
    );

    let busy = facts.busy;
    let mut row = widget::Row::new()
        .push(
            widget::checkbox(state.cluster_selected(cluster)).on_toggle({
                let key = key.clone();
                move |_| Msg::ToggleCluster(key.clone())
            }),
        )
        .push(select)
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center)
        .push(style::icon_button(
            "edit-find-symbolic",
            fl!("tagger-cluster-lookup"),
            (!busy && editable).then(|| Msg::LookupCluster(key.clone())),
        ))
        .push(style::icon_button(
            "audio-input-microphone-symbolic",
            fl!("tagger-cluster-scan"),
            (!busy && editable && props.acoustid_key_set).then(|| Msg::ScanCluster(key.clone())),
        ));
    if !unclustered {
        row = row.push(style::icon_button(
            "list-add-symbolic",
            fl!("tagger-cluster-add-release"),
            (!busy).then_some(Msg::AddAsRelease(Some(key))),
        ));
    }

    widget::container(row)
        .padding([spacing.space_xxs, spacing.space_xxs])
        .width(Length::Fill)
        .into()
}

/// Selection shortcuts and the file count above the list.
fn strip<'a>(state: &TaggerState, facts: &Facts) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let total = state.files().len();
    let selected = state.selected_count();

    let mut summary = fl!("tagger-files-count", count = total);
    if selected > 0 {
        summary = labels::dotted([summary, fl!("tagger-selected-count", count = selected)]);
    }

    let link = |label: String, on_press: Option<Msg>| -> Element<'a> {
        widget::button::text(label).on_press_maybe(on_press).into()
    };

    widget::Row::new()
        .push(widget::text::caption_heading(summary))
        .push(widget::Space::new().width(Length::Fill))
        .push(link(
            fl!("tagger-select-all"),
            (selected < total).then_some(Msg::SelectAll),
        ))
        .push(link(
            fl!("tagger-clear-selection"),
            (selected > 0).then_some(Msg::ClearSelection),
        ))
        .push(link(
            fl!("tagger-revert-all"),
            (facts.unsaved > 0 && !state.is_saving()).then_some(Msg::RevertAll),
        ))
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center)
        .into()
}

fn empty_state<'a>(loading: bool, height: Option<f32>) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let glyph = widget::container(widget::icon::from_name("audio-x-generic-symbolic").size(40))
        .padding(spacing.space_m)
        .class(style::tint_class(Tone::Accent, true));

    let mut content = widget::Column::new()
        .push(glyph)
        .push(widget::text::title3(fl!("tagger-empty-title")))
        .push(style::hint(fl!("tagger-empty-hint")).align_x(Horizontal::Center))
        .spacing(spacing.space_s)
        .align_x(Alignment::Center);
    if loading {
        content = content.push(style::spinner(24.0));
    } else {
        content = content.push(
            widget::Row::new()
                .push(
                    widget::button::suggested(fl!("tagger-add-files"))
                        .leading_icon(widget::icon::from_name("list-add-symbolic").handle())
                        .on_press(Msg::AddFiles),
                )
                .push(
                    widget::button::standard(fl!("tagger-add-folder"))
                        .leading_icon(widget::icon::from_name("folder-open-symbolic").handle())
                        .on_press(Msg::AddFolder),
                )
                .spacing(spacing.space_xs),
        );
    }

    widget::container(content.max_width(360.0))
        .padding(spacing.space_l)
        .width(Length::Fill)
        .height(height.map_or(Length::Fill, Length::Fixed))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .into()
}

/// The files pane. `height` fixes the list's height (stacked layout);
/// `None` lets it fill the space it is given (side-by-side layout).
pub fn pane<'a>(
    state: &'a TaggerState,
    props: TaggerViewProps,
    facts: &Facts,
    height: Option<f32>,
) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let body: Element<'a> = if state.is_empty() {
        empty_state(state.loading() > 0, height)
    } else {
        let by_id: HashMap<FileId, &TagFile> = state.files().iter().map(|f| (f.id, f)).collect();
        let ctx = match state.lookup().filter(|l| l.applied()) {
            Some(lookup) => RowContext {
                release_applied: true,
                unmatched: lookup
                    .plan
                    .as_ref()
                    .map(|plan| plan.unmatched().into_iter().collect())
                    .unwrap_or_default(),
            },
            None => RowContext {
                release_applied: false,
                unmatched: HashSet::new(),
            },
        };

        let mut list = widget::Column::new().spacing(spacing.space_s);
        for cluster in state.clusters() {
            let members: Vec<&'a TagFile> = cluster
                .files
                .iter()
                .filter_map(|id| by_id.get(id).copied())
                .collect();
            let editable = members.iter().any(|f| f.is_editable());
            let mut group = widget::Column::new()
                .push(cluster_header(state, props, facts, cluster, editable))
                .push(style::divider())
                .spacing(2);
            for file in members {
                group = group.push(file_row(state, &ctx, file));
            }
            list = list.push(group);
        }

        let scroller = widget::scrollable(widget::container(list).width(Length::Fill).padding([
            0,
            SCROLLBAR_CLEARANCE,
            0,
            0,
        ]));
        match height {
            Some(h) => scroller.height(Length::Fixed(h)).into(),
            None => scroller.height(Length::Fill).into(),
        }
    };

    let mut column = widget::Column::new()
        .spacing(spacing.space_xs)
        .padding(spacing.space_s)
        .width(Length::Fill);
    if !state.is_empty() {
        column = column.push(strip(state, facts));
    }
    column = column.push(body);
    if height.is_none() {
        column = column.height(Length::Fill);
    }
    widget::container(column)
        .width(Length::Fill)
        .height(if height.is_none() {
            Length::Fill
        } else {
            Length::Shrink
        })
        .class(cosmic::theme::Container::Card)
        .into()
}
