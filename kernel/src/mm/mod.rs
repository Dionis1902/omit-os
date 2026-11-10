// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: GPL-2.0-only
//! Kernel memory management.

use core::sync::atomic::{AtomicU64, Ordering};

use omit_bootproto::{BootInfo, PHYS_MAP_BASE};

pub mod frame;
pub mod heap;
pub mod kstack;

/// Size of a page frame in bytes.
pub const PAGE_SIZE: u64 = omit_bootproto::PAGE_SIZE;

/// Number of bytes of physical address space covered by the direct map;
/// zero until [`init_direct_map`] has run.
static PHYS_MAP_SIZE: AtomicU64 = AtomicU64::new(0);

/// Records the extent of the loader-provided direct map so that
/// [`phys_to_virt`] can be used.
///
/// `boot_info` must have passed `BootInfo::validate`.
pub fn init_direct_map(boot_info: &BootInfo) {
    PHYS_MAP_SIZE.store(boot_info.phys_map_size, Ordering::Release);
}

/// Translates a physical address range `[phys, phys + len)` into a pointer
/// into the direct map, or `None` if any part of it is not covered.
///
/// Obtaining the pointer is safe; dereferencing it is subject to the usual
/// raw-pointer rules (the caller must own the physical memory).
pub fn phys_to_virt(phys: u64, len: u64) -> Option<*mut u8> {
    let end = phys.checked_add(len)?;
    (end <= PHYS_MAP_SIZE.load(Ordering::Acquire)).then(|| (PHYS_MAP_BASE + phys) as *mut u8)
}
