// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! The system tray icon ("close/minimize to tray").
//!
//! On Linux it is a StatusNotifierItem (KDE Plasma, GNOME with the
//! AppIndicator extension, waybar, …) served over D-Bus by `ksni`: pure Rust,
//! no GTK. The icon's menu and clicks come back to the app as
//! [`TrayEvent`]s; the app owns the window and the playback session and
//! carries them out. Other platforms have no tray: [`SUPPORTED`] is `false`
//! and [`TrayIcon::start`] fails.

use aulos_core::task::Spawner;

/// Whether this platform can show a tray icon.
pub const SUPPORTED: bool = cfg!(target_os = "linux");

/// What the user asked of the app through the tray icon.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // only the Linux tray sends them
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Show the window if it is hidden or minimized, hide it otherwise
    /// (left click and the menu's "Show" / "Hide" entry).
    Toggle,
    PlayPause,
    Next,
    Previous,
    /// Quit for real, whatever the close setting says.
    Quit,
}

/// Something the tray reports to the app.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // only the Linux tray sends them
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Command(TrayCommand),
    /// The desktop's tray host went away: the icon is not visible any more,
    /// so the window must not stay hidden.
    Offline,
    /// The host is back.
    Online,
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Spawner, TrayCommand, TrayEvent};
    use aulos_core::fl;
    use ksni::menu::StandardItem;
    use ksni::{Icon, MenuItem, OfflineReason, Tray, TrayMethods};
    use std::sync::Arc;

    type Notify = Arc<dyn Fn(TrayEvent) + Send + Sync>;

    pub struct AulosTray {
        notify: Notify,
        /// The window is shown (not hidden to the tray): picks the label of
        /// the Show/Hide entry.
        window_visible: bool,
        icon: Vec<Icon>,
    }

    impl AulosTray {
        fn send(&self, command: TrayCommand) {
            (self.notify)(TrayEvent::Command(command));
        }
    }

    impl Tray for AulosTray {
        fn id(&self) -> String {
            "io.github.m0rf30.Aulos".into()
        }

        fn title(&self) -> String {
            "Aulos".into()
        }

        fn icon_pixmap(&self) -> Vec<Icon> {
            self.icon.clone()
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayCommand::Toggle);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let item = |label: String, icon: &str, command: TrayCommand| -> MenuItem<Self> {
                StandardItem {
                    label,
                    icon_name: icon.into(),
                    activate: Box::new(move |tray: &mut Self| tray.send(command)),
                    ..Default::default()
                }
                .into()
            };
            let toggle = if self.window_visible {
                fl!("tray-hide")
            } else {
                fl!("tray-show")
            };
            vec![
                item(toggle, "view-restore", TrayCommand::Toggle),
                MenuItem::Separator,
                item(
                    fl!("tray-play-pause"),
                    "media-playback-start",
                    TrayCommand::PlayPause,
                ),
                item(fl!("tray-next"), "media-skip-forward", TrayCommand::Next),
                item(
                    fl!("tray-previous"),
                    "media-skip-backward",
                    TrayCommand::Previous,
                ),
                MenuItem::Separator,
                item(fl!("tray-quit"), "application-exit", TrayCommand::Quit),
            ]
        }

        fn watcher_offline(&self, _reason: OfflineReason) -> bool {
            (self.notify)(TrayEvent::Offline);
            // Keep the service: shells restart, and the watcher comes back.
            true
        }

        fn watcher_online(&self) {
            (self.notify)(TrayEvent::Online);
        }
    }

    /// The app icon as an ARGB32 (network byte order) SNI pixmap, so the
    /// icon shows without being installed in a theme.
    fn icon_pixmap() -> Vec<Icon> {
        let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png"))
        else {
            return Vec::new();
        };
        let mut data = icon.rgba;
        for px in data.as_chunks_mut::<4>().0 {
            px.rotate_right(1); // RGBA -> ARGB
        }
        vec![Icon {
            width: icon.width as i32,
            height: icon.height as i32,
            data,
        }]
    }

    pub struct Handle(ksni::Handle<AulosTray>);

    impl Handle {
        pub async fn start(notify: Notify) -> Result<Self, String> {
            let tray = AulosTray {
                notify,
                window_visible: true,
                icon: icon_pixmap(),
            };
            // Inside a Flatpak the well-known bus name cannot be owned.
            let sandboxed = std::path::Path::new("/.flatpak-info").exists();
            tray.disable_dbus_name(sandboxed)
                .spawn()
                .await
                .map(Self)
                .map_err(|error| error.to_string())
        }

        pub fn set_window_visible(&self, spawner: &Spawner, visible: bool) {
            let handle = self.0.clone();
            spawner.spawn(async move {
                handle.update(|tray| tray.window_visible = visible).await;
            });
        }

        pub fn is_closed(&self) -> bool {
            self.0.is_closed()
        }

        pub fn shutdown(self, spawner: &Spawner) {
            spawner.spawn(async move {
                self.0.shutdown().await;
            });
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{Spawner, TrayEvent};
    use std::sync::Arc;

    pub struct Handle;

    impl Handle {
        pub async fn start(_notify: Arc<dyn Fn(TrayEvent) + Send + Sync>) -> Result<Self, String> {
            Err("no system tray on this platform".into())
        }

        pub fn set_window_visible(&self, _spawner: &Spawner, _visible: bool) {}

        pub fn is_closed(&self) -> bool {
            true
        }

        pub fn shutdown(self, _spawner: &Spawner) {}
    }
}

/// A running tray icon. Dropping it does not remove the icon: call
/// [`Self::shutdown`].
pub struct TrayIcon(imp::Handle);

impl TrayIcon {
    /// Registers the icon with the desktop; `notify` is called from the
    /// tray's own task for every [`TrayEvent`]. Fails when there is no tray
    /// host (no StatusNotifierWatcher) or no D-Bus session.
    pub async fn start(notify: impl Fn(TrayEvent) + Send + Sync + 'static) -> Result<Self, String> {
        imp::Handle::start(std::sync::Arc::new(notify))
            .await
            .map(Self)
    }

    /// Tells the menu whether the window is shown (its entry reads "Hide")
    /// or hidden ("Show").
    pub fn set_window_visible(&self, spawner: &Spawner, visible: bool) {
        self.0.set_window_visible(spawner, visible);
    }

    /// The tray service stopped by itself.
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    /// Removes the icon.
    pub fn shutdown(self, spawner: &Spawner) {
        self.0.shutdown(spawner);
    }
}
