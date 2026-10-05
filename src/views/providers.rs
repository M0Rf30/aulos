// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Provider settings view — configure MPD and Subsonic servers.

use crate::fl;
use cosmic::iced::Alignment;
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

/// Render the providers settings panel (shown in the context drawer).
pub fn providers_view<'a>(
    mpd_servers: &'a [MpdEditState],
    mpd_connection_status: &'a [Option<String>],
    subsonic_servers: &'a [SubsonicEditState],
    subsonic_connection_status: &'a [Option<String>],
) -> cosmic::Element<'a, ProvidersMessage> {
    let mut col = widget::Column::new().spacing(16).padding(16);

    // Remote providers section
    let has_any = !mpd_servers.is_empty() || !subsonic_servers.is_empty();

    if !has_any {
        col = col.push(widget::text::body(fl!("no-providers")));
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

    // Add buttons
    col = col.push(
        widget::Row::new()
            .push(widget::button::text(fl!("add-mpd-server")).on_press(ProvidersMessage::AddMpd))
            .push(
                widget::button::text(fl!("add-subsonic-server"))
                    .on_press(ProvidersMessage::AddSubsonic),
            )
            .spacing(8),
    );

    col.into()
}

// ── MPD card ───────────────────────────────────────────────────────────────

fn mpd_server_card<'a>(
    index: usize,
    server: &'a MpdEditState,
    connection_status: Option<&'a str>,
) -> cosmic::Element<'a, ProvidersMessage> {
    let name_input = widget::text_input(fl!("mpd-name"), &server.name)
        .on_input(move |v| ProvidersMessage::EditName(index, v));

    let host_input = widget::text_input(fl!("mpd-host"), &server.host)
        .on_input(move |v| ProvidersMessage::EditHost(index, v));

    let port_input = widget::text_input(fl!("mpd-port"), &server.port)
        .on_input(move |v| ProvidersMessage::EditPort(index, v));

    let password_input = widget::text_input(fl!("mpd-password"), &server.password)
        .on_input(move |v| ProvidersMessage::EditPassword(index, v))
        .password();

    let buttons = provider_action_buttons(
        ProvidersMessage::Save(index),
        ProvidersMessage::TestConnection(index),
        ProvidersMessage::Remove(index),
        connection_status,
    );

    widget::Column::new()
        .push(widget::text::title4(format!("MPD: {}", server.name)))
        .push(name_input)
        .push(host_input)
        .push(
            widget::Row::new()
                .push(port_input)
                .push(password_input)
                .spacing(8),
        )
        .push(widget::divider::horizontal::default())
        .push(buttons)
        .spacing(8)
        .into()
}

// ── Subsonic card ──────────────────────────────────────────────────────────

fn subsonic_server_card<'a>(
    index: usize,
    server: &'a SubsonicEditState,
    connection_status: Option<&'a str>,
) -> cosmic::Element<'a, ProvidersMessage> {
    let name_input = widget::text_input(fl!("subsonic-name"), &server.name)
        .on_input(move |v| ProvidersMessage::SubsonicEditName(index, v));

    let url_input = widget::text_input(fl!("subsonic-url"), &server.url)
        .on_input(move |v| ProvidersMessage::SubsonicEditUrl(index, v));

    let username_input = widget::text_input(fl!("subsonic-username"), &server.username)
        .on_input(move |v| ProvidersMessage::SubsonicEditUsername(index, v));

    let password_input = widget::text_input(fl!("subsonic-password"), &server.password)
        .on_input(move |v| ProvidersMessage::SubsonicEditPassword(index, v))
        .password();

    let tls_toggle = widget::toggler(server.accept_invalid_certs)
        .label(fl!("subsonic-accept-invalid-certs"))
        .on_toggle(move |v| ProvidersMessage::SubsonicToggleCerts(index, v));

    // Save + Test Connection on the left, Remove pushed to the right
    let buttons = provider_action_buttons(
        ProvidersMessage::SubsonicSave(index),
        ProvidersMessage::SubsonicTestConnection(index),
        ProvidersMessage::SubsonicRemove(index),
        connection_status,
    );

    // Task 109: Transcoding controls — use a wrapping column layout to avoid overflow
    let bitrate_options: Vec<(Option<u32>, String)> = vec![
        (None, fl!("transcoding-original")),
        (Some(320), "320 kbps".to_string()),
        (Some(256), "256 kbps".to_string()),
        (Some(192), "192 kbps".to_string()),
        (Some(128), "128 kbps".to_string()),
        (Some(96), "96 kbps".to_string()),
        (Some(64), "64 kbps".to_string()),
    ];

    let format_options: Vec<(Option<String>, String)> = vec![
        (None, fl!("transcoding-original")),
        (Some("mp3".to_string()), "MP3".to_string()),
        (Some("ogg".to_string()), "OGG Vorbis".to_string()),
        (Some("opus".to_string()), "Opus".to_string()),
        (Some("aac".to_string()), "AAC".to_string()),
    ];

    let current_bitrate = server.transcoding_max_bitrate;
    let mut bitrate_children: Vec<cosmic::Element<ProvidersMessage>> =
        vec![widget::text::body(fl!("transcoding-bitrate")).into()];
    for (bitrate, label) in bitrate_options {
        let btn = if bitrate == current_bitrate {
            widget::button::standard(label)
        } else {
            widget::button::text(label)
        };
        bitrate_children.push(
            btn.on_press(ProvidersMessage::SubsonicTranscodingBitrate(index, bitrate))
                .into(),
        );
    }
    let bitrate_row = widget::flex_row(bitrate_children).spacing(4);

    let current_format = server.transcoding_format.clone();
    let mut format_children: Vec<cosmic::Element<ProvidersMessage>> =
        vec![widget::text::body(fl!("transcoding-format")).into()];
    for (fmt, label) in format_options {
        let btn = if fmt == current_format {
            widget::button::standard(label)
        } else {
            widget::button::text(label)
        };
        let f = fmt;
        format_children.push(
            btn.on_press(ProvidersMessage::SubsonicTranscodingFormat(index, f))
                .into(),
        );
    }
    let format_row = widget::flex_row(format_children).spacing(4);

    // Task 110: Bandwidth savings estimate
    let mut transcoding_col = widget::Column::new()
        .push(widget::text::title4(fl!("transcoding")))
        .push(bitrate_row)
        .push(format_row)
        .spacing(8);

    if let Some(bitrate) = current_bitrate {
        // Rough estimate: typical FLAC ~1000 kbps, so savings ≈ (1 - bitrate/1000) * 100
        let savings_pct = ((1.0 - (bitrate as f32 / 1000.0)) * 100.0).max(0.0) as u32;
        transcoding_col = transcoding_col.push(widget::text::caption(fl!(
            "transcoding-bandwidth-estimate",
            percent = savings_pct.to_string()
        )));
    }

    widget::Column::new()
        .push(widget::text::title4(format!("Subsonic: {}", server.name)))
        .push(name_input)
        .push(url_input)
        .push(
            widget::Row::new()
                .push(username_input)
                .push(password_input)
                .spacing(8),
        )
        .push(widget::container(tls_toggle).padding([8, 0]))
        .push(transcoding_col)
        .push(widget::divider::horizontal::default())
        .push(buttons)
        .spacing(8)
        .into()
}

// ── Helpers ───────────────────────────────────────────────────────────────
/// Save/Test-connection/status row, plus a Remove button pushed to the far
/// right. Identical scaffolding for both provider kinds' server cards.
fn provider_action_buttons<'a>(
    save: ProvidersMessage,
    test: ProvidersMessage,
    remove: ProvidersMessage,
    connection_status: Option<&'a str>,
) -> cosmic::Element<'a, ProvidersMessage> {
    let mut action_buttons = widget::Row::new().spacing(8).align_y(Alignment::Center);
    action_buttons = action_buttons.push(widget::button::standard(fl!("save")).on_press(save));
    action_buttons =
        action_buttons.push(widget::button::text(fl!("test-connection")).on_press(test));
    if let Some(status) = connection_status {
        action_buttons = action_buttons.push(status_label(status));
    }

    widget::Row::new()
        .push(action_buttons)
        .push(widget::space::horizontal())
        .push(widget::button::destructive(fl!("remove")).on_press(remove))
        .align_y(Alignment::Center)
        .into()
}

/// Render a connection status label with color coding.
///
/// Green for "Connected", red for anything else (connection failed + error).
fn status_label<'a, M: 'a>(status: &str) -> cosmic::Element<'a, M> {
    let connected_text = crate::fl!("connected");
    let is_connected = status == connected_text;

    let color = if is_connected {
        Color::from_rgb(0.2, 0.8, 0.2) // green
    } else {
        Color::from_rgb(0.9, 0.2, 0.2) // red
    };

    let dot = "● ";

    widget::text::caption(format!("{dot}{status}"))
        .class(cosmic::theme::Text::Color(color))
        .into()
}
