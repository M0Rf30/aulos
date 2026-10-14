use super::text::{layout, paint_icon};
use crate::theme::{self, ColorExt, Theme};
use egui::{
    Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

/// Keyboard focus ring: a 2px accent outline just outside `rect`.
pub fn focus_ring(ui: &Ui, resp: &Response, rect: Rect, radius: CornerRadius) {
    if resp.has_focus() && ui.input(|i| i.any_touches() || !i.pointer.any_down()) {
        let t = Theme::get(ui.ctx());
        ui.painter().rect_stroke(
            rect.expand(1.0),
            radius,
            Stroke::new(2.0, t.accent.fill),
            StrokeKind::Outside,
        );
    }
}

/// A square icon button: transparent, tinted on hover, accent when `active`.
pub struct IconBtn<'a> {
    glyph: &'a str,
    size: f32,
    icon: f32,
    active: bool,
    fill: bool,
    dot: bool,
    enabled: bool,
    color: Option<Color32>,
    tip: Option<&'a str>,
    round: bool,
}

impl<'a> IconBtn<'a> {
    pub fn new(glyph: &'a str) -> Self {
        Self {
            glyph,
            size: 30.0,
            icon: 17.0,
            active: false,
            fill: false,
            dot: false,
            enabled: true,
            color: None,
            tip: None,
            round: false,
        }
    }
    pub fn size(mut self, v: f32) -> Self {
        self.size = v;
        self
    }
    pub fn icon_size(mut self, v: f32) -> Self {
        self.icon = v;
        self
    }
    /// Accent-tinted ("on") state.
    pub fn active(mut self, v: bool) -> Self {
        self.active = v;
        self
    }
    /// Use the filled glyph variant (`egui_phosphor::fill::*`).
    pub fn filled(mut self, v: bool) -> Self {
        self.fill = v;
        self
    }
    /// A small accent dot under the icon (shuffle/repeat "on").
    pub fn dot(mut self, v: bool) -> Self {
        self.dot = v;
        self
    }
    pub fn enabled(mut self, v: bool) -> Self {
        self.enabled = v;
        self
    }
    pub fn color(mut self, c: Color32) -> Self {
        self.color = Some(c);
        self
    }
    pub fn tip(mut self, t: &'a str) -> Self {
        self.tip = Some(t);
        self
    }
    pub fn round(mut self, v: bool) -> Self {
        self.round = v;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = Theme::get(ui.ctx());
        let (rect, mut resp) = ui.allocate_exact_size(
            Vec2::splat(self.size),
            if self.enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let radius = if self.round {
            CornerRadius::same(255)
        } else {
            CornerRadius::same(7)
        };
        let fg = if !self.enabled {
            t.pal.text_5
        } else if self.active {
            t.accent.text
        } else {
            self.color.unwrap_or(if resp.hovered() {
                t.pal.text
            } else {
                t.pal.text_2
            })
        };
        if self.active {
            ui.painter()
                .rect_filled(rect, radius, t.accent.fill.alpha(0.14));
        } else if self.enabled && resp.hovered() {
            ui.painter()
                .rect_filled(rect, radius, t.pal.text.alpha(0.07));
        }
        paint_icon(
            ui.painter(),
            rect.center(),
            self.glyph,
            self.icon,
            fg,
            self.fill,
        );
        if self.dot {
            ui.painter().circle_filled(
                pos2(rect.center().x, rect.bottom() - 4.0),
                1.75,
                t.accent.fill,
            );
        }
        if self.enabled {
            focus_ring(ui, &resp, rect, radius);
        }
        if let Some(tip) = self.tip {
            resp = resp.on_hover_text(tip);
        }
        resp
    }
}

pub fn icon_button(ui: &mut Ui, glyph: &str, size: f32) -> Response {
    IconBtn::new(glyph).size(size).show(ui)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PillKind {
    /// Inverse (text-coloured) fill.
    Primary,
    /// Cover accent fill.
    Accent,
    /// `surface_hi` fill.
    Secondary,
    /// Bordered, transparent.
    Outline,
    /// Text only, tinted on hover.
    Ghost,
    /// Red text on tint.
    Danger,
    /// Pressed toggle: strong-stroke fill with an inset hairline.
    Active,
}

/// A fully-rounded pill button of `height`. `icon` is an optional leading
/// glyph. Width follows the label unless `min_width` is wider.
pub fn pill_button(
    ui: &mut Ui,
    label: &str,
    icon: Option<&str>,
    kind: PillKind,
    height: f32,
    min_width: f32,
) -> Response {
    pill_button_enabled(ui, label, icon, kind, height, min_width, true)
}

pub fn pill_button_enabled(
    ui: &mut Ui,
    label: &str,
    icon: Option<&str>,
    kind: PillKind,
    height: f32,
    min_width: f32,
    enabled: bool,
) -> Response {
    let t = Theme::get(ui.ctx());
    let p = &t.pal;
    let font = theme::semibold(13.0);
    let galley = layout(
        ui.painter(),
        label,
        font.clone(),
        Color32::WHITE,
        400.0,
        0.0,
    );
    let icon_w = if icon.is_some() { 16.0 + 6.0 } else { 0.0 };
    let pad = (height * 0.5).clamp(10.0, 18.0);
    let w = (galley.size().x + icon_w + pad * 2.0).max(min_width);
    let (rect, resp) = ui.allocate_exact_size(
        vec2(w, height),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hov = enabled && resp.hovered();
    let (bg, fg, stroke) = match kind {
        PillKind::Primary => (if hov { p.text_1 } else { p.text }, p.bg, None),
        PillKind::Accent => (
            if hov {
                t.accent.fill.gamma_multiply(0.92)
            } else {
                t.accent.fill
            },
            t.accent.on_fill,
            None,
        ),
        PillKind::Secondary => (
            if hov { p.stroke_strong } else { p.surface_hi },
            p.text,
            None,
        ),
        PillKind::Outline => (
            if hov { p.hover } else { Color32::TRANSPARENT },
            p.text_1,
            Some(p.stroke_strong),
        ),
        PillKind::Ghost => (
            if hov {
                p.text.alpha(0.07)
            } else {
                Color32::TRANSPARENT
            },
            p.text_1,
            None,
        ),
        PillKind::Danger => (p.danger.alpha(if hov { 0.2 } else { 0.12 }), p.danger, None),
        PillKind::Active => (p.stroke_strong, p.text, Some(p.text_5)),
    };
    let (bg, fg) = if enabled {
        (bg, fg)
    } else {
        (p.surface_hi.alpha(0.5), p.text_5)
    };
    let r = CornerRadius::same((height / 2.0) as u8);
    ui.painter().rect_filled(rect, r, bg);
    if let Some(s) = stroke {
        ui.painter()
            .rect_stroke(rect, r, Stroke::new(1.0, s), StrokeKind::Inside);
    }
    let content_w = galley.size().x + icon_w;
    let mut x = rect.center().x - content_w / 2.0;
    if let Some(g) = icon {
        paint_icon(
            ui.painter(),
            pos2(x + 8.0, rect.center().y),
            g,
            16.0,
            fg,
            false,
        );
        x += icon_w;
    }
    let galley = layout(ui.painter(), label, font, fg, 400.0, 0.0);
    ui.painter()
        .galley(pos2(x, rect.center().y - galley.size().y / 2.0), galley, fg);
    if enabled {
        focus_ring(ui, &resp, rect, r);
    }
    resp
}

/// A filter chip: 28px, rounded, bordered; filled when `selected`.
/// `trailing` is an optional glyph (a dropdown caret).
pub fn chip(ui: &mut Ui, label: &str, selected: bool, trailing: Option<&str>) -> Response {
    let t = Theme::get(ui.ctx());
    let p = &t.pal;
    let font = theme::medium(13.0);
    let galley = layout(
        ui.painter(),
        label,
        font.clone(),
        Color32::WHITE,
        400.0,
        0.0,
    );
    let tw = if trailing.is_some() { 18.0 } else { 0.0 };
    let (rect, resp) =
        ui.allocate_exact_size(vec2(galley.size().x + 24.0 + tw, 28.0), Sense::click());
    let (bg, fg, bd) = if selected {
        (p.text, p.bg, p.text)
    } else if resp.hovered() {
        (p.hover, p.text, p.stroke_strong)
    } else {
        (Color32::TRANSPARENT, p.text_1, p.stroke)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(14), bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(14),
        Stroke::new(1.0, bd),
        StrokeKind::Inside,
    );
    focus_ring(ui, &resp, rect, CornerRadius::same(14));
    let galley = layout(ui.painter(), label, font, fg, 400.0, 0.0);
    ui.painter().galley(
        pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    if let Some(g) = trailing {
        paint_icon(
            ui.painter(),
            pos2(rect.right() - 14.0, rect.center().y),
            g,
            12.0,
            fg,
            false,
        );
    }
    resp
}

/// A segmented control; items are labels or icon glyphs. Returns true when
/// the selection changed.
pub fn segmented(ui: &mut Ui, selected: &mut usize, items: &[&str], height: f32) -> bool {
    let t = Theme::get(ui.ctx());
    let p = &t.pal;
    let font = theme::medium(12.5);
    let widths: Vec<f32> = items
        .iter()
        .map(|s| {
            if super::text::is_icon(s) {
                height
            } else {
                layout(ui.painter(), s, font.clone(), Color32::WHITE, 300.0, 0.0)
                    .size()
                    .x
                    + 24.0
            }
        })
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, height + 4.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(8), p.surface_2);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, p.stroke),
        StrokeKind::Inside,
    );
    let mut changed = false;
    let mut x = rect.left() + 2.0;
    for (i, (item, w)) in items.iter().zip(&widths).enumerate() {
        let cell = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(*w, height));
        let resp = ui.interact(cell, ui.id().with(("seg", i, item)), Sense::click());
        let sel = *selected == i;
        if sel {
            ui.painter()
                .rect_filled(cell, CornerRadius::same(6), p.seg_sel);
        } else if resp.hovered() {
            ui.painter()
                .rect_filled(cell, CornerRadius::same(6), p.text.alpha(0.05));
        }
        let fg = if sel { p.text } else { p.text_2 };
        if super::text::is_icon(item) {
            paint_icon(ui.painter(), cell.center(), item, 15.0, fg, false);
        } else {
            ui.painter()
                .text(cell.center(), Align2::CENTER_CENTER, item, font.clone(), fg);
        }
        if resp.clicked() && !sel {
            *selected = i;
            changed = true;
        }
        x += *w;
    }
    changed
}

/// An on/off switch (34×20).
pub fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let t = Theme::get(ui.ctx());
    let (rect, mut resp) = ui.allocate_exact_size(vec2(34.0, 20.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let k = ui
        .ctx()
        .animate_bool_with_time(resp.id, *on, theme::motion::HOVER);
    let bg = theme::lerp_color(t.pal.stroke_strong, t.accent.fill, k);
    ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
    let knob = if *on { t.accent.on_fill } else { t.pal.text_1 };
    let x = rect.left() + 10.0 + 14.0 * k;
    ui.painter()
        .circle_filled(pos2(x, rect.center().y), 7.0, knob);
    resp
}
