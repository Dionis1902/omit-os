// SPDX-License-Identifier: GPL-2.0-only
//! The program's heap: the global allocator behind the `alloc` crate.
//!
//! The same size-class allocator the kernel uses (`omit_mm::Heap`), fed
//! with pages from the `map` system call. Small objects are carved from
//! pages that the heap keeps; large allocations are mapped and unmapped
//! directly.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};

use omit_mm::{Heap, PageSource};

const PAGE_SIZE: usize = 4096;

/// Obtains pages from the kernel.
struct KernelPages;

// SAFETY: `map` returns zero-filled, writable memory of the requested size
// at an address aligned as requested, owned by this process and valid until
// it is unmapped, which only `free_pages` does.
unsafe impl PageSource for KernelPages {
    const PAGE_SIZE: usize = PAGE_SIZE;
    /// Up to 256 MiB in one block, the kernel's limit for one mapping.
    const MAX_ORDER: u8 = 16;

    fn alloc_pages(&mut self, order: u8) -> Option<NonNull<u8>> {
        let size = PAGE_SIZE << order;
        crate::map(size, size).ok().and_then(NonNull::new)
    }

    unsafe fn free_pages(&mut self, block: NonNull<u8>, order: u8) {
        // SAFETY: per the trait contract the block came from `alloc_pages`
        // with this order and is no longer used.
        let _ = unsafe { crate::unmap(block.as_ptr(), PAGE_SIZE << order) };
    }
}

/// The heap is shared by all threads of the process. Allocations are
/// short, so a thread that finds the heap busy gives up the CPU and tries
/// again rather than sleeping: the thread holding it was merely preempted.
/// (The lock cannot be the runtime's own `Mutex`, which may allocate.)
struct ProgramHeap {
    busy: AtomicBool,
    heap: UnsafeCell<Heap<KernelPages>>,
}

// SAFETY: access to `heap` is serialised by `busy`.
unsafe impl Sync for ProgramHeap {}

impl ProgramHeap {
    fn with<R>(&self, f: impl FnOnce(&mut Heap<KernelPages>) -> R) -> R {
        while self.busy.swap(true, Ordering::Acquire) {
            crate::yield_now();
        }
        // SAFETY: `busy` was clear and is now set, so this is the only
        // reference to the heap until it is cleared again below.
        let result = f(unsafe { &mut *self.heap.get() });
        self.busy.store(false, Ordering::Release);
        result
    }
}

// SAFETY: `Heap::allocate` returns null or a block satisfying the layout
// that is not handed out again until freed; access is serialised.
unsafe impl GlobalAlloc for ProgramHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.with(|heap| heap.allocate(layout))
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the `GlobalAlloc` contract provides what `deallocate`
        // requires.
        self.with(|heap| unsafe { heap.deallocate(pointer, layout) });
    }
}

#[global_allocator]
static HEAP: ProgramHeap = ProgramHeap {
    busy: AtomicBool::new(false),
    heap: UnsafeCell::new(Heap::new(KernelPages)),
};
