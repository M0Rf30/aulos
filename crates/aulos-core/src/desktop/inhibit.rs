//! Inhibit suspend / idle while music plays (Lollypop's `power-management`
//! setting, `inhibitor.py`).
//!
//! Linux prefers the XDG desktop portal (`org.freedesktop.portal.Inhibit`
//! through `ashpd`), which works both sandboxed and natively. Not every
//! desktop ships a portal backend for it, so on failure it falls back to a
//! systemd-logind `sleep:idle` "block" inhibitor (a file descriptor held
//! open for as long as the inhibition should last). Windows
//! (`SetThreadExecutionState`) and macOS (an IOKit power assertion) go
//! through the `keepawake` crate. Other targets (the web build) have no
//! mechanism, so [`acquire`] returns `None`.
//!
//! The held inhibition is represented by an [`InhibitToken`]; dropping every
//! clone of it releases it, and [`InhibitToken::release`] additionally closes
//! a portal request explicitly.

use crate::player::PlaybackState;
use std::sync::Arc;

/// Human-readable reason shown by the desktop in its "inhibited by" lists.
pub const REASON: &str = "Playing music";

/// Whether an inhibition should be held right now: the user enabled it, and
/// audio is audibly playing through the local engine. (Paused/stopped
/// playback, or a remote MPD server playing elsewhere, must not keep this
/// machine awake.)
pub fn should_inhibit(enabled: bool, state: PlaybackState, local_backend: bool) -> bool {
    enabled && local_backend && state == PlaybackState::Playing
}

/// A held suspend/idle inhibition. Cheap to clone (shared handle).
#[derive(Clone)]
pub struct InhibitToken(Arc<backend::Inhibitor>);

impl std::fmt::Debug for InhibitToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("InhibitToken").field(&self.0.kind()).finish()
    }
}

impl InhibitToken {
    /// Release the inhibition now. Safe to call while other clones exist:
    /// a portal request is closed explicitly; any other lock is released
    /// once the last clone is dropped.
    pub async fn release(self) {
        self.0.release().await;
    }
}

/// Take a suspend+idle inhibition. `None` when no mechanism is available
/// (logged at `warn`; playback is unaffected).
pub async fn acquire(reason: &str) -> Option<InhibitToken> {
    backend::acquire(reason)
        .await
        .map(|inhibitor| InhibitToken(Arc::new(inhibitor)))
}

#[cfg(target_os = "linux")]
mod backend {
    use std::time::Duration;

    /// How long the portal gets to answer before falling back to logind.
    const PORTAL_TIMEOUT: Duration = Duration::from_secs(4);

    pub enum Inhibitor {
        Portal(Box<ashpd::desktop::Request<()>>),
        /// Held only for its `Drop` (closes the logind inhibitor fd).
        Logind(#[allow(dead_code)] zbus::zvariant::OwnedFd),
    }

    impl Inhibitor {
        pub fn kind(&self) -> &'static str {
            match self {
                Self::Portal(_) => "portal",
                Self::Logind(_) => "logind",
            }
        }

        pub async fn release(&self) {
            if let Self::Portal(request) = self
                && let Err(e) = request.close().await
            {
                tracing::debug!("closing portal inhibit request failed: {e}");
            }
        }
    }

    pub async fn acquire(reason: &str) -> Option<Inhibitor> {
        match tokio::time::timeout(PORTAL_TIMEOUT, acquire_portal(reason)).await {
            Ok(Ok(request)) => {
                tracing::debug!("inhibiting suspend/idle via the desktop portal");
                return Some(Inhibitor::Portal(Box::new(request)));
            }
            Ok(Err(e)) => tracing::debug!("portal inhibit unavailable ({e}); trying logind"),
            Err(_) => tracing::debug!("portal inhibit timed out; trying logind"),
        }
        match acquire_logind(reason).await {
            Ok(fd) => {
                tracing::debug!("inhibiting suspend/idle via logind");
                Some(Inhibitor::Logind(fd))
            }
            Err(e) => {
                tracing::warn!("could not inhibit suspend/idle: {e}");
                None
            }
        }
    }

    async fn acquire_portal(reason: &str) -> Result<ashpd::desktop::Request<()>, ashpd::Error> {
        use ashpd::desktop::inhibit::{InhibitFlags, InhibitOptions, InhibitProxy};
        let proxy = InhibitProxy::new().await?;
        proxy
            .inhibit(
                None,
                InhibitFlags::Suspend | InhibitFlags::Idle,
                InhibitOptions::default().set_reason(reason),
            )
            .await
    }

    async fn acquire_logind(reason: &str) -> zbus::Result<zbus::zvariant::OwnedFd> {
        let conn = zbus::Connection::system().await?;
        let reply = conn
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "Inhibit",
                &("sleep:idle", "Aulos", reason, "block"),
            )
            .await?;
        reply.body().deserialize::<zbus::zvariant::OwnedFd>()
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod backend {
    /// A live `keepawake` lock. Windows ties the execution state to the
    /// thread that set it, so the lock lives on a thread of its own that
    /// waits for the sender below to drop, then lets go of it there.
    pub struct Inhibitor {
        _release: std::sync::mpsc::Sender<()>,
    }

    impl Inhibitor {
        pub fn kind(&self) -> &'static str {
            "keepawake"
        }

        /// Nothing to close explicitly: dropping the last clone ends the lock.
        pub async fn release(&self) {}
    }

    pub async fn acquire(reason: &str) -> Option<Inhibitor> {
        let reason = reason.to_owned();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("aulos-keepawake".into())
            .spawn(move || {
                let awake = keepawake::Builder::default()
                    .idle(true)
                    .reason(reason)
                    .app_name("Aulos")
                    .app_reverse_domain(crate::config::APP_ID)
                    .create();
                match awake {
                    Ok(awake) => {
                        let _ = ready_tx.send(Ok(()));
                        // Ends when the `Inhibitor` is dropped.
                        let _ = release_rx.recv();
                        drop(awake);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                    }
                }
            })
            .ok()?;
        match ready_rx.await {
            Ok(Ok(())) => {
                tracing::debug!("inhibiting suspend/idle via keepawake");
                Some(Inhibitor {
                    _release: release_tx,
                })
            }
            Ok(Err(e)) => {
                tracing::warn!("could not inhibit suspend/idle: {e}");
                None
            }
            Err(_) => None,
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod backend {
    /// No inhibition mechanism on this target: never constructed.
    pub enum Inhibitor {}

    impl Inhibitor {
        pub fn kind(&self) -> &'static str {
            match *self {}
        }

        pub async fn release(&self) {
            match *self {}
        }
    }

    pub async fn acquire(_reason: &str) -> Option<Inhibitor> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inhibits_only_while_audibly_playing_locally() {
        use PlaybackState::*;
        assert!(should_inhibit(true, Playing, true));
        assert!(!should_inhibit(true, Paused, true));
        assert!(!should_inhibit(true, Stopped, true));
        assert!(!should_inhibit(false, Playing, true), "setting off");
        assert!(!should_inhibit(true, Playing, false), "remote MPD playback");
    }
}
