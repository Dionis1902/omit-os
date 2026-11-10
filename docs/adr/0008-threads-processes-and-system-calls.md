# ADR 0008: Threads, processes and system calls

- Status: accepted
- Date: 2026-11-10

## Decisions

### Scheduler

- **Preemptive round-robin, one CPU, 10 ms time slice.** It is the simplest
  scheduler that is correct, and it establishes the interfaces everything
  else builds on: threads, context switches, wait queues, sleep and join.
  The project's real scheduling goals (latency classes, SMP, topology and
  P/E-core awareness, workload modes) are deliberately not attempted yet;
  they need measurements on real workloads that do not exist until there is
  a userspace to run them.
- **Context switch in ~20 lines of assembly**: push the callee-saved
  registers, swap stack pointers, pop, return. Preemption needs no separate
  path: the timer interrupt handler calls the same `reschedule`, and the
  preempted thread later resumes inside its own trap frame.
- **One lock** protects all scheduler state. Switches are prepared under it
  and performed after it is released with interrupts still off.
- **Kernel stacks** are 64 KiB, each in its own virtual slot with an unmapped
  guard page below, so an overflow faults instead of corrupting memory.
- **Blocking is explicit**: `WaitQueue::wait_until(condition)` re-checks the
  condition with interrupts disabled and enqueues before switching, so
  wake-ups cannot be lost. Console input uses it; the idle thread halts the
  CPU when nothing is runnable.

### Processes

- A process is a user address space plus one kernel thread. Multi-threaded
  processes are not supported yet.
- **Address spaces** have a private lower half; the upper-half top-level
  entries are copied from the kernel's table at creation and share its
  lower-level tables. This relies on the kernel never creating a new
  top-level entry after boot, which holds because all four kernel regions
  are populated during initialisation.
- **Programs** are static, fixed-address ELF64 executables. The loader
  enforces page alignment, the user address range and W^X per segment, and
  gives each process a 256 KiB stack. There is no dynamic linking, no ASLR
  and no demand paging: every page is allocated and filled at load time.
- **Exit and cleanup**: when a process's thread is reaped, dropping its
  address space walks the lower half and frees every frame and page table.
  Verified by running 32 processes (15 of them crashing) and observing
  identical free-memory figures before and after.

### System calls

- **`syscall`/`sysret`**, not a software interrupt: it is the fast path on
  x86-64 and the one a performance-oriented kernel has to get right
  eventually. The GDT is ordered as the instruction pair requires.
- The entry code switches to the thread's kernel stack by hand. On one CPU
  the two per-CPU values it needs are statics; with SMP they move behind
  `swapgs`.
- Registers the ABI does not preserve are zeroed before returning, so kernel
  values do not leak to user code.
- **User pointers are never dereferenced.** The kernel copies through the
  process's page tables and the direct map, checking each page is mapped for
  user access. A bad pointer yields `EFAULT`; it cannot fault the kernel and
  needs no SMAP override. This costs a table walk per page and will be
  revisited (direct access with fault fix-up) when measurements justify it.
- The ABI lives in `libs/abi` and is shared by the kernel and the userspace
  runtime. It has six calls: `exit`, `write` (console only), `getpid`,
  `yield`, `sleep`, `uptime`. It is explicitly unstable.

### Fault containment

A CPU exception raised in user mode ends that process (exit status 139) with
a one-line report; the kernel and all other processes continue. `kill` marks
a process, which exits the next time it is about to return to user mode —
so even a program that never makes a system call dies at the next timer
tick.

### Getting programs onto the system

- User programs are built as part of the workspace (`userspace/`), packed
  into a `ustar` archive by `xtask`, and placed on the boot volume as
  `\EFI\OMIT\INITRD`. The loader passes it through (boot protocol v3) and the
  kernel unpacks it into the RAM filesystem as `/bin/*`.
- tar was chosen over a custom format because standard tools can create and
  inspect it and the reader is about a hundred lines.

### Files, arguments and standard input (addendum)

- The root filesystem is now one kernel-wide object behind a **sleeping
  mutex** (`sync::Mutex`, built on the wait queue). The shell locks it only
  while a built-in command runs and releases it before waiting for a
  program, which may need it.
- Each process has a working directory (inherited from the shell) and a
  table of up to 32 descriptors. 0-2 are the console; files are opened with
  `open` and addressed by **path plus offset**, not by inode. That keeps the
  VFS interface unchanged for now at the cost of wrong behaviour when an
  open file is renamed or removed; it will be replaced by inode handles when
  the persistent filesystem arrives.
- **Arguments** are written to the top of the new process's stack in the
  conventional layout (count, pointer array, null, strings); `rsp` points at
  the count on entry.
- **Standard input** is line-oriented: the kernel applies the same line
  editor the shell uses and returns one line per `read`; Ctrl-D on an empty
  line is end of input. A process blocked on input can still be killed.

## Not done

- No memory-management or process-creation system calls: a program cannot
  allocate memory beyond its image and stack, or start another program.
  Only the shell can start processes.
- No directory system calls, no environment, no pipes, and no output
  redirection for programs.
- No signals; `kill` is unconditional termination.
- Panic and exception backtraces only cover the boot thread's stack.
- SMP: everything here assumes one CPU.
