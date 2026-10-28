use crate::theme::{self, ColorExt, oklch};
use aulos_core::library::covers::CoverStore;
use aulos_core::session::{ArtistPhoto, OnlineIcon};
use egui::{
    Color32, ColorImage, Context, CornerRadius, Image, Rect, TextureHandle, TextureOptions, Ui,
    pos2,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Thumbnails uploaded to the GPU per frame: a big library otherwise stalls
/// one frame and peaks staging memory.
const UPLOADS_PER_FRAME: usize = 8;
/// A thumbnail not drawn for this many frames is released from the GPU.
const IDLE_FRAMES: u64 = 240;
/// GPU bytes of thumbnails kept; the least recently drawn go first.
const RESIDENT_BUDGET: usize = 48 << 20;

/// A remote picture (icon, artist photo): its texture and the pixels, kept
/// so the texture can be made again for a new window's context.
struct Remote {
    tex: TextureHandle,
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    image: Arc<ColorImage>,
}

impl Remote {
    fn new(ctx: &Context, name: String, image: ColorImage) -> Self {
        let image = Arc::new(image);
        let tex = ctx.load_texture(name, Arc::clone(&image), TextureOptions::LINEAR);
        Self {
            tex,
            #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
            image,
        }
    }
}

struct Thumb {
    tex: TextureHandle,
    last_used: Cell<u64>,
    bytes: usize,
}

/// A remote icon (radio station, podcast art). The pixels stay, the texture
/// is only held while the icon is drawn: it uploads on first draw and is
/// released after [`IDLE_FRAMES`] unseen, then uploads again when needed.
struct Icon {
    image: Arc<ColorImage>,
    tex: Option<TextureHandle>,
    last_used: Cell<u64>,
}

/// A cached gradient texture and when it was last drawn.
struct Gradient {
    tex: TextureHandle,
    last_used: Cell<u64>,
}

/// Every texture the views draw, keyed by what the core calls it. Gradient
/// placeholders are built on demand and cached, so `&Covers` is enough to
/// draw (single-threaded UI only).
pub struct Covers {
    ctx: Context,
    /// Resident thumbnail textures: only what was drawn recently.
    thumbs: HashMap<String, Thumb>,
    /// Keys the core has a thumbnail for; a miss in `thumbs` on one of
    /// these is a request to (re)upload it.
    known: HashSet<String>,
    /// Keys drawn without a resident texture, awaiting a throttled upload.
    wanted: RefCell<Vec<String>>,
    /// Frames seen, as the clock for `Thumb::last_used`.
    frame: Cell<u64>,
    /// GPU bytes held by `thumbs`.
    resident: usize,
    icons: HashMap<String, Icon>,
    /// URLs of icons drawn without a texture, awaiting a throttled upload.
    wanted_icons: RefCell<HashSet<String>>,
    photos: HashMap<String, Remote>,
    /// Placeholder and fill gradients; the ones idle too long are released.
    gradients: RefCell<HashMap<(Color32, Color32), Gradient>>,
}

fn to_image(w: u32, h: u32, rgba: &[u8]) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba)
}

impl Covers {
    pub fn new(ctx: Context) -> Self {
        Self {
            ctx,
            thumbs: HashMap::new(),
            known: HashSet::new(),
            wanted: RefCell::default(),
            frame: Cell::new(0),
            resident: 0,
            icons: HashMap::new(),
            wanted_icons: RefCell::default(),
            photos: HashMap::new(),
            gradients: RefCell::default(),
        }
    }

    /// An album cover thumbnail by `CoverArt::album_key`.
    pub fn thumb(&self, key: &str) -> Option<&TextureHandle> {
        match self.thumbs.get(key) {
            Some(thumb) => {
                thumb.last_used.set(self.frame.get());
                Some(&thumb.tex)
            }
            None => {
                if self.known.contains(key) {
                    let mut wanted = self.wanted.borrow_mut();
                    if !wanted.iter().any(|k| k == key) {
                        wanted.push(key.to_owned());
                        self.ctx.request_repaint();
                    }
                }
                None
            }
        }
    }
    /// A remote icon (radio station, podcast art) by URL.
    pub fn icon(&self, url: &str) -> Option<&TextureHandle> {
        let icon = self.icons.get(url)?;
        if let Some(tex) = &icon.tex {
            icon.last_used.set(self.frame.get());
            return Some(tex);
        }
        if self.wanted_icons.borrow_mut().insert(url.to_owned()) {
            self.ctx.request_repaint();
        }
        None
    }
    /// An artist photo by artist name.
    pub fn photo(&self, name: &str) -> Option<&TextureHandle> {
        self.photos.get(name).map(|photo| &photo.tex)
    }

    /// The core has a new or changed thumbnail for `key`: forget the old
    /// texture; the new one uploads when it is first drawn.
    pub fn queue_thumb(&mut self, key: String) {
        self.drop_texture(&key);
        self.known.insert(key);
    }
    pub fn remove_thumb(&mut self, key: &str) {
        self.drop_texture(key);
        self.known.remove(key);
        self.wanted.borrow_mut().retain(|k| k != key);
    }
    pub fn clear_thumbs(&mut self) {
        self.thumbs.clear();
        self.known.clear();
        self.wanted.borrow_mut().clear();
        self.resident = 0;
    }
    fn drop_texture(&mut self, key: &str) {
        if let Some(old) = self.thumbs.remove(key) {
            self.resident -= old.bytes;
        }
    }

    /// Once per frame: uploads up to [`UPLOADS_PER_FRAME`] of the thumbnails
    /// drawn without a texture (decoding them from the core's compressed
    /// copy), then releases the ones idle too long or over the byte budget.
    pub fn upload_pending(&mut self, store: &CoverStore) {
        let frame = self.frame.get() + 1;
        self.frame.set(frame);
        let batch: Vec<String> = {
            let mut wanted = self.wanted.borrow_mut();
            let n = wanted.len().min(UPLOADS_PER_FRAME);
            wanted.drain(..n).collect()
        };
        for key in batch {
            let Some((w, h, rgba)) = store.get(&key).and_then(|t| t.decode()) else {
                self.known.remove(&key);
                continue;
            };
            let tex = self.ctx.load_texture(
                format!("cover:{key}"),
                to_image(w, h, &rgba),
                TextureOptions::LINEAR,
            );
            let bytes = w as usize * h as usize * 4;
            self.resident += bytes;
            let thumb = Thumb {
                tex,
                last_used: Cell::new(frame),
                bytes,
            };
            if let Some(old) = self.thumbs.insert(key, thumb) {
                self.resident -= old.bytes;
            }
        }
        let icon_batch: Vec<String> = self
            .wanted_icons
            .get_mut()
            .iter()
            .take(UPLOADS_PER_FRAME)
            .cloned()
            .collect();
        for url in icon_batch {
            self.wanted_icons.get_mut().remove(&url);
            if let Some(icon) = self.icons.get_mut(&url) {
                icon.tex = Some(self.ctx.load_texture(
                    format!("icon:{url}"),
                    Arc::clone(&icon.image),
                    TextureOptions::LINEAR,
                ));
                icon.last_used.set(frame);
            }
        }
        if !self.wanted.borrow().is_empty() || !self.wanted_icons.borrow().is_empty() {
            self.ctx.request_repaint();
        }
        self.evict(frame);
    }

    fn evict(&mut self, frame: u64) {
        let idle = |last_used: u64| frame.saturating_sub(last_used) > IDLE_FRAMES;
        for icon in self.icons.values_mut() {
            if icon.tex.is_some() && idle(icon.last_used.get()) {
                icon.tex = None;
            }
        }
        self.gradients
            .get_mut()
            .retain(|_, g| !idle(g.last_used.get()));
        let mut resident = self.resident;
        self.thumbs.retain(|_, t| {
            let idle = frame.saturating_sub(t.last_used.get()) > IDLE_FRAMES;
            if idle {
                resident -= t.bytes;
            }
            !idle
        });
        if resident > RESIDENT_BUDGET {
            let mut by_age: Vec<(u64, String)> = self
                .thumbs
                .iter()
                .filter(|(_, t)| t.last_used.get() + 1 < frame)
                .map(|(k, t)| (t.last_used.get(), k.clone()))
                .collect();
            by_age.sort_unstable();
            for (_, key) in by_age {
                if resident <= RESIDENT_BUDGET {
                    break;
                }
                if let Some(t) = self.thumbs.remove(&key) {
                    resident -= t.bytes;
                }
            }
        }
        self.resident = resident;
    }
    pub fn add_icon(&mut self, icon: OnlineIcon) {
        let entry = Icon {
            image: Arc::new(to_image(icon.width, icon.height, &icon.rgba)),
            tex: None,
            last_used: Cell::new(self.frame.get()),
        };
        self.icons.insert(icon.url, entry);
    }
    pub fn add_photo(&mut self, photo: ArtistPhoto) {
        let image = to_image(photo.width, photo.height, &photo.rgba);
        let remote = Remote::new(&self.ctx, format!("photo:{}", photo.name), image);
        self.photos.insert(photo.name, remote);
    }
    pub fn clear_photos(&mut self) {
        self.photos.clear();
    }

    /// Moves every texture to another egui context (a window closed to the
    /// tray and opened again has a new one). Thumbnails are not carried
    /// over: the views ask for the ones they draw and they upload again
    /// from the core's copy.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub fn rebind(&mut self, ctx: Context) {
        self.thumbs.clear();
        self.resident = 0;
        self.wanted.borrow_mut().clear();
        self.gradients.borrow_mut().clear();
        self.wanted_icons.borrow_mut().clear();
        for icon in self.icons.values_mut() {
            icon.tex = None;
        }
        for (name, photo) in &mut self.photos {
            photo.tex = ctx.load_texture(
                format!("photo:{name}"),
                Arc::clone(&photo.image),
                TextureOptions::LINEAR,
            );
        }
        self.ctx = ctx;
    }

    /// A cached 8×8 diagonal (top-left → bottom-right) gradient texture.
    pub fn gradient(&self, from: Color32, to: Color32) -> TextureHandle {
        let mut map = self.gradients.borrow_mut();
        if let Some(g) = map.get(&(from, to)) {
            g.last_used.set(self.frame.get());
            return g.tex.clone();
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
        let entry = Gradient {
            tex: tex.clone(),
            last_used: Cell::new(self.frame.get()),
        };
        map.insert((from, to), entry);
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

/// The hover-only "…" circle in the top-right corner of a card's `art`
/// rect. Interact it after the card's own response so it wins hit-testing;
/// `card_hovered` is the card's hover state (the circle also shows while it is
/// itself hovered).
pub fn more_trigger(
    ui: &Ui,
    id: egui::Id,
    art: Rect,
    card_hovered: bool,
    inset: f32,
) -> egui::Response {
    let rect = Rect::from_center_size(
        pos2(art.right() - inset, art.top() + inset),
        egui::vec2(28.0, 28.0),
    );
    let more = ui.interact(rect, id.with("more"), egui::Sense::click());
    if card_hovered || more.hovered() {
        ui.painter().circle_filled(
            rect.center(),
            14.0,
            Color32::from_black_alpha(if more.hovered() { 170 } else { 120 }),
        );
        super::paint_icon(
            ui.painter(),
            rect.center(),
            egui_phosphor::regular::DOTS_THREE,
            17.0,
            Color32::WHITE,
            false,
        );
    }
    more
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
