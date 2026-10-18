// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Desktop-session integration that has nothing to do with the audio path:
//! opening web pages ([`browser`]), suspend/idle inhibition while playing
//! ([`inhibit`]), track-change notifications ([`notify`]) and the files handed
//! to the process at launch ([`open_args`]), and the name and icon the
//! PipeWire/PulseAudio mixers show for the audio stream ([`stream_identity`]).

pub mod browser;
pub mod inhibit;
pub mod notify;
pub mod open_args;
pub mod stream_identity;
