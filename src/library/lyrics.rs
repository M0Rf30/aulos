// SPDX-License-Identifier: GPL-3.0

//! Lyrics fetching from embedded tags and online sources.

use super::tags;
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

use super::{LyricLine, Lyrics};

/// Regex matching a single LRC timestamp: `[mm:ss.xx]` or `[mm:ss.xxx]`.
static LRC_TIMESTAMP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[(\d{2,}):(\d{2})\.(\d{2,3})\]").unwrap());

/// Parse LRC-formatted text into a [`Lyrics`] value.
///
/// Recognised timestamp formats:
///   - `[mm:ss.xx]text`   (centiseconds)
///   - `[mm:ss.xxx]text`  (milliseconds)
///
/// Multiple timestamps may precede the same text on one line, e.g.
/// `[01:23.45][02:34.56]shared text`.
///
/// Lines without timestamps are ignored when at least one timestamped line
/// exists.  If no timestamps are found at all the full input is returned as
/// [`Lyrics::Unsynced`].
pub fn parse_lrc(text: &str) -> Lyrics {
    let mut lines: Vec<LyricLine> = Vec::new();

    for raw_line in text.lines() {
        let raw_line = raw_line.trim();
        if raw_line.is_empty() {
            continue;
        }

        // Collect all timestamps on this line.
        let timestamps: Vec<u64> = LRC_TIMESTAMP_RE
            .captures_iter(raw_line)
            .filter_map(|cap| {
                let minutes: u64 = cap[1].parse().ok()?;
                let seconds: u64 = cap[2].parse().ok()?;
                let frac_str = &cap[3];
                let frac_ms: u64 = if frac_str.len() == 2 {
                    // Centiseconds → milliseconds
                    frac_str.parse::<u64>().ok()? * 10
                } else {
                    // Already milliseconds
                    frac_str.parse().ok()?
                };
                Some(minutes * 60_000 + seconds * 1_000 + frac_ms)
            })
            .collect();

        if timestamps.is_empty() {
            continue;
        }

        // Strip all timestamps from the line to get the text portion.
        let lyric_text = LRC_TIMESTAMP_RE
            .replace_all(raw_line, "")
            .trim()
            .to_string();

        for ts in timestamps {
            lines.push(LyricLine {
                timestamp_ms: ts,
                text: lyric_text.clone(),
            });
        }
    }

    if lines.is_empty() {
        Lyrics::Unsynced(text.to_string())
    } else {
        lines.sort_by_key(|l| l.timestamp_ms);
        Lyrics::Synced(lines)
    }
}

/// Returns `true` if the text looks like it contains LRC timestamps.
fn looks_like_lrc(text: &str) -> bool {
    LRC_TIMESTAMP_RE.is_match(text)
}

/// Provides lyrics from various sources.
pub struct LyricsProvider;

impl LyricsProvider {
    /// Extract embedded lyrics from the audio file's tags.
    ///
    /// If the embedded text contains LRC timestamps it is parsed into
    /// [`Lyrics::Synced`]; otherwise it is returned as [`Lyrics::Unsynced`].
    pub fn from_tags(path: &Path) -> Option<Lyrics> {
        let probed = tags::probe(path, false)?;
        let text = probed.tags.lyrics?;

        if text.trim().is_empty() {
            return None;
        }

        if looks_like_lrc(&text) {
            Some(parse_lrc(&text))
        } else {
            Some(Lyrics::Unsynced(text))
        }
    }

    /// Fetch lyrics from the LRCLIB API (free, no key required).
    ///
    /// Returns [`Lyrics::Synced`] when the API provides `syncedLyrics`,
    /// otherwise [`Lyrics::Unsynced`] from `plainLyrics`.
    pub async fn fetch_online(artist: &str, title: &str) -> Option<Lyrics> {
        let url = format!(
            "https://lrclib.net/api/get?artist_name={}&track_name={}",
            urlencoded(artist),
            urlencoded(title),
        );

        let response = reqwest::get(&url).await.ok()?;

        if !response.status().is_success() {
            return None;
        }

        let json: serde_json::Value = response.json().await.ok()?;

        // Prefer synced lyrics — parse the LRC text into Lyrics::Synced.
        if let Some(synced) = json.get("syncedLyrics").and_then(|v| v.as_str())
            && !synced.trim().is_empty()
        {
            return Some(parse_lrc(synced));
        }

        // Fall back to plain lyrics.
        json.get("plainLyrics")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| Lyrics::Unsynced(s.to_string()))
    }

    /// Look for a `.lrc` file next to the audio file.
    ///
    /// The file content is parsed with [`parse_lrc`]; if no timestamps are
    /// found the text is returned as [`Lyrics::Unsynced`].
    pub fn from_lrc_file(audio_path: &Path) -> Option<Lyrics> {
        let lrc_path = audio_path.with_extension("lrc");
        if lrc_path.exists() {
            let text = std::fs::read_to_string(&lrc_path).ok()?;
            if text.trim().is_empty() {
                return None;
            }
            if looks_like_lrc(&text) {
                Some(parse_lrc(&text))
            } else {
                Some(Lyrics::Unsynced(text))
            }
        } else {
            None
        }
    }

    /// Try all sources: embedded tags → .lrc file → online.
    pub async fn get_lyrics(path: &Path, artist: &str, title: &str) -> Option<Lyrics> {
        // 1. Embedded in tags
        if let Some(lyrics) = Self::from_tags(path) {
            return Some(lyrics);
        }

        // 2. .lrc sidecar file
        if let Some(lyrics) = Self::from_lrc_file(path) {
            return Some(lyrics);
        }

        // 3. Online (LRCLIB)
        Self::fetch_online(artist, title).await
    }
}

/// Simple URL encoding for query parameters.
fn urlencoded(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            ' ' => "+".to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_lrc() {
        let input = "[00:12.34]Hello world\n[00:15.67]Second line\n";
        let lyrics = parse_lrc(input);
        match lyrics {
            Lyrics::Synced(lines) => {
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].timestamp_ms, 12_340);
                assert_eq!(lines[0].text, "Hello world");
                assert_eq!(lines[1].timestamp_ms, 15_670);
                assert_eq!(lines[1].text, "Second line");
            }
            Lyrics::Unsynced(_) => panic!("Expected Synced lyrics"),
        }
    }

    #[test]
    fn parse_multiple_timestamps_per_line() {
        let input = "[01:00.00][02:00.00]Chorus\n";
        let lyrics = parse_lrc(input);
        match lyrics {
            Lyrics::Synced(lines) => {
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].timestamp_ms, 60_000);
                assert_eq!(lines[0].text, "Chorus");
                assert_eq!(lines[1].timestamp_ms, 120_000);
                assert_eq!(lines[1].text, "Chorus");
            }
            Lyrics::Unsynced(_) => panic!("Expected Synced lyrics"),
        }
    }

    #[test]
    fn parse_millisecond_format() {
        let input = "[00:05.123]Three digit frac\n";
        let lyrics = parse_lrc(input);
        match lyrics {
            Lyrics::Synced(lines) => {
                assert_eq!(lines.len(), 1);
                assert_eq!(lines[0].timestamp_ms, 5_123);
            }
            Lyrics::Unsynced(_) => panic!("Expected Synced lyrics"),
        }
    }

    #[test]
    fn fallback_to_unsynced() {
        let input = "Just some plain text\nNo timestamps here\n";
        let lyrics = parse_lrc(input);
        match lyrics {
            Lyrics::Unsynced(text) => assert_eq!(text, input),
            Lyrics::Synced(_) => panic!("Expected Unsynced lyrics"),
        }
    }

    #[test]
    fn sorted_output() {
        let input = "[01:00.00]Later\n[00:30.00]Earlier\n";
        let lyrics = parse_lrc(input);
        match lyrics {
            Lyrics::Synced(lines) => {
                assert_eq!(lines[0].timestamp_ms, 30_000);
                assert_eq!(lines[0].text, "Earlier");
                assert_eq!(lines[1].timestamp_ms, 60_000);
                assert_eq!(lines[1].text, "Later");
            }
            Lyrics::Unsynced(_) => panic!("Expected Synced lyrics"),
        }
    }
}
