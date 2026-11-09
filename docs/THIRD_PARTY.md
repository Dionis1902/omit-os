# Third-party code

Omit OS itself is licensed under **GPL-2.0-only** (see [`LICENSE`](../LICENSE)).
The licence choice is provisional; it was made with future reuse of GPLv2
Linux driver code in mind. No Linux, Windows, Wine or Proton code is present
in this repository.

This file lists every third-party crate in `Cargo.lock`, where it ends up, and
its licence. Regenerate the list with `cargo metadata` when dependencies
change.

## Linked into the boot loader (`omit-boot`)

| Crate | Licence | Notes |
| --- | --- | --- |
| `uefi`, `uefi-raw`, `uefi-macros` | MIT OR Apache-2.0 | UEFI bindings; used under MIT |
| `uguid` | MIT OR Apache-2.0 | used under MIT |
| `ucs2` | MPL-2.0 | UCS-2 string handling for UEFI; MPL-2.0 permits combination with GPL-2.0 (§3.3, "Secondary Licenses") |
| `bit_field` | Apache-2.0 OR MIT | used under MIT |
| `bitflags` 2.x | MIT OR Apache-2.0 | used under MIT |
| `cfg-if` | MIT OR Apache-2.0 | used under MIT |
| `log` | MIT OR Apache-2.0 | used under MIT |
| `ptr_meta` | MIT | |

Build-time only (procedural macros, not linked into any shipped binary):
`ptr_meta_derive` (MIT), `proc-macro2`, `quote`, `syn`
(MIT OR Apache-2.0), `unicode-ident` ((MIT OR Apache-2.0) AND Unicode-3.0).

## Linked into the kernel (`omit-kernel`)

None.

## Host tooling only (`xtask`; never part of the OS image)

| Crate | Licence |
| --- | --- |
| `fatfs` | MIT |
| `bitflags` 1.x | MIT OR Apache-2.0 |
| `byteorder` | Unlicense OR MIT |
| `log` | MIT OR Apache-2.0 |

## Not redistributed

QEMU and the OVMF/EDK II firmware are used for testing but are neither
included in this repository nor in the generated disk image.
