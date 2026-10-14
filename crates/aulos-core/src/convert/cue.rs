// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Minimal CUE sheet parser.
//!
//! Handles the subset actually needed to split a single ripped audio file
//! into per-track outputs: `FILE`, `TRACK nn AUDIO`, `TITLE`, `PERFORMER`,
//! and `INDEX 01 mm:ss:ff` (75 frames/second). Anything else (`REM`,
//! `FLAGS`, `INDEX 00` pre-gaps, `CATALOG`, ...) is recognized and ignored
//! rather than erroring, since CUE sheets in the wild vary a lot and none
//! of it is needed for splitting.

use std::time::Duration;

/// One track parsed from a CUE sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct CueTrack {
    pub number: u32,
    pub title: String,
    pub performer: String,
    /// Start offset into the referenced audio file.
    pub start: Duration,
    /// End offset (the next track's start), or `None` for the last track
    /// (meaning "until end of file").
    pub end: Option<Duration>,
}

#[derive(Debug, thiserror::Error)]
pub enum CueError {
    #[error("no TRACK entries found in CUE sheet")]
    NoTracks,
    #[error("invalid INDEX timestamp on line {line}: {text:?}")]
    BadTimestamp { line: usize, text: String },
    #[error(
        "track {track}'s INDEX 01 doesn't come after the previous track's; CUE indexes must increase"
    )]
    OutOfOrder { track: u32 },
}

/// Parses a CUE sheet's tracks, in order, with `start`/`end` offsets
/// resolved from each track's `INDEX 01` timestamp.
pub fn parse(input: &str) -> Result<Vec<CueTrack>, CueError> {
    struct Building {
        number: u32,
        title: String,
        performer: String,
        start: Option<Duration>,
    }

    let mut album_performer = String::new();
    let mut current: Option<Building> = None;
    let mut tracks: Vec<Building> = Vec::new();

    for (lineno, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("REM") {
            continue;
        }
        let (keyword, rest) = split_keyword(line);

        match keyword {
            "PERFORMER" if current.is_none() => album_performer = unquote(rest),
            "TRACK" => {
                if let Some(b) = current.take() {
                    tracks.push(b);
                }
                let number = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| tracks.len() as u32 + 1);
                current = Some(Building {
                    number,
                    title: String::new(),
                    performer: album_performer.clone(),
                    start: None,
                });
            }
            "TITLE" => {
                if let Some(b) = current.as_mut() {
                    b.title = unquote(rest);
                }
            }
            "PERFORMER" => {
                if let Some(b) = current.as_mut() {
                    b.performer = unquote(rest);
                }
            }
            "INDEX" => {
                if let Some(b) = current.as_mut() {
                    let mut parts = rest.split_whitespace();
                    let index_number = parts.next();
                    let timestamp = parts.next();
                    if index_number == Some("01")
                        && let Some(ts) = timestamp
                    {
                        b.start =
                            Some(parse_timestamp(ts).ok_or_else(|| CueError::BadTimestamp {
                                line: lineno + 1,
                                text: ts.to_owned(),
                            })?);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(b) = current.take() {
        tracks.push(b);
    }
    if tracks.is_empty() {
        return Err(CueError::NoTracks);
    }

    let starts: Vec<Duration> = tracks.iter().map(|t| t.start.unwrap_or_default()).collect();
    for i in 1..starts.len() {
        if starts[i] < starts[i - 1] {
            return Err(CueError::OutOfOrder {
                track: tracks[i].number,
            });
        }
    }
    Ok(tracks
        .into_iter()
        .enumerate()
        .map(|(i, b)| CueTrack {
            number: b.number,
            title: b.title,
            performer: b.performer,
            start: starts[i],
            end: starts.get(i + 1).copied(),
        })
        .collect())
}

/// Extracts the quoted filename from a CUE sheet's `FILE "name" TYPE` line,
/// for resolving the referenced audio file relative to the `.cue` path.
pub fn parse_file_name(input: &str) -> Option<String> {
    input.lines().find_map(|raw_line| {
        let line = raw_line.trim();
        let (keyword, rest) = split_keyword(line);
        (keyword == "FILE").then(|| unquote(rest.split_whitespace().next().unwrap_or(rest)))
    })
}

/// Splits a line into its leading keyword and the (trimmed) remainder.
fn split_keyword(line: &str) -> (&str, &str) {
    match line.find(char::is_whitespace) {
        Some(pos) => (&line[..pos], line[pos..].trim_start()),
        None => (line, ""),
    }
}

/// Strips a single pair of surrounding double quotes, if present.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_owned()
    } else {
        s.to_owned()
    }
}

/// Parses a CUE `mm:ss:ff` timestamp (75 frames/second) into a [`Duration`].
fn parse_timestamp(ts: &str) -> Option<Duration> {
    let mut parts = ts.split(':');
    let minutes: u64 = parts.next()?.parse().ok()?;
    let seconds: u64 = parts.next()?.parse().ok()?;
    let frames: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let total_frames = minutes * 60 * 75 + seconds * 75 + frames;
    Some(Duration::from_secs_f64(total_frames as f64 / 75.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
REM GENRE "Progressive Rock"
REM DATE 1977
PERFORMER "Pink Floyd"
TITLE "Animals"
FILE "animals.flac" WAVE
  TRACK 01 AUDIO
    TITLE "Pigs on the Wing, Pt. 1"
    PERFORMER "Pink Floyd"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "Dogs"
    PERFORMER "Pink Floyd"
    INDEX 00 01:23:50
    INDEX 01 01:24:00
"#;

    #[test]
    fn parses_two_track_fixture() {
        let tracks = parse(FIXTURE).expect("fixture should parse");
        assert_eq!(tracks.len(), 2);

        assert_eq!(tracks[0].number, 1);
        assert_eq!(tracks[0].title, "Pigs on the Wing, Pt. 1");
        assert_eq!(tracks[0].performer, "Pink Floyd");
        assert_eq!(tracks[0].start, Duration::ZERO);
        assert_eq!(tracks[0].end, Some(Duration::from_secs(84)));

        assert_eq!(tracks[1].number, 2);
        assert_eq!(tracks[1].title, "Dogs");
        assert_eq!(tracks[1].performer, "Pink Floyd");
        assert_eq!(tracks[1].start, Duration::from_secs(84));
        assert_eq!(tracks[1].end, None);
    }

    #[test]
    fn parses_file_name() {
        assert_eq!(parse_file_name(FIXTURE), Some("animals.flac".to_owned()));
    }

    #[test]
    fn rejects_sheet_with_no_tracks() {
        assert!(matches!(
            parse("REM nothing here\n"),
            Err(CueError::NoTracks)
        ));
    }

    #[test]
    fn rejects_bad_timestamp() {
        let bad = "TRACK 01 AUDIO\nINDEX 01 not-a-timestamp\n";
        assert!(matches!(parse(bad), Err(CueError::BadTimestamp { .. })));
    }

    #[test]
    fn rejects_out_of_order_track_indexes() {
        let sheet = "TRACK 01 AUDIO\nINDEX 01 02:00:00\nTRACK 02 AUDIO\nINDEX 01 01:00:00\n";
        assert!(matches!(
            parse(sheet),
            Err(CueError::OutOfOrder { track: 2 })
        ));
    }
}
