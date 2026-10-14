// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! No media-session integration on this target (the web build): the stream
//! is empty, so no handle is ever published.

use futures::Stream;

use super::{MprisEvent, NativeWindow};

pub fn stream(_window: Option<NativeWindow>) -> impl Stream<Item = MprisEvent> {
    futures::stream::empty()
}
