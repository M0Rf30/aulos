//! The browser's host of the render loop: the page has one thread, so the
//! loop is stepped from the UI's frame callback ([`Host::drive`], called by
//! `Viz::poll`), at most once per [`FRAME_PERIOD`], and asks for the next
//! frame itself.

use super::{FRAME_PERIOD, Inputs, Loop};
use web_time::Instant;

pub struct Host {
    ctx: egui::Context,
    /// Waits for the first frame to be painted before the engine starts, so
    /// the page shows "Starting the visualizer…" while the engine builds its
    /// pipelines and the first preset compiles.
    pending: Option<Inputs>,
    painted: bool,
    state: Option<Box<Loop>>,
    last_tick: Instant,
}

impl Host {
    /// Prepares the loop; the engine starts on the second frame.
    pub fn start(inputs: Inputs) -> Self {
        // A broken preset can make wgpu report a validation error long after
        // the call that caused it. On the WebGL path the default reaction is a
        // panic, which in a page ends the whole app; log it instead (the
        // preset simply renders wrong, or the engine reports its own failure).
        inputs
            .device
            .on_uncaptured_error(std::sync::Arc::new(|error| {
                tracing::warn!("lactea: wgpu: {error}");
            }));
        Self {
            ctx: inputs.ctx.clone(),
            pending: Some(inputs),
            painted: false,
            state: None,
            last_tick: Instant::now(),
        }
    }

    /// Runs one turn of the loop if one is due, and keeps the frames coming.
    pub fn drive(&mut self) {
        if self.pending.is_some() && self.painted {
            self.state = self.pending.take().and_then(Loop::new).map(Box::new);
        }
        self.painted = true;
        if let Some(state) = self.state.as_mut() {
            // The browser's frames come with the display (60, 120, 144 Hz):
            // half a period tells a frame that is due from one that is not,
            // without skipping one that arrives a hair early.
            if self.last_tick.elapsed() >= FRAME_PERIOD / 2 {
                self.last_tick = Instant::now();
                state.tick();
            }
        }
        if self.pending.is_some() || self.state.is_some() {
            self.ctx.request_repaint();
        }
    }

    /// Drops the loop and with it the engine.
    pub fn stop(&mut self) {
        self.pending = None;
        self.state = None;
    }
}
