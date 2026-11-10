# ADR 0015: Threads within a process, and the futex

- Status: accepted
- Date: 2026-11-10

## Context

A process had exactly one thread. That is enough for small tools, but not
for a display server, for most Windows programs (the C runtime itself
expects threads and locks), or for anything that wants to compute and
respond at the same time. ADR 0013 lists threads as the first
prerequisite for further compatibility work, together with pipes
(ADR 0014).

## Decision

**A process is a set of threads sharing one address space.** The address
space is reference-counted and released with the last thread. Each thread
has its own kernel stack, saved registers, floating-point state and
`GS` base, as before; the descriptor table, working directory and standard
streams belong to the process.

**Creating a thread** (`thread_create`) takes an entry point, a stack
pointer and one argument — the program allocates the stack itself — plus
the address of a flag the kernel sets when the thread has ended. The flag
is set, and its waiters woken, only after the thread has left user mode
for good. That closes the classic race in which a joining thread frees
the stack while the ending thread is still using it.

**Ending.** `thread_exit` ends one thread; when the last thread ends, the
process ends. `exit` ends the process: the calling thread at once, every
other thread the next time it would return to user mode (threads blocked
in the kernel are woken for that). A fault in any thread ends the
process, as does `kill`. If several threads end the process, the first
exit status stands. Waiting for a process waits for all its threads.

**The futex is the only synchronisation primitive the kernel provides.**
`futex_wait(address, expected, timeout)` sleeps if the 32-bit value at the
address still equals `expected`; `futex_wake(address, count)` wakes
sleepers. Comparison and going to sleep happen under one lock with the
wake-up path, so a wake-up cannot fall between them. Locks, joins and
anything else are built in user space on top: an uncontended lock never
enters the kernel. Waiters are keyed by address space and address.

**User space.** The runtime gained `thread::spawn` / `JoinHandle`,
`sync::Mutex` (three-state, futex-based), and a heap that threads can
share. A panic in any thread ends the program.

## Verification

`/bin/threadtest`, run by the shell test in every configuration:

- a result returned through `join`;
- four threads incrementing one counter 20 000 times each under a mutex,
  yielding inside the critical section — no update may be lost;
- four threads allocating and freeing concurrently, each checking its
  data;
- futex semantics: no sleep when the value differs, timeout, wake-up from
  another thread, no phantom waiters;
- the program returning while one of its threads sleeps for ever and
  another spins: the process must end, and `ps` afterwards shows none of
  its threads.

The scheduler refactoring is also covered by every other test, since all
processes now go through the shared-address-space path.

## Limitations

- One CPU: threads are concurrent, not parallel. Nothing here has been
  tested on more than one processor, and the kernel's locking is not
  designed for it yet.
- No thread-local storage support in the native runtime (a thread can
  set its own `GS` base, which is what the Windows layer uses).
- No priorities, no affinity, no per-thread signals or cancellation: a
  thread ends by returning, by `thread_exit`, or with its process.
- The heap lock yields instead of sleeping; under heavy contention that
  wastes time slices. Not measured.
- A thread that is dropped without being joined keeps its stack until the
  process ends.
