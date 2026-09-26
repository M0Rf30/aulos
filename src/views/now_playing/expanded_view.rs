// SPDX-License-Identifier: GPL-3.0

use super::{NowPlayingMessage, format_time};
use crate::config::RepeatMode;
use crate::fl;
use crate::library::Track;
use crate::library::palette::Accent;
use crate::player::PlaybackState;
use crate::views::common;
use cosmic::cosmic_theme::palette::WithAlpha;
use cosmic::iced::alignment::{Horizontal, Vertical};
use cosmic::iced::core::Background;
use cosmic::iced::core::text::{Ellipsize, EllipsizeHeightLimit, Wrapping};
use cosmic::iced::widget::Stack;
use cosmic::iced::{Alignment, Color, Length};
use cosmic::widget;
use cosmic::widget::tooltip::Position as TooltipPosition;
use std::rc::Rc;
use std::time::Duration;

#[cfg(feature = "visualizer")]
use std::sync::{Arc, Mutex};

/// Text drawn on top of the blurred cover-art backdrop is intentionally
/// **not** theme-derived: the backdrop's luminance comes from the album art
/// itself, which can be light or dark independent of whichever COSMIC theme
/// is active, so a fixed light-on-dark pair is the only way to guarantee
/// legibility against arbitrary artwork. This pair is used ONLY while a
/// backdrop is actually painted behind the panel (`has_backdrop`); with no
/// backdrop the panel falls back to pure theme colors instead (see
/// `theme_surface_class`).
pub(super) const BACKDROP_SCRIM: Color = Color {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.72,
};
pub(super) const BACKDROP_TEXT: Color = Color::WHITE;
pub(super) const BACKDROP_SUBTEXT: Color = Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.7,
};

/// Plain COSMIC surface background — used whenever there is no blurred
/// cover-art backdrop to sit on top of, so panels match the active theme
/// (light or dark) and respect frosted-glass blur when the user has it
/// enabled (`theme.transparent`, set by libcosmic from `frosted_windows`).
fn theme_surface_class() -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        cosmic::iced::widget::container::Style {
            background: Some(cosmic.background(theme.transparent).base.into()),
            ..Default::default()
        }
    })
}

/// Transport-row icon button with a caption tooltip and an **owned**
/// label. `fl!()` returns `String`, not `&'static str`, so
/// `common::icon_button`'s borrowed signature can't take it without
/// fighting lifetimes — this local variant sidesteps that. `selected`
/// renders the accent-highlighted state (shuffle/repeat); `on_press: None`
/// renders the standard disabled style, used by the empty state so the row
/// stays visible but inert instead of disappearing.
fn transport_button<'a, M: Clone + 'static>(
    icon_name: &'static str,
    icon_size: u16,
    selected: bool,
    label: String,
    on_press: Option<M>,
) -> cosmic::Element<'a, M> {
    let button = widget::button::icon(widget::icon::from_name(icon_name).size(icon_size))
        .selected(selected)
        .on_press_maybe(on_press);
    widget::tooltip(button, widget::text::caption(label), TooltipPosition::Top).into()
}

/// Play/pause button rendered with `cosmic::theme::Button::Suggested`
/// (COSMIC's filled/accent button class) so it reads as the ONE primary
/// action in the transport row instead of a same-weight icon among the
/// other four. `transport_button`'s shared signature has no class-override
/// knob and backs four other buttons across three call sites in this file
/// — rather than complicate it, this is a small dedicated variant used
/// only for the play/pause button. When cover-art `accent` is available it
/// swaps in `accent_button_class` (accent-filled) instead of the plain
/// theme-accent `Suggested` style; `None` keeps `Suggested` untouched.
fn play_pause_button<'a, M: Clone + 'static>(
    icon_name: &'static str,
    icon_size: u16,
    label: String,
    on_press: M,
    accent: Option<&Accent>,
) -> cosmic::Element<'a, M> {
    let class = accent
        .map(accent_button_class)
        .unwrap_or(cosmic::theme::Button::Suggested);
    let button = widget::button::icon(widget::icon::from_name(icon_name).size(icon_size))
        .class(class)
        .on_press(on_press);
    widget::tooltip(button, widget::text::caption(label), TooltipPosition::Top).into()
}

/// Button class that fills with the cover-art accent colour instead of the
/// theme accent, mirroring `Button::Suggested`'s filled/pill shape via
/// `radius_xl` corners so the play/pause button stays visually consistent
/// while matching the current artwork. Hover/pressed/disabled states dim
/// the same fill via alpha rather than deriving separate colours from the
/// palette.
fn accent_button_class(accent: &Accent) -> cosmic::theme::Button {
    let background = Color::from_rgb(accent.color[0], accent.color[1], accent.color[2]);
    let on = Color::from_rgb(accent.on_color[0], accent.on_color[1], accent.on_color[2]);
    let style = move |theme: &cosmic::Theme, alpha: f32| cosmic::widget::button::Style {
        background: Some(Background::Color(Color {
            a: alpha,
            ..background
        })),
        text_color: Some(on),
        icon_color: Some(on),
        border_radius: theme.cosmic().corner_radii.radius_xl.into(),
        ..cosmic::widget::button::Style::new()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |_focused, theme| style(theme, 1.0)),
        hovered: Box::new(move |_focused, theme| style(theme, 0.85)),
        pressed: Box::new(move |_focused, theme| style(theme, 0.7)),
        disabled: Box::new(move |theme| style(theme, 0.5)),
    }
}

/// Slider class that tints only the active (played) portion of the rail
/// and the drag handle with the cover-art accent colour. Starts from
/// `Slider::Standard`'s own computed style via `Catalog::style` so border,
/// rail thickness and handle shape stay perfectly in sync with the theme;
/// only the two accent-carrying fields are overridden.
fn accent_slider_class(accent: &Accent) -> cosmic::theme::iced::Slider {
    let color = Color::from_rgb(accent.color[0], accent.color[1], accent.color[2]);
    let style = move |theme: &cosmic::Theme, status: widget::slider::Status| {
        let mut base =
            widget::slider::Catalog::style(theme, &cosmic::theme::iced::Slider::default(), status);
        base.rail.backgrounds.0 = Background::Color(color);
        base.handle.background = Background::Color(color);
        base
    };
    cosmic::theme::iced::Slider::Custom {
        active: Rc::new(move |t| style(t, widget::slider::Status::Active)),
        hovered: Rc::new(move |t| style(t, widget::slider::Status::Hovered)),
        dragging: Rc::new(move |t| style(t, widget::slider::Status::Dragged)),
    }
}

/// Small "LIVE" pill shown in place of the elapsed/remaining time labels
/// for a stream with no known duration (radio) — same row slot as the
/// time labels, just different content, so `seek_bar_row`'s tree shape
/// never changes between a normal track and a live stream.
fn live_badge<'a>() -> cosmic::Element<'a, NowPlayingMessage> {
    let pill = widget::Row::new()
        .push(widget::icon::from_name("media-record-symbolic").size(12))
        .push(widget::text::body(fl!("now-playing-live")))
        .spacing(6)
        .align_y(Alignment::Center);
    widget::container(pill)
        .padding([2, 10])
        .class(cosmic::theme::Container::custom(|theme| {
            let cosmic = theme.cosmic();
            let accent = cosmic.accent_color();
            cosmic::iced::widget::container::Style {
                background: Some(Background::Color(accent.with_alpha(0.16).into())),
                text_color: Some(accent.into()),
                icon_color: Some(accent.into()),
                border: cosmic::iced::Border {
                    radius: cosmic.corner_radii.radius_xs.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }))
        .into()
}

/// Standard collapse-to-compact-bar control, pinned top-right. 24px icon +
/// `space_xs` padding on every side gives a >= 32px hit target (24 + 2×8 =
/// 40px) — comfortably above the minimum, not just meeting it.
fn collapse_button<'a>() -> cosmic::Element<'a, NowPlayingMessage> {
    let padding = f32::from(cosmic::theme::active().cosmic().spacing.space_xs);
    let button = widget::button::icon(widget::icon::from_name("go-down-symbolic").size(24))
        .padding(padding)
        .on_press(NowPlayingMessage::Collapse);
    widget::tooltip(
        button,
        widget::text::caption(fl!("expanded-collapse")),
        TooltipPosition::Bottom,
    )
    .into()
}

/// Shown when nothing is loaded. A single composition centered in the
/// *whole* view — never the cover/text split, since there is no cover to
/// show — with the seek bar and transport row still present but disabled,
/// so the layout doesn't jump the instant playback starts.
fn empty_expanded_view<'a>() -> cosmic::Element<'a, NowPlayingMessage> {
    let spacing = cosmic::theme::active().cosmic().spacing;
    let space_xxs = f32::from(spacing.space_xxs);
    let space_s = f32::from(spacing.space_s);
    let space_m = f32::from(spacing.space_m);

    let top_bar = widget::Row::new()
        .push(
            widget::Space::new()
                .width(Length::Fill)
                .height(Length::Shrink),
        )
        .push(collapse_button())
        .align_y(Alignment::Center);

    let message = widget::Column::new()
        .push(widget::icon::from_name("audio-x-generic-symbolic").size(64))
        .push(widget::text::title3(fl!("no-track-playing")))
        .push(widget::text::body(fl!("expanded-empty-hint")))
        .spacing(space_s)
        .align_x(Alignment::Center);

    // A real `Slider` has no `disabled()` flag — its `on_change` is a
    // mandatory closure, not an `Option`, so it can never be made
    // genuinely inert. A determinate progress bar has no interaction
    // handling at all, so it reads as the same "seek bar" shape without
    // ever being draggable — the correct widget for "disabled", not a
    // degenerate slider hack.
    let seek_bar = widget::Row::new()
        .push(widget::text::body(format_time(Duration::ZERO)))
        .push(widget::determinate_linear(0.0).width(Length::Fill))
        .push(widget::text::body(format_time(Duration::ZERO)))
        .spacing(space_s)
        .align_y(Alignment::Center);

    // Same left-to-right order as the compact bar and the playing-state
    // transport row: shuffle, previous, play, stop, next, repeat.
    let transport = widget::Row::new()
        .push(transport_button::<NowPlayingMessage>(
            "media-playlist-shuffle-symbolic",
            24,
            false,
            fl!("shuffle"),
            None,
        ))
        .push(transport_button::<NowPlayingMessage>(
            "media-skip-backward-symbolic",
            28,
            false,
            fl!("previous"),
            None,
        ))
        .push(transport_button::<NowPlayingMessage>(
            "media-playback-start-symbolic",
            36,
            false,
            fl!("play"),
            None,
        ))
        .push(transport_button::<NowPlayingMessage>(
            "media-playback-stop-symbolic",
            28,
            false,
            fl!("stop"),
            None,
        ))
        .push(transport_button::<NowPlayingMessage>(
            "media-skip-forward-symbolic",
            28,
            false,
            fl!("next"),
            None,
        ))
        .push(transport_button::<NowPlayingMessage>(
            "media-playlist-repeat-symbolic",
            24,
            false,
            fl!("repeat"),
            None,
        ))
        .spacing(space_xxs)
        .align_y(Alignment::Center);

    let content = widget::Column::new()
        .push(message)
        .push(widget::container(seek_bar).width(Length::Fixed(420.0)))
        .push(transport)
        .spacing(space_m)
        .align_x(Alignment::Center);

    let body = widget::container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center);

    widget::container(
        widget::Column::new()
            .push(top_bar)
            .push(body)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .class(theme_surface_class())
    .into()
}

/// Elapsed-time label, drag/seek slider, and total-duration label — shared
/// by the two-column and fullscreen layouts so both stay in perfect sync.
/// For a live stream (`is_live`, no known duration), the same three slots
/// render a "LIVE" badge and a non-interactive progress bar instead, so the
/// row's tree shape never changes when a live stream starts or ends.
fn seek_bar_row<'a>(
    display_position: Duration,
    duration: Duration,
    progress: f32,
    space_s: f32,
    accent: Option<&Accent>,
    is_live: bool,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let seek_widget: cosmic::Element<'_, NowPlayingMessage> = if is_live {
        widget::determinate_linear(0.0).width(Length::Fill).into()
    } else {
        let mut seek_slider = widget::slider(0.0..=1.0, progress, NowPlayingMessage::SeekPreview)
            .step(0.001_f32)
            .on_release(NowPlayingMessage::SeekCommit)
            .width(Length::Fill);
        if let Some(accent) = accent {
            seek_slider = seek_slider.class(accent_slider_class(accent));
        }
        seek_slider.into()
    };
    let time_start_slot: cosmic::Element<'_, NowPlayingMessage> = if is_live {
        live_badge()
    } else {
        widget::text::body(format_time(display_position)).into()
    };
    let time_end_slot: cosmic::Element<'_, NowPlayingMessage> = if is_live {
        widget::Space::new().width(0).height(0).into()
    } else {
        widget::text::body(format_time(duration)).into()
    };
    widget::Row::new()
        .push(time_start_slot)
        .push(seek_widget)
        .push(time_end_slot)
        .spacing(space_s)
        .align_y(Alignment::Center)
        .into()
}

/// Shuffle / previous / play-pause / stop / next / repeat transport
/// controls, in the same left-to-right order as the compact bar. The
/// prev/play/stop/next "core" cluster stays tightly grouped; shuffle and
/// repeat sit a much wider gap away on either side so they read as
/// secondary controls, not same-weight siblings of the core cluster — and
/// the play/pause button itself renders as the row's one primary/filled
/// action via `play_pause_button` below. `is_live` disables shuffle/prev/
/// next/repeat (meaningless for a stream with no queue position to shift)
/// without removing them, so the row's shape never changes.
fn transport_row<'a>(
    state: PlaybackState,
    shuffle: bool,
    repeat_mode: RepeatMode,
    space_xxs: f32,
    accent: Option<&Accent>,
    is_live: bool,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let play_icon = if state == PlaybackState::Playing {
        "media-playback-pause-symbolic"
    } else {
        "media-playback-start-symbolic"
    };
    let shuffle_icon = "media-playlist-shuffle-symbolic";
    let repeat_icon = repeat_mode.icon_name();
    let play_label = if state == PlaybackState::Playing {
        fl!("pause")
    } else {
        fl!("play")
    };
    let stop_enabled = state != PlaybackState::Stopped;

    // Wider than the core cluster's own `space_xxs` gap, so shuffle/repeat
    // read as clearly-separate secondary controls instead of two more
    // same-weight siblings of prev/play/stop/next.
    let group_gap = f32::from(cosmic::theme::active().cosmic().spacing.space_l);

    let core = widget::Row::new()
        .push(transport_button(
            "media-skip-backward-symbolic",
            28,
            false,
            fl!("previous"),
            (!is_live).then_some(NowPlayingMessage::Previous),
        ))
        .push(play_pause_button(
            play_icon,
            36,
            play_label,
            NowPlayingMessage::TogglePlayback,
            accent,
        ))
        .push(transport_button(
            "media-playback-stop-symbolic",
            28,
            false,
            fl!("stop"),
            stop_enabled.then_some(NowPlayingMessage::Stop),
        ))
        .push(transport_button(
            "media-skip-forward-symbolic",
            28,
            false,
            fl!("next"),
            (!is_live).then_some(NowPlayingMessage::Next),
        ))
        .spacing(space_xxs)
        .align_y(Alignment::Center);

    widget::Row::new()
        .push(transport_button(
            shuffle_icon,
            24,
            shuffle,
            fl!("shuffle"),
            (!is_live).then_some(NowPlayingMessage::ToggleShuffle),
        ))
        .push(
            widget::Space::new()
                .width(Length::Fixed(group_gap))
                .height(Length::Shrink),
        )
        .push(core)
        .push(
            widget::Space::new()
                .width(Length::Fixed(group_gap))
                .height(Length::Shrink),
        )
        .push(transport_button(
            repeat_icon,
            24,
            repeat_mode != RepeatMode::None,
            fl!("repeat"),
            (!is_live).then_some(NowPlayingMessage::CycleRepeat),
        ))
        .align_y(Alignment::Center)
        .into()
}

/// Compact variant of `transport_row` for the fullscreen-visualizer HUD
/// card: same shuffle/prev/play/stop/next/repeat set, but at the bottom
/// compact bar's icon scale (24/24/32/24/24/24, `spacing(4)`, see
/// `compact_bar.rs`) instead of the hero layout's larger icons and wide
/// grouping gaps — the HUD card sits over the visualizer and must stay
/// visually light, not a shrunk copy of the two-panel layout's control
/// cluster. `is_live` disables shuffle/prev/next/repeat, same as
/// `transport_row`.
fn compact_transport_row<'a>(
    state: PlaybackState,
    shuffle: bool,
    repeat_mode: RepeatMode,
    is_live: bool,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let play_icon = if state == PlaybackState::Playing {
        "media-playback-pause-symbolic"
    } else {
        "media-playback-start-symbolic"
    };
    let play_label = if state == PlaybackState::Playing {
        fl!("pause")
    } else {
        fl!("play")
    };
    let stop_enabled = state != PlaybackState::Stopped;

    widget::Row::new()
        .push(transport_button(
            "media-playlist-shuffle-symbolic",
            24,
            shuffle,
            fl!("shuffle"),
            (!is_live).then_some(NowPlayingMessage::ToggleShuffle),
        ))
        .push(transport_button(
            "media-skip-backward-symbolic",
            24,
            false,
            fl!("previous"),
            (!is_live).then_some(NowPlayingMessage::Previous),
        ))
        .push(transport_button(
            play_icon,
            32,
            false,
            play_label,
            Some(NowPlayingMessage::TogglePlayback),
        ))
        .push(transport_button(
            "media-playback-stop-symbolic",
            24,
            false,
            fl!("stop"),
            stop_enabled.then_some(NowPlayingMessage::Stop),
        ))
        .push(transport_button(
            "media-skip-forward-symbolic",
            24,
            false,
            fl!("next"),
            (!is_live).then_some(NowPlayingMessage::Next),
        ))
        .push(transport_button(
            repeat_mode.icon_name(),
            24,
            repeat_mode != RepeatMode::None,
            fl!("repeat"),
            (!is_live).then_some(NowPlayingMessage::CycleRepeat),
        ))
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
}

/// Volume icon + slider, the lyrics toggle and the Up Next queue-drawer
/// toggle, plus (visualizer builds) the visualizer on/off toggle and,
/// while active, the next-preset and preset-browser-toggle buttons.
/// Identical wiring to the view's original inline `bottom_row`.
fn utility_row<'a>(
    volume: f32,
    #[cfg(feature = "visualizer")] visualizer_active: bool,
    #[cfg(feature = "visualizer")] viz_browser_open: bool,
    is_queue_open: bool,
    space_xs: f32,
    space_xxs: f32,
) -> cosmic::Element<'a, NowPlayingMessage> {
    // Not needed for the row's own (uniform `space_xs`) spacing — kept as a
    // parameter so callers have a tighter-grouping knob available without
    // changing this function's signature later.
    let _ = space_xxs;

    let volume = volume.clamp(0.0, 1.0);
    let volume_icon_name = if volume <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if volume < 0.33 {
        "audio-volume-low-symbolic"
    } else if volume < 0.66 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    };

    #[allow(unused_mut)]
    let mut row = widget::Row::new()
        .push(widget::icon::from_name(volume_icon_name).size(20))
        .push(
            widget::slider(0.0..=1.0, volume, NowPlayingMessage::SetVolume)
                .step(0.01_f32)
                .on_release(NowPlayingMessage::VolumeCommit)
                .width(Length::Fixed(160.0)),
        )
        .push(transport_button(
            "view-list-lyrics-symbolic",
            24,
            false,
            fl!("lyrics"),
            Some(NowPlayingMessage::ShowLyrics),
        ))
        .push(transport_button(
            "media-playlist-consecutive-symbolic",
            24,
            is_queue_open,
            fl!("queue"),
            Some(NowPlayingMessage::ToggleQueue),
        ));

    #[cfg(feature = "visualizer")]
    {
        let viz_icon = "applications-multimedia-symbolic";
        row = row.push(
            widget::button::icon(widget::icon::from_name(viz_icon).size(24))
                .on_press(NowPlayingMessage::ToggleVisualizer),
        );
        if visualizer_active {
            row = row.push(
                widget::button::icon(
                    widget::icon::from_name("media-skip-forward-symbolic").size(20),
                )
                .on_press(NowPlayingMessage::NextPreset),
            );
            row = row.push(transport_button(
                "view-grid-symbolic",
                20,
                viz_browser_open,
                fl!("viz-toggle-presets"),
                Some(NowPlayingMessage::TogglePresetBrowser),
            ));
        }
    }

    row.spacing(space_xs).align_y(Alignment::Center).into()
}

#[allow(clippy::too_many_arguments)]
pub fn expanded_now_playing<'a>(
    current_track: Option<&'a Track>,
    state: PlaybackState,
    position: Duration,
    duration: Duration,
    volume: f32,
    shuffle: bool,
    repeat_mode: RepeatMode,
    cover_art: Option<&'a widget::icon::Handle>,
    blurred_cover: Option<&'a widget::icon::Handle>,
    accent: Option<&'a Accent>,
    is_queue_open: bool,
    seeking_preview: Option<f32>,
    expand_progress: f32,
    lyrics_overlay_active: bool,
    lyrics: Option<&'a crate::library::Lyrics>,
    lyrics_loading: bool,
    #[cfg(feature = "visualizer")] visualizer_active: bool,
    #[cfg(feature = "visualizer")] viz_frame_buf: Arc<Mutex<super::viz_shader::VizFrameBuffer>>,
    #[cfg(feature = "visualizer")] viz_metadata_opacity: f32,
    #[cfg(feature = "visualizer")] viz_fullscreen: bool,
    // Whether the fullscreen HUD control card should currently be shown.
    // `false` once the mouse has been idle past `VIZ_HUD_HOLD_FRAMES`;
    // only governs the control card — lyrics (if overlaid) are content,
    // not chrome, and stay visible regardless.
    #[cfg(feature = "visualizer")] viz_hud_visible: bool,
    #[cfg(feature = "visualizer")] viz_browser_open: bool,
    #[cfg(feature = "visualizer")] viz_preset_entries: &'a [super::visualizer::PresetEntry],
    #[cfg(feature = "visualizer")] viz_preset_search: &'a str,
    #[cfg(feature = "visualizer")] viz_locked: bool,
    #[cfg(feature = "visualizer")] viz_beat_sensitivity: f32,
    #[cfg(feature = "visualizer")] viz_current_preset_name: Option<&'a str>,
) -> cosmic::Element<'a, NowPlayingMessage> {
    let _ = expand_progress;

    let Some(track) = current_track else {
        return empty_expanded_view();
    };

    // Radio/other zero-duration streams: no seek slider, and shuffle/prev/
    // next/repeat are meaningless (there is nothing to shuffle or skip to)
    // so they render disabled rather than disappearing.
    let is_live = super::is_live_stream(track);

    let spacing = cosmic::theme::active().cosmic().spacing;
    let space_xxs = f32::from(spacing.space_xxs);
    let space_xs = f32::from(spacing.space_xs);
    let space_s = f32::from(spacing.space_s);
    let space_m = f32::from(spacing.space_m);
    let space_l = f32::from(spacing.space_l);

    // A backdrop (blurred cover art, or the visualizer surface) is the
    // only case where the right panel is allowed to use the fixed
    // scrim/white-text pair below — see the const doc comment.
    #[cfg(feature = "visualizer")]
    let has_backdrop = visualizer_active || blurred_cover.is_some();
    #[cfg(not(feature = "visualizer"))]
    let has_backdrop = blurred_cover.is_some();

    let (progress, display_position) = if let Some(frac) = seeking_preview {
        let preview_pos = Duration::from_secs_f32(frac * duration.as_secs_f32());
        (frac, preview_pos)
    } else {
        let p = if duration.as_secs_f32() > 0.0 {
            (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (p, position)
    };

    // In fullscreen the visualizer is the "king": full-bleed behind a
    // compact control card, instead of sharing the view 50/50 with a big
    // cover-art panel. Only reachable once the visualizer feature exists,
    // is actually rendering, and the user has double-clicked into
    // fullscreen.
    #[cfg(feature = "visualizer")]
    let fullscreen_mode = viz_fullscreen && visualizer_active;
    #[cfg(not(feature = "visualizer"))]
    let fullscreen_mode = false;
    // `viz_hud_visible` only exists as a real parameter behind the
    // visualizer feature; the fullscreen-mode branch below is always dead
    // (fullscreen_mode is always false here) but still needs to compile.
    #[cfg(not(feature = "visualizer"))]
    let viz_hud_visible = true;

    let content_layer: cosmic::Element<'_, NowPlayingMessage> = if fullscreen_mode {
        // --- Fullscreen: visualizer is full-bleed; everything else lives
        // in one compact card docked at the bottom of the screen. ---
        let thumb: cosmic::Element<'_, NowPlayingMessage> = if let Some(handle) = cover_art {
            widget::container(
                widget::icon::icon(handle.clone())
                    .content_fit(cosmic::iced::ContentFit::Contain)
                    .width(Length::Fixed(48.0))
                    .height(Length::Fixed(48.0)),
            )
            .width(Length::Fixed(48.0))
            .height(Length::Fixed(48.0))
            .clip(true)
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    border: cosmic::iced::Border {
                        radius: cosmic.radius_m().into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
            .into()
        } else {
            widget::container(widget::icon::from_name("media-optical-cd-audio-symbolic").size(36))
                .width(Length::Fixed(48.0))
                .height(Length::Fixed(48.0))
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center)
                .into()
        };

        let fs_title: cosmic::Element<'_, NowPlayingMessage> =
            widget::text::title3(track.title.as_str())
                .wrapping(Wrapping::None)
                .class(cosmic::theme::Text::Color(BACKDROP_TEXT))
                .into();

        let mut fs_sub_parts: Vec<String> = Vec::new();
        if !track.artist.is_empty() {
            fs_sub_parts.push(track.artist.clone());
        }
        if !track.album.is_empty() {
            fs_sub_parts.push(track.album.clone());
        }
        if track.year > 0 {
            fs_sub_parts.push(track.year.to_string());
        }

        let mut info_col = widget::Column::new()
            .push(common::clipped_cell(fs_title))
            .spacing(2)
            .width(Length::Fill);
        if !fs_sub_parts.is_empty() {
            let fs_subtitle: cosmic::Element<'_, NowPlayingMessage> =
                widget::text::body(fs_sub_parts.join(" \u{2022} "))
                    .wrapping(Wrapping::None)
                    .class(cosmic::theme::Text::Color(BACKDROP_SUBTEXT))
                    .into();
            info_col = info_col.push(common::clipped_cell(fs_subtitle));
        }

        let info_row = widget::Row::new()
            .push(thumb)
            .push(info_col)
            .push(collapse_button())
            .spacing(space_s)
            .align_y(Alignment::Center);

        let transport_centered: cosmic::Element<'_, NowPlayingMessage> =
            widget::container(compact_transport_row(state, shuffle, repeat_mode, is_live))
                .align_x(Horizontal::Center)
                .width(Length::Fill)
                .into();

        let utility_full = utility_row(
            volume,
            #[cfg(feature = "visualizer")]
            visualizer_active,
            #[cfg(feature = "visualizer")]
            viz_browser_open,
            is_queue_open,
            space_xs,
            space_xxs,
        );

        let card_col = widget::Column::new()
            .push(info_row)
            .push(seek_bar_row(
                display_position,
                duration,
                progress,
                space_s,
                accent,
                is_live,
            ))
            .push(transport_centered)
            .push(utility_full)
            .spacing(space_xs);

        let card = widget::container(card_col)
            .padding(space_xs)
            .max_width(680.0)
            .class(cosmic::theme::Container::custom(|_theme| {
                cosmic::iced::widget::container::Style {
                    background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.55).into()),
                    text_color: Some(BACKDROP_TEXT),
                    border: cosmic::iced::Border {
                        radius: [16.0; 4].into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }));

        #[cfg(feature = "visualizer")]
        let card_hover_area: cosmic::Element<'_, NowPlayingMessage> = widget::mouse_area(card)
            .on_enter(NowPlayingMessage::VizHudPointerEnter)
            .on_exit(NowPlayingMessage::VizHudPointerExit)
            .into();
        #[cfg(not(feature = "visualizer"))]
        let card_hover_area: cosmic::Element<'_, NowPlayingMessage> = card.into();

        let card_layer: cosmic::Element<'_, NowPlayingMessage> = widget::container(card_hover_area)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Bottom)
            .padding(space_l)
            .into();

        if lyrics_overlay_active {
            // Bottom padding leaves clearance for the control card, which
            // docks at the screen bottom via `align_y(Vertical::Bottom)`
            // above — the lyrics layer must not sit underneath it. Lyrics
            // are content, not chrome: they stay visible/readable regardless
            // of `viz_hud_visible`, which only governs the control card.
            let lyrics_layer: cosmic::Element<'_, NowPlayingMessage> = widget::container(
                crate::views::lyrics::lyrics_overlay_view::<NowPlayingMessage>(
                    lyrics,
                    lyrics_loading,
                    position,
                    BACKDROP_TEXT,
                    BACKDROP_SUBTEXT,
                    accent,
                ),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding([space_l, space_l, 220.0, space_l])
            .into();

            if viz_hud_visible {
                Stack::new().push(lyrics_layer).push(card_layer).into()
            } else {
                lyrics_layer
            }
        } else if viz_hud_visible {
            card_layer
        } else {
            // HUD auto-hidden, no lyrics: nothing on top of the visualizer —
            // a zero-size element so it has no hit-testing footprint either.
            widget::Space::new().width(0).height(0).into()
        }
    } else {
        // --- Non-fullscreen: existing 50/50 cover-art + controls split. ---
        let cover_col: cosmic::Element<'_, NowPlayingMessage> = if let Some(handle) = cover_art {
            widget::container(
                widget::icon::icon(handle.clone())
                    .content_fit(cosmic::iced::ContentFit::Contain)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .padding(space_l)
            .width(Length::Fill)
            .height(Length::Fill)
            // iced has no length that means "percentage of the parent", so a
            // literal `min(60% height, 480px)` isn't directly expressible:
            // `Length::Fill` already shrinks the art to whatever the panel's
            // actual size is on small windows, and `max_width`/`max_height`
            // caps it from growing arbitrarily large on big displays instead.
            .max_width(480.0)
            .max_height(480.0)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                cosmic::iced::widget::container::Style {
                    border: cosmic::iced::Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: cosmic.radius_l().into(),
                    },
                    shadow: cosmic::iced::Shadow {
                        color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
                        offset: cosmic::iced::Vector::new(0.0, 8.0),
                        blur_radius: 24.0,
                    },
                    ..Default::default()
                }
            }))
            .into()
        } else {
            // No artwork for this track: pure theme surface, no hard-coded
            // scrim — matches the "no artwork" rule for the whole view.
            widget::container(widget::icon::from_name("media-optical-cd-audio-symbolic").size(160))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Center)
                .align_y(Vertical::Center)
                .class(theme_surface_class())
                .into()
        };

        let mut right_col = widget::Column::new().spacing(0);

        let top_bar = widget::Row::new()
            .push(
                widget::Space::new()
                    .width(Length::Fill)
                    .height(Length::Shrink),
            )
            .push(collapse_button())
            .align_y(Alignment::Center);
        right_col = right_col.push(top_bar);

        right_col = right_col.push(
            widget::Space::new()
                .width(Length::Shrink)
                .height(Length::Fill),
        );

        // At the narrowest supported logical width (1280px) each 50/50 panel
        // gets ~640px before padding, ~575px after; at 1920px it's ~960px /
        // ~895px. The title wraps up to 2 lines at that width and only
        // ellipsizes the 2nd line's tail if a 3rd would be needed (see
        // `Ellipsize::End` below) — `clipped_cell`'s `.clip(true)` is just a
        // defensive backstop now, not the primary truncation mechanism.
        let title_text = widget::text::title1(track.title.as_str())
            .wrapping(Wrapping::WordOrGlyph)
            .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(2)));
        let title_text: cosmic::Element<'_, NowPlayingMessage> = if has_backdrop {
            title_text
                .class(cosmic::theme::Text::Color(BACKDROP_TEXT))
                .into()
        } else {
            title_text.into()
        };
        let title_row = widget::Row::new()
            .push(common::clipped_cell(title_text))
            .push(common::favorite_button(
                track.is_favorite,
                NowPlayingMessage::ToggleFavorite(track.id.to_string()),
            ))
            .spacing(space_s)
            .align_y(Alignment::Start);
        right_col = right_col.push(title_row).push(
            widget::Space::new()
                .width(Length::Shrink)
                .height(Length::Fixed(space_xxs)),
        );

        let mut sub_parts: Vec<String> = Vec::new();
        if !track.artist.is_empty() {
            sub_parts.push(track.artist.clone());
        }
        if !track.album.is_empty() {
            sub_parts.push(track.album.clone());
        }
        if track.year > 0 {
            sub_parts.push(track.year.to_string());
        }
        if !sub_parts.is_empty() {
            let subtitle_class = if has_backdrop {
                cosmic::theme::Text::Color(BACKDROP_SUBTEXT)
            } else {
                // Pulled from the active theme's palette, not a fixed hex
                // value — still a "pure theme color" per the rule. `Custom`
                // takes a bare `fn` pointer (no captures allowed), so this
                // branch can only read its `theme` argument.
                cosmic::theme::Text::Custom(|theme| cosmic::iced::widget::text::Style {
                    color: Some(theme.cosmic().palette.neutral_7.into()),
                    ..Default::default()
                })
            };
            let subtitle_text = widget::text::body(sub_parts.join(" \u{2022} "))
                .wrapping(Wrapping::None)
                .class(subtitle_class);
            right_col = right_col.push(common::clipped_cell(subtitle_text.into()));
        }

        right_col = right_col.push(
            widget::Space::new()
                .width(Length::Shrink)
                .height(Length::Fixed(space_m)),
        );

        right_col = right_col.push(seek_bar_row(
            display_position,
            duration,
            progress,
            space_s,
            accent,
            is_live,
        ));

        right_col = right_col.push(
            widget::Space::new()
                .width(Length::Shrink)
                .height(Length::Fixed(space_s)),
        );

        right_col = right_col.push(transport_row(
            state,
            shuffle,
            repeat_mode,
            space_xxs,
            accent,
            is_live,
        ));

        right_col = right_col.push(
            widget::Space::new()
                .width(Length::Shrink)
                .height(Length::Fixed(space_s)),
        );

        right_col = right_col.push(utility_row(
            volume,
            #[cfg(feature = "visualizer")]
            visualizer_active,
            #[cfg(feature = "visualizer")]
            viz_browser_open,
            is_queue_open,
            space_xs,
            space_xxs,
        ));

        right_col = right_col.push(
            widget::Space::new()
                .width(Length::Shrink)
                .height(Length::Fill),
        );

        let mut right_panel = widget::container(right_col.width(Length::Fill))
            .padding([space_m, space_l, space_l, space_l])
            .width(Length::FillPortion(1))
            .height(Length::Fill);
        if has_backdrop {
            right_panel = right_panel.class(cosmic::theme::Container::custom(|_| {
                cosmic::iced::widget::container::Style {
                    text_color: Some(BACKDROP_TEXT),
                    ..Default::default()
                }
            }));
        }
        // Else: no override — the panel stays transparent and inherits the
        // plain theme surface painted by the outer container below.

        let (lyrics_text_color, lyrics_subtext_color) = if has_backdrop {
            (BACKDROP_TEXT, BACKDROP_SUBTEXT)
        } else {
            let theme = cosmic::theme::active();
            let cosmic = theme.cosmic();
            (cosmic.on_bg_color().into(), cosmic.palette.neutral_7.into())
        };
        let left_panel_content: cosmic::Element<'_, NowPlayingMessage> = if lyrics_overlay_active {
            crate::views::lyrics::lyrics_overlay_view::<NowPlayingMessage>(
                lyrics,
                lyrics_loading,
                position,
                lyrics_text_color,
                lyrics_subtext_color,
                accent,
            )
        } else {
            cover_col
        };

        let left_panel = widget::container(left_panel_content)
            .width(Length::FillPortion(1))
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center);

        widget::Row::new()
            .push(left_panel)
            .push(right_panel)
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    };

    #[cfg(feature = "visualizer")]
    let viz_metadata_overlay: Option<cosmic::Element<'_, NowPlayingMessage>> = if visualizer_active
        && viz_metadata_opacity > 0.0
    {
        let subtitle = {
            let mut parts = vec![track.artist.as_str()];
            if !track.album.is_empty() {
                parts.push(track.album.as_str());
            }
            parts.join(" \u{2022} ")
        };

        let overlay_col = widget::Column::new()
            .push(common::clipped_cell(
                widget::text::title3(track.title.as_str())
                    .wrapping(Wrapping::None)
                    .into(),
            ))
            .push(
                widget::text::body(subtitle).class(cosmic::theme::Text::Custom(|theme| {
                    cosmic::iced::widget::text::Style {
                        color: Some(theme.cosmic().palette.neutral_7.into()),
                        ..Default::default()
                    }
                })),
            )
            .spacing(4);

        let overlay_pill = widget::container(overlay_col).padding([12, 16]).class(
            cosmic::theme::Container::custom(move |_theme| {
                cosmic::iced::widget::container::Style {
                    background: Some(
                        cosmic::iced::Color::from_rgba(0.0, 0.0, 0.0, 0.65 * viz_metadata_opacity)
                            .into(),
                    ),
                    text_color: Some(cosmic::iced::Color::from_rgba(
                        1.0,
                        1.0,
                        1.0,
                        viz_metadata_opacity,
                    )),
                    border: cosmic::iced::Border {
                        radius: [8.0; 4].into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }),
        );

        let positioned = widget::container(overlay_pill)
            .padding([24, 0, 0, 24])
            .width(Length::Fill)
            .height(Length::Fill);

        Some(positioned.into())
    } else {
        None
    };

    #[cfg(feature = "visualizer")]
    let bg_element: Option<cosmic::Element<'_, NowPlayingMessage>> = if visualizer_active {
        let shader = cosmic::iced::widget::Shader::new(super::viz_shader::VizProgram::new(
            Arc::clone(&viz_frame_buf),
        ))
        .width(Length::Fill)
        .height(Length::Fill);
        Some(shader.into())
    } else {
        blurred_cover.map(|h| {
            let el: cosmic::Element<'_, NowPlayingMessage> = widget::icon::icon(h.clone())
                .content_fit(cosmic::iced::ContentFit::Cover)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
            el
        })
    };

    #[cfg(not(feature = "visualizer"))]
    let bg_element: Option<cosmic::Element<'_, NowPlayingMessage>> = blurred_cover.map(|h| {
        let el: cosmic::Element<'_, NowPlayingMessage> = widget::icon::icon(h.clone())
            .content_fit(cosmic::iced::ContentFit::Cover)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        el
    });

    if let Some(bg_layer) = bg_element {
        let black_base: cosmic::Element<'_, NowPlayingMessage> =
            widget::container(widget::Space::new().width(0).height(0))
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(|_theme| {
                    cosmic::iced::widget::container::Style {
                        background: Some(Color::BLACK.into()),
                        ..Default::default()
                    }
                }))
                .into();

        let mut stack_widget = Stack::new().push(black_base).push(bg_layer);

        // Uniform full-bleed scrim: both panels must darken identically, or the
        // seam between the (unscrimmed) art side and the (scrimmed) text side
        // shows as a hard vertical cliff. Skipped in fullscreen-visualizer mode,
        // which supplies its own scrim on just the control card and must stay
        // vivid/unscrimmed behind it.
        if !fullscreen_mode {
            let scrim: cosmic::Element<'_, NowPlayingMessage> =
                widget::container(widget::Space::new().width(0).height(0))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .class(cosmic::theme::Container::custom(|_theme| {
                        cosmic::iced::widget::container::Style {
                            background: Some(BACKDROP_SCRIM.into()),
                            ..Default::default()
                        }
                    }))
                    .into();
            stack_widget = stack_widget.push(scrim);
        }

        // The double-click-to-fullscreen detector is its own full-fill layer
        // pushed BENEATH the controls (never a `mouse_area` wrapping them):
        // a sibling layer captures every left press before lower layers see
        // it at all, so wrapping the controls would swallow their clicks
        // outright. As the bottom-most layer above the backdrop, it only
        // receives presses that fell all the way through `content_layer`
        // untouched — i.e. exactly the visualizer background — while every
        // button, slider and the seek bar keep responding normally.
        #[cfg(feature = "visualizer")]
        if visualizer_active {
            let dbl_click_layer: cosmic::Element<'_, NowPlayingMessage> = widget::mouse_area(
                widget::container(
                    widget::Space::new()
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .on_double_press(NowPlayingMessage::ToggleVizFullscreen)
            .into();
            stack_widget = stack_widget.push(dbl_click_layer);
        }

        stack_widget = stack_widget.push(content_layer);

        // The transient metadata pill is redundant once fullscreen — the
        // control card already surfaces title/artist/album/year.
        #[cfg(feature = "visualizer")]
        if !fullscreen_mode
            && let Some(meta_overlay) = viz_metadata_overlay
        {
            stack_widget = stack_widget.push(meta_overlay);
        }

        // Preset browser overlay: the topmost layer, above the metadata
        // pill and the fullscreen HUD card, so its own backdrop dims
        // everything else. Reachable in both fullscreen and non-fullscreen
        // viz layouts since both share this same outer Stack.
        #[cfg(feature = "visualizer")]
        if visualizer_active && viz_browser_open {
            stack_widget = stack_widget.push(super::preset_browser::preset_browser_overlay(
                viz_preset_entries,
                viz_preset_search,
                viz_current_preset_name,
                viz_locked,
                viz_beat_sensitivity,
            ));
        }

        stack_widget.width(Length::Fill).into()
    } else {
        widget::container(content_layer)
            .width(Length::Fill)
            .align_x(Horizontal::Center)
            .class(theme_surface_class())
            .into()
    }
}
