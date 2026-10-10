// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Equalizer view with 10-band vertical sliders and preset management.
//!
//! The preset dropdown contains built-in and custom presets. AutoEQ headphone
//! profiles are searched and selected via a separate text input + scrollable
//! results list below the preset controls.

use crate::autoeq::AutoEQProfileMetadata;
use crate::fl;
use crate::player::equalizer::{BAND_LABELS, EqPresetData, PresetSource};
use crate::views::common;
use crate::views::list_row_button_class;
use cosmic::iced::{Alignment, Length};
use cosmic::widget;

/// Messages from the equalizer view.
#[derive(Debug, Clone)]
pub enum EqualizerMessage {
    SetBand(usize, f32),
    ToggleEnabled(bool),
    SetPreamp(f32),
    /// Select a regular preset by name.
    SelectPreset(String),
    /// Select an AutoEQ profile by its repository path.
    SelectAutoEQ(String),
    /// Save (overwrite) the current custom preset.
    SavePreset,
    /// User typed a name in the "Save As" input.
    SaveAsNameChanged(String),
    /// Confirm "Save As" with the typed name.
    SavePresetAs,
    /// Delete the active custom preset.
    DeletePreset,
    /// Reset to Flat (all bands 0, preamp 0).
    ResetPreset,
    /// Fetch AutoEQ profile index from GitHub.
    FetchAutoEQ,
    /// User typed in the AutoEQ search field.
    AutoEQSearchChanged(String),
}

/// Height of the vertical band sliders.
const BAND_SLIDER_HEIGHT: f32 = 150.0;
/// Height reserved above/below the sliders for the value/frequency captions.
const BAND_CAPTION_HEIGHT: f32 = 16.0;

/// Theme-driven dimmed text colour for secondary labels.
fn dim_text() -> cosmic::theme::Text {
    cosmic::theme::Text::Color(cosmic::theme::active().cosmic().palette.neutral_7.into())
}

/// Section header: heading plus a dimmed one-line explanation.
fn section_header<'a>(title: String, description: String) -> cosmic::Element<'a, EqualizerMessage> {
    widget::Column::new()
        .push(widget::text::heading(title))
        .push(widget::text::caption(description).class(dim_text()))
        .spacing(2)
        .into()
}

/// Render the equalizer panel (shown in the context drawer, which already
/// supplies the outer padding and the scrolling).
#[allow(clippy::too_many_arguments)]
pub fn equalizer_view<'a>(
    bands: &'a [f32],
    enabled: bool,
    preamp: f32,
    all_presets: &'a [EqPresetData],
    active_preset_name: Option<&'a str>,
    dirty: bool,
    save_as_name: &'a str,
    autoeq_profiles: &'a [AutoEQProfileMetadata],
    autoeq_loading: bool,
    autoeq_search: &'a str,
) -> cosmic::Element<'a, EqualizerMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;

    widget::Column::new()
        .push(enable_section(enabled))
        .push(bands_section(bands, preamp))
        .push(preset_section(
            all_presets,
            active_preset_name,
            dirty,
            save_as_name,
        ))
        .push(autoeq_section(
            autoeq_profiles,
            autoeq_loading,
            autoeq_search,
        ))
        .spacing(sp.space_l)
        .width(Length::Fill)
        .into()
}

/// Prominent on/off switch: a whole-row toggle in its own section.
fn enable_section<'a>(enabled: bool) -> cosmic::Element<'a, EqualizerMessage> {
    let description = if enabled {
        fl!("equalizer-enabled-description")
    } else {
        fl!("equalizer-disabled-hint")
    };
    widget::settings::section()
        .add(
            widget::settings::item::builder(fl!("equalizer-enabled"))
                .description(description)
                .toggler(enabled, EqualizerMessage::ToggleEnabled),
        )
        .into()
}

/// Preamp slider and the 10 vertical band sliders, grouped in one card with
/// a dB axis on the left.
fn bands_section<'a>(bands: &'a [f32], preamp: f32) -> cosmic::Element<'a, EqualizerMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let accent: cosmic::iced::core::Color = cosmic::theme::active().cosmic().accent_color().into();

    // --- Preamp ---
    let preamp_row = widget::Row::new()
        .push(widget::text::body(fl!("equalizer-section-preamp")))
        .push(widget::space::horizontal())
        .push(
            widget::text::caption(fl!(
                "equalizer-preamp-value",
                db = format!("{:+.1}", preamp)
            ))
            .class(dim_text()),
        )
        .align_y(Alignment::Center);

    let preamp_control = widget::Column::new()
        .push(preamp_row)
        .push(widget::slider(-20.0..=10.0, preamp, EqualizerMessage::SetPreamp).width(Length::Fill))
        .spacing(sp.space_xxs);

    // --- dB axis ---
    let axis_label = |text: &'static str| {
        widget::text::caption(text)
            .size(10)
            .class(dim_text())
            .align_x(cosmic::iced::alignment::Horizontal::Right)
            .width(Length::Fill)
    };
    let axis = widget::Column::new()
        .push(widget::Space::new().height(Length::Fixed(BAND_CAPTION_HEIGHT)))
        .push(
            widget::Column::new()
                .push(axis_label("+12"))
                .push(widget::Space::new().height(Length::Fill))
                .push(axis_label("0"))
                .push(widget::Space::new().height(Length::Fill))
                .push(axis_label("-12"))
                .height(Length::Fixed(BAND_SLIDER_HEIGHT)),
        )
        .width(Length::Fixed(24.0));

    // --- 10-band vertical sliders ---
    // Each band column gets equal width via Length::Fill so they spread
    // evenly across the card.
    let mut band_row = widget::Row::new()
        .push(axis)
        .spacing(sp.space_xxxs)
        .width(Length::Fill);

    for (i, &gain) in bands.iter().enumerate().take(10) {
        let label = BAND_LABELS.get(i).copied().unwrap_or("?");
        // Flat bands stay quiet; boosted/cut bands pick up the accent.
        let value_class = if gain.abs() < 0.05 {
            dim_text()
        } else {
            cosmic::theme::Text::Color(accent)
        };

        let slider_col = widget::Column::new()
            .push(
                widget::text::caption(format!("{:+.1}", gain))
                    .size(10)
                    .class(value_class),
            )
            .push(
                widget::vertical_slider(-12.0..=12.0, gain, move |v| {
                    EqualizerMessage::SetBand(i, v)
                })
                .height(BAND_SLIDER_HEIGHT),
            )
            .push(widget::text::caption(label).size(10).class(dim_text()))
            .spacing(2)
            .width(Length::Fill)
            .align_x(Alignment::Center);

        band_row = band_row.push(slider_col);
    }

    let card = widget::container(
        widget::Column::new()
            .push(preamp_control)
            .push(widget::divider::horizontal::default())
            .push(band_row)
            .spacing(sp.space_s),
    )
    .padding(sp.space_s)
    .width(Length::Fill)
    .class(cosmic::theme::Container::Card);

    widget::Column::new()
        .push(widget::text::heading(fl!("equalizer-section-bands")))
        .push(card)
        .spacing(sp.space_xs)
        .into()
}

/// Preset picker, "save as" input and the save/reset/delete actions.
fn preset_section<'a>(
    all_presets: &'a [EqPresetData],
    active_preset_name: Option<&'a str>,
    dirty: bool,
    save_as_name: &'a str,
) -> cosmic::Element<'a, EqualizerMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;

    // --- Preset dropdown (built-in + custom only) ---
    let preset_names_display: Vec<String> = all_presets
        .iter()
        .map(|p| match p.source {
            PresetSource::Builtin => p.name.clone(),
            _ => format!("{} *", p.name),
        })
        .collect();

    let preset_names: Vec<String> = all_presets.iter().map(|p| p.name.clone()).collect();

    let selected_index =
        active_preset_name.and_then(|active| all_presets.iter().position(|p| p.name == active));

    let preset_dropdown = widget::dropdown(preset_names_display, selected_index, move |idx| {
        if let Some(name) = preset_names.get(idx) {
            EqualizerMessage::SelectPreset(name.clone())
        } else {
            EqualizerMessage::ResetPreset
        }
    })
    .width(Length::Fill);

    let mut picker = widget::Column::new()
        .push(preset_dropdown)
        .spacing(sp.space_xxs);
    if dirty {
        picker =
            picker.push(widget::text::caption(fl!("equalizer-unsaved-changes")).class(dim_text()));
    }

    // --- Save As inline input ---
    let save_as_input = widget::text_input(fl!("equalizer-preset-name-placeholder"), save_as_name)
        .on_input(EqualizerMessage::SaveAsNameChanged)
        .on_submit_maybe(
            (!save_as_name.trim().is_empty()).then_some(|_: String| EqualizerMessage::SavePresetAs),
        );

    let save_as_btn = widget::button::standard(fl!("equalizer-save-preset-as")).on_press_maybe(
        (!save_as_name.trim().is_empty()).then_some(EqualizerMessage::SavePresetAs),
    );

    let save_as_row = widget::Row::new()
        .push(save_as_input.width(Length::Fill))
        .push(save_as_btn)
        .spacing(sp.space_xs)
        .align_y(Alignment::Center);

    // --- Actions: primary save (custom presets), quiet reset/delete ---
    let is_custom_selected = active_preset_name
        .map(|name| {
            all_presets
                .iter()
                .any(|p| p.name == name && p.source != PresetSource::Builtin)
        })
        .unwrap_or(false);

    let mut actions = widget::Row::new()
        .spacing(sp.space_xs)
        .align_y(Alignment::Center);
    if is_custom_selected {
        actions = actions.push(
            widget::button::suggested(fl!("save"))
                .on_press_maybe(dirty.then_some(EqualizerMessage::SavePreset)),
        );
    }
    actions = actions
        .push(
            widget::button::standard(fl!("equalizer-reset-preset"))
                .on_press(EqualizerMessage::ResetPreset),
        )
        .push(widget::space::horizontal());
    if is_custom_selected {
        let destructive: cosmic::iced::core::Color =
            cosmic::theme::active().cosmic().destructive_color().into();
        actions = actions.push(
            widget::button::custom(
                widget::text::body(fl!("equalizer-delete-preset"))
                    .class(cosmic::theme::Text::Color(destructive)),
            )
            .padding([sp.space_xxs, sp.space_s])
            .class(cosmic::theme::Button::Text)
            .on_press(EqualizerMessage::DeletePreset),
        );
    }

    widget::settings::section()
        .title(fl!("equalizer-section-preset"))
        .add(picker)
        .add(save_as_row)
        .add(actions)
        .into()
}

/// AutoEQ headphone correction: loader, search field and scrollable results.
fn autoeq_section<'a>(
    autoeq_profiles: &'a [AutoEQProfileMetadata],
    autoeq_loading: bool,
    autoeq_search: &'a str,
) -> cosmic::Element<'a, EqualizerMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let header = section_header(
        fl!("equalizer-section-autoeq"),
        fl!("equalizer-autoeq-description"),
    );

    let mut col = widget::Column::new().push(header).spacing(sp.space_xs);

    if autoeq_profiles.is_empty() {
        // Profiles not yet loaded — show fetch button
        let fetch_btn = if autoeq_loading {
            widget::button::standard(fl!("equalizer-autoeq-loading"))
        } else {
            widget::button::standard(fl!("equalizer-autoeq-load-profiles"))
                .on_press(EqualizerMessage::FetchAutoEQ)
        };
        col = col.push(
            widget::container(fetch_btn)
                .padding(sp.space_s)
                .width(Length::Fill)
                .align_x(cosmic::iced::alignment::Horizontal::Center)
                .class(cosmic::theme::Container::Card),
        );
        return col.into();
    }

    let query = autoeq_search.trim().to_lowercase();

    let mut search_input =
        widget::search_input(fl!("equalizer-autoeq-search-placeholder"), autoeq_search)
            .on_input(EqualizerMessage::AutoEQSearchChanged);
    if !autoeq_search.is_empty() {
        search_input = search_input.on_clear(EqualizerMessage::AutoEQSearchChanged(String::new()));
    }
    col = col.push(search_input);

    let loaded_caption = fl!(
        "equalizer-autoeq-profiles-loaded",
        count = autoeq_profiles.len().to_string()
    );

    if query.len() >= 2 {
        let filtered: Vec<&AutoEQProfileMetadata> = autoeq_profiles
            .iter()
            .filter(|p| p.name.to_lowercase().contains(&query))
            .take(50) // cap results for performance
            .collect();

        let count = filtered.len();
        let count_text = if count == 0 {
            fl!("equalizer-autoeq-no-matches")
        } else if count >= 50 {
            fl!("equalizer-autoeq-too-many-matches")
        } else {
            fl!("equalizer-autoeq-match-count", count = count.to_string())
        };
        col = col.push(widget::text::caption(count_text).class(dim_text()));

        if count > 0 {
            // Scrollable, clickable list of matching profiles
            let mut result_list = widget::Column::new().spacing(1);
            for profile in &filtered {
                let subtitle = format!("{} · {}", profile.type_, profile.source);
                let row_content = widget::Column::new()
                    .push(common::cell_text(&profile.name))
                    .push(common::cell_caption(subtitle).class(dim_text()))
                    .spacing(1);

                result_list = result_list.push(
                    widget::button::custom(
                        widget::container(common::clipped_cell(row_content.into()))
                            .padding([sp.space_xxs, sp.space_xs]),
                    )
                    .on_press(EqualizerMessage::SelectAutoEQ(profile.path.clone()))
                    .width(Length::Fill)
                    .padding(0)
                    .class(list_row_button_class(false)),
                );
            }

            col = col.push(
                widget::container(
                    widget::scrollable(widget::container(result_list).width(Length::Fill))
                        .height(Length::Fixed(220.0)),
                )
                .padding(sp.space_xxs)
                .width(Length::Fill)
                .class(cosmic::theme::Container::Card),
            );
        }
    } else {
        col =
            col.push(widget::text::caption(fl!("equalizer-autoeq-search-hint")).class(dim_text()));
    }

    // Footer: profile count + quiet refresh.
    col = col.push(
        widget::Row::new()
            .push(
                widget::text::caption(loaded_caption)
                    .class(dim_text())
                    .width(Length::Fill),
            )
            .push(widget::tooltip(
                widget::button::icon(widget::icon::from_name("view-refresh-symbolic").size(16))
                    .extra_small()
                    .on_press_maybe((!autoeq_loading).then_some(EqualizerMessage::FetchAutoEQ)),
                widget::text::caption(fl!("equalizer-autoeq-refresh")),
                widget::tooltip::Position::Top,
            ))
            .align_y(Alignment::Center),
    );

    col.into()
}
