// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Filesystem watcher for the local music directories.

/// A browser page cannot watch the file system: no change is ever reported.
#[cfg(target_arch = "wasm32")]
pub fn watch_stream(
    _music_dirs: &[std::path::PathBuf],
) -> futures::stream::BoxStream<'static, Vec<std::path::PathBuf>> {
    futures::StreamExt::boxed(futures::stream::empty())
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::watch_stream;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use crate::task::channel_stream;
    use futures::stream::BoxStream;
    use futures::{SinkExt, StreamExt};
    use std::path::PathBuf;
    use std::time::Duration;

    /// Quiet time after the last filesystem event before a batch is emitted.
    const DEBOUNCE: Duration = Duration::from_secs(2);

    /// Watches `music_dirs` recursively with `notify::RecommendedWatcher` and
    /// yields the deduplicated, sorted set of changed paths once the filesystem
    /// has been quiet for two seconds. The watcher lives as long as the stream is
    /// polled; dropping the stream stops it.
    pub fn watch_stream(music_dirs: &[PathBuf]) -> BoxStream<'static, Vec<PathBuf>> {
        let music_dirs = music_dirs.to_vec();
        channel_stream(4, move |mut emitter| async move {
            use notify::{RecursiveMode, Watcher};

            let (tx, mut rx) = tokio::sync::mpsc::channel::<PathBuf>(256);

            // Create watcher that sends changed paths through the channel.
            // IMPORTANT: Only react to content changes (Create/Modify/Remove),
            // NOT Access events. Reading files during scanning triggers Access
            // events which would create an infinite scan loop.
            let _watcher = {
                let tx = tx.clone();
                let mut watcher = match notify::RecommendedWatcher::new(
                    move |result: Result<notify::Event, notify::Error>| {
                        if let Ok(event) = result {
                            use notify::EventKind;
                            match event.kind {
                                EventKind::Create(_)
                                | EventKind::Modify(_)
                                | EventKind::Remove(_) => {
                                    for path in event.paths {
                                        let _ = tx.blocking_send(path);
                                    }
                                }
                                _ => {}
                            }
                        }
                    },
                    notify::Config::default(),
                ) {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::error!("Failed to create filesystem watcher: {e}");
                        // Keep the future alive so the subscription ID is stable.
                        std::future::pending::<()>().await;
                        return;
                    }
                };

                for dir in &music_dirs {
                    if let Err(e) = watcher.watch(dir, RecursiveMode::Recursive) {
                        tracing::warn!("Failed to watch directory {}: {e}", dir.display());
                    }
                }

                watcher // keep alive
            };

            // Debounce loop: collect paths, wait for 2s of quiet, then emit.
            loop {
                // Wait for the first event.
                let first = match rx.recv().await {
                    Some(path) => path,
                    None => break, // channel closed
                };

                let mut changed_paths = vec![first];

                // Collect more events until 2 seconds of silence.
                loop {
                    match crate::task::timeout(DEBOUNCE, rx.recv()).await {
                        Ok(Some(path)) => {
                            changed_paths.push(path);
                        }
                        Ok(None) => break, // channel closed
                        Err(_) => break,   // timeout — debounce complete
                    }
                }

                // Deduplicate paths.
                changed_paths.sort();
                changed_paths.dedup();

                tracing::debug!(
                    "Filesystem watcher: {} changed paths after debounce",
                    changed_paths.len()
                );

                _ = emitter.send(changed_paths).await;
            }
        })
        .boxed()
    }
}
