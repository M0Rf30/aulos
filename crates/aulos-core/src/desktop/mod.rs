// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Desktop-session integration that has nothing to do with the audio path:
//! opening web pages ([`browser`]), suspend/idle inhibition while playing ([`inhibit`]) and track-change
//! notifications ([`notify`]).

pub mod browser;
pub mod inhibit;
pub mod notify;
