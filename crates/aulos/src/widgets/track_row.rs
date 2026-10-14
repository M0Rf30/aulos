//! The 36px track row shared by every track list (songs, album/artist/genre
//! detail, playlists, folders…) and its 32px column header.

use super::badge::quality_badge;
use super::text::{paint_icon, paint_text, paint_text_spaced};
use crate::app::App;
use crate::theme::{self, Theme};
use aulos_core::format::format_duration;
use aulos_core::library::Track;
use egui::{Align, CornerRadius, Rect, Response, Sense, Ui, pos2, vec2};
use egui_phosphor::{fill, regular};

pub const ROW_HEIGHT: f32 = 36.0;
pub const HEADER_HEIGHT: f32 = 32.0;
const NUM_W: f32 = 28.0;
const QUALITY_W: f32 = 54.0;
const HEART_W: f32 = 20.0;
const RATING_W: f32 = 72.0;
const TIME_W: f32 = 44.0;
const GAP: f32 = 12.0;
const PAD: f32 = 10.0;

/// Which columns a list shows. `number` is always the first (28px) column.
#[derive(Clone, Copy)]
pub struct Columns {
    pub artist: bool,
    pub album: bool,
    pub quality: bool,
    pub rating: bool,
}

impl Columns {
    /// The Songs layout: everything on wide windows; no artist/rating when narrow.
    pub fn songs(narrow: bool) -> Self {
        Self {
            artist: !narrow,
            album: true,
            quality: true,
            rating: !narrow,
        }
    }
    /// Track lists inside an album detail (title, quality, heart, rating, time).
    pub fn album_detail(narrow: bool) -> Self {
        Self {
            artist: false,
            album: false,
            quality: true,
            rating: !narrow,
        }
    }
}

struct Layout {
    num: Rect,
    title: Rect,
    artist: Option<Rect>,
    album: Option<Rect>,
    quality: Option<Rect>,
    heart: Rect,
    rating: Option<Rect>,
    time: Rect,
}

fn layout(row: Rect, cols: Columns) -> Layout {
    let inner = row.shrink2(vec2(PAD, 0.0));
    // Fixed columns first, the rest shared as fractions (title 1.6, others 1.1).
    let mut fixed = NUM_W + HEART_W + TIME_W;
    let mut n = 3.0; // number, heart, time
    let mut fr = 1.6;
    n += 1.0; // title
    if cols.quality {
        fixed += QUALITY_W;
        n += 1.0;
    }
    if cols.rating {
        fixed += RATING_W;
        n += 1.0;
    }
    let both = cols.artist && cols.album;
    if cols.artist {
        fr += 1.1;
        n += 1.0;
    }
    if cols.album {
        fr += if both { 1.1 } else { 1.0 };
        n += 1.0;
    }
    let avail = (inner.width() - fixed - GAP * (n - 1.0)).max(0.0);
    let unit = avail / fr;
    let mut x = inner.left();
    let mut take = |w: f32| {
        let r = Rect::from_min_size(pos2(x, inner.top()), vec2(w, inner.height()));
        x += w + GAP;
        r
    };
    let num = take(NUM_W);
    let title = take(unit * 1.6);
    let artist = cols.artist.then(|| take(unit * 1.1));
    let album = cols
        .album
        .then(|| take(unit * if both { 1.1 } else { 1.0 }));
    let quality = cols.quality.then(|| take(QUALITY_W));
    let heart = take(HEART_W);
    let rating = cols.rating.then(|| take(RATING_W));
    let time = take(TIME_W);
    Layout {
        num,
        title,
        artist,
        album,
        quality,
        heart,
        rating,
        time,
    }
}

/// The column header. `sorted` marks the title column as the sort key.
pub fn track_header(ui: &mut Ui, cols: Columns, number_label: &str) {
    let t = Theme::get(ui.ctx());
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), HEADER_HEIGHT), Sense::hover());
    let l = layout(rect, cols);
    let font = theme::semibold(11.0);
    let label = |r: Rect, align: Align, text: &str, color: egui::Color32| {
        paint_text_spaced(ui.painter(), r, align, text, font.clone(), color, 0.66);
    };
    label(l.num, Align::Max, number_label, t.pal.text_3);
    label(l.title, Align::Min, "TITLE", t.pal.text);
    if let Some(r) = l.artist {
        label(r, Align::Min, "ARTIST", t.pal.text_3);
    }
    if let Some(r) = l.album {
        label(r, Align::Min, "ALBUM", t.pal.text_3);
    }
    if let Some(r) = l.quality {
        label(r, Align::Min, "QUALITY", t.pal.text_3);
    }
    if let Some(r) = l.rating {
        label(r, Align::Min, "RATING", t.pal.text_3);
    }
    paint_icon(
        ui.painter(),
        pos2(l.time.right() - 6.0, l.time.center().y),
        regular::CLOCK,
        13.0,
        t.pal.text_3,
        false,
    );
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        egui::Stroke::new(1.0, t.pal.stroke),
    );
}

/// What happened to a row this frame.
pub struct RowOutput {
    pub response: Response,
    /// The heart was clicked.
    pub toggle_favorite: bool,
    /// A star was clicked (0 clears the rating).
    pub set_rating: Option<u8>,
}

/// One track row. `number` is the text of the first column (track/position
/// number); `current` marks the playing track; `selected` the selection.
pub fn track_row(
    ui: &mut Ui,
    app: &App,
    track: &Track,
    number: &str,
    current: bool,
    selected: bool,
    cols: Columns,
) -> RowOutput {
    let t = Theme::get(ui.ctx());
    let pal = t.pal;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    let l = layout(rect, cols);
    let hovered = response.hovered();
    let r = CornerRadius::same(6);
    if current {
        ui.painter().rect_filled(rect, r, t.accent.row_tint);
    } else if selected {
        ui.painter().rect_filled(rect, r, pal.row_sel);
    } else if hovered {
        ui.painter().rect_filled(rect, r, pal.hover);
    }

    // Number / now-playing / play-on-hover.
    if current {
        paint_icon(
            ui.painter(),
            l.num.right_center() - vec2(6.0, 0.0),
            fill::MUSIC_NOTES_SIMPLE,
            13.0,
            t.accent.fill,
            true,
        );
    } else if hovered {
        paint_icon(
            ui.painter(),
            l.num.right_center() - vec2(6.0, 0.0),
            fill::PLAY,
            12.0,
            pal.text,
            true,
        );
    } else {
        paint_text(
            ui.painter(),
            l.num,
            Align::Max,
            number,
            theme::mono(12.0),
            pal.text_3,
        );
    }
    let title_font = if current {
        theme::semibold(13.0)
    } else {
        theme::medium(13.0)
    };
    paint_text(
        ui.painter(),
        l.title,
        Align::Min,
        &track.title,
        title_font,
        if current { t.accent.text } else { pal.text },
    );
    if let Some(r) = l.artist {
        paint_text(
            ui.painter(),
            r,
            Align::Min,
            &track.artist,
            theme::regular(13.0),
            pal.text_15,
        );
    }
    if let Some(r) = l.album {
        paint_text(
            ui.painter(),
            r,
            Align::Min,
            &track.album,
            theme::regular(13.0),
            pal.text_2,
        );
    }
    if let Some(r) = l.quality
        && let Some(q) = App::quality_of(track)
    {
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(Rect::from_min_size(
                pos2(r.left(), r.center().y - 7.5),
                vec2(r.width(), 15.0),
            )),
            |ui| {
                quality_badge(ui, q);
            },
        );
    }

    // Heart.
    let fav = app
        .session
        .library
        .favorite_state(None, &track.id.to_string())
        .unwrap_or(track.is_favorite);
    let heart_hit = Rect::from_center_size(l.heart.center(), vec2(HEART_W + 6.0, ROW_HEIGHT - 6.0));
    let heart = ui.interact(heart_hit, response.id.with("heart"), Sense::click());
    let (glyph, filled, color) = if fav {
        (
            fill::HEART,
            true,
            if current { t.accent.fill } else { pal.text },
        )
    } else {
        (
            regular::HEART,
            false,
            if heart.hovered() {
                pal.text
            } else {
                pal.text_4
            },
        )
    };
    paint_icon(ui.painter(), l.heart.center(), glyph, 15.0, color, filled);

    // Rating stars.
    let mut set_rating = None;
    if let Some(r) = l.rating {
        let current_rating = track.rating.unwrap_or(0);
        for i in 0..5u8 {
            let c = pos2(r.left() + 6.5 + f32::from(i) * 13.0, r.center().y);
            let hit = Rect::from_center_size(c, vec2(13.0, ROW_HEIGHT - 6.0));
            let star = ui.interact(hit, response.id.with(("star", i)), Sense::click());
            let on = i < current_rating;
            let (g, f, col) = if on {
                (fill::STAR, true, pal.text)
            } else {
                (
                    regular::STAR,
                    false,
                    if star.hovered() {
                        pal.text_2
                    } else {
                        pal.text_5
                    },
                )
            };
            paint_icon(ui.painter(), c, g, 12.0, col, f);
            if star.clicked() {
                set_rating = Some(if current_rating == i + 1 { 0 } else { i + 1 });
            }
        }
    }
    paint_text(
        ui.painter(),
        l.time,
        Align::Max,
        &format_duration(track.duration.as_secs()),
        theme::mono(12.0),
        pal.text_2,
    );

    // The badge's child scope moved the cursor; put it back below the row.
    ui.advance_cursor_after_rect(rect);
    RowOutput {
        toggle_favorite: heart.clicked(),
        set_rating,
        response,
    }
}
