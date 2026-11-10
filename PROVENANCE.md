# Provenance of imported code

Every file, or part of a file, in this repository that was taken from
another project is listed here. Everything not listed was written for
Omit OS. Third-party *crates*, which are downloaded at build time and not
stored in the repository, are listed in
[`docs/THIRD_PARTY.md`](docs/THIRD_PARTY.md). The rules are in
[`IP_POLICY.md`](IP_POLICY.md); CI checks that every file under
`drivers/linux/` appears below.

## Linux kernel

Unmodified files, compiled against the project's own compatibility layer
(`compat/linux-kpi/`, `kernel/src/linuxkpi/`). Upstream is
<https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git>.
Checksums are in [`drivers/linux/README.md`](drivers/linux/README.md).

| Local path | Project | Upstream path | Version | Licence |
| --- | --- | --- | --- | --- |
| `drivers/linux/watchdog/i6300esb.c` | Linux | `drivers/watchdog/i6300esb.c` | v6.12 | GPL-2.0-or-later |
| `drivers/linux/misc/pvpanic/pvpanic.c` | Linux | `drivers/misc/pvpanic/pvpanic.c` | v6.12 | GPL-2.0-or-later |
| `drivers/linux/misc/pvpanic/pvpanic-pci.c` | Linux | `drivers/misc/pvpanic/pvpanic-pci.c` | v6.12 | GPL-2.0-or-later |
| `drivers/linux/misc/pvpanic/pvpanic.h` | Linux | `drivers/misc/pvpanic/pvpanic.h` | v6.12 | GPL-2.0-or-later |

## Wine, ReactOS and other Windows reimplementations

Nothing has been imported.

## Other

Nothing has been imported.
