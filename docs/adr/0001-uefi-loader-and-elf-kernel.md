# ADR 0001: Own UEFI loader, ELF kernel in the higher half

- Status: accepted
- Date: 2026-11-09

## Context

Omit OS targets x86-64 with UEFI firmware. Something has to get from "the
firmware started an EFI application" to "the kernel is running in its own
address space with a description of the machine". The options considered:

| Option | Performance | Complexity | Security | Maintenance |
| --- | --- | --- | --- | --- |
| **A. Kernel is itself a UEFI application** (single PE binary) | Same at run time | Lowest up front | Kernel runs wherever firmware put it, in firmware's identity-mapped address space with firmware's page permissions; W^X and a higher-half layout must be retrofitted by self-relocation | Kernel tied to PE/COFF and the UEFI target; early-boot code and kernel proper are entangled |
| **B. Third-party loader** (Limine, `bootloader` crate, GRUB) | Same | Low for us, but a large external component | Depends on the loader | A boot protocol we do not control; extra licence and supply-chain surface |
| **C. Own minimal UEFI loader + ELF kernel** | Same | ~600 lines of loader | Kernel starts in a fully known address space built for it, with exact per-segment permissions | Boot protocol is ours and versioned; two small binaries with a narrow interface |

Boot-path choice has no measurable effect on steady-state performance, so the
decision rests on security, clarity and long-term cost.

## Decision

Option C.

- `boot/` (`omit-boot`) is a UEFI application for `x86_64-unknown-uefi`. It
  loads `\EFI\OMIT\KERNEL.ELF`, builds the initial page tables, exits boot
  services, converts the memory map and jumps to the kernel.
- `kernel/` (`omit-kernel`) is a statically linked ELF64 executable for
  `x86_64-unknown-none`, linked at `0xFFFF_FFFF_8000_0000`
  (`-C code-model=kernel`, `-C relocation-model=static`).
- `libs/bootproto` defines the contract between the two (ADR 0002).
- `libs/elf` is a safe, host-tested ELF64 program-header parser, used by the
  loader now and intended for the userspace program loader later.

The loader uses the `uefi` crate for firmware bindings instead of hand-written
FFI tables: the bindings are mature, maintained and exactly the kind of code
the project rules say not to rewrite. The kernel itself has no third-party
dependencies.

## Consequences

- The kernel never interprets UEFI data structures and never calls boot
  services; UEFI runtime services are not used at all for now.
- The loader tags its allocations with OS-defined UEFI memory types
  (`0x8000_0001`..) so the final memory map says exactly which pages hold the
  kernel image, boot stack, page tables and boot information. The UEFI
  specification reserves that range for OS loaders, but some very old firmware
  mishandles it. **Risk:** if real hardware shows this problem, the loader must
  instead record its allocations and overlay them on the converted map.
- Legacy BIOS boot is not supported and not planned.
- Secure Boot is not addressed yet: the loader is unsigned and does not verify
  the kernel image.
