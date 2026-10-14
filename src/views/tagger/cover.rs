// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The cover art card: the pictures embedded in the selected files, with
//! their type, and the controls to change them.

use cosmic::iced::{Alignment, Length};
use cosmic::widget;

use crate::views::common;
use aulos_core::fl;
use aulos_core::tagger::edit::PICTURE_KINDS;
use aulos_core::tagger::{PictureRow, TaggerMessage as Msg, TaggerState};

use super::Thumbs;
use super::style;
use aulos_core::tagger::Facts;
use aulos_core::tagger::labels;
use aulos_core::tagger::labels::Tone;

type Element<'a> = cosmic::Element<'a, Msg>;

/// Side of a picture thumbnail.
const THUMB_SIZE: f32 = 112.0;
/// Width of a picture card (the thumbnail's width plus its padding).
const CARD_WIDTH: f32 = 136.0;

fn thumbnail<'a>(thumbs: &Thumbs, picture: &PictureRow) -> Element<'a> {
    match thumbs.get(picture.hash) {
        Some(handle) => {
            let radius = cosmic::theme::active().cosmic().corner_radii.radius_s[0];
            common::cover_art(handle, THUMB_SIZE, radius, false)
        }
        None => widget::tooltip(
            common::list_art_icon(None, THUMB_SIZE as u16, "image-x-generic-symbolic"),
            widget::text::caption(fl!("tagger-cover-placeholder")),
            widget::tooltip::Position::Top,
        )
        .into(),
    }
}

fn picture_card<'a>(thumbs: &Thumbs, picture: &PictureRow, editable: bool) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let (hash, kind) = (picture.hash, picture.kind);

    let (names, selected) = labels::picture_kind_choices(kind);
    let kind_picker = widget::dropdown(names, selected, move |index| Msg::SetPictureKind {
        hash,
        kind,
        new_kind: PICTURE_KINDS.get(index).copied().unwrap_or(kind),
    })
    .width(Length::Fixed(THUMB_SIZE));

    let mut footer = widget::Row::new()
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center);
    if picture.changed {
        footer = footer.push(style::chip(None, fl!("tagger-cover-changed"), Tone::Accent));
    }
    footer = footer
        .push(widget::Space::new().width(Length::Fill))
        .push(style::icon_button(
            "edit-delete-symbolic",
            fl!("tagger-cover-remove"),
            editable.then_some(Msg::RemovePicture { hash, kind }),
        ));

    let mut column = widget::Column::new()
        .push(thumbnail(thumbs, picture))
        .push(kind_picker)
        .push(style::muted(fl!(
            "tagger-cover-details",
            mime = picture.mime.clone(),
            size = labels::format_size(picture.bytes)
        )))
        .spacing(spacing.space_xxs);
    if picture.files < picture.of {
        column = column.push(style::tone_caption(
            fl!(
                "tagger-cover-in-files",
                count = picture.files,
                total = picture.of
            ),
            Tone::Warning,
        ));
    }

    widget::container(column.push(footer))
        .padding(spacing.space_xs)
        .width(Length::Fixed(CARD_WIDTH))
        .class(style::tile_class())
        .into()
}

fn controls<'a>(state: &TaggerState, editable: bool) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let kind_labels: Vec<String> = PICTURE_KINDS
        .iter()
        .map(|k| labels::picture_kind_label(*k))
        .collect();
    let new_kind = PICTURE_KINDS
        .iter()
        .position(|k| *k == state.new_picture_kind());

    let add = widget::button::standard(fl!("tagger-cover-add"))
        .leading_icon(widget::icon::from_name("insert-image-symbolic").handle())
        .on_press_maybe(editable.then_some(Msg::AddPicture));
    let add_as: Element<'a> = widget::Row::new()
        .push(add)
        .push(style::muted(fl!("tagger-cover-add-as")))
        .push(widget::dropdown(kind_labels, new_kind, Msg::NewPictureKind))
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center)
        .into();

    let revert = widget::button::standard(fl!("tagger-cover-revert"))
        .leading_icon(widget::icon::from_name("edit-undo-symbolic").handle())
        .on_press_maybe(editable.then_some(Msg::RevertPictures));

    let release_loaded = state
        .lookup()
        .is_some_and(|lookup| lookup.release.ready().is_some());
    let fetch = widget::button::standard(fl!("tagger-cover-fetch"))
        .leading_icon(widget::icon::from_name("emblem-downloads-symbolic").handle())
        .on_press_maybe((editable && release_loaded).then_some(Msg::FetchCover));

    let mut column = widget::Column::new()
        .push(
            widget::flex_row(vec![add_as, revert.into(), fetch.into()])
                .row_spacing(spacing.space_xs)
                .column_spacing(spacing.space_s),
        )
        .spacing(spacing.space_xxs);
    if !release_loaded {
        column = column.push(style::hint(fl!("tagger-cover-fetch-hint")));
    }
    column.into()
}

/// The cover art card.
pub fn panel<'a>(state: &'a TaggerState, thumbs: &'a Thumbs, facts: &Facts) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let title = fl!("tagger-cover-title");

    if state.selected_count() == 0 {
        return style::section(
            title,
            None,
            style::hint(fl!("tagger-metadata-empty-hint")).into(),
        );
    }

    let editable = facts.selection_editable;
    let mut body = widget::Column::new().spacing(spacing.space_s);
    if state.pictures().is_empty() {
        body = body.push(style::hint(fl!("tagger-cover-empty")));
    } else {
        let cards: Vec<Element<'a>> = state
            .pictures()
            .iter()
            .map(|picture| picture_card(thumbs, picture, editable))
            .collect();
        body = body.push(
            widget::flex_row(cards)
                .row_spacing(spacing.space_xs)
                .column_spacing(spacing.space_xs),
        );
    }
    body = body.push(controls(state, editable));

    style::section(title, None, body.into())
}
