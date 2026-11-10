// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Per-process address spaces.
//!
//! Every process has its own top-level page table. The upper half (kernel)
//! entries are copied from the kernel's table when the address space is
//! created and point at the same lower-level tables, so kernel mappings
//! are identical everywhere; the lower half is private to the process.
//!
//! That sharing relies on the kernel never adding a *new* top-level entry
//! after the first address space exists. It does not: the four kernel
//! regions (direct map, device memory, kernel mappings, kernel image) each
//! have their top-level entry populated during boot.

use core::sync::atomic::{AtomicU64, Ordering};

use omit_abi::{USER_MAX, USER_MIN};

use super::cpu;
use super::paging::MapError;
use crate::mm::{self, PAGE_SIZE, frame};

const ENTRIES: usize = 512;
const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const HUGE: u64 = 1 << 7;
/// Available to software: the frame behind this leaf entry belongs to a
/// shared memory segment, not to the address space.
const SHARED: u64 = 1 << 9;
const NO_EXECUTE: u64 = 1 << 63;
const ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Physical address of the kernel's own top-level table (the one built by
/// the loader); 0 until [`init`].
static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);

/// Records the active page table as the kernel's. Must be called once,
/// from the boot context, before any process exists.
pub fn init() {
    KERNEL_ROOT.store(cpu::read_cr3() & ADDRESS_MASK, Ordering::Release);
}

/// Physical address of the kernel's top-level page table.
pub fn kernel_root() -> u64 {
    KERNEL_ROOT.load(Ordering::Acquire)
}

fn table(phys: u64) -> Option<*mut [u64; ENTRIES]> {
    mm::phys_to_virt(phys, PAGE_SIZE).map(|ptr| ptr.cast())
}

fn indices(virt: u64) -> [usize; 4] {
    [39, 30, 21, 12].map(|shift| ((virt >> shift) & 0x1FF) as usize)
}

/// Whether `[addr, addr + len)` lies in the range user programs may use.
pub fn is_user_range(addr: u64, len: u64) -> bool {
    addr >= USER_MIN && addr.checked_add(len).is_some_and(|end| end <= USER_MAX)
}

/// The user half of a process's virtual memory. Dropping it frees every
/// frame mapped in it and all of its page tables.
#[derive(Debug)]
pub struct AddressSpace {
    root: u64,
}

impl AddressSpace {
    /// Creates an address space with the kernel mapped and nothing else.
    pub fn new() -> Result<Self, MapError> {
        let root = frame::alloc_zeroed(0).ok_or(MapError::OutOfMemory)?;
        let space = Self { root };
        let (Some(new), Some(kernel)) = (table(root), table(kernel_root())) else {
            return Err(MapError::InvalidRange);
        };
        // SAFETY: both pointers are direct-map aliases of whole page-table
        // frames: `new` was just allocated and is exclusively ours;
        // `kernel` is the live kernel root, of which only the upper-half
        // entries are read, and those never change after boot (see the
        // module documentation).
        unsafe {
            for index in ENTRIES / 2..ENTRIES {
                (*new)[index] = (&raw const (*kernel)[index]).read_volatile();
            }
        }
        Ok(space)
    }

    /// Physical address of the top-level table, for loading into CR3.
    pub fn root(&self) -> u64 {
        self.root
    }

    /// Maps the user page at `virt` to the frame `phys`.
    ///
    /// The address space takes ownership of the frame: it is freed when the
    /// address space is dropped. On error the caller keeps ownership.
    pub fn map(
        &mut self,
        virt: u64,
        phys: u64,
        writable: bool,
        executable: bool,
    ) -> Result<(), MapError> {
        self.map_with(virt, phys, writable, executable, 0)
    }

    /// Maps the user page at `virt`, writable, to a frame of a shared
    /// memory segment. The address space does not take ownership: the
    /// frame is not freed when the page is unmapped or the address space
    /// dropped. The caller keeps the segment alive for as long as the
    /// mapping exists.
    pub fn map_shared(&mut self, virt: u64, phys: u64) -> Result<(), MapError> {
        self.map_with(virt, phys, true, false, SHARED)
    }

    fn map_with(
        &mut self,
        virt: u64,
        phys: u64,
        writable: bool,
        executable: bool,
        extra: u64,
    ) -> Result<(), MapError> {
        if !virt.is_multiple_of(PAGE_SIZE)
            || !phys.is_multiple_of(PAGE_SIZE)
            || !is_user_range(virt, PAGE_SIZE)
            || (writable && executable)
        {
            return Err(MapError::InvalidRange);
        }
        let path = indices(virt);
        let mut current = self.root;
        for &index in &path[..3] {
            let entries = table(current).ok_or(MapError::InvalidRange)?;
            // SAFETY: `entries` aliases a page-table frame owned by this
            // address space (`&mut self` gives exclusive access); `index`
            // is below 512.
            let entry = unsafe { (*entries)[index] };
            current = if entry & PRESENT == 0 {
                let next = frame::alloc_zeroed(0).ok_or(MapError::OutOfMemory)?;
                // Intermediate entries are maximally permissive (including
                // user access); the leaf decides. SAFETY: as above.
                unsafe { (*entries)[index] = next | PRESENT | WRITABLE | USER };
                next
            } else {
                entry & ADDRESS_MASK
            };
        }
        let entries = table(current).ok_or(MapError::InvalidRange)?;
        let mut flags = PRESENT | USER | extra;
        if writable {
            flags |= WRITABLE;
        }
        if !executable {
            flags |= NO_EXECUTE;
        }
        // SAFETY: as above; this is the level-1 table for `virt`.
        unsafe {
            let slot = &raw mut (*entries)[path[3]];
            if slot.read() & PRESENT != 0 {
                return Err(MapError::AlreadyMapped { virt });
            }
            slot.write(phys | flags);
        }
        Ok(())
    }

    /// Removes the mapping of the user page at `virt` and returns the frame
    /// it was mapped to, which the caller now owns — unless the page was
    /// mapped with [`AddressSpace::map_shared`]: then `None` is returned,
    /// like for a page that was not mapped.
    pub fn unmap(&mut self, virt: u64) -> Option<u64> {
        if !virt.is_multiple_of(PAGE_SIZE) || !is_user_range(virt, PAGE_SIZE) {
            return None;
        }
        let path = indices(virt);
        let mut current = self.root;
        for &index in &path[..3] {
            // SAFETY: `current` is a page-table frame owned by this address
            // space; the entry is only read.
            let entry = unsafe { (*table(current)?)[index] };
            if entry & PRESENT == 0 || entry & HUGE != 0 {
                return None;
            }
            current = entry & ADDRESS_MASK;
        }
        let entries = table(current)?;
        // SAFETY: `entries` is the level-1 table for `virt`, owned by this
        // address space, to which `&mut self` gives exclusive access.
        // `invlpg` only drops a TLB entry; it is needed because this may be
        // the active address space.
        unsafe {
            let slot = &raw mut (*entries)[path[3]];
            let entry = slot.read();
            if entry & PRESENT == 0 {
                return None;
            }
            slot.write(0);
            core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
            (entry & SHARED == 0).then_some(entry & ADDRESS_MASK)
        }
    }

    /// Changes the access rights of the mapped user page at `virt`.
    /// Returns `false` if the page is not mapped or the combination is
    /// writable and executable.
    pub fn protect(&mut self, virt: u64, writable: bool, executable: bool) -> bool {
        if (writable && executable)
            || !virt.is_multiple_of(PAGE_SIZE)
            || !is_user_range(virt, PAGE_SIZE)
        {
            return false;
        }
        let path = indices(virt);
        let mut current = self.root;
        for &index in &path[..3] {
            let Some(entries) = table(current) else {
                return false;
            };
            // SAFETY: `current` is a page-table frame owned by this address
            // space; the entry is only read.
            let entry = unsafe { (*entries)[index] };
            if entry & PRESENT == 0 || entry & HUGE != 0 {
                return false;
            }
            current = entry & ADDRESS_MASK;
        }
        let Some(entries) = table(current) else {
            return false;
        };
        // SAFETY: `entries` is the level-1 table for `virt`, owned by this
        // address space (`&mut self`). The frame address is kept and only
        // the permission bits change; `invlpg` drops the stale translation
        // in case this address space is active.
        unsafe {
            let slot = &raw mut (*entries)[path[3]];
            let entry = slot.read();
            if entry & PRESENT == 0 {
                return false;
            }
            let mut flags = PRESENT | USER;
            if writable {
                flags |= WRITABLE;
            }
            if !executable {
                flags |= NO_EXECUTE;
            }
            slot.write((entry & ADDRESS_MASK) | flags);
            core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));
        }
        true
    }

    /// Looks up the user page containing `virt`. Returns the frame it is
    /// mapped to and whether user code may write to it.
    pub fn translate(&self, virt: u64) -> Option<(u64, bool)> {
        if !is_user_range(virt, 1) {
            return None;
        }
        let path = indices(virt);
        let mut current = self.root;
        let mut writable = true;
        for (level, &index) in path.iter().enumerate() {
            // SAFETY: `current` is a page-table frame reachable from this
            // address space's root; the entry is only read.
            let entry = unsafe { (&raw const (*table(current)?)[index]).read_volatile() };
            if entry & PRESENT == 0 || entry & USER == 0 || entry & HUGE != 0 {
                return None;
            }
            writable &= entry & WRITABLE != 0;
            if level == 3 {
                return Some((entry & ADDRESS_MASK, writable));
            }
            current = entry & ADDRESS_MASK;
        }
        None
    }

    /// Copies `buffer.len()` bytes from user address `addr` into `buffer`.
    /// Returns `false`, having copied nothing useful, if any part of the
    /// range is not mapped for user access.
    ///
    /// The copy goes through the direct map, never through the user
    /// addresses themselves, so it cannot fault and needs no SMAP override.
    pub fn read(&self, addr: u64, buffer: &mut [u8]) -> bool {
        self.for_each_chunk(addr, buffer.len(), false, |phys, offset, len| {
            let Some(source) = mm::phys_to_virt(phys, len as u64) else {
                return false;
            };
            // SAFETY: `source` aliases `len` bytes inside a frame mapped in
            // this address space, which stays mapped while `&self` is
            // borrowed; the destination range is inside `buffer`.
            unsafe {
                core::ptr::copy_nonoverlapping(source, buffer[offset..].as_mut_ptr(), len);
            }
            true
        })
    }

    /// Copies `data` to user address `addr`. Returns `false` if any part of
    /// the range is not mapped writable for user code (in which case an
    /// earlier part may already have been written).
    pub fn write(&mut self, addr: u64, data: &[u8]) -> bool {
        self.for_each_chunk(addr, data.len(), true, |phys, offset, len| {
            let Some(destination) = mm::phys_to_virt(phys, len as u64) else {
                return false;
            };
            // SAFETY: `destination` aliases `len` bytes inside a frame that
            // is mapped user-writable in this address space, to which
            // `&mut self` gives exclusive access; the source range is
            // inside `data`.
            unsafe {
                core::ptr::copy_nonoverlapping(data[offset..].as_ptr(), destination, len);
            }
            true
        })
    }

    /// Splits `[addr, addr + len)` at page boundaries and calls `visit`
    /// with the physical address, the offset into the range and the length
    /// of each piece. Stops and returns `false` at the first unmapped (or,
    /// with `need_write`, read-only) page.
    fn for_each_chunk(
        &self,
        addr: u64,
        len: usize,
        need_write: bool,
        mut visit: impl FnMut(u64, usize, usize) -> bool,
    ) -> bool {
        if !is_user_range(addr, len as u64) {
            return false;
        }
        let mut done = 0;
        while done < len {
            let virt = addr + done as u64;
            let in_page = (virt % PAGE_SIZE) as usize;
            let chunk = (PAGE_SIZE as usize - in_page).min(len - done);
            let Some((phys, writable)) = self.translate(virt) else {
                return false;
            };
            if (need_write && !writable) || !visit(phys + in_page as u64, done, chunk) {
                return false;
            }
            done += chunk;
        }
        true
    }
}

/// Frees the frame at `phys`, treated as a page table of `level` (4 = top),
/// and everything below it in the user half.
///
/// # Safety
///
/// The table must belong to an address space that is not active on any CPU
/// and is being destroyed.
unsafe fn free_table(phys: u64, level: u8) {
    let Some(entries) = table(phys) else {
        return;
    };
    // Only the lower half of the top-level table is private.
    let count = if level == 4 { ENTRIES / 2 } else { ENTRIES };
    for index in 0..count {
        // SAFETY: the table is exclusively owned by the address space being
        // destroyed (function contract).
        let entry = unsafe { (*entries)[index] };
        if entry & PRESENT == 0 {
            continue;
        }
        let target = entry & ADDRESS_MASK;
        if level > 1 {
            // SAFETY: a present entry above level 1 points at a table owned
            // by the same address space (huge pages are never created).
            unsafe { free_table(target, level - 1) };
        } else if entry & SHARED == 0 {
            // SAFETY: leaf frames were handed to the address space by
            // `map` and are referenced by nothing else. (Frames of
            // shared segments belong to the segment.)
            let _ = unsafe { frame::free(target, 0) };
        }
    }
    // SAFETY: the table itself is no longer referenced.
    let _ = unsafe { frame::free(phys, 0) };
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        debug_assert_ne!(
            cpu::read_cr3() & ADDRESS_MASK,
            self.root,
            "dropping the active address space"
        );
        // SAFETY: the address space is going away, and the scheduler only
        // drops one after switching to a different page table.
        unsafe { free_table(self.root, 4) };
    }
}
