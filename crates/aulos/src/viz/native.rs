//! The desktop's host of the render loop: a thread of its own, paced to
//! [`FRAME_PERIOD`], that lives from [`Host::start`] to [`Host::stop`].

use super::{FRAME_PERIOD, Inputs, Loop};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use web_time::Instant;

pub struct Host {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Host {
    /// Spawns the render thread.
    pub fn start(inputs: Inputs) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("lactea-render".into())
                .spawn(move || run(&stop, inputs))
                .map_err(|e| tracing::error!("lactea: cannot spawn the render thread: {e}"))
                .ok()
        };
        Self { stop, thread }
    }

    /// The thread runs by itself: nothing to do per UI frame.
    pub fn drive(&mut self) {}

    /// Stops the thread and waits for it.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(stop: &AtomicBool, inputs: Inputs) {
    let Some(mut state) = Loop::new(inputs) else {
        return;
    };
    let mut next_frame = Instant::now() + FRAME_PERIOD;
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if let Some(wait) = next_frame.checked_duration_since(now) {
            std::thread::sleep(wait);
            next_frame += FRAME_PERIOD;
        } else {
            next_frame = now + FRAME_PERIOD;
        }
        state.tick();
    }
}
