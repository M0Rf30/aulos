//! Aulos theme: design tokens, palette, cover-derived accent, fonts and the
//! egui `Style`. Values are taken from the "Aulos Window" design.

use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke};
use std::sync::Arc;

pub mod sp {
    pub const S3: f32 = 6.0;
    pub const S4: f32 = 8.0;
    pub const S6: f32 = 12.0;
    pub const S9: f32 = 24.0;
    pub const S10: f32 = 32.0;
}

pub mod radius {
    pub const XS: u8 = 3;
    pub const SM: u8 = 6;
    pub const MD: u8 = 8;
    pub const LG: u8 = 10;
    pub const XL: u8 = 12;
}

pub mod size {
    pub const SIDEBAR: f32 = 245.0;
    pub const RAIL: f32 = 65.0;
    pub const COLLAPSE_BELOW: f32 = 1040.0;
    pub const TOOLBAR: f32 = 56.0;
    pub const NOW_BAR: f32 = 76.0;
}

pub mod motion {
    pub const HOVER: f32 = 0.08;
    pub const ACCENT: f32 = 0.40;
    pub const TOAST_LIFE: f32 = 5.0;
    /// Sidebar collapse and queue / properties panel slide.
    pub const PANEL: f32 = 0.18;
    /// Expanded-view crossfades (cover, backdrop).
    pub const CROSSFADE: f32 = 0.24;
    pub const CROSSFADE_SLOW: f32 = 0.60;

    const REDUCE_KEY: &str = "reduce_motion";

    /// Published once a frame from the config.
    pub fn set_reduced(ctx: &egui::Context, reduce: bool) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(REDUCE_KEY), reduce));
    }

    pub fn reduced(ctx: &egui::Context) -> bool {
        ctx.data(|d| d.get_temp(egui::Id::new(REDUCE_KEY)))
            .unwrap_or(false)
    }

    /// 0 → 1 over [`PANEL`] each time the sheet `id` is (re)opened; call with
    /// `open == false` on frames it is closed to rearm.
    pub fn slide_in(ctx: &egui::Context, id: egui::Id, open: bool) -> f32 {
        let armed = ctx
            .data(|d| d.get_temp::<bool>(id.with("armed")))
            .unwrap_or(false);
        if open && !armed {
            ctx.animate_bool_with_time(id, false, 0.0);
            ctx.data_mut(|d| d.insert_temp(id.with("armed"), true));
            ctx.request_repaint();
        } else if !open && armed {
            ctx.data_mut(|d| d.insert_temp(id.with("armed"), false));
        }
        animate(ctx, id, open, PANEL)
    }

    /// `animate_bool_with_time` that snaps when reduced motion is on.
    pub fn animate(ctx: &egui::Context, id: egui::Id, on: bool, secs: f32) -> f32 {
        ctx.animate_bool_with_time(id, on, if reduced(ctx) { 0.0 } else { secs })
    }
}

pub const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Colour helpers shared by the views.
pub trait ColorExt {
    /// The colour at `a` (0..=1) of its own opacity.
    fn alpha(self, a: f32) -> Color32;
}

impl ColorExt for Color32 {
    fn alpha(self, a: f32) -> Color32 {
        Color32::from_rgba_unmultiplied(
            self.r(),
            self.g(),
            self.b(),
            (a.clamp(0.0, 1.0) * 255.0).round() as u8,
        )
    }
}

/// Neutral colours of one theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub bg_sidebar: Color32,
    pub bg_bar: Color32,
    /// Cards, sheets, dialogs.
    pub surface_1: Color32,
    /// Search field, toggle groups.
    pub surface_2: Color32,
    /// Menus.
    pub surface_3: Color32,
    /// Selected nav item / row.
    pub surface_sel: Color32,
    /// Neutral buttons.
    pub surface_hi: Color32,
    /// Segmented-control selected cell.
    pub seg_sel: Color32,
    /// Hovered row.
    pub hover: Color32,
    /// Selected row in lists.
    pub row_sel: Color32,
    pub stroke: Color32,
    pub stroke_strong: Color32,
    /// Input borders.
    pub input_bd: Color32,
    /// Progress/slider rail.
    pub track: Color32,
    pub text: Color32,
    /// Slightly dimmer than `text` (`#D6D2CA`).
    pub text_1: Color32,
    /// `#C2BDB4`.
    pub text_15: Color32,
    pub text_2: Color32,
    pub text_3: Color32,
    /// `#6A665F`: hints, keycaps.
    pub text_4: Color32,
    /// `#4A4642`: disabled glyphs.
    pub text_5: Color32,
    /// Fixed brand accent for empty states and scan progress.
    pub reed: Color32,
    pub danger: Color32,
    pub warn: Color32,
    pub ok: Color32,
}

pub const DARK: Palette = Palette {
    dark: true,
    bg: hex(0x141312),
    bg_sidebar: hex(0x0F0E0D),
    bg_bar: hex(0x100F0E),
    surface_1: hex(0x1A1917),
    surface_2: hex(0x1C1B19),
    surface_3: hex(0x252321),
    surface_sel: hex(0x262421),
    surface_hi: hex(0x2A2826),
    seg_sel: hex(0x2F2D2A),
    hover: hex(0x1E1C1A),
    row_sel: hex(0x2A2724),
    stroke: hex(0x2A2826),
    stroke_strong: hex(0x3A3734),
    input_bd: hex(0x34312D),
    track: hex(0x2E2B28),
    text: hex(0xEDEAE4),
    text_1: hex(0xD6D2CA),
    text_15: hex(0xC2BDB4),
    text_2: hex(0xA8A39A),
    text_3: hex(0x888378),
    text_4: hex(0x6A665F),
    text_5: hex(0x4A4642),
    reed: hex(0xE07A5F),
    danger: hex(0xE5675A),
    warn: hex(0xE3B14F),
    ok: hex(0x6DBE8A),
};

pub const LIGHT: Palette = Palette {
    dark: false,
    bg: hex(0xF7F5F1),
    bg_sidebar: hex(0xEFECE6),
    bg_bar: hex(0xFBFAF7),
    surface_1: hex(0xFFFFFF),
    surface_2: hex(0xF1EEE9),
    surface_3: hex(0xFFFFFF),
    surface_sel: hex(0xE6E2DB),
    surface_hi: hex(0xE9E5DE),
    seg_sel: hex(0xFFFFFF),
    hover: hex(0xEFECE6),
    row_sel: hex(0xE6E2DB),
    stroke: hex(0xE2DED7),
    stroke_strong: hex(0xCFCAC1),
    input_bd: hex(0xCFCAC1),
    track: hex(0xDDD8D0),
    text: hex(0x1A1917),
    text_1: hex(0x3B3833),
    text_15: hex(0x3B3833),
    text_2: hex(0x5E5A53),
    text_3: hex(0x77726A),
    text_4: hex(0x8F8A81),
    text_5: hex(0xBDB8AF),
    reed: hex(0xB4543C),
    danger: hex(0xB83A2E),
    warn: hex(0x9A6B12),
    ok: hex(0x2F7D4E),
};

/// How a track's/album's audio quality is labelled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    Lossy,
    Cd,
    HiRes,
    Dsd,
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Self::Lossy => "LOSSY",
            Self::Cd => "CD",
            Self::HiRes => "HI-RES",
            Self::Dsd => "DSD",
        }
    }

    pub fn fg(self, dark: bool) -> Color32 {
        match (self, dark) {
            (Self::Lossy, true) => hex(0xBDB8AF),
            (Self::Lossy, false) => hex(0x5E5A53),
            (Self::Cd, true) => hex(0x9DBDF0),
            (Self::Cd, false) => hex(0x2F5DA8),
            (Self::HiRes, true) => hex(0xE7C27D),
            (Self::HiRes, false) => hex(0x8A5F12),
            (Self::Dsd, true) => hex(0xC9AEF2),
            (Self::Dsd, false) => hex(0x6B44B0),
        }
    }

    pub fn bg(self, dark: bool) -> Color32 {
        self.fg(dark).alpha(if dark { 0.11 } else { 0.09 })
    }

    pub fn border(self, dark: bool) -> Color32 {
        self.fg(dark).alpha(0.30)
    }
}

/// Cover-derived accent after normalisation (see the design's accent rules).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Accent {
    /// Progress, play button, toggles.
    pub fill: Color32,
    /// Glyphs on `fill`.
    pub on_fill: Color32,
    /// The accent as text on the page background.
    pub text: Color32,
    /// Current-row background.
    pub row_tint: Color32,
}

/// The accent of the design's reference screens (Reed), used until a cover
/// provides one.
pub const DEFAULT_ACCENT_RGB: Color32 = hex(0xE07A5F);

impl Accent {
    /// Normalises a cover colour (`None`: nothing playing or the cover has no
    /// dominant hue) for `pal`.
    pub fn from_cover(color: Option<Color32>, pal: &Palette) -> Self {
        let fill = match color.map(oklch_of) {
            Some((l, c, h)) if c >= 0.035 => {
                let (lo, hi) = if pal.dark { (0.70, 0.82) } else { (0.48, 0.56) };
                from_oklch(l.clamp(lo, hi), c.min(0.15), h)
            }
            _ => {
                if pal.dark {
                    DEFAULT_ACCENT_RGB
                } else {
                    hex(0xB4543C)
                }
            }
        };
        Self::from_fill(fill, pal)
    }

    pub fn default_for(pal: &Palette) -> Self {
        Self::from_cover(None, pal)
    }

    fn from_fill(fill: Color32, pal: &Palette) -> Self {
        let dark_ink = hex(0x06161B);
        let on_fill = if contrast(Color32::WHITE, fill) > contrast(dark_ink, fill) {
            Color32::WHITE
        } else {
            dark_ink
        };
        let (l, c, h) = oklch_of(fill);
        let text = if pal.dark {
            from_oklch(l.max(0.84), c, h)
        } else {
            from_oklch(l.min(0.46), c, h)
        };
        Self {
            fill,
            on_fill,
            text,
            row_tint: fill.alpha(0.10),
        }
    }

    pub fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        let m = |x: Color32, y: Color32| lerp_color(x, y, t);
        Self {
            fill: m(a.fill, b.fill),
            on_fill: if t < 0.5 { a.on_fill } else { b.on_fill },
            text: m(a.text, b.text),
            row_tint: m(a.row_tint, b.row_tint),
        }
    }
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        f(a.r(), b.r()),
        f(a.g(), b.g()),
        f(a.b(), b.b()),
        f(a.a(), b.a()),
    )
}

fn srgb_to_lin(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn lin_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round() as u8
}

/// `(lightness, chroma, hue in radians)`.
pub fn oklch_of(c: Color32) -> (f32, f32, f32) {
    let (r, g, b) = (srgb_to_lin(c.r()), srgb_to_lin(c.g()), srgb_to_lin(c.b()));
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    let ll = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let bb = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    (ll, (a * a + bb * bb).sqrt(), bb.atan2(a))
}

pub fn from_oklch(l: f32, c: f32, h: f32) -> Color32 {
    let (a, b) = (c * h.cos(), c * h.sin());
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    Color32::from_rgb(
        lin_to_srgb(4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_93 * s_),
        lin_to_srgb(-1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_4 * s_),
        lin_to_srgb(-0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_),
    )
}

/// `oklch(l c h°)` with the hue in degrees, as the design writes them.
pub fn oklch(l: f32, c: f32, h_deg: f32) -> Color32 {
    from_oklch(l, c, h_deg.to_radians())
}

fn luminance(c: Color32) -> f32 {
    0.2126 * srgb_to_lin(c.r()) + 0.7152 * srgb_to_lin(c.g()) + 0.0722 * srgb_to_lin(c.b())
}

pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

// ------------------------------------------------------------------ fonts

pub const FAMILY_REGULAR: &str = "geist-regular";
pub const FAMILY_MEDIUM: &str = "geist-medium";
pub const FAMILY_SEMIBOLD: &str = "geist-semibold";
pub const FAMILY_BOLD: &str = "geist-bold";
pub const FAMILY_MONO: &str = "geist-mono";
pub const FAMILY_MONO_SEMIBOLD: &str = "geist-mono-semibold";
pub const FAMILY_ICON_FILL: &str = "phosphor-fill";

pub fn install_fonts(ctx: &egui::Context) {
    let mut f = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        f.font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add(
        FAMILY_REGULAR,
        include_bytes!("../assets/fonts/Geist-Regular.ttf"),
    );
    add(
        FAMILY_MEDIUM,
        include_bytes!("../assets/fonts/Geist-Medium.ttf"),
    );
    add(
        FAMILY_SEMIBOLD,
        include_bytes!("../assets/fonts/Geist-SemiBold.ttf"),
    );
    add(
        FAMILY_BOLD,
        include_bytes!("../assets/fonts/Geist-Bold.ttf"),
    );
    add(
        FAMILY_MONO,
        include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
    );
    add(
        FAMILY_MONO_SEMIBOLD,
        include_bytes!("../assets/fonts/GeistMono-SemiBold.ttf"),
    );
    // Geist first, egui's defaults (symbols, emoji) as fallbacks.
    f.families
        .get_mut(&FontFamily::Proportional)
        .expect("proportional")
        .insert(0, FAMILY_REGULAR.into());
    f.families
        .get_mut(&FontFamily::Monospace)
        .expect("monospace")
        .insert(0, FAMILY_MONO.into());
    egui_phosphor::add_to_fonts(&mut f, egui_phosphor::Variant::Regular);
    egui_phosphor::add_font_bytes_as_family(
        &mut f,
        FAMILY_ICON_FILL,
        egui_phosphor::Variant::Fill.font_bytes(),
    );
    let fallbacks = f.families[&FontFamily::Proportional].clone();
    for name in [
        FAMILY_MEDIUM,
        FAMILY_SEMIBOLD,
        FAMILY_BOLD,
        FAMILY_MONO_SEMIBOLD,
    ] {
        let mut keys = fallbacks.clone();
        keys.retain(|k| k != FAMILY_REGULAR);
        keys.insert(0, name.into());
        f.families.insert(FontFamily::Name(name.into()), keys);
    }
    ctx.set_fonts(f);
}

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}
pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_MEDIUM.into()))
}
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_SEMIBOLD.into()))
}
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_BOLD.into()))
}
/// Geist Mono Medium (times, counts, paths).
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}
pub fn mono_semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_MONO_SEMIBOLD.into()))
}
/// A regular-weight Phosphor glyph.
pub fn icon(size: f32) -> FontId {
    FontId::proportional(size)
}
/// A filled Phosphor glyph (use the `egui_phosphor::fill::*` constants).
pub fn icon_fill(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_ICON_FILL.into()))
}

// ------------------------------------------------------------------ style

pub fn apply(ctx: &egui::Context, pal: &Palette) {
    use egui::TextStyle;
    use egui::style::{HandleShape, Selection, WidgetVisuals, Widgets};
    ctx.set_theme(if pal.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, semibold(26.0)),
            (TextStyle::Body, regular(13.0)),
            (TextStyle::Button, regular(13.0)),
            (TextStyle::Small, regular(12.0)),
            (TextStyle::Monospace, mono(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(sp::S4, sp::S3);
        s.spacing.button_padding = egui::vec2(sp::S6, sp::S3);
        s.spacing.interact_size = egui::vec2(40.0, 28.0);
        s.spacing.menu_margin = Margin::same(5);
        s.spacing.window_margin = Margin::same(16);
        s.spacing.scroll.bar_width = 6.0;
        s.spacing.scroll.floating = true;
        s.spacing.slider_rail_height = 4.0;
        s.interaction.tooltip_delay = 0.4;
        s.animation_time = motion::HOVER;
        s.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);

        let v = &mut s.visuals;
        v.dark_mode = pal.dark;
        v.override_text_color = None;
        v.panel_fill = pal.bg;
        v.window_fill = pal.surface_1;
        v.extreme_bg_color = pal.surface_2;
        v.faint_bg_color = pal.hover;
        v.code_bg_color = pal.surface_2;
        v.hyperlink_color = pal.text;
        v.warn_fg_color = pal.warn;
        v.error_fg_color = pal.danger;
        v.window_stroke = Stroke::new(1.0, pal.stroke_strong);
        v.window_corner_radius = CornerRadius::same(radius::XL);
        v.menu_corner_radius = CornerRadius::same(radius::LG);
        v.window_shadow = Shadow {
            offset: [0, 16],
            blur: 40,
            spread: 0,
            color: Color32::from_black_alpha(128),
        };
        v.popup_shadow = v.window_shadow;
        v.selection = Selection {
            bg_fill: pal.surface_sel,
            stroke: Stroke::new(1.0, pal.text),
        };
        v.text_cursor.stroke = Stroke::new(1.5, pal.text);
        v.slider_trailing_fill = true;
        v.handle_shape = HandleShape::Circle;

        let w = |bg: Color32, stroke: Color32, fg: Color32| WidgetVisuals {
            bg_fill: bg,
            weak_bg_fill: bg,
            bg_stroke: Stroke::new(1.0, stroke),
            corner_radius: CornerRadius::same(radius::SM),
            fg_stroke: Stroke::new(1.0, fg),
            expansion: 0.0,
        };
        v.widgets = Widgets {
            noninteractive: w(pal.bg, pal.stroke, pal.text),
            inactive: w(pal.surface_hi, Color32::TRANSPARENT, pal.text),
            hovered: w(pal.stroke_strong, Color32::TRANSPARENT, pal.text),
            active: w(pal.surface_sel, Color32::TRANSPARENT, pal.text),
            open: w(pal.surface_3, pal.stroke_strong, pal.text),
        };
    });
}

/// Drop shadows used by the design.
pub fn shadow(offset: [i8; 2], blur: u8, alpha: u8) -> Shadow {
    Shadow {
        offset,
        blur,
        spread: 0,
        color: Color32::from_black_alpha(alpha),
    }
}

// ------------------------------------------------------------------ context

/// The palette and (animated) accent in effect this frame, published by the
/// app so any widget can read them from its `Ui`/`Context`.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub pal: Palette,
    pub accent: Accent,
}

impl Theme {
    pub fn publish(self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::NULL, self));
    }

    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(egui::Id::NULL))
            .unwrap_or(Self {
                pal: DARK,
                accent: Accent::default_for(&DARK),
            })
    }
}
