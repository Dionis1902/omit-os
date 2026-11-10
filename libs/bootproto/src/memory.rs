// SPDX-License-Identifier: GPL-2.0-only
//! Firmware-independent physical memory map.
//!
//! The loader converts the UEFI memory map into this format so the kernel
//! never has to interpret firmware data structures.

use crate::PAGE_SIZE;

/// Classification of a physical memory region.
///
/// A transparent integer rather than an `enum` so that a value written by a
/// mismatched loader can never be an invalid bit pattern.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemoryKind(pub u32);

impl MemoryKind {
    /// Free RAM the kernel may allocate.
    pub const USABLE: Self = Self(1);
    /// RAM used by the firmware's boot services and the loader itself. Holds
    /// nothing the kernel needs once it has consumed the boot information.
    pub const BOOT_RECLAIMABLE: Self = Self(2);
    /// The loaded kernel image.
    pub const KERNEL_IMAGE: Self = Self(3);
    /// The boot stack.
    pub const KERNEL_STACK: Self = Self(4);
    /// Page tables built by the loader.
    pub const PAGE_TABLES: Self = Self(5);
    /// [`crate::BootInfo`], the memory map and the command line.
    pub const BOOT_INFO: Self = Self(6);
    /// ACPI tables; reclaimable after the kernel has parsed them.
    pub const ACPI_RECLAIMABLE: Self = Self(7);
    /// ACPI non-volatile storage; must be preserved.
    pub const ACPI_NVS: Self = Self(8);
    /// UEFI runtime services code and data; must be preserved.
    pub const FIRMWARE_RUNTIME: Self = Self(9);
    /// Memory-mapped I/O.
    pub const MMIO: Self = Self(10);
    /// RAM reported as defective by the firmware.
    pub const UNUSABLE: Self = Self(11);
    /// Anything else; must not be touched.
    pub const RESERVED: Self = Self(12);
    /// Persistent (non-volatile) memory.
    pub const PERSISTENT: Self = Self(13);
    /// The initial ramdisk loaded by the boot loader.
    pub const INITRD: Self = Self(14);

    /// Human-readable name, for diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::USABLE => "usable",
            Self::BOOT_RECLAIMABLE => "boot-reclaimable",
            Self::KERNEL_IMAGE => "kernel-image",
            Self::KERNEL_STACK => "kernel-stack",
            Self::PAGE_TABLES => "page-tables",
            Self::BOOT_INFO => "boot-info",
            Self::ACPI_RECLAIMABLE => "acpi-reclaimable",
            Self::ACPI_NVS => "acpi-nvs",
            Self::FIRMWARE_RUNTIME => "firmware-runtime",
            Self::MMIO => "mmio",
            Self::UNUSABLE => "unusable",
            Self::RESERVED => "reserved",
            Self::PERSISTENT => "persistent",
            Self::INITRD => "initrd",
            _ => "unknown",
        }
    }

    /// Whether the region is backed by RAM (as opposed to MMIO, reserved
    /// address space or persistent memory).
    pub const fn is_ram(self) -> bool {
        !matches!(
            self,
            Self::MMIO | Self::RESERVED | Self::UNUSABLE | Self::PERSISTENT
        ) && self.0 >= Self::USABLE.0
            && self.0 <= Self::INITRD.0
    }
}

impl core::fmt::Debug for MemoryKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}({})", self.name(), self.0)
    }
}

/// A contiguous run of physical pages of one [`MemoryKind`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    /// Physical address of the first byte; a multiple of [`PAGE_SIZE`].
    pub start: u64,
    /// Number of [`PAGE_SIZE`] pages; never zero.
    pub pages: u64,
    /// What the region holds.
    pub kind: MemoryKind,
    /// Reserved, zero.
    pub reserved: u32,
}

impl MemoryRegion {
    /// Creates a region.
    pub const fn new(start: u64, pages: u64, kind: MemoryKind) -> Self {
        Self {
            start,
            pages,
            kind,
            reserved: 0,
        }
    }

    /// Size in bytes, or `None` on overflow.
    pub const fn size(&self) -> Option<u64> {
        self.pages.checked_mul(PAGE_SIZE)
    }

    /// Exclusive end address, or `None` on overflow.
    pub const fn end(&self) -> Option<u64> {
        match self.size() {
            Some(size) => self.start.checked_add(size),
            None => None,
        }
    }
}

/// A defect found by [`validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMapError {
    /// The map has no entries.
    Empty,
    /// Entry `index` has a start address that is not page-aligned.
    Misaligned {
        /// Offending entry.
        index: usize,
    },
    /// Entry `index` covers zero pages.
    ZeroLength {
        /// Offending entry.
        index: usize,
    },
    /// Entry `index` wraps around the end of the address space.
    Overflow {
        /// Offending entry.
        index: usize,
    },
    /// Entry `index` starts before the end of the previous entry.
    Unsorted {
        /// Offending entry.
        index: usize,
    },
    /// The map contains no usable RAM at all.
    NoUsableMemory,
}

impl core::fmt::Display for MemoryMapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => write!(f, "memory map is empty"),
            Self::Misaligned { index } => write!(f, "memory map entry {index} is not page-aligned"),
            Self::ZeroLength { index } => write!(f, "memory map entry {index} is empty"),
            Self::Overflow { index } => {
                write!(f, "memory map entry {index} overflows the address space")
            }
            Self::Unsorted { index } => write!(
                f,
                "memory map entry {index} overlaps or precedes its predecessor"
            ),
            Self::NoUsableMemory => write!(f, "memory map contains no usable RAM"),
        }
    }
}

/// Aggregate figures derived from a validated memory map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemorySummary {
    /// Number of entries.
    pub regions: usize,
    /// Bytes of [`MemoryKind::USABLE`] RAM.
    pub usable_bytes: u64,
    /// Bytes of [`MemoryKind::BOOT_RECLAIMABLE`] RAM.
    pub reclaimable_bytes: u64,
    /// Bytes of RAM of any kind (see [`MemoryKind::is_ram`]).
    pub ram_bytes: u64,
    /// Exclusive end of the highest RAM region.
    pub ram_end: u64,
    /// Size in bytes of the largest single usable region.
    pub largest_usable_bytes: u64,
}

/// Checks the structural invariants of a memory map (page-aligned, non-empty,
/// sorted, non-overlapping entries; at least one usable region) and returns
/// aggregate figures.
pub fn validate(map: &[MemoryRegion]) -> Result<MemorySummary, MemoryMapError> {
    if map.is_empty() {
        return Err(MemoryMapError::Empty);
    }
    let mut summary = MemorySummary {
        regions: map.len(),
        ..MemorySummary::default()
    };
    let mut prev_end = 0u64;
    for (index, region) in map.iter().enumerate() {
        if region.start % PAGE_SIZE != 0 {
            return Err(MemoryMapError::Misaligned { index });
        }
        if region.pages == 0 {
            return Err(MemoryMapError::ZeroLength { index });
        }
        let (Some(size), Some(end)) = (region.size(), region.end()) else {
            return Err(MemoryMapError::Overflow { index });
        };
        if region.start < prev_end {
            return Err(MemoryMapError::Unsorted { index });
        }
        prev_end = end;

        if region.kind.is_ram() {
            summary.ram_bytes += size;
            summary.ram_end = end;
        }
        if region.kind == MemoryKind::USABLE {
            summary.usable_bytes += size;
            summary.largest_usable_bytes = summary.largest_usable_bytes.max(size);
        } else if region.kind == MemoryKind::BOOT_RECLAIMABLE {
            summary.reclaimable_bytes += size;
        }
    }
    if summary.usable_bytes == 0 {
        return Err(MemoryMapError::NoUsableMemory);
    }
    Ok(summary)
}

/// Sorts `regions` by start address and merges adjacent entries of the same
/// kind in place. Returns the number of entries remaining at the front of
/// the slice.
///
/// Used by the loader after converting the firmware map, which is neither
/// guaranteed to be sorted nor coalesced. Entries with zero pages are
/// dropped.
pub fn sort_and_merge(regions: &mut [MemoryRegion]) -> usize {
    regions.sort_unstable_by_key(|r| r.start);
    let mut out = 0usize;
    for i in 0..regions.len() {
        let cur = regions[i];
        if cur.pages == 0 {
            continue;
        }
        if out > 0 {
            let prev = &mut regions[out - 1];
            if prev.kind == cur.kind && prev.end() == Some(cur.start) {
                prev.pages += cur.pages;
                continue;
            }
        }
        regions[out] = cur;
        out += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    const fn r(start: u64, pages: u64, kind: MemoryKind) -> MemoryRegion {
        MemoryRegion::new(start, pages, kind)
    }

    #[test]
    fn summarises_a_valid_map() {
        let map = [
            r(0x0, 0xA0, MemoryKind::USABLE),
            r(0xA_0000, 0x60, MemoryKind::RESERVED),
            r(0x10_0000, 0x100, MemoryKind::BOOT_RECLAIMABLE),
            r(0x20_0000, 0x10, MemoryKind::KERNEL_IMAGE),
            r(0x21_0000, 0x1000, MemoryKind::USABLE),
            r(0xFEC0_0000, 1, MemoryKind::MMIO),
        ];
        let s = validate(&map).unwrap();
        assert_eq!(s.regions, 6);
        assert_eq!(s.usable_bytes, (0xA0 + 0x1000) * PAGE_SIZE);
        assert_eq!(s.reclaimable_bytes, 0x100 * PAGE_SIZE);
        assert_eq!(s.ram_bytes, (0xA0 + 0x100 + 0x10 + 0x1000) * PAGE_SIZE);
        assert_eq!(s.ram_end, 0x21_0000 + 0x1000 * PAGE_SIZE);
        assert_eq!(s.largest_usable_bytes, 0x1000 * PAGE_SIZE);
    }

    #[test]
    fn rejects_structural_defects() {
        assert_eq!(validate(&[]), Err(MemoryMapError::Empty));
        assert_eq!(
            validate(&[r(0x800, 1, MemoryKind::USABLE)]),
            Err(MemoryMapError::Misaligned { index: 0 })
        );
        assert_eq!(
            validate(&[r(0x1000, 0, MemoryKind::USABLE)]),
            Err(MemoryMapError::ZeroLength { index: 0 })
        );
        assert_eq!(
            validate(&[r(0x1000, u64::MAX / 2, MemoryKind::USABLE)]),
            Err(MemoryMapError::Overflow { index: 0 })
        );
        assert_eq!(
            validate(&[
                r(0xFFFF_FFFF_FFFF_F000, 1, MemoryKind::USABLE),
                r(0x1000, 1, MemoryKind::USABLE)
            ]),
            Err(MemoryMapError::Overflow { index: 0 })
        );
        assert_eq!(
            validate(&[
                r(0x1000, 2, MemoryKind::USABLE),
                r(0x2000, 1, MemoryKind::USABLE)
            ]),
            Err(MemoryMapError::Unsorted { index: 1 })
        );
        assert_eq!(
            validate(&[
                r(0x5000, 1, MemoryKind::USABLE),
                r(0x1000, 1, MemoryKind::USABLE)
            ]),
            Err(MemoryMapError::Unsorted { index: 1 })
        );
        assert_eq!(
            validate(&[r(0x1000, 1, MemoryKind::RESERVED)]),
            Err(MemoryMapError::NoUsableMemory)
        );
    }

    #[test]
    fn sort_and_merge_coalesces_only_adjacent_same_kind() {
        let mut v = vec![
            r(0x3000, 1, MemoryKind::USABLE),
            r(0x1000, 1, MemoryKind::USABLE),
            r(0x2000, 1, MemoryKind::USABLE),
            r(0x4000, 0, MemoryKind::RESERVED),
            r(0x4000, 2, MemoryKind::RESERVED),
            r(0x8000, 1, MemoryKind::RESERVED),
            r(0x9000, 1, MemoryKind::USABLE),
        ];
        let n = sort_and_merge(&mut v);
        assert_eq!(
            &v[..n],
            &[
                r(0x1000, 3, MemoryKind::USABLE),
                r(0x4000, 2, MemoryKind::RESERVED),
                r(0x8000, 1, MemoryKind::RESERVED),
                r(0x9000, 1, MemoryKind::USABLE),
            ]
        );
        assert!(validate(&v[..n]).is_ok());
    }

    #[test]
    fn kind_classification() {
        assert!(MemoryKind::USABLE.is_ram());
        assert!(MemoryKind::ACPI_NVS.is_ram());
        assert!(!MemoryKind::MMIO.is_ram());
        assert!(!MemoryKind::RESERVED.is_ram());
        assert!(!MemoryKind(0).is_ram());
        assert!(!MemoryKind(999).is_ram());
        assert_eq!(MemoryKind(999).name(), "unknown");
    }

    #[test]
    fn region_layout_is_stable() {
        assert_eq!(size_of::<MemoryRegion>(), 24);
        assert_eq!(align_of::<MemoryRegion>(), 8);
    }
}
