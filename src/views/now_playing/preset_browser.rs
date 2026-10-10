// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Preset browser overlay for the ProjectM visualizer (behind the
//! `visualizer` feature flag). Lists every discovered `.milk` preset,
//! grouped by category, with a live search filter, click-to-load, and
//! playback controls (next/lock/beat sensitivity). Stacked as the topmost
//! layer over the visualizer in `expanded_view::expanded_now_playing`.

use super::NowPlayingMessage;
use super::expanded_view::{BACKDROP_SUBTEXT, BACKDROP_TEXT};
use super::visualizer::PresetEntry;
use crate::fl;
use crate::views::common;
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::widget::Stack;
use cosmic::iced::{Alignment, Color, Length};
use cosmic::widget;
use cosmic::widget::button::Style as ButtonStyle;
use cosmic::widget::tooltip::Position as TooltipPosition;
use std::path::Path;

/// Height of the scrollable preset list — the "fixed height + scrollable"
/// half of the panel's approximate 70%-of-viewport cap (the other half is
/// the header/divider/controls, which size to content).
const LIST_HEIGHT: f32 = 380.0;
const PANEL_MAX_WIDTH: f32 = 720.0;
/// Height of a preset button, and of its row slot (button + 2 px gap). Every
/// list line is exactly one `ROW_STRIDE` tall so the visible window follows
/// from the scroll offset by arithmetic.
const ROW_HEIGHT: f32 = 30.0;
const ROW_STRIDE: f32 = ROW_HEIGHT + 2.0;
/// Extra rows built beyond each viewport edge so fast scrolling never
/// shows a blank edge before the next rebuild.
const OVERSCAN_ROWS: usize = 3;

/// List-row style for preset rows. Unlike `list_row_button_class` (tuned
/// for rows on the app's normal themed surface), this panel always sits on
/// a fixed dark backdrop regardless of the active COSMIC theme, so
/// unselected rows use the fixed `BACKDROP_TEXT`/`BACKDROP_SUBTEXT` pair
/// instead of theme on-background colors, which could render near-black
/// (and unreadable) in a light theme.
fn preset_row_class(selected: bool) -> cosmic::theme::Button {
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.2).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: None,
                    text_color: Some(BACKDROP_TEXT),
                    icon_color: Some(BACKDROP_TEXT),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        hovered: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            if selected {
                let accent = cosmic.accent_color();
                ButtonStyle {
                    background: Some(Background::Color(accent.with_alpha(0.28).into())),
                    text_color: Some(accent.into()),
                    icon_color: Some(accent.into()),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            } else {
                ButtonStyle {
                    background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.08))),
                    text_color: Some(BACKDROP_TEXT),
                    icon_color: Some(BACKDROP_TEXT),
                    border_radius: cosmic.corner_radii.radius_s.into(),
                    ..ButtonStyle::new()
                }
            }
        }),
        pressed: Box::new(move |_focused, theme| {
            let cosmic = theme.cosmic();
            ButtonStyle {
                background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.14))),
                text_color: Some(BACKDROP_TEXT),
                icon_color: Some(BACKDROP_TEXT),
                border_radius: cosmic.corner_radii.radius_s.into(),
                ..ButtonStyle::new()
            }
        }),
        disabled: Box::new(|theme| ButtonStyle {
            background: None,
            text_color: Some(BACKDROP_SUBTEXT),
            icon_color: Some(BACKDROP_SUBTEXT),
            border_radius: theme.cosmic().corner_radii.radius_s.into(),
            ..ButtonStyle::new()
        }),
    }
}

/// Bare icon button with a caption tooltip, styled for this panel's fixed
/// dark backdrop. Takes an owned `String` label (rather than
/// `common::icon_button`'s borrowed `&'a str`) so it can be built from
/// `fl!()` — see `expanded_view::transport_button`'s doc comment for why
/// the borrowed signature can't take an `fl!()` value here.
fn panel_icon_button<'a>(
    icon_name: &'static str,
    icon_size: u16,
    label: String,
    on_press: NowPlayingMessage,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let button =
        widget::button::icon(widget::icon::from_name(icon_name).size(icon_size)).on_press(on_press);
    widget::tooltip(button, widget::text::caption(label), TooltipPosition::Top).into()
}

/// One line of the flattened, filtered list: a category heading or a
/// preset, both identified by their index into the full entries slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Heading for the category of `entries[i]`.
    Category(usize),
    Preset(usize),
}

/// Whether `entry` matches every whitespace-separated word of `query`
/// (case-insensitive substring of `"category name"`). An empty/blank query
/// matches everything.
fn matches_query(entry: &PresetEntry, words: &[String]) -> bool {
    words.iter().all(|w| entry.search_key.contains(w.as_str()))
}

/// Flattens `entries` (already sorted by category) into display rows,
/// dropping non-matching presets and emitting a heading whenever the
/// category changes between two *shown* presets.
pub fn build_rows(entries: &[PresetEntry], query: &str) -> Vec<Row> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut rows = Vec::new();
    let mut last_category: Option<&str> = None;
    for (i, entry) in entries.iter().enumerate() {
        if !matches_query(entry, &words) {
            continue;
        }
        if last_category != Some(entry.category.as_str()) {
            rows.push(Row::Category(i));
            last_category = Some(entry.category.as_str());
        }
        rows.push(Row::Preset(i));
    }
    rows
}

/// Position in `rows` of the preset whose *path* is `current` — never its
/// name: the same stem exists in several category directories.
pub fn current_row(rows: &[Row], entries: &[PresetEntry], current: Option<&Path>) -> Option<usize> {
    let current = current?;
    rows.iter()
        .position(|row| matches!(row, Row::Preset(i) if entries[*i].path == current))
}

/// Scroll offset that puts `row` in the vertical middle of the viewport.
pub fn scroll_offset_for_row(row: usize) -> f32 {
    (row as f32 * ROW_STRIDE - (LIST_HEIGHT - ROW_STRIDE) / 2.0).max(0.0)
}

/// Widget id of the list's scrollable (target of `scroll_to`).
pub fn list_scroll_id() -> widget::Id {
    widget::Id::new("aulos-viz-preset-list")
}

/// A blank block standing in for `rows` unbuilt rows.
fn row_spacer<'a>(rows: usize) -> cosmic::Element<'a, NowPlayingMessage> {
    widget::Space::new()
        .width(Length::Fill)
        .height(Length::Fixed(rows as f32 * ROW_STRIDE))
        .into()
}

/// Build the full preset browser overlay: a dimming backdrop (click to
/// close) beneath a centered panel with a search header, a scrollable
/// category-grouped preset list, and a next/lock/beat-sensitivity controls
/// row.
///
/// The list is virtualized — only the rows intersecting the viewport (at
/// `scroll_offset`) are built — because the stock projectM install has
/// ~4000 presets and `view()` re-runs at the visualizer's 30 fps.
pub fn preset_browser_overlay<'a>(
    entries: &'a [PresetEntry],
    scanned: bool,
    search: &'a str,
    scroll_offset: f32,
    current_preset: Option<&'a Path>,
    locked: bool,
    beat_sensitivity: f32,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let space_xs = f32::from(spacing.space_xs);
    let space_s = f32::from(spacing.space_s);
    let space_m = f32::from(spacing.space_m);

    let header = widget::Row::new()
        .push(
            widget::text::title3(fl!("viz-presets"))
                .class(cosmic::theme::Text::Color(BACKDROP_TEXT)),
        )
        .push(
            widget::search_input(fl!("viz-preset-search"), search)
                .on_input(NowPlayingMessage::PresetSearchInput)
                .on_clear(NowPlayingMessage::PresetSearchInput(String::new()))
                .width(Length::Fill),
        )
        .push(panel_icon_button(
            "window-close-symbolic",
            20,
            fl!("viz-close-presets"),
            NowPlayingMessage::TogglePresetBrowser,
        ))
        .spacing(space_s)
        .align_y(Alignment::Center);

    let rows = build_rows(entries, search);
    let range = common::visible_range(
        scroll_offset,
        0.0,
        LIST_HEIGHT,
        rows.len(),
        ROW_STRIDE,
        OVERSCAN_ROWS,
    );
    let mut list = widget::Column::new();
    if range.start > 0 {
        list = list.push(row_spacer(range.start));
    }
    for row in &rows[range.clone()] {
        let element: cosmic::Element<'a, NowPlayingMessage> = match *row {
            Row::Category(i) => widget::container(
                widget::text::caption(entries[i].category.as_str())
                    .class(cosmic::theme::Text::Color(BACKDROP_SUBTEXT)),
            )
            .height(Length::Fixed(ROW_STRIDE))
            .align_y(Vertical::Bottom)
            .padding([0.0, 0.0, 2.0, 0.0])
            .into(),
            Row::Preset(i) => {
                let entry = &entries[i];
                let is_active = current_preset == Some(entry.path.as_path());
                let button = widget::button::custom(
                    widget::container(
                        widget::text::body(entry.name.as_str()).wrapping(Wrapping::None),
                    )
                    .height(Length::Fill)
                    .align_y(Vertical::Center),
                )
                .on_press(NowPlayingMessage::LoadVizPreset(entry.path.clone()))
                .padding([0.0, space_s])
                .width(Length::Fill)
                .height(Length::Fixed(ROW_HEIGHT))
                .class(preset_row_class(is_active));
                widget::container(button)
                    .height(Length::Fixed(ROW_STRIDE))
                    .align_y(Vertical::Top)
                    .into()
            }
        };
        list = list.push(element);
    }
    if range.end < rows.len() {
        list = list.push(row_spacer(rows.len() - range.end));
    }
    if rows.is_empty() {
        let message = if !search.trim().is_empty() {
            fl!("viz-preset-empty")
        } else if scanned {
            fl!("viz-preset-none-found")
        } else {
            fl!("viz-preset-scanning")
        };
        list = list.push(
            widget::container(
                widget::Column::new()
                    .push(
                        widget::icon::from_name("edit-find-symbolic")
                            .size(32)
                            .icon(),
                    )
                    .push(
                        widget::text::body(message)
                            .class(cosmic::theme::Text::Color(BACKDROP_SUBTEXT)),
                    )
                    .spacing(space_xs)
                    .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .padding(space_m)
            .align_x(Horizontal::Center),
        );
    }

    let scroll = widget::scrollable(widget::container(list).width(Length::Fill))
        .id(list_scroll_id())
        .on_scroll(|viewport| NowPlayingMessage::PresetListScrolled(viewport.absolute_offset().y))
        .height(Length::Fixed(LIST_HEIGHT));

    // Next + lock on the left, beat sensitivity pushed to the right.
    let controls = widget::Row::new()
        .push(panel_icon_button(
            "media-skip-forward-symbolic",
            20,
            fl!("viz-next-preset"),
            NowPlayingMessage::NextPreset,
        ))
        .push(widget::toggler(locked).on_toggle(NowPlayingMessage::SetVizLocked))
        .push(widget::text::body(fl!("viz-lock")).class(cosmic::theme::Text::Color(BACKDROP_TEXT)))
        .push(
            widget::Space::new()
                .width(Length::Fill)
                .height(Length::Shrink),
        )
        .push(
            widget::text::body(fl!("viz-beat-sensitivity"))
                .class(cosmic::theme::Text::Color(BACKDROP_TEXT)),
        )
        .push(
            widget::slider(
                0.0..=2.0,
                beat_sensitivity,
                NowPlayingMessage::SetVizBeatSensitivity,
            )
            .step(0.05_f32)
            .width(Length::Fixed(160.0)),
        )
        .spacing(space_s)
        .align_y(Alignment::Center);

    let panel_col = widget::Column::new()
        .push(header)
        .push(widget::divider::horizontal::default())
        .push(scroll)
        .push(widget::divider::horizontal::default())
        .push(controls)
        .spacing(space_s)
        .width(Length::Fill);

    let panel = widget::container(panel_col)
        .padding(space_m)
        .width(Length::Fill)
        .max_width(PANEL_MAX_WIDTH)
        .class(cosmic::theme::Container::custom(|theme| {
            cosmic::iced::widget::container::Style {
                background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.62).into()),
                text_color: Some(BACKDROP_TEXT),
                border: cosmic::iced::Border {
                    radius: theme.cosmic().corner_radii.radius_l.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }));

    // Swallows presses anywhere on the panel (including its own blank
    // background — `MouseArea::update` calls `shell.capture_event()`
    // unconditionally on a left press within its bounds) so they never
    // fall through to the backdrop's close-on-click-outside handler below.
    // Interactive children (buttons, the search input, the scrollable)
    // still get their own clicks first, unaffected.
    let panel_swallow = widget::mouse_area(panel);

    let centered_panel = widget::container(panel_swallow)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .padding(space_m);

    let backdrop = widget::mouse_area(
        widget::container(widget::Space::new().width(0).height(0))
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(|_theme| {
                cosmic::iced::widget::container::Style {
                    background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.45).into()),
                    ..Default::default()
                }
            })),
    )
    .on_press(NowPlayingMessage::TogglePresetBrowser);

    Stack::new().push(backdrop).push(centered_panel).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(category: &str, name: &str) -> PresetEntry {
        PresetEntry::from_path(PathBuf::from(format!(
            "/presets/presets_{category}/{name}.milk"
        )))
    }

    /// Same stem in three categories, like the stock projectM install.
    fn sample() -> Vec<PresetEntry> {
        vec![
            entry("milkdrop", "Geiss - Eddies 2"),
            entry("milkdrop", "Rovastar - Fractal"),
            entry("stock", "Geiss - Eddies 2"),
            entry("tryptonaut", "Geiss - Eddies 2"),
            entry("tryptonaut", "Zylot - Aurora"),
        ]
    }

    fn presets(rows: &[Row]) -> Vec<usize> {
        rows.iter()
            .filter_map(|r| match r {
                Row::Preset(i) => Some(*i),
                Row::Category(_) => None,
            })
            .collect()
    }

    #[test]
    fn empty_query_lists_everything_with_one_heading_per_category() {
        let entries = sample();
        let rows = build_rows(&entries, "  ");
        assert_eq!(presets(&rows), vec![0, 1, 2, 3, 4]);
        let headings: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Category(i) => Some(*i),
                Row::Preset(_) => None,
            })
            .collect();
        assert_eq!(headings, vec![0, 2, 3]);
        assert_eq!(rows[0], Row::Category(0));
        assert_eq!(rows[1], Row::Preset(0));
    }

    #[test]
    fn query_is_case_insensitive_and_every_word_must_match() {
        let entries = sample();
        assert_eq!(presets(&build_rows(&entries, "GEISS")), vec![0, 2, 3]);
        // Words may appear in any order and across category + name.
        assert_eq!(presets(&build_rows(&entries, "eddies stock")), vec![2]);
        assert_eq!(presets(&build_rows(&entries, "stock eddies")), vec![2]);
        assert!(build_rows(&entries, "geiss fractal").is_empty());
    }

    #[test]
    fn category_names_match_and_headings_follow_only_shown_presets() {
        let entries = sample();
        let rows = build_rows(&entries, "tryptonaut");
        assert_eq!(presets(&rows), vec![3, 4]);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], Row::Category(3));

        // A match that skips a category's first preset still gets a heading.
        let rows = build_rows(&entries, "aurora");
        assert_eq!(rows, vec![Row::Category(4), Row::Preset(4)]);
    }

    #[test]
    fn current_row_matches_by_path_not_by_name() {
        let entries = sample();
        let rows = build_rows(&entries, "");
        // entries[2] and entries[3] share the stem of entries[0].
        let playing = entries[3].path.clone();
        let pos = current_row(&rows, &entries, Some(&playing)).unwrap();
        assert_eq!(rows[pos], Row::Preset(3));
        assert_eq!(
            rows.iter()
                .filter(|r| matches!(r, Row::Preset(i) if entries[*i].path == playing))
                .count(),
            1
        );
        assert_eq!(current_row(&rows, &entries, None), None);
        assert_eq!(
            current_row(&rows, &entries, Some(Path::new("/elsewhere.milk"))),
            None
        );
    }

    #[test]
    fn current_row_is_none_when_the_playing_preset_is_filtered_out() {
        let entries = sample();
        let rows = build_rows(&entries, "zylot");
        assert_eq!(current_row(&rows, &entries, Some(&entries[0].path)), None);
    }

    #[test]
    fn scroll_offset_centers_the_row_and_never_goes_negative() {
        assert_eq!(scroll_offset_for_row(0), 0.0);
        let y = scroll_offset_for_row(100);
        let row_top = 100.0 * ROW_STRIDE;
        assert!(y < row_top && y + LIST_HEIGHT > row_top + ROW_STRIDE);
    }

    #[test]
    fn virtualized_window_stays_small_for_thousands_of_rows() {
        let range =
            common::visible_range(50_000.0, 0.0, LIST_HEIGHT, 4_200, ROW_STRIDE, OVERSCAN_ROWS);
        assert!(range.len() <= 20, "built {} rows", range.len());
        // The row under the viewport's top edge is inside the window.
        let first_visible = (50_000.0 / ROW_STRIDE) as usize;
        assert!(range.contains(&first_visible));
    }
}
