//! The Properties side sheet: technical data and library details for a track
//! or album. Track data is read from the file in the background; the
//! Track/Album tabs switch to the album a track belongs to.

use super::overlays;
use crate::app::{App, Sheet};
use crate::theme::{self, Palette};
use crate::widgets::{cover_image, empty_state, icon_button, quality_badge, segmented};
use aulos_core::format::{format_duration_exact, format_sample_rate, format_size};
use aulos_core::library::properties::{
    Loadable, MISSING, Subject, format_bit_depth, format_bitrate_line, format_bitrate_range,
    format_channels, format_codec, format_file_size, format_year_range, known_from_track,
    list_or_dash, merge, source_label, text_or_dash,
};
use aulos_core::library::{Album, CoverArt, Track};
use egui::{
    Align, Area, Context, CornerRadius, Frame, Id, Key, Layout, Order, RichText, ScrollArea, Sense,
    Stroke, Ui, pos2, vec2,
};
use egui_phosphor::regular;

const HEADER_H: f32 = 52.0;
const KEY_W: f32 = 112.0;

enum Intent {
    Close,
    /// The Album tab was chosen on a track: open its album's properties.
    AlbumOf(Box<Track>),
}

pub fn show(app: &mut App, ctx: &Context) {
    if app.sheet != Some(Sheet::Properties) {
        theme::motion::slide_in(ctx, Id::new("aulos_properties_sheet_slide"), false);
        return;
    }
    for intent in draw(app, ctx) {
        apply(app, intent);
    }
}

fn draw(app: &App, ctx: &Context) -> Vec<Intent> {
    let pal = app.pal;
    let mut out = Vec::new();
    if ctx.input(|i| i.key_pressed(Key::Escape)) && !overlays::active(app) {
        out.push(Intent::Close);
    }

    let content = ctx.content_rect();
    let (width, top, bottom) = super::sheet_extent(app, content, 8.0);
    let height = (bottom - top).max(0.0);
    // Slides in from the right edge.
    let slide = theme::motion::slide_in(ctx, Id::new("aulos_properties_sheet_slide"), true);
    let origin = pos2(
        content.right() - 8.0 - width + (1.0 - slide) * (width + 16.0),
        top,
    );
    let subject = app.session.properties.subject();

    Area::new(Id::new("aulos_properties_sheet"))
        .order(Order::Foreground)
        .fixed_pos(origin)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.set_height(height);
            Frame::new()
                .fill(pal.surface_1)
                .stroke(Stroke::new(1.0, pal.track))
                .corner_radius(CornerRadius::same(12))
                .shadow(theme::shadow([-12, 0], 40, 102))
                .show(ui, |ui| {
                    ui.set_width(width - 2.0);
                    ui.set_height(height - 2.0);
                    header(ui, pal, subject, &mut out);
                    let body_h = (height - 2.0 - HEADER_H).max(0.0);
                    ScrollArea::vertical()
                        .max_height(body_h)
                        .auto_shrink([false, false])
                        .show(ui, |ui| match subject {
                            None => {
                                empty_state(
                                    ui,
                                    regular::INFO,
                                    &aulos_core::fl!("props-empty-title"),
                                    &aulos_core::fl!("props-empty-hint"),
                                    None,
                                );
                            }
                            Some(subject) => body(ui, app, subject),
                        });
                });
        });
    out
}

fn header(ui: &mut Ui, pal: Palette, subject: Option<&Subject>, out: &mut Vec<Intent>) {
    let r = ui.horizontal(|ui| {
        ui.set_height(HEADER_H);
        ui.add_space(16.0);
        ui.label(
            RichText::new(aulos_core::fl!("properties"))
                .font(theme::semibold(15.0))
                .color(pal.text),
        );
        // Track / Album tabs sit beside the title.
        if let Some(subject) = subject {
            ui.add_space(10.0);
            let mut chosen = usize::from(matches!(subject, Subject::Album(_)));
            if segmented(
                ui,
                &mut chosen,
                &[
                    &aulos_core::fl!("props-tab-track"),
                    &aulos_core::fl!("props-tab-album"),
                ],
                28.0,
            ) && chosen == 1
                && let Subject::Track(track) = subject
            {
                out.push(Intent::AlbumOf(Box::new(track.clone())));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(10.0);
            if icon_button(ui, regular::X, 28.0).clicked() {
                out.push(Intent::Close);
            }
        });
    });
    let rect = r.response.rect;
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, pal.stroke),
    );
}

fn body(ui: &mut Ui, app: &App, subject: &Subject) {
    let pal = app.pal;
    ui.add_space(12.0);

    ui.add_space(14.0);
    let (title, subtitle, key, quality) = match subject {
        Subject::Track(track) => (
            track.title.clone(),
            format!("{} · {}", track.grouping_artist(), track.album),
            track.cover_key(),
            crate::app::App::quality_of(track),
        ),
        Subject::Album(album) => (
            album.name.clone(),
            album.artist.clone(),
            CoverArt::album_key(&album.artist, &album.name),
            None,
        ),
    };
    ui.horizontal_top(|ui| {
        ui.add_space(16.0);
        let (cover_rect, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
        cover_image(ui, &app.covers, cover_rect, app.covers.thumb(&key), &key, 6);
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new(&title)
                    .font(theme::semibold(14.0))
                    .color(pal.text),
            );
            ui.label(
                RichText::new(&subtitle)
                    .font(theme::regular(12.0))
                    .color(pal.text_2),
            );
            if let Some(quality) = quality {
                ui.add_space(4.0);
                quality_badge(ui, quality);
            }
        });
    });

    ui.add_space(16.0);
    match subject {
        Subject::Track(track) => track_rows(ui, app, track),
        Subject::Album(album) => album_rows(ui, app, album),
    }
    ui.add_space(12.0);
}

fn track_rows(ui: &mut Ui, app: &App, track: &Track) {
    let state = app.session.properties.track_info();
    let loaded = match state {
        Loadable::Loaded(info) => Some(info),
        _ => None,
    };
    let info = merge(loaded, known_from_track(track));
    match state {
        Loadable::Loading => status(ui, app, &aulos_core::fl!("props-reading-file")),
        Loadable::Failed(message) => status(
            ui,
            app,
            &aulos_core::fl!("props-read-file-failed", error = message.as_str()),
        ),
        Loadable::Idle | Loadable::Loaded(_) => {}
    }

    let rows: Vec<(String, String)> = vec![
        (aulos_core::fl!("props-artist"), track.artist.clone()),
        (
            aulos_core::fl!("props-album"),
            text_or_dash(Some(&track.album)),
        ),
        (
            aulos_core::fl!("props-duration"),
            info.duration
                .map_or(MISSING.to_string(), format_duration_exact),
        ),
        (aulos_core::fl!("props-source"), source_label(track)),
        (
            aulos_core::fl!("props-container"),
            text_or_dash(info.container.as_ref()),
        ),
        (aulos_core::fl!("props-codec"), format_codec(&info)),
        (aulos_core::fl!("props-bitrate"), format_bitrate_line(&info)),
        (
            aulos_core::fl!("props-sample-rate"),
            info.sample_rate
                .map_or(MISSING.to_string(), format_sample_rate),
        ),
        (
            aulos_core::fl!("props-bit-depth"),
            format_bit_depth(info.bits_per_sample, info.lossless),
        ),
        (
            aulos_core::fl!("props-channels"),
            format_channels(info.channels, info.channel_layout.as_deref()),
        ),
        (aulos_core::fl!("props-size"), format_file_size(&info)),
        (
            aulos_core::fl!("props-encoder"),
            text_or_dash(info.encoder.as_ref()),
        ),
        (
            aulos_core::fl!("props-tag-formats"),
            list_or_dash(&info.tag_formats),
        ),
        (
            aulos_core::fl!("props-date"),
            text_or_dash(info.date.as_ref()),
        ),
        (
            aulos_core::fl!("props-replaygain-track"),
            text_or_dash(info.replay_gain.track_gain.as_ref()),
        ),
        (
            aulos_core::fl!("props-replaygain-album"),
            text_or_dash(info.replay_gain.album_gain.as_ref()),
        ),
        (
            aulos_core::fl!("props-location"),
            text_or_dash(info.remote_path.as_ref()),
        ),
    ];
    for (key, value) in rows {
        row(ui, app, &key, &value);
    }
}

fn album_rows(ui: &mut Ui, app: &App, album: &Album) {
    let state = app.session.properties.album_summary();
    let summary = match state {
        Loadable::Loaded(summary) => Some(summary),
        _ => None,
    };
    match state {
        Loadable::Loading => status(ui, app, &aulos_core::fl!("props-reading-files")),
        Loadable::Failed(message) => status(
            ui,
            app,
            &aulos_core::fl!("props-read-files-failed", error = message.as_str()),
        ),
        Loadable::Idle | Loadable::Loaded(_) => {}
    }

    let total: std::time::Duration = album.tracks.iter().map(|t| t.duration).sum();
    let year = match summary {
        Some(s) => format_year_range(s.year_min, s.year_max),
        None => text_or_dash((album.year > 0).then(|| album.year.to_string())),
    };
    let mut rows: Vec<(String, String)> = vec![
        (aulos_core::fl!("props-artist"), album.artist.clone()),
        (aulos_core::fl!("props-year"), year),
        (
            aulos_core::fl!("props-tracks"),
            album.tracks.len().to_string(),
        ),
        (
            aulos_core::fl!("props-duration"),
            format_duration_exact(summary.map_or(total, |s| s.total_duration)),
        ),
    ];
    if let Some(s) = summary {
        let rates: Vec<String> = s
            .sample_rates
            .iter()
            .map(|r| format_sample_rate(*r))
            .collect();
        rows.push((
            aulos_core::fl!("props-size"),
            s.total_size.map_or(MISSING.to_string(), format_size),
        ));
        rows.push((aulos_core::fl!("props-formats"), list_or_dash(&s.formats)));
        rows.push((aulos_core::fl!("props-sample-rates"), list_or_dash(&rates)));
        rows.push((
            aulos_core::fl!("props-bit-depths"),
            list_or_dash(&s.bit_depths),
        ));
        rows.push((
            aulos_core::fl!("props-bitrate"),
            format_bitrate_range(s.bitrate_min_kbps, s.bitrate_max_kbps),
        ));
        if s.unprobed > 0 {
            rows.push((
                aulos_core::fl!("props-not-read"),
                aulos_core::fl!("props-not-read-count", count = s.unprobed),
            ));
        }
    }
    for (key, value) in rows {
        row(ui, app, &key, &value);
    }
}

fn status(ui: &mut Ui, app: &App, text: &str) {
    let pal = app.pal;
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.label(
            RichText::new(text)
                .font(theme::regular(12.0))
                .color(pal.text_3),
        );
    });
}

/// One key/value row: 112px key, value wraps in the rest, hairline below.
fn row(ui: &mut Ui, app: &App, key: &str, value: &str) {
    let pal = app.pal;
    ui.add_space(6.0);
    let r = ui.horizontal_top(|ui| {
        ui.add_space(16.0);
        ui.allocate_ui(vec2(KEY_W, 0.0), |ui| {
            ui.label(
                RichText::new(key)
                    .font(theme::regular(12.0))
                    .color(pal.text_2),
            );
        });
        ui.add(
            egui::Label::new(
                RichText::new(value)
                    .font(theme::regular(12.5))
                    .color(pal.text),
            )
            .wrap(),
        );
    });
    ui.add_space(6.0);
    let rect = r.response.rect;
    ui.painter().hline(
        rect.left() + 16.0..=rect.right() - 16.0,
        rect.bottom() + 0.5,
        Stroke::new(1.0, pal.stroke),
    );
}

fn apply(app: &mut App, intent: Intent) {
    match intent {
        Intent::Close => app.sheet = None,
        Intent::AlbumOf(track) => {
            let album = app
                .session
                .library
                .find_album(track.grouping_artist(), &track.album)
                .and_then(|index| app.session.library.albums.get(index).cloned());
            match album {
                Some(album) => {
                    let effect = app.session.open_album_properties(album);
                    app.run(effect);
                }
                None => app.toast_error(aulos_core::fl!(
                    "props-album-not-in-library",
                    album = track.album.as_str()
                )),
            }
        }
    }
}
