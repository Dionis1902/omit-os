// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Kernel thread stacks.
//!
//! Every stack is mapped in its own slot of a dedicated virtual range, with
//! an unmapped guard page below it, so an overflow faults immediately
//! instead of silently corrupting whatever lies beneath. Slots of exited
//! threads are reused.

use alloc::vec::Vec;

use super::{PAGE_SIZE, frame};
use crate::arch::paging::{self, KERNEL_STACKS_BASE, KERNEL_STACKS_END};
use crate::sync::SpinLock;

/// Usable size of a kernel thread stack.
pub const STACK_SIZE: u64 = 64 * 1024;
const STACK_PAGES: u64 = STACK_SIZE / PAGE_SIZE;
/// One guard page plus the stack itself.
const SLOT_SIZE: u64 = STACK_SIZE + PAGE_SIZE;

struct Slots {
    /// Address of the next never-used slot.
    next: u64,
    /// Slots whose stacks were freed.
    free: Vec<u64>,
}

static SLOTS: SpinLock<Slots> = SpinLock::new(Slots {
    next: KERNEL_STACKS_BASE,
    free: Vec::new(),
});

/// An allocated, mapped kernel stack. Dropping it unmaps the stack and
/// returns its memory.
#[derive(Debug)]
pub struct KernelStack {
    /// Start of the slot (the guard page).
    slot: u64,
}

impl KernelStack {
    /// Allocates and maps a zero-filled stack. Returns `None` if memory or
    /// address space is exhausted.
    pub fn new() -> Option<Self> {
        let slot = {
            let mut slots = SLOTS.lock();
            match slots.free.pop() {
                Some(slot) => slot,
                None => {
                    let slot = slots.next;
                    if slot + SLOT_SIZE > KERNEL_STACKS_END {
                        return None;
                    }
                    slots.next += SLOT_SIZE;
                    slot
                }
            }
        };
        let stack = Self { slot };
        for page in 0..STACK_PAGES {
            let mapped = frame::alloc_zeroed(0).is_some_and(|phys| {
                // SAFETY: the frame was just allocated for this stack and is
                // mapped nowhere else (the direct map alias is not used for
                // it); the virtual page belongs to this stack's slot, which
                // no other stack uses.
                let result = unsafe { paging::map_kernel_page(stack.page(page), phys, true) };
                if result.is_err() {
                    // SAFETY: the frame was never mapped or otherwise used.
                    let _ = unsafe { frame::free(phys, 0) };
                }
                result.is_ok()
            });
            if !mapped {
                // Dropping `stack` releases the pages mapped so far.
                return None;
            }
        }
        Some(stack)
    }

    fn page(&self, index: u64) -> u64 {
        self.slot + PAGE_SIZE + index * PAGE_SIZE
    }

    /// Exclusive upper end of the stack; the initial stack pointer.
    pub fn top(&self) -> u64 {
        self.slot + SLOT_SIZE
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        for page in 0..STACK_PAGES {
            // SAFETY: owning the `KernelStack` means its thread has finished
            // and been switched away from for good (the scheduler only drops
            // stacks of dead threads from another thread's context), so
            // nothing accesses these pages any more.
            if let Ok(Some(phys)) = unsafe { paging::unmap_kernel_page(self.page(page)) } {
                // SAFETY: the frame was owned by this stack and is now
                // unmapped.
                let _ = unsafe { frame::free(phys, 0) };
            }
        }
        SLOTS.lock().free.push(self.slot);
    }
}
