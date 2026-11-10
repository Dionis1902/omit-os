// SPDX-License-Identifier: GPL-2.0-only
//! Omit OS UEFI boot loader.
//!
//! A deliberately small UEFI application whose only job is to get the kernel
//! running under the hand-off contract defined by the `omit_bootproto`
//! crate:
//!
//! 1. read `\EFI\OMIT\KERNEL.ELF` (and the optional `\EFI\OMIT\CMDLINE`)
//!    from the volume the loader itself was started from;
//! 2. validate the ELF image and copy its segments into freshly allocated
//!    memory;
//! 3. build the initial kernel address space (see [`paging`]);
//! 4. exit boot services and convert the firmware memory map;
//! 5. switch page tables and stack and jump to the kernel entry point.
//!
//! The loader has no heap. Everything it hands to the kernel lives in pages
//! allocated with OS-defined UEFI memory types, which is how those pages are
//! recognised when the final memory map is converted.

#![no_std]
#![no_main]

mod paging;

use core::arch::asm;
use core::arch::x86_64::__cpuid;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use omit_bootproto::{
    BOOT_INFO_MAGIC, BOOT_PROTOCOL_VERSION, BootInfo, CMDLINE_MAX_LEN, CMDLINE_PATH, KERNEL_BASE,
    KERNEL_PATH, KERNEL_STACK_BOTTOM, KERNEL_STACK_SIZE, KERNEL_STACK_TOP, MemoryKind,
    MemoryRegion, PAGE_SIZE, PHYS_MAP_BASE, PHYS_MAP_MAX_SIZE, memory,
};
use omit_elf::{EM_X86_64, ET_EXEC, Elf64};
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::boot::{OpenProtocolAttributes, OpenProtocolParams};
use uefi::mem::memory_map::MemoryMap;
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::proto::media::file::{Directory, File, FileAttribute, FileMode, RegularFile};
use uefi::table::cfg::ConfigTableEntry;
use uefi::{CStr16, system};

use paging::{AddressSpace, NO_EXECUTE, WRITABLE};

// OS-defined UEFI memory types (the range 0x8000_0000.. is reserved for OS
// loaders by the UEFI specification). Tagging allocations this way lets the
// memory map conversion tell the kernel exactly what each one holds.
const MEM_KERNEL_IMAGE: MemoryType = MemoryType::custom(0x8000_0001);
const MEM_KERNEL_STACK: MemoryType = MemoryType::custom(0x8000_0002);
const MEM_PAGE_TABLES: MemoryType = MemoryType::custom(0x8000_0003);
const MEM_BOOT_INFO: MemoryType = MemoryType::custom(0x8000_0004);

/// Upper bound on the size of the kernel ELF file, as a sanity check.
const KERNEL_FILE_MAX: u64 = 256 << 20;
/// Upper bound on the in-memory size of the kernel image.
const KERNEL_IMAGE_MAX: u64 = 256 << 20;
/// The direct map always covers at least the 32-bit address space, where
/// firmware tables and legacy devices live.
const PHYS_MAP_MIN_SIZE: u64 = 4 << 30;
/// Slack added to the memory-map array for entries created between sizing
/// it and exiting boot services.
const MEMORY_MAP_SLACK: usize = 64;
/// Offset of the command line within the boot-info page.
const CMDLINE_OFFSET: u64 = 2048;
const _: () = assert!(size_of::<BootInfo>() as u64 <= CMDLINE_OFFSET);
const _: () = assert!(CMDLINE_OFFSET + CMDLINE_MAX_LEN as u64 <= PAGE_SIZE);

/// A fatal loader error.
#[derive(Debug, Clone, Copy)]
pub struct Error {
    what: &'static str,
    status: Option<Status>,
}

impl Error {
    /// An error that did not originate from a firmware call.
    pub const fn new(what: &'static str) -> Self {
        Self { what, status: None }
    }

    fn uefi<D: fmt::Debug>(what: &'static str) -> impl FnOnce(uefi::Error<D>) -> Self {
        move |e| Self {
            what,
            status: Some(e.status()),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.status {
            Some(status) => write!(f, "{} ({status:?})", self.what),
            None => f.write_str(self.what),
        }
    }
}

/// Cleared just before `ExitBootServices`; console output is only possible
/// while it is set.
static BOOT_SERVICES_ACTIVE: AtomicBool = AtomicBool::new(true);

fn console_write(args: fmt::Arguments<'_>) {
    if BOOT_SERVICES_ACTIVE.load(Ordering::Relaxed) {
        system::with_stdout(|out| {
            let _ = out.write_fmt(args);
        });
    }
}

macro_rules! println {
    ($($arg:tt)*) => {
        console_write(format_args!("omit-boot: {}\n", format_args!($($arg)*)))
    };
}

/// Allocates `pages` zero-filled pages of UEFI memory type `ty` and returns
/// their physical address.
pub fn alloc_zeroed_pages(ty: MemoryType, pages: u64) -> Result<u64, Error> {
    let count = usize::try_from(pages).map_err(|_| Error::new("allocation too large"))?;
    let ptr = boot::allocate_pages(AllocateType::AnyPages, ty, count)
        .map_err(Error::uefi("out of memory"))?;
    // SAFETY: the firmware just handed us `count` whole pages starting at
    // `ptr`, exclusively owned and writable.
    unsafe { ptr.as_ptr().write_bytes(0, count * PAGE_SIZE as usize) };
    Ok(ptr.as_ptr() as u64)
}

fn pages_for(bytes: u64) -> u64 {
    bytes.div_ceil(PAGE_SIZE)
}

/// Reads a whole file into freshly allocated loader memory.
///
/// Returns `Ok(None)` if the file does not exist. The buffer is never freed:
/// it is ordinary loader data, which the kernel sees as boot-reclaimable.
fn read_file(
    root: &mut Directory,
    path: &str,
    max_size: u64,
) -> Result<Option<&'static [u8]>, Error> {
    let mut path_buf = [0u16; 64];
    let path = CStr16::from_str_with_buf(path, &mut path_buf)
        .map_err(|_| Error::new("invalid file path"))?;

    let handle = match root.open(path, FileMode::Read, FileAttribute::empty()) {
        Ok(handle) => handle,
        Err(e) if e.status() == Status::NOT_FOUND => return Ok(None),
        Err(e) => return Err(Error::uefi("cannot open file")(e)),
    };
    let mut file = handle
        .into_regular_file()
        .ok_or(Error::new("path is not a regular file"))?;

    file.set_position(RegularFile::END_OF_FILE)
        .map_err(Error::uefi("cannot seek file"))?;
    let size = file
        .get_position()
        .map_err(Error::uefi("cannot determine file size"))?;
    file.set_position(0)
        .map_err(Error::uefi("cannot seek file"))?;
    if size > max_size {
        return Err(Error::new("file is too large"));
    }
    if size == 0 {
        return Ok(Some(&[]));
    }

    let address = alloc_zeroed_pages(MemoryType::LOADER_DATA, pages_for(size))?;
    // SAFETY: `address` is a fresh, zero-initialised allocation of at least
    // `size` bytes that is never freed or otherwise referenced, so a unique
    // `'static` slice over it is sound.
    let buffer = unsafe { core::slice::from_raw_parts_mut(address as *mut u8, size as usize) };

    let mut filled = 0;
    while filled < buffer.len() {
        let n = file
            .read(&mut buffer[filled..])
            .map_err(Error::uefi("cannot read file"))?;
        if n == 0 {
            return Err(Error::new("unexpected end of file"));
        }
        filled += n;
    }
    Ok(Some(buffer))
}

/// Where the kernel image ended up.
struct LoadedKernel {
    phys_base: u64,
    virt_base: u64,
    size: u64,
    entry: u64,
}

/// Validates the kernel ELF image, copies its segments into newly allocated
/// memory and maps them into `space` with the permissions the segments ask
/// for.
fn load_kernel(elf: &Elf64<'_>, space: &mut AddressSpace) -> Result<LoadedKernel, Error> {
    if elf.machine() != EM_X86_64 {
        return Err(Error::new("kernel is not an x86-64 image"));
    }
    if elf.elf_type() != ET_EXEC {
        return Err(Error::new("kernel is not a fixed-address executable"));
    }

    // Pass 1: validate the layout and find the image's extent.
    let mut virt_base = u64::MAX;
    let mut virt_end = 0u64;
    let mut prev_end = 0u64;
    let mut entry_ok = false;
    for ph in elf.load_segments() {
        if ph.memsz == 0 {
            continue;
        }
        if ph.vaddr % PAGE_SIZE != 0 {
            return Err(Error::new("kernel segment is not page-aligned"));
        }
        if ph.vaddr < KERNEL_BASE {
            return Err(Error::new("kernel segment lies below KERNEL_BASE"));
        }
        if ph.is_writable() && ph.is_executable() {
            return Err(Error::new("kernel segment is writable and executable"));
        }
        let end = ph
            .vaddr_end()
            .checked_next_multiple_of(PAGE_SIZE)
            .ok_or(Error::new("kernel segment wraps the address space"))?;
        if ph.vaddr < prev_end {
            return Err(Error::new("kernel segments overlap or are unsorted"));
        }
        prev_end = end;
        virt_base = virt_base.min(ph.vaddr);
        virt_end = virt_end.max(end);
        if ph.is_executable() && (ph.vaddr..ph.vaddr_end()).contains(&elf.entry()) {
            entry_ok = true;
        }
    }
    if virt_end == 0 {
        return Err(Error::new("kernel has no loadable segments"));
    }
    if !entry_ok {
        return Err(Error::new("kernel entry point is not in executable code"));
    }
    let size = virt_end - virt_base;
    if size > KERNEL_IMAGE_MAX {
        return Err(Error::new("kernel image is too large"));
    }

    // Pass 2: copy and map. The allocation is zero-filled, which takes care
    // of .bss and of padding between segments.
    let phys_base = alloc_zeroed_pages(MEM_KERNEL_IMAGE, size / PAGE_SIZE)?;
    for ph in elf.load_segments() {
        if ph.memsz == 0 {
            continue;
        }
        let phys = phys_base + (ph.vaddr - virt_base);
        let data = elf.segment_data(&ph);
        // SAFETY: `[phys, phys + memsz)` lies inside the image allocation
        // because the segment's virtual range lies inside
        // `[virt_base, virt_end)`, and `data.len() == filesz <= memsz` is
        // guaranteed by the ELF parser. Source (the file buffer) and
        // destination are distinct allocations.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), phys as *mut u8, data.len()) };

        let mut flags = 0;
        if ph.is_writable() {
            flags |= WRITABLE;
        }
        if !ph.is_executable() {
            flags |= NO_EXECUTE;
        }
        space.map_range(ph.vaddr, phys, pages_for(ph.memsz), flags)?;
    }

    Ok(LoadedKernel {
        phys_base,
        virt_base,
        size,
        entry: elf.entry(),
    })
}

/// Translates a UEFI memory type into the kernel's classification.
fn memory_kind(ty: MemoryType) -> MemoryKind {
    match ty {
        MemoryType::CONVENTIONAL => MemoryKind::USABLE,
        MemoryType::LOADER_CODE
        | MemoryType::LOADER_DATA
        | MemoryType::BOOT_SERVICES_CODE
        | MemoryType::BOOT_SERVICES_DATA => MemoryKind::BOOT_RECLAIMABLE,
        MemoryType::RUNTIME_SERVICES_CODE | MemoryType::RUNTIME_SERVICES_DATA => {
            MemoryKind::FIRMWARE_RUNTIME
        }
        MemoryType::ACPI_RECLAIM => MemoryKind::ACPI_RECLAIMABLE,
        MemoryType::ACPI_NON_VOLATILE => MemoryKind::ACPI_NVS,
        MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => MemoryKind::MMIO,
        MemoryType::UNUSABLE => MemoryKind::UNUSABLE,
        MemoryType::PERSISTENT_MEMORY => MemoryKind::PERSISTENT,
        MEM_KERNEL_IMAGE => MemoryKind::KERNEL_IMAGE,
        MEM_KERNEL_STACK => MemoryKind::KERNEL_STACK,
        MEM_PAGE_TABLES => MemoryKind::PAGE_TABLES,
        MEM_BOOT_INFO => MemoryKind::BOOT_INFO,
        _ => MemoryKind::RESERVED,
    }
}

/// The firmware's linear framebuffer, in boot-protocol terms.
#[derive(Clone, Copy, Default)]
struct Framebuffer {
    phys: u64,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
}

/// Describes the framebuffer of the current graphics mode, if the firmware
/// has a Graphics Output Protocol with a 32-bit direct-colour mode. The
/// mode is not changed.
fn find_framebuffer() -> Option<Framebuffer> {
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>().ok()?;
    // SAFETY: the protocol is opened non-exclusively ("get protocol") so the
    // firmware's own console keeps working on the same device; it is only
    // queried, and the handle is closed when `gop` is dropped at the end of
    // this function, before boot services are exited.
    let mut gop = unsafe {
        boot::open_protocol::<GraphicsOutput>(
            OpenProtocolParams {
                handle,
                agent: boot::image_handle(),
                controller: None,
            },
            OpenProtocolAttributes::GetProtocol,
        )
    }
    .ok()?;

    let info = gop.current_mode_info();
    let format = match info.pixel_format() {
        PixelFormat::Rgb => omit_bootproto::FRAMEBUFFER_RGB,
        PixelFormat::Bgr => omit_bootproto::FRAMEBUFFER_BGR,
        // Bitmask layouts and BLT-only devices are not supported.
        _ => return None,
    };
    let (width, height) = info.resolution();
    Some(Framebuffer {
        phys: gop.frame_buffer().as_mut_ptr() as u64,
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        stride: u32::try_from(info.stride()).ok()?,
        format,
    })
}

/// CPU capabilities the loader depends on.
struct CpuFeatures {
    pages_1g: bool,
}

fn check_cpu() -> Result<CpuFeatures, Error> {
    let max_extended = __cpuid(0x8000_0000).eax;
    if max_extended < 0x8000_0001 {
        return Err(Error::new("CPU lacks extended CPUID leaves"));
    }
    let edx = __cpuid(0x8000_0001).edx;
    if edx & (1 << 20) == 0 {
        return Err(Error::new("CPU lacks NX page protection"));
    }

    let cr4: u64;
    // SAFETY: reading CR4 has no side effects; UEFI applications run in
    // ring 0.
    unsafe { asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags)) };
    if cr4 & (1 << 12) != 0 {
        return Err(Error::new(
            "firmware enabled 5-level paging, which is not supported",
        ));
    }
    Ok(CpuFeatures {
        pages_1g: edx & (1 << 26) != 0,
    })
}

/// Writes a message to the legacy COM1 port. Used only for failures after
/// boot services have been exited, when the firmware console is gone.
fn emergency_serial(message: &str) {
    for byte in message.bytes() {
        // SAFETY: port 0x3F8 is the COM1 data register on PC-compatible
        // hardware; writing a byte to it cannot affect memory safety. If no
        // UART is present the write is ignored.
        unsafe { asm!("out dx, al", in("dx") 0x3F8u16, in("al") byte, options(nomem, nostack)) };
    }
}

fn halt() -> ! {
    loop {
        // SAFETY: disabling interrupts and halting is always permitted in
        // ring 0 and is the intended terminal state.
        unsafe { asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    println!("PANIC: {info}");
    if !BOOT_SERVICES_ACTIVE.load(Ordering::Relaxed) {
        emergency_serial("omit-boot: panic after ExitBootServices\r\n");
    }
    halt()
}

#[entry]
fn main() -> Status {
    let error = match run() {
        Ok(never) => match never {},
        Err(error) => error,
    };
    println!("fatal: {error}");
    // Leave the message on screen before returning to the firmware.
    boot::stall(Duration::from_secs(5));
    Status::LOAD_ERROR
}

fn run() -> Result<core::convert::Infallible, Error> {
    println!(
        "Omit OS boot loader {} (protocol v{BOOT_PROTOCOL_VERSION})",
        env!("CARGO_PKG_VERSION")
    );
    let cpu = check_cpu()?;

    // --- Load files -------------------------------------------------------
    let (kernel_file, cmdline) = {
        let mut fs = boot::get_image_file_system(boot::image_handle())
            .map_err(Error::uefi("cannot access the boot volume"))?;
        let mut root = fs
            .open_volume()
            .map_err(Error::uefi("cannot open the boot volume"))?;
        let kernel = read_file(&mut root, KERNEL_PATH, KERNEL_FILE_MAX)?
            .ok_or(Error::new("kernel image \\EFI\\OMIT\\KERNEL.ELF not found"))?;
        let cmdline = read_file(&mut root, CMDLINE_PATH, CMDLINE_MAX_LEN as u64)?.unwrap_or(&[]);
        (kernel, cmdline)
    };
    println!("kernel image: {} bytes", kernel_file.len());

    // --- Build the kernel address space -----------------------------------
    let elf = Elf64::parse(kernel_file).map_err(|_| Error::new("kernel image is not valid ELF"))?;
    let mut space = AddressSpace::new(MEM_PAGE_TABLES)?;
    let kernel = load_kernel(&elf, &mut space)?;

    let stack_pages = KERNEL_STACK_SIZE / PAGE_SIZE;
    let stack_phys = alloc_zeroed_pages(MEM_KERNEL_STACK, stack_pages)?;
    space.map_range(
        KERNEL_STACK_BOTTOM,
        stack_phys,
        stack_pages,
        WRITABLE | NO_EXECUTE,
    )?;

    // Size the direct map and the memory-map array from the current
    // firmware map. RAM cannot appear later, so the extent is final; the
    // entry count can still grow slightly, hence the slack.
    let (ram_end, firmware_entries) = {
        let map = boot::memory_map(MemoryType::LOADER_DATA)
            .map_err(Error::uefi("cannot read the memory map"))?;
        let ram_end = map
            .entries()
            .filter(|d| memory_kind(d.ty).is_ram())
            .map(|d| {
                d.phys_start
                    .saturating_add(d.page_count.saturating_mul(PAGE_SIZE))
            })
            .max()
            .unwrap_or(0);
        (ram_end, map.len())
    };
    let phys_map_size = ram_end
        .max(PHYS_MAP_MIN_SIZE)
        .checked_next_multiple_of(1 << 30)
        .filter(|&size| size <= PHYS_MAP_MAX_SIZE)
        .ok_or(Error::new(
            "more physical memory than the direct map supports",
        ))?;
    space.map_physical(phys_map_size, cpu.pages_1g)?;

    // --- Boot information block -------------------------------------------
    // Page 0: `BootInfo`, with the command line at CMDLINE_OFFSET.
    // Pages 1..: the memory region array.
    let region_capacity = firmware_entries + MEMORY_MAP_SLACK;
    let region_bytes = (region_capacity * size_of::<MemoryRegion>()) as u64;
    let info_phys = alloc_zeroed_pages(MEM_BOOT_INFO, 1 + pages_for(region_bytes))?;
    let cmdline_phys = info_phys + CMDLINE_OFFSET;
    let regions_phys = info_phys + PAGE_SIZE;
    // SAFETY: `cmdline.len() <= CMDLINE_MAX_LEN` (enforced by `read_file`),
    // and the destination range lies within the first page of the fresh
    // boot-info allocation (see the constant assertions above).
    unsafe {
        core::ptr::copy_nonoverlapping(cmdline.as_ptr(), cmdline_phys as *mut u8, cmdline.len());
    }

    let rsdp_phys = system::with_config_table(|tables| {
        let find = |guid| {
            tables
                .iter()
                .find(|t| t.guid == guid)
                .map(|t| t.address as u64)
        };
        find(ConfigTableEntry::ACPI2_GUID)
            .or_else(|| find(ConfigTableEntry::ACPI_GUID))
            .unwrap_or(0)
    });

    println!(
        "kernel loaded at phys {:#x}, entry {:#x}; direct map {} GiB ({} pages)",
        kernel.phys_base,
        kernel.entry,
        phys_map_size >> 30,
        if cpu.pages_1g { "1 GiB" } else { "2 MiB" },
    );
    let framebuffer = find_framebuffer().unwrap_or_default();
    if framebuffer.format != omit_bootproto::FRAMEBUFFER_NONE {
        println!(
            "framebuffer: {}x{} at phys {:#x}",
            framebuffer.width, framebuffer.height, framebuffer.phys
        );
    } else {
        println!("framebuffer: none (serial console only)");
    }
    println!("exiting boot services");

    // --- Point of no return -----------------------------------------------
    BOOT_SERVICES_ACTIVE.store(false, Ordering::Relaxed);
    // SAFETY: no boot-services resources (protocols, pool allocations,
    // console writers) are held past this point: the file system handles
    // were dropped above, and all console output is gated on
    // `BOOT_SERVICES_ACTIVE`.
    let firmware_map = unsafe { boot::exit_boot_services(None) };

    // SAFETY: `regions_phys` addresses `region_capacity` zero-initialised
    // `MemoryRegion` slots (all-zero is a valid value) in the boot-info
    // allocation; it is page-aligned and referenced nowhere else.
    let regions = unsafe {
        core::slice::from_raw_parts_mut(regions_phys as *mut MemoryRegion, region_capacity)
    };
    let mut count = 0;
    for descriptor in firmware_map.entries() {
        let Some(slot) = regions.get_mut(count) else {
            emergency_serial("omit-boot: fatal: memory map grew beyond its buffer\r\n");
            halt();
        };
        *slot = MemoryRegion::new(
            descriptor.phys_start,
            descriptor.page_count,
            memory_kind(descriptor.ty),
        );
        count += 1;
    }
    let region_count = memory::sort_and_merge(&mut regions[..count]);
    // The firmware map's backing memory must not be returned to the (now
    // defunct) pool allocator.
    core::mem::forget(firmware_map);

    let boot_info = BootInfo {
        magic: BOOT_INFO_MAGIC,
        version: BOOT_PROTOCOL_VERSION,
        reserved: 0,
        phys_map_base: PHYS_MAP_BASE,
        phys_map_size,
        memory_map_addr: PHYS_MAP_BASE + regions_phys,
        memory_map_len: region_count as u64,
        kernel_phys_base: kernel.phys_base,
        kernel_virt_base: kernel.virt_base,
        kernel_size: kernel.size,
        rsdp_phys: if rsdp_phys < phys_map_size {
            rsdp_phys
        } else {
            0
        },
        cmdline_addr: PHYS_MAP_BASE + cmdline_phys,
        cmdline_len: cmdline.len() as u64,
        framebuffer_phys: framebuffer.phys,
        framebuffer_width: framebuffer.width,
        framebuffer_height: framebuffer.height,
        framebuffer_stride: framebuffer.stride,
        framebuffer_format: framebuffer.format,
    };
    // SAFETY: `info_phys` is the page-aligned start of the boot-info
    // allocation, large enough for a `BootInfo` and not aliased.
    unsafe { (info_phys as *mut BootInfo).write(boot_info) };

    // SAFETY: this is the hand-off. The new page tables map (a) all physical
    // memory at its identity address, so the loader's code keeps executing
    // after CR3 changes, (b) the kernel image at its link address with the
    // entry point in an executable segment (validated by `load_kernel`),
    // (c) the boot stack ending at KERNEL_STACK_TOP, and (d) the direct map
    // through which `rdi` addresses the `BootInfo`. NX support was verified
    // by `check_cpu`, so setting EFER.NXE is valid and must precede loading
    // tables that use the NX bit. A zero return address is pushed so the
    // kernel entry sees an ABI-conformant stack and backtraces terminate.
    unsafe {
        asm!(
            "cli",
            "mov ecx, 0xC0000080", // IA32_EFER
            "rdmsr",
            "or eax, 0x800",       // NXE
            "wrmsr",
            "mov cr3, r8",
            "mov rsp, r9",
            "xor ebp, ebp",
            "push 0",
            "jmp r10",
            // Fixed registers: `rdmsr`/`wrmsr` clobber rax, rcx and rdx, so
            // the operands must not be allocated there.
            in("r8") space.root(),
            in("r9") KERNEL_STACK_TOP,
            in("r10") kernel.entry,
            in("rdi") PHYS_MAP_BASE + info_phys,
            options(noreturn),
        )
    }
}
