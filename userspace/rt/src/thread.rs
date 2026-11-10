// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Threads: several flows of control in one program, sharing its memory.

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

use omit_abi::{Errno, nr};

use crate::sync::Mutex;

/// Stack given to each thread.
const STACK_SIZE: usize = 256 * 1024;
const PAGE: usize = 4096;

type Task = Box<dyn FnOnce() + Send>;

/// A thread that can be waited for. Dropping the handle lets the thread
/// run on by itself; its stack is then never released.
pub struct JoinHandle<T> {
    /// Set to 1 by the kernel when the thread has ended. Allocated for
    /// the life of the program: the kernel writes to it whenever the
    /// thread ends, and [`Waiter`]s may look at it at any time after.
    ended: &'static AtomicU32,
    /// The whole mapping, including the guard page.
    stack: *mut u8,
    result: Arc<Mutex<Option<T>>>,
}

// SAFETY: the stack pointer is only an address, used to release the
// mapping; the handle can be used from any thread.
unsafe impl<T: Send> Send for JoinHandle<T> {}

/// Something that can wait for a thread to end without owning its
/// [`JoinHandle`]; any number may exist, in any thread.
#[derive(Clone, Copy)]
pub struct Waiter(&'static AtomicU32);

impl Waiter {
    /// Whether the thread has ended.
    pub fn is_finished(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }

    /// Waits until the thread has ended or `timeout_ms` milliseconds have
    /// passed (`None`: without limit). Returns whether it has ended.
    pub fn wait(&self, timeout_ms: Option<u64>) -> bool {
        let deadline = timeout_ms.map(|timeout| crate::uptime_ms().saturating_add(timeout));
        while !self.is_finished() {
            let remaining = match deadline {
                Some(deadline) => {
                    let now = crate::uptime_ms();
                    if now >= deadline {
                        return false;
                    }
                    Some(deadline - now)
                }
                None => None,
            };
            // Sleeps only while the flag is still 0; the kernel sets it
            // and wakes the waiters when the thread has ended.
            let _ = crate::futex_wait(self.0, 0, remaining);
        }
        true
    }
}

/// Ends the calling thread at once, without running the destructors of
/// values on its stack. If it is the last thread, the program ends with
/// status 0.
pub fn exit() -> ! {
    // SAFETY: no arguments; the kernel does not return.
    unsafe { crate::syscall(nr::THREAD_EXIT, 0, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
}

/// First function of every thread.
extern "C" fn thread_start(task: usize) -> ! {
    // SAFETY: `task` is the pointer produced by `Box::into_raw` in `spawn`
    // for exactly this thread.
    let task = unsafe { Box::from_raw(task as *mut Task) };
    task();
    // The kernel marks the thread as ended only after it has left user
    // mode.
    exit()
}

/// Starts a thread running `function`.
pub fn spawn<T, F>(function: F) -> Result<JoinHandle<T>, Errno>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let result = Arc::new(Mutex::new(None));
    let slot = result.clone();
    let task: Task = Box::new(move || {
        let value = function();
        *slot.lock() = Some(value);
    });
    // A page below the stack stays unmapped, so that running out of stack
    // faults instead of overwriting other memory.
    let stack = crate::map(STACK_SIZE + PAGE, PAGE)?;
    // SAFETY: just mapped, not yet used.
    unsafe { crate::unmap(stack, PAGE) }?;
    let top = stack as usize + PAGE + STACK_SIZE;
    let ended: &'static AtomicU32 = Box::leak(Box::new(AtomicU32::new(0)));
    let task = Box::into_raw(Box::new(task));
    // SAFETY: the entry point is `thread_start`, which takes the task
    // pointer; the stack is the fresh mapping above, with the stack
    // pointer placed as at the entry of a called function (8 below a
    // 16-byte boundary); `ended` lives for the rest of the program.
    let created = crate::result(unsafe {
        crate::syscall4(
            nr::THREAD_CREATE,
            thread_start as *const () as u64,
            (top - 8) as u64,
            task as u64,
            core::ptr::from_ref(ended) as u64,
        )
    });
    match created {
        Ok(_) => Ok(JoinHandle {
            ended,
            stack,
            result,
        }),
        Err(error) => {
            // SAFETY: the thread does not exist, so the task, the flag and
            // the stack are still exclusively ours.
            unsafe {
                drop(Box::from_raw(task));
                drop(Box::from_raw(core::ptr::from_ref(ended).cast_mut()));
                let _ = crate::unmap(stack.add(PAGE), STACK_SIZE);
            }
            Err(error)
        }
    }
}

impl<T> JoinHandle<T> {
    /// Whether the thread has ended.
    pub fn is_finished(&self) -> bool {
        self.waiter().is_finished()
    }

    /// A [`Waiter`] for this thread.
    pub fn waiter(&self) -> Waiter {
        Waiter(self.ended)
    }

    /// Releases the thread's stack if the thread has ended; otherwise the
    /// thread runs on by itself, as when the handle is dropped. Returns
    /// whether the stack was released.
    pub fn reclaim(self) -> bool {
        let finished = self.is_finished();
        if finished {
            // SAFETY: the kernel has set the flag, which it does only
            // after the thread has left user mode for good: nothing uses
            // the stack any more.
            let _ = unsafe { crate::unmap(self.stack.add(PAGE), STACK_SIZE) };
        }
        finished
    }

    /// Waits for the thread to end and returns what its function returned.
    pub fn join(self) -> T {
        self.waiter().wait(None);
        let result = self.result.clone();
        self.reclaim();
        // The thread stores its result before it ends. Without one, it
        // was ended some other way ([`exit`]), and there is nothing to
        // return: that is a bug in the caller, treated like a panic.
        let value = result.lock().take();
        match value {
            Some(value) => value,
            None => crate::exit(omit_abi::EXIT_PANIC),
        }
    }
}
