// SPDX-License-Identifier: GPL-2.0-only
//! Sharing data between threads.

use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

const FREE: u32 = 0;
const LOCKED: u32 = 1;
/// Locked, and at least one thread may be sleeping until it is released.
const CONTENDED: u32 = 2;

/// A mutual-exclusion lock around a value.
///
/// Taking a free lock and releasing a lock nobody waits for are single
/// atomic instructions; the kernel is only involved when a thread has to
/// sleep (see the `futex_wait` system call). There is no poisoning: a
/// panic ends the whole program.
pub struct Mutex<T> {
    state: AtomicU32,
    value: UnsafeCell<T>,
}

// SAFETY: the lock gives one thread at a time access to the value, so
// sharing the mutex is sound whenever sending the value is.
unsafe impl<T: Send> Sync for Mutex<T> {}
// SAFETY: moving the mutex moves the value.
unsafe impl<T: Send> Send for Mutex<T> {}

impl<T> Mutex<T> {
    /// Creates an unlocked mutex holding `value`.
    pub const fn new(value: T) -> Self {
        Self {
            state: AtomicU32::new(FREE),
            value: UnsafeCell::new(value),
        }
    }

    /// Takes the lock, sleeping until it is free if necessary.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        if self
            .state
            .compare_exchange(FREE, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            // Mark the lock as contended so that the holder wakes someone
            // on release; whoever gets it this way keeps the mark, since
            // it cannot know whether others are still sleeping.
            while self.state.swap(CONTENDED, Ordering::Acquire) != FREE {
                // Sleeps only if the lock is still marked contended; any
                // outcome just leads to another attempt.
                let _ = crate::futex_wait(&self.state, CONTENDED, None);
            }
        }
        MutexGuard { mutex: self }
    }

    /// Takes the lock if it is free.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.state
            .compare_exchange(FREE, LOCKED, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard { mutex: self })
    }
}

/// Access to the value of a locked [`Mutex`]; releases the lock when
/// dropped.
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard exists only while the lock is held.
        unsafe { &*self.mutex.value.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard exists only while the lock is held, and it is
        // borrowed mutably.
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        if self.mutex.state.swap(FREE, Ordering::Release) == CONTENDED {
            crate::futex_wake(&self.mutex.state, 1);
        }
    }
}
