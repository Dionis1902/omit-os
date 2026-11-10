# ADR 0016: Installing to a disk

- Status: accepted
- Date: 2026-11-10

## Context

Omit OS could only be started from its live image. "Installing" meant
copying that image onto a disk with another operating system. The system
itself needs a way to put a bootable copy of itself on a disk, with a
place for data that survives restarts.

## Decision

**`install DEVICE --erase` writes the running system to a whole disk.**
What is installed is exactly what is running: the same boot loader,
kernel, initial ramdisk and kernel command line. Nothing is downloaded or
chosen. The system still runs from memory after booting from the disk —
as it does from the live image — and a separate partition holds persistent
data.

**Layout.** A GUID partition table with two partitions: an EFI System
Partition (48 MiB, FAT16) holding the four boot files at the paths the
loader already uses, and an "Omit OS data" partition (the rest, ext2)
with its own partition type. At start-up the kernel reads the partition
table of the VirtIO disk, exposes partitions as block devices (`vda1`,
`vda2`), and mounts a data partition at `/data` if there is one.

**Where the files come from.** The boot loader and the kernel image are
part of the initial ramdisk (`/boot`), so a running system can always
reproduce its boot medium; the ramdisk itself is still in memory where
the loader put it. The kernel image on boot media is the ELF file reduced
to its load segments: the debugging sections of a debug build made the
file larger than the RAM filesystem can hold (see Limitations), and no
loader needs them.

**Three small libraries rather than one large driver.**

- `libs/gpt` builds and parses partition tables. It replaced the image
  builder's private copy, so the same code lays out the boot image and
  the installed disk.
- `libs/fat` writes a FAT16 filesystem *with a known set of files in one
  pass* — boot sector, allocation tables, directories, then file data in
  consecutive clusters. An installer needs nothing more, and it is a
  fraction of a general FAT driver. The firmware is the only reader.
- The data partition uses the existing ext2 code.

**Order of writes.** The system partition is written before the partition
table, and the backup table before the primary one. Until the last step
the disk still shows whatever it showed before; an interrupted
installation does not leave a table pointing at half-written partitions.

**Safety interlocks.** The command needs the literal `--erase`; it refuses
a partition, a disk that does not exist, a disk with a mounted
filesystem (the kernel's notion of "in use" now covers a disk and its
partitions together), and a disk smaller than 64 MiB.

## Verification

- `libs/gpt`: round trip, both headers and checksums against the
  specification's layout, damaged and hostile tables (a forged header
  with a valid checksum found an integer overflow during development).
- `libs/fat`: every filesystem is read back with the independent `fatfs`
  crate — file contents, directory listings, allocation accounting —
  across the FAT16 size range and at the edges (empty files, exact
  cluster multiples, directories spanning clusters).
- In QEMU (`install-to-disk`): the system installs itself onto a blank
  VirtIO disk, writes files to the new data partition, and the machine is
  then started **from that disk alone**. It must boot, mount `/data`, find
  the files, run a program stored there and a Windows program. Afterwards
  the test harness checks the disk on the host without using the kernel's
  code: the partition table is parsed, each boot file is read from the
  FAT partition with `fatfs` and compared byte for byte with the build
  products, and `e2fsck` checks the data partition.

## Limitations

- **Only VirtIO and NVMe disks** (NVMe since ADR 0017; when this was
  first written, only VirtIO). SATA disks and USB storage cannot be
  accessed. Installation on a real machine has never been attempted.
- Tested only under QEMU with OVMF firmware, which boots the default
  loader path from the disk. Firmware boot entries are not created.
- No dual boot, no resizing, no partitioning choices: the whole disk is
  taken.
- No updates in place: installing again erases the data partition too.
- The installed system still unpacks its programs into memory at every
  start; only `/data` is persistent. A root filesystem on disk is a
  separate step.
- The RAM filesystem stores each file in one allocation, which limits a
  file to 4 MiB. That is why the kernel image is stripped; it also means
  a larger file cannot be copied into `/` at all (it can on `/data`).
- Identifiers of the disk and partitions come from the clocks, not from a
  random number generator.
