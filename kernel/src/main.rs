// SPDX-License-Identifier: GPL-2.0-only
//! The Omit OS kernel.
//!
//! Execution starts at [`_start`], entered from the UEFI loader under the
//! hand-off contract documented in the `omit_bootproto` crate.

#![no_std]
#![no_main]

mod arch;
mod log;
mod panic;
mod sync;

use core::fmt;

use omit_bootproto::memory::{self, MemoryMapError};
use omit_bootproto::{BootInfo, BootInfoError, Cmdline};

use arch::cpu::CpuError;
use arch::paging::PagingError;
use arch::qemu::{self, ExitCode};
use arch::serial::SerialError;

/// The line that announces a fully initialised kernel. Printed only after
/// every step of [`init`] has succeeded; the boot tests look for it.
const SUCCESS_MESSAGE: &str = "Omit OS — Kernel initialized successfully.";

/// Why kernel initialisation stopped.
#[derive(Debug)]
enum InitError {
    Serial(SerialError),
    BootInfoPointer,
    BootInfo(BootInfoError),
    Cpu(CpuError),
    ExceptionSelfTest,
    MemoryMap(MemoryMapError),
    Paging(PagingError),
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serial(e) => write!(f, "serial console: {e:?}"),
            Self::BootInfoPointer => write!(f, "boot info pointer is null or misaligned"),
            Self::BootInfo(e) => write!(f, "boot info: {e}"),
            Self::Cpu(e) => write!(f, "cpu: {e}"),
            Self::ExceptionSelfTest => {
                write!(f, "exception self-test: breakpoint was not handled")
            }
            Self::MemoryMap(e) => write!(f, "memory map: {e}"),
            Self::Paging(e) => write!(f, "paging: {e}"),
        }
    }
}

/// Kernel entry point.
///
/// # Safety
///
/// Must be entered exactly once, by a loader that fulfils the hand-off
/// contract of the `omit_bootproto` crate; in particular `boot_info` must be
/// the direct-map address of a `BootInfo` that stays valid forever.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn _start(boot_info: *const BootInfo) -> ! {
    // SAFETY: forwarded from this function's contract.
    let result = unsafe { init(boot_info) };

    let cmdline = match result {
        Ok(cmdline) => cmdline,
        Err(error) => {
            error!("kernel initialization failed: {error}");
            qemu::exit(ExitCode::Failure);
            arch::halt_forever();
        }
    };

    kprintln!("{SUCCESS_MESSAGE}");

    if let Some(kind) = cmdline.value("fault") {
        inject_fault(kind);
    }

    qemu::exit(ExitCode::Success);
    info!("nothing left to do; halting");
    arch::halt_forever();
}

/// Brings up every foundational kernel component, in dependency order.
/// Returns the kernel command line on success.
///
/// # Safety
///
/// Same contract as [`_start`].
unsafe fn init(boot_info: *const BootInfo) -> Result<Cmdline<'static>, InitError> {
    // 1. Console. Its result is checked only after the command line has been
    //    read, so that a failure can still be reported to the test harness
    //    through the QEMU exit device.
    let serial = arch::serial::init();
    kprintln!();
    info!(
        "Omit OS kernel {} ({}, {} build)",
        env!("CARGO_PKG_VERSION"),
        arch::NAME,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );

    // 2. Boot information.
    if boot_info.is_null() || !boot_info.is_aligned() {
        return Err(InitError::BootInfoPointer);
    }
    // SAFETY: the pointer is non-null and aligned, and per the hand-off
    // contract it addresses a `BootInfo` in the direct map that is never
    // modified or freed, so a `'static` shared reference is sound.
    let boot_info: &'static BootInfo = unsafe { &*boot_info };
    let boot_info_result = boot_info.validate();

    let cmdline = match boot_info_result {
        // SAFETY: `validate` succeeded and the loader's direct map is active.
        Ok(()) => match Cmdline::from_bytes(unsafe { boot_info.cmdline_bytes() }) {
            Some(cmdline) => cmdline,
            None => {
                warn!("boot: ignoring command line with non-printable bytes");
                Cmdline::EMPTY
            }
        },
        Err(_) => Cmdline::EMPTY,
    };
    if cmdline.flag("qemu-exit") {
        qemu::enable();
    }

    serial.map_err(InitError::Serial)?;
    boot_info_result.map_err(InitError::BootInfo)?;
    info!("serial: COM1 16550 UART ready (115200 8N1), loopback test passed");
    info!(
        "boot: protocol v{}, command line \"{}\"",
        boot_info.version,
        cmdline.as_str()
    );
    info!(
        "boot: kernel image {:#x}..{:#x} at phys {:#x} ({} KiB)",
        boot_info.kernel_virt_base,
        boot_info.kernel_virt_base + boot_info.kernel_size,
        boot_info.kernel_phys_base,
        boot_info.kernel_size / 1024
    );
    if boot_info.rsdp_phys != 0 {
        info!("boot: ACPI RSDP at phys {:#x}", boot_info.rsdp_phys);
    } else {
        warn!("boot: firmware provided no ACPI RSDP");
    }

    // 3. CPU: protection features, descriptor tables, exception handling.
    let cpu = arch::cpu::init().map_err(InitError::Cpu)?;
    info!(
        "cpu: {} | NX=on WP=on SMEP={} SMAP={} UMIP={} 1GiB-pages={}",
        cpu.vendor(),
        on_off(cpu.smep),
        on_off(cpu.smap),
        on_off(cpu.umip),
        if cpu.pages_1g { "yes" } else { "no" },
    );
    arch::gdt::init();
    info!("cpu: GDT and TSS loaded (double-fault stack on IST1)");
    arch::idt::init();
    if !arch::idt::self_test() {
        return Err(InitError::ExceptionSelfTest);
    }
    info!("cpu: IDT loaded, 32 exception vectors installed, #BP self-test passed");

    // 4. Physical memory map.
    // SAFETY: `validate` succeeded and the loader's direct map is active.
    let map = unsafe { boot_info.memory_map() };
    let summary = memory::validate(map).map_err(InitError::MemoryMap)?;
    for region in map {
        info!(
            "mem:   {:#014x}..{:#014x} {:>8} KiB  {}",
            region.start,
            region.end().unwrap_or(u64::MAX),
            region.pages * 4,
            region.kind.name()
        );
    }
    info!(
        "mem: {} regions, {} MiB RAM, {} MiB usable (largest block {} MiB), {} MiB reclaimable",
        summary.regions,
        summary.ram_bytes >> 20,
        summary.usable_bytes >> 20,
        summary.largest_usable_bytes >> 20,
        summary.reclaimable_bytes >> 20,
    );

    // 5. Address space: drop the loader's identity mapping, then audit.
    // SAFETY: `boot_info` is validated and describes the active address
    // space. The kernel is linked and running in the higher half on the
    // higher-half boot stack, and every pointer it holds (`boot_info`, the
    // memory map, the command line) points into the direct map.
    unsafe { arch::paging::unmap_identity(boot_info) }.map_err(InitError::Paging)?;
    // SAFETY: as above; nothing else can modify the page tables (single CPU,
    // interrupts disabled).
    let audit = unsafe { arch::paging::audit(boot_info) }.map_err(InitError::Paging)?;
    info!(
        "paging: identity map removed; direct map of {} MiB at {:#x}",
        boot_info.phys_map_size >> 20,
        boot_info.phys_map_base
    );
    info!(
        "paging: audit passed (W^X holds): {} KiB r-x, {} KiB r--, {} MiB rw-, {} table pages",
        audit.executable / 1024,
        audit.read_only / 1024,
        audit.writable >> 20,
        audit.tables
    );

    Ok(cmdline)
}

fn on_off(enabled: bool) -> &'static str {
    if enabled { "on" } else { "unsupported" }
}

/// Deliberately crashes the kernel so the automated tests can verify the
/// panic and exception diagnostics. Selected with `fault=<kind>` on the
/// kernel command line; never triggered otherwise.
fn inject_fault(kind: &str) {
    match kind {
        "panic" => panic!("fault injection: deliberate panic requested on the command line"),
        "pagefault" => {
            warn!("fault injection: reading from unmapped address 0x1000");
            arch::trigger_page_fault(0x1000);
        }
        "stackoverflow" => {
            warn!("fault injection: overflowing the kernel stack");
            overflow_stack(0);
        }
        other => warn!("fault injection: unknown fault kind \"{other}\" ignored"),
    }
}

/// Recurses until the stack guard page is hit.
#[allow(unconditional_recursion)]
#[inline(never)]
fn overflow_stack(depth: u64) -> u64 {
    let pad = core::hint::black_box([depth; 64]);
    overflow_stack(depth + 1) + pad[0]
}
