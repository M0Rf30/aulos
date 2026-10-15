// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! UI-agnostic async plumbing between backend state machines and a UI.
//!
//! Backend controllers own their state and an `update(message)` method, and
//! hand back an [`Effect`]: the asynchronous work the UI must run, whose
//! messages it feeds back into the same `update`. The UI decides how to run
//! it (a tokio task feeding a UI channel, a toolkit-native stream, …).
//!
//! This module is also the seam to the platform's executor: natively work
//! runs on a tokio runtime ([`Spawner`]) and CPU/file-system work on its
//! blocking pool ([`spawn_blocking`]); in the browser there are no threads, so
//! tasks are `wasm-bindgen-futures` tasks and "blocking" work runs inline on
//! the main thread. [`sleep`], [`timeout`] and [`interval_stream`] use the
//! runtime's timers natively and `setTimeout` in the browser.

use std::time::Duration;

use futures::channel::mpsc;
use futures::stream::{self, BoxStream, Stream, StreamExt};

/// The work of [`spawn_blocking`] did not complete (it panicked, or the
/// runtime shut down).
#[derive(Debug)]
pub struct JoinError(String);

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JoinError {}

/// Runs `f` on tokio's blocking pool (natively; it starts at once, the result
/// is awaited). In the browser there is no other thread: `f` runs inline,
/// when the future is first polled.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn_blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> impl Future<Output = Result<T, JoinError>> + Send {
    let handle = tokio::task::spawn_blocking(f);
    async move { handle.await.map_err(|e| JoinError(e.to_string())) }
}

/// See the native version.
#[cfg(target_arch = "wasm32")]
pub async fn spawn_blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, JoinError> {
    Ok(f())
}

/// Runs `f` off the calling thread without waiting for it (inline in the
/// browser).
pub fn run_detached(f: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    drop(tokio::task::spawn_blocking(f));
    #[cfg(target_arch = "wasm32")]
    f();
}

/// Resolves after `duration`.
#[cfg(not(target_arch = "wasm32"))]
pub fn sleep(duration: Duration) -> impl Future<Output = ()> + Send {
    tokio::time::sleep(duration)
}

/// See the native version.
#[cfg(target_arch = "wasm32")]
pub fn sleep(duration: Duration) -> impl Future<Output = ()> + Send {
    // The timer future holds a JS closure, so it is not `Send`; there is a
    // single thread in the browser, which makes the wrapper sound.
    send_wrapper::SendWrapper::new(gloo_timers::future::sleep(duration))
}

/// `future`'s output, or `Err(Elapsed)` when it takes longer than `duration`.
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    use futures::future::{Either, select};
    futures::pin_mut!(future);
    let timer = sleep(duration);
    futures::pin_mut!(timer);
    match select(future, timer).await {
        Either::Left((output, _)) => Ok(output),
        Either::Right(((), _)) => Err(Elapsed),
    }
}

/// [`timeout`] ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl std::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("deadline has elapsed")
    }
}

impl std::error::Error for Elapsed {}

/// Where the UI runs [`Effect`]s and streams: a tokio runtime natively, the
/// browser's event loop on wasm.
#[derive(Clone)]
pub struct Spawner {
    #[cfg(not(target_arch = "wasm32"))]
    handle: tokio::runtime::Handle,
}

/// A task started by a [`Spawner`]; dropping it leaves the task running.
pub struct TaskHandle {
    #[cfg(not(target_arch = "wasm32"))]
    task: tokio::task::JoinHandle<()>,
    #[cfg(target_arch = "wasm32")]
    abort: futures::future::AbortHandle,
}

impl TaskHandle {
    /// Stops the task at its next await point.
    pub fn abort(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.task.abort();
        #[cfg(target_arch = "wasm32")]
        self.abort.abort();
    }

    /// Waits for the task to end (it ends early when aborted).
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn join(self) {
        let _ = self.task.await;
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Spawner {
    /// A spawner running on `handle`'s runtime.
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self { handle }
    }

    /// A spawner on the runtime the caller is inside of.
    ///
    /// # Panics
    /// Outside of a tokio runtime.
    pub fn current() -> Self {
        Self::new(tokio::runtime::Handle::current())
    }

    /// The runtime tasks run on.
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.handle
    }

    /// Runs `future` to completion in the background.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> TaskHandle {
        TaskHandle {
            task: self.handle.spawn(future),
        }
    }

    /// Runs `f` on the runtime's blocking pool.
    pub fn spawn_blocking(&self, f: impl FnOnce() + Send + 'static) {
        drop(self.handle.spawn_blocking(f));
    }
}

#[cfg(target_arch = "wasm32")]
impl Spawner {
    /// The browser's event loop.
    pub fn browser() -> Self {
        Self {}
    }

    /// Runs `future` to completion in the background.
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> TaskHandle {
        let (future, abort) = futures::future::abortable(future);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = future.await;
        });
        TaskHandle { abort }
    }

    /// Runs `f` inline: there is no blocking pool in the browser.
    pub fn spawn_blocking(&self, f: impl FnOnce() + Send + 'static) {
        f();
    }
}

/// Asynchronous work producing zero or more messages of type `M`.
#[must_use = "an Effect does nothing unless the UI runs it"]
pub struct Effect<M>(Option<BoxStream<'static, M>>);

impl<M: Send + 'static> Effect<M> {
    /// No work.
    pub fn none() -> Self {
        Self(None)
    }

    /// One message, once `future` resolves.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn future(future: impl Future<Output = M> + Send + 'static) -> Self {
        Self(Some(stream::once(future).boxed()))
    }

    /// One message, once `future` resolves. Browser futures (`fetch`, JS
    /// timers) hold JS handles and are not `Send`; the page has a single
    /// thread, so the future is only ever polled where it was created.
    #[cfg(target_arch = "wasm32")]
    pub fn future(future: impl Future<Output = M> + 'static) -> Self {
        Self(Some(
            stream::once(send_wrapper::SendWrapper::new(future)).boxed(),
        ))
    }

    /// Every message of `stream`, in order.
    pub fn stream(stream: impl Stream<Item = M> + Send + 'static) -> Self {
        Self(Some(stream.boxed()))
    }

    /// Runs `f` on the blocking pool and maps its result to a message.
    pub fn blocking<T: Send + 'static>(
        f: impl FnOnce() -> Result<T, String> + Send + 'static,
        to_msg: impl FnOnce(Result<T, String>) -> M + Send + 'static,
    ) -> Self {
        Self::future(async move {
            to_msg(
                spawn_blocking(f)
                    .await
                    .unwrap_or_else(|e| Err(e.to_string())),
            )
        })
    }

    /// Every effect of `effects`, run concurrently; messages arrive in
    /// completion order.
    pub fn batch(effects: impl IntoIterator<Item = Effect<M>>) -> Self {
        let mut streams: Vec<_> = effects.into_iter().filter_map(|e| e.0).collect();
        match streams.len() {
            0 => Self(None),
            1 => Self(streams.pop()),
            _ => Self(Some(stream::select_all(streams).boxed())),
        }
    }

    /// Converts every produced message with `f`.
    pub fn map<N: Send + 'static>(self, f: impl Fn(M) -> N + Send + 'static) -> Effect<N> {
        Effect(self.0.map(|s| s.map(f).boxed()))
    }

    /// Whether there is no work to run.
    pub fn is_none(&self) -> bool {
        self.0.is_none()
    }

    /// The work as a stream, or `None` when there is nothing to run.
    pub fn into_stream(self) -> Option<BoxStream<'static, M>> {
        self.0
    }

    /// Runs the work as a task on `spawner`, handing every message to
    /// `sink` in order: for UIs that are not built around an executor of
    /// their own (a sink that pushes into a channel and wakes the UI, say).
    /// Returns the task, to abort it, or `None` when there was nothing to
    /// run. UIs with their own executor use [`Self::into_stream`] instead.
    pub fn spawn(
        self,
        spawner: &Spawner,
        mut sink: impl FnMut(M) + Send + 'static,
    ) -> Option<TaskHandle> {
        let mut stream = self.0?;
        Some(spawner.spawn(async move {
            while let Some(message) = stream.next().await {
                sink(message);
            }
        }))
    }
}

/// A stream fed through a bounded channel by the long-running `producer`,
/// which is driven by polling the stream itself (no separate task). The
/// stream ends once `producer` returns and every sent item is consumed.
///
/// The stream is not necessarily `Unpin`: `.boxed()` it (or `Box::pin`) to
/// store it or poll it from a hand-written loop.
pub fn channel_stream<T, F, Fut>(size: usize, producer: F) -> impl Stream<Item = T>
where
    F: FnOnce(mpsc::Sender<T>) -> Fut,
    Fut: Future<Output = ()>,
{
    let (sender, receiver) = mpsc::channel(size);
    let runner = stream::once(producer(sender)).filter_map(|()| async { None });
    stream::select(receiver, runner)
}

/// A stream yielding `()` every `period`, the first one immediately, for as
/// long as it is polled. UIs map it to a "redraw/refresh" message.
///
/// Boxed (and so `Unpin`) because it is built to be stored and driven by
/// whichever runtime a UI uses; natively it needs a tokio runtime to be
/// entered while polled.
pub fn interval_stream(period: std::time::Duration) -> BoxStream<'static, ()> {
    channel_stream(1, move |mut sender: mpsc::Sender<()>| async move {
        use futures::SinkExt;
        loop {
            if sender.send(()).await.is_err() {
                break;
            }
            sleep(period).await;
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;

    #[tokio::test]
    async fn channel_stream_yields_every_item_then_ends() {
        let s = channel_stream(1, |mut tx: mpsc::Sender<u32>| async move {
            for i in 0..5 {
                tx.send(i).await.unwrap();
            }
        });
        assert_eq!(s.collect::<Vec<_>>().await, vec![0, 1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn effect_map_converts_messages_and_none_has_no_stream() {
        let e = Effect::stream(stream::iter([1, 2])).map(|n| n * 10);
        assert_eq!(
            e.into_stream().unwrap().collect::<Vec<_>>().await,
            vec![10, 20]
        );
        assert!(Effect::<u8>::none().into_stream().is_none());
    }

    #[tokio::test]
    async fn effect_spawn_delivers_every_message_to_the_sink() {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = Effect::stream(stream::iter([1, 2, 3]))
            .spawn(&Spawner::current(), move |n| {
                tx.send(n).unwrap();
            })
            .expect("work to run");
        handle.join().await;
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(
            Effect::<u8>::none()
                .spawn(&Spawner::current(), |_| {})
                .is_none()
        );
    }

    #[tokio::test]
    async fn interval_stream_is_unpin_and_ticks_immediately() {
        let mut ticks = interval_stream(std::time::Duration::from_secs(3600));
        assert_eq!(ticks.next().await, Some(()));
    }
}
