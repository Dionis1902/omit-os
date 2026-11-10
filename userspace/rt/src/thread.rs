// SPDX-License-Identifier: GPL-2.0-only
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
    /// the life of the program unless the thread is joined, because the
    /// kernel writes to it whenever the thread ends.
    ended: &'static AtomicU32,
    /// The whole mapping, including the guard page.
    stack: *mut u8,
    result: Arc<Mutex<Option<T>>>,
}

/// First function of every thread.
extern "C" fn thread_start(task: usize) -> ! {
    // SAFETY: `task` is the pointer produced by `Box::into_raw` in `spawn`
    // for exactly this thread.
    let task = unsafe { Box::from_raw(task as *mut Task) };
    task();
    // SAFETY: no arguments. The kernel does not return here; it marks the
    // thread as ended only after it has left user mode.
    unsafe { crate::syscall(nr::THREAD_EXIT, 0, 0, 0) };
    loop {
        core::hint::spin_loop();
    }
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
        self.ended.load(Ordering::Acquire) != 0
    }

    /// Waits for the thread to end and returns what its function returned.
    pub fn join(self) -> T {
        while self.ended.load(Ordering::Acquire) == 0 {
            // Sleeps only while the flag is still 0; the kernel sets it
            // and wakes this thread when the other one has ended.
            let _ = crate::futex_wait(self.ended, 0, None);
        }
        // SAFETY: the kernel has set the flag, which it does only after
        // the thread has left user mode for good: nothing uses the stack
        // any more, and the kernel does not touch the flag again.
        unsafe {
            let _ = crate::unmap(self.stack.add(PAGE), STACK_SIZE);
            drop(Box::from_raw(core::ptr::from_ref(self.ended).cast_mut()));
        }
        // The thread stores its result before it ends. (If the thread was
        // ended from outside, the whole program is ending too.)
        let value = self.result.lock().take();
        match value {
            Some(value) => value,
            None => crate::exit(omit_abi::EXIT_PANIC),
        }
    }
}
