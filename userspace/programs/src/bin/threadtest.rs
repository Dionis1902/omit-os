// SPDX-License-Identifier: GPL-2.0-only
//! `threadtest` — tests threads, locks and the futex.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use omit_rt::abi::Errno;
use omit_rt::println;
use omit_rt::sync::Mutex;
use omit_rt::thread;

omit_rt::entry!(main);

const THREADS: u64 = 4;
const ROUNDS: u64 = 20_000;

fn run() -> Result<(), &'static str> {
    // Results come back through join.
    let handle = thread::spawn(|| 6 * 7).map_err(|_| "cannot start a thread")?;
    if handle.join() != 42 {
        return Err("a thread's result did not come back");
    }

    // A counter updated without a lock loses updates when threads are
    // preempted mid-update; under a mutex none may be lost. Each thread
    // also yields now and then, to make the interleaving worse.
    let counter = Arc::new(Mutex::new(0u64));
    let mut handles = Vec::new();
    for id in 0..THREADS {
        let counter = counter.clone();
        let handle = thread::spawn(move || {
            for round in 0..ROUNDS {
                let mut value = counter.lock();
                let read = *value;
                if round % 512 == id {
                    omit_rt::yield_now();
                }
                *value = read + 1;
            }
            id
        })
        .map_err(|_| "cannot start a thread")?;
        handles.push(handle);
    }
    let ids: u64 = handles.into_iter().map(thread::JoinHandle::join).sum();
    if ids != (0..THREADS).sum() {
        return Err("joined threads returned the wrong values");
    }
    if *counter.lock() != THREADS * ROUNDS {
        return Err("updates were lost under a mutex");
    }

    // The heap is shared: threads allocate and free at the same time.
    let mut handles = Vec::new();
    for id in 0..THREADS {
        let handle = thread::spawn(move || {
            let mut total = 0u64;
            for round in 0..300u64 {
                let numbers: Vec<u64> = (0..200).map(|n| n * (id + 1) + round).collect();
                total += numbers.iter().sum::<u64>();
            }
            total
        })
        .map_err(|_| "cannot start a thread")?;
        handles.push((id, handle));
    }
    for (id, handle) in handles {
        let expected: u64 = (0..300u64)
            .map(|round| (0..200u64).map(|n| n * (id + 1) + round).sum::<u64>())
            .sum();
        if handle.join() != expected {
            return Err("heap data was corrupted by concurrent use");
        }
    }

    // The futex: no sleep if the value differs, a timeout if nobody
    // wakes, and a wake-up from another thread.
    let word = Arc::new(AtomicU32::new(5));
    if omit_rt::futex_wait(&word, 4, Some(1000)) != Err(Errno::WouldBlock) {
        return Err("futex_wait slept although the value differed");
    }
    let before = omit_rt::uptime_ms();
    if omit_rt::futex_wait(&word, 5, Some(60)) != Err(Errno::TimedOut) {
        return Err("futex_wait did not time out");
    }
    if omit_rt::uptime_ms() - before < 50 {
        return Err("futex_wait timed out too early");
    }
    let woke_at = Arc::new(AtomicU64::new(0));
    let (shared, stamp) = (word.clone(), woke_at.clone());
    let waker = thread::spawn(move || {
        omit_rt::sleep_ms(40);
        shared.store(6, Ordering::Release);
        stamp.store(omit_rt::uptime_ms(), Ordering::Release);
        omit_rt::futex_wake(&shared, 1)
    })
    .map_err(|_| "cannot start a thread")?;
    let outcome = omit_rt::futex_wait(&word, 5, Some(5000));
    // Either woken, or the value had already changed by the time this
    // thread got to wait; never a timeout.
    if outcome == Err(Errno::TimedOut) || word.load(Ordering::Acquire) != 6 {
        return Err("futex_wake did not wake the waiter");
    }
    waker.join();
    if omit_rt::futex_wake(&word, 10) != 0 {
        return Err("futex_wake reported waiters that do not exist");
    }

    // A thread that never ends must not keep the program alive: when the
    // first thread returns, the process ends with all its threads.
    let forever = Arc::new(AtomicU32::new(0));
    let _detached = thread::spawn(move || {
        loop {
            let _ = omit_rt::futex_wait(&forever, 0, None);
        }
    })
    .map_err(|_| "cannot start a thread")?;
    let _spinning = thread::spawn(|| {
        loop {
            core::hint::spin_loop();
        }
    })
    .map_err(|_| "cannot start a thread")?;
    Ok(())
}

fn main() -> i32 {
    match run() {
        Ok(()) => {
            println!("threadtest: all checks passed");
            0
        }
        Err(what) => {
            println!("threadtest: FAILED: {what}");
            1
        }
    }
}
