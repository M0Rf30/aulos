// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Wakes whoever is waiting on the app's event queues: the UI when a window
//! is open, the headless loop (see `host.rs`) while the window is closed to
//! the system tray.
//!
//! Background tasks hold a [`Wake`] instead of an `egui::Context`, because
//! the context belongs to one window and a window closed to the tray is a
//! new one, with a new context, when it comes back.

use egui::Context;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// A cloneable "something is waiting in a queue" signal.
#[derive(Clone)]
pub struct Wake(Arc<Shared>);

struct Shared {
    /// The open window's context; `None` while there is no window.
    ctx: Mutex<Option<Context>>,
    /// A wake-up arrived since the last [`Wake::wait`].
    pending: Mutex<bool>,
    signal: Condvar,
}

/// Locks a mutex that guards plain data: a panic elsewhere cannot leave it
/// inconsistent, so poisoning is ignored.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Wake {
    /// A signal that repaints `ctx`'s window.
    pub fn new(ctx: Context) -> Self {
        Self(Arc::new(Shared {
            ctx: Mutex::new(Some(ctx)),
            pending: Mutex::new(false),
            signal: Condvar::new(),
        }))
    }

    /// Repaints the window if there is one, and ends a [`Self::wait`].
    pub fn request_repaint(&self) {
        if let Some(ctx) = lock(&self.0.ctx).as_ref() {
            ctx.request_repaint();
        }
        *lock(&self.0.pending) = true;
        self.0.signal.notify_all();
    }

    /// Points the signal at a new window's context.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub fn attach(&self, ctx: Context) {
        *lock(&self.0.ctx) = Some(ctx);
    }

    /// The window is gone: wake-ups only end [`Self::wait`] from now on.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub fn detach(&self) {
        *lock(&self.0.ctx) = None;
    }

    /// Blocks until a wake-up arrives or `timeout` passes. A wake-up that
    /// came in since the last call returns at once.
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    pub fn wait(&self, timeout: std::time::Duration) {
        let mut pending = lock(&self.0.pending);
        if !*pending {
            pending = self
                .0
                .signal
                .wait_timeout(pending, timeout)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        *pending = false;
    }
}
