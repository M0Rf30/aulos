// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! UI-agnostic async plumbing between backend state machines and a UI.
//!
//! Backend controllers own their state and an `update(message)` method, and
//! hand back an [`Effect`]: the asynchronous work the UI must run, whose
//! messages it feeds back into the same `update`. The UI decides how to run
//! it (an iced `Task::stream`, a tokio task feeding an egui channel, …).

use futures::channel::mpsc;
use futures::stream::{self, BoxStream, Stream, StreamExt};

/// Asynchronous work producing zero or more messages of type `M`.
#[must_use = "an Effect does nothing unless the UI runs it"]
pub struct Effect<M>(Option<BoxStream<'static, M>>);

impl<M: Send + 'static> Effect<M> {
    /// No work.
    pub fn none() -> Self {
        Self(None)
    }

    /// One message, once `future` resolves.
    pub fn future(future: impl Future<Output = M> + Send + 'static) -> Self {
        Self(Some(stream::once(future).boxed()))
    }

    /// Every message of `stream`, in order.
    pub fn stream(stream: impl Stream<Item = M> + Send + 'static) -> Self {
        Self(Some(stream.boxed()))
    }

    /// Runs `f` on tokio's blocking pool and maps its result to a message.
    pub fn blocking<T: Send + 'static>(
        f: impl FnOnce() -> Result<T, String> + Send + 'static,
        to_msg: impl FnOnce(Result<T, String>) -> M + Send + 'static,
    ) -> Self {
        Self::future(async move {
            to_msg(
                tokio::task::spawn_blocking(f)
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
}

/// A stream fed through a bounded channel by the long-running `producer`,
/// which is driven by polling the stream itself (no separate task). The
/// stream ends once `producer` returns and every sent item is consumed.
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
pub fn interval_stream(period: std::time::Duration) -> impl Stream<Item = ()> {
    channel_stream(1, move |mut sender: mpsc::Sender<()>| async move {
        use futures::SinkExt;
        let mut interval = tokio::time::interval(period);
        loop {
            interval.tick().await;
            if sender.send(()).await.is_err() {
                break;
            }
        }
    })
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
}
