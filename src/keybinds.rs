// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Global keyboard shortcuts.
//!
//! `cosmic::iced::keyboard::on_key_press` requires a bare `fn` pointer as
//! its mapping callback, so it cannot close over `AppModel` state.
//! [`resolve`] is therefore a pure function that turns a raw key press into
//! a self-describing [`Shortcut`]; `AppModel::update` decides what each
//! variant actually does, based on the application state the resolver
//! never gets to see (e.g. whether the library search field is active).

use cosmic::iced::keyboard::{Key, Modifiers, key::Named};

/// A global keyboard shortcut, already disambiguated from the raw key that
/// produced it. Interpreting a variant -- e.g. `Escape` closing whichever
/// overlay is topmost -- is `AppModel::update`'s job, not this module's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    PlayPause,
    Stop,
    Next,
    Previous,
    SeekForward,
    SeekBackward,
    VolumeUp,
    VolumeDown,
    Mute,
    ToggleShuffle,
    CycleRepeat,
    ToggleFavorite,
    ToggleLyrics,
    ToggleExpanded,
    /// Toggle the "Up Next" queue drawer.
    ToggleQueue,
    /// Enter/leave mini-player mode (`Ctrl+M`).
    ToggleMiniPlayer,
    FocusSearch,
    /// 1-based page number, matching the digit key pressed (`1`-`9`).
    NavPage(u8),
    Escape,
}

/// Translates a raw key press into a [`Shortcut`], or `None` when the key
/// isn't bound.
///
/// Bare-key bindings (`s`, `n`, `p`, `z`, `r`, `f`, `l`, `m`, the arrows,
/// space, `Tab`, digits, ...) are rejected outright when Ctrl, Alt, or
/// Super is held, so they never hijack a menu accelerator. `Ctrl+F` is the
/// sole exception: it doubles as the pre-existing "focus search" menu
/// shortcut, so it must still resolve to `FocusSearch` with Ctrl held.
///
/// Note that this iced revision has no `Named::Space`: the space bar
/// arrives as `Key::Character(" ")`, which is why it is matched alongside
/// the other printable bindings rather than with the named keys.
pub fn resolve(key: &Key, modifiers: Modifiers) -> Option<Shortcut> {
    let is_ctrl_f = modifiers.control()
        && !modifiers.alt()
        && !modifiers.logo()
        && matches!(key, Key::Character(c) if c.as_str().eq_ignore_ascii_case("f"));
    if is_ctrl_f {
        return Some(Shortcut::FocusSearch);
    }

    let is_ctrl_m = modifiers.control()
        && !modifiers.alt()
        && !modifiers.logo()
        && matches!(key, Key::Character(c) if c.as_str().eq_ignore_ascii_case("m"));
    if is_ctrl_m {
        return Some(Shortcut::ToggleMiniPlayer);
    }

    // Every other binding is a bare key: reject if any modifier besides
    // Shift is held (Shift is needed for e.g. `>` and `+` on most layouts).
    if modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }

    match key {
        Key::Named(Named::ArrowRight) => Some(Shortcut::SeekForward),
        Key::Named(Named::ArrowLeft) => Some(Shortcut::SeekBackward),
        Key::Named(Named::ArrowUp) => Some(Shortcut::VolumeUp),
        Key::Named(Named::ArrowDown) => Some(Shortcut::VolumeDown),
        Key::Named(Named::Tab) => Some(Shortcut::ToggleExpanded),
        Key::Named(Named::Escape) => Some(Shortcut::Escape),
        Key::Character(c) => match c.as_str() {
            " " => Some(Shortcut::PlayPause),
            "s" => Some(Shortcut::Stop),
            ">" | "n" => Some(Shortcut::Next),
            "<" | "p" => Some(Shortcut::Previous),
            "+" | "=" => Some(Shortcut::VolumeUp),
            "-" => Some(Shortcut::VolumeDown),
            "m" => Some(Shortcut::Mute),
            "z" => Some(Shortcut::ToggleShuffle),
            "r" => Some(Shortcut::CycleRepeat),
            "f" => Some(Shortcut::ToggleFavorite),
            "l" => Some(Shortcut::ToggleLyrics),
            "u" => Some(Shortcut::ToggleQueue),
            "/" => Some(Shortcut::FocusSearch),
            "1" => Some(Shortcut::NavPage(1)),
            "2" => Some(Shortcut::NavPage(2)),
            "3" => Some(Shortcut::NavPage(3)),
            "4" => Some(Shortcut::NavPage(4)),
            "5" => Some(Shortcut::NavPage(5)),
            "6" => Some(Shortcut::NavPage(6)),
            "7" => Some(Shortcut::NavPage(7)),
            "8" => Some(Shortcut::NavPage(8)),
            "9" => Some(Shortcut::NavPage(9)),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_resolves_to_play_pause() {
        assert_eq!(
            resolve(&Key::Character(" ".into()), Modifiers::empty()),
            Some(Shortcut::PlayPause)
        );
    }

    #[test]
    fn bare_letter_with_ctrl_held_is_ignored() {
        assert_eq!(resolve(&Key::Character("s".into()), Modifiers::CTRL), None);
    }

    #[test]
    fn ctrl_f_still_focuses_search() {
        assert_eq!(
            resolve(&Key::Character("f".into()), Modifiers::CTRL),
            Some(Shortcut::FocusSearch)
        );
    }

    #[test]
    fn ctrl_m_toggles_the_mini_player() {
        assert_eq!(
            resolve(&Key::Character("m".into()), Modifiers::CTRL),
            Some(Shortcut::ToggleMiniPlayer)
        );
        // Bare `m` stays mute.
        assert_eq!(
            resolve(&Key::Character("m".into()), Modifiers::empty()),
            Some(Shortcut::Mute)
        );
    }

    #[test]
    fn digit_keys_map_to_the_matching_nav_page() {
        assert_eq!(
            resolve(&Key::Character("1".into()), Modifiers::empty()),
            Some(Shortcut::NavPage(1))
        );
        assert_eq!(
            resolve(&Key::Character("5".into()), Modifiers::empty()),
            Some(Shortcut::NavPage(5))
        );
        assert_eq!(
            resolve(&Key::Character("8".into()), Modifiers::empty()),
            Some(Shortcut::NavPage(8))
        );
    }

    #[test]
    fn u_toggles_the_queue_drawer() {
        assert_eq!(
            resolve(&Key::Character("u".into()), Modifiers::empty()),
            Some(Shortcut::ToggleQueue)
        );
    }

    #[test]
    fn unbound_key_resolves_to_none() {
        assert_eq!(
            resolve(&Key::Character("q".into()), Modifiers::empty()),
            None
        );
    }
}
