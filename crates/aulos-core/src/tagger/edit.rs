// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The metadata table over a selection of files, and the edits it makes.
//!
//! Picard's metadata box shows one row per field of the union of the
//! selected files' tags, with the value on disk next to the value being
//! built; where the selected files disagree the cell shows a "different
//! values" marker, and typing a value sets it on every selected file. The
//! functions here compute those rows and apply those edits to plain
//! [`TagSet`]s; [`super::TaggerState`] drives them for the selection.

use super::file::TagFile;
use crate::library::tagio::{EditPicture, TagSet, keys, model::KNOWN_KEYS, picture_type};
use std::collections::BTreeSet;
use std::hash::{Hash, Hasher};

/// What separates the values of a multi-valued field in the editor's text
/// box (`Rock; Electronic`).
pub const SEPARATOR: &str = "; ";

/// Fields that hold a list of values. Every other field is one value, and
/// a `;` inside it is just text (`Title; Part 2`) — unless the file already
/// stores several values for it, in which case it is treated as a list too.
pub const MULTI_VALUED_KEYS: &[&str] = &[
    keys::ARTISTS,
    keys::GENRE,
    keys::COMPOSER,
    keys::LYRICIST,
    keys::CONDUCTOR,
    keys::ARRANGER,
    keys::PRODUCER,
    keys::ENGINEER,
    keys::MIXER,
    keys::REMIXER,
    keys::LABEL,
    keys::CATALOGNUMBER,
    keys::ISRC,
    keys::RELEASECOUNTRY,
    keys::RELEASETYPE,
    keys::MOOD,
    keys::LANGUAGE,
    keys::MUSICBRAINZ_ARTISTID,
    keys::MUSICBRAINZ_ALBUMARTISTID,
    keys::MUSICBRAINZ_LABELID,
    keys::MUSICBRAINZ_WORKID,
];

/// Whether `key` is stored as a list of values.
#[must_use]
pub fn is_multi_valued(key: &str) -> bool {
    MULTI_VALUED_KEYS.contains(&key)
}

/// `values` as the editor's text: joined with [`SEPARATOR`].
#[must_use]
pub fn join_values(values: &[String]) -> String {
    values.join(SEPARATOR)
}

/// The values a field takes from the text typed into its box.
/// `current_values` is how many values the field has now (the most of any
/// selected file): a field that already holds a list stays a list.
#[must_use]
pub fn parse_values(key: &str, text: &str, current_values: usize) -> Vec<String> {
    if is_multi_valued(key) || current_values > 1 {
        text.split(';')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        let value = text.trim();
        if value.is_empty() {
            Vec::new()
        } else {
            vec![value.to_owned()]
        }
    }
}

/// Normalises a field name typed by the user (trimmed, lowercase); `None`
/// when it can't be a tag name: empty, or containing anything but visible
/// ASCII, or `=` (not representable in Vorbis comments).
#[must_use]
pub fn normalize_key(raw: &str) -> Option<String> {
    let key = raw.trim().to_ascii_lowercase();
    let valid = !key.is_empty() && key.bytes().all(|b| b.is_ascii_graphic() && b != b'=');
    valid.then_some(key)
}

/// What a field holds across the selected files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// No selected file has the field.
    Empty,
    /// Every selected file has exactly these values.
    Same(Vec<String>),
    /// The selected files disagree.
    Mixed,
}

impl FieldValue {
    /// The editor text: the values joined, empty for `Empty` and `Mixed`.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Same(values) => join_values(values),
            Self::Empty | Self::Mixed => String::new(),
        }
    }

    #[must_use]
    pub fn is_mixed(&self) -> bool {
        matches!(self, Self::Mixed)
    }

    fn combine<'a>(mut values: impl Iterator<Item = &'a [String]>) -> Self {
        let Some(first) = values.next() else {
            return Self::Empty;
        };
        if values.any(|v| v != first) {
            return Self::Mixed;
        }
        if first.is_empty() {
            Self::Empty
        } else {
            Self::Same(first.to_vec())
        }
    }
}

/// One row of the metadata table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRow {
    /// The Picard key.
    pub key: String,
    /// The value on disk.
    pub original: FieldValue,
    /// The value being built.
    pub new: FieldValue,
    /// At least one selected file's new value differs from its original.
    pub changed: bool,
    /// A value contains a line break; a one-line box can't edit it without
    /// flattening it, so the view shows it read-only.
    pub multiline: bool,
    /// The most values any selected file holds (see [`parse_values`]).
    pub max_values: usize,
}

/// The sort position of `key` in the table: well-known fields in
/// [`KNOWN_KEYS`] order, then the rest alphabetically.
fn key_order(key: &str) -> (usize, &str) {
    (
        KNOWN_KEYS
            .iter()
            .position(|k| *k == key)
            .unwrap_or(KNOWN_KEYS.len()),
        key,
    )
}

/// The table for `files` (the selection): one row per key any of them has,
/// on disk or edited, in display order. Read-only files take part in the
/// table (their values show) but are never changed by edits.
#[must_use]
pub fn field_rows(files: &[&TagFile]) -> Vec<FieldRow> {
    let mut all_keys: BTreeSet<&str> = BTreeSet::new();
    for file in files {
        all_keys.extend(file.original.iter().map(|(k, _)| k));
        all_keys.extend(file.edited.iter().map(|(k, _)| k));
    }
    let mut ordered: Vec<&str> = all_keys.into_iter().collect();
    ordered.sort_by_key(|k| key_order(k));

    ordered
        .into_iter()
        .map(|key| FieldRow {
            key: key.to_owned(),
            original: FieldValue::combine(files.iter().map(|f| f.original.get_all(key))),
            new: FieldValue::combine(files.iter().map(|f| f.edited.get_all(key))),
            changed: files
                .iter()
                .any(|f| f.edited.get_all(key) != f.original.get_all(key)),
            multiline: files
                .iter()
                .flat_map(|f| f.edited.get_all(key))
                .any(|v| v.contains('\n')),
            max_values: files
                .iter()
                .map(|f| f.edited.get_all(key).len())
                .max()
                .unwrap_or(0),
        })
        .collect()
}

/// Sets `key` on `edited` from the text typed into its box.
pub fn apply_text(edited: &mut TagSet, key: &str, text: &str, current_values: usize) {
    edited.set(key, parse_values(key, text, current_values));
}

/// Puts `key` back to what is on disk.
pub fn revert_field(edited: &mut TagSet, original: &TagSet, key: &str) {
    edited.set(key, original.get_all(key).iter().cloned());
}

// --- Pictures ---------------------------------------------------------------

/// The picture types offered when adding an image, front cover first.
pub const PICTURE_KINDS: [u8; 6] = [
    picture_type::FRONT_COVER,
    picture_type::BACK_COVER,
    picture_type::LEAFLET,
    picture_type::MEDIA,
    picture_type::ARTIST,
    picture_type::OTHER,
];

/// A content hash identifying a picture across files and edits (SipHash
/// of the image bytes).
#[must_use]
pub fn picture_hash(picture: &EditPicture) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    picture.data.hash(&mut hasher);
    hasher.finish()
}

/// One distinct picture of the selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PictureRow {
    /// [`picture_hash`] of the image.
    pub hash: u64,
    /// Picture type (see [`picture_type`]).
    pub kind: u8,
    pub mime: String,
    pub description: String,
    pub bytes: usize,
    /// How many selected files carry it …
    pub files: usize,
    /// … out of this many selected files.
    pub of: usize,
    /// Some file has it now that did not have it on disk.
    pub changed: bool,
}

/// The distinct pictures of the selection's *edited* tags: front cover
/// first, then by type.
#[must_use]
pub fn picture_rows(files: &[&TagFile]) -> Vec<PictureRow> {
    let mut rows: Vec<PictureRow> = Vec::new();
    for file in files {
        // Hashing is only needed for files whose pictures were touched.
        let touched = file.edited.pictures != file.original.pictures;
        let on_disk: Vec<(u64, u8)> = if touched {
            file.original
                .pictures
                .iter()
                .map(|p| (picture_hash(p), p.kind))
                .collect()
        } else {
            Vec::new()
        };
        for picture in &file.edited.pictures {
            let hash = picture_hash(picture);
            let new_here = touched && !on_disk.contains(&(hash, picture.kind));
            match rows
                .iter_mut()
                .find(|r| r.hash == hash && r.kind == picture.kind)
            {
                Some(row) => {
                    row.files += 1;
                    row.changed |= new_here;
                }
                None => rows.push(PictureRow {
                    hash,
                    kind: picture.kind,
                    mime: picture.mime.clone(),
                    description: picture.description.clone(),
                    bytes: picture.data.len(),
                    files: 1,
                    of: files.len(),
                    changed: new_here,
                }),
            }
        }
    }
    rows.sort_by_key(|r| (r.kind != picture_type::FRONT_COVER, r.kind, r.hash));
    rows
}

/// Wraps image bytes as a picture of `kind`; rejects anything that isn't a
/// recognisable image.
pub fn picture_from_bytes(data: Vec<u8>, kind: u8) -> Result<EditPicture, String> {
    let mime = crate::library::tags::infer_mime(&data);
    if !mime.starts_with("image/") {
        return Err("The file is not a supported image".to_owned());
    }
    Ok(EditPicture {
        kind,
        mime: mime.to_owned(),
        description: String::new(),
        data,
    })
}

/// Adds `picture`. A front or back cover replaces the existing one of the
/// same type; other types are appended (an identical image of the same type
/// is not added twice).
pub fn add_picture(tags: &mut TagSet, picture: EditPicture) {
    if matches!(
        picture.kind,
        picture_type::FRONT_COVER | picture_type::BACK_COVER
    ) {
        tags.pictures.retain(|p| p.kind != picture.kind);
    } else if tags
        .pictures
        .iter()
        .any(|p| p.kind == picture.kind && p.data == picture.data)
    {
        return;
    }
    tags.pictures.push(picture);
}

/// Removes the picture identified by `hash` and `kind`; whether one was.
pub fn remove_picture(tags: &mut TagSet, hash: u64, kind: u8) -> bool {
    let before = tags.pictures.len();
    tags.pictures
        .retain(|p| !(p.kind == kind && picture_hash(p) == hash));
    tags.pictures.len() != before
}

/// Changes the type of the picture identified by `hash` and `kind`; whether
/// one changed.
pub fn set_picture_kind(tags: &mut TagSet, hash: u64, kind: u8, new_kind: u8) -> bool {
    let mut changed = false;
    for p in &mut tags.pictures {
        if p.kind == kind && picture_hash(p) == hash {
            p.kind = new_kind;
            changed = true;
        }
    }
    changed
}

/// Makes `picture` the front cover, keeping every other picture.
pub fn set_front_cover(tags: &mut TagSet, picture: &EditPicture) {
    let mut front = picture.clone();
    front.kind = picture_type::FRONT_COVER;
    add_picture(tags, front);
    // Keep the front cover first, as players that read one picture expect.
    tags.pictures
        .sort_by_key(|p| p.kind != picture_type::FRONT_COVER);
}

#[cfg(test)]
mod tests {
    use super::super::file::testing::*;
    use super::*;

    fn pic(kind: u8, byte: u8) -> EditPicture {
        EditPicture {
            kind,
            mime: "image/png".into(),
            description: String::new(),
            data: vec![0x89, b'P', b'N', b'G', byte],
        }
    }

    #[test]
    fn splits_multi_valued_fields_on_semicolons() {
        assert_eq!(
            parse_values(keys::GENRE, "Rock;  Electronic ;;Pop", 1),
            ["Rock", "Electronic", "Pop"]
        );
        assert_eq!(parse_values(keys::GENRE, "  ", 1), Vec::<String>::new());
        assert_eq!(join_values(&["a".into(), "b".into()]), "a; b");
    }

    #[test]
    fn single_valued_fields_keep_their_semicolons() {
        assert_eq!(
            parse_values(keys::TITLE, " Hello; World ", 1),
            ["Hello; World"]
        );
        // A field that already holds a list stays a list.
        assert_eq!(parse_values(keys::TITLE, "a; b", 2), ["a", "b"]);
    }

    #[test]
    fn rows_are_the_union_of_keys_in_display_order() {
        let a = file(1, &[(keys::ARTIST, "X"), (keys::TITLE, "T")]);
        let mut b = file(2, &[(keys::ALBUM, "A")]);
        b.edited.set_one("x-custom", "1");
        let rows = field_rows(&[&a, &b]);
        let order: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(order, ["title", "artist", "album", "x-custom"]);
    }

    #[test]
    fn differing_values_show_as_mixed_and_equal_ones_as_themselves() {
        let a = file(1, &[(keys::ALBUM, "Same"), (keys::TITLE, "One")]);
        let b = file(2, &[(keys::ALBUM, "Same"), (keys::TITLE, "Two")]);
        let rows = field_rows(&[&a, &b]);
        let row = |k: &str| rows.iter().find(|r| r.key == k).unwrap();
        assert_eq!(row("album").new, FieldValue::Same(vec!["Same".into()]));
        assert!(row("title").new.is_mixed());
        assert!(row("title").original.is_mixed());
    }

    #[test]
    fn a_field_only_some_files_have_is_mixed() {
        let a = file(1, &[(keys::GENRE, "Rock")]);
        let b = file(2, &[]);
        let rows = field_rows(&[&a, &b]);
        assert!(rows[0].new.is_mixed());
        // …and a value that is the same absence everywhere is not shown.
        assert!(field_rows(&[&b]).is_empty());
    }

    #[test]
    fn changed_flags_edits_against_disk() {
        let mut a = file(1, &[(keys::TITLE, "Old")]);
        a.edited.set_one(keys::TITLE, "New");
        a.edited.set_one(keys::GENRE, "Jazz");
        let rows = field_rows(&[&a]);
        let row = |k: &str| rows.iter().find(|r| r.key == k).unwrap();
        assert!(row("title").changed);
        assert!(row("genre").changed);
        assert_eq!(row("genre").original, FieldValue::Empty);
        assert_eq!(row("title").original.display(), "Old");
        assert_eq!(row("title").new.display(), "New");
    }

    #[test]
    fn applying_text_sets_it_and_empty_text_removes_the_field() {
        let mut tags = TagSet::new();
        apply_text(&mut tags, keys::GENRE, "Rock; Pop", 0);
        assert_eq!(tags.get_all(keys::GENRE), ["Rock", "Pop"]);
        apply_text(&mut tags, keys::GENRE, "", 2);
        assert!(tags.get(keys::GENRE).is_none());
    }

    #[test]
    fn reverting_a_field_restores_or_removes_it() {
        let original = {
            let mut t = TagSet::new();
            t.set_one(keys::TITLE, "Old");
            t
        };
        let mut edited = original.clone();
        edited.set_one(keys::TITLE, "New");
        edited.set_one(keys::GENRE, "Jazz");
        revert_field(&mut edited, &original, keys::TITLE);
        revert_field(&mut edited, &original, keys::GENRE);
        assert_eq!(edited, original);
    }

    #[test]
    fn key_names_are_normalised_and_validated() {
        assert_eq!(normalize_key("  MyField "), Some("myfield".into()));
        assert_eq!(
            normalize_key("performer:guitar"),
            Some("performer:guitar".into())
        );
        assert_eq!(normalize_key(""), None);
        assert_eq!(normalize_key("a=b"), None);
        assert_eq!(normalize_key("a b"), None);
        assert_eq!(normalize_key("é"), None);
    }

    #[test]
    fn front_and_back_covers_replace_their_own_kind() {
        let mut tags = TagSet::new();
        add_picture(&mut tags, pic(picture_type::FRONT_COVER, 1));
        add_picture(&mut tags, pic(picture_type::BACK_COVER, 2));
        add_picture(&mut tags, pic(picture_type::FRONT_COVER, 3));
        assert_eq!(tags.pictures.len(), 2);
        assert!(tags.pictures.iter().any(|p| p.data.last() == Some(&3)));
        assert!(!tags.pictures.iter().any(|p| p.data.last() == Some(&1)));
        // Other kinds accumulate but never duplicate.
        add_picture(&mut tags, pic(picture_type::LEAFLET, 4));
        add_picture(&mut tags, pic(picture_type::LEAFLET, 5));
        add_picture(&mut tags, pic(picture_type::LEAFLET, 5));
        assert_eq!(tags.pictures.len(), 4);
    }

    #[test]
    fn pictures_are_removed_and_retyped_by_content() {
        let mut tags = TagSet::new();
        let p = pic(picture_type::FRONT_COVER, 1);
        let hash = picture_hash(&p);
        tags.pictures.push(p);
        assert!(set_picture_kind(
            &mut tags,
            hash,
            picture_type::FRONT_COVER,
            picture_type::BACK_COVER
        ));
        assert_eq!(tags.pictures[0].kind, picture_type::BACK_COVER);
        assert!(!remove_picture(&mut tags, hash, picture_type::FRONT_COVER));
        assert!(remove_picture(&mut tags, hash, picture_type::BACK_COVER));
        assert!(tags.pictures.is_empty());
    }

    #[test]
    fn picture_rows_merge_identical_images_and_count_files() {
        let cover = pic(picture_type::FRONT_COVER, 7);
        let mut a = file(1, &[]);
        let mut b = file(2, &[]);
        a.original.pictures.push(cover.clone());
        a.edited = a.original.clone();
        b.original.pictures.push(cover);
        b.edited = b.original.clone();
        // File b gets a new back cover.
        b.edited.pictures.push(pic(picture_type::BACK_COVER, 9));
        let rows = picture_rows(&[&a, &b]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, picture_type::FRONT_COVER);
        assert_eq!((rows[0].files, rows[0].of), (2, 2));
        assert!(!rows[0].changed);
        assert_eq!((rows[1].files, rows[1].of), (1, 2));
        assert!(rows[1].changed);
    }

    #[test]
    fn set_front_cover_keeps_other_pictures_and_puts_the_front_first() {
        let mut tags = TagSet::new();
        tags.pictures.push(pic(picture_type::BACK_COVER, 1));
        set_front_cover(&mut tags, &pic(picture_type::OTHER, 2));
        assert_eq!(tags.pictures.len(), 2);
        assert_eq!(tags.pictures[0].kind, picture_type::FRONT_COVER);
    }

    #[test]
    fn non_images_are_rejected() {
        assert!(picture_from_bytes(b"not an image".to_vec(), picture_type::FRONT_COVER).is_err());
        let png = b"\x89PNG\r\n\x1a\nxxxx".to_vec();
        let p = picture_from_bytes(png, picture_type::FRONT_COVER).unwrap();
        assert_eq!(p.mime, "image/png");
    }
}
