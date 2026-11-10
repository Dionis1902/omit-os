# Omit OS

> Everything you need. Nothing you don't.

Omit OS is a from-scratch operating system for x86-64 with its own kernel,
written in Rust. It is not a Linux distribution and contains no Linux or
Windows code.

**This is an early-stage kernel.** Today it boots under UEFI, takes control of
the CPU and the address space, manages memory, handles timer and device
interrupts, schedules threads, and runs programs as isolated user-mode
processes. You get an interactive shell with a RAM filesystem, on screen
(with a PS/2 keyboard) and on the serial console. There is no persistent
storage and no driver model yet. Programs can use files, directories,
dynamic memory and start other programs, but there are no pipes yet. See [Status](#status) for exactly what exists and what
has been tested.

![Omit OS console: status bar, boot log and shell](docs/screenshot.png)

The same session on the serial console:

```text
omit-boot: Omit OS boot loader 0.1.0 (protocol v1)
omit-boot: kernel image: 1649912 bytes
omit-boot: kernel loaded at phys 0x3e03000, entry 0xffffffff80008120; direct map 4 GiB (2 MiB pages)
omit-boot: exiting boot services

[INFO ] Omit OS kernel 0.1.0 (x86_64, release build)
[INFO ] serial: COM1 16550 UART ready (115200 8N1), loopback test passed
[INFO ] boot: protocol v1, command line ""
[INFO ] boot: kernel image 0xffffffff80000000..0xffffffff8002a000 at phys 0x3e03000 (168 KiB)
[INFO ] boot: ACPI RSDP at phys 0x577e014
[INFO ] cpu: AuthenticAMD | NX=on WP=on SMEP=unsupported SMAP=unsupported UMIP=unsupported 1GiB-pages=no
[INFO ] cpu: GDT and TSS loaded (double-fault stack on IST1)
[INFO ] cpu: IDT loaded, 256 vectors installed, #BP self-test passed
[INFO ] mem: 38 regions, 95 MiB RAM, 46 MiB usable (largest block 34 MiB), 43 MiB reclaimable
[INFO ] paging: identity map removed; direct map of 4096 MiB at 0xffff800000000000
[INFO ] mem: frame allocator ready: 45 MiB managed, 45 MiB free, 284 KiB metadata; self-test passed
[INFO ] mem: kernel heap ready: self-test passed (11 slab pages retained)
[INFO ] paging: map/unmap self-test passed
[INFO ] irq: legacy PIC masked; local APIC 0 enabled (xAPIC mode, version 0x14)
[INFO ] time: TSC 2099 MHz (not invariant), APIC timer 62 MHz, tick 100 Hz
[INFO ] time: timer interrupt self-test passed (10 ticks in 100 ms)
[INFO ] irq: ACPI lists 1 CPU(s); I/O APIC at 0xfec00000 with 24 lines
[INFO ] console: serial input enabled (GSI 4 -> vector 50)
[INFO ] paging: audit passed (W^X holds): 120 KiB r-x, 20 KiB r--, 4096 MiB rw-, 17 table pages
Omit OS — Kernel initialized successfully.
[INFO ] vfs: RAM filesystem mounted at /
Omit OS shell. Type 'help' for a list of commands.
omit:/# mkdir -p /home/user/docs
omit:/# cd /home/user
omit:/home/user# pwd
/home/user
omit:/home/user# echo hello from omit > greeting.txt
omit:/home/user# cat greeting.txt
hello from omit
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
QEMU with the serial console on your terminal. You land in the Omit OS shell;
type `help` for the commands. Press `Ctrl-C` to stop QEMU. (`xtask` runs QEMU
without a window; to see the screen, boot the image yourself as shown under
[Releases](#releases).)

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
| `cargo xtask dist` | Build the release artifacts: versioned disk image and ISO in `target/omit/dist/` |
| `cargo xtask run` | Also boot the image in QEMU |
| `cargo xtask test` | Build and run the QEMU integration tests (TAP output) |

Options: `--release`, `--cmdline "<text>"` (image/run), `--cpu <model>`,
`--memory <size>`, `--accel <name>` (e.g. `kvm`), `--iso` (use an ISO instead of
a disk image), `--gdb` (run). Environment: `QEMU`, `OVMF_CODE` +
`OVMF_VARS` (if the firmware is not in a standard location),
`OMIT_TEST_TIMEOUT`.

### Kernel command line

Stored in `\EFI\OMIT\CMDLINE` on the boot volume; set it with `--cmdline`.

| Token | Effect |
| --- | --- |
| `noshell` | Do not start the interactive shell after initialisation. |
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

## Releases

Tagged versions are published on the repository's Releases page with two
images and their checksums:

| File | Use |
| --- | --- |
| `omit-os-<version>-x86_64.iso` | virtual machines (attach as optical drive, enable EFI), optical media |
| `omit-os-<version>-x86_64.img` | USB sticks (`dd`), virtual hard disks |

Both are live systems: they run from memory and install nothing. **There is
no installer yet.**

```bash
qemu-system-x86_64 -machine q35 -m 256M -bios /usr/share/ovmf/OVMF.fd -cdrom omit-os-0.1.0-x86_64.iso -serial stdio
```

To cut a release, set the version in `Cargo.toml`, merge, then push a
matching tag:

```bash
git tag v0.1.0 && git push origin v0.1.0
```

The release workflow tests the exact binaries from both media before
publishing. See [`docs/RELEASE_NOTES.md`](docs/RELEASE_NOTES.md).

### Real hardware

**Not tested on real hardware.** Requirements: x86-64, UEFI, Secure Boot off.
The screen should work wherever the firmware provides a 32-bit framebuffer.
Keyboard input needs a PS/2 keyboard (or firmware that emulates one for a USB
keyboard); otherwise the only way to type is a serial port (COM1, 115200 8N1).

## Status

Milestones 0 (development environment), 1 (bootable kernel), 2 (memory and
interrupts), 3 (terminal and filesystem) and 4 (processes and userspace) are
implemented, with one
addition from Milestone 8: a framebuffer text console.

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
| ACPI static tables (RSDP, RSDT/XSDT, MADT), I/O APIC | Working; parser host-tested, used at every boot |
| Interrupt-driven serial input, line editing | Working, tested end to end |
| Virtual filesystem layer and RAM filesystem | Working; host-tested, exercised end to end through the shell |
| Shell with built-in `help echo pwd cd ls cat mkdir touch cp mv rm uname clear exit`, quoting and `>`/`>>` | Working; host-tested and driven over the serial port in the QEMU tests |
| Kernel threads, preemptive round-robin scheduler, sleep, wait queues | Working; self-tested at every boot (including preemption of a thread that never yields) |
| User mode: per-process address spaces, ELF loader, `syscall`/`sysret` entry | Working; tested end to end in QEMU |
| System calls: `exit`, `write`, `read`, `open`, `close`, `map`, `unmap`, `stat`, `readdir`, `mkdir`, `remove`, `rename`, `spawn`, `wait`, `getpid`, `yield`, `sleep`, `uptime` | Working, tested |
| Heap for programs: the Rust `alloc` crate works in user space | Working, tested (1 M-element vector, 5000-node map, repeated 8 MiB blocks) |
| Output redirection (`>`, `>>`) for programs | Working, tested |
| Program arguments, standard input (line-edited, Ctrl-D ends input), per-process working directory and open files | Working, tested |
| Kernel-wide root filesystem behind a sleeping mutex, shared by the shell and by programs | Working, tested |
| Fault containment: a crashing program is terminated, the system continues | Working, tested (bad pointers to the kernel, direct kernel memory access) |
| Shell: run programs from `/bin` or by path, `&` for background, `ps`, `kill`, `free` | Working, tested |
| Userspace runtime (`omit-rt`) and programs `cat`, `echo`, `tee`, `ls`, `run`, `hello`, `sleeper`, `spin`, `crash`, `memtest`, `fileops` | Working; shipped in the initial ramdisk |
| Framebuffer console with status bar (text only) | Working; verified by screenshot in the QEMU tests |
| PS/2 keyboard (US layout) | Working; verified by injected key presses in the QEMU tests |
| USB (and therefore USB keyboards) | Not started |
| Bootable ISO, release workflow | ISO boot tested in QEMU; the release workflow itself has not run yet (it needs a tag) |
| Pipes, input redirection, environment variables, signals, multi-threaded processes | Not started |
| SMP | Not started |
| Persistent storage, drivers, Windows/Linux compatibility | Not started |

### What has been verified, and how

On QEMU 8.2.2 with OVMF, Rust 1.97.0 — locally with software emulation
(TCG), and in CI additionally under KVM on the runner's host CPU:

- 111 host unit tests (`cargo test`): boot protocol validation, memory map
  validation/merging, command line parsing, ELF parsing including malformed
  input, the buddy allocator and the heap (each including a randomised test
  against an independent model), ACPI table parsing, the VFS and RAM
  filesystem, the shell's parser, line editor and every command, the terminal
  emulator and keyboard decoder, GPT and ISO writers.
- 7 QEMU integration tests (`cargo xtask test`), in debug and release builds,
  with `-cpu max` and with `-cpu qemu64` (no 1 GiB pages, SMEP, SMAP or UMIP),
  with 96 MiB to 6 GiB of guest RAM, and booted from both the disk image and
  the ISO.
- At every boot the kernel itself checks: a breakpoint exception round-trip;
  frame allocation, alignment, memory read-back and rejection of invalid
  frees; heap use through `Box`, `Vec` and `String` with nothing leaked;
  mapping, aliasing and unmapping of a page; thread scheduling including
  preemption; that ten timer interrupts arrive and take 100 ms ± a factor of two
  by the TSC; and that no mapping is writable and executable.
- `cargo fmt --check` and `cargo clippy -D warnings` for host crates, kernel
  and loader.
- GDB attach and breakpoint at `_start`; `addr2line` on a panic backtrace
  (verified for the first milestone; not re-run since).
- Release disk images are byte-identical across clean rebuilds.

### Known limitations and untested behaviour

- **Never run on real hardware** or with firmware other than OVMF. KVM has
  only been exercised in CI.
- The kernel requires a working COM1 serial port even when a screen is
  present, and stops silently without one.
- The screen is a text console only: fixed font, ASCII, no graphics or
  windows. Debug builds draw noticeably slowly under emulation.
- Only PS/2 keyboards with a US layout. No USB, no mouse.
- There is no installer and no persistent storage: every boot starts from an
  empty RAM filesystem.
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
- The filesystem lives in RAM and is empty at every boot. It has no
  permissions, timestamps, links or mount points, and is owned by the shell
  rather than being a kernel-wide object (see ADR 0006).
- Built-in shell commands cannot be interrupted, there are no pipes,
  variables or globs, and `cp` does not copy directories.
- There are no pipes, no input redirection and no environment variables.
  Open files are tracked by path, so renaming a file that is open confuses
  its descriptor. Memory a program maps is never shared and address space
  in the map area is not reused. The system call ABI is unstable.
- The shell's own file commands (`ls`, `cat`, ...) are still built in and
  take precedence over the programs of the same name in `/bin`.
- The scheduler is plain round-robin on one CPU with a 10 ms slice; it has
  no priorities and has not been measured.
- Panic and exception backtraces only cover the boot thread's stack.
- Machines without ACPI or an I/O APIC are rejected at boot.
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
libs/abi/        system call numbers and error codes shared by kernel and userspace
libs/acpi/       ACPI static table parser (RSDP, RSDT/XSDT, MADT), host-tested
libs/elf/        safe ELF64 program-header parser
libs/mm/         memory-management algorithms (buddy frame allocator), host-tested
libs/term/       terminal emulator and keyboard scancode decoder, host-tested
libs/vfs/        virtual filesystem layer and RAM filesystem, host-tested
shell/           the built-in shell (line editor, parser, commands), host-tested
userspace/rt/    runtime for user programs: entry point, system calls, printing
userspace/programs/  sample user programs, shipped in the initial ramdisk
tools/xtask/     build / image / run / test driver
docs/adr/        architecture decision records
.github/         CI
```

Dependency direction: `boot` and `kernel` depend on `libs/*`; `shell` depends
on `libs/vfs` and is used by `kernel`; nothing depends
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
   ACPI and the I/O APIC, serial input, and finally a page-table audit.
5. Only if every step succeeded does it print
   `Omit OS — Kernel initialized successfully.`
6. It then mounts a RAM filesystem, unpacks the initial ramdisk (user
   programs) into `/bin`, and runs the shell on the console.

The exact contract between loader and kernel is documented in
[`libs/bootproto`](libs/bootproto/src/lib.rs). Design rationale is in the ADRs:

- [ADR 0001 — Own UEFI loader, ELF kernel in the higher half](docs/adr/0001-uefi-loader-and-elf-kernel.md)
- [ADR 0002 — Boot protocol and initial address space](docs/adr/0002-boot-protocol-and-initial-address-space.md)
- [ADR 0003 — Toolchain, dependency policy and test harness](docs/adr/0003-toolchain-dependencies-and-test-harness.md)
- [ADR 0004 — Physical memory management](docs/adr/0004-physical-memory-management.md)
- [ADR 0005 — Interrupts and timekeeping](docs/adr/0005-interrupts-and-timekeeping.md)
- [ADR 0006 — Terminal, virtual filesystem and built-in shell](docs/adr/0006-terminal-filesystem-and-shell.md)
- [ADR 0007 — Framebuffer console, keyboard input and releases](docs/adr/0007-display-keyboard-and-releases.md)
- [ADR 0008 — Threads, processes and system calls](docs/adr/0008-threads-processes-and-system-calls.md)

## Testing

`cargo xtask test` boots the real kernel image in QEMU once per test and
checks both QEMU's exit status and the serial log:

| Test | Checks |
| --- | --- |
| `boot` | Every initialisation step (including the frame-allocator and timer-interrupt self-tests) logs success, the success line is printed, exit status reports success, no panic/exception/error/warning output |
| `shell-session` | Boots into the shell and types about 40 commands over the serial port, checking the output of each: directories, files, redirection, copy/move/remove, error messages, line editing; then runs user programs — a greeting, one that passes bad pointers and reads kernel memory (must be terminated), one that sleeps and returns a status, one that spins forever in the background (must show up in `ps` and die on `kill`), and `echo`/`cat`/`tee` exercising arguments, relative paths, file reads and writes, error reporting and typed standard input |
| `display-and-keyboard` | Types a command on the emulated PS/2 keyboard and checks it ran; takes a screenshot and checks the status bar and console colours |
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

**Next step (Milestone 5):** PCI device discovery, a VirtIO block driver and
a persistent filesystem, which is also what an installer needs.

## Licence

GPL-2.0-only; see [`LICENSE`](LICENSE). The licence choice and the project
name are provisional. Third-party code is listed in
[`docs/THIRD_PARTY.md`](docs/THIRD_PARTY.md).
