// SPDX-License-Identifier: GPL-2.0-only
//! Local APIC: the per-CPU interrupt controller and its timer.
//!
//! Both access modes are supported: xAPIC (memory-mapped registers) and
//! x2APIC (registers in MSRs). x2APIC is used whenever the CPU offers it:
//! register access is a plain MSR read or write instead of a device-memory
//! access (which is markedly cheaper under virtualisation), and it is the
//! only mode that can address more than 255 CPUs.

use core::fmt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::interrupts::{self, FIRST_USABLE_VECTOR, RegisterError, SPURIOUS_VECTOR};
use super::paging::{self, MapError};
use super::{cpu, pit};
use crate::error;

/// Vector of the local APIC timer interrupt.
pub const TIMER_VECTOR: u8 = FIRST_USABLE_VECTOR;
/// Vector of the local APIC error interrupt.
const ERROR_VECTOR: u8 = FIRST_USABLE_VECTOR + 1;

const MSR_APIC_BASE: u32 = 0x1B;
const APIC_BASE_X2APIC: u64 = 1 << 10;
const APIC_BASE_ENABLE: u64 = 1 << 11;
const APIC_BASE_ADDRESS: u64 = 0x000F_FFFF_FFFF_F000;
const MSR_X2APIC_BASE: u32 = 0x800;

// Register offsets in the xAPIC page; the x2APIC MSR index is `offset >> 4`.
const REG_ID: u32 = 0x20;
const REG_VERSION: u32 = 0x30;
const REG_TASK_PRIORITY: u32 = 0x80;
const REG_EOI: u32 = 0xB0;
const REG_SPURIOUS: u32 = 0xF0;
const REG_ERROR_STATUS: u32 = 0x280;
const REG_LVT_TIMER: u32 = 0x320;
const REG_LVT_ERROR: u32 = 0x370;
const REG_TIMER_INITIAL: u32 = 0x380;
const REG_TIMER_CURRENT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3E0;

const SPURIOUS_APIC_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const LVT_TIMER_PERIODIC: u32 = 1 << 17;
/// Divide-configuration value for "divide by 16".
const TIMER_DIVIDE_BY_16: u32 = 0b0011;

/// PIT ticks to calibrate against: 1/50 s.
const CALIBRATION_PIT_TICKS: u16 = (pit::FREQUENCY_HZ / 50) as u16;

/// Why the local APIC or its timer could not be set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApicError {
    /// CPUID reports no local APIC.
    NotPresent,
    /// The register page could not be mapped.
    Map(MapError),
    /// An interrupt handler could not be registered.
    Register(RegisterError),
    /// The PIT never signalled the end of the calibration interval.
    CalibrationTimeout,
    /// Calibration produced a frequency of zero, or one too low to generate
    /// the requested tick rate.
    ImplausibleFrequency,
}

impl fmt::Display for ApicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotPresent => write!(f, "CPU has no local APIC"),
            Self::Map(e) => write!(f, "cannot map local APIC: {e}"),
            Self::Register(e) => write!(f, "cannot register handler: {e:?}"),
            Self::CalibrationTimeout => write!(f, "PIT calibration timed out"),
            Self::ImplausibleFrequency => write!(f, "timer calibration gave an unusable result"),
        }
    }
}

/// x2APIC mode is active; registers are accessed through MSRs.
static X2APIC: AtomicBool = AtomicBool::new(false);
/// Virtual address of the xAPIC register page; 0 while unmapped.
static MMIO_BASE: AtomicU64 = AtomicU64::new(0);
/// Set once [`init`] has completed.
static READY: AtomicBool = AtomicBool::new(false);

fn read(register: u32) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: in x2APIC mode every register used by this module exists
        // as MSR `0x800 + (offset >> 4)` and is readable.
        unsafe { cpu::read_msr(MSR_X2APIC_BASE + (register >> 4)) as u32 }
    } else {
        let base = MMIO_BASE.load(Ordering::Relaxed);
        debug_assert!(base != 0);
        // SAFETY: `base` is the device mapping of the 4 KiB xAPIC register
        // page established by `init`; `register` is a valid, 4-byte-aligned
        // offset within it. Device registers require volatile access.
        unsafe { ((base + u64::from(register)) as *const u32).read_volatile() }
    }
}

fn write(register: u32, value: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: as in `read`; all registers this module writes are
        // writable, and the values written are valid per the Intel SDM.
        unsafe { cpu::write_msr(MSR_X2APIC_BASE + (register >> 4), u64::from(value)) };
    } else {
        let base = MMIO_BASE.load(Ordering::Relaxed);
        debug_assert!(base != 0);
        // SAFETY: as in `read`.
        unsafe { ((base + u64::from(register)) as *mut u32).write_volatile(value) };
    }
}

/// What [`init`] found.
#[derive(Debug, Clone, Copy)]
pub struct ApicInfo {
    /// The local APIC ID of the boot CPU.
    pub id: u32,
    /// Version register contents (low byte is the version).
    pub version: u32,
    /// Whether x2APIC mode is in use.
    pub x2apic: bool,
}

/// Enables the local APIC of the current CPU with its timer masked.
pub fn init() -> Result<ApicInfo, ApicError> {
    let (_, _, features_ecx, features_edx) = cpu::cpuid(1, 0);
    if features_edx & (1 << 9) == 0 {
        return Err(ApicError::NotPresent);
    }
    let x2apic_supported = features_ecx & (1 << 21) != 0;

    // SAFETY: IA32_APIC_BASE exists whenever CPUID reports a local APIC.
    let mut base = unsafe { cpu::read_msr(MSR_APIC_BASE) };
    let wanted = base
        | APIC_BASE_ENABLE
        | if x2apic_supported {
            APIC_BASE_X2APIC
        } else {
            0
        };
    if wanted != base {
        // SAFETY: setting the global-enable bit is always valid, and the
        // x2APIC bit is only set when CPUID reports support. The transition
        // xAPIC -> x2APIC is permitted (unlike the reverse); all other bits
        // are written back unchanged. Interrupts are disabled and no APIC
        // interrupt source is active yet, so nothing is lost in the switch.
        unsafe { cpu::write_msr(MSR_APIC_BASE, wanted) };
        // SAFETY: as for the read above.
        base = unsafe { cpu::read_msr(MSR_APIC_BASE) };
    }

    let x2apic = base & APIC_BASE_X2APIC != 0;
    if !x2apic {
        let virt = paging::map_mmio(base & APIC_BASE_ADDRESS, 4096).map_err(ApicError::Map)?;
        MMIO_BASE.store(virt, Ordering::Relaxed);
    }
    X2APIC.store(x2apic, Ordering::Relaxed);

    interrupts::register(ERROR_VECTOR, on_error).map_err(ApicError::Register)?;

    write(REG_TASK_PRIORITY, 0); // accept all priority classes
    write(REG_LVT_TIMER, LVT_MASKED);
    write(REG_LVT_ERROR, u32::from(ERROR_VECTOR));
    write(
        REG_SPURIOUS,
        SPURIOUS_APIC_ENABLE | u32::from(SPURIOUS_VECTOR),
    );
    READY.store(true, Ordering::Release);

    Ok(ApicInfo {
        // xAPIC keeps the 8-bit ID in the top byte; x2APIC uses all 32 bits.
        id: if x2apic {
            read(REG_ID)
        } else {
            read(REG_ID) >> 24
        },
        version: read(REG_VERSION),
        x2apic,
    })
}

/// Signals completion of the interrupt currently being serviced.
pub fn end_of_interrupt() {
    if READY.load(Ordering::Acquire) {
        write(REG_EOI, 0);
    }
}

fn on_error() {
    // The error status register latches on write and is then read.
    write(REG_ERROR_STATUS, 0);
    error!(
        "apic: local APIC error, status {:#x}",
        read(REG_ERROR_STATUS)
    );
}

/// Measured clock rates.
#[derive(Debug, Clone, Copy)]
pub struct Calibration {
    /// Time-stamp counter frequency in Hz.
    pub tsc_hz: u64,
    /// Local APIC timer frequency in Hz, after the divide-by-16 prescaler.
    pub timer_hz: u64,
}

/// Measures the TSC and local APIC timer frequencies against the PIT.
///
/// Takes about 20 ms. Must be called after [`init`], with interrupts
/// disabled so the measurement is not stretched by a handler.
pub fn calibrate() -> Result<Calibration, ApicError> {
    write(REG_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
    write(REG_LVT_TIMER, LVT_MASKED);

    pit::start_one_shot(CALIBRATION_PIT_TICKS);
    write(REG_TIMER_INITIAL, u32::MAX);
    let tsc_start = cpu::read_tsc();
    if !pit::wait_for_one_shot() {
        return Err(ApicError::CalibrationTimeout);
    }
    let tsc_end = cpu::read_tsc();
    let timer_elapsed = u32::MAX - read(REG_TIMER_CURRENT);
    write(REG_TIMER_INITIAL, 0); // stop the timer

    let scale = |count: u64| {
        (u128::from(count) * u128::from(pit::FREQUENCY_HZ) / u128::from(CALIBRATION_PIT_TICKS))
            as u64
    };
    let calibration = Calibration {
        tsc_hz: scale(tsc_end.wrapping_sub(tsc_start)),
        timer_hz: scale(u64::from(timer_elapsed)),
    };
    if calibration.tsc_hz == 0 || calibration.timer_hz == 0 {
        return Err(ApicError::ImplausibleFrequency);
    }
    Ok(calibration)
}

/// Starts the local APIC timer firing [`TIMER_VECTOR`] `hz` times per
/// second. `timer_hz` is the frequency measured by [`calibrate`].
pub fn start_periodic_timer(timer_hz: u64, hz: u32) -> Result<(), ApicError> {
    let initial = u32::try_from(timer_hz / u64::from(hz.max(1)))
        .ok()
        .filter(|&count| count >= 16)
        .ok_or(ApicError::ImplausibleFrequency)?;
    write(REG_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
    write(REG_LVT_TIMER, LVT_TIMER_PERIODIC | u32::from(TIMER_VECTOR));
    write(REG_TIMER_INITIAL, initial);
    Ok(())
}
