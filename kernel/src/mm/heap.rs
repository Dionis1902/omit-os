// SPDX-License-Identifier: GPL-2.0-only
//! Kernel heap: the global allocator behind `alloc::{boxed, vec, string, ..}`.
//!
//! The allocation logic is the size-class heap from the `omit_mm` crate;
//! this module connects it to the physical frame allocator (pages are
//! reached through the direct map, so no page-table work is needed) and to
//! Rust's `GlobalAlloc` interface.
//!
//! Allocation failure is reported to the caller as a null pointer; for the
//! infallible `alloc` APIs that turns into a kernel panic.

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::fmt::Write;
use core::ptr::NonNull;

use omit_bootproto::PHYS_MAP_BASE;
use omit_mm::{Heap, HeapStats, PageSource};

use super::{PAGE_SIZE, frame, phys_to_virt};
use crate::sync::SpinLock;

/// Supplies the heap with frames from the physical allocator.
struct FramePages;

// SAFETY: `frame::alloc` returns `2^order` contiguous frames aligned to the
// block size, owned exclusively by the caller until freed; `phys_to_virt`
// turns that into a direct-map pointer with the same size and alignment
// (the direct map base is aligned far beyond any block size), mapped
// readable and writable for the lifetime of the kernel.
unsafe impl PageSource for FramePages {
    const PAGE_SIZE: usize = PAGE_SIZE as usize;
    const MAX_ORDER: u8 = omit_mm::MAX_ORDER;

    fn alloc_pages(&mut self, order: u8) -> Option<NonNull<u8>> {
        let phys = frame::alloc(order)?;
        NonNull::new(phys_to_virt(phys, PAGE_SIZE << order)?)
    }

    unsafe fn free_pages(&mut self, block: NonNull<u8>, order: u8) {
        let phys = block.as_ptr() as u64 - PHYS_MAP_BASE;
        // SAFETY: per the trait contract the block came from `alloc_pages`
        // with this order and is no longer used, so its frames can go back.
        let result = unsafe { frame::free(phys, order) };
        debug_assert!(
            result.is_ok(),
            "heap returned a block the frame allocator rejects"
        );
    }
}

/// The kernel heap behind an interrupt-safe lock.
struct KernelHeap(SpinLock<Heap<FramePages>>);

// SAFETY: `Heap::allocate` returns either null or a block that satisfies the
// layout and is not handed out again until freed, and the lock serialises
// all access, as `GlobalAlloc` requires. Neither method unwinds.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0.lock().allocate(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the `GlobalAlloc` contract gives exactly the guarantees
        // `Heap::deallocate` asks for.
        unsafe { self.0.lock().deallocate(ptr, layout) };
    }
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap(SpinLock::new(Heap::new(FramePages)));

/// Current heap counters.
pub fn stats() -> HeapStats {
    HEAP.0.lock().stats()
}

/// Exercises the heap through the standard collection types: small and
/// large allocations, growth by reallocation, over-aligned types, and
/// verifies that everything is released again.
///
/// Must run after the frame allocator is initialised.
pub fn self_test() -> Result<(), &'static str> {
    #[repr(align(256))]
    struct Aligned(u8);

    let before = stats();
    let frames_before = frame::stats()
        .ok_or("frame allocator not initialised")?
        .free;

    {
        let boxed = Box::new(0xDEAD_BEEF_u64);
        if *boxed != 0xDEAD_BEEF {
            return Err("boxed value did not read back");
        }

        let aligned = Box::new(Aligned(7));
        if !(&raw const *aligned).is_aligned() || aligned.0 != 7 {
            return Err("over-aligned allocation is misaligned");
        }

        // Grows through every size class into multi-page allocations.
        let mut numbers = Vec::new();
        for value in 0..50_000u64 {
            numbers.push(value);
        }
        if numbers.iter().copied().sum::<u64>() != 50_000 * 49_999 / 2 {
            return Err("vector contents were corrupted while growing");
        }

        let mut text = String::new();
        for index in 0..100 {
            write!(text, "{index},").map_err(|_| "formatting into a String failed")?;
        }
        if !text.starts_with("0,1,2,") || !text.ends_with("98,99,") {
            return Err("string contents are wrong");
        }

        // Many small objects alive at once must not overlap.
        let boxes: Vec<Box<usize>> = (0..1000).map(Box::new).collect();
        if boxes
            .iter()
            .enumerate()
            .any(|(index, value)| **value != index)
        {
            return Err("small allocations overlap");
        }

        if stats().live_allocations <= before.live_allocations {
            return Err("live allocation count did not increase");
        }
    }

    let after = stats();
    if after.live_allocations != before.live_allocations
        || after.allocated_bytes != before.allocated_bytes
        || after.large_pages != before.large_pages
    {
        return Err("heap did not return to its initial state");
    }
    // Large blocks go back to the frame allocator; only slab pages stay.
    let slab_growth = after.slab_pages - before.slab_pages;
    let frames_after = frame::stats().ok_or("frame allocator vanished")?.free;
    if frames_before - frames_after != slab_growth {
        return Err("frames were leaked");
    }
    Ok(())
}
