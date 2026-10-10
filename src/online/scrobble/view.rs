// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The "Scrobbling" block of the settings page.

use super::Service;
use super::controller::{ScrobbleController, ScrobbleMessage, enabled_of, is_connected, user_of};
use crate::config::Config;
use crate::fl;
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

/// All scrobbling sections: general options, then one per service.
pub fn view<'a>(ctrl: &'a ScrobbleController, config: &'a Config) -> cosmic::Element<'a, Msg> {
    let sp = cosmic::theme::active().cosmic().spacing;

    let general = widget::settings::section()
        .header(header(
            fl!("scrobbling"),
            Some(fl!("scrobbling-description")),
        ))
        .add(
            widget::settings::item::builder(fl!("scrobble-streams"))
                .description(fl!("scrobble-streams-description"))
                .toggler(config.scrobble_streams, Msg::SetStreams),
        );

    widget::Column::new()
        .spacing(sp.space_l)
        .width(Length::Fill)
        .push(general)
        .push(service_section(ctrl, config, Service::ListenBrainz))
        .push(service_section(ctrl, config, Service::LastFm))
        .push(service_section(ctrl, config, Service::LibreFm))
        .into()
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
