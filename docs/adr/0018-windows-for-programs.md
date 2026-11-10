# ADR 0018: Shared memory, channels, poll — and windows for other programs

- Status: accepted
- Date: 2026-11-10

## Context

The desktop (ADR 0012) was one program that drew everything. For any
other program to have a window, three things were missing, as that ADR
said: a way for two programs to share pixel memory, a way for a program
to reach the desktop without having been started by it, and a way for
the desktop to wait for input and for several programs at once.

## Decision

Three small kernel mechanisms, each useful on its own, and a protocol on
top of them.

**Shared memory segments.** `shared_create(length)` makes a zero-filled
segment, maps it and returns a 64-bit token; `shared_map(token)` maps
the same frames into another process. A segment lives exactly as long
as it is mapped somewhere: each mapping holds a reference, dropped by
`unmap` or when the process ends. Page-table entries of shared pages
carry a software bit so that tearing down an address space never frees
a frame it does not own. Shared mappings can only be unmapped whole and
never become executable.

**Channels.** `channel_listen(name)` claims a name; `channel_connect`
makes a connection to whoever listens on it; `channel_accept` takes the
next one. A connection is a descriptor that can be read and written —
internally two pipes — so `read`, `write` and end-of-input behave as
they do for pipes. Closing the listener frees the name.

**`poll`** waits until any of a set of descriptors can be read without
waiting (data, end of input, a connection to accept), optionally also
for an input event, with a timeout. All pollers sleep on one queue that
every relevant change wakes; they re-check their own descriptors. With
a handful of pollers that is cheaper than per-object wait lists and
obviously correct. `write` gained a no-wait flag: a server must never
block on a client.

**The window protocol** (`libs/window`, host-tested, no system calls):
a program connects to the channel `desktop` and sends *Create(width,
height, title)*. The desktop creates the pixel segment, answers with its
token, and opens a window. The program draws into the shared memory and
sends *Present(rectangle)*; the desktop sends *Key* and *Pointer*
events. One connection is one window; closing either end closes it.
`omit_rt::window::Window` wraps this for programs.

The desktop treats other programs as untrusted: frames are validated by
the decoder (tested against arbitrary bytes), the pixel memory is
created and sized by the desktop, events are written without waiting,
and a program that breaks the protocol or stops reading only loses its
window.

## What works

- `/bin/ipctest` (in the `shell-session` QEMU test): a segment mapped
  twice in one process and once in another, lifetime ("gone after the
  last unmap"), refusal of partial unmaps and of executable shared
  memory; channels between two processes; `poll` timeouts, wake-ups on
  connect, data and hang-up; no-wait writes that stop at a full buffer.
- The kernel tests channels at every boot.
- `/bin/scribble` and `/bin/clock` are programs with windows. The
  `desktop` QEMU test starts `scribble` from the Run window and, through
  the emulated mouse and keyboard, draws a stroke (press, move, release
  — checked at three points of the line), changes the colour, clears the
  drawing and quits; the title bar text set by the program is checked
  too, and that the window disappears when the program ends.
- Host tests: the protocol (round trips byte by byte, rejection of
  everything outside it, 2000 random streams), pointer capture in the
  window manager, clipped row copies.

## Limitations

- **Tokens and channel names are not a security boundary.** Any program
  can connect to any name, claim a free name (including `desktop` when
  no desktop runs), and map any segment whose token it learns or
  guesses; tokens come from a clock-seeded generator, not a
  cryptographic one. There are no users or permissions in the system
  yet, so this adds no new exposure, but it is not a design to keep.
- One window per connection, fixed size, no resizing, no menus, no
  clipboard, no way for a program to learn about focus or exposure.
- The desktop copies a window's pixels into its own image and from there
  to the screen: two copies per update. *Present* names a rectangle, but
  the desktop currently repaints the whole window.
- A program can change its pixels while the desktop copies them; the
  result is a torn frame until the next *Present*. There is no
  double-buffering or frame synchronisation.
- At most 31 windows of other programs (the size of a `poll` set).
- `poll` reports readability only; there is no way to wait for room to
  write.
- Pointer events reach a program only between a press in its window and
  the release (no hover), and only the left button is reported.
- No measurements of drawing speed have been made.

## Alternatives considered

- **Passing descriptors over connections** instead of tokens: the usual
  design, and the one to move to when there are permissions to enforce.
  Tokens needed no new kernel machinery beyond the segment table.
- **Mapping the framebuffer into programs**: not needed for windows, and
  still deferred for the reasons in ADR 0012.
- **Per-object wait queues for `poll`**: more code in every object for
  no measurable benefit at this scale.
