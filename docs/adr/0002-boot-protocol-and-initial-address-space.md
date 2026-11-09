# ADR 0002: Boot protocol and initial address space

- Status: accepted
- Date: 2026-11-09

## Context

The loader and the kernel are separate binaries (ADR 0001) and need a precise
contract: CPU state at entry, what is mapped where, and how machine
information is passed. The layout chosen now constrains the virtual memory
manager, user mode and SMP work that follow.

## Decision

The contract lives in one crate, `libs/bootproto` (`omit-bootproto`), which
both sides compile against. Its crate-level documentation is the normative
text; this ADR records the reasoning.

### Address space at hand-off (4-level paging)

| Range | Contents | Permissions |
| --- | --- | --- |
| `0x0000_0000_0000_0000` + N GiB | Identity map of physical memory (temporary) | RWX |
| `0xFFFF_8000_0000_0000` + N GiB | Direct map of physical memory | RW, NX |
| `0xFFFF_FFFF_7FFE_F000`..`7FFF_F000` | Boot stack, 64 KiB, unmapped guard pages on both sides | RW, NX |
| `0xFFFF_FFFF_8000_0000`.. | Kernel image | per ELF segment: R-X, R--, RW- |

- N is physical RAM rounded up to 1 GiB, at least 4 GiB, at most 512 GiB (one
  PML4 slot). 1 GiB pages are used when the CPU has them, 2 MiB pages
  otherwise.
- **A direct map** makes physical-to-virtual translation a single addition,
  which keeps page-table manipulation and, later, DMA and page-cache code
  cheap. The alternative, recursive page tables, saves no memory worth
  having and complicates address-space switching.
- **The identity map exists only so the loader survives its own CR3 switch.**
  It shares lower-level tables with the direct map and is removed by the
  kernel during initialisation, after which the whole lower half (including
  the null page) faults. The kernel audits the live tables afterwards.
- **W^X from the first instruction.** The loader rejects any kernel segment
  that is both writable and executable, maps each segment with exactly its
  ELF permissions, enables `EFER.NXE`, and makes the direct map
  non-executable. The kernel additionally sets `CR0.WP` and, when available,
  SMEP, SMAP and UMIP.

### Information passing

`rdi` holds the direct-map address of a `#[repr(C)] BootInfo` with a magic
number and protocol version. It carries the memory map, kernel placement, the
ACPI RSDP address and the kernel command line.

- The **memory map** is converted by the loader into a firmware-independent
  array of `(start, pages, kind)`, sorted and coalesced. `kind` is a
  transparent integer, not a Rust `enum`, so a mismatched loader can never
  hand the kernel an invalid enum value (which would be undefined behaviour
  before any validation could run).
- The kernel **validates** the structure (`BootInfo::validate`,
  `memory::validate`) before using it, and refuses to boot on a version
  mismatch.
- The **command line** is the optional file `\EFI\OMIT\CMDLINE` on the boot
  volume: whitespace-separated `flag` and `key=value` tokens. It lets tests
  and, later, performance-mode policy configure the same kernel binary
  without rebuilding it.

## Consequences

- The direct map currently also covers MMIO holes below the top of RAM with
  ordinary write-back caching. Nothing accesses devices through it yet; the
  kernel's own VMM (Milestone 2) must map device memory explicitly with the
  right cache attributes.
- More than 512 GiB of RAM and 5-level paging (LA57) are rejected by the
  loader with a clear message rather than handled.
- Framebuffer information is not part of protocol v1. It will be added, with
  a version bump, when the kernel has a use for it.
- Kernel address-space layout randomisation is not implemented; the fixed link
  address is a known hardening gap to revisit once the VMM exists.
