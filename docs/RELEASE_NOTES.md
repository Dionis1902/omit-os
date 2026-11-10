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

Omit OS can install itself onto a **SATA**, **NVMe** or **VirtIO** disk.
All three drivers have been tested only with QEMU's emulated disks and
have never run on real hardware, so do not point the installer at a
machine whose data you care about.
Give the VM an empty disk of at least 64 MiB, boot the ISO, and at the
prompt:

```text
lsblk                    lists the disks: SATA disks are sda, sdb, ... (by
                         port), an NVMe disk is nvme0n1, a VirtIO disk vda
install sda --erase      writes the system; everything on sda is lost
```

Check the name and size in `lsblk` first: if you started from a disk
image rather than the ISO, that image is a disk in the list too.

Then shut the VM down, detach the ISO and start it again: it boots from
the disk. Files under `/data` are kept across restarts; everything else
is recreated from the installed image at each start.

With QEMU:

```bash
qemu-img create -f raw omit-disk.img 256M
```

```bash
qemu-system-x86_64 -machine q35 -m 256M -bios /usr/share/ovmf/OVMF.fd -cdrom omit-os-*-x86_64.iso -drive format=raw,file=omit-disk.img -serial stdio
```

After `install sda --erase`, start it again without `-cdrom ...`.

In VirtualBox or VMware, the VM's default (SATA) disk works the same
way; this has not been tried by the authors.

## Requirements

- x86-64 CPU, UEFI firmware, **Secure Boot disabled**.
- 128 MiB of RAM or more.

## Trying it

QEMU (needs the OVMF firmware package):

```bash
qemu-system-x86_64 -machine q35 -m 256M -bios /usr/share/ovmf/OVMF.fd -cdrom omit-os-*-x86_64.iso -serial stdio
```

A window shows the Omit OS console; you can type in it or in the terminal.

VirtualBox: create a VM of type "Other", version "Other/Unknown
(64-bit)", and under Settings → System → Motherboard tick **Enable EFI**
(without it VirtualBox reports that the VM "failed to boot"); attach the
ISO as an optical drive. This has been tried with VirtualBox 7 and boots
(from version 0.1.1 on). VMware: the same idea — a 64-bit "Other" VM with
UEFI firmware — but untried.

If the screen stays black, add a serial port writing to a file (COM1,
"Raw File") and look there: the system logs everything to it.

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
date                 the date and time (UTC)
winhello.exe         a Windows console program, run natively
wintest.exe          a Windows program that tests the Windows API layer
winrun --list        the Windows functions that are implemented
ls /bin | tee /list  a pipeline (programs only; see `help`)
lspci                hardware, with driver status
lsblk                disks and partitions
ifconfig             the network interface (VirtIO network cards only)
ping 10.0.2.2        echo requests to an IPv4 address
host example.org     look a name up (needs a network that reaches a name
                     server)
tftp 10.0.2.2 FILE   fetch a file from a TFTP server
fetch http://HOST/FILE [DEST]
                     download over HTTP (no https)
desktop              a graphical desktop (needs a mouse: PS/2, as virtual
                     machines provide, or USB); the power button in
                     the Start panel, or F12, returns to the console
```

On the desktop, the icons on the task bar start programs — hold the
pointer over one to see its name — and the mark at the left lists them
all; type there to search. Settings switches between light and dark and
chooses the accent colour; Files browses the folders (click a row to
select it, again to open it; the buttons at the right of its tool bar make a folder, rename (F2) and delete (Delete, twice — there is no Recycle Bin)). Drag a window by its title to the left or right edge of the screen to give it that half, to the top edge to maximise it. The Calculator takes the mouse or the keyboard. Notes is a small text editor: its icon starts a new note, a text file clicked twice in Files opens in it, Ctrl+S saves; closing the window with unsaved changes is refused once (close again to discard them), but ending the desktop does not ask. The mouse wheel scrolls the list or text under the pointer. Alt+Tab switches between windows (hold Alt, press Tab until the one you want is outlined, let go) and Alt+F4 closes the one in front. System Monitor lists what is running — select a process and press End task (or Delete) to end it — and draws the processor's and memory's last minute. Some have windows of their own (Scribble, Clock, the game Pop), and
the Run window starts anything by
name — try `wingui.exe`, a graphical Windows program: click in its
window, type, press `m` for a message box, `x` for a timer, `q` to quit.

Windows support is a first step: programs run only if everything they
import is among the implemented functions of `KERNEL32.dll`, `USER32.dll`
and `GDI32.dll` (`winrun --list`). The programs on the image were written
for it; existing Windows software does not run.

Networking is a first step too: with a VirtIO network card (QEMU:
`-netdev user,id=n -device virtio-net-pci,netdev=n`; VirtualBox: adapter
type "Paravirtualized Network (virtio-net)") the system gets an address by
DHCP; `ifconfig` and `ping` work, and programs can use UDP: `host` looks
names up and `tftp` fetches files (QEMU has a TFTP server built in:
`-netdev user,id=n,tftp=DIRECTORY`), and TCP: `fetch` downloads from
`http://` addresses. There is no TLS, so no `https`, and no other
network card has a driver. Only QEMU has been tried.

On a real computer: the keyboard must be PS/2 (as built into many
laptops) or USB. A serial port is not needed.

## Status

See the README for exactly what works, what has been tested and how, and the
known limitations. In short: the system has been tested in QEMU (software
emulation and KVM) with OVMF firmware. **It has not been tested on real
hardware.**
