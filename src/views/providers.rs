// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Provider settings view — configure MPD and Subsonic servers.

use crate::fl;
use cosmic::iced::Alignment;
use cosmic::iced::Length;
use cosmic::iced::core::Color;
use cosmic::widget;

// ── MPD editing state ──────────────────────────────────────────────────────

/// Editing state for a single MPD server entry.
///
/// Kept as strings so text inputs can bind directly without
/// lifetime issues around temporary conversions.
#[derive(Debug, Clone)]
pub struct MpdEditState {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: String,
    pub password: String,
}

impl MpdEditState {
    /// Create from a config entry.
    ///
    /// When `password_in_keyring` is set the password is fetched from the
    /// system keyring so the edit form can show (and round-trip) the real value.
    pub fn from_config(entry: &crate::config::MpdConfigEntry) -> Self {
        let password = if entry.password_in_keyring {
            crate::credentials::retrieve_password(&entry.id)
                .ok()
                .flatten()
                .unwrap_or_default()
        } else {
            entry.password.clone().unwrap_or_default()
        };

        Self {
            id: entry.id.clone(),
            name: entry.name.clone(),
            host: entry.host.clone(),
            port: entry.port.to_string(),
            password,
        }
    }

    /// Convert back to a config entry (port defaults to 6600 on parse failure).
    ///
    /// The password is always stored as plaintext here so that the startup
    /// migration code can move it to the keyring on the next launch.
    pub fn to_config(&self) -> crate::config::MpdConfigEntry {
        crate::config::MpdConfigEntry {
            id: self.id.clone(),
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port.parse().unwrap_or(6600),
            password: if self.password.is_empty() {
                None
            } else {
                Some(self.password.clone())
            },
            // Always reset to false so the startup migration path stores it in
            // the keyring (or the plaintext fallback is used if unavailable).
            password_in_keyring: false,
        }
    }

    /// Create a new empty entry with a generated id.
    pub fn new_default(index: usize) -> Self {
        Self {
            id: format!("mpd-{index}"),
            name: "MPD Server".to_string(),
            host: "localhost".to_string(),
            port: "6600".to_string(),
            password: String::new(),
        }
    }
}

// ── Subsonic editing state ─────────────────────────────────────────────────

/// Editing state for a single Subsonic/Navidrome server entry.
#[derive(Debug, Clone)]
pub struct SubsonicEditState {
    pub id: String,
    pub name: String,
    pub url: String,
    pub username: String,
    pub password: String,
    pub accept_invalid_certs: bool,
    /// Transcoding max bitrate (None = original quality).
    pub transcoding_max_bitrate: Option<u32>,
    /// Transcoding format (None = original format).
    pub transcoding_format: Option<String>,
}

impl SubsonicEditState {
    /// Create from a config entry.
    ///
    /// When `password_in_keyring` is set the password is fetched from the
    /// system keyring so the edit form can show (and round-trip) the real value.
    pub fn from_config(entry: &crate::config::SubsonicConfigEntry) -> Self {
        let password = if entry.password_in_keyring {
            crate::credentials::retrieve_password(&entry.id)
                .ok()
                .flatten()
                .unwrap_or_default()
        } else {
            entry.password.clone().unwrap_or_default()
        };

        Self {
            id: entry.id.clone(),
            name: entry.name.clone(),
            url: entry.url.clone(),
            username: entry.username.clone(),
            password,
            accept_invalid_certs: entry.accept_invalid_certs,
            transcoding_max_bitrate: entry.transcoding_max_bitrate,
            transcoding_format: entry.transcoding_format.clone(),
        }
    }

    /// Convert back to a config entry.
    ///
    /// The password is always stored as plaintext here so that the startup
    /// migration code can move it to the keyring on the next launch (or
    /// immediately if the keyring is available).
    pub fn to_config(&self) -> crate::config::SubsonicConfigEntry {
        crate::config::SubsonicConfigEntry {
            id: self.id.clone(),
            name: self.name.clone(),
            url: self.url.clone(),
            username: self.username.clone(),
            password: if self.password.is_empty() {
                None
            } else {
                Some(self.password.clone())
            },
            // Always reset to false so the startup migration path stores it in
            // the keyring (or the plaintext fallback is used if unavailable).
            password_in_keyring: false,
            accept_invalid_certs: self.accept_invalid_certs,
            transcoding_max_bitrate: self.transcoding_max_bitrate,
            transcoding_format: self.transcoding_format.clone(),
        }
    }

    /// Create a new empty entry with a generated id.
    pub fn new_default(index: usize) -> Self {
        Self {
            id: format!("subsonic-{index}"),
            name: "Subsonic Server".to_string(),
            url: "https://".to_string(),
            username: String::new(),
            password: String::new(),
            accept_invalid_certs: false,
            transcoding_max_bitrate: None,
            transcoding_format: None,
        }
    }
}

// ── Messages ───────────────────────────────────────────────────────────────

/// Messages emitted by the providers settings view.
#[derive(Debug, Clone)]
pub enum ProvidersMessage {
    // MPD
    AddMpd,
    EditName(usize, String),
    EditHost(usize, String),
    EditPort(usize, String),
    EditPassword(usize, String),
    Save(usize),
    Remove(usize),
    TestConnection(usize),

    // Subsonic
    AddSubsonic,
    SubsonicEditName(usize, String),
    SubsonicEditUrl(usize, String),
    SubsonicEditUsername(usize, String),
    SubsonicEditPassword(usize, String),
    SubsonicToggleCerts(usize, bool),
    SubsonicSave(usize),
    SubsonicRemove(usize),
    SubsonicTestConnection(usize),
    /// Subsonic transcoding bitrate changed (server index, bitrate or None for original).
    SubsonicTranscodingBitrate(usize, Option<u32>),
    /// Subsonic transcoding format changed (server index, format or None for original).
    SubsonicTranscodingFormat(usize, Option<String>),
}

// ── View ───────────────────────────────────────────────────────────────────

/// Transcoding bitrate choices, in dropdown order (`None` = original).
const BITRATES: [Option<u32>; 7] = [
    None,
    Some(320),
    Some(256),
    Some(192),
    Some(128),
    Some(96),
    Some(64),
];

/// Transcoding format choices, in dropdown order (`None` = original).
const FORMATS: [Option<&str>; 5] = [None, Some("mp3"), Some("ogg"), Some("opus"), Some("aac")];

/// Render the providers settings panel (shown in the context drawer, which
/// already supplies the outer padding and the scrolling).
pub fn providers_view<'a>(
    mpd_servers: &'a [MpdEditState],
    mpd_connection_status: &'a [Option<String>],
    subsonic_servers: &'a [SubsonicEditState],
    subsonic_connection_status: &'a [Option<String>],
) -> cosmic::Element<'a, ProvidersMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let mut col = widget::Column::new()
        .spacing(sp.space_m)
        .width(Length::Fill)
        .push(widget::text::body(fl!("providers-description")).class(dim_text()));

    if mpd_servers.is_empty() && subsonic_servers.is_empty() {
        col = col.push(empty_state());
    }

    // MPD servers
    for (i, server) in mpd_servers.iter().enumerate() {
        let status = mpd_connection_status.get(i).and_then(|s| s.as_deref());
        col = col.push(mpd_server_card(i, server, status));
    }

    // Subsonic servers
    for (i, server) in subsonic_servers.iter().enumerate() {
        let status = subsonic_connection_status.get(i).and_then(|s| s.as_deref());
        col = col.push(subsonic_server_card(i, server, status));
    }

    // Add buttons (wrap instead of overflowing a narrow drawer)
    let add_icon = || widget::icon::from_name("list-add-symbolic").size(16);
    col = col.push(
        widget::flex_row(vec![
            widget::button::standard(fl!("add-mpd-server"))
                .leading_icon(add_icon())
                .on_press(ProvidersMessage::AddMpd)
                .into(),
            widget::button::standard(fl!("add-subsonic-server"))
                .leading_icon(add_icon())
                .on_press(ProvidersMessage::AddSubsonic)
                .into(),
        ])
        .spacing(sp.space_xs),
    );

    col.into()
}

/// Theme-driven dimmed text colour for secondary labels.
fn dim_text() -> cosmic::theme::Text {
    cosmic::theme::Text::Color(cosmic::theme::active().cosmic().palette.neutral_7.into())
}

/// Placeholder shown while no server is configured.
fn empty_state<'a>() -> cosmic::Element<'a, ProvidersMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    widget::container(
        widget::Column::new()
            .push(
                widget::icon::icon(widget::icon::from_name("network-server-symbolic").handle())
                    .size(48)
                    .class(cosmic::theme::Svg::custom(|theme| {
                        cosmic::iced::widget::svg::Style {
                            color: Some(theme.cosmic().palette.neutral_6.into()),
                        }
                    })),
            )
            .push(widget::text::title4(fl!("no-providers")))
            .push(
                widget::text::caption(fl!("providers-empty-hint"))
                    .class(dim_text())
                    .align_x(cosmic::iced::alignment::Horizontal::Center),
            )
            .spacing(sp.space_xs)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([sp.space_l, sp.space_s])
    .align_x(cosmic::iced::alignment::Horizontal::Center)
    .class(cosmic::theme::Container::Card)
    .into()
}

// ── MPD card ───────────────────────────────────────────────────────────────

fn mpd_server_card<'a>(
    index: usize,
    server: &'a MpdEditState,
    connection_status: Option<&'a str>,
) -> cosmic::Element<'a, ProvidersMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;

    let name_input = widget::text_input(fl!("mpd-name"), &server.name)
        .on_input(move |v| ProvidersMessage::EditName(index, v));

    let host_input = widget::text_input("localhost", &server.host)
        .on_input(move |v| ProvidersMessage::EditHost(index, v));

    let port_input = widget::text_input("6600", &server.port)
        .on_input(move |v| ProvidersMessage::EditPort(index, v));

    let password_input = widget::text_input(fl!("mpd-password"), &server.password)
        .on_input(move |v| ProvidersMessage::EditPassword(index, v))
        .password();

    let body = widget::Column::new()
        .push(field(fl!("mpd-name"), name_input, Length::Fill))
        .push(field(fl!("mpd-host"), host_input, Length::Fill))
        .push(
            widget::Row::new()
                .push(field(fl!("mpd-port"), port_input, Length::FillPortion(1)))
                .push(field(
                    fl!("mpd-password"),
                    password_input,
                    Length::FillPortion(2),
                ))
                .spacing(sp.space_xs),
        )
        .spacing(sp.space_xs);

    let actions = provider_action_buttons(
        ProvidersMessage::Save(index),
        ProvidersMessage::TestConnection(index),
        ProvidersMessage::Remove(index),
    );

    server_card("MPD", &server.name, connection_status, body.into(), actions)
}

// ── Subsonic card ──────────────────────────────────────────────────────────

fn subsonic_server_card<'a>(
    index: usize,
    server: &'a SubsonicEditState,
    connection_status: Option<&'a str>,
) -> cosmic::Element<'a, ProvidersMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;

    let name_input = widget::text_input(fl!("subsonic-name"), &server.name)
        .on_input(move |v| ProvidersMessage::SubsonicEditName(index, v));

    let url_input = widget::text_input("https://music.example.com", &server.url)
        .on_input(move |v| ProvidersMessage::SubsonicEditUrl(index, v));

    let username_input = widget::text_input(fl!("subsonic-username"), &server.username)
        .on_input(move |v| ProvidersMessage::SubsonicEditUsername(index, v));

    let password_input = widget::text_input(fl!("subsonic-password"), &server.password)
        .on_input(move |v| ProvidersMessage::SubsonicEditPassword(index, v))
        .password();

    let tls_item = widget::settings::item::builder(fl!("subsonic-accept-invalid-certs"))
        .description(fl!("subsonic-accept-invalid-certs-hint"))
        .control(
            widget::toggler(server.accept_invalid_certs)
                .on_toggle(move |v| ProvidersMessage::SubsonicToggleCerts(index, v)),
        );

    // Transcoding: two compact dropdowns instead of a wall of buttons.
    let bitrate_labels: Vec<String> = BITRATES
        .iter()
        .map(|b| match b {
            None => fl!("transcoding-original"),
            Some(kbps) => format!("{kbps} kbps"),
        })
        .collect();
    let bitrate_selected = BITRATES
        .iter()
        .position(|b| *b == server.transcoding_max_bitrate);
    let bitrate_item = widget::settings::item::builder(fl!("transcoding-bitrate")).control(
        widget::dropdown(bitrate_labels, bitrate_selected, move |i| {
            ProvidersMessage::SubsonicTranscodingBitrate(index, BITRATES[i])
        }),
    );

    let format_labels: Vec<String> = FORMATS
        .iter()
        .map(|f| match f {
            None => fl!("transcoding-original"),
            Some("mp3") => "MP3".to_string(),
            Some("ogg") => "OGG Vorbis".to_string(),
            Some("opus") => "Opus".to_string(),
            Some("aac") => "AAC".to_string(),
            Some(other) => other.to_string(),
        })
        .collect();
    let format_selected = FORMATS
        .iter()
        .position(|f| f.map(str::to_string) == server.transcoding_format);
    let format_item = widget::settings::item::builder(fl!("transcoding-format")).control(
        widget::dropdown(format_labels, format_selected, move |i| {
            ProvidersMessage::SubsonicTranscodingFormat(index, FORMATS[i].map(str::to_string))
        }),
    );

    let mut transcoding_col = widget::Column::new()
        .push(widget::text::heading(fl!("transcoding")))
        .push(widget::text::caption(fl!("transcoding-description")).class(dim_text()))
        .push(bitrate_item)
        .push(format_item)
        .spacing(sp.space_xs);

    if let Some(bitrate) = server.transcoding_max_bitrate {
        // Rough estimate: typical FLAC ~1000 kbps, so savings ≈ (1 - bitrate/1000) * 100
        let savings_pct = ((1.0 - (bitrate as f32 / 1000.0)) * 100.0).max(0.0) as u32;
        transcoding_col = transcoding_col.push(
            widget::text::caption(fl!(
                "transcoding-bandwidth-estimate",
                percent = savings_pct.to_string()
            ))
            .class(dim_text()),
        );
    }

    let body = widget::Column::new()
        .push(field(fl!("subsonic-name"), name_input, Length::Fill))
        .push(field(fl!("subsonic-url"), url_input, Length::Fill))
        .push(
            widget::Row::new()
                .push(field(
                    fl!("subsonic-username"),
                    username_input,
                    Length::FillPortion(1),
                ))
                .push(field(
                    fl!("subsonic-password"),
                    password_input,
                    Length::FillPortion(1),
                ))
                .spacing(sp.space_xs),
        )
        .push(tls_item)
        .push(widget::divider::horizontal::default())
        .push(transcoding_col)
        .spacing(sp.space_xs);

    let actions = provider_action_buttons(
        ProvidersMessage::SubsonicSave(index),
        ProvidersMessage::SubsonicTestConnection(index),
        ProvidersMessage::SubsonicRemove(index),
    );

    server_card(
        "Subsonic",
        &server.name,
        connection_status,
        body.into(),
        actions,
    )
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// A labelled form field: small dimmed label above the input.
fn field<'a>(
    label: String,
    input: impl Into<cosmic::Element<'a, ProvidersMessage>>,
    width: Length,
) -> cosmic::Element<'a, ProvidersMessage> {
    widget::Column::new()
        .push(widget::text::caption(label).class(dim_text()))
        .push(input)
        .spacing(2)
        .width(width)
        .into()
}

/// Per-server card: kind + name header with a connection status indicator,
/// the form body, a divider, the action row and (after a failed test) the
/// error detail.
fn server_card<'a>(
    kind: &'static str,
    name: &'a str,
    connection_status: Option<&'a str>,
    body: cosmic::Element<'a, ProvidersMessage>,
    actions: cosmic::Element<'a, ProvidersMessage>,
) -> cosmic::Element<'a, ProvidersMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let title = if name.trim().is_empty() { kind } else { name };

    let header = widget::Row::new()
        .push(
            widget::Column::new()
                .push(widget::text::title4(title))
                .push(widget::text::caption(kind).class(dim_text()))
                .width(Length::Fill),
        )
        .push(status_badge(connection_status))
        .spacing(sp.space_xs)
        .align_y(Alignment::Center);

    let mut col = widget::Column::new()
        .push(header)
        .push(widget::divider::horizontal::default())
        .push(body)
        .push(widget::divider::horizontal::default())
        .push(actions)
        .spacing(sp.space_s);

    if let Some(status) = connection_status
        && !is_connected(status)
    {
        let failed = fl!("connection-failed");
        let detail = status
            .strip_prefix(failed.as_str())
            .map(|rest| rest.trim_start_matches(':').trim())
            .filter(|rest| !rest.is_empty());
        if let Some(detail) = detail {
            col = col.push(
                widget::text::caption(detail.to_string())
                    .class(cosmic::theme::Text::Color(destructive_color())),
            );
        }
    }

    widget::container(col)
        .padding(sp.space_s)
        .width(Length::Fill)
        .class(cosmic::theme::Container::Card)
        .into()
}

/// Save (primary) / Test connection (secondary) on the left, a quiet red
/// Remove on the far right.
fn provider_action_buttons<'a>(
    save: ProvidersMessage,
    test: ProvidersMessage,
    remove: ProvidersMessage,
) -> cosmic::Element<'a, ProvidersMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;

    let remove_button = widget::button::custom(
        widget::text::body(fl!("remove")).class(cosmic::theme::Text::Color(destructive_color())),
    )
    .padding([sp.space_xxs, sp.space_s])
    .class(cosmic::theme::Button::Text)
    .on_press(remove);

    widget::Row::new()
        .push(widget::button::suggested(fl!("save")).on_press(save))
        .push(widget::button::standard(fl!("test-connection")).on_press(test))
        .push(widget::space::horizontal())
        .push(remove_button)
        .spacing(sp.space_xs)
        .align_y(Alignment::Center)
        .into()
}

fn destructive_color() -> Color {
    cosmic::theme::active().cosmic().destructive_color().into()
}

fn is_connected(status: &str) -> bool {
    status == crate::fl!("connected")
}

/// Status dot + short label: green "Connected", red "Connection Failed",
/// or a dim "Not tested" before any attempt.
fn status_badge<'a>(status: Option<&str>) -> cosmic::Element<'a, ProvidersMessage> {
    let sp = cosmic::theme::active().cosmic().spacing;
    let theme = cosmic::theme::active();
    let (color, label) = match status {
        Some(s) if is_connected(s) => (
            Color::from(theme.cosmic().success_color()),
            fl!("connected"),
        ),
        Some(_) => (destructive_color(), fl!("connection-failed")),
        None => (
            Color::from(theme.cosmic().palette.neutral_7),
            fl!("provider-not-tested"),
        ),
    };

    let dot = widget::container(widget::Space::new())
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .class(cosmic::theme::Container::custom(move |_theme| {
            cosmic::iced::widget::container::Style {
                background: Some(cosmic::iced::Background::Color(color)),
                border: cosmic::iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        }));

    widget::Row::new()
        .push(dot)
        .push(widget::text::caption(label).class(cosmic::theme::Text::Color(color)))
        .spacing(sp.space_xxs)
        .align_y(Alignment::Center)
        .into()
}
