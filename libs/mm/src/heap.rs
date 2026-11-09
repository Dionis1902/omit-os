// SPDX-License-Identifier: GPL-2.0-only
//! Size-class heap allocator.
//!
//! Serves arbitrary [`Layout`]s on top of a source of whole pages:
//!
//! * Requests up to [`MAX_SMALL_SIZE`] bytes are rounded up to a power-of-two
//!   size class (16 bytes to 2 KiB). Each class keeps a free list threaded
//!   through the free blocks themselves; an empty class is refilled by
//!   carving one page into blocks. Allocation and deallocation are a
//!   constant number of pointer operations.
//! * Larger requests go straight to the page source as a power-of-two run of
//!   pages and are returned to it when freed.
//!
//! Because every block is a power of two carved from a page-aligned page,
//! each block is naturally aligned to its own size, so alignment is
//! satisfied by choosing a class at least as large as the alignment.
//!
//! # Trade-offs
//!
//! * Internal fragmentation is up to 50 % (power-of-two rounding). Finer
//!   classes are an easy later refinement if measurements call for them.
//! * Pages given to a small size class are never returned to the page
//!   source, even when all their blocks are free.
//! * Free blocks store their list link in place, so a write to freed memory
//!   can corrupt the heap. Hardening (canaries, quarantine) is future work.

use core::alloc::Layout;
use core::ptr::{self, NonNull};

/// Smallest block size; must hold a free-list link.
pub const MIN_BLOCK_SIZE: usize = 16;
/// Largest request served from a size class.
pub const MAX_SMALL_SIZE: usize = 2048;

const MIN_SHIFT: u32 = MIN_BLOCK_SIZE.trailing_zeros();
const CLASS_COUNT: usize = (MAX_SMALL_SIZE.trailing_zeros() - MIN_SHIFT + 1) as usize;

/// A provider of page-aligned memory for a [`Heap`].
///
/// # Safety
///
/// An implementation must guarantee that every block returned by
/// [`PageSource::alloc_pages`]
///
/// * is `PAGE_SIZE << order` bytes long and aligned to that size,
/// * is valid for reads and writes and not aliased by anything else,
/// * stays valid until it is passed back to [`PageSource::free_pages`].
///
/// `PAGE_SIZE` must be a power of two no smaller than [`MAX_SMALL_SIZE`].
pub unsafe trait PageSource {
    /// Size of one page in bytes.
    const PAGE_SIZE: usize;
    /// Largest supported `order`.
    const MAX_ORDER: u8;

    /// Allocates `2^order` contiguous pages, or returns `None` if that is
    /// not possible.
    fn alloc_pages(&mut self, order: u8) -> Option<NonNull<u8>>;

    /// Returns a block to the source.
    ///
    /// # Safety
    ///
    /// `block` must have come from `alloc_pages(order)` on this source, with
    /// the same `order`, and must not be used afterwards.
    unsafe fn free_pages(&mut self, block: NonNull<u8>, order: u8);
}

/// Header written into every free small block.
struct FreeBlock {
    next: Option<NonNull<FreeBlock>>,
}

/// Counters describing a heap's current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeapStats {
    /// Bytes currently handed out, counted at their rounded-up block size.
    pub allocated_bytes: usize,
    /// Number of live allocations.
    pub live_allocations: usize,
    /// Pages obtained for small size classes (never returned).
    pub slab_pages: usize,
    /// Pages currently held by large allocations.
    pub large_pages: usize,
}

/// How a layout is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Size class index; block size is `MIN_BLOCK_SIZE << index`.
    Small(usize),
    /// Page run of `2^order` pages.
    Large(u8),
}

/// A size-class heap drawing pages from `S`.
pub struct Heap<S: PageSource> {
    free: [Option<NonNull<FreeBlock>>; CLASS_COUNT],
    source: S,
    stats: HeapStats,
}

// SAFETY: the raw pointers in `free` refer to memory owned exclusively by
// this heap (obtained from its page source and not handed out), so moving
// the heap to another thread moves that ownership with it.
unsafe impl<S: PageSource + Send> Send for Heap<S> {}

impl<S: PageSource> Heap<S> {
    /// Creates an empty heap. No pages are taken until the first
    /// allocation.
    pub const fn new(source: S) -> Self {
        assert!(S::PAGE_SIZE.is_power_of_two() && S::PAGE_SIZE >= MAX_SMALL_SIZE);
        Self {
            free: [None; CLASS_COUNT],
            source,
            stats: HeapStats {
                allocated_bytes: 0,
                live_allocations: 0,
                slab_pages: 0,
                large_pages: 0,
            },
        }
    }

    /// Current counters.
    pub fn stats(&self) -> HeapStats {
        self.stats
    }

    /// The page source, for inspection.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// Decides how `layout` is served, or `None` if it is too large.
    fn classify(layout: Layout) -> Option<Class> {
        // A block must be at least as large as the size and the alignment.
        let needed = layout.size().max(layout.align()).max(MIN_BLOCK_SIZE);
        if needed <= MAX_SMALL_SIZE {
            let shift = needed.next_power_of_two().trailing_zeros();
            return Some(Class::Small((shift - MIN_SHIFT) as usize));
        }
        let pages = needed.div_ceil(S::PAGE_SIZE).checked_next_power_of_two()?;
        let order = pages.trailing_zeros();
        (order <= u32::from(S::MAX_ORDER)).then_some(Class::Large(order as u8))
    }

    /// Carves a fresh page into blocks for size class `class`.
    fn refill(&mut self, class: usize) -> Option<()> {
        let page = self.source.alloc_pages(0)?;
        let block_size = MIN_BLOCK_SIZE << class;
        let count = S::PAGE_SIZE / block_size;
        // Link the blocks in address order, so consecutive allocations from
        // a fresh page are adjacent in memory.
        let mut head = None;
        for index in (0..count).rev() {
            // SAFETY: `index * block_size < PAGE_SIZE`, so the block lies
            // within the page just obtained, which the source guarantees to
            // be valid, writable and exclusively ours. The page is
            // page-aligned and `block_size` is a power of two >= 16, so the
            // block is suitably aligned for `FreeBlock`.
            unsafe {
                let block = page.as_ptr().add(index * block_size).cast::<FreeBlock>();
                block.write(FreeBlock { next: head });
                head = Some(NonNull::new_unchecked(block));
            }
        }
        self.free[class] = head;
        self.stats.slab_pages += 1;
        Some(())
    }

    /// Allocates memory for `layout`. Returns null if the request cannot be
    /// satisfied. The memory is not initialised. Zero-sized layouts are
    /// served like one-byte ones.
    pub fn allocate(&mut self, layout: Layout) -> *mut u8 {
        let Some(class) = Self::classify(layout) else {
            return ptr::null_mut();
        };
        match class {
            Class::Small(index) => {
                if self.free[index].is_none() && self.refill(index).is_none() {
                    return ptr::null_mut();
                }
                let Some(block) = self.free[index] else {
                    return ptr::null_mut();
                };
                // SAFETY: every pointer on a free list was written as a
                // valid `FreeBlock` by `refill` or `deallocate` and has not
                // been handed out since.
                self.free[index] = unsafe { block.as_ref().next };
                self.stats.allocated_bytes += MIN_BLOCK_SIZE << index;
                self.stats.live_allocations += 1;
                block.as_ptr().cast()
            }
            Class::Large(order) => match self.source.alloc_pages(order) {
                Some(block) => {
                    self.stats.allocated_bytes += S::PAGE_SIZE << order;
                    self.stats.large_pages += 1 << order;
                    self.stats.live_allocations += 1;
                    block.as_ptr()
                }
                None => ptr::null_mut(),
            },
        }
    }

    /// Frees memory obtained from [`Heap::allocate`].
    ///
    /// # Safety
    ///
    /// `ptr` must have been returned by `allocate` on this heap with the
    /// same `layout`, must not have been freed already, and must not be used
    /// afterwards.
    pub unsafe fn deallocate(&mut self, ptr: *mut u8, layout: Layout) {
        let (Some(block), Some(class)) = (NonNull::new(ptr), Self::classify(layout)) else {
            return;
        };
        match class {
            Class::Small(index) => {
                let block = block.cast::<FreeBlock>();
                // SAFETY: per the contract the block came from size class
                // `index`, so it is at least 16 bytes, suitably aligned, and
                // no longer in use by the caller.
                unsafe {
                    block.as_ptr().write(FreeBlock {
                        next: self.free[index],
                    });
                }
                self.free[index] = Some(block);
                self.stats.allocated_bytes -= MIN_BLOCK_SIZE << index;
            }
            Class::Large(order) => {
                // SAFETY: per the contract the block came from
                // `alloc_pages(order)` on this heap's source and is no
                // longer in use.
                unsafe { self.source.free_pages(block, order) };
                self.stats.allocated_bytes -= S::PAGE_SIZE << order;
                self.stats.large_pages -= 1 << order;
            }
        }
        self.stats.live_allocations -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc, dealloc};
    use std::vec::Vec;

    const PAGE: usize = 4096;

    /// Page source backed by the host allocator, with accounting and an
    /// optional page budget.
    struct HostPages {
        outstanding_pages: usize,
        total_calls: usize,
        budget_pages: usize,
    }

    impl HostPages {
        fn new(budget_pages: usize) -> Self {
            Self {
                outstanding_pages: 0,
                total_calls: 0,
                budget_pages,
            }
        }

        fn layout(order: u8) -> Layout {
            Layout::from_size_align(PAGE << order, PAGE << order).unwrap()
        }
    }

    // SAFETY: blocks come from the host allocator with exactly the size and
    // alignment the trait requires and stay valid until `free_pages`.
    unsafe impl PageSource for HostPages {
        const PAGE_SIZE: usize = PAGE;
        const MAX_ORDER: u8 = 4;

        fn alloc_pages(&mut self, order: u8) -> Option<NonNull<u8>> {
            let pages = 1usize << order;
            if self.outstanding_pages + pages > self.budget_pages {
                return None;
            }
            self.outstanding_pages += pages;
            self.total_calls += 1;
            // SAFETY: the layout has non-zero size.
            NonNull::new(unsafe { alloc(Self::layout(order)) })
        }

        unsafe fn free_pages(&mut self, block: NonNull<u8>, order: u8) {
            self.outstanding_pages -= 1 << order;
            // SAFETY: forwarded from the caller's contract.
            unsafe { dealloc(block.as_ptr(), Self::layout(order)) };
        }
    }

    type TestHeap = Heap<HostPages>;

    fn layout(size: usize, align: usize) -> Layout {
        Layout::from_size_align(size, align).unwrap()
    }

    #[test]
    fn classifies_by_size_and_alignment() {
        use Class::{Large, Small};
        let c = |size, align| TestHeap::classify(layout(size, align));
        assert_eq!(c(0, 1), Some(Small(0)));
        assert_eq!(c(1, 1), Some(Small(0)));
        assert_eq!(c(16, 8), Some(Small(0)));
        assert_eq!(c(17, 1), Some(Small(1)));
        assert_eq!(c(8, 64), Some(Small(2)), "alignment selects the class");
        assert_eq!(c(2048, 8), Some(Small(CLASS_COUNT - 1)));
        assert_eq!(c(2049, 8), Some(Large(0)));
        assert_eq!(c(4096, 4096), Some(Large(0)));
        assert_eq!(c(4097, 8), Some(Large(1)));
        assert_eq!(c(8, 8192), Some(Large(1)));
        assert_eq!(c(PAGE << 4, 8), Some(Large(4)));
        assert_eq!(c((PAGE << 4) + 1, 8), None);
    }

    #[test]
    fn small_allocations_are_aligned_distinct_and_reused() {
        let mut heap = TestHeap::new(HostPages::new(usize::MAX));
        let l = layout(24, 8); // 32-byte class
        let a = heap.allocate(l);
        let b = heap.allocate(l);
        assert!(!a.is_null() && !b.is_null() && a != b);
        assert_eq!(a as usize % 32, 0);
        assert_eq!(b as usize, a as usize + 32, "fresh blocks are adjacent");
        assert_eq!(heap.stats().slab_pages, 1);
        assert_eq!(heap.stats().allocated_bytes, 64);

        // SAFETY: `a` and `b` came from this heap with layout `l`.
        unsafe {
            heap.deallocate(a, l);
            assert_eq!(heap.allocate(l), a, "most recently freed block is reused");
            heap.deallocate(a, l);
            heap.deallocate(b, l);
        }
        assert_eq!(heap.stats().live_allocations, 0);
        assert_eq!(heap.stats().allocated_bytes, 0);
        assert_eq!(heap.source().total_calls, 1, "no further pages needed");
    }

    #[test]
    fn one_page_yields_exactly_its_capacity() {
        let mut heap = TestHeap::new(HostPages::new(1));
        let l = layout(512, 8);
        let blocks: Vec<_> = (0..PAGE / 512).map(|_| heap.allocate(l)).collect();
        assert!(blocks.iter().all(|p| !p.is_null()));
        assert!(heap.allocate(l).is_null(), "page budget exhausted");
        assert!(heap.allocate(layout(16, 8)).is_null());
        // SAFETY: the blocks came from this heap with layout `l`.
        unsafe {
            heap.deallocate(blocks[3], l);
        }
        assert_eq!(heap.allocate(l), blocks[3], "recovers after a free");
    }

    #[test]
    fn honours_large_alignments() {
        let mut heap = TestHeap::new(HostPages::new(usize::MAX));
        for align in [1usize, 16, 64, 512, 2048, 4096, 16384] {
            let l = layout(40, align);
            let p = heap.allocate(l);
            assert!(!p.is_null());
            assert_eq!(p as usize % align, 0, "alignment {align}");
            // SAFETY: `p` came from this heap with layout `l`.
            unsafe { heap.deallocate(p, l) };
        }
    }

    #[test]
    fn large_allocations_return_pages_to_the_source() {
        let mut heap = TestHeap::new(HostPages::new(usize::MAX));
        let l = layout(3 * PAGE, 8); // rounds to 4 pages
        let p = heap.allocate(l);
        assert!(!p.is_null());
        assert_eq!(p as usize % (4 * PAGE), 0);
        assert_eq!(heap.stats().large_pages, 4);
        assert_eq!(heap.source().outstanding_pages, 4);
        // SAFETY: `p` is valid for 3 pages; it came from this heap.
        unsafe {
            p.write_bytes(0xAB, 3 * PAGE);
            heap.deallocate(p, l);
        }
        assert_eq!(heap.stats().large_pages, 0);
        assert_eq!(heap.source().outstanding_pages, 0);
        assert!(heap.allocate(layout((PAGE << 4) + 1, 8)).is_null());
    }

    #[test]
    fn null_free_is_ignored() {
        let mut heap = TestHeap::new(HostPages::new(usize::MAX));
        // SAFETY: freeing null is defined to be a no-op.
        unsafe { heap.deallocate(ptr::null_mut(), layout(8, 8)) };
        assert_eq!(heap.stats(), HeapStats::default());
    }

    /// Random mix of sizes and alignments. Every allocation is filled with
    /// a byte unique to it and checked before being freed, so any overlap
    /// between live allocations (or with free-list links) is detected.
    #[test]
    fn randomised_no_overlap() {
        let mut heap = TestHeap::new(HostPages::new(usize::MAX));
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut live: Vec<(*mut u8, Layout, u8)> = Vec::new();

        for round in 0..20_000u32 {
            if live.is_empty() || next() % 5 < 3 {
                let size = match next() % 10 {
                    0 => 1 + (next() % 20_000) as usize,
                    _ => 1 + (next() % 600) as usize,
                };
                let align = 1usize << (next() % 8);
                let l = layout(size, align);
                let p = heap.allocate(l);
                assert!(!p.is_null());
                assert_eq!(p as usize % align, 0);
                let tag = round as u8;
                // SAFETY: `p` is a fresh allocation of `size` bytes.
                unsafe { p.write_bytes(tag, size) };
                live.push((p, l, tag));
            } else {
                let index = (next() as usize) % live.len();
                let (p, l, tag) = live.swap_remove(index);
                // SAFETY: `p` is live, `l.size()` bytes long, from this heap.
                unsafe {
                    let bytes = core::slice::from_raw_parts(p, l.size());
                    assert!(
                        bytes.iter().all(|&b| b == tag),
                        "allocation was overwritten"
                    );
                    heap.deallocate(p, l);
                }
            }
            assert_eq!(heap.stats().live_allocations, live.len());
        }

        for (p, l, tag) in live.drain(..) {
            // SAFETY: as above.
            unsafe {
                assert!(
                    core::slice::from_raw_parts(p, l.size())
                        .iter()
                        .all(|&b| b == tag)
                );
                heap.deallocate(p, l);
            }
        }
        assert_eq!(heap.stats().allocated_bytes, 0);
        assert_eq!(heap.stats().large_pages, 0);
        assert_eq!(heap.source().outstanding_pages, heap.stats().slab_pages);
    }
}
