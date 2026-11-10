# Third-party code

Omit OS itself is licensed under **GPL-2.0-only** (see [`LICENSE`](../LICENSE)).
The licence choice is provisional; it was made with future reuse of GPLv2
Linux driver code in mind. The repository contains no Windows, Wine or Proton code, and no
Linux code other than the individual drivers listed below.

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

| Crate | Licence | Notes |
| --- | --- | --- |
| `noto-sans-mono-bitmap` | MIT (crate); SIL Open Font License 1.1 (the embedded Noto Sans Mono glyph data) | Font for the framebuffer console. The OFL permits bundling and embedding the font in software under any licence; the font data itself stays under the OFL. |

## Linux kernel code linked into the kernel

Individual Linux device drivers are compiled, unmodified, against the Omit
OS LinuxKPI layer. The authoritative list, with upstream paths, versions and
checksums, is [`drivers/linux/README.md`](../drivers/linux/README.md).

| File | Licence | Copyright |
| --- | --- | --- |
| `drivers/linux/watchdog/i6300esb.c` | GPL-2.0-or-later | (c) 2004 Google Inc.; (c) 2005 David Härdeman; see the file header |
| `drivers/linux/misc/pvpanic/pvpanic.c`, `pvpanic-pci.c`, `pvpanic.h` | GPL-2.0-or-later | (c) 2013 Fujitsu; (c) 2018 ZTE; (c) 2021 Oracle; see the file headers |

The LinuxKPI headers and runtime in `compat/linux-kpi/` are original Omit OS
code under GPL-2.0-only. They declare interfaces compatible with Linux's but
contain no Linux source.

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
