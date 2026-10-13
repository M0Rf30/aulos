// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! [`TagSet`] to the fork's `Tag`/`Visual` model, then
//! `symphonia_tagging::write_file`.

use super::model::{EditPicture, TagSet, join_position, split_position};
use super::{Result, keys, map};
use std::path::Path;
use symphonia::core::meta::{RawTag, StandardTag, Tag, Visual};
use symphonia_tagging::WriteOptions;

pub(super) fn write(path: &Path, tags: &TagSet) -> Result<()> {
    let visuals: Vec<Visual> = tags.pictures.iter().map(visual).collect();
    symphonia_tagging::write_file(path, &to_tags(tags), &visuals, &WriteOptions::default())?;
    Ok(())
}

/// One `Tag` per value. Fields with a standard tag carry it (the fork's
/// writers pick the native field from it); everything else, and values that
/// don't fit their standard tag's type, are free-form under the Picard key.
fn to_tags(set: &TagSet) -> Vec<Tag> {
    let mut out = Vec::new();
    for (key, value) in expand(set) {
        let std = map::std_for(&key, &value);
        out.push(Tag {
            raw: RawTag::new(key, value),
            std,
        });
    }
    out
}

/// `(key, value)` pairs, with `"3/12"` style track/disc numbers split: the
/// total joins the total field unless that is set explicitly.
fn expand(set: &TagSet) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (key, values) in set.iter() {
        let total_key = match key {
            keys::TRACKNUMBER => Some(keys::TOTALTRACKS),
            keys::DISCNUMBER => Some(keys::TOTALDISCS),
            _ => None,
        };
        for value in values {
            if let Some(total_key) = total_key
                && value.contains('/')
            {
                let (n, t) = split_position(value);
                let n = n.map(|n| n.to_string());
                let t = t.map(|t| t.to_string());
                let joined = join_position(n.as_deref(), None);
                if !joined.is_empty() {
                    out.push((key.to_owned(), joined));
                }
                if let Some(t) = t
                    && set.get(total_key).is_none()
                {
                    out.push((total_key.to_owned(), t));
                }
                continue;
            }
            out.push((key.to_owned(), value.clone()));
        }
    }
    out
}

fn visual(p: &EditPicture) -> Visual {
    let tags = if p.description.is_empty() {
        Vec::new()
    } else {
        vec![Tag::new_from_parts(
            "",
            "",
            Some(StandardTag::Description(std::sync::Arc::new(
                p.description.clone(),
            ))),
        )]
    };
    Visual {
        media_type: Some(p.mime.clone()).filter(|m| !m.is_empty()),
        dimensions: None,
        color_mode: None,
        usage: Some(map::usage_for_type(p.kind)),
        tags,
        data: p.data.clone().into_boxed_slice(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_and_free_form_tags() {
        let mut s = TagSet::new();
        s.set_one(keys::TITLE, "T");
        s.set(keys::GENRE, ["a".to_owned(), "b".to_owned()]);
        s.set_one(keys::ARTISTS, "A");
        s.set_one(keys::TRACKNUMBER, "x1");
        let tags = to_tags(&s);
        let title = tags.iter().find(|t| t.raw.key == keys::TITLE).unwrap();
        assert!(matches!(title.std, Some(StandardTag::TrackTitle(_))));
        assert_eq!(tags.iter().filter(|t| t.raw.key == keys::GENRE).count(), 2);
        assert!(
            tags.iter()
                .find(|t| t.raw.key == keys::ARTISTS)
                .unwrap()
                .std
                .is_none()
        );
        assert!(
            tags.iter()
                .find(|t| t.raw.key == keys::TRACKNUMBER)
                .unwrap()
                .std
                .is_none()
        );
    }

    #[test]
    fn positions_are_split() {
        let mut s = TagSet::new();
        s.set_one(keys::TRACKNUMBER, "3/12");
        s.set_one(keys::DISCNUMBER, "1/2");
        s.set_one(keys::TOTALDISCS, "5");
        let tags = to_tags(&s);
        let get = |k: &str| {
            tags.iter()
                .filter(|t| t.raw.key == k)
                .map(|t| t.raw.value.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(get(keys::TRACKNUMBER), ["3"]);
        assert_eq!(get(keys::TOTALTRACKS), ["12"]);
        assert_eq!(get(keys::DISCNUMBER), ["1"]);
        assert_eq!(get(keys::TOTALDISCS), ["5"]);
    }

    #[test]
    fn picture_becomes_a_typed_visual() {
        let v = visual(&EditPicture {
            kind: 4,
            mime: "image/png".into(),
            description: "back".into(),
            data: vec![1, 2, 3],
        });
        assert_eq!(
            v.usage,
            Some(symphonia::core::meta::StandardVisualKey::BackCover)
        );
        assert_eq!(v.media_type.as_deref(), Some("image/png"));
        assert_eq!(v.tags.len(), 1);
        assert_eq!(&*v.data, &[1, 2, 3]);
    }
}
