// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Cross-format contract of the Picard layer: for every writable container,
//! `read(write(tags)) == tags` with the full Picard field set, and writing
//! an empty set removes every field and picture.

use super::{EditPicture, TagSet, keys, picture_type, read, write};
use std::path::{Path, PathBuf};

fn fixture_copy(name: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tagio")
        .join(name);
    let dir = std::env::temp_dir().join(format!("aulos-tagio-rt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dst = dir.join(name);
    std::fs::copy(&src, &dst).unwrap();
    dst
}

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([200, 30, 30]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

fn rich_tags() -> TagSet {
    let mut t = TagSet::new();
    t.set_one(keys::TITLE, "Tïtle ✓");
    t.set(keys::ARTIST, ["Artist A".into(), "Artist B".into()]);
    t.set_one(keys::ALBUM, "Albüm");
    t.set_one(keys::ALBUMARTIST, "Album Artist");
    t.set_one(keys::DATE, "2001-05-03");
    t.set_one(keys::ORIGINALDATE, "1999-01-02");
    t.set(keys::GENRE, ["Electronic".into(), "Ambient".into()]);
    t.set_one(keys::TRACKNUMBER, "3");
    t.set_one(keys::TOTALTRACKS, "12");
    t.set_one(keys::DISCNUMBER, "1");
    t.set_one(keys::TOTALDISCS, "2");
    t.set_one(keys::COMPOSER, "Composer");
    t.set_one(keys::COMMENT, "A comment");
    t.set_one(keys::LYRICS, "La la la");
    t.set_one(keys::LABEL, "Label");
    t.set_one(keys::CATALOGNUMBER, "CAT-001");
    t.set_one(keys::BARCODE, "0123456789012");
    t.set_one(keys::ISRC, "DEABC0100001");
    t.set_one(keys::MEDIA, "CD");
    t.set_one(keys::RELEASECOUNTRY, "DE");
    t.set_one(keys::COMPILATION, "1");
    t.set_one(
        keys::MUSICBRAINZ_RECORDINGID,
        "11111111-1111-1111-1111-111111111111",
    );
    t.set_one(
        keys::MUSICBRAINZ_TRACKID,
        "22222222-2222-2222-2222-222222222222",
    );
    t.set_one(
        keys::MUSICBRAINZ_ALBUMID,
        "33333333-3333-3333-3333-333333333333",
    );
    t.set(
        keys::MUSICBRAINZ_ARTISTID,
        [
            "44444444-4444-4444-4444-444444444444".into(),
            "55555555-5555-5555-5555-555555555555".into(),
        ],
    );
    t.set_one(
        keys::MUSICBRAINZ_ALBUMARTISTID,
        "66666666-6666-6666-6666-666666666666",
    );
    t.set_one(
        keys::MUSICBRAINZ_RELEASEGROUPID,
        "77777777-7777-7777-7777-777777777777",
    );
    t.set_one(keys::ACOUSTID_ID, "88888888-8888-8888-8888-888888888888");
    t.set_one(keys::REPLAYGAIN_TRACK_GAIN, "-6.50 dB");
    t.set_one("aulos_custom", "custom value");
    t.pictures.push(EditPicture {
        kind: picture_type::FRONT_COVER,
        mime: "image/png".into(),
        description: String::new(),
        data: png(),
    });
    t
}

/// `(key, values)` pairs that differ between `want` and `got`.
fn diff(want: &TagSet, got: &TagSet) -> Vec<String> {
    let mut out = Vec::new();
    let mut keys: Vec<&str> = want.iter().map(|(k, _)| k).collect();
    keys.extend(got.iter().map(|(k, _)| k));
    keys.sort_unstable();
    keys.dedup();
    for k in keys {
        if want.get_all(k) != got.get_all(k) {
            out.push(format!(
                "{k}: want {:?}, got {:?}",
                want.get_all(k),
                got.get_all(k)
            ));
        }
    }
    let pics = |t: &TagSet| {
        t.pictures
            .iter()
            .map(|p| (p.kind, p.mime.clone(), p.data.clone()))
            .collect::<Vec<_>>()
    };
    if pics(want) != pics(got) {
        out.push(format!(
            "pictures: want {} got {}",
            want.pictures.len(),
            got.pictures.len()
        ));
    }
    out
}

fn round_trip(name: &str) {
    let path = fixture_copy(name);
    let tags = rich_tags();
    write(&path, &tags).unwrap_or_else(|e| panic!("{name}: write: {e}"));
    let back = read(&path)
        .unwrap_or_else(|e| panic!("{name}: read: {e}"))
        .tags;
    let d = diff(&tags, &back);
    assert!(d.is_empty(), "{name} round trip:\n{}", d.join("\n"));

    write(&path, &TagSet::new()).unwrap_or_else(|e| panic!("{name}: clear: {e}"));
    let cleared = read(&path).unwrap().tags;
    let d = diff(&TagSet::new(), &cleared);
    assert!(d.is_empty(), "{name} clear:\n{}", d.join("\n"));
}

#[test]
fn mp3_id3v24() {
    round_trip("plain.mp3");
}

#[test]
fn flac() {
    round_trip("tagged.flac");
}

#[test]
fn ogg_vorbis() {
    round_trip("plain.ogg");
}

#[test]
fn ogg_opus() {
    round_trip("tagged.opus");
}

#[test]
fn mp4() {
    round_trip("tagged.m4a");
}

#[test]
fn wav() {
    round_trip("plain.wav");
}

#[test]
fn aiff() {
    round_trip("plain.aiff");
}

#[test]
fn wavpack() {
    round_trip("plain.wv");
}
