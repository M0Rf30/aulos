// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Directory-hierarchy browse view.
//!
//! Complements the tag-driven views (Albums, Artists, Genres) for
//! collections whose metadata is sparse or wrong: browses the library by
//! its on-disk (or on-provider) folder structure instead of by tags. The
//! tree and the browse position live in `aulos_core::library::folder_tree`;
//! this module only lays them out.

use crate::views::common;
use crate::views::track_row;
use aulos_core::fl;
use aulos_core::format::truncate_str;
use aulos_core::library::Track;
use aulos_core::library::folder_tree::{self, FolderState, FolderTree};
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Messages from the folder browse view.
#[derive(Debug, Clone)]
pub enum FolderMessage {
    /// Descend into a child directory.
    Open(PathBuf),
    /// Move up to the parent of the current directory.
    Up,
    /// Jump to a breadcrumb segment by index.
    GoTo(usize),
    /// Play a specific track (index into `all_tracks`).
    PlayTrack(usize),
    /// Play every track under the current directory, recursively.
    PlayFolder,
    /// Queue every track under the current directory, recursively.
    QueueFolder,
    /// Toggle favorite status for a track (by track ID string).
    ToggleFavorite(String),
    /// Set rating (1-5) for a track. Pass 0 to clear.
    SetRating(String, u8),
    /// Jump to another view (artist page).
    Navigate(crate::views::Route),
    /// Open the Properties drawer for this track.
    ShowProperties(Track),
    /// Open the tag editor on this (local) track.
    Tag(Track, aulos_core::tagger::TagAction),
}

/// Fixed pixel size of the cover/placeholder in a subfolder tile.
const TILE_ART: f32 = 48.0;
/// Narrowest / widest a subfolder tile may be before the grid adds or drops
/// a column.
const TILE_MIN: f32 = 248.0;
const TILE_MAX: f32 = 420.0;
/// Cover/placeholder size in the folder header card.
const HEADER_ART: f32 = 96.0;
/// Horizontal space reserved on the right for the overlaid scrollbar.
const SCROLLBAR_CLEARANCE: f32 = 16.0;
/// At most this many breadcrumb pills are shown; deeper trails collapse
/// their middle into an ellipsis pill.
const MAX_CRUMBS: usize = 5;
/// Longest crumb label before it is shortened with an ellipsis.
const CRUMB_MAX_CHARS: usize = 28;
/// Render the folder browse view: breadcrumb pills and a folder header card
/// (art, counts, play/queue), then subfolder tiles, then the tracks stored
/// directly in the current directory.
///
/// `cover_images` is the app's album-cover cache; a folder borrows the first
/// cached cover found among its tracks as its artwork.
pub fn folder_view<'a>(
    state: &'a FolderState,
    tracks: &'a [Track],
    current_track: Option<&'a Track>,
    cover_images: &'a HashMap<String, widget::icon::Handle>,
) -> cosmic::Element<'a, FolderMessage> {
    if tracks.is_empty() {
        return common::empty_state(
            "folder-symbolic",
            fl!("no-folders"),
            fl!("folders-empty-hint"),
        );
    }

    widget::responsive(move |size| page(state, tracks, current_track, cover_images, size.width))
        .into()
}

/// The whole scrollable page, laid out for a viewport `width` pixels wide.
fn page<'a>(
    state: &'a FolderState,
    tracks: &'a [Track],
    current_track: Option<&'a Track>,
    covers: &'a HashMap<String, widget::icon::Handle>,
    width: f32,
) -> cosmic::Element<'a, FolderMessage> {
    let cosmic_theme = cosmic::theme::active();
    let spacing = cosmic_theme.cosmic().spacing;
    let radii = cosmic_theme.cosmic().corner_radii;
    let pad = f32::from(spacing.space_m);
    let gap = f32::from(spacing.space_s);

    let tree = state.tree();
    let current = state.effective_current();
    let children = tree.child_dirs(&current);
    let direct = tree.direct_tracks(&current);
    let has_any_here = !children.is_empty() || !direct.is_empty();
    let total_tracks = tree.track_count_in(&current, true);
    let total_secs = tree.total_secs_in(&current, tracks);

    let crumbs = state.visible_breadcrumbs();
    let title = crumbs
        .last()
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| fl!("folders-root"));
    let at_base = current == state.base();

    // --- Header card ------------------------------------------------------
    let art: cosmic::Element<'a, FolderMessage> = match tree.first_cover(&current, tracks, covers) {
        Some(handle) => common::cover_art(handle, HEADER_ART, radii.radius_m[0], true),
        None => folder_placeholder(HEADER_ART, 48),
    };

    let mut meta = widget::Column::new()
        .push(common::clipped_cell(
            widget::text::title2(title).wrapping(Wrapping::None).into(),
        ))
        .width(Length::Fill)
        .spacing(spacing.space_xxs);
    if !current.as_os_str().is_empty() {
        meta = meta.push(common::clipped_cell(
            common::cell_caption(current.to_string_lossy().into_owned()).into(),
        ));
    }
    meta = meta
        .push(common::cell_caption(folder_tree::summary(
            children.len(),
            total_tracks,
            total_secs,
        )))
        .push(
            widget::Row::new()
                .push(
                    widget::button::suggested(fl!("play-folder"))
                        .on_press_maybe(has_any_here.then_some(FolderMessage::PlayFolder)),
                )
                .push(
                    widget::button::standard(fl!("queue-folder-tooltip"))
                        .on_press_maybe(has_any_here.then_some(FolderMessage::QueueFolder)),
                )
                .spacing(spacing.space_xs),
        );

    let header = widget::container(
        widget::Row::new()
            .push(art)
            .push(meta)
            .spacing(spacing.space_m)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(spacing.space_m)
    .class(cosmic::theme::Container::Card);

    let mut content = widget::Column::new()
        .push(breadcrumb_bar(crumbs, !at_base))
        .push(header)
        .spacing(pad)
        .width(Length::Fill);

    // --- Subfolder tiles --------------------------------------------------
    if !children.is_empty() {
        let avail = (width - 2.0 * pad - SCROLLBAR_CLEARANCE).max(TILE_MIN);
        let columns = (((avail + gap) / (TILE_MIN + gap)).floor() as usize).max(1);
        let tile_width = ((avail - gap * (columns - 1) as f32) / columns as f32).min(TILE_MAX);

        let mut grid = widget::Column::new().spacing(gap);
        for chunk in children.chunks(columns) {
            let mut row = widget::Row::new().spacing(gap);
            for child in chunk {
                row = row.push(folder_tile(
                    tree, &current, child, tracks, covers, tile_width,
                ));
            }
            grid = grid.push(row);
        }
        content = content
            .push(section_title(fl!("folders"), children.len()))
            .push(grid);
    }

    // --- Tracks -----------------------------------------------------------
    if !direct.is_empty() {
        // Width the track table is actually laid out in.
        let list_width = width - 2.0 * pad - SCROLLBAR_CLEARANCE;
        let columns = track_row::Columns {
            artist: true,
            album: true,
            favorite: true,
            rating: true,
            quality: true,
            ..Default::default()
        }
        .responsive(list_width);
        let rows = direct.iter().enumerate().filter_map(|(position, &index)| {
            let track = tracks.get(index)?;
            let track_id = track.id.to_string();
            let rating_track_id = track_id.clone();
            Some(
                track_row::TrackRow::new(
                    track,
                    (position + 1).to_string(),
                    current_track.map(|t| t.id) == Some(track.id),
                    columns,
                    FolderMessage::PlayTrack(index),
                )
                .with_navigate(FolderMessage::Navigate)
                .with_favorite(FolderMessage::ToggleFavorite(track_id))
                .with_rating(move |r| FolderMessage::SetRating(rating_track_id.clone(), r))
                .with_properties(FolderMessage::ShowProperties(track.clone()))
                .with_tagging(FolderMessage::Tag)
                .with_artist_subtitle(!columns.artist)
                .view(),
            )
        });
        content = content
            .push(section_title(fl!("folder-section-tracks"), direct.len()))
            .push(track_row::rows_column(
                Some(track_row::Header::new(columns)),
                rows,
            ));
    }

    if !has_any_here {
        content = content.push(
            widget::container(
                widget::Column::new()
                    .push(widget::icon::from_name("folder-symbolic").size(48))
                    .push(widget::text::title4(fl!("folder-empty")))
                    .push(widget::text::body(fl!("folder-empty-hint")))
                    .spacing(spacing.space_xs)
                    .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .padding(spacing.space_xl)
            .align_x(Horizontal::Center),
        );
    }

    widget::scrollable(widget::container(content).width(Length::Fill).padding(
        cosmic::iced::Padding {
            top: gap,
            right: pad + SCROLLBAR_CLEARANCE,
            bottom: pad,
            left: pad,
        },
    ))
    .height(Length::Fill)
    .into()
}

/// Section label with a dim item count, e.g. "Folders  12".
fn section_title<'a>(title: String, count: usize) -> cosmic::Element<'a, FolderMessage> {
    widget::Row::new()
        .push(widget::text::title4(title))
        .push(common::cell_caption(count.to_string()))
        .spacing(cosmic::theme::active().cosmic().spacing.space_xs)
        .align_y(Alignment::Center)
        .into()
}

/// Card-framed folder icon used wherever a folder has no cached cover.
fn folder_placeholder<'a>(size: f32, icon_size: u16) -> cosmic::Element<'a, FolderMessage> {
    widget::container(widget::icon::from_name("folder-symbolic").size(icon_size))
        .width(size)
        .height(size)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .class(cosmic::theme::Container::Card)
        .into()
}

/// Style for a breadcrumb pill. The current crumb is the (disabled)
/// filled pill; the rest are quiet until hovered.
fn crumb_class() -> cosmic::theme::Button {
    fn style(
        theme: &cosmic::Theme,
        background: Option<cosmic::iced::Color>,
        text: cosmic::iced::Color,
    ) -> ButtonStyle {
        ButtonStyle {
            background: background.map(Background::Color),
            text_color: Some(text),
            icon_color: Some(text),
            border_radius: theme.cosmic().corner_radii.radius_xl.into(),
            ..ButtonStyle::new()
        }
    }
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| {
            style(theme, None, theme.cosmic().palette.neutral_7.into())
        }),
        hovered: Box::new(|_focused, theme| {
            let comp = &theme.cosmic().background(false).component;
            style(theme, Some(comp.hover.into()), comp.on.into())
        }),
        pressed: Box::new(|_focused, theme| {
            let comp = &theme.cosmic().background(false).component;
            style(theme, Some(comp.pressed.into()), comp.on.into())
        }),
        disabled: Box::new(|theme| {
            let comp = &theme.cosmic().background(false).component;
            style(theme, Some(comp.base.into()), comp.on.into())
        }),
    }
}

/// Style for a subfolder tile: a subtle filled card at rest, lifting on
/// hover.
fn tile_class() -> cosmic::theme::Button {
    fn style(theme: &cosmic::Theme, background: cosmic::iced::Color) -> ButtonStyle {
        let cosmic = theme.cosmic();
        ButtonStyle {
            background: Some(Background::Color(background)),
            text_color: Some(cosmic.background(false).component.on.into()),
            icon_color: Some(cosmic.background(false).component.on.into()),
            border_radius: cosmic.corner_radii.radius_m.into(),
            ..ButtonStyle::new()
        }
    }
    cosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| {
            style(
                theme,
                theme.cosmic().background(false).component.base.into(),
            )
        }),
        hovered: Box::new(|_focused, theme| {
            style(
                theme,
                theme.cosmic().background(false).component.hover.into(),
            )
        }),
        pressed: Box::new(|_focused, theme| {
            style(
                theme,
                theme.cosmic().background(false).component.pressed.into(),
            )
        }),
        disabled: Box::new(|theme| {
            style(
                theme,
                theme.cosmic().background(false).component.base.into(),
            )
        }),
    }
}

/// Pill-style breadcrumb trail with an optional "up one level" button. The
/// last crumb is the current folder (emphasised, not clickable). Long
/// trails collapse their middle into a single "…" pill that jumps to the
/// nearest hidden ancestor.
fn breadcrumb_bar<'a>(
    crumbs: Vec<(PathBuf, String)>,
    can_go_up: bool,
) -> cosmic::Element<'a, FolderMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let mut bar = widget::Row::new()
        .spacing(spacing.space_xxs)
        .align_y(Alignment::Center);

    if can_go_up {
        bar = bar.push(widget::tooltip(
            widget::button::icon(widget::icon::from_name("go-up-symbolic").size(16))
                .on_press(FolderMessage::Up),
            widget::text::caption(fl!("folders-up")),
            widget::tooltip::Position::Bottom,
        ));
    }

    let count = crumbs.len();
    let last = count.saturating_sub(1);
    // Index range of crumbs hidden behind the ellipsis pill.
    let hidden = if count > MAX_CRUMBS {
        1..(count - (MAX_CRUMBS - 2))
    } else {
        0..0
    };

    for (index, (path, label)) in crumbs.into_iter().enumerate() {
        if hidden.contains(&index) {
            if index + 1 == hidden.end {
                // Last hidden crumb doubles as the ellipsis target.
                bar = bar.push(chevron()).push(
                    widget::button::custom(common::cell_text("\u{2026}"))
                        .padding([4, 10])
                        .on_press(FolderMessage::Open(path))
                        .class(crumb_class()),
                );
            }
            continue;
        }
        if index > 0 {
            bar = bar.push(chevron());
        }
        let is_current = index == last;
        let text = truncate_str(&label, CRUMB_MAX_CHARS);
        let content: cosmic::Element<'a, FolderMessage> = if index == 0 {
            widget::Row::new()
                .push(widget::icon::from_name("folder-music-symbolic").size(16))
                .push(crumb_text(text, is_current))
                .spacing(spacing.space_xxs)
                .align_y(Alignment::Center)
                .into()
        } else {
            crumb_text(text, is_current).into()
        };
        bar = bar.push(
            widget::button::custom(content)
                .padding([4, 12])
                .on_press_maybe((!is_current).then_some(FolderMessage::Open(path)))
                .class(crumb_class()),
        );
    }

    bar.into()
}

fn crumb_text<'a>(label: String, emphasised: bool) -> common::Text<'a> {
    let text = common::cell_text(label);
    if emphasised {
        text.font(cosmic::font::semibold())
    } else {
        text
    }
}

fn chevron<'a>() -> cosmic::Element<'a, FolderMessage> {
    widget::icon::from_name("go-next-symbolic").size(12).into()
}

/// One subfolder tile: cover (or folder icon), name and counts. Chains of
/// single-child folders are collapsed, so the tile opens the first
/// directory that actually branches or holds tracks.
fn folder_tile<'a>(
    tree: &FolderTree,
    parent: &Path,
    child: &Path,
    tracks: &[Track],
    covers: &'a HashMap<String, widget::icon::Handle>,
    width: f32,
) -> cosmic::Element<'a, FolderMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let target = tree.collapse(child);
    let label = folder_tree::chain_label(parent, &target);
    let track_count = tree.track_count_in(&target, true);
    let sub_count = tree.child_dirs(&target).len();

    let caption = folder_tree::summary(sub_count, track_count, 0);

    let art: cosmic::Element<'a, FolderMessage> = match tree.first_cover(&target, tracks, covers) {
        Some(handle) => common::cover_art(
            handle,
            TILE_ART,
            cosmic::theme::active().cosmic().corner_radii.radius_s[0],
            false,
        ),
        None => folder_placeholder(TILE_ART, 24),
    };

    let labels = widget::Column::new()
        .push(common::clipped_cell(common::cell_text(label).into()))
        .push(common::clipped_cell(common::cell_caption(caption).into()))
        .spacing(2)
        .width(Length::Fill);

    widget::button::custom(
        widget::Row::new()
            .push(art)
            .push(labels)
            .spacing(spacing.space_s)
            .align_y(Alignment::Center),
    )
    .padding(spacing.space_xs)
    .width(Length::Fixed(width))
    .on_press(FolderMessage::Open(target))
    .class(tile_class())
    .into()
}
