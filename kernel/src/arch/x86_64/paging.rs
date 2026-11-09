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
