// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The MusicBrainz panel: search terms, the candidate releases, and — once
//! a release is chosen — how the files line up with it.

use cosmic::iced::{Alignment, Length};
use cosmic::widget;

use crate::fl;
use crate::online::musicbrainz::ReleaseCandidate;
use crate::tagger::{LookupSource, LookupState, Remote, TaggerMessage as Msg, TaggerState};
use crate::views::common;

use super::TaggerViewProps;
use super::labels;
use super::style::{self, Tone};

type Element<'a> = cosmic::Element<'a, Msg>;

/// Height of one candidate row; fixed so the list's height is arithmetic.
const CANDIDATE_HEIGHT: f32 = 50.0;
/// Candidates shown before the list scrolls.
const VISIBLE_CANDIDATES: usize = 5;
/// Unmatched file names listed before "and N more".
const UNMATCHED_SHOWN: usize = 5;

fn busy_line<'a>(text: String) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    widget::Row::new()
        .push(style::spinner(16.0))
        .push(style::muted(text))
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center)
        .into()
}

fn query_row<'a>(lookup: &'a LookupState) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    widget::Row::new()
        .push(
            widget::text_input(fl!("tagger-lookup-artist"), lookup.artist.as_str())
                .on_input(Msg::QueryArtist)
                .on_submit(|_| Msg::SearchAgain)
                .width(Length::FillPortion(2)),
        )
        .push(
            widget::text_input(fl!("tagger-lookup-album"), lookup.album.as_str())
                .on_input(Msg::QueryAlbum)
                .on_submit(|_| Msg::SearchAgain)
                .width(Length::FillPortion(3)),
        )
        .push(
            widget::button::standard(fl!("tagger-lookup-search"))
                .leading_icon(widget::icon::from_name("edit-find-symbolic").handle())
                .on_press_maybe((!lookup.candidates.is_loading()).then_some(Msg::SearchAgain)),
        )
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center)
        .into()
}

fn candidate_row<'a>(candidate: &'a ReleaseCandidate, chosen: bool) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let title = if candidate.disambiguation.trim().is_empty() {
        candidate.title.clone()
    } else {
        format!("{} ({})", candidate.title, candidate.disambiguation)
    };

    let text = widget::Column::new()
        .push(common::cell_text(title).font(cosmic::font::semibold()))
        .push(style::muted(labels::candidate_details(candidate)))
        .spacing(1);

    let mut row = widget::Row::new()
        .push(common::clipped_cell(text.into()))
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center);
    if candidate.score > 0 {
        row = row.push(style::chip(
            None,
            format!("{}%", candidate.score),
            if chosen { Tone::Accent } else { Tone::Neutral },
        ));
    }

    widget::button::custom(row)
        .on_press(Msg::ChooseRelease(candidate.id.clone()))
        .width(Length::Fill)
        .height(Length::Fixed(CANDIDATE_HEIGHT))
        .padding([spacing.space_xxs, spacing.space_xs])
        .class(crate::views::list_row_button_class(chosen))
        .into()
}

fn candidates<'a>(lookup: &'a LookupState) -> Element<'a> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    match &lookup.candidates {
        Remote::Idle => style::hint(fl!("tagger-lookup-idle")).into(),
        Remote::Loading => busy_line(fl!("tagger-lookup-searching")),
        Remote::Failed(error) => style::tone_hint(
            fl!("tagger-lookup-failed", error = error.clone()),
            Tone::Danger,
        )
        .into(),
        Remote::Ready(items) if items.is_empty() => style::hint(fl!("tagger-lookup-none")).into(),
        Remote::Ready(items) => {
            let mut list = widget::Column::new().spacing(2);
            for candidate in items {
                let chosen = lookup.chosen.as_deref() == Some(candidate.id.as_str());
                list = list.push(candidate_row(candidate, chosen));
            }
            let rows = items.len().min(VISIBLE_CANDIDATES) as f32;
            widget::scrollable(widget::container(list).width(Length::Fill).padding([
                0,
                spacing.space_s,
                0,
                0,
            ]))
            .height(Length::Fixed(rows * (CANDIDATE_HEIGHT + 2.0)))
            .into()
        }
    }
}

/// The cover line under an applied release.
fn cover_line<'a>(lookup: &LookupState, props: TaggerViewProps) -> Option<Element<'a>> {
    match &lookup.cover {
        Remote::Idle if !props.use_caa_cover => {
            Some(style::hint(fl!("tagger-lookup-cover-off")).into())
        }
        Remote::Idle => None,
        Remote::Loading => Some(busy_line(fl!("tagger-lookup-cover-loading"))),
        Remote::Failed(error) => Some(
            style::tone_hint(
                fl!("tagger-lookup-cover-failed", error = error.clone()),
                Tone::Warning,
            )
            .into(),
        ),
        Remote::Ready(None) => Some(style::hint(fl!("tagger-lookup-cover-none")).into()),
        Remote::Ready(Some(_)) => {
            Some(style::tone_hint(fl!("tagger-lookup-cover-found"), Tone::Success).into())
        }
    }
}

/// How the files line up with the chosen release, and what can be done
/// with it.
fn release_state<'a>(
    state: &TaggerState,
    lookup: &'a LookupState,
    props: TaggerViewProps,
) -> Option<Element<'a>> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    match &lookup.release {
        Remote::Idle => None,
        Remote::Loading => Some(busy_line(fl!("tagger-lookup-loading-release"))),
        Remote::Failed(error) => Some(
            style::tone_hint(
                fl!("tagger-lookup-release-failed", error = error.clone()),
                Tone::Danger,
            )
            .into(),
        ),
        Remote::Ready(release) => {
            let mut column = widget::Column::new().spacing(spacing.space_xs);

            if let Some(plan) = &lookup.plan {
                column = column.push(
                    widget::Row::new()
                        .push(style::tone_icon("emblem-ok-symbolic", 16, Tone::Success))
                        .push(
                            widget::text::body(fl!(
                                "tagger-lookup-matched",
                                matched = plan.matched(),
                                total = plan.entries.len()
                            ))
                            .font(cosmic::font::semibold()),
                        )
                        .spacing(spacing.space_xs)
                        .align_y(Alignment::Center),
                );
                if plan.unused_tracks() > 0 {
                    column = column.push(style::hint(fl!(
                        "tagger-lookup-unused",
                        count = plan.unused_tracks()
                    )));
                }
                let names: Vec<String> = plan
                    .unmatched()
                    .into_iter()
                    .filter_map(|id| state.file(id))
                    .map(|f| f.file_name())
                    .collect();
                if !names.is_empty() {
                    let (shown, more) = labels::name_list(&names, UNMATCHED_SHOWN);
                    let text = if more == 0 {
                        fl!("tagger-lookup-unmatched", files = shown)
                    } else {
                        fl!("tagger-lookup-unmatched-more", files = shown, more = more)
                    };
                    column = column.push(style::tone_hint(text, Tone::Warning));
                }
                if let Some(line) = cover_line(lookup, props) {
                    column = column.push(line);
                }
            }

            let mut actions = widget::Row::new()
                .push(
                    widget::button::standard(fl!("tagger-lookup-open"))
                        .leading_icon(widget::icon::from_name("web-browser-symbolic").handle())
                        .on_press(Msg::OpenReleasePage(release.id.clone())),
                )
                .spacing(spacing.space_xs);
            if lookup.applied() {
                actions = actions.push(
                    widget::button::standard(fl!("tagger-lookup-discard"))
                        .leading_icon(widget::icon::from_name("edit-undo-symbolic").handle())
                        .on_press(Msg::DiscardRelease),
                );
            }
            Some(column.push(actions).into())
        }
    }
}

/// The lookup card, or `None` while no lookup is open.
pub fn panel<'a>(state: &'a TaggerState, props: TaggerViewProps) -> Option<Element<'a>> {
    let lookup = state.lookup()?;
    let spacing = cosmic::theme::active().cosmic().spacing;

    let title = match lookup.source {
        LookupSource::Search => fl!("tagger-lookup-title"),
        LookupSource::AcoustId => fl!("tagger-lookup-title-acoustid"),
    };

    let mut body = widget::Column::new()
        .spacing(spacing.space_s)
        .push(style::muted(fl!(
            "tagger-lookup-files",
            count = lookup.targets.len()
        )));
    if lookup.source == LookupSource::AcoustId
        && let Some(choice) = &lookup.acoustid_choice
    {
        body = body.push(style::tone_hint(
            fl!(
                "tagger-lookup-acoustid-summary",
                matched = choice.covered.len(),
                total = lookup.targets.len(),
                release = choice.title.clone()
            ),
            Tone::Accent,
        ));
    }
    body = body.push(query_row(lookup)).push(candidates(lookup));
    if let Some(release) = release_state(state, lookup, props) {
        body = body.push(style::divider()).push(release);
    }

    let close = widget::button::standard(fl!("tagger-lookup-close"))
        .leading_icon(widget::icon::from_name("window-close-symbolic").handle())
        .on_press(Msg::CloseLookup);
    Some(style::section(title, Some(close.into()), body.into()))
}
