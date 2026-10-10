// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Filter chips for the Albums page: kind (All / Albums / Compilations) and
//! release decade. The state is tiny and the predicate pure, so the albums
//! view recomputes the visible index list each frame instead of caching
//! cloned album data in the model.

use crate::fl;
use crate::library::Album;
use crate::library::compilations::decade_of;
use cosmic::widget;

/// Which kind of album to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlbumKind {
    #[default]
    All,
    /// Everything that is not a compilation.
    Albums,
    /// "Various Artists" compilations only.
    Compilations,
}

/// A change to the chip selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMsg {
    Kind(AlbumKind),
    /// `Some(1990)` selects the 1990s; `None` clears the decade filter.
    Decade(Option<u32>),
}

/// Current chip selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlbumFilter {
    pub kind: AlbumKind,
    pub decade: Option<u32>,
}

impl AlbumFilter {
    pub fn is_active(&self) -> bool {
        self.kind != AlbumKind::All || self.decade.is_some()
    }

    pub fn update(&mut self, msg: FilterMsg) {
        match msg {
            FilterMsg::Kind(kind) => self.kind = kind,
            FilterMsg::Decade(decade) => self.decade = decade,
        }
    }

    pub fn matches(&self, album: &Album) -> bool {
        let kind_ok = match self.kind {
            AlbumKind::All => true,
            AlbumKind::Albums => !album.is_compilation(),
            AlbumKind::Compilations => album.is_compilation(),
        };
        kind_ok && self.decade.is_none_or(|d| decade_of(album.year) == Some(d))
    }

    /// Indices into `albums` of the albums passing the filter, in order.
    pub fn visible_indices(&self, albums: &[Album]) -> Vec<usize> {
        albums
            .iter()
            .enumerate()
            .filter(|(_, a)| self.matches(a))
            .map(|(i, _)| i)
            .collect()
    }
}

/// Distinct decades (ascending) present among `albums`.
pub fn decades_present(albums: &[Album]) -> Vec<u32> {
    let mut decades: Vec<u32> = albums.iter().filter_map(|a| decade_of(a.year)).collect();
    decades.sort_unstable();
    decades.dedup();
    decades
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
pub fn filter_bar<'a>(filter: &AlbumFilter, albums: &[Album]) -> cosmic::Element<'a, FilterMsg> {
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

    let mut decades = decades_present(albums);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn album(artist: &str, year: u32) -> Album {
        Album {
            name: "A".into(),
            artist: artist.into(),
            year,
            tracks: Vec::new(),
            cover_source: None,
        }
    }

    #[test]
    fn kind_filter() {
        let albums = vec![album("Band", 1994), album("Various Artists", 1999)];
        let mut f = AlbumFilter::default();
        assert!(!f.is_active());
        assert_eq!(f.visible_indices(&albums), vec![0, 1]);
        f.update(FilterMsg::Kind(AlbumKind::Compilations));
        assert_eq!(f.visible_indices(&albums), vec![1]);
        f.update(FilterMsg::Kind(AlbumKind::Albums));
        assert_eq!(f.visible_indices(&albums), vec![0]);
    }

    #[test]
    fn decade_filter_and_listing() {
        let albums = vec![
            album("A", 1994),
            album("B", 1989),
            album("C", 0),
            album("D", 1991),
        ];
        assert_eq!(decades_present(&albums), vec![1980, 1990]);
        let mut f = AlbumFilter::default();
        f.update(FilterMsg::Decade(Some(1990)));
        assert!(f.is_active());
        assert_eq!(f.visible_indices(&albums), vec![0, 3]);
        f.update(FilterMsg::Decade(None));
        assert_eq!(f.visible_indices(&albums).len(), 4);
    }
}
