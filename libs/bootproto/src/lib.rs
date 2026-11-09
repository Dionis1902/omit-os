// SPDX-License-Identifier: GPL-2.0-only
//! The Omit OS boot protocol.
//!
//! This crate is the single source of truth for everything the UEFI loader
//! (`omit-boot`) and the kernel (`omit-kernel`) must agree on: the layout of
//! the kernel address space at hand-off, the [`BootInfo`] structure, the
//! memory map format and the kernel command line syntax.
//!
//! It contains no `unsafe` code except the two accessors that turn addresses
//! stored in [`BootInfo`] back into references, and it is unit-tested on the
//! host.
//!
//! # Hand-off contract
//!
//! When the loader jumps to the kernel entry point:
//!
//! * the CPU is in 64-bit long mode with 4-level paging, interrupts disabled;
//! * `EFER.NXE` is set;
//! * `rdi` holds the direct-map virtual address of a [`BootInfo`];
//! * `rsp` is [`KERNEL_STACK_TOP`]` - 8`, on a stack of [`KERNEL_STACK_SIZE`]
//!   bytes with an unmapped guard page below it;
//! * physical memory `[0, phys_map_size)` is mapped at [`PHYS_MAP_BASE`]
//!   (writable, non-executable) and, temporarily, also identity-mapped in the
//!   lower half (the kernel removes that mapping during initialisation);
//! * the kernel image is mapped at its link address with per-segment
//!   permissions (no page is both writable and executable).

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(test)]
extern crate std;

pub mod cmdline;
pub mod memory;

pub use cmdline::Cmdline;
pub use memory::{MemoryKind, MemoryMapError, MemoryRegion, MemorySummary};

/// Size of a base page on x86-64.
pub const PAGE_SIZE: u64 = 4096;

/// Value of [`BootInfo::magic`]: ASCII `"OMITBOOT"`.
pub const BOOT_INFO_MAGIC: u64 = u64::from_be_bytes(*b"OMITBOOT");

/// Version of the protocol described by this crate. Bumped on any
/// incompatible change to [`BootInfo`] or the hand-off contract.
pub const BOOT_PROTOCOL_VERSION: u32 = 1;

/// Start of the higher half (first canonical kernel address); all physical
/// memory is mapped here ("direct map").
pub const PHYS_MAP_BASE: u64 = 0xFFFF_8000_0000_0000;

/// Largest direct map the loader will build (one PML4 slot).
pub const PHYS_MAP_MAX_SIZE: u64 = 512 << 30;

/// Lowest address the kernel image may be linked at (top 2 GiB, as required
/// by the `kernel` code model). Must match `kernel/linker.ld`.
pub const KERNEL_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// Size of the boot stack handed to the kernel.
pub const KERNEL_STACK_SIZE: u64 = 64 * 1024;

/// Exclusive upper bound of the boot stack. The page below the stack and the
/// page at `KERNEL_STACK_TOP` itself are left unmapped as guard pages.
pub const KERNEL_STACK_TOP: u64 = KERNEL_BASE - PAGE_SIZE;

/// Inclusive lower bound of the boot stack.
pub const KERNEL_STACK_BOTTOM: u64 = KERNEL_STACK_TOP - KERNEL_STACK_SIZE;

// The boot stack must be ABI-aligned, made of whole pages, and sit between the
// largest possible direct map and the kernel image.
const _: () = {
    assert!(KERNEL_STACK_TOP.is_multiple_of(16));
    assert!(KERNEL_STACK_SIZE.is_multiple_of(PAGE_SIZE));
    assert!(KERNEL_STACK_BOTTOM > PHYS_MAP_BASE + PHYS_MAP_MAX_SIZE);
    assert!(KERNEL_STACK_TOP < KERNEL_BASE);
};

/// Maximum length in bytes of the kernel command line.
pub const CMDLINE_MAX_LEN: usize = 1024;

/// Path of the kernel image on the EFI system partition.
pub const KERNEL_PATH: &str = "\\EFI\\OMIT\\KERNEL.ELF";

/// Path of the optional kernel command line file on the EFI system partition.
pub const CMDLINE_PATH: &str = "\\EFI\\OMIT\\CMDLINE";

/// Information passed from the loader to the kernel.
///
/// All `*_addr` fields are virtual addresses inside the direct map unless
/// their name ends in `_phys`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootInfo {
    /// Must equal [`BOOT_INFO_MAGIC`].
    pub magic: u64,
    /// Must equal [`BOOT_PROTOCOL_VERSION`].
    pub version: u32,
    /// Reserved, zero.
    pub reserved: u32,
    /// Virtual base of the direct map (always [`PHYS_MAP_BASE`]).
    pub phys_map_base: u64,
    /// Number of bytes of physical address space covered by the direct map.
    pub phys_map_size: u64,
    /// Address of an array of [`MemoryRegion`], sorted by start address,
    /// non-overlapping.
    pub memory_map_addr: u64,
    /// Number of entries in the memory map array.
    pub memory_map_len: u64,
    /// Physical address the kernel image was loaded at.
    pub kernel_phys_base: u64,
    /// Virtual address the kernel image is mapped at.
    pub kernel_virt_base: u64,
    /// Size in bytes of the loaded kernel image (page-aligned).
    pub kernel_size: u64,
    /// Physical address of the ACPI RSDP, or 0 if the firmware provided none.
    pub rsdp_phys: u64,
    /// Address of the kernel command line bytes (not NUL-terminated).
    pub cmdline_addr: u64,
    /// Length of the kernel command line in bytes
    /// (at most [`CMDLINE_MAX_LEN`]).
    pub cmdline_len: u64,
}

/// Reasons a [`BootInfo`] is rejected by [`BootInfo::validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootInfoError {
    /// [`BootInfo::magic`] is wrong: the kernel was not started by a
    /// conforming loader.
    BadMagic(u64),
    /// Loader and kernel were built against different protocol versions.
    VersionMismatch {
        /// Version the loader reported.
        found: u32,
    },
    /// A field holds a value that violates the hand-off contract.
    BadField(&'static str),
}

impl core::fmt::Display for BootInfoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadMagic(m) => write!(f, "bad boot info magic {m:#018x}"),
            Self::VersionMismatch { found } => write!(
                f,
                "boot protocol version mismatch: loader={found}, kernel={BOOT_PROTOCOL_VERSION}"
            ),
            Self::BadField(name) => write!(f, "invalid boot info field `{name}`"),
        }
    }
}

impl BootInfo {
    /// Checks the self-describing fields and that every address handed over
    /// lies inside the direct map, so the accessors below cannot be pointed
    /// outside of it.
    pub fn validate(&self) -> Result<(), BootInfoError> {
        if self.magic != BOOT_INFO_MAGIC {
            return Err(BootInfoError::BadMagic(self.magic));
        }
        if self.version != BOOT_PROTOCOL_VERSION {
            return Err(BootInfoError::VersionMismatch {
                found: self.version,
            });
        }
        if self.phys_map_base != PHYS_MAP_BASE {
            return Err(BootInfoError::BadField("phys_map_base"));
        }
        if self.phys_map_size == 0
            || self.phys_map_size > PHYS_MAP_MAX_SIZE
            || !self.phys_map_size.is_multiple_of(PAGE_SIZE)
        {
            return Err(BootInfoError::BadField("phys_map_size"));
        }
        let map_bytes = self
            .memory_map_len
            .checked_mul(size_of::<MemoryRegion>() as u64)
            .ok_or(BootInfoError::BadField("memory_map_len"))?;
        if self.memory_map_len == 0 {
            return Err(BootInfoError::BadField("memory_map_len"));
        }
        if !self.in_phys_map(self.memory_map_addr, map_bytes)
            || !self
                .memory_map_addr
                .is_multiple_of(align_of::<MemoryRegion>() as u64)
        {
            return Err(BootInfoError::BadField("memory_map_addr"));
        }
        if self.cmdline_len > CMDLINE_MAX_LEN as u64 {
            return Err(BootInfoError::BadField("cmdline_len"));
        }
        if self.cmdline_len != 0 && !self.in_phys_map(self.cmdline_addr, self.cmdline_len) {
            return Err(BootInfoError::BadField("cmdline_addr"));
        }
        if self.kernel_virt_base < KERNEL_BASE
            || self.kernel_size == 0
            || !self.kernel_size.is_multiple_of(PAGE_SIZE)
            || !self.kernel_phys_base.is_multiple_of(PAGE_SIZE)
        {
            return Err(BootInfoError::BadField("kernel_*"));
        }
        if self.rsdp_phys >= self.phys_map_size {
            return Err(BootInfoError::BadField("rsdp_phys"));
        }
        Ok(())
    }

    /// Whether `[addr, addr + len)` lies entirely inside the direct map.
    fn in_phys_map(&self, addr: u64, len: u64) -> bool {
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        let Some(map_end) = self.phys_map_base.checked_add(self.phys_map_size) else {
            return false;
        };
        addr >= self.phys_map_base && end <= map_end
    }

    /// Translates a physical address into its direct-map virtual address, or
    /// `None` if it is not covered by the direct map.
    pub fn phys_to_virt(&self, phys: u64) -> Option<u64> {
        (phys < self.phys_map_size).then(|| self.phys_map_base + phys)
    }

    /// Returns the memory map.
    ///
    /// # Safety
    ///
    /// [`BootInfo::validate`] must have succeeded, the direct map described
    /// by this structure must be active, and the loader-provided array must
    /// not have been overwritten.
    pub unsafe fn memory_map(&self) -> &[MemoryRegion] {
        // SAFETY: per the function contract the array lies in mapped,
        // initialised, suitably aligned memory that outlives `self`.
        unsafe {
            core::slice::from_raw_parts(
                self.memory_map_addr as *const MemoryRegion,
                self.memory_map_len as usize,
            )
        }
    }

    /// Returns the raw kernel command line bytes.
    ///
    /// # Safety
    ///
    /// Same contract as [`BootInfo::memory_map`].
    pub unsafe fn cmdline_bytes(&self) -> &[u8] {
        if self.cmdline_len == 0 {
            return &[];
        }
        // SAFETY: per the function contract the bytes lie in mapped,
        // initialised memory that outlives `self`; `u8` has no alignment or
        // validity requirements.
        unsafe {
            core::slice::from_raw_parts(self.cmdline_addr as *const u8, self.cmdline_len as usize)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> BootInfo {
        BootInfo {
            magic: BOOT_INFO_MAGIC,
            version: BOOT_PROTOCOL_VERSION,
            reserved: 0,
            phys_map_base: PHYS_MAP_BASE,
            phys_map_size: 4 << 30,
            memory_map_addr: PHYS_MAP_BASE + 0x10_0000,
            memory_map_len: 8,
            kernel_phys_base: 0x20_0000,
            kernel_virt_base: KERNEL_BASE,
            kernel_size: 0x4000,
            rsdp_phys: 0xE_0000,
            cmdline_addr: PHYS_MAP_BASE + 0x11_0000,
            cmdline_len: 10,
        }
    }

    #[test]
    fn magic_spells_omitboot() {
        assert_eq!(&BOOT_INFO_MAGIC.to_be_bytes(), b"OMITBOOT");
    }

    #[test]
    fn accepts_valid_boot_info() {
        assert_eq!(valid().validate(), Ok(()));
        assert_eq!(valid().phys_to_virt(0x1000), Some(PHYS_MAP_BASE + 0x1000));
        assert_eq!(valid().phys_to_virt(4 << 30), None);
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        let mut bi = valid();
        bi.magic = 0;
        assert_eq!(bi.validate(), Err(BootInfoError::BadMagic(0)));
        let mut bi = valid();
        bi.version += 1;
        assert!(matches!(
            bi.validate(),
            Err(BootInfoError::VersionMismatch { .. })
        ));
    }

    #[test]
    fn rejects_pointers_outside_direct_map() {
        let mut bi = valid();
        bi.memory_map_addr = 0x1000;
        assert_eq!(
            bi.validate(),
            Err(BootInfoError::BadField("memory_map_addr"))
        );

        let mut bi = valid();
        bi.memory_map_addr = PHYS_MAP_BASE + bi.phys_map_size - 8;
        assert_eq!(
            bi.validate(),
            Err(BootInfoError::BadField("memory_map_addr"))
        );

        let mut bi = valid();
        bi.memory_map_len = u64::MAX;
        assert_eq!(
            bi.validate(),
            Err(BootInfoError::BadField("memory_map_len"))
        );

        let mut bi = valid();
        bi.cmdline_addr = u64::MAX - 2;
        assert_eq!(bi.validate(), Err(BootInfoError::BadField("cmdline_addr")));

        let mut bi = valid();
        bi.cmdline_len = CMDLINE_MAX_LEN as u64 + 1;
        assert_eq!(bi.validate(), Err(BootInfoError::BadField("cmdline_len")));
    }

    #[test]
    fn rejects_bad_kernel_and_phys_map_fields() {
        let mut bi = valid();
        bi.kernel_virt_base = 0x40_0000;
        assert_eq!(bi.validate(), Err(BootInfoError::BadField("kernel_*")));

        let mut bi = valid();
        bi.phys_map_size = PHYS_MAP_MAX_SIZE + PAGE_SIZE;
        assert_eq!(bi.validate(), Err(BootInfoError::BadField("phys_map_size")));

        let mut bi = valid();
        bi.rsdp_phys = bi.phys_map_size;
        assert_eq!(bi.validate(), Err(BootInfoError::BadField("rsdp_phys")));
    }

    #[test]
    fn empty_cmdline_needs_no_valid_address() {
        let mut bi = valid();
        bi.cmdline_addr = 0;
        bi.cmdline_len = 0;
        assert_eq!(bi.validate(), Ok(()));
        // SAFETY: with a zero length the accessor never dereferences.
        assert!(unsafe { bi.cmdline_bytes() }.is_empty());
    }
}
