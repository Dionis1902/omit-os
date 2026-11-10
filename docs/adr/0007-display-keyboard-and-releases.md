# ADR 0007: Framebuffer console, keyboard input and releases

- Status: accepted
- Date: 2026-11-10

## Decisions

### On-screen console

- The boot loader passes the firmware's linear framebuffer to the kernel
  (boot protocol v2). It uses whatever graphics mode the firmware selected and
  accepts only 32-bit RGB/BGR layouts; anything else means "no framebuffer".
- The screen is **optional**. Without one the kernel logs that fact and runs
  on the serial console alone.
- The console is a character grid: a status bar on the first row and
  scrolling text below. Terminal behaviour lives in `libs/term` (pure state
  machine, host-tested); the kernel only converts cells to pixels.
- Scrolling redraws the cells whose contents changed instead of copying
  framebuffer memory, because reading back from write-combined video memory
  is very slow on real hardware.
- The framebuffer is mapped write-combining through PAT entry 1.
- Glyphs come from the `noto-sans-mono-bitmap` crate (pre-rasterised Noto
  Sans Mono, 16 px, ASCII only). Drawing a font by hand would be a lot of
  unverifiable data; this is the same choice the Rust OS ecosystem's boot
  crates make. Characters outside ASCII are shown as `?`.
- Console output before the screen exists is kept in an 8 KiB buffer and
  replayed, so the screen shows the whole boot log.
- The status bar shows version, uptime, free memory and heap use. It is
  refreshed from the input wait loop, not from an interrupt handler, so no
  drawing happens in interrupt context.

This is a text console, not a graphical desktop. Windows, a compositor and
GPU acceleration (Milestone 8) are separate, much larger work.

### Keyboard

- PS/2 keyboard through the 8042 controller, using it as the firmware left
  it, with scancode set 1 decoded by `libs/term` (US layout).
- Key presses feed the same input queue as the serial port, so the shell
  does not know which one is in use.
- **No USB HID.** A USB keyboard works only where firmware keeps emulating a
  PS/2 keyboard after boot, which is common in virtual machines and
  unreliable on real hardware. On such machines the serial console remains
  the way in. A USB stack (xHCI + HID) is its own milestone.

### Test coverage

`cargo xtask test` drives both through QEMU's monitor: it presses keys on the
emulated keyboard, checks that the typed command ran, takes a screenshot and
checks the status bar and text colours pixel-exactly.

### Releases

- `cargo xtask dist` builds two artifacts from the same binaries: a GPT disk
  image (USB sticks, virtual hard disks) and an ISO 9660 image with an El
  Torito EFI boot entry (virtual machines, optical media). The ISO writer is
  about 200 lines in `tools/xtask`, so building a release needs no host tools
  beyond Cargo.
- The ISO is not a hybrid image: written raw to a USB stick it will not boot.
  Use the disk image for that.
- Pushing a tag `vX.Y.Z` that matches the Cargo version runs the release
  workflow: host tests, boot tests from both media, then a GitHub pre-release
  with the two images and their SHA-256 checksums.
- There is **no installer**. Both images are live systems that run from RAM.
  Installation needs storage drivers and a persistent filesystem
  (Milestone 5).
