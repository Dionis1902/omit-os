# ADR 0011: Running Windows executables natively

- Status: accepted
- Date: 2026-11-10

## Context

A long-term goal of Omit OS is to run Windows programs without Wine,
Proton, a virtual machine or CPU emulation, and without any Microsoft
code. A Windows program is x86-64 machine code in a PE32+ file that
expects three things from its surroundings: to be laid out in memory as
the file describes, to find the functions it imports from system libraries
such as `KERNEL32.dll`, and to be called with the Windows x64 calling
convention. None of that needs the Windows kernel. It needs a loader and
our own implementations of the functions.

This ADR records the first working version and, just as importantly, how
little of Windows it covers.

## Decision

**Where the code runs.** Windows support lives entirely in user space. The
loader, `/bin/winrun`, is an ordinary Omit OS program; the Windows program
runs inside that process, directly on the CPU. The kernel knows nothing
about PE files or the Windows API. It gained only general facilities:

- `protect` — change mapped memory between read-only, read/write and
  read/execute (never write+execute);
- `set_thread_pointer` — set the `GS` base, saved and restored per thread;
- SSE/x87 state saved and restored per process (`FXSAVE`). The kernel and
  the native programs are built without SSE, so this was not needed before;
  Windows programs use SSE freely.

When the shell or the `spawn` system call is asked to run a file that
starts with `MZ`, it runs `/bin/winrun` on it instead, so Windows programs
are started like any other.

**The pieces.**

- `compat/windows/pe` — parser for PE32+ files: headers, sections, imports,
  base relocations; builds the in-memory image. No `unsafe`, fully
  bounds-checked, tested on the build host with hand-assembled images,
  truncation at every length and 200 000 random corruptions.
- `compat/windows/win32` — the machine-independent part of the API layer:
  Win32 error codes, path translation (`C:\a\b` is `/a/b`), command-line
  quoting, UTF-16 conversion, the handle table. Host-tested.
- `userspace/programs/src/bin/winrun` — the loader and the implementation
  of 77 `KERNEL32.dll` functions as `extern "win64"` Rust functions over
  Omit OS system calls.
- `compat/windows/samples` — test programs. They are real Windows console
  executables (target `x86_64-pc-windows-msvc`), built by `cargo xtask`
  with the LLVM linker that ships with Rust.

**Loading.** `winrun` validates the file, maps the image at a 64
KiB-aligned address chosen by the kernel, applies base relocations, fills
the import address table and applies per-section access rights. It then
allocates a stack with a guard page, sets up a minimal thread environment
block (stack bounds, self pointer, process ID, last-error slot, pointer to
a process environment block holding the image base) and calls the entry
point on the new stack. No code of the program runs before every import
has been resolved.

**Libraries (DLLs).** An import from `KERNEL32.dll` is bound to the
built-in implementation. An import from any other library makes the
loader look for a file of that name — next to the importing image, then
in `/bin` — and load it the same way, recursively; its export table
provides the addresses. Libraries are initialised (`DllMain` with
`DLL_PROCESS_ATTACH`) after the libraries they depend on and before the
program starts. `LoadLibraryA`, `GetProcAddress`, `GetModuleHandleA/W`,
`GetModuleFileNameA` and `FreeLibrary` work on the same module list;
`KERNEL32.dll` appears in it as a pseudo-module. Libraries are never
unloaded, exports forwarded to another library are not followed, and a
failed `LoadLibrary` leaves nothing mapped. No Microsoft library is, or
may be, shipped: a program that needs `USER32.dll` or the C runtime DLLs
is refused because those files do not exist here.

**Threads.** `CreateThread` starts a native thread (ADR 0015) on its own
stack, with its own thread environment block — so the last-error code and
the 64 thread-local storage slots (`TlsAlloc` and friends) are per thread,
as on Windows. `WaitForSingleObject` waits for threads and events with a
timeout; events are manual- or auto-reset; critical sections are
recursive locks kept in the program's own 40-byte structure. All of them
sleep on futexes; none polls. The layer's own state is behind a lock that
is never held while program code runs.

**Honesty rules for the API.** A function is either implemented for the
cases its comment states, or absent. A program that imports an absent
function or library is refused before it starts, with every missing item
named —
it does not start and crash later. Within an implemented function,
unsupported cases fail with a real Win32 error code. There are no stubs
that return success without doing the work.

**Clean-room rule.** Everything is written from public documentation
(Microsoft's "PE Format" specification and the API reference on Microsoft
Learn). No Windows binaries, headers or SDK files are used, shipped, or
needed to build; no code is taken from Wine or ReactOS.

## What works

Verified in QEMU by the `windows-programs` test:

- a minimal executable prints through `GetStdHandle`/`WriteFile`;
- a test executable performs 94 self-checked API operations — heap,
  virtual memory, file and directory operations with the documented error
  codes (including reading and writing through one handle with the
  position moved about, and directory listings with wildcards), console
  output in UTF-16, timing and the date, the TEB/PEB fields, data that
  requires relocation, and other programs started with `CreateProcessA`
  (a Windows program whose exit code is collected, one named by file, a
  native program that is terminated, a missing one) — and its exit code
  reaches the shell;
- output redirection to a file works for Windows programs;
- two Windows programs running at once keep their SSE registers apart
  (the test fails when the kernel's save/restore is disabled — this was
  checked);
- a program imports functions from a DLL file, which is loaded,
  relocated, bound to `KERNEL32` itself and initialised first; the
  program then loads a second DLL on request, which in turn depends on
  the first (33 self-checked operations);
- a multi-threaded program: four threads counting under a critical
  section (entered recursively), exit codes including `ExitThread` from a
  nested call, per-thread storage and error codes, auto- and manual-reset
  events with waiters, timeouts, and a thread still running when the
  program returns (44 self-checked operations);
- a graphical program (`wingui.exe`, `desktop` test): it registers a
  window class, creates a window, paints text, a filled rectangle and a
  line in `WM_PAINT`, and runs a message loop. Driven through the
  emulated mouse and keyboard, the test checks on screenshots that a
  click arrives as `WM_LBUTTONDOWN` with the right position, typed keys
  arrive as `WM_CHAR`, `SetWindowTextA` changes the title bar, a
  `MessageBoxA` appears and is dismissed with Enter, a timer started
  with `SetTimer` delivers five `WM_TIMER` messages and is then stopped
  by the program, and that after
  `DestroyWindow` → `WM_DESTROY` → `PostQuitMessage(7)` the loop ends
  with that code; the program also confirms `WM_CREATE`'s parameters,
  `WM_SIZE`, a posted message and `WM_KEYUP`, and checks 39 results of
  the remaining functions itself (sizes, error codes, device context
  state, text extents, object lifetime). Started without a desktop, it
  reports that its window cannot be created;
- a program needing unimplemented functions or a missing library, a file
  that is not a PE image, a DLL started as a program, and a native
  program given to `winrun` are each refused with a specific message.

## Limitations

This is a proof that the approach works, not Windows compatibility.

- **Only programs whose imports are all satisfied by the implemented
  functions — 77 of `KERNEL32.dll`, 28 of `USER32.dll`, 14 of
  `GDI32.dll` — or by DLL files that are present and themselves
  loadable, run.** That excludes practically all existing software:
  anything linked against the C runtime or the Rust standard library, or
  using any other Windows system DLL. `winrun --list` prints the
  functions.
- Graphical programs get a first slice only (see the module comment of
  `winrun/gui.rs` for the full list of differences): one kind of
  top-level window of fixed size, visible from creation; no child
  windows and therefore no controls, menus or dialogs other than a
  one-button `MessageBoxA`; one fixed-width font; a dozen message
  types; solid brushes and one-pixel pens; no bitmaps, regions,
  clipping, clipboard, or `...W` functions. Timers have the system
  timer's 10 ms resolution and are only looked at when the program
  asks for a message. Windows need the
  desktop to be running.
- No registry, networking,
  structured exception handling, or environment.
- Threads: libraries are not notified of them (`DLL_THREAD_ATTACH`),
  static thread-local storage (`__declspec(thread)`, the PE TLS
  directory) is not supported — only the `Tls*` functions — and there are
  no mutex, semaphore or `WaitForMultipleObjects` objects yet. One CPU.
- The test programs were built here and have **not been run on Windows**
  to confirm they behave identically there.
- Images without relocation information are refused: the kernel cannot
  map at a requested address. 32-bit programs are refused.
- File times are not recorded (listings report zero); a directory
  listing has no `.` and `..` entries and no short names.
- `CreateProcessA` ignores everything but the program and its command
  line (no environment, start-up information, working directory or
  suspended start), and a terminated program's exit code is the
  kernel's, not the one asked for.
  File names are case-sensitive. `...A` functions take UTF-8.
- No memory may be writable and executable, so programs that generate
  code at run time (JIT compilers) cannot work without changes here.
- AVX is not enabled; programs must not use it (CPUID reports it as
  unavailable to well-behaved programs because `OSXSAVE` is clear).
- Whether standard output is a console cannot be determined; it is always
  reported as one.

## Alternatives considered

- **Windows API inside the kernel** (a second system call personality, as
  Windows NT does for its own subsystems). Rejected: it puts a large,
  attacker-facing compatibility surface in the most privileged code, for
  no benefit at this stage.
- **Porting Wine.** Wine needs a POSIX system with a C library, threads,
  sockets and X11/Wayland — none of which exist here — and the project's
  rules ask for independence from it.
- **Fixed-address mapping** so that images without relocations can run.
  Deferred; nearly all 64-bit programs are relocatable (ASLR).

## Consequences

- The kernel stays independent of Windows; all compatibility code can
  crash without affecting anything but its own process.
- Compatibility grows by adding functions to the loader, one documented
  API at a time, each with a test program. The next steps of real value
  are static thread-local storage, structured exception handling, and
  the remaining functions the C runtime start-up code calls.
