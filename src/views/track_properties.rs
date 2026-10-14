// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Properties drawer: technical details of one track, or the aggregate of
//! one album.
//!
//! The view is independent of `AppModel`: it takes the subject plus the
//! (possibly still loading) [`TechInfo`] / [`AlbumSummary`] and emits its own
//! [`PropertiesMessage`]. The drawer already supplies padding and scrolling,
//! so the views return a plain column of sections.
//!
//! A value nobody knows is always rendered as an em dash ([`MISSING`]),
//! never as `0`, because a remote server or a stream legitimately reports
//! only a subset of what a local file can tell.

use aulos_core::fl;
use aulos_core::format::{format_bitrate, format_duration_exact, format_sample_rate, format_size};
use aulos_core::library::properties::{
    Loadable, MISSING, SourceKind, format_bit_depth, format_bitrate_line, format_bitrate_range,
    format_channels, format_codec, format_file_size, format_year_range, known_from_track,
    list_or_dash, merge, number_or_dash, source_kind, source_label, text_or_dash,
};
use aulos_core::library::tech_info::{AlbumSummary, TechInfo};
use aulos_core::library::{Album, Track};
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use std::path::PathBuf;

/// Messages emitted by the Properties drawer.
#[derive(Debug, Clone)]
pub enum PropertiesMessage {
    /// Open the folder containing this file in the file manager.
    OpenContainingFolder(PathBuf),
    /// Open this (local) track in the tag editor.
    EditTags(Box<Track>),
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

type Element<'a> = cosmic::Element<'a, PropertiesMessage>;

fn dim_text() -> cosmic::theme::Text {
    cosmic::theme::Text::Color(cosmic::theme::active().cosmic().palette.neutral_7.into())
}

/// One `label  value` line. The value is selectable (right-click → Copy),
/// since users copy paths, MD5s and encoder strings out of this drawer.
fn row<'a>(label: String, value: String) -> Element<'a> {
    let sp = cosmic::theme::active().cosmic().spacing;
    widget::Row::new()
        .push(
            widget::text::body(label)
                .class(dim_text())
                .width(Length::FillPortion(2)),
        )
        .push(
            widget::selectable_text::body(value)
                .wrapping(Wrapping::WordOrGlyph)
                .width(Length::FillPortion(3)),
        )
        .spacing(sp.space_s)
        .align_y(Alignment::Start)
        .width(Length::Fill)
        .into()
}

fn section<'a>(title: String, rows: Vec<Element<'a>>) -> Element<'a> {
    rows.into_iter()
        .fold(widget::settings::section().title(title), |s, r| s.add(r))
        .into()
}

/// Spinner while loading, the reason when loading failed, nothing otherwise.
fn status_banner<'a, T>(state: &Loadable<T>) -> Option<Element<'a>> {
    let sp = cosmic::theme::active().cosmic().spacing;
    match state {
        Loadable::Loading => Some(
            widget::Row::new()
                .push(widget::indeterminate_circular().size(14.0).bar_height(2.0))
                .push(widget::text::caption(fl!("props-loading")).class(dim_text()))
                .spacing(sp.space_xs)
                .align_y(Alignment::Center)
                .into(),
        ),
        Loadable::Failed(error) => Some(
            widget::text::caption(fl!("props-error", error = error.as_str()))
                .class(dim_text())
                .wrapping(Wrapping::Word)
                .width(Length::Fill)
                .into(),
        ),
        Loadable::Idle | Loadable::Loaded(_) => None,
    }
}

/// The row of drawer-level actions, or `None` while there are none (the
/// drawer then renders nothing for it): 'Edit tags…' for local files.
fn actions_row<'a>(track: &'a Track) -> Option<Element<'a>> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let mut actions: Vec<Element<'a>> = Vec::new();
    if aulos_core::tagger::is_taggable(track) {
        actions.push(
            widget::button::standard(fl!("menu-edit-tags"))
                .leading_icon(widget::icon::from_name("document-edit-symbolic").size(16))
                .on_press(PropertiesMessage::EditTags(Box::new(track.clone())))
                .into(),
        );
    }
    if actions.is_empty() {
        return None;
    }
    Some(
        widget::Row::with_children(actions)
            .spacing(sp.space_xs)
            .align_y(Alignment::Center)
            .into(),
    )
}

// ---------------------------------------------------------------------------
// Track
// ---------------------------------------------------------------------------

/// The Properties drawer body for a track.
pub fn track_view<'a>(track: &'a Track, info: Loadable<&TechInfo>) -> Element<'a> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let merged = merge(
        match &info {
            Loadable::Loaded(i) => Some(*i),
            _ => None,
        },
        known_from_track(track),
    );

    let mut page = widget::Column::new()
        .spacing(sp.space_l)
        .width(Length::Fill);
    if let Some(banner) = status_banner(&info) {
        page = page.push(banner);
    }
    if let Some(actions) = actions_row(track) {
        page = page.push(actions);
    }
    page.push(overview_section(track, &merged))
        .push(technical_section(&merged))
        .push(loudness_section(&merged))
        .push(file_section(track, &merged))
        .into()
}

fn overview_section<'a>(track: &Track, info: &TechInfo) -> Element<'a> {
    section(
        fl!("props-section-overview"),
        vec![
            row(fl!("props-title"), text_or_dash(Some(&track.title))),
            row(fl!("props-artist"), text_or_dash(Some(&track.artist))),
            row(
                fl!("props-album-artist"),
                text_or_dash(Some(&track.album_artist)),
            ),
            row(fl!("props-album"), text_or_dash(Some(&track.album))),
            row(
                fl!("props-composer"),
                text_or_dash(info.composer.as_deref()),
            ),
            row(fl!("props-genre"), text_or_dash(Some(&track.genre))),
            row(fl!("props-year"), number_or_dash(track.year)),
            row(fl!("props-date"), text_or_dash(info.date.as_deref())),
            row(
                fl!("props-original-date"),
                text_or_dash(info.original_date.as_deref()),
            ),
            row(
                fl!("props-track-number"),
                number_or_dash(track.track_number),
            ),
            row(fl!("props-disc-number"), number_or_dash(track.disc_number)),
            row(fl!("props-source"), source_label(track)),
        ],
    )
}

fn technical_section<'a>(info: &TechInfo) -> Element<'a> {
    let mut rows = vec![
        row(fl!("props-format"), text_or_dash(info.container.as_deref())),
        row(fl!("props-codec"), format_codec(info)),
        row(fl!("props-bitrate"), format_bitrate_line(info)),
        row(
            fl!("props-sample-rate"),
            info.sample_rate
                .map_or_else(|| MISSING.to_string(), format_sample_rate),
        ),
        row(
            fl!("props-bit-depth"),
            format_bit_depth(info.bits_per_sample, info.lossless),
        ),
        row(
            fl!("props-channels"),
            format_channels(info.channels, info.channel_layout.as_deref()),
        ),
        row(
            fl!("props-duration"),
            info.duration
                .map_or_else(|| MISSING.to_string(), format_duration_exact),
        ),
        row(fl!("props-file-size"), format_file_size(info)),
        row(fl!("props-encoder"), text_or_dash(info.encoder.as_deref())),
        row(fl!("props-tag-formats"), list_or_dash(&info.tag_formats)),
        row(fl!("props-md5"), text_or_dash(info.md5.as_deref())),
    ];
    // Only servers report a MIME type; a dash row would be noise elsewhere.
    if let Some(mime) = info.mime_type.as_deref() {
        rows.push(row(fl!("props-mime-type"), text_or_dash(Some(mime))));
    }
    section(fl!("props-section-technical"), rows)
}

fn loudness_section<'a>(info: &TechInfo) -> Element<'a> {
    let rg = &info.replay_gain;
    section(
        fl!("props-section-loudness"),
        vec![
            row(
                fl!("props-rg-track-gain"),
                text_or_dash(rg.track_gain.as_deref()),
            ),
            row(
                fl!("props-rg-track-peak"),
                text_or_dash(rg.track_peak.as_deref()),
            ),
            row(
                fl!("props-rg-album-gain"),
                text_or_dash(rg.album_gain.as_deref()),
            ),
            row(
                fl!("props-rg-album-peak"),
                text_or_dash(rg.album_peak.as_deref()),
            ),
        ],
    )
}

fn file_section<'a>(track: &Track, info: &TechInfo) -> Element<'a> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let kind = source_kind(track);
    let mut rows = Vec::new();
    match kind {
        SourceKind::Local => {
            let path = track.path.to_string_lossy();
            rows.push(row(fl!("props-path"), text_or_dash(Some(&path))));
        }
        SourceKind::Stream => {
            rows.push(row(
                fl!("props-url"),
                text_or_dash(info.remote_path.as_deref()),
            ));
        }
        SourceKind::Subsonic | SourceKind::Mpd => {
            rows.push(row(
                fl!("props-server-path"),
                text_or_dash(info.remote_path.as_deref()),
            ));
        }
    }
    if kind == SourceKind::Local {
        let button = widget::button::standard(fl!("props-open-folder"))
            .leading_icon(widget::icon::from_name("folder-open-symbolic").size(16))
            .on_press(PropertiesMessage::OpenContainingFolder(track.path.clone()));
        rows.push(
            widget::Row::new()
                .push(widget::space::horizontal())
                .push(button)
                .padding([sp.space_xxs, 0])
                .width(Length::Fill)
                .into(),
        );
    }
    section(fl!("props-section-file"), rows)
}

// ---------------------------------------------------------------------------
// Album
// ---------------------------------------------------------------------------

/// The Properties drawer body for an album.
pub fn album_view<'a>(album: &Album, summary: Loadable<&AlbumSummary>) -> Element<'a> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let mut page = widget::Column::new()
        .spacing(sp.space_l)
        .width(Length::Fill);
    if let Some(banner) = status_banner(&summary) {
        page = page.push(banner);
    }

    let loaded = match &summary {
        Loadable::Loaded(s) => Some(*s),
        _ => None,
    };
    let year = match loaded {
        Some(s) => format_year_range(s.year_min, s.year_max),
        None => number_or_dash(album.year),
    };
    page = page.push(section(
        fl!("props-section-overview"),
        vec![
            row(fl!("props-album"), text_or_dash(Some(&album.name))),
            row(fl!("props-artist"), text_or_dash(Some(&album.artist))),
            row(fl!("props-year"), year),
            row(fl!("props-tracks"), album.tracks.len().to_string()),
            row(
                fl!("props-total-duration"),
                format_duration_exact(album.total_duration()),
            ),
        ],
    ));

    let dash = || MISSING.to_string();
    let mut rows = vec![
        row(
            fl!("props-formats"),
            loaded.map_or_else(dash, |s| list_or_dash(&s.formats)),
        ),
        row(
            fl!("props-sample-rates"),
            loaded.map_or_else(dash, |s| {
                let rates: Vec<String> = s
                    .sample_rates
                    .iter()
                    .map(|r| format_sample_rate(*r))
                    .collect();
                list_or_dash(&rates)
            }),
        ),
        row(
            fl!("props-bit-depths"),
            loaded.map_or_else(dash, |s| {
                let depths: Vec<String> = s.bit_depths.iter().map(|b| format!("{b}-bit")).collect();
                list_or_dash(&depths)
            }),
        ),
        row(
            fl!("props-bitrate-range"),
            loaded.map_or_else(dash, |s| {
                format_bitrate_range(s.bitrate_min_kbps, s.bitrate_max_kbps)
            }),
        ),
        row(
            fl!("props-bitrate-average"),
            loaded
                .and_then(|s| s.bitrate_avg_kbps)
                .map_or_else(dash, format_bitrate),
        ),
        row(
            fl!("props-total-size"),
            loaded
                .and_then(|s| s.total_size)
                .map_or_else(dash, format_size),
        ),
    ];
    if let Some(s) = loaded.filter(|s| s.unprobed > 0) {
        rows.push(
            widget::text::caption(fl!("props-unprobed", count = s.unprobed))
                .class(dim_text())
                .wrapping(Wrapping::Word)
                .width(Length::Fill)
                .into(),
        );
    }
    page.push(section(fl!("props-section-summary"), rows))
        .into()
}
