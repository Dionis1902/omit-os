// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Kernel address-space finalisation and auditing.
//!
//! The loader hands over page tables that map the kernel image, the boot
//! stack, the direct map of physical memory, and (so the loader itself could
//! keep running across the switch) an identity mapping of the lower half.
//! This module removes that identity mapping and then verifies, by walking
//! the live page tables, that the address space has the properties the
//! kernel relies on.

use core::fmt;

use omit_bootproto::{BootInfo, PAGE_SIZE};

use super::cpu;

const ENTRIES: usize = 512;
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const HUGE: u64 = 1 << 7;
const NO_EXECUTE: u64 = 1 << 63;
const ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// A violated address-space invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagingError {
    /// A page table lies outside the direct map and cannot be inspected.
    TableOutsideDirectMap {
        /// Physical address of the table.
        phys: u64,
    },
    /// A mapping is both writable and executable.
    WritableExecutable {
        /// Virtual address of the offending mapping.
        virt: u64,
    },
    /// Something is still mapped in the lower (user) half.
    LowerHalfMapped {
        /// Index of the offending top-level entry.
        pml4_index: usize,
    },
    /// The huge-page bit is set at a level that does not allow it.
    MalformedEntry {
        /// Virtual address translated by the offending entry.
        virt: u64,
    },
}

impl fmt::Display for PagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TableOutsideDirectMap { phys } => {
                write!(f, "page table at {phys:#x} is outside the direct map")
            }
            Self::WritableExecutable { virt } => {
                write!(f, "W^X violation: {virt:#018x} is writable and executable")
            }
            Self::LowerHalfMapped { pml4_index } => {
                write!(f, "lower-half PML4 entry {pml4_index} is still present")
            }
            Self::MalformedEntry { virt } => {
                write!(f, "malformed page table entry for {virt:#018x}")
            }
        }
    }
}

/// Totals gathered by [`audit`], in bytes of mapped virtual address space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AuditReport {
    /// Mapped read + execute.
    pub executable: u64,
    /// Mapped read-only, non-executable.
    pub read_only: u64,
    /// Mapped read + write, non-executable.
    pub writable: u64,
    /// Number of page-table pages reachable from the root.
    pub tables: u64,
}

fn table(boot_info: &BootInfo, phys: u64) -> Result<*mut [u64; ENTRIES], PagingError> {
    let end = phys.checked_add(PAGE_SIZE);
    match (boot_info.phys_to_virt(phys), end) {
        (Some(virt), Some(end)) if end <= boot_info.phys_map_size => Ok(virt as *mut _),
        _ => Err(PagingError::TableOutsideDirectMap { phys }),
    }
}

/// Removes the loader's identity mapping of the lower half of the address
/// space and flushes the TLB.
///
/// Afterwards every address below the canonical hole, including the null
/// page, faults when accessed.
///
/// # Safety
///
/// * `boot_info` must have passed [`BootInfo::validate`] and describe the
///   active address space.
/// * The caller must be executing from, and using a stack in, the higher
///   half, and must hold no pointers into the identity mapping.
pub unsafe fn unmap_identity(boot_info: &BootInfo) -> Result<(), PagingError> {
    let cr3 = cpu::read_cr3();
    let pml4 = table(boot_info, cr3 & ADDRESS_MASK)?;
    // SAFETY: `pml4` is the direct-map alias of the active top-level table
    // (checked to lie inside the direct map), valid for reads and writes of
    // 512 entries. Per the function contract nothing the kernel uses lives
    // in the lower half, so clearing entries 0..256 unmaps only memory that
    // is no longer referenced. Reloading the unchanged CR3 is valid and
    // flushes the stale (non-global) translations.
    unsafe {
        for index in 0..ENTRIES / 2 {
            (&raw mut (*pml4)[index]).write_volatile(0);
        }
        cpu::write_cr3(cr3);
    }
    Ok(())
}

/// Walks the active page tables and verifies that
///
/// * nothing is mapped in the lower half,
/// * no mapping is simultaneously writable and executable (W^X),
/// * every table is reachable through the direct map.
///
/// # Safety
///
/// `boot_info` must have passed [`BootInfo::validate`] and describe the
/// active address space, and the page tables must not be modified
/// concurrently.
pub unsafe fn audit(boot_info: &BootInfo) -> Result<AuditReport, PagingError> {
    let mut report = AuditReport::default();
    let root = cpu::read_cr3() & ADDRESS_MASK;
    // SAFETY: forwarded from the function contract.
    unsafe { walk(boot_info, root, 4, 0, true, false, &mut report)? };
    Ok(report)
}

/// Recursively audits the table at `phys`.
///
/// `level` is 4 for the PML4 down to 1 for a page table; `base` is the
/// virtual address translated by the table's first entry; `writable` and
/// `no_execute` are the effective permissions accumulated on the path from
/// the root (write permission is the AND, no-execute the OR of all levels).
///
/// # Safety
///
/// Same contract as [`audit`]; `phys` must be the address of a live page
/// table at `level`.
unsafe fn walk(
    boot_info: &BootInfo,
    phys: u64,
    level: u8,
    base: u64,
    writable: bool,
    no_execute: bool,
    report: &mut AuditReport,
) -> Result<(), PagingError> {
    let table = table(boot_info, phys)?;
    report.tables += 1;
    let span = PAGE_SIZE << (9 * (u32::from(level) - 1));

    for index in 0..ENTRIES {
        // SAFETY: `table` is the direct-map alias of a live page table
        // (function contract), valid for reads of 512 entries; `index` is in
        // bounds.
        let entry = unsafe { (&raw const (*table)[index]).read_volatile() };
        if entry & PRESENT == 0 {
            continue;
        }

        let mut virt = base + index as u64 * span;
        if level == 4 {
            if index < ENTRIES / 2 {
                return Err(PagingError::LowerHalfMapped { pml4_index: index });
            }
            virt |= 0xFFFF_0000_0000_0000; // canonical sign extension
        }
        let writable = writable && entry & WRITABLE != 0;
        let no_execute = no_execute || entry & NO_EXECUTE != 0;

        let is_leaf = level == 1 || entry & HUGE != 0;
        if !is_leaf {
            // SAFETY: a present non-leaf entry points at the next-level
            // table; the remaining contract is forwarded.
            unsafe {
                walk(
                    boot_info,
                    entry & ADDRESS_MASK,
                    level - 1,
                    virt,
                    writable,
                    no_execute,
                    report,
                )?;
            }
            continue;
        }
        if level > 3 {
            return Err(PagingError::MalformedEntry { virt });
        }

        match (writable, no_execute) {
            (true, false) => return Err(PagingError::WritableExecutable { virt }),
            (false, false) => report.executable += span,
            (false, true) => report.read_only += span,
            (true, true) => report.writable += span,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mapping
// ---------------------------------------------------------------------------

use crate::mm::{self, frame};
use crate::sync::SpinLock;

/// Page-level cache disable. With the power-on PAT this selects the
/// "uncacheable" memory type, which is what device registers need.
const CACHE_DISABLE: u64 = 1 << 4;

/// Page-level write-through bit. Selects PAT entry 1, which the kernel
/// reprograms from "write-through" to "write-combining".
const WRITE_THROUGH_BIT: u64 = 1 << 3;

/// Start of the virtual range used for device memory (PML4 slot 384).
const MMIO_BASE: u64 = 0xFFFF_C000_0000_0000;
/// Exclusive end of the device memory range (64 GiB).
const MMIO_END: u64 = MMIO_BASE + (64 << 30);

/// Why a mapping could not be established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// No frame was available for a page table.
    OutOfMemory,
    /// The virtual address is already mapped.
    AlreadyMapped {
        /// The offending virtual address.
        virt: u64,
    },
    /// An address is not page-aligned, the range is empty, or it overflows.
    InvalidRange,
    /// The device-memory virtual range is exhausted.
    OutOfVirtualSpace,
    /// A page table could not be reached through the direct map.
    Table(PagingError),
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => write!(f, "out of memory for page tables"),
            Self::AlreadyMapped { virt } => write!(f, "{virt:#018x} is already mapped"),
            Self::InvalidRange => write!(f, "invalid address range"),
            Self::OutOfVirtualSpace => write!(f, "device-memory address range exhausted"),
            Self::Table(e) => write!(f, "{e}"),
        }
    }
}

/// State of the kernel address space that mapping operations mutate. The
/// lock also serialises all page-table modifications.
struct KernelSpace {
    /// Next unused address in the device-memory range.
    mmio_next: u64,
}

static KERNEL_SPACE: SpinLock<KernelSpace> = SpinLock::new(KernelSpace {
    mmio_next: MMIO_BASE,
});

fn table_ptr(phys: u64) -> Result<*mut [u64; ENTRIES], MapError> {
    mm::phys_to_virt(phys, PAGE_SIZE)
        .map(|ptr| ptr.cast())
        .ok_or(MapError::Table(PagingError::TableOutsideDirectMap { phys }))
}

/// Maps the 4 KiB page at kernel virtual address `virt` to the physical
/// frame `phys` with leaf `flags`, allocating intermediate tables as needed.
///
/// # Safety
///
/// The caller must hold the `KERNEL_SPACE` lock, `virt` must be a higher-half
/// address that the kernel image, stack and direct map do not use, and the
/// frame allocator must be initialised.
unsafe fn map_page_locked(virt: u64, phys: u64, flags: u64) -> Result<(), MapError> {
    let mut table = table_ptr(cpu::read_cr3() & ADDRESS_MASK)?;
    for level in (2..=4u32).rev() {
        let index = ((virt >> (12 + 9 * (level - 1))) & 0x1FF) as usize;
        // SAFETY: `table` is the direct-map alias of a live page table,
        // valid for reads and writes of 512 entries; `index < 512`. The
        // caller holds the lock that serialises page-table updates.
        let entry = unsafe { (&raw mut (*table)[index]).read_volatile() };
        let next = if entry & PRESENT == 0 {
            let frame = frame::alloc_zeroed(0).ok_or(MapError::OutOfMemory)?;
            // Intermediate entries are maximally permissive; the leaf
            // decides. SAFETY: as above.
            unsafe { (&raw mut (*table)[index]).write_volatile(frame | PRESENT | WRITABLE) };
            frame
        } else if entry & HUGE != 0 {
            return Err(MapError::AlreadyMapped { virt });
        } else {
            entry & ADDRESS_MASK
        };
        table = table_ptr(next)?;
    }

    let index = ((virt >> 12) & 0x1FF) as usize;
    // SAFETY: `table` is now the level-1 table for `virt`; see above.
    unsafe {
        let slot = &raw mut (*table)[index];
        if slot.read_volatile() & PRESENT != 0 {
            return Err(MapError::AlreadyMapped { virt });
        }
        // A previously non-present entry is never cached in the TLB, so no
        // invalidation is needed.
        slot.write_volatile(phys | flags | PRESENT);
    }
    Ok(())
}

/// Maps the device memory `[phys, phys + len)` into the kernel's
/// device-memory range and returns the virtual address corresponding to
/// `phys`.
///
/// The mapping is writable, non-executable and uncached, and is separated
/// from neighbouring mappings by an unmapped guard page. Mappings are
/// permanent; there is no unmap yet.
///
/// Establishing the mapping is safe; accessing the device through the
/// returned address is subject to the usual raw-pointer rules.
pub fn map_mmio(phys: u64, len: u64) -> Result<u64, MapError> {
    map_device(phys, len, CACHE_DISABLE)
}

/// Like [`map_mmio`], but with the write-combining memory type, which is
/// what a linear framebuffer wants: writes are batched instead of each one
/// going to the device individually. Relies on PAT entry 1 having been
/// programmed for write-combining by [`cpu::init`]; on CPUs without PAT the
/// mapping silently behaves as write-through, which is merely slower.
pub fn map_framebuffer(phys: u64, len: u64) -> Result<u64, MapError> {
    map_device(phys, len, WRITE_THROUGH_BIT)
}

fn map_device(phys: u64, len: u64, cache_flags: u64) -> Result<u64, MapError> {
    let offset = phys % PAGE_SIZE;
    let first = phys - offset;
    let pages = offset
        .checked_add(len)
        .filter(|_| len != 0)
        .ok_or(MapError::InvalidRange)?
        .div_ceil(PAGE_SIZE);
    if first
        .checked_add(pages * PAGE_SIZE)
        .is_none_or(|end| end > ADDRESS_MASK)
    {
        return Err(MapError::InvalidRange);
    }

    let mut space = KERNEL_SPACE.lock();
    let virt = space.mmio_next;
    let next = virt + (pages + 1) * PAGE_SIZE; // + guard page
    if next > MMIO_END {
        return Err(MapError::OutOfVirtualSpace);
    }
    space.mmio_next = next;

    for page in 0..pages {
        // SAFETY: the lock is held; the address comes from the dedicated
        // device-memory range, which nothing else uses and from which each
        // page is handed out exactly once.
        unsafe {
            map_page_locked(
                virt + page * PAGE_SIZE,
                first + page * PAGE_SIZE,
                WRITABLE | NO_EXECUTE | cache_flags,
            )?;
        }
    }
    Ok(virt + offset)
}

/// Start of the virtual range for general kernel mappings (PML4 slot 416).
const KERNEL_MAP_BASE: u64 = 0xFFFF_D000_0000_0000;
/// Exclusive end of that range (64 GiB).
const KERNEL_MAP_END: u64 = KERNEL_MAP_BASE + (64 << 30);

fn check_kernel_map_address(virt: u64) -> Result<(), MapError> {
    if virt.is_multiple_of(PAGE_SIZE) && (KERNEL_MAP_BASE..KERNEL_MAP_END).contains(&virt) {
        Ok(())
    } else {
        Err(MapError::InvalidRange)
    }
}

/// Returns a pointer to the leaf (level 1) entry for `virt`, or `None` if
/// an intermediate table is missing or a huge page covers the address.
///
/// # Safety
///
/// The caller must hold the `KERNEL_SPACE` lock.
unsafe fn leaf_entry(virt: u64) -> Result<Option<*mut u64>, MapError> {
    let mut table = table_ptr(cpu::read_cr3() & ADDRESS_MASK)?;
    for level in (2..=4u32).rev() {
        let index = ((virt >> (12 + 9 * (level - 1))) & 0x1FF) as usize;
        // SAFETY: `table` is the direct-map alias of a live page table,
        // valid for reads of 512 entries; the caller holds the lock.
        let entry = unsafe { (&raw const (*table)[index]).read_volatile() };
        if entry & PRESENT == 0 || entry & HUGE != 0 {
            return Ok(None);
        }
        table = table_ptr(entry & ADDRESS_MASK)?;
    }
    let index = ((virt >> 12) & 0x1FF) as usize;
    // SAFETY: `index < 512`, so the pointer stays inside the level-1 table.
    Ok(Some(unsafe { (&raw mut (*table)[index]).cast::<u64>() }))
}

/// Maps the page at `virt`, inside the general kernel mapping range, to the
/// frame `phys`. The mapping is non-executable, and writable if `writable`.
///
/// # Safety
///
/// The caller must own the frame at `phys` and must ensure the mapping does
/// not create aliasing that violates Rust's rules for whatever is stored
/// there.
pub unsafe fn map_kernel_page(virt: u64, phys: u64, writable: bool) -> Result<(), MapError> {
    check_kernel_map_address(virt)?;
    if !phys.is_multiple_of(PAGE_SIZE) || phys > ADDRESS_MASK {
        return Err(MapError::InvalidRange);
    }
    let flags = NO_EXECUTE | if writable { WRITABLE } else { 0 };
    let _space = KERNEL_SPACE.lock();
    // SAFETY: the lock is held and `virt` lies in the general kernel mapping
    // range, which the image, stack and direct map do not use.
    unsafe { map_page_locked(virt, phys, flags) }
}

/// Removes the mapping of the page at `virt` and returns the physical
/// address it was mapped to, or `None` if it was not mapped. The stale
/// translation is flushed from this CPU's TLB. Intermediate page tables are
/// kept for reuse.
///
/// # Safety
///
/// Nothing may access the page through `virt` afterwards.
pub unsafe fn unmap_kernel_page(virt: u64) -> Result<Option<u64>, MapError> {
    check_kernel_map_address(virt)?;
    let _space = KERNEL_SPACE.lock();
    // SAFETY: the lock is held.
    let Some(slot) = (unsafe { leaf_entry(virt)? }) else {
        return Ok(None);
    };
    // SAFETY: `slot` points at a live level-1 entry (see `leaf_entry`) and
    // the lock serialises updates. `invlpg` only drops a TLB entry.
    unsafe {
        let entry = slot.read_volatile();
        if entry & PRESENT == 0 {
            return Ok(None);
        }
        slot.write_volatile(0);
        core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
        Ok(Some(entry & ADDRESS_MASK))
    }
}

/// Looks up the physical frame the page at `virt` (in the general kernel
/// mapping range) is mapped to.
pub fn translate_kernel_page(virt: u64) -> Result<Option<u64>, MapError> {
    check_kernel_map_address(virt)?;
    let _space = KERNEL_SPACE.lock();
    // SAFETY: the lock is held; the entry is only read.
    let entry = match unsafe { leaf_entry(virt)? } {
        // SAFETY: `slot` points at a live level-1 entry.
        Some(slot) => unsafe { slot.read_volatile() },
        None => return Ok(None),
    };
    Ok((entry & PRESENT != 0).then_some(entry & ADDRESS_MASK))
}

/// First page of the general kernel mapping range, reserved for
/// [`self_test`] and the `fault=unmapped` fault-injection test.
pub const SCRATCH_PAGE: u64 = KERNEL_MAP_BASE;

/// Maps a fresh frame at [`SCRATCH_PAGE`], checks that the mapping and the
/// direct map show the same memory, unmaps it and checks that the
/// translation is gone.
///
/// With `leave_stale_access` the function finishes by reading through the
/// unmapped address, which must raise a page fault (and would silently
/// succeed if the TLB entry had not been flushed). That mode never returns
/// normally and is used only by fault injection.
pub fn self_test(leave_stale_access: bool) -> Result<(), &'static str> {
    const PATTERN: u64 = 0x0123_4567_89AB_CDEF;
    let frame = frame::alloc_zeroed(0).ok_or("no frame for the mapping test")?;
    let direct = mm::phys_to_virt(frame, PAGE_SIZE)
        .ok_or("test frame outside the direct map")?
        .cast::<u64>();
    let mapped = SCRATCH_PAGE as *mut u64;

    // SAFETY: the frame was just allocated, so this test owns it; it is only
    // accessed through raw pointers with volatile operations, so the two
    // virtual aliases cannot violate reference rules. `SCRATCH_PAGE` is
    // reserved for this purpose.
    unsafe {
        map_kernel_page(SCRATCH_PAGE, frame, true).map_err(|_| "mapping failed")?;
        if map_kernel_page(SCRATCH_PAGE, frame, true)
            != Err(MapError::AlreadyMapped { virt: SCRATCH_PAGE })
        {
            return Err("double mapping was not rejected");
        }
        if translate_kernel_page(SCRATCH_PAGE) != Ok(Some(frame)) {
            return Err("translation does not match the mapping");
        }
        mapped.write_volatile(PATTERN);
        if direct.read_volatile() != PATTERN {
            return Err("mapping does not reach the mapped frame");
        }
        direct.add(1).write_volatile(!PATTERN);
        if mapped.add(1).read_volatile() != !PATTERN {
            return Err("mapping does not show writes made through the direct map");
        }

        if unmap_kernel_page(SCRATCH_PAGE) != Ok(Some(frame)) {
            return Err("unmap did not return the mapped frame");
        }
        if translate_kernel_page(SCRATCH_PAGE) != Ok(None) {
            return Err("page is still mapped after unmap");
        }
        if unmap_kernel_page(SCRATCH_PAGE) != Ok(None) {
            return Err("second unmap did not report an unmapped page");
        }
        frame::free(frame, 0).map_err(|_| "could not free the test frame")?;
    }

    if leave_stale_access {
        super::trigger_page_fault(SCRATCH_PAGE);
    }
    Ok(())
}

/// Start of the part of the general kernel mapping range used for kernel
/// thread stacks.
pub const KERNEL_STACKS_BASE: u64 = KERNEL_MAP_BASE + (1 << 30);
/// Exclusive end of the kernel thread stack area (32 GiB of address space).
pub const KERNEL_STACKS_END: u64 = KERNEL_STACKS_BASE + (32 << 30);
