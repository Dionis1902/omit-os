// SPDX-License-Identifier: GPL-2.0-only
//! Physical page-frame allocator.
//!
//! A thin, locked wrapper around the buddy allocator from the `omit_mm`
//! crate. This module's own job is the part that cannot be tested on the
//! host: deciding which physical memory to manage and finding a home for the
//! allocator's metadata.
//!
//! Managed memory is every [`MemoryKind::USABLE`] region above 1 MiB.
//!
//! * The first megabyte is left alone: it is riddled with legacy holes and
//!   will be needed for application-processor startup code.
//! * Boot-reclaimable memory (firmware boot services, the loader) is not
//!   reclaimed yet, although nothing in it is still needed.

use core::fmt;

use omit_bootproto::{MemoryKind, MemoryRegion};
use omit_mm::{Buddy, BuddyError, FrameMeta, MAX_ORDER};

use super::{PAGE_SIZE, phys_to_virt};
use crate::sync::SpinLock;

/// Physical memory below this address is never handed to the allocator.
const LOW_MEMORY_RESERVE: u64 = 1 << 20;

static ALLOCATOR: SpinLock<Option<Buddy<'static>>> = SpinLock::new(None);

/// Why the frame allocator could not be set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameInitError {
    /// [`init`] was called twice.
    AlreadyInitialised,
    /// No usable region is large enough to hold the allocator metadata.
    NoRoomForMetadata,
    /// Usable memory extends beyond the direct map.
    BeyondDirectMap,
    /// The buddy allocator rejected a region (overlapping memory map).
    Buddy(BuddyError),
}

impl fmt::Display for FrameInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialised => write!(f, "frame allocator initialised twice"),
            Self::NoRoomForMetadata => write!(f, "no usable region can hold the frame metadata"),
            Self::BeyondDirectMap => write!(f, "usable memory lies outside the direct map"),
            Self::Buddy(e) => write!(f, "{e}"),
        }
    }
}

/// A snapshot of the allocator's accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    /// Frames handed to the allocator, free or allocated.
    pub managed: usize,
    /// Frames currently free.
    pub free: usize,
    /// Bytes used by the allocator's own metadata.
    pub metadata_bytes: u64,
}

/// The part of a usable region the allocator may manage, as a frame range.
fn manageable(region: &MemoryRegion) -> Option<(u64, u64)> {
    if region.kind != MemoryKind::USABLE {
        return None;
    }
    let start = region.start.max(LOW_MEMORY_RESERVE);
    let end = region.end()?;
    (start < end).then_some((start / PAGE_SIZE, end / PAGE_SIZE))
}

/// Builds the frame allocator from the boot memory map.
///
/// # Safety
///
/// * `map` must be the validated boot memory map, and every `USABLE` region
///   in it must be RAM that nothing else in the system uses or references.
/// * [`super::init_direct_map`] must have been called.
pub unsafe fn init(map: &[MemoryRegion]) -> Result<FrameStats, FrameInitError> {
    let mut allocator = ALLOCATOR.lock();
    if allocator.is_some() {
        return Err(FrameInitError::AlreadyInitialised);
    }

    // One metadata entry per frame from frame 0 to the end of the highest
    // manageable region.
    let frame_count = map
        .iter()
        .filter_map(manageable)
        .map(|(_, end)| end)
        .max()
        .unwrap_or(0);
    let metadata_bytes = frame_count * size_of::<FrameMeta>() as u64;
    let metadata_frames = metadata_bytes.div_ceil(PAGE_SIZE);

    // Put the metadata at the start of the largest region that can hold it.
    let (metadata_start, _) = map
        .iter()
        .filter_map(manageable)
        .filter(|(start, end)| end - start >= metadata_frames)
        .max_by_key(|(start, end)| end - start)
        .ok_or(FrameInitError::NoRoomForMetadata)?;
    let metadata_end = metadata_start + metadata_frames;

    let metadata_ptr = phys_to_virt(metadata_start * PAGE_SIZE, metadata_frames * PAGE_SIZE)
        .ok_or(FrameInitError::BeyondDirectMap)?
        .cast::<FrameMeta>();
    // SAFETY: the range is usable RAM that per the function contract nothing
    // else uses, it lies inside the direct map (checked by `phys_to_virt`),
    // is page-aligned (so suitably aligned for `FrameMeta`) and large enough
    // for `frame_count` entries. It is zeroed before the slice is formed,
    // and all-zero bytes are a valid `FrameMeta`. The range is excluded from
    // the memory handed to the allocator below and never freed, so the
    // `'static` exclusive borrow is sound.
    let metadata = unsafe {
        metadata_ptr
            .cast::<u8>()
            .write_bytes(0, (metadata_frames * PAGE_SIZE) as usize);
        core::slice::from_raw_parts_mut(metadata_ptr, frame_count as usize)
    };

    let mut buddy = Buddy::new(metadata);
    for (start, end) in map.iter().filter_map(manageable) {
        // Skip the frames now occupied by the metadata.
        let start = if start == metadata_start {
            metadata_end
        } else {
            start
        };
        buddy
            .add_free_range(start as usize, end as usize)
            .map_err(FrameInitError::Buddy)?;
    }

    let stats = FrameStats {
        managed: buddy.managed_frames(),
        free: buddy.free_frames(),
        metadata_bytes: metadata_frames * PAGE_SIZE,
    };
    *allocator = Some(buddy);
    Ok(stats)
}

/// Allocates `2^order` contiguous frames and returns the physical address of
/// the first, aligned to the block size. The contents are unspecified.
///
/// Returns `None` if memory is exhausted, `order` is too large, or the
/// allocator is not initialised.
pub fn alloc(order: u8) -> Option<u64> {
    let frame = ALLOCATOR.lock().as_mut()?.alloc(order)?;
    Some(frame as u64 * PAGE_SIZE)
}

/// Like [`alloc`], but the returned memory is zero-filled.
pub fn alloc_zeroed(order: u8) -> Option<u64> {
    let phys = alloc(order)?;
    let len = PAGE_SIZE << order;
    let ptr = phys_to_virt(phys, len).expect("managed frames lie inside the direct map");
    // SAFETY: the block was just allocated, so this code owns it exclusively;
    // it is `len` bytes long and mapped writable through the direct map.
    unsafe { ptr.write_bytes(0, len as usize) };
    Some(phys)
}

/// Returns a block obtained from [`alloc`] to the allocator.
///
/// Misuse that the allocator can detect (double free, wrong order, an
/// address that is not the start of a block) is reported and leaves the
/// allocator unchanged.
///
/// # Safety
///
/// Nothing may use the block's memory after this call: no live references,
/// no mappings, no pending device I/O.
pub unsafe fn free(phys: u64, order: u8) -> Result<(), BuddyError> {
    if !phys.is_multiple_of(PAGE_SIZE) {
        return Err(BuddyError::NotAllocated);
    }
    let frame = usize::try_from(phys / PAGE_SIZE).map_err(|_| BuddyError::OutOfRange)?;
    match ALLOCATOR.lock().as_mut() {
        Some(buddy) => buddy.free(frame, order),
        None => Err(BuddyError::NotAllocated),
    }
}

/// Current allocator accounting, or `None` before [`init`].
pub fn stats() -> Option<FrameStats> {
    let allocator = ALLOCATOR.lock();
    let buddy = allocator.as_ref()?;
    Some(FrameStats {
        managed: buddy.managed_frames(),
        free: buddy.free_frames(),
        metadata_bytes: (buddy.frame_count() * size_of::<FrameMeta>()) as u64,
    })
}

/// Exercises the allocator against real memory: allocates blocks of several
/// orders, checks alignment and that they are distinct, writes and reads
/// back a pattern through the direct map, verifies that invalid frees are
/// rejected, then frees everything and checks that the accounting returns to
/// its starting point.
pub fn self_test() -> Result<(), &'static str> {
    const ORDERS: [u8; 4] = [0, 0, 3, MAX_ORDER];
    let before = stats().ok_or("allocator not initialised")?.free;

    let mut blocks = [0u64; ORDERS.len()];
    for (slot, order) in blocks.iter_mut().zip(ORDERS) {
        let phys = alloc_zeroed(order).ok_or("allocation failed")?;
        if !phys.is_multiple_of(PAGE_SIZE << order) {
            return Err("block is not naturally aligned");
        }
        if phys < LOW_MEMORY_RESERVE {
            return Err("block lies in reserved low memory");
        }
        *slot = phys;
    }
    if blocks[0] == blocks[1] {
        return Err("the same frame was allocated twice");
    }

    for (index, (&phys, order)) in blocks.iter().zip(ORDERS).enumerate() {
        let len = (PAGE_SIZE << order) as usize;
        let ptr = phys_to_virt(phys, len as u64).ok_or("block outside the direct map")?;
        let pattern = 0xA5u8.wrapping_add(index as u8);
        // SAFETY: the block is owned by this test (allocated above, not yet
        // freed), `len` bytes long and mapped writable in the direct map.
        // Volatile access keeps the compiler from eliding the memory test.
        unsafe {
            if ptr.read_volatile() != 0 || ptr.add(len - 1).read_volatile() != 0 {
                return Err("alloc_zeroed returned non-zero memory");
            }
            ptr.write_volatile(pattern);
            ptr.add(len - 1).write_volatile(!pattern);
            if ptr.read_volatile() != pattern || ptr.add(len - 1).read_volatile() != !pattern {
                return Err("memory did not retain the test pattern");
            }
        }
    }

    let expected_free = before - ORDERS.iter().map(|&o| 1usize << o).sum::<usize>();
    if stats().map(|s| s.free) != Some(expected_free) {
        return Err("free-frame accounting is wrong after allocation");
    }

    // SAFETY: the blocks belong to this test and nothing references their
    // memory any more. The first two calls and the last are deliberately
    // invalid and must be rejected without side effects.
    unsafe {
        if free(blocks[2], 2) != Err(BuddyError::WrongOrder) {
            return Err("free with the wrong order was not rejected");
        }
        if free(blocks[2] + PAGE_SIZE, 0) != Err(BuddyError::NotAllocated) {
            return Err("free of a block interior was not rejected");
        }
        for (&phys, order) in blocks.iter().zip(ORDERS) {
            free(phys, order).map_err(|_| "valid free was rejected")?;
        }
        if free(blocks[0], 0) != Err(BuddyError::NotAllocated) {
            return Err("double free was not rejected");
        }
    }

    if stats().map(|s| s.free) != Some(before) {
        return Err("free-frame accounting did not return to its initial value");
    }
    Ok(())
}
