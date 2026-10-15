use crate::theme::{self, ColorExt, oklch};
use aulos_core::library::covers::CoverStore;
use aulos_core::session::{ArtistPhoto, OnlineIcon};
use egui::{
    Color32, ColorImage, Context, CornerRadius, Image, Rect, TextureHandle, TextureOptions, Ui,
    pos2,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Thumbnails uploaded to the GPU per frame: a big library otherwise stalls
/// one frame and peaks staging memory.
const UPLOADS_PER_FRAME: usize = 8;

/// Every texture the views draw, keyed by what the core calls it. Gradient
/// placeholders are built on demand and cached, so `&Covers` is enough to
/// draw (single-threaded UI only).
pub struct Covers {
    ctx: Context,
    thumbs: HashMap<String, TextureHandle>,
    /// Keys whose thumbnail awaits its (throttled) GPU upload.
    pending: Vec<String>,
    icons: HashMap<String, TextureHandle>,
    photos: HashMap<String, TextureHandle>,
    gradients: RefCell<HashMap<(Color32, Color32), TextureHandle>>,
}

fn to_image(w: u32, h: u32, rgba: &[u8]) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba)
}

impl Covers {
    pub fn new(ctx: Context) -> Self {
        Self {
            ctx,
            thumbs: HashMap::new(),
            pending: Vec::new(),
            icons: HashMap::new(),
            photos: HashMap::new(),
            gradients: RefCell::default(),
        }
    }

    /// An album cover thumbnail by `CoverArt::album_key`.
    pub fn thumb(&self, key: &str) -> Option<&TextureHandle> {
        self.thumbs.get(key)
    }
    /// A remote icon (radio station, podcast art) by URL.
    pub fn icon(&self, url: &str) -> Option<&TextureHandle> {
        self.icons.get(url)
    }
    /// An artist photo by artist name.
    pub fn photo(&self, name: &str) -> Option<&TextureHandle> {
        self.photos.get(name)
    }

    /// Queues `key` for upload; [`Self::upload_pending`] does the work.
    pub fn queue_thumb(&mut self, key: String) {
        if !self.pending.contains(&key) {
            self.pending.push(key);
        }
    }
    /// Uploads up to [`UPLOADS_PER_FRAME`] queued thumbnails taking their
    /// pixels out of `store`, and asks for another frame while more wait.
    pub fn upload_pending(&mut self, store: &mut CoverStore) {
        let n = self.pending.len().min(UPLOADS_PER_FRAME);
        for key in self.pending.drain(..n).collect::<Vec<_>>() {
            if let Some((w, h, rgba)) = store.take_pixels(&key) {
                let tex = self.ctx.load_texture(
                    format!("cover:{key}"),
                    to_image(w, h, &rgba),
                    TextureOptions::LINEAR,
                );
                self.thumbs.insert(key, tex);
            }
        }
        if !self.pending.is_empty() {
            self.ctx.request_repaint();
        }
    }
    pub fn remove_thumb(&mut self, key: &str) {
        self.thumbs.remove(key);
        self.pending.retain(|k| k != key);
    }
    pub fn clear_thumbs(&mut self) {
        self.thumbs.clear();
        self.pending.clear();
    }
    pub fn add_icon(&mut self, icon: OnlineIcon) {
        let tex = self.ctx.load_texture(
            format!("icon:{}", icon.url),
            to_image(icon.width, icon.height, &icon.rgba),
            TextureOptions::LINEAR,
        );
        self.icons.insert(icon.url, tex);
    }
    pub fn add_photo(&mut self, photo: ArtistPhoto) {
        let tex = self.ctx.load_texture(
            format!("photo:{}", photo.name),
            to_image(photo.width, photo.height, &photo.rgba),
            TextureOptions::LINEAR,
        );
        self.photos.insert(photo.name, tex);
    }
    pub fn clear_photos(&mut self) {
        self.photos.clear();
    }

    /// A cached 8×8 diagonal (top-left → bottom-right) gradient texture.
    pub fn gradient(&self, from: Color32, to: Color32) -> TextureHandle {
        let mut map = self.gradients.borrow_mut();
        if let Some(t) = map.get(&(from, to)) {
            return t.clone();
        }
        const N: usize = 16;
        let mut pixels = Vec::with_capacity(N * N);
        for y in 0..N {
            for x in 0..N {
                let t = (x + y) as f32 / (2 * (N - 1)) as f32;
                pixels.push(theme::lerp_color(from, to, t));
            }
        }
        let img = ColorImage {
            size: [N, N],
            source_size: egui::vec2(N as f32, N as f32),
            pixels,
        };
        let tex = self
            .ctx
            .load_texture("gradient", img, TextureOptions::LINEAR);
        map.insert((from, to), tex.clone());
        tex
    }

    /// The deterministic placeholder gradient for `seed` (album key, genre…).
    pub fn placeholder(&self, seed: &str) -> TextureHandle {
        let (a, b) = seed_colors(seed);
        self.gradient(a, b)
    }
}

/// Two gradient stops derived from `seed`: a cool-to-warm pair in the
/// design's oklch ranges.
pub fn seed_colors(seed: &str) -> (Color32, Color32) {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut h);
    let v = h.finish();
    let hue = (v % 360) as f32;
    let drift = 30.0 + ((v >> 16) % 50) as f32;
    (oklch(0.58, 0.09, hue), oklch(0.36, 0.07, hue + drift))
}

/// Paints a diagonal gradient over `rect`.
pub fn gradient_fill(covers: &Covers, ui: &Ui, rect: Rect, radius: u8, from: Color32, to: Color32) {
    let tex = covers.gradient(from, to);
    paint_tex(ui, rect, &tex, radius, None);
}

fn paint_tex(ui: &Ui, rect: Rect, tex: &TextureHandle, radius: u8, crop_square: Option<()>) {
    let mut img = Image::new((tex.id(), tex.size_vec2())).corner_radius(CornerRadius::same(radius));
    if crop_square.is_some() {
        let [w, h] = tex.size();
        let (w, h) = (w as f32, h as f32);
        let uv = if w > h {
            let m = (1.0 - h / w) / 2.0;
            Rect::from_min_max(pos2(m, 0.0), pos2(1.0 - m, 1.0))
        } else {
            let m = (1.0 - w / h) / 2.0;
            Rect::from_min_max(pos2(0.0, m), pos2(1.0, 1.0 - m))
        };
        img = img.uv(uv);
    }
    img.paint_at(ui, rect);
}

/// Paints a cover into `rect` (centre-cropped to the square); falls back to
/// the placeholder gradient of `seed`.
pub fn cover_image(
    ui: &Ui,
    covers: &Covers,
    rect: Rect,
    tex: Option<&TextureHandle>,
    seed: &str,
    radius: u8,
) {
    match tex {
        Some(tex) => paint_tex(ui, rect, tex, radius, Some(())),
        None => paint_tex(ui, rect, &covers.placeholder(seed), radius, None),
    }
}

/// A circular (or rounded) tile with the initials of `name` on a tinted
/// gradient, or the artist photo when present.
pub fn avatar_tile(
    ui: &Ui,
    covers: &Covers,
    rect: Rect,
    name: &str,
    photo: Option<&TextureHandle>,
    circle: bool,
) {
    let radius = if circle { 255 } else { 8 };
    if let Some(tex) = photo {
        paint_tex(ui, rect, tex, radius, Some(()));
        return;
    }
    let (a, b) = seed_colors(name);
    gradient_fill(covers, ui, rect, radius, a, b);
    let initials: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    // Design: 12px mono on small tiles (radio rows); scales on large ones.
    let size = if rect.height() <= 48.0 {
        12.0
    } else {
        (rect.height() * 0.34).clamp(11.0, 40.0)
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initials,
        theme::mono_semibold(size),
        Color32::WHITE.alpha(0.88),
    );
}
