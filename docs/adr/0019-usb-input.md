# ADR 0019: USB keyboards and mice (xHCI)

- Status: accepted
- Date: 2026-11-10

## Context

Input came only from PS/2 devices. Laptops' built-in keyboards are often
still PS/2 behind the scenes, but desktop machines have USB keyboards
and mice only, and firmware stops emulating PS/2 for them once an
operating system takes over. Without USB the system could be booted on
such a machine but not used.

## Decision

A native driver for the xHCI host controller,
`kernel/src/drivers/xhci.rs`, written from the xHCI, USB 2.0 and HID
specifications, doing the minimum that makes keyboards and mice work:

- take the controller over from the firmware, reset it, give it its
  data structures (device context array, scratchpad pages, command
  ring, one event ring segment) and start it;
- for each device plugged directly into a port: reset the port, enable
  a slot, address the device, read its device and configuration
  descriptors over the control endpoint;
- if it has a HID *boot protocol* keyboard or mouse interface: select
  the configuration, switch the interface to the boot protocol,
  configure its interrupt endpoint and keep transfers queued on it;
- a kernel thread reads the event ring every timer tick, turns reports
  into input and handles devices being plugged in and removed.

The boot protocol's fixed report formats avoid a HID report-descriptor
parser. Keyboard reports list the keys that are down; `libs/term`'s new
`hid` module compares consecutive reports and produces the scancodes a
PC keyboard would have sent, so the existing scancode decoder — and its
one layout table — serves both kinds of keyboard. The driver also
repeats held keys, which PC keyboards do by themselves and USB
keyboards leave to the host.

No interrupts are used: like the storage drivers, the driver polls. The
cost is up to one tick (10 ms) of added input latency.

## What works

- `usb-keyboard` (QEMU): with the kernel told not to use PS/2 devices
  (`nops2`), a line typed on an emulated USB keyboard runs a command.
- `desktop-usb-input` (QEMU): the complete desktop scenario — pointer
  movement, clicks, a drag, drawing a stroke, typing into native and
  Windows programs — with USB keyboard and mouse only.
- Host tests of the report decoder: every printable key against the PC
  layout, modifiers, navigation and function keys, held keys and
  auto-repeat, malformed and roll-over reports, mouse reports.
- By hand, in QEMU: plugging a keyboard in at run time, typing,
  unplugging it and plugging in another all work. This is not part of
  the automated tests.

## Limitations

- **Tested only against QEMU's emulated controller and devices.** Real
  controllers and devices differ in timing and in how strictly they
  follow the specifications; this driver has never met one.
- Hubs (added later; up to five in a row) are asked about their ports
  twice a second over the control endpoint; their own change
  notifications are not used. Tested with QEMU's emulated hub: a
  keyboard behind one hub and a mouse behind two.
- Only boot-protocol keyboards and mice. No other HID devices
  (touchpads that are not also boot mice, tablets, game controllers),
  no mouse wheel, no keyboard LEDs, no storage or any other class.
- One controller; no power management, no USB 3 specifics beyond what
  enumeration needs.
- Memory for a device's structures is not returned when it is
  unplugged (a few pages per plug-in).
- Polling instead of interrupts; a device whose endpoint reports an
  error is not restarted until it is replugged.
- US layout only, as for PS/2.

## Alternatives considered

- **Relying on firmware PS/2 emulation.** It ends when the operating
  system starts using the machine, and newer firmware does not offer it.
- **Reusing Linux's xHCI and HID drivers through LinuxKPI.** They sit
  on the Linux USB core, which is far larger than the compatibility
  layer; a native driver of about a thousand lines is the smaller job.
- **A full HID report-descriptor parser.** Needed for devices beyond
  boot keyboards and mice; not needed to make a machine usable.
