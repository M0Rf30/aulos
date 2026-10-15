// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! "Add release to MusicBrainz": turns a cluster of tagged files into a
//! self-submitting HTML form that POSTs the documented seeding fields
//! (<https://wiki.musicbrainz.org/Development/Release_Editor_Seeding>) to
//! MusicBrainz's release editor, which opens pre-filled so the user can
//! review and submit it. This is Picard's "Add cluster as release"
//! approach: it needs no MusicBrainz login in the app, and the edit note
//! says where the data came from.

use std::path::{Path, PathBuf};

use super::matching::FileForMatch;
use crate::library::tagio::{TagSet, keys, model::split_position};

const ADD_URL: &str = "https://musicbrainz.org/release/add";
/// The "Various Artists" special-purpose artist.
const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// One element of a seeded artist credit.
struct Credit {
    name: String,
    mbid: Option<String>,
    join: String,
}

/// Splits a credit into its artists. When `artists` and `ids` line up
/// (several artists, one id each) and the names can all be located in
/// `credit` in order, the text between them becomes the join phrases;
/// otherwise the credit is one name (with its id when there is exactly
/// one).
fn credit_parts(credit: &str, artists: &[String], ids: &[String]) -> Vec<Credit> {
    let single = || {
        vec![Credit {
            name: credit.to_owned(),
            mbid: (ids.len() == 1).then(|| ids[0].clone()),
            join: String::new(),
        }]
    };
    if artists.len() < 2 || artists.len() != ids.len() {
        return single();
    }
    let mut parts: Vec<Credit> = Vec::with_capacity(artists.len());
    let mut at = 0;
    for (i, name) in artists.iter().enumerate() {
        let Some(found) = credit[at..].find(name.as_str()).map(|p| p + at) else {
            return single();
        };
        if i == 0 && found != 0 {
            return single();
        }
        if let Some(prev) = parts.last_mut() {
            prev.join = credit[at..found].to_owned();
        }
        parts.push(Credit {
            name: name.clone(),
            mbid: Some(ids[i].clone()),
            join: String::new(),
        });
        at = found + name.len();
    }
    if at != credit.len() {
        return single();
    }
    parts
}

/// The release-editor name for a lowercase Picard `releasetype` /
/// `releasestatus` value ("ep" → "EP", "dj-mix" → "DJ-mix").
fn mb_name(lower: &str) -> String {
    match lower {
        "ep" => "EP".to_owned(),
        "dj-mix" => "DJ-mix".to_owned(),
        "pseudo-release" => "Pseudo-Release".to_owned(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

fn first_value<'a>(files: &[FileForMatch<'a>], key: &str) -> Option<&'a str> {
    files
        .iter()
        .filter_map(|f| f.tags.get(key))
        .map(str::trim)
        .find(|v| !v.is_empty())
}

fn push(fields: &mut Vec<(String, String)>, name: impl Into<String>, value: impl Into<String>) {
    let value = value.into();
    if !value.trim().is_empty() {
        fields.push((name.into(), value));
    }
}

fn push_credit(fields: &mut Vec<(String, String)>, prefix: &str, parts: &[Credit]) {
    for (i, part) in parts.iter().enumerate() {
        let base = format!("{prefix}.names.{i}");
        push(fields, format!("{base}.name"), part.name.as_str());
        push(fields, format!("{base}.artist.name"), part.name.as_str());
        if let Some(mbid) = &part.mbid {
            push(fields, format!("{base}.mbid"), mbid.as_str());
        }
        if !part.join.is_empty() {
            push(fields, format!("{base}.join_phrase"), part.join.as_str());
        }
    }
}

/// The seeding form fields for `files`, treated as one release: release
/// level data comes from the first file that has each value, files are
/// ordered by disc and track number and split into mediums by disc number.
#[must_use]
pub fn seed_fields(files: &[FileForMatch<'_>]) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    push(
        &mut fields,
        "name",
        first_value(files, keys::ALBUM).unwrap_or_default(),
    );

    // Release artist: the album artist, else the common track artist, else
    // Various Artists.
    let track_artist = |f: &FileForMatch<'_>| {
        f.tags
            .get(keys::ARTIST)
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    };
    let release_artist = if let Some(a) = first_value(files, keys::ALBUMARTIST) {
        let ids = files
            .iter()
            .find(|f| f.tags.get(keys::ALBUMARTIST).is_some())
            .map(|f| f.tags.get_all(keys::MUSICBRAINZ_ALBUMARTISTID).to_vec())
            .unwrap_or_default();
        credit_parts(a, &[], &ids)
    } else {
        let mut names: Vec<String> = files
            .iter()
            .map(track_artist)
            .filter(|a| !a.is_empty())
            .collect();
        names.dedup();
        match names.as_slice() {
            [] => Vec::new(),
            [one] => credit_parts(one, &[], &[]),
            _ => vec![Credit {
                name: "Various Artists".to_owned(),
                mbid: Some(VARIOUS_ARTISTS_MBID.to_owned()),
                join: String::new(),
            }],
        }
    };
    push_credit(&mut fields, "artist_credit", &release_artist);
    let release_artist_text: String = release_artist
        .iter()
        .map(|c| format!("{}{}", c.name, c.join))
        .collect();

    if let Some(v) = first_value(files, keys::MUSICBRAINZ_RELEASEGROUPID) {
        push(&mut fields, "release_group", v);
    }
    for t in files
        .iter()
        .find(|f| !f.tags.get_all(keys::RELEASETYPE).is_empty())
        .map(|f| f.tags.get_all(keys::RELEASETYPE))
        .unwrap_or_default()
    {
        push(&mut fields, "type", mb_name(&t.to_lowercase()));
    }
    if let Some(v) = first_value(files, keys::RELEASESTATUS) {
        push(&mut fields, "status", mb_name(&v.to_lowercase()));
    }
    push(
        &mut fields,
        "language",
        first_value(files, keys::LANGUAGE).unwrap_or_default(),
    );
    push(
        &mut fields,
        "script",
        first_value(files, keys::SCRIPT).unwrap_or_default(),
    );
    push(
        &mut fields,
        "barcode",
        first_value(files, keys::BARCODE).unwrap_or_default(),
    );

    // Release event: date (year[-month[-day]]) and country.
    let date = first_value(files, keys::DATE);
    let country = first_value(files, keys::RELEASECOUNTRY);
    if date.is_some() || country.is_some() {
        let mut parts = date.unwrap_or_default().splitn(3, ['-', '/', '.']);
        for (suffix, part) in ["year", "month", "day"].into_iter().zip(&mut parts) {
            if part.parse::<u32>().is_ok_and(|n| n > 0) {
                push(
                    &mut fields,
                    format!("events.0.date.{suffix}"),
                    part.trim_start_matches('0'),
                );
            }
        }
        push(&mut fields, "events.0.country", country.unwrap_or_default());
    }

    // Label and catalog number.
    let label = first_value(files, keys::LABEL);
    let catno = first_value(files, keys::CATALOGNUMBER);
    if label.is_some() || catno.is_some() {
        push(&mut fields, "labels.0.name", label.unwrap_or_default());
        if let Some(id) = first_value(files, keys::MUSICBRAINZ_LABELID) {
            push(&mut fields, "labels.0.mbid", id);
        }
        push(
            &mut fields,
            "labels.0.catalog_number",
            catno.unwrap_or_default(),
        );
    }

    // Mediums: files ordered by (disc, track), grouped by disc number.
    let number =
        |tags: &TagSet, key: &str| tags.get(key).and_then(|v| split_position(v).0).unwrap_or(0);
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&i| {
        (
            number(files[i].tags, keys::DISCNUMBER).max(1),
            number(files[i].tags, keys::TRACKNUMBER),
            i,
        )
    });
    let mut medium_idx: i64 = -1;
    let mut last_disc = None;
    let mut track_idx = 0;
    for i in order {
        let file = &files[i];
        let tags = file.tags;
        let disc = number(tags, keys::DISCNUMBER).max(1);
        if last_disc != Some(disc) {
            last_disc = Some(disc);
            medium_idx += 1;
            track_idx = 0;
            let m = format!("mediums.{medium_idx}");
            // Disc-level fields from whichever file of the disc has them.
            let on_disc = |key: &str| {
                files
                    .iter()
                    .filter(|f| number(f.tags, keys::DISCNUMBER).max(1) == disc)
                    .filter_map(|f| f.tags.get(key))
                    .find(|v| !v.trim().is_empty())
                    .unwrap_or_default()
            };
            push(&mut fields, format!("{m}.format"), on_disc(keys::MEDIA));
            push(
                &mut fields,
                format!("{m}.name"),
                on_disc(keys::DISCSUBTITLE),
            );
        }
        let t = format!("mediums.{medium_idx}.track.{track_idx}");
        track_idx += 1;
        push(
            &mut fields,
            format!("{t}.name"),
            tags.get(keys::TITLE).or(file.file_stem).unwrap_or_default(),
        );
        let n = number(tags, keys::TRACKNUMBER);
        push(
            &mut fields,
            format!("{t}.number"),
            if n > 0 {
                n.to_string()
            } else {
                track_idx.to_string()
            },
        );
        if let Some(secs) = file.duration_secs.filter(|d| d.is_finite() && *d > 0.0) {
            push(
                &mut fields,
                format!("{t}.length"),
                format!("{}", (secs * 1000.0).round() as u64),
            );
        }
        if let Some(id) = tags.get(keys::MUSICBRAINZ_RECORDINGID) {
            push(&mut fields, format!("{t}.recording"), id);
        }
        // Only credit a track artist that differs from the release's.
        let artist = tags.get(keys::ARTIST).map(str::trim).unwrap_or_default();
        if !artist.is_empty() && artist != release_artist_text {
            let parts = credit_parts(
                artist,
                tags.get_all(keys::ARTISTS),
                tags.get_all(keys::MUSICBRAINZ_ARTISTID),
            );
            push_credit(&mut fields, &format!("{t}.artist_credit"), &parts);
        }
    }

    push(
        &mut fields,
        "edit_note",
        format!("Seeded by Aulos {}", env!("CARGO_PKG_VERSION")),
    );
    fields
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// A page that POSTs `fields` to MusicBrainz's release editor on load
/// (with a visible button when scripting is off).
#[must_use]
pub fn seed_html(fields: &[(String, String)]) -> String {
    let mut html = String::from(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n\
         <title>Aulos: add release to MusicBrainz</title></head>\n<body>\n\
         <p>Sending the release to MusicBrainz&hellip;</p>\n",
    );
    html.push_str(&format!(
        "<form id=\"seed\" method=\"post\" action=\"{ADD_URL}\" accept-charset=\"UTF-8\">\n"
    ));
    for (name, value) in fields {
        html.push_str(&format!(
            "<input type=\"hidden\" name=\"{}\" value=\"{}\">\n",
            escape_attr(name),
            escape_attr(value)
        ));
    }
    html.push_str(
        "<noscript><button type=\"submit\">Open in the MusicBrainz release editor</button></noscript>\n\
         </form>\n<script>document.getElementById('seed').submit();</script>\n</body></html>\n",
    );
    html
}

/// Writes the seeding page for `files` into Aulos's cache directory and
/// returns its path, for the caller to open in the browser.
pub fn seed_release_add(files: &[FileForMatch<'_>]) -> Result<PathBuf, String> {
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("aulos")
        .join("musicbrainz");
    write_seed_page(files, &dir)
}

fn write_seed_page(files: &[FileForMatch<'_>], dir: &Path) -> Result<PathBuf, String> {
    if files.is_empty() {
        return Err("No files to seed a release from".to_owned());
    }
    let fields = seed_fields(files);
    if !fields.iter().any(|(k, _)| k == "name") {
        return Err("Files have no album title: set one before adding the release".to_owned());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let stamp = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = dir.join(format!("add-release-{stamp}.html"));
    std::fs::write(&path, seed_html(&fields))
        .map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &[&str])]) -> TagSet {
        let mut t = TagSet::new();
        for (k, vs) in pairs {
            t.set(k, vs.iter().map(|v| (*v).to_owned()));
        }
        t
    }

    fn file<'a>(tags: &'a TagSet, secs: f64) -> FileForMatch<'a> {
        FileForMatch {
            tags,
            duration_secs: Some(secs),
            file_stem: None,
        }
    }

    fn get<'a>(fields: &'a [(String, String)], name: &str) -> Vec<&'a str> {
        fields
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    #[test]
    fn seeds_release_mediums_and_tracks() {
        let t1 = tags(&[
            (keys::ALBUM, &["Test Album"]),
            (keys::ALBUMARTIST, &["The Band"]),
            (
                keys::MUSICBRAINZ_ALBUMARTISTID,
                &["7754905b-8bf7-48e2-935a-03d566fec464"],
            ),
            (keys::ARTIST, &["The Band"]),
            (keys::TITLE, &["Second"]),
            (keys::TRACKNUMBER, &["2/2"]),
            (keys::DISCNUMBER, &["1/2"]),
            (keys::DATE, &["2021-03-07"]),
            (keys::RELEASECOUNTRY, &["DE"]),
            (keys::LABEL, &["Some Label"]),
            (keys::CATALOGNUMBER, &["SL-001"]),
            (keys::BARCODE, &["0123456789012"]),
            (keys::RELEASETYPE, &["album", "ep"]),
            (keys::RELEASESTATUS, &["official"]),
            (keys::MEDIA, &["CD"]),
        ]);
        let t0 = tags(&[
            (keys::ALBUM, &["Test Album"]),
            (keys::ARTIST, &["The Band"]),
            (keys::TITLE, &["First & <Best>"]),
            (keys::TRACKNUMBER, &["1"]),
            (keys::DISCNUMBER, &["1"]),
            (
                keys::MUSICBRAINZ_RECORDINGID,
                &["c35eb744-c525-44d0-ad21-b6af3e6caad2"],
            ),
        ]);
        let t2 = tags(&[
            (keys::TITLE, &["Guest Song"]),
            (keys::ARTIST, &["The Band feat. Guest"]),
            (keys::ARTISTS, &["The Band", "Guest"]),
            (
                keys::MUSICBRAINZ_ARTISTID,
                &[
                    "7754905b-8bf7-48e2-935a-03d566fec464",
                    "99999999-9999-9999-9999-999999999999",
                ],
            ),
            (keys::TRACKNUMBER, &["1"]),
            (keys::DISCNUMBER, &["2"]),
            (keys::DISCSUBTITLE, &["Bonus"]),
        ]);
        // Deliberately out of order.
        let f = seed_fields(&[file(&t2, 61.5), file(&t1, 200.0), file(&t0, 180.25)]);

        assert_eq!(get(&f, "name"), ["Test Album"]);
        assert_eq!(get(&f, "artist_credit.names.0.name"), ["The Band"]);
        assert_eq!(
            get(&f, "artist_credit.names.0.mbid"),
            ["7754905b-8bf7-48e2-935a-03d566fec464"]
        );
        assert_eq!(get(&f, "type"), ["Album", "EP"]);
        assert_eq!(get(&f, "status"), ["Official"]);
        assert_eq!(get(&f, "barcode"), ["0123456789012"]);
        assert_eq!(get(&f, "events.0.date.year"), ["2021"]);
        assert_eq!(get(&f, "events.0.date.month"), ["3"]);
        assert_eq!(get(&f, "events.0.date.day"), ["7"]);
        assert_eq!(get(&f, "events.0.country"), ["DE"]);
        assert_eq!(get(&f, "labels.0.name"), ["Some Label"]);
        assert_eq!(get(&f, "labels.0.catalog_number"), ["SL-001"]);
        assert_eq!(
            get(&f, "edit_note"),
            [format!("Seeded by Aulos {}", env!("CARGO_PKG_VERSION"))]
        );

        // Disc 1: tracks in number order whatever the input order was.
        assert_eq!(get(&f, "mediums.0.format"), ["CD"]);
        assert_eq!(get(&f, "mediums.0.track.0.name"), ["First & <Best>"]);
        assert_eq!(get(&f, "mediums.0.track.0.number"), ["1"]);
        assert_eq!(get(&f, "mediums.0.track.0.length"), ["180250"]);
        assert_eq!(
            get(&f, "mediums.0.track.0.recording"),
            ["c35eb744-c525-44d0-ad21-b6af3e6caad2"]
        );
        assert_eq!(get(&f, "mediums.0.track.1.name"), ["Second"]);
        assert_eq!(get(&f, "mediums.0.track.1.number"), ["2"]);
        // Same artist as the release: no per-track credit.
        assert!(get(&f, "mediums.0.track.1.artist_credit.names.0.name").is_empty());
        // Disc 2, with a subtitle and a differing, multi-artist credit.
        assert_eq!(get(&f, "mediums.1.name"), ["Bonus"]);
        assert_eq!(get(&f, "mediums.1.track.0.name"), ["Guest Song"]);
        assert_eq!(
            get(&f, "mediums.1.track.0.artist_credit.names.0.name"),
            ["The Band"]
        );
        assert_eq!(
            get(&f, "mediums.1.track.0.artist_credit.names.0.join_phrase"),
            [" feat. "]
        );
        assert_eq!(
            get(&f, "mediums.1.track.0.artist_credit.names.1.name"),
            ["Guest"]
        );
        assert_eq!(
            get(&f, "mediums.1.track.0.artist_credit.names.1.mbid"),
            ["99999999-9999-9999-9999-999999999999"]
        );
    }

    #[test]
    fn differing_artists_without_album_artist_become_various_artists() {
        let a = tags(&[
            (keys::ALBUM, &["Mix"]),
            (keys::ARTIST, &["A"]),
            (keys::TITLE, &["x"]),
        ]);
        let b = tags(&[(keys::ARTIST, &["B"]), (keys::TITLE, &["y"])]);
        let f = seed_fields(&[file(&a, 10.0), file(&b, 10.0)]);
        assert_eq!(get(&f, "artist_credit.names.0.name"), ["Various Artists"]);
        assert_eq!(
            get(&f, "artist_credit.names.0.mbid"),
            [VARIOUS_ARTISTS_MBID]
        );
        assert_eq!(
            get(&f, "mediums.0.track.0.artist_credit.names.0.name"),
            ["A"]
        );
        assert_eq!(
            get(&f, "mediums.0.track.1.artist_credit.names.0.name"),
            ["B"]
        );
        // Missing track numbers fall back to the sequence.
        assert_eq!(get(&f, "mediums.0.track.1.number"), ["2"]);
    }

    #[test]
    fn credit_parts_falls_back_to_a_single_name() {
        let ids = vec!["i1".to_owned(), "i2".to_owned()];
        // Names that can't be located in the credit text.
        let parts = credit_parts("Somebody Else", &["A".into(), "B".into()], &ids);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "Somebody Else");
        assert_eq!(parts[0].mbid, None);
        // Trailing text not covered by the artists.
        let parts = credit_parts("A & B and friends", &["A".into(), "B".into()], &ids);
        assert_eq!(parts.len(), 1);
        let parts = credit_parts("A & B", &["A".into(), "B".into()], &ids);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].join, " & ");
    }

    #[test]
    fn html_escapes_and_targets_the_release_editor() {
        let fields = vec![
            ("name".to_owned(), "Q\"uote & <tag> 'x'".to_owned()),
            ("mediums.0.track.0.name".to_owned(), "ok".to_owned()),
        ];
        let html = seed_html(&fields);
        assert!(html.contains("method=\"post\" action=\"https://musicbrainz.org/release/add\""));
        assert!(html.contains(
            "<input type=\"hidden\" name=\"name\" value=\"Q&quot;uote &amp; &lt;tag&gt; &#39;x&#39;\">"
        ));
        assert!(html.contains("name=\"mediums.0.track.0.name\" value=\"ok\""));
        assert!(html.contains("document.getElementById('seed').submit()"));
        assert!(html.contains("<noscript>"));
    }

    #[test]
    fn writes_the_page_into_the_given_dir() {
        let dir = std::env::temp_dir().join(format!("aulos-seed-test-{}", std::process::id()));
        let t = tags(&[(keys::ALBUM, &["Written"]), (keys::TITLE, &["t"])]);
        let path = write_seed_page(&[file(&t, 5.0)], &dir).unwrap();
        assert!(path.starts_with(&dir));
        let html = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(html.contains("value=\"Written\""));
        let no_album = tags(&[(keys::TITLE, &["t"])]);
        assert!(write_seed_page(&[file(&no_album, 5.0)], &dir).is_err());
        assert!(write_seed_page(&[], &dir).is_err());
        assert!(!dir.exists(), "nothing is created for invalid input");
    }
}
