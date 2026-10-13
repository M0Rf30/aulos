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

use crate::fl;
use crate::library::tech_info::{AlbumSummary, BitrateMode, ReplayGain, TechInfo};
use crate::library::{Album, Track};
use cosmic::iced::core::text::Wrapping;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;
use std::path::PathBuf;
use std::time::Duration;

/// Shown in place of a value that is not known.
pub const MISSING: &str = "\u{2014}";

/// Messages emitted by the Properties drawer.
#[derive(Debug, Clone)]
pub enum PropertiesMessage {
    /// Open the folder containing this file in the file manager.
    OpenContainingFolder(PathBuf),
    /// Open this (local) track in the tag editor.
    EditTags(Box<Track>),
}

/// State of an asynchronously loaded value.
#[derive(Debug, Clone, Default)]
pub enum Loadable<T> {
    /// Nothing requested yet.
    #[default]
    Idle,
    Loading,
    Loaded(T),
    /// Loading failed; the message is shown to the user.
    Failed(String),
}

impl<T> Loadable<T> {
    /// Borrowing view, so the drawer can be built from `&self` state.
    pub fn as_ref(&self) -> Loadable<&T> {
        match self {
            Self::Idle => Loadable::Idle,
            Self::Loading => Loadable::Loading,
            Self::Loaded(v) => Loadable::Loaded(v),
            Self::Failed(e) => Loadable::Failed(e.clone()),
        }
    }
}

/// Where a track's bytes live, which decides how its properties are read
/// (see `app::properties`) and which file actions make sense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A readable local file (library track, downloaded podcast episode).
    Local,
    /// A Subsonic/OpenSubsonic server song.
    Subsonic,
    /// An MPD server file.
    Mpd,
    /// A live or streamed source with no file to read (radio, podcast
    /// episode that is not downloaded).
    Stream,
}

/// Classify a track by its provider; mirrors `player::resolve_track_source`.
pub fn source_kind(track: &Track) -> SourceKind {
    let id: &str = &track.provider_id;
    if id.starts_with("mpd") {
        SourceKind::Mpd
    } else if id.starts_with("subsonic") {
        SourceKind::Subsonic
    } else if id.starts_with("radio") {
        SourceKind::Stream
    } else if track.path.as_os_str().is_empty() {
        // Streamed podcast episodes and anything else without a file.
        SourceKind::Stream
    } else {
        SourceKind::Local
    }
}

// ---------------------------------------------------------------------------
// Formatting (pure)
// ---------------------------------------------------------------------------

/// `Some(non-blank)` as-is, anything else as [`MISSING`].
fn text_or_dash<S: AsRef<str>>(value: Option<S>) -> String {
    match value.as_ref().map(|v| v.as_ref().trim()) {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => MISSING.to_string(),
    }
}

/// A count that is meaningless at zero (track number, year…).
fn number_or_dash(n: u32) -> String {
    if n == 0 {
        MISSING.to_string()
    } else {
        n.to_string()
    }
}

/// Fixed-point with at most `max_decimals` places, trailing zeros trimmed.
fn trim_decimal(value: f64, max_decimals: usize) -> String {
    let s = format!("{value:.max_decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// `44.1 kHz`, `48 kHz`, `2.8224 MHz`.
pub fn format_sample_rate(hz: u32) -> String {
    if hz >= 1_000_000 {
        format!("{} MHz", trim_decimal(f64::from(hz) / 1_000_000.0, 4))
    } else {
        format!("{} kHz", trim_decimal(f64::from(hz) / 1_000.0, 3))
    }
}

/// `320 kbps`.
pub fn format_bitrate(kbps: u32) -> String {
    format!("{kbps} kbps")
}

/// Binary-prefixed size with one decimal: `812 B`, `12.3 MiB`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Exact duration, `m:ss.mmm` (`h:mm:ss.mmm` from an hour up).
pub fn format_duration_exact(d: Duration) -> String {
    let total_ms = d.as_millis();
    let ms = total_ms % 1000;
    let secs = (total_ms / 1000) as u64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{m}:{s:02}.{ms:03}")
    }
}

/// Bit depth; lossy formats have none (`n/a`), unknown is a dash.
pub fn format_bit_depth(bits: Option<u32>, lossless: Option<bool>) -> String {
    match (bits, lossless) {
        (Some(b), _) => format!("{b}-bit"),
        (None, Some(false)) => fl!("props-not-applicable"),
        (None, _) => MISSING.to_string(),
    }
}

/// `2 (Stereo)`, `2`, `Stereo` or a dash.
pub fn format_channels(channels: Option<u32>, layout: Option<&str>) -> String {
    match (channels, layout.filter(|l| !l.is_empty())) {
        (Some(n), Some(l)) => format!("{n} ({l})"),
        (Some(n), None) => n.to_string(),
        (None, Some(l)) => l.to_string(),
        (None, None) => MISSING.to_string(),
    }
}

/// A replay-gain value in dB, signed (`-6.50 dB`).
fn format_gain(db: f32) -> String {
    format!("{db:+.2} dB")
}

/// `1999` or `1999–2003`; a dash when neither end is known.
pub fn format_year_range(min: Option<u32>, max: Option<u32>) -> String {
    match (min.filter(|y| *y > 0), max.filter(|y| *y > 0)) {
        (Some(a), Some(b)) if a != b => format!("{a}\u{2013}{b}"),
        (Some(y), _) | (None, Some(y)) => y.to_string(),
        (None, None) => MISSING.to_string(),
    }
}

/// `245\u{2013}320 kbps` (or a single value when both ends agree).
pub fn format_bitrate_range(min: Option<u32>, max: Option<u32>) -> String {
    match (min, max) {
        (Some(a), Some(b)) if a != b => format!("{a}\u{2013}{b} kbps"),
        (Some(v), _) | (None, Some(v)) => format_bitrate(v),
        (None, None) => MISSING.to_string(),
    }
}

/// Comma-joined list, a dash when empty.
fn list_or_dash<T: ToString>(items: &[T]) -> String {
    if items.is_empty() {
        MISSING.to_string()
    } else {
        items
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn bitrate_mode_label(mode: BitrateMode) -> String {
    match mode {
        BitrateMode::Cbr => fl!("props-mode-cbr"),
        BitrateMode::Vbr => fl!("props-mode-vbr"),
        BitrateMode::Abr => fl!("props-mode-abr"),
        BitrateMode::Lossless => fl!("props-mode-lossless"),
    }
}

/// Bitrate with its mode and, when it differs, the nominal bitrate.
fn format_bitrate_line(info: &TechInfo) -> String {
    let Some(kbps) = info.bitrate_kbps.or(info.nominal_bitrate_kbps) else {
        return MISSING.to_string();
    };
    let mut line = format_bitrate(kbps);
    if let Some(mode) = info.bitrate_mode {
        line.push_str(" \u{00b7} ");
        line.push_str(&bitrate_mode_label(mode));
    }
    if let Some(nominal) = info.nominal_bitrate_kbps.filter(|n| *n != kbps) {
        line.push_str(" \u{00b7} ");
        line.push_str(&fl!("props-nominal", value = format_bitrate(nominal)));
    }
    line
}

fn format_codec(info: &TechInfo) -> String {
    let name = info.codec.as_deref().or(info.codec_short.as_deref());
    match (name, info.profile.as_deref()) {
        (Some(n), Some(p)) => format!("{n} ({p})"),
        (Some(n), None) => n.to_string(),
        (None, Some(p)) => p.to_string(),
        (None, None) => MISSING.to_string(),
    }
}

fn format_file_size(info: &TechInfo) -> String {
    match (info.file_size, info.payload_bytes) {
        (Some(total), Some(payload)) if payload != total => format!(
            "{} ({})",
            format_size(total),
            fl!("props-audio-data", size = format_size(payload))
        ),
        (Some(total), _) => format_size(total),
        (None, _) => MISSING.to_string(),
    }
}

fn source_label(track: &Track) -> String {
    match &*track.provider_id {
        "local" => fl!("props-source-local"),
        "radio" => fl!("props-source-radio"),
        "podcast" => fl!("props-source-podcast"),
        other => text_or_dash(Some(other)),
    }
}

// ---------------------------------------------------------------------------
// What the Track itself knows
// ---------------------------------------------------------------------------

/// The technical values a [`Track`] already carries. Used for sources that
/// cannot be probed (radio, streamed podcasts) and as the baseline that
/// loaded data is merged over, so a failed or still running probe never
/// blanks what the track knew all along.
pub fn known_from_track(track: &Track) -> TechInfo {
    let nonzero = |v: u32| (v > 0).then_some(v);
    // Only streams expose their URL: a Subsonic `source_uri` carries the
    // account's signed credentials, and a local file's is not a location.
    let url = (source_kind(track) == SourceKind::Stream && !track.source_uri.is_empty())
        .then(|| track.source_uri.clone());
    TechInfo {
        sample_rate: nonzero(track.sample_rate),
        bitrate_kbps: nonzero(track.bitrate),
        duration: (!track.duration.is_zero()).then_some(track.duration),
        replay_gain: ReplayGain {
            track_gain: track.rg_track_gain.map(format_gain),
            album_gain: track.rg_album_gain.map(format_gain),
            ..ReplayGain::default()
        },
        remote_path: url,
        ..TechInfo::default()
    }
}

/// `loaded` with every gap filled from `known`.
fn merge(loaded: Option<&TechInfo>, known: TechInfo) -> TechInfo {
    let Some(loaded) = loaded else {
        return known;
    };
    let mut out = loaded.clone();
    out.sample_rate = out.sample_rate.or(known.sample_rate);
    out.bitrate_kbps = out.bitrate_kbps.or(known.bitrate_kbps);
    out.duration = out.duration.or(known.duration);
    out.remote_path = out.remote_path.or(known.remote_path);
    let rg = &mut out.replay_gain;
    rg.track_gain = rg.track_gain.take().or(known.replay_gain.track_gain);
    rg.album_gain = rg.album_gain.take().or(known.replay_gain.album_gain);
    out
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
    if crate::tagger::is_taggable(track) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_rates_trim_trailing_zeros() {
        assert_eq!(format_sample_rate(44_100), "44.1 kHz");
        assert_eq!(format_sample_rate(48_000), "48 kHz");
        assert_eq!(format_sample_rate(96_000), "96 kHz");
        assert_eq!(format_sample_rate(22_050), "22.05 kHz");
        assert_eq!(format_sample_rate(2_822_400), "2.8224 MHz");
    }

    #[test]
    fn bitrate_has_unit() {
        assert_eq!(format_bitrate(320), "320 kbps");
    }

    #[test]
    fn sizes_use_binary_units_with_one_decimal() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(812), "812 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(12_897_484), "12.3 MiB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn durations_are_exact_to_the_millisecond() {
        assert_eq!(format_duration_exact(Duration::from_millis(0)), "0:00.000");
        assert_eq!(
            format_duration_exact(Duration::from_millis(215_042)),
            "3:35.042"
        );
        assert_eq!(
            format_duration_exact(Duration::from_millis(3_723_007)),
            "1:02:03.007"
        );
    }

    #[test]
    fn missing_values_render_as_an_em_dash_never_zero() {
        assert_eq!(text_or_dash::<&str>(None), MISSING);
        assert_eq!(text_or_dash(Some("  ")), MISSING);
        assert_eq!(text_or_dash(Some(" x ")), "x");
        assert_eq!(number_or_dash(0), MISSING);
        assert_eq!(number_or_dash(7), "7");
        assert_eq!(list_or_dash::<String>(&[]), MISSING);
        assert_eq!(format_channels(None, None), MISSING);
        assert_eq!(format_bitrate_range(None, None), MISSING);
        assert_eq!(format_year_range(None, Some(0)), MISSING);
    }

    #[test]
    fn bit_depth_distinguishes_unknown_from_not_applicable() {
        assert_eq!(format_bit_depth(Some(24), Some(true)), "24-bit");
        assert_eq!(format_bit_depth(None, None), MISSING);
        assert_eq!(format_bit_depth(None, Some(true)), MISSING);
        assert_ne!(format_bit_depth(None, Some(false)), MISSING);
    }

    #[test]
    fn channels_combine_count_and_layout() {
        assert_eq!(format_channels(Some(2), Some("Stereo")), "2 (Stereo)");
        assert_eq!(format_channels(Some(1), None), "1");
        assert_eq!(format_channels(None, Some("5.1")), "5.1");
    }

    #[test]
    fn ranges_collapse_when_both_ends_agree() {
        assert_eq!(format_bitrate_range(Some(320), Some(320)), "320 kbps");
        assert_eq!(
            format_bitrate_range(Some(245), Some(320)),
            "245\u{2013}320 kbps"
        );
        assert_eq!(format_year_range(Some(1999), Some(1999)), "1999");
        assert_eq!(
            format_year_range(Some(1999), Some(2003)),
            "1999\u{2013}2003"
        );
    }

    #[test]
    fn merge_fills_gaps_but_never_overrides_loaded_values() {
        let known = TechInfo {
            sample_rate: Some(44_100),
            bitrate_kbps: Some(128),
            ..TechInfo::default()
        };
        let loaded = TechInfo {
            bitrate_kbps: Some(320),
            ..TechInfo::default()
        };
        let merged = merge(Some(&loaded), known);
        assert_eq!(merged.bitrate_kbps, Some(320));
        assert_eq!(merged.sample_rate, Some(44_100));
    }
}
