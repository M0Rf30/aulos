// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Direct symphonia (DSD-capable fork) + cpal audio engine, replacing rodio.
//!
//! Ported from `~/M0Rf30/rmpd`'s `rmpd-player` crate — see
//! `local://rmpd_player_report.md` (or re-derive from
//! `~/M0Rf30/rmpd/rmpd-player/src/`) for the architecture this is based on.
//! Key difference from rodio: DSD files can be output via DoP (DSD-over-PCM)
//! on a single dedicated code path that never touches gain/volume/EQ/resampling
//! — that separation is what guarantees a DoP stream reaches the DAC
//! unprocessed instead of risking corruption from an accidental shared
//! mixing/DSP stage.

// The decoding half (symphonia, resampling, filters, fades) lives in the
// shared `playcore` crate. What drives a sound card (cpal, the playback
// thread) exists natively only; the browser's output is `local_backend`'s
// business.
#[cfg(not(target_arch = "wasm32"))]
pub mod cpal_utils;
#[cfg(not(target_arch = "wasm32"))]
pub mod dop_output;
// `engine::engine` intentionally mirrors rmpd's module layout; renaming would
// churn every `use` path across the crate (including app.rs).
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::module_inception)]
pub mod engine;
#[cfg(not(target_arch = "wasm32"))]
pub mod output;
#[cfg(not(target_arch = "wasm32"))]
pub mod sample_buffer;
