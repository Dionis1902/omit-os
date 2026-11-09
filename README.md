# Omit OS

> Everything you need. Nothing you don't.

Omit OS is a from-scratch operating system for x86-64 with its own kernel,
written in Rust. It is not a Linux distribution and contains no Linux or
Windows code.

**This is an early-stage kernel.** Today it boots under UEFI, takes control of
the CPU and the address space, manages physical memory and a kernel heap,
handles timer interrupts, and reports its state on the serial console. There
is no scheduler, filesystem, shell or driver model yet. See [Status](#status) for exactly what exists and what has been tested.

```text
omit-boot: Omit OS boot loader 0.1.0 (protocol v1)
omit-boot: kernel image: 938800 bytes
omit-boot: kernel loaded at phys 0xe2c6000, entry 0xffffffff80004cc0; direct map 4 GiB (1 GiB pages)
omit-boot: exiting boot services

[INFO ] Omit OS kernel 0.1.0 (x86_64, release build)
[INFO ] serial: COM1 16550 UART ready (115200 8N1), loopback test passed
[INFO ] boot: protocol v1, command line ""
[INFO ] boot: kernel image 0xffffffff80000000..0xffffffff80014000 at phys 0xe2c6000 (80 KiB)
[INFO ] boot: ACPI RSDP at phys 0xfb7e014
[INFO ] cpu: AuthenticAMD | NX=on WP=on SMEP=on SMAP=on UMIP=on 1GiB-pages=yes
[INFO ] cpu: GDT and TSS loaded (double-fault stack on IST1)
[INFO ] cpu: IDT loaded, 256 vectors installed, #BP self-test passed
[INFO ] mem: 40 regions, 255 MiB RAM, 211 MiB usable (largest block 167 MiB), 38 MiB reclaimable
[INFO ] paging: identity map removed; direct map of 4096 MiB at 0xffff800000000000
[INFO ] mem: frame allocator ready: 209 MiB managed, 209 MiB free, 764 KiB metadata; self-test passed
[INFO ] irq: legacy PIC masked; local APIC 0 enabled (xAPIC mode, version 0x14)
[INFO ] time: TSC 2101 MHz (not invariant), APIC timer 62 MHz, tick 100 Hz
[INFO ] time: timer interrupt self-test passed (10 ticks in 100 ms)
[INFO ] paging: audit passed (W^X holds): 40 KiB r-x, 12 KiB r--, 4096 MiB rw-, 10 table pages
Omit OS — Kernel initialized successfully.
```

## Quick start

### Prerequisites

| Tool | Purpose | Debian/Ubuntu |
| --- | --- | --- |
| [rustup](https://rustup.rs) | Rust toolchain (version, components and targets are pinned in `rust-toolchain.toml` and installed automatically) | — |
| QEMU (x86-64 system emulator) | running and testing | `apt install qemu-system-x86` |
| OVMF | UEFI firmware for QEMU | `apt install ovmf` |
| GDB (optional) | kernel debugging | `apt install gdb` |

No other host tools are needed; the disk image is produced by the build driver
itself.

### Build, run, test

```bash
cargo xtask run
```

Builds the loader and kernel, assembles a bootable disk image and boots it in
QEMU with the serial console on your terminal. The kernel halts when it has
nothing left to do; press `Ctrl-C` to stop QEMU.

```bash
cargo xtask test
```

Runs the QEMU integration tests (see [Testing](#testing)).

```bash
cargo test
```

Runs the host-side unit tests.

All `xtask` commands:

| Command | Effect |
| --- | --- |
| `cargo xtask build` | Build `omit-boot.efi` and the `omit-kernel` ELF |
| `cargo xtask image` | Also write `target/omit/<profile>/omit-os.img` (GPT + FAT32 ESP) |
| `cargo xtask run` | Also boot the image in QEMU |
| `cargo xtask test` | Build and run the QEMU integration tests (TAP output) |

Options: `--release`, `--cmdline "<text>"` (image/run), `--cpu <model>`,
`--memory <size>`, `--accel <name>` (e.g. `kvm`), `--gdb` (run). Environment: `QEMU`, `OVMF_CODE` +
`OVMF_VARS` (if the firmware is not in a standard location),
`OMIT_TEST_TIMEOUT`.

### Kernel command line

Stored in `\EFI\OMIT\CMDLINE` on the boot volume; set it with `--cmdline`.

| Token | Effect |
| --- | --- |
| `qemu-exit` | Report final status through QEMU's `isa-debug-exit` device and terminate the VM. Only for use under QEMU. |
| `fault=panic` \| `pagefault` \| `stackoverflow` | Deliberately crash after initialisation, to exercise the diagnostics. Test use only. |

### Debugging with GDB

```bash
cargo xtask run --gdb
```

QEMU starts paused with a GDB server on `localhost:1234`. In another terminal:

```bash
gdb target/x86_64-unknown-none/debug/omit-kernel -ex 'target remote :1234' -ex 'hbreak _start' -ex 'continue'
```

Use hardware breakpoints (`hbreak`) for anything set before the kernel is
running: its addresses are not mapped while the firmware is still in charge.

Panic and exception reports include a backtrace of raw return addresses.
Resolve them with:

```bash
addr2line -f -C -e target/x86_64-unknown-none/debug/omit-kernel 0xffffffff80000fef
```

### Booting real hardware

`omit-os.img` is a standard GPT disk with an EFI System Partition and the
loader at the removable-media path `\EFI\BOOT\BOOTX64.EFI`, so it can be
written to a USB stick. **This has not been tested on real hardware**; expect
it to need work (the only console is the legacy COM1 serial port, and Secure
Boot must be disabled).

## Status

Milestone 0 (development environment), Milestone 1 (bootable kernel) and
Milestone 2 (memory and interrupts) are implemented.

| Component | State |
| --- | --- |
| UEFI loader: file loading, ELF validation, page tables, memory map conversion, hand-off | Working, tested in QEMU |
| Kernel entry, boot-information validation, command line | Working, tested |
| Serial console (16550, polled) and levelled logging | Working, tested |
| CPU setup: NX, write-protect, SMEP/SMAP/UMIP when available | Working, tested |
| GDT, TSS, IDT for all 256 vectors, exception handlers, double-fault stack | Working, tested (breakpoint round-trip, page fault, stack overflow) |
| Physical memory map: validation and reporting | Working, tested |
| Address space: identity map removal, live page-table W^X audit | Working, tested |
| Panic handler with location and backtrace | Working, tested |
| Physical frame allocator (buddy, orders 0-10) | Working; algorithm host-tested, kernel integration self-tested at every boot |
| Kernel heap (size classes 16 B-2 KiB, larger requests from the frame allocator); `alloc` crate available in the kernel | Working; algorithm host-tested, kernel integration self-tested at every boot |
| Page mapping: device memory (`map_mmio`), kernel pages (map, unmap with TLB flush, translate) | Working; self-tested at every boot, plus a use-after-unmap fault test |
| Interrupt-safe spinlock, interrupt handler registration and dispatch | Working, tested |
| Legacy PIC masking, local APIC (xAPIC mode) | Working, tested |
| Local APIC in x2APIC mode | Working; tested in CI under KVM (QEMU's software emulation does not offer x2APIC, so it cannot be tested without KVM) |
| Clocks: TSC monotonic clock and 100 Hz APIC timer tick, calibrated against the PIT | Working; tick rate verified against the TSC at every boot |
| ACPI table parsing, I/O APIC, device interrupts, SMP | Not started |
| Filesystem, shell, processes, user mode, drivers, Windows/Linux compatibility | Not started |

### What has been verified, and how

On QEMU 8.2.2 with OVMF, Rust 1.97.0 — locally with software emulation
(TCG), and in CI additionally under KVM on the runner's host CPU:

- 40 host unit tests (`cargo test`): boot protocol validation, memory map
  validation/merging, command line parsing, ELF parsing including malformed
  input, the buddy allocator and the heap (each including a randomised test
  against an independent model), GPT writer.
- 5 QEMU integration tests (`cargo xtask test`), in debug and release builds,
  with `-cpu max` and with `-cpu qemu64` (no 1 GiB pages, SMEP, SMAP or UMIP),
  and with 96 MiB, 256 MiB and 6 GiB of guest RAM.
- At every boot the kernel itself checks: a breakpoint exception round-trip;
  frame allocation, alignment, memory read-back and rejection of invalid
  frees; heap use through `Box`, `Vec` and `String` with nothing leaked;
  mapping, aliasing and unmapping of a page; that ten timer interrupts arrive and take 100 ms ± a factor of two
  by the TSC; and that no mapping is writable and executable.
- `cargo fmt --check` and `cargo clippy -D warnings` for host crates, kernel
  and loader.
- GDB attach and breakpoint at `_start`; `addr2line` on a panic backtrace
  (verified for the first milestone; not re-run since).
- Release disk images are byte-identical across clean rebuilds.

### Known limitations and untested behaviour

- **Never run on real hardware** or with firmware other than OVMF. KVM has
  only been exercised in CI.
- The serial port is the only console. If COM1 is absent the kernel stops
  without any visible output.
- Clock calibration needs the legacy PIT. On hardware without one the kernel
  reports a calibration timeout and stops; CPUID- or HPET-based calibration
  is future work. The TSC is used as the clock even when the CPU does not
  advertise it as invariant (a warning-worthy condition on real hardware,
  normal under emulation).
- The tick is periodic at 100 Hz even when idle; tickless idle is future
  work.
- The frame allocator manages usable RAM above 1 MiB only. Boot-reclaimable
  memory (about 38 MiB under OVMF) is not reclaimed yet.
- Device mappings are permanent, and the loader's direct map still covers
  device address ranges with ordinary caching.
- The heap rounds sizes up to powers of two, never returns small-object pages
  to the frame allocator, and has no hardening against use-after-free. A
  failed allocation in infallible `alloc` APIs panics the kernel.
- Unmapping flushes the TLB of the current CPU only (there is only one).
- Loader error paths (missing or corrupt kernel image, unsupported CPU,
  5-level paging, more than 512 GiB of RAM) are implemented but not covered by
  automated tests.
- Only the boot CPU is used.
- No Secure Boot support; the loader does not authenticate the kernel.
- No kernel address-space randomisation.
- No performance measurements have been made, and no performance claims are
  made.

## Repository layout

```text
boot/            UEFI boot loader (x86_64-unknown-uefi)
kernel/          the kernel (x86_64-unknown-none), linker script
  src/arch/      architecture-specific code (x86_64 only)
libs/bootproto/  loader <-> kernel contract: BootInfo, memory map, command line
libs/elf/        safe ELF64 program-header parser
libs/mm/         memory-management algorithms (buddy frame allocator), host-tested
tools/xtask/     build / image / run / test driver
docs/adr/        architecture decision records
.github/         CI
```

Dependency direction: `boot` and `kernel` depend on `libs/*`; nothing depends
on `boot` or `kernel`; `xtask` depends on neither and only handles their build
products.

## How it boots

1. OVMF (or any UEFI firmware) loads `\EFI\BOOT\BOOTX64.EFI`, the loader.
2. The loader reads `\EFI\OMIT\KERNEL.ELF`, checks it, copies its segments
   into fresh memory and builds new page tables: kernel image with exact
   per-segment permissions, a guarded boot stack, and a direct map of physical
   memory.
3. It exits UEFI boot services, converts the firmware memory map into a
   firmware-independent format, switches page tables and jumps to `_start`.
4. The kernel initialises, in order: serial console, boot-information
   validation, CPU protection features, GDT/TSS, IDT (verified with a
   breakpoint round-trip), memory map validation, identity-map removal, the
   physical frame allocator (self-tested), the interrupt controllers, clock
   calibration and the timer tick (verified against an independent clock),
   and finally a page-table audit.
5. Only if every step succeeded does it print
   `Omit OS — Kernel initialized successfully.`

The exact contract between loader and kernel is documented in
[`libs/bootproto`](libs/bootproto/src/lib.rs). Design rationale is in the ADRs:

- [ADR 0001 — Own UEFI loader, ELF kernel in the higher half](docs/adr/0001-uefi-loader-and-elf-kernel.md)
- [ADR 0002 — Boot protocol and initial address space](docs/adr/0002-boot-protocol-and-initial-address-space.md)
- [ADR 0003 — Toolchain, dependency policy and test harness](docs/adr/0003-toolchain-dependencies-and-test-harness.md)
- [ADR 0004 — Physical memory management](docs/adr/0004-physical-memory-management.md)
- [ADR 0005 — Interrupts and timekeeping](docs/adr/0005-interrupts-and-timekeeping.md)

## Testing

`cargo xtask test` boots the real kernel image in QEMU once per test and
checks both QEMU's exit status and the serial log:

| Test | Checks |
| --- | --- |
| `boot` | Every initialisation step (including the frame-allocator and timer-interrupt self-tests) logs success, the success line is printed, exit status reports success, no panic/exception/error/warning output |
| `panic-diagnostics` | A deliberate panic prints message, source location and backtrace, and reports failure |
| `use-after-unmap-faults` | A page is mapped, used, unmapped and read again: the read must fault, proving the mapping and its TLB entry are gone |
| `page-fault-diagnostics` | Reading address `0x1000` raises a page fault with the right address and cause — which also proves the identity map is gone |
| `stack-overflow-double-fault` | Overflowing the boot stack hits the guard page and is caught as a double fault on its dedicated stack |

Serial logs are kept in `target/omit/<profile>/test-*.serial.log`. CI
(`.github/workflows/ci.yml`) runs formatting, lints, host tests and the QEMU
tests on every push and pull request.

## Roadmap

Development proceeds in independently verifiable milestones. The long-term
goals — reusing Linux drivers through a compatibility layer, and running
Windows executables natively — are research-heavy and far away; nothing in
this repository implements them yet.

**Next step (Milestone 3):** device interrupts (ACPI MADT, I/O APIC), serial
and keyboard input, a virtual filesystem with a RAM filesystem, and an
interactive shell with real `ls`, `cd`, `pwd`, `cat`, `mkdir` and `echo`.

## Licence

GPL-2.0-only; see [`LICENSE`](LICENSE). The licence choice and the project
name are provisional. Third-party code is listed in
[`docs/THIRD_PARTY.md`](docs/THIRD_PARTY.md).
