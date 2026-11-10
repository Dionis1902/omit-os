// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Waiting for any of several things.
//!
//! A thread in the `poll` system call sleeps on one global queue, and
//! everything it might be waiting for — data arriving in a pipe, a pipe
//! end closing, a connection to a listener, an input event — wakes that
//! queue. The woken thread checks its own descriptors again. That is
//! simple and correct; it wakes pollers more often than needed, which
//! does not matter with a handful of them.

use crate::sched::WaitQueue;

static ACTIVITY: WaitQueue = WaitQueue::new();

/// Something a poller may be waiting for has changed.
pub fn notify() {
    ACTIVITY.wake_all();
}

/// Blocks until `ready` returns true, the calling thread is killed or
/// `ticks` timer ticks have passed. `ready` runs with interrupts
/// disabled and must not block.
pub fn wait(mut ready: impl FnMut() -> bool, ticks: u64) {
    ACTIVITY.wait_until_or_timeout(|| ready() || crate::sched::current_killed(), ticks);
}
