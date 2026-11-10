# ADR 0012: Graphics for programs, mouse input and a first desktop

- Status: accepted
- Date: 2026-11-10

## Context

Until now the screen belonged to the kernel's text console. A desktop
needs three things the system did not have: a way for a program to put
pixels on the screen, keyboard and mouse events delivered to that program,
and code that turns both into windows. The goal for this step was a
desktop that is real enough to review and test the system with — move
windows, browse files, run programs and see their output — while keeping
the kernel's part small and safe.

## Decision

**The kernel offers the screen as an exclusive resource.** One process at
a time can *acquire* the display. While it holds it:

- the text console keeps recording output but stops drawing;
- key presses and mouse reports are queued for the owner
  (`input_read`, with a timeout) instead of feeding the console;
- the owner draws by handing the kernel rectangles of pixels
  (`display_blit`).

When the owner releases the display or its process ends for any reason —
exit, crash, kill — the console repaints itself. A kernel panic or fatal
exception takes the screen back first, so diagnostics are never hidden
behind a frozen desktop.

**The framebuffer is not mapped into the process.** The owner draws into
its own memory and the kernel copies. That costs one copy per update, but
the kernel never has to map device memory into user space, cannot leave a
stale mapping behind when ownership changes, and validates every rectangle
against the screen. Pixels have one format for programs (`0x00RRGGBB`);
the kernel converts if the hardware differs.

**Mouse.** A PS/2 mouse driver (standard three-byte protocol) next to the
keyboard driver, sharing the controller. The decoder lives in `libs/term`
and resynchronises after lost bytes.

**Everything else is user space.**

- `libs/gfx` — drawing (clipped fills, outlines, anti-aliased text) and a
  window manager: overlapping windows that are raised by clicking, moved
  by their title bars and closed by a button; a task bar; a pointer. It is
  driven by method calls and draws into a memory image, repainting and
  reporting only what changed. It makes no system calls, so all of it is
  tested on the build host — including a test that the incrementally
  updated image always equals a full repaint.
- `/bin/desktop` — one program that owns the display, runs the event loop
  and provides the windows: *Files* (browse directories, open files),
  *Viewer* (text files), *Run* (start a program and show its output) and
  *About*.

**Supporting system calls**, useful beyond the desktop: `spawn` can direct
the new program's output to a file the caller has open; `wait` has a
non-blocking form; `kill`.

## What works

Verified by the `desktop` QEMU test, which operates the system the way a
person would — through the emulated mouse and keyboard — and inspects
screenshots, recognising text by rendering the expected string with the
same font code and searching for it:

- the desktop appears with its task bar and a window;
- clicking a task bar button opens the Run window and moves the keyboard
  focus to it;
- typing `hello` and `winhello.exe` runs a native and a Windows program,
  and the output of each appears in the window;
- dragging the title bar moves the window with its contents;
- the Exit button ends the desktop and the text console is back, intact.

## Limitations

- **One program draws everything.** Other programs cannot open windows:
  there is no inter-process communication yet. The *Run* window is not a
  terminal — the started program cannot read the keyboard, and its error
  output goes to the text console.
- Only the firmware's framebuffer and resolution; no mode setting, no
  acceleration, no vertical-sync, one fixed-size ASCII-oriented font.
- PS/2 mouse and keyboard only (no USB), no mouse wheel, US layout.
- Windows cannot be resized, minimised or maximised.
- Input is polled once per timer tick (10 ms).
- Under software emulation in debug builds, full-screen repaints are
  visibly slow; no performance measurements have been made.
- Not tested on real hardware.

## Alternatives considered

- **Mapping the framebuffer into the display owner.** Faster, and the
  usual choice. Deferred: it needs device-memory mappings in user address
  spaces and a way to revoke them, for a gain that does not matter yet.
- **A compositor in the kernel.** Rejected: window management is policy
  and belongs in user space, where a bug costs one process.
- **A display server with client programs.** The right long-term design.
  It needs IPC and shared memory first; this step delivers the drawing
  and input layers that such a server would be built on.

## Consequences

The next step towards a real desktop is inter-process communication
(pipes or message passing, and shared memory), which would turn *Run*
into a terminal, let programs own windows, and allow the shell to move
out of the kernel.

*Update:* pipes now exist and *Run* is a line-mode terminal (ADR 0014).
Programs can now own windows (ADR 0018).
