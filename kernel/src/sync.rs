// SPDX-License-Identifier: GPL-2.0-only
//! Kernel synchronisation primitives.

use core::cell::UnsafeCell;
use core::hint;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::interrupts;

/// An interrupt-safe test-and-test-and-set spinlock.
///
/// Interrupts are disabled on the local CPU for as long as the lock is held
/// and restored to their previous state on release. That makes the lock
/// usable from interrupt handlers: a handler can never interrupt code on its
/// own CPU that holds the lock, so it cannot deadlock against it.
///
/// Critical sections must therefore be short. The kernel currently runs on a
/// single CPU, where the lock is never contended; the spinning path exists
/// for SMP.
pub struct SpinLock<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// SAFETY: the lock hands out at most one `&mut T` at a time (see `lock`), so
// sharing the lock between threads is sound whenever sending `T` is.
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    /// Creates an unlocked lock holding `value`.
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Acquires the lock, spinning until it is available.
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let interrupts_were_enabled = interrupts::save_and_disable();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                hint::spin_loop();
            }
        }
        SpinLockGuard {
            lock: self,
            interrupts_were_enabled,
        }
    }

    /// Acquires the lock if it is free.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        let interrupts_were_enabled = interrupts::save_and_disable();
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            Some(SpinLockGuard {
                lock: self,
                interrupts_were_enabled,
            })
        } else {
            interrupts::restore(interrupts_were_enabled);
            None
        }
    }
}

/// Grants access to the value protected by a [`SpinLock`]; releases the lock
/// and restores the interrupt state when dropped.
pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    interrupts_were_enabled: bool,
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: a guard exists only while `locked` is held, and only one
        // guard can exist at a time, so no `&mut T` aliases this reference.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: as in `deref`; `&mut self` additionally guarantees this is
        // the only reference derived from the unique guard.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        interrupts::restore(self.interrupts_were_enabled);
    }
}
