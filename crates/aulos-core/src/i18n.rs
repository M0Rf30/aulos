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

/// [`init`] with `language` (a BCP 47 tag), or the system languages when it
/// is empty or unparsable. Also the way to switch language at runtime.
pub fn init_for(language: &str) {
    match language.parse::<LanguageIdentifier>() {
        Ok(id) if !language.is_empty() => init(&[id]),
        _ => init_from_system(),
    }
}

/// Languages that ship translations, as BCP 47 tags, fallback first.
#[must_use]
pub fn available_languages() -> Vec<String> {
    let mut tags: Vec<String> = LANGUAGE_LOADER
        .available_languages(&Localizations)
        .unwrap_or_default()
        .iter()
        .map(ToString::to_string)
        .collect();
    tags.sort();
    tags
}

/// A language's name in that language, for a language picker; the tag itself
/// when unknown.
#[must_use]
pub fn native_name(tag: &str) -> String {
    match tag {
        "en" => "English",
        "it" => "Italiano",
        "de" => "Deutsch",
        "fr" => "Français",
        "es" => "Español",
        "pt-BR" => "Português (Brasil)",
        other => other,
    }
    .to_owned()
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

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_syntax::{ast, parser};
    use std::collections::BTreeMap;

    /// Message id -> its `$variables`, panicking on any syntax error (the
    /// loader would skip a bad entry silently and show English instead).
    fn messages(tag: &str) -> BTreeMap<String, Vec<String>> {
        fn vars(pattern: &ast::Pattern<&str>, out: &mut Vec<String>) {
            for el in &pattern.elements {
                if let ast::PatternElement::Placeable { expression } = el {
                    expr(expression, out);
                }
            }
        }
        fn expr(e: &ast::Expression<&str>, out: &mut Vec<String>) {
            match e {
                ast::Expression::Inline(ast::InlineExpression::VariableReference { id }) => {
                    out.push(id.name.to_owned());
                }
                ast::Expression::Inline(ast::InlineExpression::Placeable { expression }) => {
                    expr(expression, out);
                }
                ast::Expression::Select { selector, variants } => {
                    expr(&ast::Expression::Inline(selector.clone()), out);
                    for v in variants {
                        vars(&v.value, out);
                    }
                }
                ast::Expression::Inline(_) => {}
            }
        }
        let file = Localizations::get(&format!("{tag}/aulos.ftl")).unwrap();
        let text = std::str::from_utf8(&file.data).unwrap();
        let resource = parser::parse(text).unwrap_or_else(|(_, e)| panic!("{tag}: {e:?}"));
        resource
            .body
            .iter()
            .filter_map(|entry| match entry {
                ast::Entry::Message(m) => {
                    let mut v = Vec::new();
                    if let Some(p) = &m.value {
                        vars(p, &mut v);
                    }
                    v.sort();
                    v.dedup();
                    Some((m.id.name.to_owned(), v))
                }
                _ => None,
            })
            .collect()
    }

    /// A translation that drops a message or a placeholder shows English (or
    /// a missing value) to the user; a malformed one fails to parse.
    #[test]
    fn every_translation_has_the_english_messages_and_placeholders() {
        let en = messages("en");
        let langs = available_languages();
        assert!(langs.len() > 1, "only {langs:?}");
        for tag in langs.iter().filter(|t| *t != "en") {
            let tr = messages(tag);
            for (id, vars) in &en {
                assert_eq!(tr.get(id), Some(vars), "{tag}: `{id}`");
            }
            assert_eq!(tr.len(), en.len(), "{tag} has extra messages");
        }
    }
}
