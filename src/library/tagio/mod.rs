// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Tag reading and writing for the built-in tag editor: the Picard layer.
//!
//! The editor works on a [`TagSet`] of MusicBrainz Picard's tag names. Both
//! directions go through the Symphonia fork: [`read`] probes the file with
//! the same readers the player uses, and [`write`] hands the tags to the
//! fork's `symphonia-tagging` crate, whose serializers share the readers'
//! field tables and keep native data the model doesn't represent. The only
//! thing Aulos owns is the Picard-key to `StandardTag` table in `map.rs`,
//! used for both directions.

mod map;
pub mod model;

mod read;
mod write;

pub mod base64;

#[cfg(test)]
mod roundtrip_tests;

pub use model::{EditPicture, TagSet, keys, picture_type};
pub use symphonia_tagging::Container;

use std::io;
use std::path::Path;

/// Everything [`read`] returns.
#[derive(Debug, Clone)]
pub struct FileTags {
    pub container: Container,
    pub tags: TagSet,
}

#[derive(Debug, thiserror::Error)]
pub enum TagError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("unsupported file: {0}")]
    Unsupported(String),
    #[error("malformed file: {0}")]
    Malformed(String),
}

impl From<symphonia_tagging::Error> for TagError {
    fn from(e: symphonia_tagging::Error) -> Self {
        match e {
            symphonia_tagging::Error::Io(e) => TagError::Io(e),
            symphonia_tagging::Error::Unsupported(s) => TagError::Unsupported(s),
            symphonia_tagging::Error::Malformed(s) => TagError::Malformed(s),
        }
    }
}

pub type Result<T, E = TagError> = std::result::Result<T, E>;

/// Reads `path`'s tags, pictures included.
pub fn read(path: &Path) -> Result<FileTags> {
    let container = sniff(path)?;
    read::read(path, container)
}

/// Replaces `path`'s tags with exactly `tags`: every field not in `tags` is
/// removed (except native data the model doesn't represent), and
/// `tags.pictures` replaces all embedded pictures.
pub fn write(path: &Path, tags: &TagSet) -> Result<()> {
    write::write(path, tags)
}

/// Identifies `path`'s container from its leading bytes (never from the
/// extension).
pub fn sniff(path: &Path) -> Result<Container> {
    Ok(symphonia_tagging::sniff(path)?)
}
