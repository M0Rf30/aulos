// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Inhibit suspend / idle while music plays (Lollypop's `power-management`
//! setting, `inhibitor.py`).
//!
//! Preferred path is the XDG desktop portal (`org.freedesktop.portal.Inhibit`
//! through `ashpd`), which works both sandboxed and natively. Not every
//! desktop ships a portal backend for it, so on failure this falls back to a
//! systemd-logind `sleep:idle` "block" inhibitor (a file descriptor held
//! open for as long as the inhibition should last).
//!
//! The held inhibition is represented by an [`InhibitToken`]; dropping every
//! clone of it releases a logind lock (closing the fd), and
//! [`InhibitToken::release`] additionally closes a portal request
//! explicitly.

use crate::player::PlaybackState;
use std::sync::Arc;
use std::time::Duration;

/// Human-readable reason shown by the desktop in its "inhibited by" lists.
pub const REASON: &str = "Playing music";

/// How long the portal gets to answer before falling back to logind.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(4);

/// Whether an inhibition should be held right now: the user enabled it, and
/// audio is audibly playing through the local engine. (Paused/stopped
/// playback, or a remote MPD server playing elsewhere, must not keep this
/// machine awake.)
pub fn should_inhibit(enabled: bool, state: PlaybackState, local_backend: bool) -> bool {
    enabled && local_backend && state == PlaybackState::Playing
}

enum Inhibitor {
    Portal(Box<ashpd::desktop::Request<()>>),
    /// Held only for its `Drop` (closes the logind inhibitor fd).
    Logind(#[allow(dead_code)] zbus::zvariant::OwnedFd),
}

/// A held suspend/idle inhibition. Cheap to clone (shared handle).
#[derive(Clone)]
pub struct InhibitToken(Arc<Inhibitor>);

impl std::fmt::Debug for InhibitToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &*self.0 {
            Inhibitor::Portal(_) => "portal",
            Inhibitor::Logind(_) => "logind",
        };
        f.debug_tuple("InhibitToken").field(&kind).finish()
    }
}

impl InhibitToken {
    /// Release the inhibition now. Safe to call while other clones exist:
    /// a portal request is closed explicitly; a logind lock is released
    /// once the last clone is dropped.
    pub async fn release(self) {
        if let Inhibitor::Portal(request) = &*self.0
            && let Err(e) = request.close().await
        {
            tracing::debug!("closing portal inhibit request failed: {e}");
        }
    }
}

/// Take a suspend+idle inhibition. `None` when no mechanism is available
/// (logged at `warn`; playback is unaffected).
pub async fn acquire(reason: &str) -> Option<InhibitToken> {
    match tokio::time::timeout(PORTAL_TIMEOUT, acquire_portal(reason)).await {
        Ok(Ok(request)) => {
            tracing::debug!("inhibiting suspend/idle via the desktop portal");
            return Some(InhibitToken(Arc::new(Inhibitor::Portal(Box::new(request)))));
        }
        Ok(Err(e)) => tracing::debug!("portal inhibit unavailable ({e}); trying logind"),
        Err(_) => tracing::debug!("portal inhibit timed out; trying logind"),
    }
    match acquire_logind(reason).await {
        Ok(fd) => {
            tracing::debug!("inhibiting suspend/idle via logind");
            Some(InhibitToken(Arc::new(Inhibitor::Logind(fd))))
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
