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
                WRITABLE | NO_EXECUTE | CACHE_DISABLE,
            )?;
        }
    }
    Ok(virt + offset)
}
