// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! What the Properties drawer shows about a track or album, as text.
//!
//! A value nobody knows is always rendered as an em dash ([`MISSING`]),
//! never as `0`, because a remote server or a stream legitimately reports
//! only a subset of what a local file can tell. This module holds the
//! UI-independent half: where a track's bytes live ([`SourceKind`]), what
//! the [`Track`] itself already knows ([`known_from_track`], [`merge`]) and
//! the display strings for every row. [`PropertiesState`] tracks what the
//! drawer currently describes and which asynchronous request may still fill
//! it in: every opening takes a fresh, monotonically increasing request id
//! and the result carries it back, so a slow result from a drawer the user
//! has since closed or re-opened on another subject is dropped instead of
//! overwriting the current one.

use super::tech_info::{self, AlbumSummary, BitrateMode, ReplayGain, TechInfo};
use super::{Album, Track};
use crate::fl;
use crate::format::{format_bitrate, format_size};
use crate::provider::MusicProvider;

/// Shown in place of a value that is not known.
pub const MISSING: &str = "\u{2014}";

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

/// `Some(non-blank)` as-is, anything else as [`MISSING`].
pub fn text_or_dash<S: AsRef<str>>(value: Option<S>) -> String {
    match value.as_ref().map(|v| v.as_ref().trim()) {
        Some(v) if !v.is_empty() => v.to_string(),
        _ => MISSING.to_string(),
    }
}

/// A count that is meaningless at zero (track number, year…).
pub fn number_or_dash(n: u32) -> String {
    if n == 0 {
        MISSING.to_string()
    } else {
        n.to_string()
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
pub fn list_or_dash<T: ToString>(items: &[T]) -> String {
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
pub fn format_bitrate_line(info: &TechInfo) -> String {
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

pub fn format_codec(info: &TechInfo) -> String {
    let name = info.codec.as_deref().or(info.codec_short.as_deref());
    match (name, info.profile.as_deref()) {
        (Some(n), Some(p)) => format!("{n} ({p})"),
        (Some(n), None) => n.to_string(),
        (None, Some(p)) => p.to_string(),
        (None, None) => MISSING.to_string(),
    }
}

pub fn format_file_size(info: &TechInfo) -> String {
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

pub fn source_label(track: &Track) -> String {
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
        bits_per_sample: nonzero(track.bit_depth),
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
pub fn merge(loaded: Option<&TechInfo>, known: TechInfo) -> TechInfo {
    let Some(loaded) = loaded else {
        return known;
    };
    let mut out = loaded.clone();
    out.sample_rate = out.sample_rate.or(known.sample_rate);
    out.bits_per_sample = out.bits_per_sample.or(known.bits_per_sample);
    out.bitrate_kbps = out.bitrate_kbps.or(known.bitrate_kbps);
    out.duration = out.duration.or(known.duration);
    out.remote_path = out.remote_path.or(known.remote_path);
    let rg = &mut out.replay_gain;
    rg.track_gain = rg.track_gain.take().or(known.replay_gain.track_gain);
    rg.album_gain = rg.album_gain.take().or(known.replay_gain.album_gain);
    out
}

/// What the drawer currently describes.
#[derive(Debug, Clone)]
pub enum Subject {
    Track(Track),
    Album(Album),
}

/// Drawer state: the subject, the id of the request that may still fill it
/// in, and the loading state of each kind of data.
#[derive(Debug, Default)]
pub struct PropertiesState {
    /// Id of the latest opening; results carrying an older id are stale.
    request_id: u64,
    subject: Option<Subject>,
    track_info: Loadable<TechInfo>,
    album_summary: Loadable<AlbumSummary>,
}

impl PropertiesState {
    /// Start describing `track`; returns the id its result must carry.
    pub fn begin_track(&mut self, track: Track, initial: Loadable<TechInfo>) -> u64 {
        self.request_id += 1;
        self.subject = Some(Subject::Track(track));
        self.track_info = initial;
        self.album_summary = Loadable::Idle;
        self.request_id
    }

    /// Start describing `album`; returns the id its summary must carry.
    pub fn begin_album(&mut self, album: Album) -> u64 {
        self.request_id += 1;
        self.subject = Some(Subject::Album(album));
        self.track_info = Loadable::Idle;
        self.album_summary = Loadable::Loading;
        self.request_id
    }

    /// Whether a result tagged `request_id` still belongs to the drawer.
    #[must_use]
    pub fn accepts(&self, request_id: u64) -> bool {
        request_id == self.request_id
    }

    /// Record a finished track probe; a stale one is dropped.
    pub fn finish_track(&mut self, request_id: u64, result: Result<TechInfo, String>) {
        if !self.accepts(request_id) || !matches!(self.subject, Some(Subject::Track(_))) {
            tracing::debug!(request_id, "dropping stale track properties");
            return;
        }
        self.track_info = match result {
            Ok(info) => Loadable::Loaded(info),
            Err(error) => Loadable::Failed(error),
        };
    }

    /// Record a finished album summary; a stale one is dropped.
    pub fn finish_album(&mut self, request_id: u64, summary: AlbumSummary) {
        if !self.accepts(request_id) || !matches!(self.subject, Some(Subject::Album(_))) {
            tracing::debug!(request_id, "dropping stale album properties");
            return;
        }
        self.album_summary = Loadable::Loaded(summary);
    }

    /// What the drawer currently describes, if anything.
    #[must_use]
    pub fn subject(&self) -> Option<&Subject> {
        self.subject.as_ref()
    }

    /// Loading state of the current track's technical details.
    #[must_use]
    pub fn track_info(&self) -> &Loadable<TechInfo> {
        &self.track_info
    }

    /// Loading state of the current album's summary.
    #[must_use]
    pub fn album_summary(&self) -> &Loadable<AlbumSummary> {
        &self.album_summary
    }
}

/// Whether reading a track's properties needs work off the UI thread.
/// Streams (radio, streamed podcasts) have nothing to read beyond what the
/// [`Track`] carries.
#[must_use]
pub fn needs_probe(track: &Track) -> bool {
    source_kind(track) != SourceKind::Stream
}

/// Reads one track's technical details (blocking). The single place that
/// dispatches on where the track lives: local files are probed directly,
/// servers are asked through their provider, streams only report what the
/// track already knows.
pub fn probe_track(
    track: &Track,
    provider: Option<&dyn MusicProvider>,
) -> Result<TechInfo, String> {
    match source_kind(track) {
        SourceKind::Local => tech_info::probe(&track.path),
        SourceKind::Subsonic | SourceKind::Mpd => {
            let provider = provider.ok_or_else(|| fl!("props-provider-unavailable"))?;
            provider
                .track_tech_info(track)
                .map_err(|e| e.to_string())
                .map(|info| info.unwrap_or_else(|| known_from_track(track)))
        }
        SourceKind::Stream => Ok(known_from_track(track)),
    }
}

#[cfg(test)]
mod state_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    fn track(provider: &str, path: &str) -> Track {
        Track {
            id: 0,
            path: PathBuf::from(path),
            title: String::new(),
            artist: String::new(),
            album_artist: String::new(),
            album: String::new(),
            genre: String::new(),
            track_number: 0,
            disc_number: 0,
            year: 0,
            duration: Duration::ZERO,
            bitrate: 0,
            sample_rate: 0,
            provider_id: Arc::from(provider),
            source_uri: String::new(),
            is_favorite: false,
            rating: None,
            rg_track_gain: None,
            rg_album_gain: None,
            bit_depth: 0,
            release_type: String::new(),
            added_at: 0,
            favorited_at: 0,
        }
    }

    fn info(rate: u32) -> TechInfo {
        TechInfo {
            sample_rate: Some(rate),
            ..TechInfo::default()
        }
    }

    #[test]
    fn request_ids_increase_with_every_opening() {
        let mut state = PropertiesState::default();
        let a = state.begin_track(track("local", "/a.flac"), Loadable::Loading);
        let b = state.begin_track(track("local", "/b.flac"), Loadable::Loading);
        assert!(b > a);
    }

    #[test]
    fn a_result_of_an_older_opening_is_dropped() {
        let mut state = PropertiesState::default();
        let old = state.begin_track(track("local", "/a.flac"), Loadable::Loading);
        let current = state.begin_track(track("local", "/b.flac"), Loadable::Loading);

        state.finish_track(old, Ok(info(44_100)));
        assert!(matches!(state.track_info, Loadable::Loading));

        state.finish_track(current, Ok(info(96_000)));
        match &state.track_info {
            Loadable::Loaded(i) => assert_eq!(i.sample_rate, Some(96_000)),
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    #[test]
    fn a_track_result_never_lands_on_an_album_subject() {
        let mut state = PropertiesState::default();
        let track_id = state.begin_track(track("local", "/a.flac"), Loadable::Loading);
        let album_id =
            state.begin_album(Album::new("A".into(), "B".into(), 2000, Vec::new(), None));
        state.finish_track(track_id, Ok(info(44_100)));
        assert!(matches!(state.track_info, Loadable::Idle));

        state.finish_album(track_id, AlbumSummary::default());
        assert!(matches!(state.album_summary, Loadable::Loading));
        state.finish_album(album_id, AlbumSummary::default());
        assert!(matches!(state.album_summary, Loadable::Loaded(_)));
    }

    #[test]
    fn a_failed_probe_is_kept_for_display() {
        let mut state = PropertiesState::default();
        let id = state.begin_track(track("local", "/a.flac"), Loadable::Loading);
        state.finish_track(id, Err("unreadable".into()));
        assert!(matches!(&state.track_info, Loadable::Failed(e) if e == "unreadable"));
    }

    #[test]
    fn only_streams_skip_the_probe() {
        assert!(needs_probe(&track("local", "/a.flac")));
        assert!(needs_probe(&track("subsonic-home", "42")));
        assert!(needs_probe(&track("mpd-home", "")));
        assert!(!needs_probe(&track("radio", "")));
        // A downloaded podcast episode is a file; a streamed one is not.
        assert!(needs_probe(&track("podcast", "/ep.mp3")));
        assert!(!needs_probe(&track("podcast", "")));
    }

    #[test]
    fn a_remote_track_without_its_provider_reports_an_error() {
        assert!(probe_track(&track("subsonic-home", "42"), None).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
