Omit OS is an early-stage, from-scratch operating system. This is a
**preview for review and testing**, not something to install on a machine you
depend on.

## What you get

- `omit-os-<version>-x86_64.iso` — bootable ISO for virtual machines and
  optical media.
- `omit-os-<version>-x86_64.img` — raw disk image for USB sticks and virtual
  hard disks.
- `SHA256SUMS` — checksums of both.

Both boot the same live system: it runs entirely from memory and does not
touch the machine's disks unless you tell it to.

## Installing (tested in virtual machines only)

Omit OS can install itself onto a **VirtIO** or **NVMe** disk. Both have
been tested only with QEMU's emulated disks: the NVMe driver has never
run on a real disk, so do not point it at a machine whose data you care
about. SATA disks are not supported.
Give the VM an empty disk of at least 64 MiB, boot the ISO, and at the
prompt:

```text
lsblk                    a VirtIO disk appears as vda, an NVMe disk as nvme0n1
install vda --erase      writes the system; everything on vda is lost
```

Then shut the VM down, detach the ISO and start it again: it boots from
the disk. Files under `/data` are kept across restarts; everything else
is recreated from the installed image at each start.

With QEMU:

```bash
qemu-img create -f raw omit-disk.img 256M
```

```bash
qemu-system-x86_64 -machine q35 -m 256M -bios /usr/share/ovmf/OVMF.fd -cdrom omit-os-*-x86_64.iso -drive if=none,id=d,format=raw,file=omit-disk.img -device virtio-blk-pci,drive=d -serial stdio
```

After `install vda --erase`, start it again without `-cdrom ...`.

## Requirements

- x86-64 CPU, UEFI firmware, **Secure Boot disabled**.
- 128 MiB of RAM or more.

## Trying it

QEMU (needs the OVMF firmware package):

```bash
qemu-system-x86_64 -machine q35 -m 256M -bios /usr/share/ovmf/OVMF.fd -cdrom omit-os-*-x86_64.iso -serial stdio
```

A window shows the Omit OS console; you can type in it or in the terminal.

VirtualBox / VMware: create a 64-bit "Other" VM, **enable EFI**, attach the
ISO as an optical drive.

USB stick (this erases the stick; replace `/dev/sdX` with its device):

```bash
sudo dd if=omit-os-*-x86_64.img of=/dev/sdX bs=4M conv=fsync status=progress
```

## Things to try

At the `omit:/#` prompt:

```text
help                 the built-in commands
ls /bin              the programs on the image
hello                a native program
winhello.exe         a Windows console program, run natively
wintest.exe          a Windows program that tests the Windows API layer
winrun --list        the Windows functions that are implemented
ls /bin | tee /list  a pipeline (programs only; see `help`)
lspci                hardware, with driver status
desktop              a graphical desktop (needs a mouse: PS/2, as virtual
                     machines provide, or USB); Exit or F12 returns to the
                     console
```

Windows support is a first step: only console programs that use a small
set of `KERNEL32.dll` functions run. Existing Windows software does not.

## Status

See the README for exactly what works, what has been tested and how, and the
known limitations. In short: the system has been tested in QEMU (software
emulation and KVM) with OVMF firmware. **It has not been tested on real
hardware.**
