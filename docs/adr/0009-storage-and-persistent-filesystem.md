# ADR 0009: Storage: PCI, VirtIO block and ext2

- Status: accepted
- Date: 2026-11-10

## Decisions

### PCI

- Devices are found by scanning all buses through the legacy configuration
  ports (0xCF8/0xCFC). That reaches the first 256 bytes of configuration
  space everywhere, which is all current drivers need. ECAM (via the ACPI
  MCFG table) is deferred until a driver needs extended capabilities.
- BARs are used as assigned by firmware; the kernel does not allocate PCI
  address space.

### VirtIO block driver

The first storage driver targets the device QEMU and most hypervisors
provide. It is written to be obviously correct rather than fast:

| Aspect | Now | Later |
| --- | --- | --- |
| Completion | polling the used ring, yielding between polls | MSI-X interrupts |
| Concurrency | one request at a time | a full queue of requests |
| Data path | copy through one bounce page (8 sectors per request) | scatter-gather straight from the caller's pages |
| Features | `VERSION_1` only | flush, discard, multi-queue |

None of this changes the `BlockDevice` interface above the driver, so the
filesystem is unaffected when the driver improves. NVMe, the storage target
for real hardware, will sit behind the same interface.

### Filesystem: ext2

| Option | For | Against |
| --- | --- | --- |
| Custom format | freedom | no independent tools; every bug is ours to find alone |
| FAT32 | universal interchange | no permissions or links to grow into; long-name handling is fiddly |
| **ext2** | simple, documented, room to grow (ext3/4 features), and **checkable with `e2fsck`/`debugfs`** | no journal |
| ext4 | modern | far larger: extents, journal, checksums |

ext2 was chosen chiefly for verifiability. The driver's test suite runs
`e2fsck -f` over images it has written, reads files written by `debugfs`,
has `debugfs` read files it wrote, and modifies filesystems created by
`mke2fs` at all three block sizes. That caught a real bug during development
(a deletion time that `e2fsck` read as an orphan-list link) that no
self-consistency test would have found. The QEMU test does the same end to
end: the kernel formats and fills a VirtIO disk, the machine reboots and
reads it back, and `e2fsck` then checks the disk image on the host.

The driver (`libs/ext2`) is `no_std`, has no `unsafe`, and implements the
same `FileSystem` trait as the RAM filesystem.

### Mount points

The VFS now holds a list of mounted filesystems and routes each path to the
one with the longest matching mount point. Renames across filesystems are
refused (`EXDEV`); mount points cannot be removed or renamed; a filesystem
with another mounted beneath it cannot be unmounted. The root stays a RAM
filesystem populated from the initial ramdisk.

## Known gaps

- **No journal and no cache.** Every metadata change is written through
  immediately, but an operation is several writes; a crash or power loss in
  the middle can leave the filesystem inconsistent until `e2fsck` repairs
  it. There is also no "mounted" marker written to the superblock.
- **Slow.** No block cache, one 4 KiB request at a time, polling.
- Timestamps are not maintained (there is no wall clock); ownership and
  permissions are stored but ignored; no symbolic or hard links; no
  triple-indirect blocks.
- Unmounting does not wait for or refuse on open files; descriptors into an
  unmounted filesystem simply start failing.
- Only the first VirtIO block device is used, and there is no partition
  table support: the filesystem occupies the whole device.
- There is still no installer. The pieces it needs now exist (a disk
  driver, `mkfs`, a persistent filesystem), but booting from the installed
  disk needs partitioning, a FAT ESP writer and a root-on-disk boot path.
- uutils coreutils have not been ported: they need the Rust standard
  library, which requires an Omit OS target for `std`. That is a project of
  its own.
