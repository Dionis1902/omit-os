// SPDX-License-Identifier: GPL-2.0-only
//! Construction of the initial kernel address space.
//!
//! The loader builds a fresh set of 4-level page tables rather than editing
//! the firmware's, so the kernel starts from a fully known state:
//!
//! * the kernel image and boot stack, mapped with 4 KiB pages and exact
//!   permissions;
//! * a direct map of physical memory in the higher half, using the largest
//!   page size the CPU supports;
//! * the same physical memory identity-mapped in the lower half, sharing the
//!   direct map's lower-level tables. The loader needs it to survive the CR3
//!   switch; the kernel removes it.

use omit_bootproto::memory::MemoryKind;
use omit_bootproto::{PAGE_SIZE, PHYS_MAP_BASE, PHYS_MAP_MAX_SIZE};

use crate::{Error, alloc_zeroed_pages};

/// Entry is valid.
pub const PRESENT: u64 = 1 << 0;
/// Writes are allowed.
pub const WRITABLE: u64 = 1 << 1;
/// Entry maps a 2 MiB / 1 GiB page instead of pointing to a table.
const HUGE: u64 = 1 << 7;
/// Instruction fetches are forbidden (requires `EFER.NXE`).
pub const NO_EXECUTE: u64 = 1 << 63;

const ENTRIES: u64 = 512;
const ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const GIB: u64 = 1 << 30;
const MIB_2: u64 = 2 << 20;

/// A set of page tables under construction.
///
/// While boot services are active the firmware identity-maps all memory, so
/// the physical addresses of the tables double as pointers.
pub struct AddressSpace {
    pml4: u64,
}

/// How the kernel is told about the pages that hold tables.
const TABLES: Option<MemoryKind> = Some(MemoryKind::PAGE_TABLES);

impl AddressSpace {
    /// Allocates an empty top-level table.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            pml4: alloc_zeroed_pages(TABLES, 1)?,
        })
    }

    /// Physical address of the top-level table, suitable for CR3.
    pub fn root(&self) -> u64 {
        self.pml4
    }

    fn entry(table: u64, index: u64) -> *mut u64 {
        debug_assert!(index < ENTRIES && table.is_multiple_of(PAGE_SIZE));
        (table + index * 8) as *mut u64
    }

    fn read(table: u64, index: u64) -> u64 {
        // SAFETY: `table` is a page-table page allocated by
        // `alloc_zeroed_pages` (4096 bytes, page-aligned, initialised) and
        // `index < 512`, so the entry is in bounds. The firmware's identity
        // mapping makes the physical address a valid pointer.
        unsafe { Self::entry(table, index).read() }
    }

    fn write(table: u64, index: u64, value: u64) {
        // SAFETY: as in `read`; the loader is single-threaded and holds no
        // references into table pages.
        unsafe { Self::entry(table, index).write(value) }
    }

    /// Returns the table referenced by `table[index]`, allocating it if the
    /// entry is empty.
    fn next_table(&self, table: u64, index: u64) -> Result<u64, Error> {
        let entry = Self::read(table, index);
        if entry & PRESENT != 0 {
            if entry & HUGE != 0 {
                return Err(Error::new("page table conflict: huge page in the way"));
            }
            return Ok(entry & ADDRESS_MASK);
        }
        let next = alloc_zeroed_pages(TABLES, 1)?;
        // Intermediate entries are maximally permissive; the leaf decides.
        Self::write(table, index, next | PRESENT | WRITABLE);
        Ok(next)
    }

    /// Maps the 4 KiB page at `virt` to the frame at `phys` with leaf
    /// `flags`. Fails if `virt` is already mapped.
    pub fn map_4k(&mut self, virt: u64, phys: u64, flags: u64) -> Result<(), Error> {
        if !virt.is_multiple_of(PAGE_SIZE) || !phys.is_multiple_of(PAGE_SIZE) {
            return Err(Error::new("unaligned page mapping"));
        }
        let pdpt = self.next_table(self.pml4, (virt >> 39) & 0x1FF)?;
        let pd = self.next_table(pdpt, (virt >> 30) & 0x1FF)?;
        let pt = self.next_table(pd, (virt >> 21) & 0x1FF)?;
        let index = (virt >> 12) & 0x1FF;
        if Self::read(pt, index) & PRESENT != 0 {
            return Err(Error::new("page mapped twice"));
        }
        Self::write(pt, index, phys | flags | PRESENT);
        Ok(())
    }

    /// Maps `count` consecutive 4 KiB pages.
    pub fn map_range(&mut self, virt: u64, phys: u64, count: u64, flags: u64) -> Result<(), Error> {
        for page in 0..count {
            self.map_4k(virt + page * PAGE_SIZE, phys + page * PAGE_SIZE, flags)?;
        }
        Ok(())
    }

    /// Maps physical memory `[0, size)` at [`PHYS_MAP_BASE`] (writable,
    /// non-executable) and at address 0 (writable, executable — the loader
    /// runs from there until it jumps to the kernel).
    ///
    /// `size` must be a multiple of 1 GiB and at most [`PHYS_MAP_MAX_SIZE`].
    /// 1 GiB pages are used when `use_1g_pages` is set, 2 MiB pages
    /// otherwise.
    pub fn map_physical(&mut self, size: u64, use_1g_pages: bool) -> Result<(), Error> {
        if size == 0 || !size.is_multiple_of(GIB) || size > PHYS_MAP_MAX_SIZE {
            return Err(Error::new("unsupported direct map size"));
        }
        let direct_slot = (PHYS_MAP_BASE >> 39) & 0x1FF;
        if Self::read(self.pml4, 0) & PRESENT != 0
            || Self::read(self.pml4, direct_slot) & PRESENT != 0
        {
            return Err(Error::new("direct map slot already in use"));
        }

        let pdpt = alloc_zeroed_pages(TABLES, 1)?;
        for gib in 0..size / GIB {
            if use_1g_pages {
                Self::write(pdpt, gib, (gib * GIB) | PRESENT | WRITABLE | HUGE);
            } else {
                let pd = alloc_zeroed_pages(TABLES, 1)?;
                for i in 0..ENTRIES {
                    let phys = gib * GIB + i * MIB_2;
                    Self::write(pd, i, phys | PRESENT | WRITABLE | HUGE);
                }
                Self::write(pdpt, gib, pd | PRESENT | WRITABLE);
            }
        }

        // Both halves share the PDPT. NX on the top-level entry makes the
        // whole direct map non-executable regardless of the lower levels.
        Self::write(self.pml4, 0, pdpt | PRESENT | WRITABLE);
        Self::write(
            self.pml4,
            direct_slot,
            pdpt | PRESENT | WRITABLE | NO_EXECUTE,
        );
        Ok(())
    }
}
