// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Single source of truth for "which formats can aulos scan/play", derived at
//! runtime from Symphonia's own probe registry instead of a hand-maintained
//! extension list, plus the one shared [`probe`] every open path uses.
//!
//! Ported from rmpd's `format_registry`.

use std::sync::LazyLock;

use symphonia::core::formats::probe::{Probe, ProbeOptions, RegisteredFormatInfo};

/// Extensions aulos treats as scannable that no registered reader declares,
/// next to the reader that demuxes them in practice.
///
/// - `alac`: bare ALAC files are ISO-BMFF; isomp4 detects them by marker.
/// - `mka`: Matroska audio; upstream only declares `webm`/`mkv`.
const EXTRA_EXTENSION_ALIASES: &[(&str, &str)] = &[("alac", "isomp4"), ("mka", "matroska")];

/// Extensions Symphonia can demux but the library scanner and tag/audio file
/// pickers must not offer: video (or unrelated, e.g. `.ts`) containers whose
/// audio isn't "music". They stay in [`SUPPORTED_EXTENSIONS`] (playback by
/// path/URL, the converter).
pub const SCAN_EXCLUDED_EXTENSIONS: &[&str] = &[
    "flv", "ts", "m2ts", "mts", "m2t", "tsv", "trp", "mpg", "mpeg", "mpe", "vob", "ps", "m2p",
    "ogv", "ogm", "m4v", "mkv", "mov", "avi", "wmv", "asf", "rm", "rmvb",
];

/// Every extension (lowercase, no dot) any registered format reader matches.
pub static SUPPORTED_EXTENSIONS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut exts = Vec::new();
    let mut mimes = Vec::new();
    collect(&mut exts, &mut mimes);
    exts
});

/// Every MIME type any registered format reader declares.
pub static SUPPORTED_MIME_TYPES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut exts = Vec::new();
    let mut mimes = Vec::new();
    collect(&mut exts, &mut mimes);
    mimes
});

/// [`SUPPORTED_EXTENSIONS`] minus [`SCAN_EXCLUDED_EXTENSIONS`]: what the
/// library scanner and audio-file pickers accept.
pub static SCAN_EXTENSIONS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    SUPPORTED_EXTENSIONS
        .iter()
        .copied()
        .filter(|e| !SCAN_EXCLUDED_EXTENSIONS.contains(e))
        .collect()
});

fn collect(exts: &mut Vec<&'static str>, mimes: &mut Vec<&'static str>) {
    for reg in probe().formats() {
        // Metadata-only readers (ID3, APE tags...) are not playable containers.
        if !matches!(reg.info(), RegisteredFormatInfo::Format(_)) {
            continue;
        }
        exts.extend(reg.extensions().iter().copied());
        mimes.extend(reg.mime_types().iter().copied());
    }
    exts.extend(EXTRA_EXTENSION_ALIASES.iter().map(|(e, _)| *e));
    exts.sort_unstable();
    exts.dedup();
    mimes.sort_unstable();
    mimes.dedup();
}

/// Whether `ext` (no dot, any case) names a format the probe can demux.
#[must_use]
pub fn is_playable_extension(ext: &str) -> bool {
    SUPPORTED_EXTENSIONS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
}

/// Whether `ext` (no dot, any case) is a playable, scannable audio format.
#[must_use]
pub fn is_supported_extension(ext: &str) -> bool {
    SCAN_EXTENSIONS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// Bytes the probe scans for a container marker before giving up. Symphonia's
/// own default (1 MiB) rejects files with more junk, padding or miscounted
/// tag data than that before the first audio frame.
pub const DEFAULT_MAX_PROBE_DEPTH: u32 = 16 * 1024 * 1024;

/// Lower bound for the probe depth (Symphonia's own default).
pub const MIN_MAX_PROBE_DEPTH: u32 = 1024 * 1024;

static PROBE: LazyLock<Probe> = LazyLock::new(|| {
    let mut probe = Probe::new_with_options(&ProbeOptions {
        max_probe_depth: DEFAULT_MAX_PROBE_DEPTH.max(MIN_MAX_PROBE_DEPTH),
        ..ProbeOptions::default()
    });
    symphonia::default::register_enabled_formats(&mut probe);
    probe
});

/// The probe every local-file and stream open must use instead of
/// `symphonia::default::get_probe()`, so the scanner, tag reader, converter
/// and player agree on which files are readable.
pub fn probe() -> &'static Probe {
    &PROBE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    /// 2 MiB of junk followed by MPEG-1 Layer III frames.
    fn mp3_after_junk() -> Vec<u8> {
        let mut data = vec![0x55u8; 2 * 1024 * 1024];
        for _ in 0..32 {
            data.extend_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
            data.extend_from_slice(&[0u8; 413]);
        }
        data
    }

    fn probes(probe: &Probe, data: Vec<u8>) -> bool {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(data)), Default::default());
        probe
            .probe(
                &Hint::new(),
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .is_ok()
    }

    #[test]
    fn shared_probe_tolerates_more_than_1mib_of_leading_junk() {
        assert!(probes(probe(), mp3_after_junk()));
        // Symphonia's stock 1 MiB probe rejects the same bytes.
        assert!(!probes(symphonia::default::get_probe(), mp3_after_junk()));
    }

    #[test]
    fn extension_list_is_registry_derived() {
        for ext in ["vqf", "ape", "wv", "dsf", "flac", "mp3", "alac", "mka"] {
            assert!(SUPPORTED_EXTENSIONS.contains(&ext), "missing {ext}");
            assert!(is_supported_extension(ext), "not scannable: {ext}");
        }
        assert!(is_supported_extension("FLAC"));
        assert!(is_playable_extension("mkv") && !is_supported_extension("mkv"));
        assert!(SUPPORTED_MIME_TYPES.contains(&"audio/flac"));
    }
}
