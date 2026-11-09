# ADR 0006: Terminal, virtual filesystem and built-in shell

- Status: accepted
- Date: 2026-11-09

## Context

Milestone 3 asks for a working interactive terminal with real filesystem
operations, before there are processes, a scheduler or a userspace.

## Decisions

### Console input: interrupt-driven serial

- The serial port is the terminal. Its receive interrupt (ISA IRQ 4) is routed
  through the **I/O APIC**, which is located by parsing the ACPI **MADT**,
  including interrupt source overrides. The alternative, polling the UART from
  the timer tick, needs no ACPI code but adds up to 10 ms of latency, wastes
  wake-ups, and would have to be thrown away for every later device.
- ACPI parsing (`libs/acpi`) is limited to the static tables needed for this:
  RSDP, RSDT/XSDT and MADT, with lengths and checksums validated. There is no
  AML interpreter, and none is planned until something requires it.
- Received bytes go into a fixed 256-byte queue filled by the interrupt
  handler; `console::read_byte` halts the CPU until one is available.
  Overflow drops bytes and counts them; the count is reported.
- **No PS/2 or USB keyboard driver yet.** The kernel has no display output,
  so a keyboard would have nowhere to echo to. It will arrive together with
  the framebuffer console.

### Filesystem

- `libs/vfs` separates the `FileSystem` trait (inode- and component-level
  operations a concrete filesystem implements) from `Vfs` (path-based
  operations and the rules common to all filesystems).
- `RamFs` stores files as byte vectors and directories as sorted maps in one
  inode table on the kernel heap. File growth uses fallible allocation, so a
  full heap is reported as "No space left on device" rather than a panic.
- Paths handed to the VFS must be absolute and normalised; `path::normalize`
  resolves `.` and `..` lexically, which is exact while there are no symbolic
  links.
- **Deliberately not there yet:** mount points, permissions and ownership,
  timestamps, hard and symbolic links, and open-file handles. Each will be
  added with the feature that needs it (persistent storage in Milestone 5,
  processes and file descriptors in Milestone 4), rather than as unused
  scaffolding now.

### Shell

- `shell/` (`omit-shell`) is a line editor, a command-line parser (quoting,
  escapes, `>`/`>>`) and a set of built-in commands implemented directly on
  the VFS: `help echo pwd cd ls cat mkdir touch cp mv rm uname clear exit`.
- It is an interim tool. Once programs can be executed, file utilities are
  expected to become real executables (the plan is uutils coreutils) and the
  shell to shrink to what a shell must do itself.
- `ps` and `kill` exist only as commands that say process management is not
  implemented. They are not simulated.
- The shell knows nothing about the kernel: bytes in, `fmt::Write` out, a
  `Vfs` to operate on. The kernel-side glue is about forty lines.

### Testability

All three crates are `no_std` + `alloc`, contain no `unsafe`, and are
unit-tested on the host. End to end, `cargo xtask test` boots the kernel,
waits for the shell prompt, types a session over the emulated serial port and
checks the output of every command.

## Consequences

- The RAM filesystem is created by, and owned by, the shell's run loop. A
  kernel-wide filesystem object needs a lock that may be held across blocking
  operations, i.e. a sleeping mutex, which needs the scheduler. Until then the
  kernel has exactly one filesystem user.
- A command runs to completion on the boot stack with interrupts enabled; it
  cannot be interrupted with Ctrl-C.
- Machines without an I/O APIC or without ACPI cannot boot this kernel. All
  x86-64 machines of the last fifteen years have both.
- Only the first I/O APIC is used, and its 8-bit destination field limits the
  target CPU to APIC IDs below 255, which is not a constraint for one CPU.
