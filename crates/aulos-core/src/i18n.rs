// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Provides localization support for this crate.

use i18n_embed::{
    DefaultLocalizer, LanguageLoader, Localizer,
    fluent::{FluentLanguageLoader, fluent_language_loader},
    unic_langid::LanguageIdentifier,
};
use rust_embed::RustEmbed;
use std::sync::LazyLock;

/// Applies the requested language(s) to requested translations from the `fl!()` macro.
pub fn init(requested_languages: &[LanguageIdentifier]) {
    if let Err(why) = localizer().select(requested_languages) {
        eprintln!("error while loading fluent localizations: {why}");
    }
    // Newly loaded bundles start with isolation marks; see `LANGUAGE_LOADER`.
    LANGUAGE_LOADER.set_use_isolating(false);
}

/// [`init`] with the desktop's preferred languages (locale environment
/// variables, or the settings portal where there is one), or the browser's
/// `navigator.languages`.
pub fn init_from_system() {
    #[cfg(not(target_arch = "wasm32"))]
    let requested = i18n_embed::DesktopLanguageRequester::requested_languages();
    #[cfg(target_arch = "wasm32")]
    let requested = i18n_embed::WebLanguageRequester::requested_languages();
    init(&requested);
}

// Get the `Localizer` to be used for localizing this library.
#[must_use]
pub fn localizer() -> Box<dyn Localizer> {
    Box::from(DefaultLocalizer::new(&*LANGUAGE_LOADER, &Localizations))
}

#[derive(RustEmbed)]
#[folder = "../../i18n/"]
struct Localizations;

pub static LANGUAGE_LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader: FluentLanguageLoader = fluent_language_loader!();

    loader
        .load_fallback_language(&Localizations)
        .expect("Error while loading fallback language");
    // Interpolated values (counts, names, paths) go straight into labels
    // that are painted as plain text, where Unicode isolation marks would
    // show up as stray glyphs.
    loader.set_use_isolating(false);

    loader
});

/// Request a localized string by ID from the i18n/ directory.
#[macro_export]
macro_rules! fl {
    ($message_id:literal) => {{
        i18n_embed_fl::fl!($crate::i18n::LANGUAGE_LOADER, $message_id)
    }};

    ($message_id:literal, $($args:expr),*) => {{
        i18n_embed_fl::fl!($crate::i18n::LANGUAGE_LOADER, $message_id, $($args), *)
    }};
}
