// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Which items a track / album context menu shows, in which order, and where
//! the divider goes — as plain data, with no widget types.
//!
//! The host view hands over typed entries ([`TrackMenuAction`] /
//! [`AlbumMenuAction`], each carrying the message it emits); the `*_menu_specs`
//! functions order them, drop empty ones and produce [`MenuSpec`] nodes that
//! any UI toolkit can render. Icon names are freedesktop symbolic names.
//!
//! Translated static labels are resolved once per process (the UI language
//! is fixed at startup) and handed out as `&'static str`, so a row only
//! allocates for what is really per-row — its messages and playlist names.

use crate::fl;
use std::borrow::Cow;
use std::sync::LazyLock;

/// Translated labels that never vary per row.
struct Labels {
    play: String,
    play_album: String,
    play_next: String,
    add_to_queue: String,
    add_to_playlist: String,
    go_to_album: String,
    go_to_artist: String,
    properties: String,
    edit_tags: String,
    edit_album_tags: String,
    lookup_musicbrainz: String,
    scan_acoustid: String,
}

/// The UI language is chosen once in `main` before any view is built (see
/// `i18n::init`), so these never go stale. A `static` borrow is `'static`,
/// which is what the menu elements require.
static LABELS: LazyLock<Labels> = LazyLock::new(|| Labels {
    play: fl!("play"),
    play_album: fl!("play-album"),
    play_next: fl!("queue-play-next"),
    add_to_queue: fl!("queue-add"),
    add_to_playlist: fl!("menu-add-to-playlist"),
    go_to_album: fl!("menu-go-to-album"),
    go_to_artist: fl!("menu-go-to-artist"),
    properties: fl!("properties"),
    edit_tags: fl!("menu-edit-tags"),
    edit_album_tags: fl!("menu-edit-album-tags"),
    lookup_musicbrainz: fl!("menu-lookup-musicbrainz"),
    scan_acoustid: fl!("menu-scan-acoustid"),
});

/// The translated "Properties" caption, shared with the quality-cell button's
/// tooltip so rows don't format it again.
#[must_use]
pub fn properties_label() -> &'static str {
    &LABELS.properties
}

fn label(text: &'static str) -> Cow<'static, str> {
    Cow::Borrowed(text)
}

/// Renderer-independent description of one menu node.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuSpec<M> {
    /// A clickable entry that emits `message`.
    Item {
        label: Cow<'static, str>,
        icon: Option<&'static str>,
        message: M,
    },
    /// An entry that opens a submenu.
    Folder {
        label: Cow<'static, str>,
        icon: Option<&'static str>,
        children: Vec<MenuSpec<M>>,
    },
    /// A thin separator line. Never first or last.
    Divider,
}

/// What a menu entry contributes, how it is ordered and which section it
/// belongs to. Implemented by the typed action enums below.
trait MenuEntry<M> {
    /// Sort key: entries are emitted in ascending order regardless of the
    /// order the host view listed them in.
    fn rank(&self) -> u8;
    /// Footer entries come after a divider (when anything precedes them).
    fn is_footer(&self) -> bool;
    /// The node to show, or `None` when the entry has nothing to offer (an
    /// empty playlist submenu).
    fn into_spec(self) -> Option<MenuSpec<M>>;
}

/// Order `entries`, drop the empty ones and put a single divider between the
/// main section and the footer.
fn assemble<M, E: MenuEntry<M>>(mut entries: Vec<E>) -> Vec<MenuSpec<M>> {
    entries.sort_by_key(|entry| entry.rank());
    let mut specs = Vec::with_capacity(entries.len() + 1);
    let mut footer_started = false;
    for entry in entries {
        let footer = entry.is_footer();
        let Some(spec) = entry.into_spec() else {
            continue;
        };
        if footer && !footer_started {
            footer_started = true;
            if !specs.is_empty() {
                specs.push(MenuSpec::Divider);
            }
        }
        specs.push(spec);
    }
    specs
}

// --- Track menu -------------------------------------------------------------

/// One entry of a track row's context menu; the payload is the message the
/// entry emits. Only entries the host view actually wired are passed in.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackMenuAction<M> {
    /// Play the track (the row's own press action).
    Play(M),
    /// Insert right after the playing track.
    PlayNext(M),
    /// Append to the queue.
    AddToQueue(M),
    /// "Add to playlist" submenu: one `(playlist name, message)` per playlist.
    /// Omitted when empty.
    AddToPlaylist(Vec<(String, M)>),
    /// Open the track's album page.
    GoToAlbum(M),
    /// Open the track's artist page.
    GoToArtist(M),
    /// Open the tag editor on the track ("Edit tags…").
    EditTags(M),
    /// Open the tag editor and search MusicBrainz for the track.
    LookUpMusicBrainz(M),
    /// Open the tag editor and identify the track with AcoustID.
    ScanAcoustId(M),
    /// Open the Properties drawer.
    Properties(M),
}

impl<M> MenuEntry<M> for TrackMenuAction<M> {
    fn rank(&self) -> u8 {
        match self {
            Self::Play(_) => 0,
            Self::PlayNext(_) => 1,
            Self::AddToQueue(_) => 2,
            Self::AddToPlaylist(_) => 3,
            Self::GoToAlbum(_) => 4,
            Self::GoToArtist(_) => 5,
            Self::EditTags(_) => 6,
            Self::LookUpMusicBrainz(_) => 7,
            Self::ScanAcoustId(_) => 8,
            Self::Properties(_) => 9,
        }
    }

    fn is_footer(&self) -> bool {
        matches!(
            self,
            Self::EditTags(_)
                | Self::LookUpMusicBrainz(_)
                | Self::ScanAcoustId(_)
                | Self::Properties(_)
        )
    }

    fn into_spec(self) -> Option<MenuSpec<M>> {
        let item = |text: &'static String, icon, message| {
            Some(MenuSpec::Item {
                label: label(text),
                icon: Some(icon),
                message,
            })
        };
        match self {
            Self::Play(m) => item(&LABELS.play, "media-playback-start-symbolic", m),
            Self::PlayNext(m) => item(&LABELS.play_next, "go-next-symbolic", m),
            Self::AddToQueue(m) => item(&LABELS.add_to_queue, "view-list-ordered-symbolic", m),
            Self::AddToPlaylist(playlists) if playlists.is_empty() => None,
            Self::AddToPlaylist(playlists) => Some(MenuSpec::Folder {
                label: label(&LABELS.add_to_playlist),
                icon: Some("list-add-symbolic"),
                children: playlists
                    .into_iter()
                    .map(|(name, message)| MenuSpec::Item {
                        label: Cow::Owned(name),
                        icon: None,
                        message,
                    })
                    .collect(),
            }),
            Self::GoToAlbum(m) => item(&LABELS.go_to_album, "media-optical-symbolic", m),
            Self::GoToArtist(m) => item(&LABELS.go_to_artist, "system-users-symbolic", m),
            Self::EditTags(m) => item(&LABELS.edit_tags, "document-edit-symbolic", m),
            Self::LookUpMusicBrainz(m) => {
                item(&LABELS.lookup_musicbrainz, "system-search-symbolic", m)
            }
            Self::ScanAcoustId(m) => item(&LABELS.scan_acoustid, "audio-x-generic-symbolic", m),
            Self::Properties(m) => item(&LABELS.properties, "dialog-information-symbolic", m),
        }
    }
}

/// Which items a track row's context menu shows and in which order, as
/// widget-free data.
///
/// **Extension point:** to add an entry, add a [`TrackMenuAction`] variant,
/// give it a `rank`/`is_footer` and a `MenuSpec` in `MenuEntry::into_spec`,
/// and have `RowMenu::entries` (in `track_row`) pass it in.
/// [`track_menu`] renders whatever this returns.
#[must_use]
pub fn track_menu_specs<M>(entries: Vec<TrackMenuAction<M>>) -> Vec<MenuSpec<M>> {
    assemble(entries)
}

// --- Album menu -------------------------------------------------------------

/// One entry of an album card / hero context menu.
#[derive(Debug, Clone, PartialEq)]
pub enum AlbumMenuAction<M> {
    /// Play the whole album.
    Play(M),
    /// Insert the album right after the playing track.
    PlayNext(M),
    /// Append the album to the queue.
    AddToQueue(M),
    /// Open the tag editor on the whole album ("Edit album tags…").
    EditTags(M),
    /// Open the tag editor and search MusicBrainz for the album.
    LookUpMusicBrainz(M),
    /// Open the tag editor and identify the album's tracks with AcoustID.
    ScanAcoustId(M),
    /// Open the album Properties drawer.
    Properties(M),
}

impl<M> MenuEntry<M> for AlbumMenuAction<M> {
    fn rank(&self) -> u8 {
        match self {
            Self::Play(_) => 0,
            Self::PlayNext(_) => 1,
            Self::AddToQueue(_) => 2,
            Self::EditTags(_) => 3,
            Self::LookUpMusicBrainz(_) => 4,
            Self::ScanAcoustId(_) => 5,
            Self::Properties(_) => 6,
        }
    }

    fn is_footer(&self) -> bool {
        matches!(
            self,
            Self::EditTags(_)
                | Self::LookUpMusicBrainz(_)
                | Self::ScanAcoustId(_)
                | Self::Properties(_)
        )
    }

    fn into_spec(self) -> Option<MenuSpec<M>> {
        let item = |text: &'static String, icon, message| {
            Some(MenuSpec::Item {
                label: label(text),
                icon: Some(icon),
                message,
            })
        };
        match self {
            Self::Play(m) => item(&LABELS.play_album, "media-playback-start-symbolic", m),
            Self::PlayNext(m) => item(&LABELS.play_next, "go-next-symbolic", m),
            Self::AddToQueue(m) => item(&LABELS.add_to_queue, "view-list-ordered-symbolic", m),
            Self::EditTags(m) => item(&LABELS.edit_album_tags, "document-edit-symbolic", m),
            Self::LookUpMusicBrainz(m) => {
                item(&LABELS.lookup_musicbrainz, "system-search-symbolic", m)
            }
            Self::ScanAcoustId(m) => item(&LABELS.scan_acoustid, "audio-x-generic-symbolic", m),
            Self::Properties(m) => item(&LABELS.properties, "dialog-information-symbolic", m),
        }
    }
}

/// Which items an album context menu shows, as widget-free data.
#[must_use]
pub fn album_menu_specs<M>(entries: Vec<AlbumMenuAction<M>>) -> Vec<MenuSpec<M>> {
    assemble(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compact shape of a spec list: item messages, `[..]` for submenus,
    /// `-` for dividers.
    fn shape(specs: &[MenuSpec<u32>]) -> String {
        specs
            .iter()
            .map(|spec| match spec {
                MenuSpec::Item { message, .. } => message.to_string(),
                MenuSpec::Folder { children, .. } => format!("[{}]", shape(children)),
                MenuSpec::Divider => "-".to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn track_menu_orders_entries_and_divides_before_properties() {
        // Listed out of order on purpose: the menu order is fixed.
        let specs = track_menu_specs(vec![
            TrackMenuAction::Properties(7),
            TrackMenuAction::GoToArtist(6),
            TrackMenuAction::AddToPlaylist(vec![("A".into(), 40), ("B".into(), 41)]),
            TrackMenuAction::Play(1),
            TrackMenuAction::GoToAlbum(5),
            TrackMenuAction::AddToQueue(3),
            TrackMenuAction::PlayNext(2),
        ]);
        assert_eq!(shape(&specs), "1 2 3 [40 41] 5 6 - 7");
    }

    #[test]
    fn track_menu_only_shows_wired_entries() {
        let specs = track_menu_specs(vec![
            TrackMenuAction::Play(1),
            TrackMenuAction::Properties(7),
        ]);
        assert_eq!(shape(&specs), "1 - 7");

        let specs = track_menu_specs(vec![
            TrackMenuAction::Play(1),
            TrackMenuAction::PlayNext(2),
            TrackMenuAction::AddToQueue(3),
        ]);
        assert_eq!(shape(&specs), "1 2 3");
    }

    #[test]
    fn track_menu_omits_empty_playlist_submenu() {
        let specs = track_menu_specs(vec![
            TrackMenuAction::Play(1),
            TrackMenuAction::AddToPlaylist(Vec::new()),
            TrackMenuAction::Properties(7),
        ]);
        assert_eq!(shape(&specs), "1 - 7");
    }

    #[test]
    fn track_menu_playlist_submenu_has_one_entry_per_playlist() {
        let specs = track_menu_specs(vec![TrackMenuAction::AddToPlaylist(vec![
            ("Rock".into(), 10),
            ("Jazz".into(), 11),
            ("Mix".into(), 12),
        ])]);
        let [MenuSpec::Folder { children, .. }] = specs.as_slice() else {
            panic!("expected a single folder, got {specs:?}");
        };
        let names: Vec<_> = children
            .iter()
            .filter_map(|c| match c {
                MenuSpec::Item { label, .. } => Some(&**label),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["Rock", "Jazz", "Mix"]);
    }

    #[test]
    fn lone_properties_entry_has_no_divider() {
        let specs = track_menu_specs(vec![TrackMenuAction::Properties(7)]);
        assert_eq!(shape(&specs), "7");
    }

    #[test]
    fn empty_entry_list_gives_empty_menu() {
        assert!(track_menu_specs::<u32>(Vec::new()).is_empty());
        assert!(album_menu_specs::<u32>(Vec::new()).is_empty());
    }

    #[test]
    fn album_menu_has_queue_actions_then_divider_then_properties() {
        let specs = album_menu_specs(vec![
            AlbumMenuAction::Properties(4),
            AlbumMenuAction::AddToQueue(3),
            AlbumMenuAction::PlayNext(2),
            AlbumMenuAction::Play(1),
        ]);
        assert_eq!(shape(&specs), "1 2 3 - 4");
    }

    /// `(label, icon, message)` of every `Item` in a flat spec list.
    fn items(specs: &[MenuSpec<u32>]) -> Vec<(String, Option<&'static str>, u32)> {
        specs
            .iter()
            .filter_map(|spec| match spec {
                MenuSpec::Item {
                    label,
                    icon,
                    message,
                } => Some((label.to_string(), *icon, *message)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn track_tag_entries_sit_in_the_footer_before_properties() {
        // Listed out of order on purpose.
        let specs = track_menu_specs(vec![
            TrackMenuAction::Properties(7),
            TrackMenuAction::ScanAcoustId(10),
            TrackMenuAction::GoToArtist(6),
            TrackMenuAction::EditTags(8),
            TrackMenuAction::Play(1),
            TrackMenuAction::LookUpMusicBrainz(9),
        ]);
        // One divider, then Edit tags…, Look up, Scan, Properties.
        assert_eq!(shape(&specs), "1 6 - 8 9 10 7");
    }

    #[test]
    fn track_menu_without_tag_entries_is_unchanged() {
        // What a non-local track (or a view that never wired tagging) gets.
        let specs = track_menu_specs(vec![
            TrackMenuAction::Play(1),
            TrackMenuAction::GoToAlbum(5),
            TrackMenuAction::Properties(7),
        ]);
        assert_eq!(shape(&specs), "1 5 - 7");
    }

    #[test]
    fn tag_entries_alone_get_no_leading_divider() {
        let specs = track_menu_specs(vec![
            TrackMenuAction::ScanAcoustId(10),
            TrackMenuAction::EditTags(8),
        ]);
        assert_eq!(shape(&specs), "8 10");
    }

    #[test]
    fn track_tag_entries_have_their_labels_and_icons() {
        let specs = track_menu_specs(vec![
            TrackMenuAction::EditTags(8),
            TrackMenuAction::LookUpMusicBrainz(9),
            TrackMenuAction::ScanAcoustId(10),
        ]);
        let items = items(&specs);
        assert_eq!(
            items,
            [
                ("Edit tags…".to_string(), Some("document-edit-symbolic"), 8),
                (
                    "Look up on MusicBrainz".to_string(),
                    Some("system-search-symbolic"),
                    9
                ),
                (
                    "Scan with AcoustID".to_string(),
                    Some("audio-x-generic-symbolic"),
                    10
                ),
            ]
        );
    }

    #[test]
    fn album_tag_entries_sit_in_the_footer_before_properties() {
        let specs = album_menu_specs(vec![
            AlbumMenuAction::Properties(7),
            AlbumMenuAction::ScanAcoustId(10),
            AlbumMenuAction::AddToQueue(3),
            AlbumMenuAction::EditTags(8),
            AlbumMenuAction::Play(1),
            AlbumMenuAction::LookUpMusicBrainz(9),
            AlbumMenuAction::PlayNext(2),
        ]);
        assert_eq!(shape(&specs), "1 2 3 - 8 9 10 7");
    }

    #[test]
    fn album_edit_entry_is_labelled_for_the_whole_album() {
        let specs = album_menu_specs(vec![AlbumMenuAction::EditTags(8)]);
        assert_eq!(items(&specs)[0].0, "Edit album tags…");
    }
}
