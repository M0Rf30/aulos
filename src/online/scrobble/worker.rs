// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Background worker: owns the offline queue and the backends, and retries
//! failed submissions with exponential backoff.
//!
//! All network I/O happens on a dedicated thread so the UI never blocks.
//! [`Engine`] holds the (testable) logic; [`WorkerHandle`] is the cloneable
//! front the app talks to.

use super::audioscrobbler::{self, Audioscrobbler, Endpoint};
use super::listenbrainz::ListenBrainz;
use super::queue::Queue;
use super::{ScrobbleError, ScrobbleTrack, Scrobbler, Service, keys, unix_now};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

const BASE_BACKOFF: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// Bad credentials won't fix themselves quickly: retry rarely.
const AUTH_BACKOFF: Duration = Duration::from_secs(10 * 60);
/// Pause between bulk love/unlove calls (loved-track sync).
const BULK_LOVE_PAUSE: Duration = Duration::from_millis(300);

/// Delay before the next attempt after `attempts` consecutive failures
/// (1 → 30 s, doubling, capped at 30 min).
pub fn backoff_delay(attempts: u32) -> Duration {
    let shift = attempts.saturating_sub(1).min(16);
    BASE_BACKOFF.saturating_mul(1u32 << shift).min(MAX_BACKOFF)
}

/// Which services the worker should talk to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerSettings {
    pub listenbrainz: bool,
    pub lastfm: bool,
    pub librefm: bool,
    pub lastfm_api_key: String,
}

pub enum Cmd {
    Configure(WorkerSettings),
    NowPlaying(ScrobbleTrack),
    Scrobble(ScrobbleTrack),
    Love {
        artist: String,
        title: String,
        loved: bool,
        bulk: bool,
    },
    /// Forget everything queued for a service (on disconnect).
    Purge(Service),
    /// Retry queued listens now, ignoring backoff.
    FlushNow,
}

/// State shared with the UI for status display.
#[derive(Debug, Default)]
pub struct Shared {
    pub queued: HashMap<Service, usize>,
    pub errors: HashMap<Service, String>,
}

#[derive(Clone)]
pub struct WorkerHandle {
    tx: mpsc::Sender<Cmd>,
    shared: Arc<Mutex<Shared>>,
}

impl WorkerHandle {
    /// Start the worker thread, loading the queue from `queue_path`.
    pub fn spawn(queue_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let thread_shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("aulos-scrobbler".into())
            .spawn(move || {
                let queue = Queue::load(&queue_path);
                let mut engine = Engine::new(queue, thread_shared);
                run(&mut engine, rx);
            });
        if let Err(e) = spawned {
            tracing::error!("failed to start scrobbler thread: {e}");
        }
        Self { tx, shared }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn queued(&self, service: Service) -> usize {
        self.shared
            .lock()
            .queued
            .get(&service)
            .copied()
            .unwrap_or(0)
    }

    pub fn error(&self, service: Service) -> Option<String> {
        self.shared.lock().errors.get(&service).cloned()
    }
}

fn run(engine: &mut Engine, rx: mpsc::Receiver<Cmd>) {
    loop {
        let wait = engine
            .next_retry_in(Instant::now())
            .unwrap_or(Duration::from_secs(3600));
        match rx.recv_timeout(wait) {
            Ok(Cmd::Configure(settings)) => {
                engine.set_backends(build_backends(&settings));
                engine.flush_all(Instant::now(), true);
            }
            Ok(Cmd::NowPlaying(t)) => engine.now_playing(&t),
            Ok(Cmd::Scrobble(t)) => engine.scrobble(t, Instant::now()),
            Ok(Cmd::Love {
                artist,
                title,
                loved,
                bulk,
            }) => {
                engine.love(&artist, &title, loved);
                if bulk {
                    std::thread::sleep(BULK_LOVE_PAUSE);
                }
            }
            Ok(Cmd::Purge(service)) => engine.purge(service),
            Ok(Cmd::FlushNow) => engine.flush_all(Instant::now(), true),
            Err(RecvTimeoutError::Timeout) => engine.flush_all(Instant::now(), false),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Instantiate a backend for every enabled service that has credentials.
fn build_backends(settings: &WorkerSettings) -> Vec<Box<dyn Scrobbler>> {
    let secret = |id: String| {
        crate::credentials::retrieve_password(&id)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
    };
    let mut out: Vec<Box<dyn Scrobbler>> = Vec::new();
    if settings.listenbrainz
        && let Some(token) = secret(keys::listenbrainz_token())
    {
        out.push(Box::new(ListenBrainz::new(token)));
    }
    let lastfm_secret = secret(keys::lastfm_secret()).unwrap_or_default();
    for (enabled, service) in [
        (settings.lastfm, Service::LastFm),
        (settings.librefm, Service::LibreFm),
    ] {
        if !enabled {
            continue;
        }
        let Some(endpoint) = Endpoint::for_service(service) else {
            continue;
        };
        let Some(session) = secret(keys::session_key(service)) else {
            continue;
        };
        let (key, sec) =
            audioscrobbler::api_credentials(service, &settings.lastfm_api_key, &lastfm_secret);
        if key.is_empty() || sec.is_empty() {
            continue;
        }
        out.push(Box::new(Audioscrobbler::new(endpoint, key, sec, session)));
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct Backoff {
    attempts: u32,
    next: Instant,
}

/// Queue + backends + retry state. Single-threaded; driven by [`run`].
pub struct Engine {
    backends: Vec<Box<dyn Scrobbler>>,
    queue: Queue,
    backoff: HashMap<Service, Backoff>,
    shared: Arc<Mutex<Shared>>,
}

impl Engine {
    pub fn new(mut queue: Queue, shared: Arc<Mutex<Shared>>) -> Self {
        queue.prune(unix_now());
        let engine = Self {
            backends: Vec::new(),
            queue,
            backoff: HashMap::new(),
            shared,
        };
        engine.publish();
        engine
    }

    pub fn set_backends(&mut self, backends: Vec<Box<dyn Scrobbler>>) {
        self.backends = backends;
        self.backoff.clear();
        self.shared.lock().errors.clear();
        self.publish();
    }

    fn publish(&self) {
        let mut s = self.shared.lock();
        for service in Service::ALL {
            s.queued.insert(service, self.queue.count(service));
        }
    }

    fn set_error(&self, service: Service, error: Option<String>) {
        let mut s = self.shared.lock();
        match error {
            Some(e) => {
                s.errors.insert(service, e);
            }
            None => {
                s.errors.remove(&service);
            }
        }
    }

    fn save(&self) {
        if let Err(e) = self.queue.save() {
            tracing::warn!("failed to save scrobble queue: {e}");
        }
    }

    pub fn now_playing(&self, track: &ScrobbleTrack) {
        for b in &self.backends {
            if let Err(e) = b.now_playing(track) {
                tracing::debug!("{} now-playing failed: {e}", b.service().display_name());
                if let ScrobbleError::Auth(m) = e {
                    self.set_error(b.service(), Some(m));
                }
            }
        }
    }

    /// Queue a finished listen for every active service and try to send it.
    pub fn scrobble(&mut self, track: ScrobbleTrack, now: Instant) {
        let services: Vec<Service> = self.backends.iter().map(|b| b.service()).collect();
        if services.is_empty() {
            return;
        }
        for s in &services {
            self.queue.push(*s, track.clone());
        }
        self.save();
        self.publish();
        for s in services {
            self.flush_service(s, now, false);
        }
    }

    pub fn love(&self, artist: &str, title: &str, loved: bool) {
        for b in self
            .backends
            .iter()
            .filter(|b| b.supports_love() && b.service() == Service::LastFm)
        {
            if let Err(e) = b.set_loved(artist, title, loved) {
                tracing::warn!("{} love failed: {e}", b.service().display_name());
            }
        }
    }

    pub fn purge(&mut self, service: Service) {
        self.queue.purge(service);
        self.backoff.remove(&service);
        self.save();
        self.set_error(service, None);
        self.publish();
    }

    /// Try every service with something queued.
    pub fn flush_all(&mut self, now: Instant, force: bool) {
        if self.queue.prune(unix_now()) > 0 {
            self.save();
            self.publish();
        }
        let services: Vec<Service> = self.backends.iter().map(|b| b.service()).collect();
        for s in services {
            self.flush_service(s, now, force);
        }
    }

    /// Time until the earliest scheduled retry, if any queue is waiting.
    pub fn next_retry_in(&self, now: Instant) -> Option<Duration> {
        self.backends
            .iter()
            .map(|b| b.service())
            .filter(|s| self.queue.count(*s) > 0)
            .filter_map(|s| self.backoff.get(&s))
            .map(|b| b.next.saturating_duration_since(now))
            .min()
    }

    fn flush_service(&mut self, service: Service, now: Instant, force: bool) {
        if !force
            && let Some(b) = self.backoff.get(&service)
            && b.next > now
        {
            return;
        }
        let Some(idx) = self.backends.iter().position(|b| b.service() == service) else {
            return;
        };
        let mut changed = false;
        loop {
            let batch = self.queue.pending(service, self.backends[idx].max_batch());
            if batch.is_empty() {
                self.backoff.remove(&service);
                self.set_error(service, None);
                break;
            }
            let (done, error) = submit_batch(self.backends[idx].as_ref(), &batch);
            if done > 0 {
                self.queue.remove_front(service, done);
                changed = true;
            }
            match error {
                None => {
                    self.backoff.remove(&service);
                    self.set_error(service, None);
                }
                Some(e) => {
                    let attempts = self.backoff.get(&service).map_or(0, |b| b.attempts) + 1;
                    let mut delay = backoff_delay(attempts);
                    if matches!(e, ScrobbleError::Auth(_)) {
                        delay = delay.max(AUTH_BACKOFF);
                    }
                    tracing::warn!(
                        "{} submit failed (attempt {attempts}, retry in {delay:?}): {e}",
                        service.display_name()
                    );
                    self.backoff.insert(
                        service,
                        Backoff {
                            attempts,
                            next: now + delay,
                        },
                    );
                    self.set_error(service, Some(e.to_string()));
                    break;
                }
            }
        }
        if changed {
            self.save();
        }
        self.publish();
    }
}

/// Submit `batch`; returns how many leading listens are settled (accepted or
/// permanently rejected) and the error that stopped progress, if any.
fn submit_batch(
    backend: &dyn Scrobbler,
    batch: &[ScrobbleTrack],
) -> (usize, Option<ScrobbleError>) {
    match backend.submit(batch) {
        Ok(()) => (batch.len(), None),
        Err(ScrobbleError::Rejected(m)) if batch.len() > 1 => {
            tracing::debug!("batch rejected ({m}); retrying listens one by one");
            let mut done = 0;
            for t in batch {
                match backend.submit(std::slice::from_ref(t)) {
                    Ok(()) | Err(ScrobbleError::Rejected(_)) => done += 1,
                    Err(e) => return (done, Some(e)),
                }
            }
            (done, None)
        }
        Err(ScrobbleError::Rejected(m)) => {
            tracing::warn!("listen rejected, dropping: {m}");
            (batch.len(), None)
        }
        Err(e) => (0, Some(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Scripted backend: pops one result per `submit` call (default Ok).
    struct Fake {
        service: Service,
        script: Mutex<Vec<Result<(), ScrobbleError>>>,
        accepted: Arc<Mutex<Vec<String>>>,
        calls: Arc<AtomicUsize>,
        batch: usize,
        /// Titles that are rejected when submitted alone.
        poison: Vec<&'static str>,
    }

    impl Scrobbler for Fake {
        fn service(&self) -> Service {
            self.service
        }
        fn now_playing(&self, _: &ScrobbleTrack) -> Result<(), ScrobbleError> {
            Ok(())
        }
        fn submit(&self, tracks: &[ScrobbleTrack]) -> Result<(), ScrobbleError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(r) = {
                let mut s = self.script.lock();
                (!s.is_empty()).then(|| s.remove(0))
            } {
                r?;
            }
            if tracks
                .iter()
                .any(|t| self.poison.contains(&t.title.as_str()))
            {
                if tracks.len() == 1 {
                    return Err(ScrobbleError::Rejected("bad".into()));
                }
                return Err(ScrobbleError::Rejected("batch has bad".into()));
            }
            let mut a = self.accepted.lock();
            a.extend(tracks.iter().map(|t| t.title.clone()));
            Ok(())
        }
        fn max_batch(&self) -> usize {
            self.batch
        }
    }

    struct Rig {
        engine: Engine,
        accepted: Arc<Mutex<Vec<String>>>,
        calls: Arc<AtomicUsize>,
        shared: Arc<Mutex<Shared>>,
    }

    fn rig(script: Vec<Result<(), ScrobbleError>>, poison: Vec<&'static str>, batch: usize) -> Rig {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let accepted = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut engine = Engine::new(Queue::in_memory(), Arc::clone(&shared));
        engine.set_backends(vec![Box::new(Fake {
            service: Service::LastFm,
            script: Mutex::new(script),
            accepted: Arc::clone(&accepted),
            calls: Arc::clone(&calls),
            batch,
            poison,
        })]);
        Rig {
            engine,
            accepted,
            calls,
            shared,
        }
    }

    fn tr(title: &str) -> ScrobbleTrack {
        ScrobbleTrack {
            artist: "A".into(),
            title: title.into(),
            album: String::new(),
            album_artist: String::new(),
            duration_secs: 200,
            track_number: 0,
            timestamp: unix_now(),
        }
    }

    fn queued(r: &Rig) -> usize {
        r.shared.lock().queued[&Service::LastFm]
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff_delay(1), Duration::from_secs(30));
        assert_eq!(backoff_delay(2), Duration::from_secs(60));
        assert_eq!(backoff_delay(3), Duration::from_secs(120));
        assert_eq!(backoff_delay(50), MAX_BACKOFF);
        assert_eq!(backoff_delay(0), Duration::from_secs(30));
    }

    #[test]
    fn accepted_scrobble_leaves_queue_empty() {
        let mut r = rig(vec![], vec![], 50);
        r.engine.scrobble(tr("one"), Instant::now());
        assert_eq!(*r.accepted.lock(), vec!["one"]);
        assert_eq!(queued(&r), 0);
    }

    #[test]
    fn transient_failure_keeps_listen_and_backs_off() {
        let mut r = rig(
            vec![Err(ScrobbleError::Transient("offline".into()))],
            vec![],
            50,
        );
        let t0 = Instant::now();
        r.engine.scrobble(tr("one"), t0);
        assert_eq!(queued(&r), 1);
        assert!(r.shared.lock().errors.contains_key(&Service::LastFm));
        let calls = r.calls.load(Ordering::SeqCst);

        // Still inside the backoff window: no new attempt.
        r.engine.flush_all(t0 + Duration::from_secs(10), false);
        assert_eq!(r.calls.load(Ordering::SeqCst), calls);
        assert_eq!(r.engine.next_retry_in(t0), Some(Duration::from_secs(30)));

        // After the window it retries and succeeds.
        r.engine.flush_all(t0 + Duration::from_secs(31), false);
        assert_eq!(queued(&r), 0);
        assert_eq!(*r.accepted.lock(), vec!["one"]);
        assert!(r.shared.lock().errors.is_empty());
    }

    #[test]
    fn failures_accumulate_while_offline_then_flush_in_batches() {
        let mut r = rig(
            vec![Err(ScrobbleError::Transient("offline".into()))],
            vec![],
            2,
        );
        let t0 = Instant::now();
        r.engine.scrobble(tr("a"), t0); // fails, backoff
        r.engine.scrobble(tr("b"), t0); // queued, in backoff
        r.engine.scrobble(tr("c"), t0);
        assert_eq!(queued(&r), 3);
        r.engine.flush_all(t0 + Duration::from_secs(60), false);
        assert_eq!(queued(&r), 0);
        assert_eq!(*r.accepted.lock(), vec!["a", "b", "c"]);
    }

    #[test]
    fn auth_failure_keeps_queue_with_long_backoff() {
        let mut r = rig(
            vec![Err(ScrobbleError::Auth("bad session".into()))],
            vec![],
            50,
        );
        let t0 = Instant::now();
        r.engine.scrobble(tr("one"), t0);
        assert_eq!(queued(&r), 1);
        assert!(r.engine.next_retry_in(t0).unwrap() >= AUTH_BACKOFF);
    }

    #[test]
    fn rejected_listen_is_dropped_not_retried() {
        let mut r = rig(vec![], vec!["bad"], 50);
        r.engine.scrobble(tr("bad"), Instant::now());
        assert_eq!(queued(&r), 0);
        assert!(r.accepted.lock().is_empty());
    }

    #[test]
    fn rejected_batch_isolates_the_bad_listen() {
        let mut r = rig(
            vec![Err(ScrobbleError::Transient("offline".into()))],
            vec!["bad"],
            50,
        );
        let t0 = Instant::now();
        r.engine.scrobble(tr("a"), t0);
        r.engine.scrobble(tr("bad"), t0);
        r.engine.scrobble(tr("c"), t0);
        r.engine.flush_all(t0 + Duration::from_secs(60), true);
        assert_eq!(queued(&r), 0);
        assert_eq!(*r.accepted.lock(), vec!["a", "c"]);
    }

    #[test]
    fn purge_clears_one_service() {
        let mut r = rig(vec![Err(ScrobbleError::Transient("x".into()))], vec![], 50);
        r.engine.scrobble(tr("a"), Instant::now());
        assert_eq!(queued(&r), 1);
        r.engine.purge(Service::LastFm);
        assert_eq!(queued(&r), 0);
        assert!(r.shared.lock().errors.is_empty());
    }

    #[test]
    fn no_backends_means_nothing_is_queued() {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut e = Engine::new(Queue::in_memory(), Arc::clone(&shared));
        e.scrobble(tr("a"), Instant::now());
        assert_eq!(shared.lock().queued[&Service::LastFm], 0);
    }
}
