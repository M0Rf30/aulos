// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Reads a file's tags through the Symphonia fork's own probe and turns them
//! into a [`TagSet`] of Picard keys (see [`super::map`]).

use super::map;
use super::model::EditPicture;
use super::{FileTags, Result, TagError, TagSet};
use crate::library::tags::infer_mime;
use crate::player::engine::decoder::local_file_format_options;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::mem::{Discriminant, discriminant};
use std::path::Path;
use symphonia::core::formats::TrackType;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{
    MetadataOptions, MetadataRevision, RawTag, RawValue, StandardTag, Tag, Visual,
};

pub(super) fn read(path: &Path, container: super::Container) -> Result<FileTags> {
    let file = File::open(path)?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut reader = crate::player::engine::format_registry::probe()
        .probe(
            &hint,
            mss,
            local_file_format_options(path),
            MetadataOptions::default(),
        )
        .map_err(probe_error)?;

    let track_id = reader
        .default_track(TrackType::Audio)
        .map(|t| u64::from(t.id));

    // `pop` discards the oldest revision, leaving the newest as `current`.
    let mut log = reader.metadata();
    let mut older = Vec::new();
    while let Some(rev) = log.pop() {
        older.push(rev);
    }
    let revisions: Vec<&MetadataRevision> = log
        .current()
        .into_iter()
        .chain(older.iter().rev())
        .collect();

    Ok(FileTags {
        container,
        tags: tag_set(&revisions, track_id),
    })
}

fn probe_error(e: symphonia::core::errors::Error) -> TagError {
    use symphonia::core::errors::Error;
    match e {
        Error::IoError(e) => TagError::Io(e),
        Error::Unsupported(s) => TagError::Unsupported(s.to_owned()),
        other => TagError::Malformed(other.to_string()),
    }
}

/// Merges `revisions` (newest first) into one [`TagSet`].
///
/// A field is taken only from the newest revision that has it, so a stale or
/// truncated older block (ID3v1's 30-character fields) never shadows or
/// duplicates the real one, while a sparse newer block doesn't hide the
/// older block's fields. Within a revision every value is kept
/// (multi-value fields).
fn tag_set(revisions: &[&MetadataRevision], track_id: Option<u64>) -> TagSet {
    let mut out = TagSet::new();
    let mut seen_std: HashSet<Discriminant<StandardTag>> = HashSet::new();
    let mut seen_raw: HashSet<String> = HashSet::new();
    // Picard key -> the standard tag kind that supplied it (several kinds
    // alias the same key, e.g. `RecordingDate` and `RecordingYear`).
    let mut owner: HashMap<&'static str, Discriminant<StandardTag>> = HashMap::new();

    for rev in revisions {
        let mut rev_std = Vec::new();
        let mut rev_raw = Vec::new();
        for tag in revision_tags(rev, track_id) {
            if let Some(std) = &tag.std {
                let kind = discriminant(std);
                if seen_std.contains(&kind) || matches!(std, StandardTag::CompilationFlag(false)) {
                    continue;
                }
                rev_std.push(kind);
                if let Some((key, value)) = map::key_for(std) {
                    if owner.get(key).is_some_and(|o| *o != kind) {
                        continue;
                    }
                    owner.insert(key, kind);
                    push_unique(&mut out, key, value);
                    continue;
                }
                // A standard tag Aulos has no Picard key for: keep it under
                // its raw key, except bare ID3 frame ids (`TSSE`, ...) that
                // would otherwise come back as meaningless free-form fields.
                if tag.raw.sub_fields.is_none() && looks_like_frame_id(&tag.raw.key) {
                    continue;
                }
            }
            let Some(key) = raw_key(&tag.raw) else {
                continue;
            };
            if seen_raw.contains(&key) {
                continue;
            }
            rev_raw.push(key.clone());
            for value in raw_values(&tag.raw.value) {
                push_unique(&mut out, &key, value);
            }
        }
        seen_std.extend(rev_std);
        seen_raw.extend(rev_raw);
    }

    if let Some(rev) = revisions.iter().find(|r| !r.media.visuals.is_empty()) {
        out.pictures = rev.media.visuals.iter().map(picture).collect();
    }
    out
}

fn push_unique(out: &mut TagSet, key: &str, value: String) {
    let value = value.trim().to_owned();
    if !out.get_all(key).contains(&value) {
        out.push(key, value);
    }
}

/// The revision's media-level tags plus the default audio track's own tags
/// (Matroska routes track-targeted tags there).
fn revision_tags(rev: &MetadataRevision, track_id: Option<u64>) -> Vec<&Tag> {
    let mut tags: Vec<&Tag> = rev.media.tags.iter().collect();
    if let Some(id) = track_id {
        tags.extend(
            rev.per_track
                .iter()
                .filter(|pt| pt.track_id == id)
                .flat_map(|pt| pt.metadata.tags.iter()),
        );
    }
    tags
}

fn looks_like_frame_id(key: &str) -> bool {
    matches!(key.len(), 3 | 4)
        && key
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// The lowercase free-form key of a tag Aulos has no Picard key for: the
/// ID3 `TXXX` description, the MP4 freeform name, or the raw key itself.
fn raw_key(raw: &RawTag) -> Option<String> {
    let mut key = raw.key.clone();
    if key.eq_ignore_ascii_case("TXXX") || key.eq_ignore_ascii_case("WXXX") {
        key = raw
            .sub_fields
            .as_deref()?
            .iter()
            .find(|f| f.field.eq_ignore_ascii_case("DESCRIPTION"))
            .map(|f| f.value.to_string())?;
    }
    for prefix in ["TXXX:", "----:", "com.apple.iTunes:"] {
        if key.len() >= prefix.len() && key[..prefix.len()].eq_ignore_ascii_case(prefix) {
            key.drain(..prefix.len());
        }
    }
    let key = key.trim().to_lowercase();
    (!key.is_empty()).then_some(key)
}

fn raw_values(value: &RawValue) -> Vec<String> {
    match value {
        RawValue::String(s) => vec![s.to_string()],
        RawValue::StringList(l) => l.iter().cloned().collect(),
        RawValue::UnsignedInt(_) | RawValue::SignedInt(_) | RawValue::Float(_) => {
            vec![value.to_string()]
        }
        RawValue::Binary(_) | RawValue::Boolean(_) | RawValue::Flag => Vec::new(),
        _ => Vec::new(),
    }
}

fn picture(v: &Visual) -> EditPicture {
    let description = v
        .tags
        .iter()
        .find_map(|t| match &t.std {
            Some(StandardTag::Description(d)) => Some(d.to_string()),
            _ => None,
        })
        .unwrap_or_default();
    EditPicture {
        kind: map::type_for_usage(v.usage),
        mime: v
            .media_type
            .clone()
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| infer_mime(&v.data).to_owned()),
        description,
        data: v.data.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::tagio::keys;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tagio")
            .join(name)
    }

    fn read_fixture(name: &str) -> FileTags {
        read(&fixture(name), super::super::sniff(&fixture(name)).unwrap()).unwrap()
    }

    const REC: &str = "33333333-3333-3333-3333-333333333333";
    const REL_TRACK: &str = "22222222-2222-2222-2222-222222222222";
    const ALBUM: &str = "11111111-1111-1111-1111-111111111111";

    fn common(t: &TagSet) {
        assert_eq!(t.get(keys::TITLE), Some("Song"));
        assert_eq!(t.get(keys::ARTIST), Some("Artist"));
        assert_eq!(t.get(keys::ALBUMARTIST), Some("AlbumArtist"));
        assert_eq!(t.get(keys::ALBUM), Some("Album"));
        assert_eq!(t.get(keys::GENRE), Some("Rock"));
        assert_eq!(t.get(keys::COMPOSER), Some("Composer"));
        assert_eq!(t.get(keys::TRACKNUMBER), Some("3"));
        assert_eq!(t.get(keys::TOTALTRACKS), Some("12"));
        assert_eq!(t.get(keys::DISCNUMBER), Some("1"));
        assert_eq!(t.get(keys::TOTALDISCS), Some("2"));
    }

    #[test]
    fn mp3_id3v23_with_txxx_and_ufid() {
        let f = read_fixture("v23.mp3");
        assert_eq!(f.container, super::super::Container::MpegAudio);
        common(&f.tags);
        // The fork's reader combines the legacy TYER + TDAT frames.
        assert_eq!(f.tags.get(keys::DATE), Some("2001-05-03"));
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_RECORDINGID), Some(REC));
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_TRACKID), Some(REL_TRACK));
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_ALBUMID), Some(ALBUM));
        assert_eq!(f.tags.get("customfield"), Some("custom"));
        // No frame ids or `TXXX:` prefixes leak into keys.
        assert!(
            f.tags
                .iter()
                .all(|(k, _)| k != "tsse" && !k.starts_with("txxx"))
        );
    }

    #[test]
    fn flac_vorbis_comments() {
        let f = read_fixture("tagged.flac");
        assert_eq!(f.container, super::super::Container::Flac);
        common(&f.tags);
        assert_eq!(f.tags.get(keys::DATE), Some("2001-05-03"));
        assert_eq!(f.tags.get(keys::ORIGINALDATE), Some("1999-01-01"));
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_RECORDINGID), Some(REC));
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_TRACKID), Some(REL_TRACK));
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_ALBUMID), Some(ALBUM));
        assert_eq!(f.tags.get("customfield"), Some("custom"));
    }

    #[test]
    fn opus_vorbis_comments() {
        let f = read_fixture("tagged.opus");
        assert_eq!(f.container, super::super::Container::OggOpus);
        common(&f.tags);
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_RECORDINGID), Some(REC));
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_TRACKID), Some(REL_TRACK));
        assert_eq!(f.tags.get("customfield"), Some("custom"));
    }

    #[test]
    fn m4a_ilst_and_freeform() {
        let f = read_fixture("tagged.m4a");
        assert_eq!(f.container, super::super::Container::Mp4);
        common(&f.tags);
        assert_eq!(f.tags.get(keys::DATE), Some("2001-05-03"));
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_RECORDINGID), Some(REC));
        assert_eq!(f.tags.get(keys::MUSICBRAINZ_TRACKID), Some(REL_TRACK));
        assert_eq!(f.tags.get("customfield"), Some("custom"));
        assert!(
            f.tags
                .iter()
                .all(|(k, _)| !k.starts_with("----") && !k.contains("apple"))
        );
    }
}
