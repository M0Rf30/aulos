//! Shared widgets, painted to match the design's measurements.
//!
//! Every widget reads the palette/accent through [`Theme::get`], so callers
//! never thread colours around.

mod badge;
mod bar;
mod button;
mod cover;
mod input;
mod states;
mod text;
mod track_row;

pub use badge::{keycap, quality_badge, tag_badge};
pub use bar::{seek_bar, slider_bar};
pub use button::{
    IconBtn, PillKind, chip, icon_button, pill_button, pill_button_enabled, segmented, toggle,
};
pub use cover::{Covers, avatar_tile, cover_image, gradient_fill, more_trigger, seed_colors};
pub use input::{search_field, text_field};
pub use states::{empty_state, empty_state_actions, empty_state_tinted, skeleton};
pub use text::{
    caps_label, label_text, marquee_id, marquee_text, paint_icon, paint_text, paint_text_spaced,
    text_width, truncated_runs,
};
pub use track_row::{
    Columns, HEADER_HEIGHT, ROW_HEIGHT, track_header, track_header_sortable, track_row,
};
