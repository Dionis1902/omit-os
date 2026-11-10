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
  of 43 `KERNEL32.dll` functions as `extern "win64"` Rust functions over
  Omit OS system calls.
- `compat/windows/samples` — test programs. They are real Windows console
  executables (target `x86_64-pc-windows-msvc`), built by `cargo xtask`
  with the LLVM linker that ships with Rust.

**Loading.** `winrun` validates the file, resolves every import *before*
mapping anything, maps the image at a 64 KiB-aligned address chosen by the
kernel, applies base relocations, fills the import address table, applies
per-section access rights, allocates a stack with a guard page, sets up a
minimal thread environment block (stack bounds, self pointer, process ID,
last-error slot, pointer to a process environment block holding the image
base) and calls the entry point on the new stack.

**Honesty rules for the API.** A function is either implemented for the
cases its comment states, or absent. A program that imports an absent
function is refused before it starts, with every missing function named —
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
- a test executable performs 57 self-checked API operations — heap,
  virtual memory, file and directory operations with the documented error
  codes, console output in UTF-16, timing, the TEB/PEB fields, data that
  requires relocation — and its exit code reaches the shell;
- output redirection to a file works for Windows programs;
- two Windows programs running at once keep their SSE registers apart
  (the test fails when the kernel's save/restore is disabled — this was
  checked);
- a program needing unimplemented functions, a file that is not a PE
  image, and a native program given to `winrun` are each refused with a
  specific message.

## Limitations

This is a proof that the approach works, not Windows compatibility.

- **Only programs that import nothing but the 43 implemented functions
  run.** That excludes practically all existing software: anything linked
  against the C runtime, the Rust standard library, or any DLL other than
  `KERNEL32.dll`. `winrun --list` prints the functions.
- Console programs only. No graphics, windows, messages, registry,
  networking, threads, processes started by Windows programs, DLL loading,
  structured exception handling, thread-local storage, or environment.
- The test programs were built here and have **not been run on Windows**
  to confirm they behave identically there.
- Images without relocation information are refused: the kernel cannot
  map at a requested address. 32-bit programs are refused.
- A file is opened for reading or for writing, not both; no seeking.
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
  are threads, the functions the C runtime start-up code calls, and
  loading several modules (the import of one DLL from another).
