# Linux drivers used by Omit OS

The files below are taken **unmodified** from the Linux kernel and compiled
against the Omit OS LinuxKPI (`compat/linux-kpi/`). They keep their original
copyright notices and licences. Do not edit them: fixes belong upstream or
in the compatibility layer.

| File | Upstream | Version | Licence | SHA-256 |
| --- | --- | --- | --- | --- |
| `watchdog/i6300esb.c` | `drivers/watchdog/i6300esb.c` | Linux v6.12 | GPL-2.0-or-later | `ce08519e7df3003a0e1baa79eb21fe9f2e50312d05487cb0b400745820453107` |

## Status

| Driver | Device | Validated on |
| --- | --- | --- |
| i6300esb watchdog | Intel 6300ESB (PCI 8086:25ab) | QEMU's emulated device only: probing, start, keep-alive, stop, and expiry resetting the machine |

No driver here has been tested on physical hardware.
