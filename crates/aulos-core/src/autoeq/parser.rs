// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Parsers for AutoEQ file formats (INDEX.md and FixedBandEQ.txt).

use super::{AutoEQError, AutoEQProfile, AutoEQProfileMetadata, Result};
use regex::Regex;

/// Parse the AutoEQ INDEX.md file to extract profile metadata.
///
/// The INDEX.md file contains markdown links in the format:
/// `- [Display Name](./path/to/profile)`
pub fn parse_index(content: &str) -> Result<Vec<AutoEQProfileMetadata>> {
    let link_regex = Regex::new(r"-\s*\[([^\]]+)\]\(\./([^)]+)\)")
        .map_err(|e| AutoEQError::InvalidFormat(format!("Regex error: {}", e)))?;

    let mut profiles = Vec::new();

    for line in content.lines() {
        let Some(captures) = link_regex.captures(line) else {
            continue;
        };
        let Some(name) = captures.get(1) else {
            continue;
        };
        let Some(path) = captures.get(2) else {
            continue;
        };
        let name = name.as_str();
        let path = path.as_str();

        // URL-decode name and path
        let name = urlencoding::decode(name)
            .unwrap_or(std::borrow::Cow::Borrowed(name))
            .to_string();
        let path_str = urlencoding::decode(path)
            .unwrap_or(std::borrow::Cow::Borrowed(path))
            .to_string();

        // Extract source and type from path (format: source/type/name)
        let parts: Vec<&str> = path_str.split('/').collect();
        let (source, type_) = if parts.len() >= 2 {
            (
                urlencoding::decode(parts[0])
                    .unwrap_or_else(|_| std::borrow::Cow::Borrowed(parts[0]))
                    .to_string(),
                urlencoding::decode(parts[1])
                    .unwrap_or_else(|_| std::borrow::Cow::Borrowed(parts[1]))
                    .to_string(),
            )
        } else {
            (String::new(), String::new())
        };

        profiles.push(AutoEQProfileMetadata {
            name,
            path: path.to_string(),
            source,
            type_,
        });
    }

    Ok(profiles)
}

/// Parse a FixedBandEQ.txt file to extract equalizer settings.
///
/// Format:
/// ```text
/// Preamp: -6.5 dB
/// Filter 1: ON PK Fc 31 Hz Gain 5.0 dB Q 0.70
/// ...
/// Filter 10: ON PK Fc 16000 Hz Gain -2.0 dB Q 0.70
/// ```
pub fn parse_fixed_band_eq(path: &str, content: &str) -> Result<AutoEQProfile> {
    // Parse preamp
    let preamp_regex = Regex::new(r"Preamp:\s*([-+]?\d+\.?\d*)\s*dB")
        .map_err(|e| AutoEQError::InvalidFormat(format!("Regex error: {}", e)))?;

    let preamp: f32 = preamp_regex
        .captures(content)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<f32>().ok())
        .ok_or_else(|| AutoEQError::InvalidFormat("Preamp not found or invalid".to_string()))?;

    if !preamp.is_finite() {
        return Err(AutoEQError::InvalidFormat(format!(
            "Preamp is not a finite value: {}",
            preamp
        )));
    }

    // Parse filters
    let filter_regex =
        Regex::new(r"Filter\s+\d+:.*?Fc\s+(\d+)\s+Hz.*?Gain\s+([-+]?\d+\.?\d*)\s*dB")
            .map_err(|e| AutoEQError::InvalidFormat(format!("Regex error: {}", e)))?;

    // Sanity bounds well outside any real AutoEQ measurement, just to reject corrupt data.
    const MIN_FREQ_HZ: u32 = 1;
    const MAX_FREQ_HZ: u32 = 200_000;
    const MAX_GAIN_DB: f32 = 100.0;

    let mut gains = Vec::new();
    for captures in filter_regex.captures_iter(content) {
        let freq_str = captures
            .get(1)
            .ok_or_else(|| AutoEQError::InvalidFormat("Filter missing frequency".to_string()))?
            .as_str();
        let gain_str = captures
            .get(2)
            .ok_or_else(|| AutoEQError::InvalidFormat("Filter missing gain".to_string()))?
            .as_str();

        let freq: u32 = freq_str.parse().map_err(|_| {
            AutoEQError::InvalidFormat(format!("Filter frequency out of range: {}", freq_str))
        })?;
        let gain: f32 = gain_str.parse().map_err(|_| {
            AutoEQError::InvalidFormat(format!("Invalid filter gain: {}", gain_str))
        })?;

        if !gain.is_finite() {
            return Err(AutoEQError::InvalidFormat(format!(
                "Filter gain is not finite: {}",
                gain
            )));
        }
        if !(MIN_FREQ_HZ..=MAX_FREQ_HZ).contains(&freq) {
            return Err(AutoEQError::InvalidFormat(format!(
                "Filter frequency is unreasonable: {} Hz",
                freq
            )));
        }
        if gain.abs() > MAX_GAIN_DB {
            return Err(AutoEQError::InvalidFormat(format!(
                "Filter gain is unreasonable: {} dB",
                gain
            )));
        }

        gains.push((freq, gain));
    }

    if gains.len() != 10 {
        return Err(AutoEQError::InvalidFormat(format!(
            "Expected 10 filters, found {}",
            gains.len()
        )));
    }

    // Reject any deviation from the standard 10-band layout: the
    // equalizer applies `bands` at a fixed, hardcoded set of frequencies
    // (see `player::equalizer::BAND_FREQUENCIES`), so a filter listed out
    // of order or at an unexpected frequency would silently have its gain
    // applied to the wrong band instead of failing loudly.
    let expected_freqs = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
    for (i, (freq, _)) in gains.iter().enumerate() {
        if *freq != expected_freqs[i] {
            return Err(AutoEQError::InvalidFormat(format!(
                "Filter {} has unexpected frequency {} Hz (expected {} Hz)",
                i + 1,
                freq,
                expected_freqs[i]
            )));
        }
    }

    // Extract bands (just gains, ordered) directly into the fixed array, no temp Vec.
    let mut bands = [0.0f32; 10];
    for (i, (_, gain)) in gains.iter().enumerate() {
        bands[i] = *gain;
    }

    // Extract name, source, type from path
    let decoded_path = urlencoding::decode(path)
        .unwrap_or(std::borrow::Cow::Borrowed(path))
        .to_string();

    let parts: Vec<&str> = decoded_path.split('/').collect();
    let (name, source, type_) = if parts.len() >= 3 {
        (
            parts[parts.len() - 1].to_string(),
            parts[0].to_string(),
            parts[1].to_string(),
        )
    } else {
        (decoded_path.clone(), String::new(), String::new())
    };

    Ok(AutoEQProfile {
        name,
        path: path.to_string(),
        source,
        type_,
        preamp,
        bands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_index() {
        let content = r#"
# Index

- [Sennheiser HD 650](./oratory1990/over-ear/Sennheiser%20HD%20650)
- [Sony WH-1000XM4](./rtings/over-ear/Sony%20WH-1000XM4)
        "#;

        let profiles = parse_index(content).unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].name, "Sennheiser HD 650");
        assert_eq!(profiles[0].source, "oratory1990");
        assert_eq!(profiles[0].type_, "over-ear");
    }

    #[test]
    fn test_parse_fixed_band_eq() {
        let content = r#"
Preamp: -6.5 dB
Filter 1: ON PK Fc 31 Hz Gain 5.0 dB Q 0.70
Filter 2: ON PK Fc 62 Hz Gain 4.0 dB Q 0.70
Filter 3: ON PK Fc 125 Hz Gain 3.0 dB Q 0.70
Filter 4: ON PK Fc 250 Hz Gain 2.0 dB Q 0.70
Filter 5: ON PK Fc 500 Hz Gain 1.0 dB Q 0.70
Filter 6: ON PK Fc 1000 Hz Gain 0.0 dB Q 0.70
Filter 7: ON PK Fc 2000 Hz Gain -1.0 dB Q 0.70
Filter 8: ON PK Fc 4000 Hz Gain -2.0 dB Q 0.70
Filter 9: ON PK Fc 8000 Hz Gain -3.0 dB Q 0.70
Filter 10: ON PK Fc 16000 Hz Gain -4.0 dB Q 0.70
        "#;

        let profile = parse_fixed_band_eq("oratory1990/over-ear/Test", content).unwrap();
        assert_eq!(profile.preamp, -6.5);
        assert_eq!(profile.bands[0], 5.0);
        assert_eq!(profile.bands[9], -4.0);
        assert_eq!(profile.name, "Test");
        assert_eq!(profile.source, "oratory1990");
    }

    #[test]
    fn test_parse_fixed_band_eq_oversized_frequency_is_rejected() {
        let content = r#"
Preamp: -6.5 dB
Filter 1: ON PK Fc 99999999999999999999 Hz Gain 5.0 dB Q 0.70
Filter 2: ON PK Fc 62 Hz Gain 4.0 dB Q 0.70
Filter 3: ON PK Fc 125 Hz Gain 3.0 dB Q 0.70
Filter 4: ON PK Fc 250 Hz Gain 2.0 dB Q 0.70
Filter 5: ON PK Fc 500 Hz Gain 1.0 dB Q 0.70
Filter 6: ON PK Fc 1000 Hz Gain 0.0 dB Q 0.70
Filter 7: ON PK Fc 2000 Hz Gain -1.0 dB Q 0.70
Filter 8: ON PK Fc 4000 Hz Gain -2.0 dB Q 0.70
Filter 9: ON PK Fc 8000 Hz Gain -3.0 dB Q 0.70
Filter 10: ON PK Fc 16000 Hz Gain -4.0 dB Q 0.70
        "#;

        let err = parse_fixed_band_eq("oratory1990/over-ear/Test", content).unwrap_err();
        assert!(matches!(err, AutoEQError::InvalidFormat(_)));
    }

    #[test]
    fn test_parse_fixed_band_eq_non_finite_gain_is_rejected() {
        let content = r#"
Preamp: -6.5 dB
Filter 1: ON PK Fc 31 Hz Gain 99999999999999999999999999999999999999999999999.0 dB Q 0.70
Filter 2: ON PK Fc 62 Hz Gain 4.0 dB Q 0.70
Filter 3: ON PK Fc 125 Hz Gain 3.0 dB Q 0.70
Filter 4: ON PK Fc 250 Hz Gain 2.0 dB Q 0.70
Filter 5: ON PK Fc 500 Hz Gain 1.0 dB Q 0.70
Filter 6: ON PK Fc 1000 Hz Gain 0.0 dB Q 0.70
Filter 7: ON PK Fc 2000 Hz Gain -1.0 dB Q 0.70
Filter 8: ON PK Fc 4000 Hz Gain -2.0 dB Q 0.70
Filter 9: ON PK Fc 8000 Hz Gain -3.0 dB Q 0.70
Filter 10: ON PK Fc 16000 Hz Gain -4.0 dB Q 0.70
        "#;

        let err = parse_fixed_band_eq("oratory1990/over-ear/Test", content).unwrap_err();
        assert!(matches!(err, AutoEQError::InvalidFormat(_)));
    }

    #[test]
    fn test_parse_fixed_band_eq_frequency_order_mismatch_is_rejected() {
        // Each in-range frequency, but band 1 and band 2 are swapped: if
        // this weren't rejected, the parsed gains would silently be
        // applied to the wrong band by the fixed-frequency equalizer.
        let content = r#"
Preamp: -6.5 dB
Filter 1: ON PK Fc 62 Hz Gain 5.0 dB Q 0.70
Filter 2: ON PK Fc 31 Hz Gain 4.0 dB Q 0.70
Filter 3: ON PK Fc 125 Hz Gain 3.0 dB Q 0.70
Filter 4: ON PK Fc 250 Hz Gain 2.0 dB Q 0.70
Filter 5: ON PK Fc 500 Hz Gain 1.0 dB Q 0.70
Filter 6: ON PK Fc 1000 Hz Gain 0.0 dB Q 0.70
Filter 7: ON PK Fc 2000 Hz Gain -1.0 dB Q 0.70
Filter 8: ON PK Fc 4000 Hz Gain -2.0 dB Q 0.70
Filter 9: ON PK Fc 8000 Hz Gain -3.0 dB Q 0.70
Filter 10: ON PK Fc 16000 Hz Gain -4.0 dB Q 0.70
        "#;

        let err = parse_fixed_band_eq("oratory1990/over-ear/Test", content).unwrap_err();
        assert!(matches!(err, AutoEQError::InvalidFormat(_)));
    }

    #[test]
    fn test_parse_fixed_band_eq_unreasonable_gain_is_rejected() {
        let content = r#"
Preamp: -6.5 dB
Filter 1: ON PK Fc 31 Hz Gain 500.0 dB Q 0.70
Filter 2: ON PK Fc 62 Hz Gain 4.0 dB Q 0.70
Filter 3: ON PK Fc 125 Hz Gain 3.0 dB Q 0.70
Filter 4: ON PK Fc 250 Hz Gain 2.0 dB Q 0.70
Filter 5: ON PK Fc 500 Hz Gain 1.0 dB Q 0.70
Filter 6: ON PK Fc 1000 Hz Gain 0.0 dB Q 0.70
Filter 7: ON PK Fc 2000 Hz Gain -1.0 dB Q 0.70
Filter 8: ON PK Fc 4000 Hz Gain -2.0 dB Q 0.70
Filter 9: ON PK Fc 8000 Hz Gain -3.0 dB Q 0.70
Filter 10: ON PK Fc 16000 Hz Gain -4.0 dB Q 0.70
        "#;

        let err = parse_fixed_band_eq("oratory1990/over-ear/Test", content).unwrap_err();
        assert!(matches!(err, AutoEQError::InvalidFormat(_)));
    }

    #[test]
    fn test_parse_fixed_band_eq_malformed_content_returns_error_not_panic() {
        let content = "this is not an AutoEQ file at all, just 💥 random junk 12345";

        let err = parse_fixed_band_eq("bogus/path", content).unwrap_err();
        assert!(matches!(err, AutoEQError::InvalidFormat(_)));
    }

    #[test]
    fn test_parse_fixed_band_eq_missing_preamp_returns_error_not_panic() {
        let content = r#"
Filter 1: ON PK Fc 31 Hz Gain 5.0 dB Q 0.70
Filter 2: ON PK Fc 62 Hz Gain 4.0 dB Q 0.70
Filter 3: ON PK Fc 125 Hz Gain 3.0 dB Q 0.70
Filter 4: ON PK Fc 250 Hz Gain 2.0 dB Q 0.70
Filter 5: ON PK Fc 500 Hz Gain 1.0 dB Q 0.70
Filter 6: ON PK Fc 1000 Hz Gain 0.0 dB Q 0.70
Filter 7: ON PK Fc 2000 Hz Gain -1.0 dB Q 0.70
Filter 8: ON PK Fc 4000 Hz Gain -2.0 dB Q 0.70
Filter 9: ON PK Fc 8000 Hz Gain -3.0 dB Q 0.70
Filter 10: ON PK Fc 16000 Hz Gain -4.0 dB Q 0.70
        "#;

        let err = parse_fixed_band_eq("oratory1990/over-ear/Test", content).unwrap_err();
        assert!(matches!(err, AutoEQError::InvalidFormat(_)));
    }

    #[test]
    fn test_parse_index_malformed_link_does_not_panic() {
        let content = "- [Broken link without closing paren(./oratory1990/over-ear/Foo";

        let profiles = parse_index(content).unwrap();
        assert_eq!(profiles.len(), 0);
    }
}
