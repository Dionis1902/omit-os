# Linux drivers used by Omit OS

The files below are taken **unmodified** from the Linux kernel and compiled
against the Omit OS LinuxKPI (`compat/linux-kpi/`). They keep their original
copyright notices and licences. Do not edit them: fixes belong upstream or
in the compatibility layer. The project's manifest of imported code is
[`PROVENANCE.md`](../../PROVENANCE.md); every file here must be listed
there (CI checks it).

| File | Upstream | Version | Licence | SHA-256 |
| --- | --- | --- | --- | --- |
| `watchdog/i6300esb.c` | `drivers/watchdog/i6300esb.c` | Linux v6.12 | GPL-2.0-or-later | `ce08519e7df3003a0e1baa79eb21fe9f2e50312d05487cb0b400745820453107` |
| `misc/pvpanic/pvpanic.c` | `drivers/misc/pvpanic/pvpanic.c` | Linux v6.12 | GPL-2.0-or-later | `ee907764d7540598b2189a43904c161c9dfb8575c24cc939c4ca54c5aba34659` |
| `misc/pvpanic/pvpanic-pci.c` | `drivers/misc/pvpanic/pvpanic-pci.c` | Linux v6.12 | GPL-2.0-or-later | `9ccb6e1c335ae93ca9a485fd6ae85f23d2fddd1b61a66f14a8f12e33c00c7edd` |
| `misc/pvpanic/pvpanic.h` | `drivers/misc/pvpanic/pvpanic.h` | Linux v6.12 | GPL-2.0-or-later | `3c0b1d6d904ce1974145c5a909552bc4ca8c0d09b055d01734efd82c357a3aa9` |

## Status

| Driver | Device | Validated on |
| --- | --- | --- |
| i6300esb watchdog | Intel 6300ESB (PCI 8086:25ab) | QEMU's emulated device only: probing, start, keep-alive, stop, and expiry resetting the machine |
| pvpanic (core and PCI front end) | QEMU pvpanic (PCI 1b36:0011) | QEMU only: probing, the `capability` and `events` attributes (read and write, including rejected values), unbinding, and a kernel panic being reported to the hypervisor. The shutdown event is implemented but untested: the QEMU version used (8.2) does not offer it |

No driver here has been tested on physical hardware.
