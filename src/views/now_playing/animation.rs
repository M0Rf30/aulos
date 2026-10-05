// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Expand/collapse animation state and easing functions.

/// Cubic ease-out: fast start, gentle end. Used for expansion.
pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Cubic ease-in: gentle start, fast end. Used for collapse.
pub fn ease_in(t: f32) -> f32 {
    t.powi(3)
}

/// Animation duration in milliseconds.
pub const ANIMATION_DURATION_MS: f32 = 250.0;

/// Interpolate between `from` and `to` using the given progress `t` (0.0–1.0).
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}
