Omit OS is an early-stage, from-scratch operating system. This is a
**preview for review and testing**, not something to install on a machine you
depend on.

## What you get

- `omit-os-<version>-x86_64.iso` — bootable ISO for virtual machines and
  optical media.
- `omit-os-<version>-x86_64.img` — raw disk image for USB sticks and virtual
  hard disks.
- `SHA256SUMS` — checksums of both.

Both boot the same live system: it runs entirely from memory, installs
nothing and does not touch the machine's disks. There is no installer.

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
desktop              a graphical desktop (needs a PS/2 mouse, as virtual
                     machines provide); Exit or F12 returns to the console
```

Windows support is a first step: only console programs that use a small
set of `KERNEL32.dll` functions run. Existing Windows software does not.

## Status

See the README for exactly what works, what has been tested and how, and the
known limitations. In short: the system has been tested in QEMU (software
emulation and KVM) with OVMF firmware. **It has not been tested on real
hardware.**
