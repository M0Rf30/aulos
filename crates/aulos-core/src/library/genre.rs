// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Genre presentation data that does not depend on a UI toolkit: the
//! deterministic colour a genre is shown in, and its summary text.

use super::Track;
use super::palette::Accent;
use crate::fl;
use crate::format::format_duration_coarse;

/// Curated gradient pairs (start, end) as sRGB bytes. All are dark enough
/// for white text to stay legible on top.
const GRADIENTS: [([u8; 3], [u8; 3]); 12] = [
    ([214, 40, 70], [130, 20, 70]),
    ([30, 140, 110], [16, 80, 85]),
    ([50, 80, 170], [85, 45, 150]),
    ([200, 100, 20], [140, 50, 25]),
    ([140, 100, 180], [80, 55, 135]),
    ([20, 120, 225], [20, 60, 160]),
    ([180, 45, 145], [110, 30, 125]),
    ([10, 140, 165], [20, 80, 120]),
    ([100, 110, 125], [55, 60, 75]),
    ([205, 65, 35], [130, 30, 45]),
    ([100, 140, 30], [45, 95, 45]),
    ([205, 85, 120], [150, 45, 95]),
];

/// Deterministic `(start, end)` gradient of a genre as sRGB bytes
/// (case/whitespace-insensitive), picked from a hash of its name so the same
/// genre always gets the same colours.
#[must_use]
pub fn gradient_rgb8(genre: &str) -> ([u8; 3], [u8; 3]) {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in genre.trim().to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    GRADIENTS[(hash % GRADIENTS.len() as u64) as usize]
}

/// Accent of a genre's detail page: its gradient's start colour with white
/// foreground, as `0.0..=1.0` channels.
#[must_use]
pub fn accent(genre: &str) -> Accent {
    let (start, _) = gradient_rgb8(genre);
    Accent {
        color: start.map(|c| f32::from(c) / 255.0),
        on_color: [1.0, 1.0, 1.0],
    }
}

/// Localized "N tracks" label used in the genre detail header.
#[must_use]
pub fn track_count_label(count: usize) -> String {
    if count == 1 {
        fl!("genre-track-count-one", count = count.to_string())
    } else {
        fl!("genre-track-count-other", count = count.to_string())
    }
}

/// Header summary of a genre's `tracks`: "N tracks · 1h 20m", or just
/// "0 tracks" when there are none.
#[must_use]
pub fn summary(tracks: &[Track]) -> String {
    if tracks.is_empty() {
        return track_count_label(0);
    }
    let total_secs: u64 = tracks.iter().map(|t| t.duration.as_secs()).sum();
    format!(
        "{} \u{b7} {}",
        track_count_label(tracks.len()),
        format_duration_coarse(total_secs)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_is_stable_and_case_insensitive() {
        assert_eq!(gradient_rgb8("Rock"), gradient_rgb8("  rock "));
        assert!(GRADIENTS.contains(&gradient_rgb8("Jazz")));
    }

    #[test]
    fn accent_is_the_gradient_start_with_white_text() {
        let (start, _) = gradient_rgb8("Jazz");
        let accent = accent("Jazz");
        assert_eq!(accent.color[0], f32::from(start[0]) / 255.0);
        assert_eq!(accent.on_color, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn empty_summary_is_just_the_count() {
        assert_eq!(summary(&[]), track_count_label(0));
    }
}
