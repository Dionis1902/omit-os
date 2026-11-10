# ADR 0010: Reusing Linux drivers through a LinuxKPI layer

- Status: accepted
- Date: 2026-11-10

## Context

Omit OS cannot hope to write a driver for every device. The plan from the
start has been to reuse open-source Linux drivers without running a Linux
kernel. Linux deliberately has no stable in-kernel driver interface, so
"reuse" has to mean: implement, on top of native Omit OS facilities, the
part of the Linux kernel API a given driver uses, and compile the driver
against it. FreeBSD does this for graphics and wireless drivers under the
same name, LinuxKPI.

This ADR records the first working instance and what it does and does not
show.

## Decision

- `compat/linux-kpi/include/linux/*.h` are **our own** headers, written for
  Omit OS, declaring the Linux functions, macros and structures a driver
  uses. No Linux header is copied.
- The functions are implemented in Rust (`kernel/src/linuxkpi/`) as
  `extern "C"` symbols over native PCI, memory-mapping, heap and logging
  code. The one exception is printf-style formatting, done in a small C
  file because stable Rust cannot define variadic functions.
- Driver sources live in `drivers/linux/`, **unmodified**, with their
  upstream path, version, licence and SHA-256 recorded. They are compiled by
  the kernel's build script with clang or gcc using the kernel's own code
  model and ABI restrictions, and linked statically.
- Structures shared by C and Rust are mirrored by hand. To keep that
  honest, a C file exports their sizes and key offsets and the kernel
  compares them with Rust's at start-up, refusing to run Linux drivers on a
  mismatch.
- Where Linux exposes a subsystem to user space (here: watchdog devices),
  the compatibility layer exposes it to the rest of the kernel instead.
- A small registry records which driver, native or Linux, is bound to which
  device (`lsdrv`), and Linux drivers can be unbound, which calls their
  `remove` function and frees their managed memory.

### First driver: i6300esb

`drivers/watchdog/i6300esb.c` from Linux v6.12 (353 lines, GPL-2.0-or-later)
drives the Intel 6300ESB watchdog timer, which QEMU emulates. It was chosen
because it is small but real: it matches a PCI ID table, enables the device,
maps a BAR, reads and writes configuration space and device registers, uses
`devm` allocation, logs, and registers with a subsystem. It needs fifteen
kernel functions and a handful of macros.

What has been demonstrated, in QEMU:

- the driver's own probe message appears
  (`initialized. heartbeat=30 sec (nowayout=0)`);
- it can be started, stopped and reprogrammed;
- with the timeout at 6 s, the machine stays up for well over 6 s while
  keep-alives are sent;
- started and left alone, the hardware resets the machine;
- unbinding detaches it.

### Second driver, from another subsystem: pvpanic

`drivers/misc/pvpanic/` from Linux v6.12 (three files, about 290 lines,
GPL-2.0-or-later) lets a virtual machine tell its hypervisor that the
guest kernel has panicked. It was added to find out how much of the layer
was specific to the first driver. It uses none of the watchdog interface
and a different slice of the kernel API: linked lists, spinlocks, the
panic notifier chain, power-off ("sys-off") handlers, managed clean-up
actions, device attributes with `show`/`store` functions, strict number
parsing, and a driver split over two modules. All three files compile
unmodified; the layer grew by thirteen headers and about 400 lines of
Rust. Nothing written for the first driver had to change, only to be
extended (one new field in `struct pci_driver`).

What that required of the native kernel is as instructive as the layer:

- **PCI resource assignment.** The device's register block is two bytes
  long, and the firmware (OVMF) leaves such a BAR unassigned. Like Linux,
  the kernel now assigns addresses to memory BARs the firmware skipped —
  by a simple rule (directly above the highest firmware-assigned region
  on the root bus, checked against the memory map), because the proper
  source of the bridge window is ACPI byte code the kernel cannot read.
- **Panic hook.** The panic handler calls the Linux panic notifier chain
  after printing its own report.
- **Attributes without sysfs.** There is no sysfs; the attributes a driver
  declares are reachable through the shell's `devattr` command, which
  calls the driver's own `show` and `store`.

Demonstrated in QEMU: the driver binds; `capability` and `events` read
back the device's values; writing `events` goes through the driver's
validation (bad values are rejected with its error code); unbinding runs
the driver's clean-up action; and a kernel panic makes QEMU — started
with `-action panic=exit-failure` — terminate with the status reserved
for a guest panic, which only the driver's write to the device can cause.

### Hardware database

`libs/hwdb` maps PCI IDs to names and support status and is what `lspci`
prints. An entry may claim a driver only if that driver is in the tree and
has been validated; a unit test enforces that the two fields agree.

## What this does not show

- **Two small drivers (about 640 lines together), one bus, no interrupts,
  no DMA, no sleeping, no timers, no work queues.** Those are exactly the areas where
  a compatibility layer gets hard: Linux's locking rules, memory allocation
  contexts, the device model, power management and the network or block
  stacks. Network, storage, USB and above all GPU drivers need orders of
  magnitude more interface, and each of those is its own project.
- **Emulation only.** Nothing here has run on physical hardware.
- **No loadable drivers and no driver packages.** Drivers are compiled into
  the kernel. A package manager with signature verification, versioning and
  rollback presupposes a loadable-module mechanism and a stable boundary to
  load against; neither exists yet. The automatic "track upstream, patch,
  build, test, publish" pipeline is likewise future work.
- **Maintenance cost is real.** A driver taken from a different Linux
  version may use different interfaces; the layer has to be extended and
  re-validated per driver and per version.
- `pci_request_region` and `pci_disable_device` are permissive no-ops, and
  `iounmap` does not unmap: adequate while one driver is bound per device
  and mappings are permanent, wrong once resources can conflict.

## Licensing

The reused driver is GPL-2.0-or-later and the kernel is GPL-2.0-only, so the
combination is distributable under GPL-2.0. Each Linux file keeps its
copyright notices unchanged; provenance is tracked in
`drivers/linux/README.md` and `docs/THIRD_PARTY.md`.
