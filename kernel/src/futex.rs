// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Futexes: the kernel half of user-space locks.
//!
//! A program keeps the state of a lock (or of anything else it wants to
//! wait for) in an ordinary 32-bit variable and only calls the kernel when
//! it has to wait. [`wait`] puts the calling thread to sleep *if* the
//! variable still holds the value the thread saw; [`wake`] wakes threads
//! sleeping on a variable. The check and the going-to-sleep are one step
//! as far as wake-ups are concerned, which is what makes it impossible to
//! miss one.
//!
//! Waiters are identified by address space and address, so threads of
//! different processes never interfere.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sched::{self, WaitQueue};
use crate::sync::SpinLock;
use crate::time;

struct Waiter {
    space: u64,
    address: u64,
    woken: Arc<AtomicBool>,
}

static WAITERS: SpinLock<Vec<Waiter>> = SpinLock::new(Vec::new());
/// All sleeping waiters; each checks its own flag when woken.
static QUEUE: WaitQueue = WaitQueue::new();

/// How a [`wait`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitResult {
    /// Woken by [`wake`].
    Woken,
    /// The variable did not hold the expected value; the thread did not
    /// sleep.
    ValueChanged,
    /// The timeout passed (or the thread was killed).
    TimedOut,
    /// The address is not readable user memory.
    Fault,
}

/// Sleeps until woken if the `u32` at `address` in the calling thread's
/// address space equals `expected`. `timeout_ms` of `None` waits without
/// limit.
pub fn wait(address: u64, expected: u32, timeout_ms: Option<u64>) -> WaitResult {
    let Some(space) = sched::current_space() else {
        return WaitResult::Fault;
    };
    let woken = Arc::new(AtomicBool::new(false));
    {
        // Under this lock a concurrent `wake` either runs before the value
        // is read — then the caller of `wake` changed the value first and
        // the comparison fails — or after the waiter is registered.
        let mut waiters = WAITERS.lock();
        let mut bytes = [0u8; 4];
        if !space.with(|space| space.read(address, &mut bytes)) {
            return WaitResult::Fault;
        }
        if u32::from_le_bytes(bytes) != expected {
            return WaitResult::ValueChanged;
        }
        waiters.push(Waiter {
            space: space.id(),
            address,
            woken: woken.clone(),
        });
    }
    drop(space);

    let done = || woken.load(Ordering::Acquire) || sched::current_killed();
    match timeout_ms {
        None => QUEUE.wait_until(done),
        Some(milliseconds) => {
            let tick_ms = 1000 / u64::from(time::TICK_HZ);
            QUEUE.wait_until_or_timeout(done, milliseconds.div_ceil(tick_ms));
        }
    }
    if woken.load(Ordering::Acquire) {
        return WaitResult::Woken;
    }
    // Not woken: take the registration back. A `wake` may slip in right
    // here; then the flag is set after all, and the wake-up counts.
    WAITERS
        .lock()
        .retain(|waiter| !Arc::ptr_eq(&waiter.woken, &woken));
    if woken.load(Ordering::Acquire) {
        WaitResult::Woken
    } else {
        WaitResult::TimedOut
    }
}

/// Wakes up to `count` threads of the calling thread's process that sleep
/// on `address`. Returns how many were woken.
pub fn wake(address: u64, count: usize) -> usize {
    let Some(space) = sched::current_space().map(|space| space.id()) else {
        return 0;
    };
    let mut woken = 0;
    WAITERS.lock().retain(|waiter| {
        if woken < count && waiter.space == space && waiter.address == address {
            waiter.woken.store(true, Ordering::Release);
            woken += 1;
            false
        } else {
            true
        }
    });
    if woken != 0 {
        QUEUE.wake_all();
    }
    woken
}
