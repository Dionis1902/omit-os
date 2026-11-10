// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! Binary buddy allocator for physical page frames.
//!
//! Memory is handed out in blocks of `2^order` frames, naturally aligned.
//! Freeing a block merges it with its "buddy" (the neighbouring block of the
//! same size that shares its parent) whenever that buddy is free too, which
//! keeps large contiguous blocks available for huge pages and DMA.
//!
//! # Design
//!
//! * Allocation and freeing are O([`MAX_ORDER`]): no searching.
//! * All bookkeeping lives in one [`FrameMeta`] per frame (12 bytes, 0.3 % of
//!   managed memory) in a caller-provided slice. Free lists are doubly linked
//!   through frame indices in that slice rather than through pointers stored
//!   in the free pages themselves. That costs a little memory, but it means
//!   the allocator never touches the memory it manages, needs no `unsafe`,
//!   and cannot be corrupted by a stray write to a free page.
//! * Frees are validated: freeing a frame that is not the head of an
//!   allocated block, or with the wrong order, is reported instead of
//!   corrupting the free lists.
//!
//! Frame numbers are indices into the metadata slice; the caller decides how
//! they relate to physical addresses (the kernel uses `address / 4096`).

/// Largest supported block order: `2^10` frames (4 MiB with 4 KiB frames).
pub const MAX_ORDER: u8 = 10;

const ORDERS: usize = MAX_ORDER as usize + 1;
const NIL: u32 = u32::MAX;

/// Not handed to the allocator (reserved memory, holes, firmware).
const STATE_RESERVED: u8 = 0;
/// Managed, but not the first frame of a block.
const STATE_TAIL: u8 = 1;
/// First frame of a free block of `order`; linked into a free list.
const STATE_FREE_HEAD: u8 = 2;
/// First frame of an allocated block of `order`.
const STATE_ALLOC_HEAD: u8 = 3;

/// Per-frame bookkeeping.
///
/// The all-zero bit pattern is a valid value and means "reserved", so the
/// kernel can create the metadata array from zeroed memory.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameMeta {
    next: u32,
    prev: u32,
    order: u8,
    state: u8,
}

impl FrameMeta {
    /// Metadata for a frame the allocator does not manage.
    pub const RESERVED: Self = Self {
        next: 0,
        prev: 0,
        order: 0,
        state: STATE_RESERVED,
    };
}

/// A rejected allocator operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuddyError {
    /// The frame number, order or range lies outside the metadata slice or
    /// the supported order range.
    OutOfRange,
    /// The range passed to [`Buddy::add_free_range`] overlaps frames that
    /// are already managed.
    AlreadyManaged,
    /// The frame is not the first frame of an allocated block (double free,
    /// or a pointer into the middle of a block).
    NotAllocated,
    /// The block was allocated with a different order than it is being
    /// freed with.
    WrongOrder,
}

impl core::fmt::Display for BuddyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::OutOfRange => "frame or order out of range",
            Self::AlreadyManaged => "frame range is already managed",
            Self::NotAllocated => "frame is not the head of an allocated block",
            Self::WrongOrder => "block freed with the wrong order",
        })
    }
}

/// A buddy allocator over the frames described by a metadata slice.
#[derive(Debug)]
pub struct Buddy<'a> {
    frames: &'a mut [FrameMeta],
    heads: [u32; ORDERS],
    free_blocks: [usize; ORDERS],
    free_frames: usize,
    managed_frames: usize,
}

impl<'a> Buddy<'a> {
    /// Creates an allocator with every frame reserved. Memory is made
    /// available with [`Buddy::add_free_range`].
    ///
    /// The contents of `frames` are overwritten. At most `u32::MAX` frames
    /// (16 TiB of 4 KiB frames) are supported; a longer slice is truncated.
    pub fn new(frames: &'a mut [FrameMeta]) -> Self {
        let len = frames.len().min(NIL as usize);
        let frames = &mut frames[..len];
        frames.fill(FrameMeta::RESERVED);
        Self {
            frames,
            heads: [NIL; ORDERS],
            free_blocks: [0; ORDERS],
            free_frames: 0,
            managed_frames: 0,
        }
    }

    /// Number of frames covered by the metadata (managed or not).
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Number of frames handed to the allocator, free or allocated.
    pub fn managed_frames(&self) -> usize {
        self.managed_frames
    }

    /// Number of frames currently free.
    pub fn free_frames(&self) -> usize {
        self.free_frames
    }

    /// Number of free blocks of exactly `order`.
    pub fn free_blocks(&self, order: u8) -> usize {
        self.free_blocks.get(order as usize).copied().unwrap_or(0)
    }

    /// The largest order for which a free block exists right now.
    pub fn largest_free_order(&self) -> Option<u8> {
        (0..=MAX_ORDER)
            .rev()
            .find(|&o| self.heads[o as usize] != NIL)
    }

    fn push(&mut self, frame: u32, order: u8) {
        let head = self.heads[order as usize];
        self.frames[frame as usize] = FrameMeta {
            next: head,
            prev: NIL,
            order,
            state: STATE_FREE_HEAD,
        };
        if head != NIL {
            self.frames[head as usize].prev = frame;
        }
        self.heads[order as usize] = frame;
        self.free_blocks[order as usize] += 1;
    }

    /// Removes the free block headed by `frame` from its list, leaving the
    /// frame in the tail state.
    fn unlink(&mut self, frame: u32) {
        let FrameMeta {
            next, prev, order, ..
        } = self.frames[frame as usize];
        if prev == NIL {
            self.heads[order as usize] = next;
        } else {
            self.frames[prev as usize].next = next;
        }
        if next != NIL {
            self.frames[next as usize].prev = prev;
        }
        self.free_blocks[order as usize] -= 1;
        self.frames[frame as usize].state = STATE_TAIL;
    }

    /// Hands the frames `start..end` to the allocator as free memory.
    ///
    /// Fails without changing anything if the range is out of bounds or
    /// overlaps frames that are already managed. An empty range is accepted.
    pub fn add_free_range(&mut self, start: usize, end: usize) -> Result<(), BuddyError> {
        if start > end || end > self.frames.len() {
            return Err(BuddyError::OutOfRange);
        }
        if self.frames[start..end]
            .iter()
            .any(|f| f.state != STATE_RESERVED)
        {
            return Err(BuddyError::AlreadyManaged);
        }
        for frame in &mut self.frames[start..end] {
            frame.state = STATE_TAIL;
        }
        self.managed_frames += end - start;

        // Carve the range into the largest naturally aligned blocks and
        // release each through the normal free path, which also merges with
        // adjacent ranges added earlier.
        let mut frame = start;
        while frame < end {
            let align = if frame == 0 {
                u32::from(MAX_ORDER)
            } else {
                frame.trailing_zeros()
            };
            let fit = (end - frame).ilog2();
            let order = align.min(fit).min(u32::from(MAX_ORDER)) as u8;
            self.release(frame as u32, order);
            frame += 1 << order;
        }
        Ok(())
    }

    /// Allocates a block of `2^order` contiguous frames and returns the
    /// number of its first frame, which is a multiple of `2^order`.
    ///
    /// Returns `None` if `order` exceeds [`MAX_ORDER`] or no block of
    /// sufficient size is free.
    pub fn alloc(&mut self, order: u8) -> Option<usize> {
        if order > MAX_ORDER {
            return None;
        }
        let mut current = (order..=MAX_ORDER).find(|&o| self.heads[o as usize] != NIL)?;
        let frame = self.heads[current as usize];
        self.unlink(frame);

        // Split down to the requested size, returning the upper halves.
        while current > order {
            current -= 1;
            self.push(frame + (1 << current), current);
        }

        let meta = &mut self.frames[frame as usize];
        meta.state = STATE_ALLOC_HEAD;
        meta.order = order;
        self.free_frames -= 1 << order;
        Some(frame as usize)
    }

    /// Frees the block of `2^order` frames starting at `frame`, which must
    /// have been returned by [`Buddy::alloc`] with the same `order` and not
    /// freed since.
    pub fn free(&mut self, frame: usize, order: u8) -> Result<(), BuddyError> {
        if order > MAX_ORDER || frame >= self.frames.len() {
            return Err(BuddyError::OutOfRange);
        }
        let meta = self.frames[frame];
        if meta.state != STATE_ALLOC_HEAD {
            return Err(BuddyError::NotAllocated);
        }
        if meta.order != order {
            return Err(BuddyError::WrongOrder);
        }
        self.release(frame as u32, order);
        Ok(())
    }

    /// Inserts a block into the free lists, merging with free buddies.
    /// The caller guarantees the block is managed, aligned and not free.
    fn release(&mut self, mut frame: u32, mut order: u8) {
        self.free_frames += 1 << order;
        self.frames[frame as usize].state = STATE_TAIL;

        while order < MAX_ORDER {
            let buddy = frame ^ (1 << order);
            let Some(meta) = self.frames.get(buddy as usize) else {
                break;
            };
            // The buddy can only be merged if all of it is one free block of
            // the same size; a free head of a smaller order means part of
            // the buddy is still in use.
            if meta.state != STATE_FREE_HEAD || meta.order != order {
                break;
            }
            self.unlink(buddy);
            frame = frame.min(buddy);
            order += 1;
        }
        self.push(frame, order);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    const MAX_BLOCK: usize = 1 << MAX_ORDER;

    #[test]
    fn metadata_layout_is_compact_and_zero_is_reserved() {
        assert_eq!(size_of::<FrameMeta>(), 12);
        assert_eq!(FrameMeta::RESERVED.state, 0);
        assert_eq!(FrameMeta::RESERVED.next, 0);
    }

    #[test]
    fn empty_allocator_allocates_nothing() {
        let mut meta = vec![FrameMeta::RESERVED; 64];
        let mut buddy = Buddy::new(&mut meta);
        assert_eq!(buddy.alloc(0), None);
        assert_eq!(buddy.free_frames(), 0);
        assert_eq!(buddy.largest_free_order(), None);
        assert_eq!(buddy.add_free_range(10, 10), Ok(()));
        assert_eq!(buddy.managed_frames(), 0);
    }

    #[test]
    fn aligned_range_becomes_maximal_blocks() {
        let mut meta = vec![FrameMeta::RESERVED; 4 * MAX_BLOCK];
        let mut buddy = Buddy::new(&mut meta);
        buddy.add_free_range(0, 4 * MAX_BLOCK).unwrap();
        assert_eq!(buddy.free_blocks(MAX_ORDER), 4);
        assert_eq!(buddy.free_frames(), 4 * MAX_BLOCK);
        assert_eq!(buddy.largest_free_order(), Some(MAX_ORDER));
    }

    #[test]
    fn unaligned_range_is_fully_usable() {
        let mut meta = vec![FrameMeta::RESERVED; 100];
        let mut buddy = Buddy::new(&mut meta);
        buddy.add_free_range(3, 37).unwrap();
        assert_eq!(buddy.free_frames(), 34);
        assert_eq!(buddy.managed_frames(), 34);

        let mut got = Vec::new();
        while let Some(frame) = buddy.alloc(0) {
            got.push(frame);
        }
        got.sort_unstable();
        assert_eq!(got, (3..37).collect::<Vec<_>>());
        assert_eq!(buddy.free_frames(), 0);
    }

    #[test]
    fn split_and_merge_round_trip() {
        let mut meta = vec![FrameMeta::RESERVED; MAX_BLOCK];
        let mut buddy = Buddy::new(&mut meta);
        buddy.add_free_range(0, MAX_BLOCK).unwrap();

        let a = buddy.alloc(0).unwrap();
        // One block of every order below the maximum is left over.
        for order in 0..MAX_ORDER {
            assert_eq!(buddy.free_blocks(order), 1, "order {order}");
        }
        assert_eq!(buddy.free_blocks(MAX_ORDER), 0);

        let b = buddy.alloc(3).unwrap();
        assert_eq!(b % 8, 0);
        buddy.free(a, 0).unwrap();
        buddy.free(b, 3).unwrap();
        assert_eq!(buddy.free_blocks(MAX_ORDER), 1);
        assert_eq!(buddy.free_frames(), MAX_BLOCK);
    }

    #[test]
    fn adjacent_ranges_merge_but_reserved_holes_do_not() {
        let mut meta = vec![FrameMeta::RESERVED; 32];
        let mut buddy = Buddy::new(&mut meta);
        buddy.add_free_range(0, 8).unwrap();
        buddy.add_free_range(8, 16).unwrap();
        assert_eq!(buddy.free_blocks(4), 1, "two halves merge into 16 frames");

        // Frame 24 stays reserved: 16..24 and 25..32 must never merge
        // across it.
        buddy.add_free_range(16, 24).unwrap();
        buddy.add_free_range(25, 32).unwrap();
        assert_eq!(buddy.free_blocks(5), 0);
        assert_eq!(buddy.alloc(5), None);
        assert_eq!(buddy.free_frames(), 31);
    }

    #[test]
    fn rejects_invalid_operations() {
        let mut meta = vec![FrameMeta::RESERVED; 64];
        let mut buddy = Buddy::new(&mut meta);
        buddy.add_free_range(0, 32).unwrap();

        assert_eq!(
            buddy.add_free_range(16, 48),
            Err(BuddyError::AlreadyManaged)
        );
        assert_eq!(buddy.add_free_range(60, 65), Err(BuddyError::OutOfRange));
        assert_eq!(buddy.add_free_range(9, 8), Err(BuddyError::OutOfRange));
        assert_eq!(buddy.managed_frames(), 32, "failed adds change nothing");
        assert_eq!(buddy.alloc(MAX_ORDER + 1), None);

        let frame = buddy.alloc(2).unwrap();
        assert_eq!(buddy.free(frame, 1), Err(BuddyError::WrongOrder));
        assert_eq!(buddy.free(frame + 1, 0), Err(BuddyError::NotAllocated));
        assert_eq!(buddy.free(40, 0), Err(BuddyError::NotAllocated));
        assert_eq!(buddy.free(64, 0), Err(BuddyError::OutOfRange));
        assert_eq!(
            buddy.free(frame, MAX_ORDER + 1),
            Err(BuddyError::OutOfRange)
        );
        assert_eq!(buddy.free(frame, 2), Ok(()));
        assert_eq!(buddy.free(frame, 2), Err(BuddyError::NotAllocated));
        assert_eq!(buddy.free_frames(), 32);
    }

    #[test]
    fn exhaustion_and_recovery() {
        let mut meta = vec![FrameMeta::RESERVED; 16];
        let mut buddy = Buddy::new(&mut meta);
        buddy.add_free_range(0, 16).unwrap();
        let blocks: Vec<_> = (0..4).map(|_| buddy.alloc(2).unwrap()).collect();
        assert_eq!(buddy.alloc(0), None);
        for block in blocks {
            buddy.free(block, 2).unwrap();
        }
        assert_eq!(buddy.alloc(4), Some(0));
    }

    /// Deterministic xorshift generator; the test must not depend on a
    /// random-number crate or on the host's entropy.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    /// Random allocations and frees, checked against an independent model:
    /// blocks never overlap each other or unmanaged frames, accounting stays
    /// exact, and once everything is freed the memory is fully coalesced.
    #[test]
    fn randomised_against_model() {
        const FRAMES: usize = 6 * MAX_BLOCK + 123;
        // Managed ranges, with reserved holes between them.
        let ranges = [(5, 2 * MAX_BLOCK), (2 * MAX_BLOCK + 7, FRAMES - 3)];

        let mut meta = vec![FrameMeta::RESERVED; FRAMES];
        let mut buddy = Buddy::new(&mut meta);
        let mut managed = vec![false; FRAMES];
        for (start, end) in ranges {
            buddy.add_free_range(start, end).unwrap();
            managed[start..end].fill(true);
        }
        let total: usize = managed.iter().filter(|&&m| m).count();
        assert_eq!(buddy.managed_frames(), total);
        assert_eq!(buddy.free_frames(), total);
        let initial_blocks: Vec<_> = (0..=MAX_ORDER).map(|o| buddy.free_blocks(o)).collect();

        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut in_use = vec![false; FRAMES];
        let mut live: Vec<(usize, u8)> = Vec::new();
        let mut used = 0usize;

        for _ in 0..20_000 {
            let allocate = live.is_empty() || rng.next() % 5 < 3;
            if allocate {
                let order = (rng.next() % (u64::from(MAX_ORDER) + 1)) as u8;
                if let Some(frame) = buddy.alloc(order) {
                    let size = 1usize << order;
                    assert_eq!(frame % size, 0, "block is naturally aligned");
                    for i in frame..frame + size {
                        assert!(managed[i], "allocated an unmanaged frame");
                        assert!(!in_use[i], "allocated a frame twice");
                        in_use[i] = true;
                    }
                    live.push((frame, order));
                    used += size;
                }
            } else {
                let index = (rng.next() as usize) % live.len();
                let (frame, order) = live.swap_remove(index);
                buddy.free(frame, order).unwrap();
                in_use[frame..frame + (1 << order)].fill(false);
                used -= 1 << order;
            }
            assert_eq!(buddy.free_frames(), total - used);
        }

        for (frame, order) in live.drain(..) {
            buddy.free(frame, order).unwrap();
        }
        assert_eq!(buddy.free_frames(), total);
        let final_blocks: Vec<_> = (0..=MAX_ORDER).map(|o| buddy.free_blocks(o)).collect();
        assert_eq!(final_blocks, initial_blocks, "memory fully coalesced");
    }
}
