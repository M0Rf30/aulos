// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Filter chips for the Albums page: kind (All / Albums / Compilations) and
//! release decade. The filter state, predicate and memo cache live in
//! `aulos_core::library::album_filter`; this module holds the chip message
//! and widgets.

use aulos_core::fl;
use aulos_core::library::album_filter::{AlbumFilter, AlbumFilterCache, AlbumKind};
use cosmic::widget;

/// A change to the chip selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMsg {
    Kind(AlbumKind),
    /// `Some(1990)` selects the 1990s; `None` clears the decade filter.
    Decade(Option<u32>),
}

impl FilterMsg {
    /// Applies this chip click to `filter`.
    pub fn apply(self, filter: &mut AlbumFilter) {
        match self {
            Self::Kind(kind) => filter.set_kind(kind),
            Self::Decade(decade) => filter.set_decade(decade),
        }
    }
}

/// What the albums view needs to apply the chip filters: the selection,
/// the memo cache, and the epoch identifying the albums slice it is shown.
pub struct FilterCtx<'a> {
    pub filter: &'a AlbumFilter,
    pub cache: &'a AlbumFilterCache,
    /// Changes whenever the albums slice handed to the view may differ
    /// (library reload/batch, search filter refresh, source switch).
    pub epoch: u64,
}

fn chip<'a>(label: String, selected: bool, on_press: FilterMsg) -> cosmic::Element<'a, FilterMsg> {
    let button = widget::button::text(label).on_press(on_press);
    if selected {
        button.class(cosmic::theme::Button::Suggested).into()
    } else {
        button.class(cosmic::theme::Button::Standard).into()
    }
}

/// The chip rows: kind chips, then one chip per decade present (hidden when
/// no album has a year). The selected decade stays visible even if a search
/// currently excludes it, so it can always be cleared.
pub fn filter_bar<'a>(filter: &AlbumFilter, decades: &[u32]) -> cosmic::Element<'a, FilterMsg> {
    let spacing = cosmic::theme::active().cosmic().spacing;

    let mut chips: Vec<cosmic::Element<'a, FilterMsg>> = vec![
        chip(
            fl!("filter-all"),
            filter.kind == AlbumKind::All,
            FilterMsg::Kind(AlbumKind::All),
        ),
        chip(
            fl!("filter-albums"),
            filter.kind == AlbumKind::Albums,
            FilterMsg::Kind(AlbumKind::Albums),
        ),
        chip(
            fl!("filter-compilations"),
            filter.kind == AlbumKind::Compilations,
            FilterMsg::Kind(AlbumKind::Compilations),
        ),
    ];

    let mut decades = decades.to_vec();
    if let Some(selected) = filter.decade
        && !decades.contains(&selected)
    {
        decades.push(selected);
        decades.sort_unstable();
    }
    if !decades.is_empty() {
        chips.push(
            widget::container(widget::divider::vertical::default())
                .height(24)
                .into(),
        );
        for decade in decades {
            let selected = filter.decade == Some(decade);
            chips.push(chip(
                fl!("filter-decade", decade = decade.to_string()),
                selected,
                FilterMsg::Decade((!selected).then_some(decade)),
            ));
        }
    }

    widget::container(
        widget::flex_row(chips)
            .row_spacing(spacing.space_xxs)
            .column_spacing(spacing.space_xxs),
    )
    .padding([0, spacing.space_m + 16, spacing.space_xs, spacing.space_m])
    .into()
}
