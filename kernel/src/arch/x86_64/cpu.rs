// SPDX-License-Identifier: GPL-2.0-only
//! CPU identification and control-register setup.

use core::arch::asm;
use core::arch::x86_64::__cpuid_count;
use core::fmt;

const CR0_WP: u64 = 1 << 16;
const CR4_UMIP: u64 = 1 << 11;
const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;
const CR4_LA57: u64 = 1 << 12;
const MSR_EFER: u32 = 0xC000_0080;
const EFER_NXE: u64 = 1 << 11;

/// Reasons the CPU cannot run the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuError {
    /// The CPU has no no-execute page protection; W^X cannot be enforced.
    NoExecuteUnsupported,
    /// A control bit the kernel wrote did not read back as set.
    ControlBitNotSticky(&'static str),
    /// The firmware left 5-level paging enabled, which the kernel does not
    /// support yet.
    FiveLevelPaging,
}

impl fmt::Display for CpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoExecuteUnsupported => write!(f, "CPU lacks NX page protection"),
            Self::ControlBitNotSticky(bit) => write!(f, "could not enable {bit}"),
            Self::FiveLevelPaging => write!(f, "5-level paging is active but unsupported"),
        }
    }
}

/// What [`init`] found and enabled.
#[derive(Debug, Clone, Copy)]
pub struct CpuInfo {
    vendor: [u8; 12],
    /// Supervisor Mode Execution Prevention is enabled.
    pub smep: bool,
    /// Supervisor Mode Access Prevention is enabled.
    pub smap: bool,
    /// User-Mode Instruction Prevention is enabled.
    pub umip: bool,
    /// The CPU supports 1 GiB pages.
    pub pages_1g: bool,
    /// The time-stamp counter runs at a constant rate in all power states.
    pub invariant_tsc: bool,
}

impl CpuInfo {
    /// The CPUID vendor string, e.g. `GenuineIntel`.
    pub fn vendor(&self) -> &str {
        core::str::from_utf8(&self.vendor).unwrap_or("<non-ascii>")
    }
}

/// Executes CPUID and returns `(eax, ebx, ecx, edx)`.
pub fn cpuid(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    let r = __cpuid_count(leaf, subleaf);
    (r.eax, r.ebx, r.ecx, r.edx)
}

/// Reads CR2, the faulting address of the most recent page fault.
pub fn read_cr2() -> u64 {
    let value: u64;
    // SAFETY: reading CR2 has no side effects and is permitted in ring 0.
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Reads CR3, the physical address of the active top-level page table plus
/// flag bits.
pub fn read_cr3() -> u64 {
    let value: u64;
    // SAFETY: reading CR3 has no side effects and is permitted in ring 0.
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Loads CR3, switching address space and flushing non-global TLB entries.
///
/// # Safety
///
/// `value` must point to a valid top-level page table that maps all code and
/// data the kernel is currently using.
pub unsafe fn write_cr3(value: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe { asm!("mov cr3, {}", in(reg) value, options(nostack, preserves_flags)) };
}

fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: reading CR0 has no side effects and is permitted in ring 0.
    unsafe { asm!("mov {}, cr0", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

fn read_cr4() -> u64 {
    let value: u64;
    // SAFETY: reading CR4 has no side effects and is permitted in ring 0.
    unsafe { asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Reads a model-specific register.
///
/// # Safety
///
/// `msr` must exist on this CPU; reading a non-existent MSR raises #GP.
pub unsafe fn read_msr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: the caller guarantees the MSR exists, so `rdmsr` cannot fault;
    // it has no effect on memory.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi,
             options(nomem, nostack, preserves_flags));
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Writes a model-specific register.
///
/// # Safety
///
/// `msr` must exist on this CPU and `value` must be valid for it. MSRs
/// control fundamental CPU behaviour; the caller is responsible for the
/// consequences of the specific write.
pub unsafe fn write_msr(msr: u32, value: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32,
             options(nomem, nostack, preserves_flags));
    }
}

/// Reads the time-stamp counter.
pub fn read_tsc() -> u64 {
    // SAFETY: `rdtsc` is available on every x86-64 CPU, is permitted in
    // ring 0 regardless of CR4.TSD, and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Verifies the CPU meets the kernel's requirements and turns on the
/// protection features it offers:
///
/// * `EFER.NXE` – no-execute pages (required);
/// * `CR0.WP` – read-only pages bind the kernel too (required);
/// * `CR4.SMEP`, `CR4.SMAP`, `CR4.UMIP` – when supported.
pub fn init() -> Result<CpuInfo, CpuError> {
    let (_, ebx, ecx, edx) = cpuid(0, 0);
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&edx.to_le_bytes());
    vendor[8..12].copy_from_slice(&ecx.to_le_bytes());

    let (max_basic, ..) = cpuid(0, 0);
    let (max_ext, ..) = cpuid(0x8000_0000, 0);
    let ext_edx = if max_ext >= 0x8000_0001 {
        cpuid(0x8000_0001, 0).3
    } else {
        0
    };
    let (leaf7_ebx, leaf7_ecx) = if max_basic >= 7 {
        let (_, ebx, ecx, _) = cpuid(7, 0);
        (ebx, ecx)
    } else {
        (0, 0)
    };

    if ext_edx & (1 << 20) == 0 {
        return Err(CpuError::NoExecuteUnsupported);
    }
    if read_cr4() & CR4_LA57 != 0 {
        return Err(CpuError::FiveLevelPaging);
    }

    // SAFETY: EFER exists on every x86-64 CPU. NX support was just confirmed
    // via CPUID, so setting EFER.NXE is valid; it only makes the NX bit in
    // page tables effective, which the loader's tables already rely on.
    let nxe_enabled = unsafe {
        let efer = read_msr(MSR_EFER);
        if efer & EFER_NXE == 0 {
            write_msr(MSR_EFER, efer | EFER_NXE);
        }
        read_msr(MSR_EFER) & EFER_NXE != 0
    };
    if !nxe_enabled {
        return Err(CpuError::ControlBitNotSticky("EFER.NXE"));
    }

    let cr0 = read_cr0() | CR0_WP;
    // SAFETY: setting CR0.WP only removes the kernel's ability to write
    // through read-only mappings. The kernel image's writable data is mapped
    // writable by the loader, so no legitimate access is affected.
    unsafe { asm!("mov cr0, {}", in(reg) cr0, options(nostack, preserves_flags)) };
    if read_cr0() & CR0_WP == 0 {
        return Err(CpuError::ControlBitNotSticky("CR0.WP"));
    }

    let smep = leaf7_ebx & (1 << 7) != 0;
    let smap = leaf7_ebx & (1 << 20) != 0;
    let umip = leaf7_ecx & (1 << 2) != 0;
    let mut cr4 = read_cr4();
    if smep {
        cr4 |= CR4_SMEP;
    }
    if smap {
        cr4 |= CR4_SMAP;
    }
    if umip {
        cr4 |= CR4_UMIP;
    }
    // SAFETY: each bit is only set when CPUID reports the feature, so the
    // write cannot fault. SMEP/SMAP restrict kernel access to user-mode
    // pages, of which none exist yet; UMIP only affects ring 3.
    unsafe { asm!("mov cr4, {}", in(reg) cr4, options(nostack, preserves_flags)) };
    if read_cr4() != cr4 {
        return Err(CpuError::ControlBitNotSticky("CR4 protection bits"));
    }

    Ok(CpuInfo {
        vendor,
        smep,
        smap,
        umip,
        pages_1g: ext_edx & (1 << 26) != 0,
        invariant_tsc: max_ext >= 0x8000_0007 && cpuid(0x8000_0007, 0).3 & (1 << 8) != 0,
    })
}
