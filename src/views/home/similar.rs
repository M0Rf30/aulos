// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! "Because you listened to …" shelves: similar artists of the user's top
//! artists (see `crate::online::similar`). Artists present in the library
//! get regular artist cards (avatar, click → artist page, play button);
//! the rest are quieter "discover" cards that open their Last.fm /
//! MusicBrainz page in the browser.

use super::{
    CARD_ART, CARD_LABEL_HEIGHT, CARD_PADDING, HomeMessage, dim_text, secondary_caption, shelf,
    with_play_overlay,
};
use crate::fl;
use crate::online::similar::{DiscoverArtist, SimilarShelf};
use crate::views::{card_button_class, common};
use cosmic::widget;
use std::collections::HashMap;

/// Card of a similar artist that is in the library.
fn library_card<'a>(
    name: &'a str,
    photos: &'a HashMap<String, widget::image::Handle>,
) -> cosmic::Element<'a, HomeMessage> {
    let avatar = common::artist_avatar(name, photos.get(name), CARD_ART);
    let art = with_play_overlay(
        avatar,
        CARD_ART,
        HomeMessage::PlayArtist(name.to_string()),
        fl!("home-play-artist"),
    );
    let label = common::grid_card_label(
        CARD_ART,
        CARD_LABEL_HEIGHT,
        common::clipped_cell(
            common::cell_text(name)
                .font(cosmic::font::semibold())
                .into(),
        ),
        common::clipped_cell(secondary_caption(fl!("home-in-library")).into()),
    );
    widget::button::custom(common::grid_card(art, CARD_ART, label))
        .on_press(HomeMessage::OpenArtist(name.to_string()))
        .padding(CARD_PADDING as u16)
        .class(card_button_class())
        .into()
}

/// Quieter card of an artist that is not in the library: initials avatar,
/// dimmed name, opens the artist's web page.
fn discover_card<'a>(artist: &'a DiscoverArtist) -> cosmic::Element<'a, HomeMessage> {
    let avatar = common::artist_avatar(&artist.name, None, CARD_ART);
    let label = common::grid_card_label(
        CARD_ART,
        CARD_LABEL_HEIGHT,
        common::clipped_cell(
            common::cell_text(artist.name.as_str())
                .class(dim_text())
                .into(),
        ),
        common::clipped_cell(secondary_caption(fl!("home-discover-artist")).into()),
    );
    let tooltip = fl!(
        "home-discover-open",
        artist = artist.name.clone(),
        site = artist.site
    );
    widget::tooltip(
        widget::button::custom(common::grid_card(avatar, CARD_ART, label))
            .on_press(HomeMessage::OpenUrl(artist.url.clone()))
            .padding(CARD_PADDING as u16)
            .class(card_button_class()),
        widget::text::caption(tooltip),
        widget::tooltip::Position::Top,
    )
    .into()
}

/// One shelf per seed artist, library artists before discover ones.
pub(super) fn similar_shelves<'a>(
    shelves: &'a [SimilarShelf],
    photos: &'a HashMap<String, widget::image::Handle>,
) -> Vec<cosmic::Element<'a, HomeMessage>> {
    shelves
        .iter()
        .filter(|s| !s.is_empty())
        .map(|s| {
            let cards = s
                .in_library
                .iter()
                .map(|name| library_card(name, photos))
                .chain(s.discover.iter().map(discover_card))
                .collect();
            shelf(
                fl!("home-because-you-listened", artist = s.seed.clone()),
                Some(fl!("home-because-hint")),
                None,
                cards,
            )
        })
        .collect()
}
