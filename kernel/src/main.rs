// SPDX-License-Identifier: GPL-2.0-only
//! The Omit OS kernel.
//!
//! Execution starts at [`_start`], entered from the UEFI loader under the
//! hand-off contract documented in the `omit_bootproto` crate.

#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod console;
mod drivers;
mod fbcon;
mod fs;
mod log;
mod mm;
mod panic;
mod pci;
mod process;
mod sched;
mod shell;
mod sync;
mod syscall;
mod time;
mod ui;

use core::fmt;

use omit_bootproto::memory::{self, MemoryMapError};
use omit_bootproto::{BootInfo, BootInfoError, Cmdline};

use arch::acpi::AcpiLookupError;
use arch::apic::ApicError;
use arch::cpu::CpuError;
use arch::interrupts::RegisterError;
use arch::ioapic::IoApicError;
use arch::paging::PagingError;
use arch::qemu::{self, ExitCode};
use arch::serial::SerialError;
use mm::frame::FrameInitError;
use time::TickTestError;

/// Interrupt vector of the serial port's receive interrupt.
const SERIAL_VECTOR: u8 = arch::interrupts::FIRST_USABLE_VECTOR + 2;

/// Interrupt vector of the PS/2 keyboard.
const KEYBOARD_VECTOR: u8 = arch::interrupts::FIRST_USABLE_VECTOR + 3;

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
    FrameAllocator(FrameInitError),
    FrameSelfTest(&'static str),
    HeapSelfTest(&'static str),
    MappingSelfTest(&'static str),
    Apic(ApicError),
    Timer(ApicError),
    TickSelfTest(TickTestError),
    Acpi(AcpiLookupError),
    IoApic(IoApicError),
    InputInterrupt(RegisterError),
    Scheduler(sched::SpawnError),
    SchedulerSelfTest(&'static str),
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
            Self::FrameAllocator(e) => write!(f, "frame allocator: {e}"),
            Self::FrameSelfTest(e) => write!(f, "frame allocator self-test: {e}"),
            Self::HeapSelfTest(e) => write!(f, "heap self-test: {e}"),
            Self::MappingSelfTest(e) => write!(f, "page mapping self-test: {e}"),
            Self::Apic(e) => write!(f, "interrupt controller: {e}"),
            Self::Timer(e) => write!(f, "timer: {e}"),
            Self::TickSelfTest(e) => write!(f, "timer self-test: {e}"),
            Self::Acpi(e) => write!(f, "acpi: {e}"),
            Self::IoApic(e) => write!(f, "I/O APIC: {e}"),
            Self::InputInterrupt(e) => write!(f, "input interrupt: {e:?}"),
            Self::Scheduler(e) => write!(f, "scheduler: {e:?}"),
            Self::SchedulerSelfTest(e) => write!(f, "scheduler self-test: {e}"),
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

    let (cmdline, initrd) = match result {
        Ok(context) => context,
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

    fs::init(initrd);
    if !cmdline.flag("noshell") {
        shell::run();
    }

    qemu::exit(ExitCode::Success);
    info!("nothing left to do; idling");
    loop {
        arch::interrupts::wait();
    }
}

/// Brings up every foundational kernel component, in dependency order.
/// Returns the kernel command line on success.
///
/// # Safety
///
/// Same contract as [`_start`].
unsafe fn init(boot_info: *const BootInfo) -> Result<(Cmdline<'static>, &'static [u8]), InitError> {
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
    info!("cpu: IDT loaded, 256 vectors installed, #BP self-test passed");

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

    // 5. Address space: drop the loader's identity mapping.
    // SAFETY: `boot_info` is validated and describes the active address
    // space. The kernel is linked and running in the higher half on the
    // higher-half boot stack, and every pointer it holds (`boot_info`, the
    // memory map, the command line) points into the direct map.
    unsafe { arch::paging::unmap_identity(boot_info) }.map_err(InitError::Paging)?;
    mm::init_direct_map(boot_info);
    arch::uspace::init();
    info!(
        "paging: identity map removed; direct map of {} MiB at {:#x}",
        boot_info.phys_map_size >> 20,
        boot_info.phys_map_base
    );

    // 6. Physical frame allocator.
    // SAFETY: `map` is the validated boot memory map. Its usable regions are
    // free RAM by the loader's contract: the kernel image, stack, page
    // tables and boot information are all tagged with other kinds, and the
    // kernel has allocated nothing yet. The direct map was registered above.
    let frames = unsafe { mm::frame::init(map) }.map_err(InitError::FrameAllocator)?;
    mm::frame::self_test().map_err(InitError::FrameSelfTest)?;
    info!(
        "mem: frame allocator ready: {} MiB managed, {} MiB free, {} KiB metadata; self-test passed",
        (frames.managed as u64 * mm::PAGE_SIZE) >> 20,
        (frames.free as u64 * mm::PAGE_SIZE) >> 20,
        frames.metadata_bytes / 1024,
    );

    mm::heap::self_test().map_err(InitError::HeapSelfTest)?;
    let heap = mm::heap::stats();
    info!(
        "mem: kernel heap ready: self-test passed ({} slab pages retained)",
        heap.slab_pages
    );
    arch::paging::self_test(false).map_err(InitError::MappingSelfTest)?;
    info!("paging: map/unmap self-test passed");

    // The screen is optional: a machine without a framebuffer still has the
    // serial console, so its absence is reported but is not a failure.
    match fbcon::init(boot_info) {
        Ok(screen) => info!(
            "display: framebuffer console {}x{} pixels, {}x{} characters",
            screen.width, screen.height, screen.cols, screen.rows
        ),
        Err(fbcon::FbconError::NoFramebuffer) => {
            info!("display: no framebuffer; serial console only");
        }
        Err(error) => warn!("display: framebuffer console unavailable: {error}"),
    }

    // 7. Interrupt controllers.
    arch::pic::disable();
    let apic = arch::apic::init().map_err(InitError::Apic)?;
    info!(
        "irq: legacy PIC masked; local APIC {} enabled ({} mode, version {:#x})",
        apic.id,
        if apic.x2apic { "x2APIC" } else { "xAPIC" },
        apic.version & 0xFF,
    );

    // 8. Clocks and the periodic tick. Interrupts are enabled from here on.
    let clocks = time::init().map_err(InitError::Timer)?;
    info!(
        "time: TSC {} MHz ({}), APIC timer {} MHz, tick {} Hz",
        clocks.tsc_hz / 1_000_000,
        if cpu.invariant_tsc {
            "invariant"
        } else {
            "not invariant"
        },
        clocks.timer_hz / 1_000_000,
        time::TICK_HZ,
    );
    let elapsed_ns = time::self_test().map_err(InitError::TickSelfTest)?;
    info!(
        "time: timer interrupt self-test passed ({} ticks in {} ms)",
        time::SELF_TEST_TICKS,
        elapsed_ns / 1_000_000,
    );

    // 9. Device interrupts: find the I/O APIC through ACPI and route the
    //    serial port's receive interrupt, which makes the console interactive.
    // SAFETY: `rsdp_phys` is the RSDP address reported by the firmware (or
    // 0), passed through the validated boot information, and the direct map
    // was registered in step 5.
    let acpi =
        unsafe { arch::acpi::interrupt_config(boot_info.rsdp_phys) }.map_err(InitError::Acpi)?;
    let lines =
        arch::ioapic::init(acpi.ioapic_address, acpi.ioapic_gsi_base).map_err(InitError::IoApic)?;
    info!(
        "irq: ACPI lists {} CPU(s); I/O APIC at {:#x} with {} lines",
        acpi.cpu_count, acpi.ioapic_address, lines
    );
    arch::interrupts::register(SERIAL_VECTOR, arch::serial::handle_interrupt)
        .map_err(InitError::InputInterrupt)?;
    // The I/O APIC's physical destination field is 8 bits wide; the boot
    // CPU's ID always fits on machines this kernel can currently boot.
    arch::ioapic::enable(acpi.serial_irq, SERIAL_VECTOR, apic.id as u8)
        .map_err(InitError::IoApic)?;
    console::enable_input();
    info!(
        "console: serial input enabled (GSI {} -> vector {})",
        acpi.serial_irq.gsi, SERIAL_VECTOR
    );
    arch::ps2::init(console::push_input);
    arch::interrupts::register(KEYBOARD_VECTOR, arch::ps2::handle_interrupt)
        .map_err(InitError::InputInterrupt)?;
    arch::ioapic::enable(acpi.keyboard_irq, KEYBOARD_VECTOR, apic.id as u8)
        .map_err(InitError::IoApic)?;
    info!(
        "console: PS/2 keyboard input enabled (GSI {} -> vector {})",
        acpi.keyboard_irq.gsi, KEYBOARD_VECTOR
    );

    // Threads and the scheduler. From here on the boot context is thread 1.
    sched::init().map_err(InitError::Scheduler)?;
    sched::self_test().map_err(InitError::SchedulerSelfTest)?;
    info!("sched: round-robin scheduler started; preemption self-test passed");
    sched::spawn("status-bar", ui::status_thread, 0).map_err(InitError::Scheduler)?;
    arch::syscall::init();
    info!("syscall: user mode and system call entry enabled");

    // Devices. Storage is optional: the system runs from RAM without it.
    let functions = pci::init();
    info!("pci: {functions} function(s) found");
    match drivers::virtio_blk::init() {
        Ok(Some(sectors)) => {
            // Reading the first sector proves the request path end to end
            // without modifying the disk.
            let mut sector = [0u8; drivers::block::SECTOR_SIZE];
            match drivers::block::with_device("vda", |device| device.read(0, &mut sector)) {
                Ok(()) => info!(
                    "virtio-blk: vda ready, {} MiB; read test passed",
                    (sectors * drivers::block::SECTOR_SIZE as u64) >> 20
                ),
                Err(error) => warn!("virtio-blk: vda read test failed: {error}"),
            }
        }
        Ok(None) => info!("virtio-blk: no device"),
        Err(error) => warn!("virtio-blk: cannot initialise device: {error}"),
    }

    // 10. Audit the final address space, including the mappings made above.
    // SAFETY: `boot_info` is validated and describes the active address
    // space. Page tables are only modified under the address-space lock by
    // mapping calls, none of which can run concurrently with this (single
    // CPU, and interrupt handlers do not map memory).
    let audit = unsafe { arch::paging::audit(boot_info) }.map_err(InitError::Paging)?;
    info!(
        "paging: audit passed (W^X holds): {} KiB r-x, {} KiB r--, {} MiB rw-, {} table pages",
        audit.executable / 1024,
        audit.read_only / 1024,
        audit.writable >> 20,
        audit.tables
    );
    let unhandled = arch::interrupts::unhandled_count();
    if unhandled != 0 {
        warn!("irq: {unhandled} interrupt(s) arrived on vectors without a handler");
    }

    // The initial ramdisk: memory the loader tagged for that purpose, which
    // the frame allocator therefore never hands out.
    let initrd: &'static [u8] = if boot_info.initrd_len == 0 {
        &[]
    } else {
        let pointer = mm::phys_to_virt(boot_info.initrd_phys, boot_info.initrd_len)
            .ok_or(InitError::BootInfo(BootInfoError::BadField("initrd_*")))?;
        // SAFETY: `validate` checked that the range lies inside the direct
        // map (so `phys_to_virt` succeeded); the loader filled it with the
        // ramdisk file and tagged it `INITRD`, a kind the kernel neither
        // allocates nor writes, so the bytes are initialised and immutable
        // for the lifetime of the kernel.
        unsafe { core::slice::from_raw_parts(pointer, boot_info.initrd_len as usize) }
    };

    Ok((cmdline, initrd))
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
        "unmapped" => {
            warn!("fault injection: reading through a page after unmapping it");
            if let Err(error) = arch::paging::self_test(true) {
                error!("fault injection: mapping test failed early: {error}");
            }
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
