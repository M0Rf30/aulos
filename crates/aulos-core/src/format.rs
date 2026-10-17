// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Human-readable text for durations, sizes, rates and strings, shared by
//! every UI so all views render the same value the same way.

use crate::fl;
use std::time::Duration;

/// Format a duration in whole seconds as `H:MM:SS` when at least an hour,
/// otherwise `M:SS`.
#[must_use]
pub fn format_duration(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Coarse human-readable duration for headers: `2h 44m`, `44m`, `12s`.
#[must_use]
pub fn format_duration_coarse(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{total_secs}s")
    }
}

/// Truncate on a `char` boundary, appending `…` when the input exceeds
/// `max_chars`.
#[must_use]
pub fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", truncated.trim_end())
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

/// Integer with `,` thousands separators: `2,304`. Takes any unsigned integer
/// up to 64 bits (`usize` counts go through `as u64`).
pub fn format_thousands(n: impl Into<u64>) -> String {
    let digits = n.into().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Sample format as audiophile shorthand: `24/96`, `16/44.1`. Without a bit
/// depth it falls back to the plain rate (`44.1 kHz`); `None` when the rate
/// is unknown.
pub fn format_depth_rate(bit_depth: u32, hz: u32) -> Option<String> {
    if hz == 0 {
        return None;
    }
    if bit_depth == 0 {
        return Some(format_sample_rate(hz));
    }
    Some(format!(
        "{bit_depth}/{}",
        trim_decimal(f64::from(hz) / 1_000.0, 3)
    ))
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

/// Localized "N tracks" label (singular/plural forms are separate strings).
#[must_use]
pub fn track_count_label(count: usize) -> String {
    if count == 1 {
        fl!("songs-track-count-one", count = count.to_string())
    } else {
        fl!("songs-track-count-other", count = count.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_sub_minute() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(59), "0:59");
    }

    #[test]
    fn format_duration_minutes_only() {
        assert_eq!(format_duration(60), "1:00");
        assert_eq!(format_duration(3599), "59:59");
    }

    #[test]
    fn format_duration_hour_boundary() {
        assert_eq!(format_duration(3600), "1:00:00");
    }

    #[test]
    fn format_duration_regression_164_minutes() {
        // 9860s = 2h 44m 20s. Previously rendered as the bogus "164:20"
        // because minutes were never rolled over into hours.
        assert_eq!(format_duration(9860), "2:44:20");
    }

    #[test]
    fn format_duration_coarse_seconds_only() {
        assert_eq!(format_duration_coarse(12), "12s");
    }

    #[test]
    fn format_duration_coarse_minutes_only() {
        assert_eq!(format_duration_coarse(300), "5m");
    }

    #[test]
    fn format_duration_coarse_hours_and_minutes() {
        assert_eq!(format_duration_coarse(9860), "2h 44m");
    }

    #[test]
    fn format_duration_coarse_hour_boundary_zero_minutes() {
        assert_eq!(format_duration_coarse(3600), "1h 00m");
    }

    #[test]
    fn truncate_str_shorter_than_max_is_unchanged() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact_max_is_unchanged() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_over_max_ends_with_ellipsis_and_respects_length() {
        let result = truncate_str("hello world", 8);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 8);
    }

    #[test]
    fn truncate_str_trims_trailing_whitespace_before_ellipsis() {
        // Cutting right after "hello" lands on trailing spaces; they must be
        // trimmed so the ellipsis doesn't float after visible whitespace.
        let result = truncate_str("hello   world", 8);
        assert_eq!(result, "hello…");
    }

    #[test]
    fn truncate_str_multibyte_safe_no_panic() {
        // Multibyte (non-ASCII) chars must be counted and sliced by `char`,
        // never by byte, or this would panic on a non-boundary split.
        let cjk = "日本語のテスト";
        let result = truncate_str(cjk, 4);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 4);

        let accented = "ééééééééé";
        let result = truncate_str(accented, 5);
        assert!(result.ends_with('…'));
        assert!(result.chars().count() <= 5);
    }

    #[test]
    fn sample_rates_trim_trailing_zeros() {
        assert_eq!(format_sample_rate(44_100), "44.1 kHz");
        assert_eq!(format_sample_rate(48_000), "48 kHz");
        assert_eq!(format_sample_rate(96_000), "96 kHz");
        assert_eq!(format_sample_rate(22_050), "22.05 kHz");
        assert_eq!(format_sample_rate(2_822_400), "2.8224 MHz");
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(format_thousands(0_u32), "0");
        assert_eq!(format_thousands(320_u32), "320");
        assert_eq!(format_thousands(2_304_u32), "2,304");
        assert_eq!(format_thousands(1_234_567_u32), "1,234,567");
        // Counts of any width: the library views pass `len() as u64`.
        assert_eq!(format_thousands(999_u16), "999");
        assert_eq!(format_thousands(1_000_u64), "1,000");
        assert_eq!(format_thousands(4_812_usize as u64), "4,812");
        assert_eq!(format_thousands(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn depth_rate_shorthand() {
        assert_eq!(format_depth_rate(24, 96_000).as_deref(), Some("24/96"));
        assert_eq!(format_depth_rate(16, 44_100).as_deref(), Some("16/44.1"));
        assert_eq!(format_depth_rate(0, 44_100).as_deref(), Some("44.1 kHz"));
        assert_eq!(format_depth_rate(24, 0), None);
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
}
