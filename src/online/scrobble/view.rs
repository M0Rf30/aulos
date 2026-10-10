// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The "Scrobbling" block of the settings page.

use super::Service;
use super::controller::{ScrobbleController, ScrobbleMessage, enabled_of, is_connected, user_of};
use crate::config::Config;
use crate::fl;
use crate::views::settings::SearchQuery;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{self, settings::Section};

type Msg = ScrobbleMessage;

fn dim_text() -> cosmic::theme::Text {
    cosmic::theme::Text::Color(cosmic::theme::active().cosmic().palette.neutral_7.into())
}

fn caption<'a>(text: impl Into<std::borrow::Cow<'a, str>> + 'a) -> cosmic::Element<'a, Msg> {
    widget::text::caption(text)
        .class(dim_text())
        .wrapping(cosmic::iced::core::text::Wrapping::Word)
        .into()
}

/// Header with title and a dimmed description, like the other sections.
fn header<'a>(title: String, description: Option<String>) -> cosmic::Element<'a, Msg> {
    let mut col = widget::Column::new()
        .push(widget::text::heading(title))
        .spacing(2);
    if let Some(d) = description {
        col = col.push(caption(d));
    }
    col.into()
}

/// Searchable strings of the general scrobbling section.
fn general_keywords() -> Vec<String> {
    vec![
        fl!("scrobbling"),
        fl!("scrobbling-description"),
        fl!("scrobble-streams"),
        fl!("scrobble-streams-description"),
        fl!("scrobble-import-on-connect"),
        fl!("scrobble-import-on-connect-description"),
        fl!("online-suggestions"),
        fl!("online-suggestions-description"),
    ]
}

/// Searchable strings of one service's section.
fn service_keywords(service: Service) -> Vec<String> {
    let mut words = vec![
        service.display_name().to_string(),
        fl!("scrobbling"),
        fl!("scrobble-connect"),
        fl!("scrobble-disconnect"),
        fl!("scrobble-import"),
        fl!(
            "scrobble-import-description",
            service = service.display_name()
        ),
    ];
    if service == Service::LastFm {
        words.push(fl!("scrobble-love-sync"));
        words.push(fl!("scrobble-love-sync-description"));
        words.push(fl!("scrobble-sync-loved"));
    }
    words
}

/// Everything the Scrobbling block can be searched by.
pub fn keywords() -> Vec<String> {
    let mut words = general_keywords();
    for service in Service::ALL {
        words.extend(service_keywords(service));
    }
    words
}

/// All scrobbling sections: general options, then one per service. With a
/// non-empty `search`, only the sections matching it are shown.
pub fn view<'a>(
    ctrl: &'a ScrobbleController,
    config: &'a Config,
    search: &str,
) -> cosmic::Element<'a, Msg> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let query = SearchQuery::new(search);

    let general = widget::settings::section()
        .header(header(
            fl!("scrobbling"),
            Some(fl!("scrobbling-description")),
        ))
        .add(
            widget::settings::item::builder(fl!("scrobble-streams"))
                .description(fl!("scrobble-streams-description"))
                .toggler(config.scrobble_streams, Msg::SetStreams),
        )
        .add(
            widget::settings::item::builder(fl!("scrobble-import-on-connect"))
                .description(fl!("scrobble-import-on-connect-description"))
                .toggler(config.scrobble_import_on_connect, Msg::SetImportOnConnect),
        )
        .add(
            widget::settings::item::builder(fl!("online-suggestions"))
                .description(fl!("online-suggestions-description"))
                .toggler(config.home_online_suggestions, Msg::SetOnlineSuggestions),
        );

    let mut col = widget::Column::new()
        .spacing(sp.space_l)
        .width(Length::Fill);
    if query.matches(&general_keywords()) {
        col = col.push(general);
    }
    for service in Service::ALL {
        if query.matches(&service_keywords(service)) {
            col = col.push(service_section(ctrl, config, service));
        }
    }
    col.into()
}

fn service_section<'a>(
    ctrl: &'a ScrobbleController,
    config: &'a Config,
    service: Service,
) -> cosmic::Element<'a, Msg> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let mut section = widget::settings::section().title(service.display_name());

    section = if is_connected(config, service) {
        connected_rows(section, ctrl, config, service)
    } else {
        connect_rows(section, ctrl, config, service)
    };

    if let Some(text) = ctrl.status(service) {
        section = section.add(
            widget::container(caption(text.to_string()))
                .padding([sp.space_xxs, 0])
                .width(Length::Fill),
        );
    }
    section.into()
}

fn connected_rows<'a>(
    mut section: Section<'a, Msg>,
    ctrl: &'a ScrobbleController,
    config: &'a Config,
    service: Service,
) -> Section<'a, Msg> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let queued = ctrl.queued(service);

    let mut info = widget::Column::new()
        .push(widget::text::body(fl!(
            "scrobble-connected-as",
            user = user_of(config, service).to_string()
        )))
        .spacing(2);
    if queued > 0 {
        info = info.push(caption(fl!("scrobble-queued", count = queued)));
    }
    if let Some(err) = ctrl.error(service) {
        info = info.push(caption(fl!("scrobble-error", message = err)));
    }
    section = section.add(info).add(
        widget::settings::item::builder(fl!("scrobble-enabled", service = service.display_name()))
            .toggler(enabled_of(config, service), move |v| {
                Msg::SetEnabled(service, v)
            }),
    );

    section = section.add(import_row(ctrl, service));

    if service == Service::LastFm {
        section = section
            .add(
                widget::settings::item::builder(fl!("scrobble-love-sync"))
                    .description(fl!("scrobble-love-sync-description"))
                    .toggler(config.scrobble_lastfm_love_sync, Msg::SetLoveSync),
            )
            .add(
                widget::button::standard(if ctrl.is_busy(service) {
                    fl!("scrobble-working")
                } else {
                    fl!("scrobble-sync-loved")
                })
                .on_press_maybe((!ctrl.is_busy(service)).then_some(Msg::SyncLoved)),
            );
    }

    let mut buttons = widget::Row::new()
        .spacing(sp.space_xs)
        .align_y(Alignment::Center);
    if queued > 0 {
        buttons = buttons
            .push(widget::button::standard(fl!("scrobble-retry-now")).on_press(Msg::RetryNow));
    }
    buttons = buttons.push(
        widget::button::destructive(fl!("scrobble-disconnect")).on_press(Msg::Disconnect(service)),
    );
    section.add(buttons)
}

/// "Import listening history": description, button, and — while running —
/// progress and a cancel button.
fn import_row<'a>(ctrl: &'a ScrobbleController, service: Service) -> cosmic::Element<'a, Msg> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let mut col = widget::Column::new()
        .push(widget::text::body(fl!("scrobble-import")))
        .push(caption(fl!(
            "scrobble-import-description",
            service = service.display_name()
        )))
        .spacing(sp.space_xxs)
        .width(Length::Fill);

    if let Some(p) = ctrl.import_progress(service) {
        let text = match p.total {
            Some(total) => fl!(
                "scrobble-import-progress-total",
                fetched = p.fetched.to_string(),
                total = total.to_string(),
                matched = p.matched.to_string()
            ),
            None => fl!(
                "scrobble-import-progress",
                fetched = p.fetched.to_string(),
                matched = p.matched.to_string()
            ),
        };
        col = col.push(caption(text));
        if let Some(fraction) = p.fraction() {
            col = col.push(widget::progress_bar::determinate_linear(fraction).width(Length::Fill));
        }
        col = col.push(
            widget::button::standard(fl!("scrobble-import-cancel"))
                .on_press(Msg::ImportCancel(service)),
        );
    } else {
        col = col.push(
            widget::button::standard(fl!("scrobble-import"))
                .on_press_maybe((!ctrl.is_importing()).then_some(Msg::ImportStart(service))),
        );
    }
    col.into()
}

fn connect_rows<'a>(
    mut section: Section<'a, Msg>,
    ctrl: &'a ScrobbleController,
    config: &'a Config,
    service: Service,
) -> Section<'a, Msg> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let busy = ctrl.is_busy(service);
    section = section.add(widget::text::body(fl!("scrobble-not-connected")));

    match service {
        Service::ListenBrainz => {
            let connect = widget::button::suggested(if busy {
                fl!("scrobble-working")
            } else {
                fl!("scrobble-connect")
            })
            .on_press_maybe(
                (!busy && !ctrl.lb_token_input.trim().is_empty()).then_some(Msg::LbConnect),
            );
            section = section.add(caption(fl!("scrobble-lb-token-hint"))).add(
                widget::Row::new()
                    .push(
                        widget::text_input(
                            fl!("scrobble-lb-token-placeholder"),
                            &ctrl.lb_token_input,
                        )
                        .on_input(Msg::LbTokenInput)
                        .on_submit(|_| Msg::LbConnect)
                        .password()
                        .width(Length::Fill),
                    )
                    .push(connect)
                    .spacing(sp.space_xs)
                    .align_y(Alignment::Center),
            );
        }
        Service::LastFm | Service::LibreFm => {
            if service == Service::LastFm {
                let save =
                    widget::button::standard(fl!("scrobble-save")).on_press(Msg::SaveLastFmKeys);
                section = section
                    .add(caption(fl!("scrobble-lastfm-keys-hint")))
                    .add(
                        widget::text_input(
                            fl!("scrobble-lastfm-key-placeholder"),
                            &ctrl.lastfm_key_input,
                        )
                        .on_input(Msg::LastFmKeyInput)
                        .width(Length::Fill),
                    )
                    .add(
                        widget::Row::new()
                            .push(
                                widget::text_input(
                                    fl!("scrobble-lastfm-secret-placeholder"),
                                    &ctrl.lastfm_secret_input,
                                )
                                .on_input(Msg::LastFmSecretInput)
                                .password()
                                .width(Length::Fill),
                            )
                            .push(save)
                            .spacing(sp.space_xs)
                            .align_y(Alignment::Center),
                    );
            }
            let can_start = !busy
                && (service == Service::LibreFm || !config.scrobble_lastfm_api_key.is_empty());
            let mut row = widget::Row::new()
                .spacing(sp.space_xs)
                .align_y(Alignment::Center)
                .push(
                    widget::button::suggested(if busy {
                        fl!("scrobble-working")
                    } else {
                        fl!("scrobble-connect")
                    })
                    .on_press_maybe(can_start.then_some(Msg::StartAuth(service))),
                );
            if ctrl.is_pending(service) {
                row = row.push(
                    widget::button::standard(fl!("scrobble-complete"))
                        .on_press_maybe((!busy).then_some(Msg::CompleteAuth(service))),
                );
            }
            section = section.add(row);
        }
    }
    section
}
