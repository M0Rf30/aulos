// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The metadata box: one row per field of the selection with the value on
//! disk next to the value being built, and the "add a field" form.

use cosmic::iced::alignment::Horizontal;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;

use crate::views::common;
use aulos_core::fl;
use aulos_core::library::tagio::model::KNOWN_KEYS;
use aulos_core::tagger::edit::join_values;
use aulos_core::tagger::{FieldRow, FieldValue, TaggerMessage as Msg, TaggerState};

use super::style;
use aulos_core::tagger::Facts;
use aulos_core::tagger::labels;
use aulos_core::tagger::labels::Tone;

type Element<'a> = cosmic::Element<'a, Msg>;

/// Share of the width each table column takes.
const FIELD_PORTION: u16 = 3;
const ORIGINAL_PORTION: u16 = 3;
const NEW_PORTION: u16 = 4;
/// Width of the dropdown of well-known fields in the add form.
const KNOWN_FIELD_WIDTH: f32 = 220.0;

fn italic() -> cosmic::iced::Font {
    cosmic::iced::Font {
        style: cosmic::iced::font::Style::Italic,
        ..cosmic::font::default()
    }
}

/// The read-only "original" value of a row.
fn original_cell<'a>(row: &FieldRow) -> Element<'a> {
    match &row.original {
        FieldValue::Empty => widget::Space::new().into(),
        FieldValue::Same(values) => {
            let text = join_values(values);
            let first_line = text.lines().next().unwrap_or_default();
            let shown = if text.contains('\n') {
                format!("{first_line}…")
            } else {
                text
            };
            style::muted(shown).into()
        }
        FieldValue::Mixed => style::muted(fl!("tagger-different-values"))
            .font(italic())
            .into(),
    }
}

/// The "new" value of a row: a text box, or a read-only preview when the
/// value spans several lines.
fn new_cell<'a>(state: &TaggerState, row: &FieldRow, editable: bool) -> Element<'a> {
    if row.multiline {
        let text = row.new.display();
        let first_line = text.lines().next().unwrap_or_default();
        let preview = format!("{first_line}…");
        return widget::tooltip(
            common::clipped_cell(common::cell_text(preview).into()),
            widget::text::caption(fl!("tagger-multiline-tooltip")),
            widget::tooltip::Position::Top,
        )
        .into();
    }

    let placeholder = if row.new.is_mixed() {
        fl!("tagger-different-values")
    } else {
        String::new()
    };
    let mut input = widget::text_input(placeholder, state.input_text(row)).width(Length::Fill);
    if editable {
        let key = row.key.clone();
        input = input.on_input(move |text| Msg::EditField {
            key: key.clone(),
            text,
        });
    }
    input.into()
}

fn row_actions<'a>(row: &FieldRow, editable: bool) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let revert: Element<'a> = if row.changed {
        style::icon_button(
            "edit-undo-symbolic",
            fl!("tagger-revert-field"),
            editable.then(|| Msg::RevertField(row.key.clone())),
        )
    } else {
        style::icon_slot()
    };
    widget::Row::new()
        .push(revert)
        .push(style::icon_button(
            "edit-delete-symbolic",
            fl!("tagger-delete-field"),
            editable.then(|| Msg::DeleteField(row.key.clone())),
        ))
        .spacing(spacing.space_xxs)
        .into()
}

fn field_name<'a>(row: &FieldRow) -> Element<'a> {
    let name = labels::field_label(&row.key);
    let text = common::cell_text(name).font(cosmic::font::semibold());
    let text = if row.changed {
        text.class(cosmic::theme::Text::Accent)
    } else {
        text
    };
    widget::tooltip(
        text,
        widget::text::caption(row.key.clone()),
        widget::tooltip::Position::Top,
    )
    .into()
}

fn field_row<'a>(
    state: &TaggerState,
    row: &FieldRow,
    editable: bool,
    compact: bool,
) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let content = if compact {
        // Narrow pane: the original value moves under the field name.
        let label = widget::Column::new()
            .push(field_name(row))
            .push(original_cell(row))
            .spacing(1);
        widget::Row::new()
            .push(
                widget::container(common::clipped_cell(label.into()))
                    .width(Length::FillPortion(FIELD_PORTION)),
            )
            .push(
                widget::container(new_cell(state, row, editable))
                    .width(Length::FillPortion(ORIGINAL_PORTION + NEW_PORTION)),
            )
    } else {
        widget::Row::new()
            .push(
                widget::container(common::clipped_cell(field_name(row)))
                    .width(Length::FillPortion(FIELD_PORTION)),
            )
            .push(
                widget::container(common::clipped_cell(original_cell(row)))
                    .width(Length::FillPortion(ORIGINAL_PORTION)),
            )
            .push(
                widget::container(new_cell(state, row, editable))
                    .width(Length::FillPortion(NEW_PORTION)),
            )
    };

    widget::container(
        content
            .push(row_actions(row, editable))
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center),
    )
    .padding([spacing.space_xxs, spacing.space_xs])
    .width(Length::Fill)
    .class(style::changed_row_class(row.changed))
    .into()
}

/// Column captions, laid out like a row so they line up with the cells.
fn table_header<'a>(compact: bool) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let caption = |text: String| {
        widget::text::caption_heading(text).class(cosmic::theme::Text::Custom(|theme| {
            cosmic::iced::widget::text::Style {
                color: Some(theme.cosmic().palette.neutral_7.into()),
                ..Default::default()
            }
        }))
    };

    let mut row = widget::Row::new().push(
        widget::container(caption(fl!("tagger-col-field")))
            .width(Length::FillPortion(FIELD_PORTION)),
    );
    if compact {
        row = row.push(
            widget::container(caption(fl!("tagger-col-new")))
                .width(Length::FillPortion(ORIGINAL_PORTION + NEW_PORTION)),
        );
    } else {
        row = row
            .push(
                widget::container(caption(fl!("tagger-col-original")))
                    .width(Length::FillPortion(ORIGINAL_PORTION)),
            )
            .push(
                widget::container(caption(fl!("tagger-col-new")))
                    .width(Length::FillPortion(NEW_PORTION)),
            );
    }
    // The two action buttons of the rows below.
    row = row.push(widget::Space::new().width(Length::Fixed(
        style::ICON_SLOT * 2.0 + f32::from(spacing.space_xxs),
    )));
    widget::container(row.spacing(spacing.space_xs).align_y(Alignment::Center))
        .padding([0, spacing.space_xs])
        .into()
}

/// The form that adds a field to the selection.
fn add_form<'a>(state: &'a TaggerState, editable: bool, compact: bool) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let draft = state.add_field();

    let known_labels: Vec<String> = KNOWN_KEYS.iter().map(|k| labels::field_label(k)).collect();
    let known = widget::dropdown(known_labels, draft.known, Msg::AddFieldKnown)
        .width(Length::Fixed(KNOWN_FIELD_WIDTH))
        .placeholder(fl!("tagger-add-field-known"));
    let custom = widget::text_input(fl!("tagger-add-field-custom"), draft.custom.as_str())
        .on_input(Msg::AddFieldCustom)
        .on_submit(|_| Msg::AddFieldCommit)
        .width(Length::Fill);
    let value = widget::text_input(fl!("tagger-add-field-value"), draft.value.as_str())
        .on_input(Msg::AddFieldValue)
        .on_submit(|_| Msg::AddFieldCommit)
        .width(Length::Fill);
    let add = widget::button::standard(fl!("tagger-add-field"))
        .leading_icon(widget::icon::from_name("list-add-symbolic").handle())
        .on_press_maybe(editable.then_some(Msg::AddFieldCommit));

    let controls: Element<'a> = if compact {
        widget::Column::new()
            .push(
                widget::Row::new()
                    .push(known)
                    .push(custom)
                    .spacing(spacing.space_xs)
                    .align_y(Alignment::Center),
            )
            .push(
                widget::Row::new()
                    .push(value)
                    .push(add)
                    .spacing(spacing.space_xs)
                    .align_y(Alignment::Center),
            )
            .spacing(spacing.space_xs)
            .into()
    } else {
        widget::Row::new()
            .push(known)
            .push(custom)
            .push(value)
            .push(add)
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center)
            .into()
    };

    let mut column = widget::Column::new()
        .push(widget::text::caption_heading(fl!("tagger-add-field-title")))
        .push(controls)
        .spacing(spacing.space_xs);
    if let Some(error) = state.add_field_error() {
        column = column.push(style::tone_hint(
            labels::add_field_error_text(error),
            Tone::Danger,
        ));
    }
    column.into()
}

fn nothing_selected<'a>() -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    widget::container(
        widget::Column::new()
            .push(
                widget::container(widget::icon::from_name("document-edit-symbolic").size(32))
                    .padding(spacing.space_s)
                    .class(style::tint_class(Tone::Neutral, true)),
            )
            .push(widget::text::heading(fl!("tagger-metadata-empty-title")))
            .push(style::hint(fl!("tagger-metadata-empty-hint")).align_x(Horizontal::Center))
            .spacing(spacing.space_xs)
            .align_x(Alignment::Center),
    )
    .padding(spacing.space_m)
    .width(Length::Fill)
    .align_x(Horizontal::Center)
    .into()
}

/// The metadata card. `compact` folds the "original" column under the
/// field names for narrow panes.
pub fn panel<'a>(state: &'a TaggerState, facts: &Facts, compact: bool) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let selected = state.selected_count();

    let trailing: Option<Element<'a>> = (selected > 0)
        .then(|| style::muted(fl!("tagger-metadata-selection", count = selected)).into());

    let body: Element<'a> = if selected == 0 {
        nothing_selected()
    } else {
        let editable = facts.selection_editable;
        let mut column = widget::Column::new().spacing(spacing.space_xxs);
        if !editable {
            column = column.push(style::tone_hint(
                fl!("tagger-metadata-readonly"),
                Tone::Warning,
            ));
        }
        column = column.push(table_header(compact)).push(style::divider());
        for row in state.rows() {
            column = column.push(field_row(state, row, editable, compact));
        }
        column
            .push(style::hint(fl!("tagger-multi-hint")))
            .push(style::divider())
            .push(add_form(state, editable, compact))
            .into()
    };

    style::section(fl!("tagger-metadata-title"), trailing, body)
}
