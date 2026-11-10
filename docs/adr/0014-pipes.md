# ADR 0014: Pipes, standard streams and pipelines

- Status: accepted
- Date: 2026-11-10

## Context

Processes could not exchange data except through files, and a program's
standard input was always the console. That ruled out shell pipelines,
made the desktop's "Run" window a file-polling contraption that could not
feed input to a program or show its error output, and is the first thing
on the list of prerequisites in ADR 0013.

## Decision

**Pipes are kernel objects with two reference-counted ends.** A pipe is a
64 KiB buffer. Reading waits while it is empty and returns end-of-input
when it is empty and no writing end remains; writing waits while it is
full and fails (`BrokenPipe`) when no reading end remains. Both waits end
if the waiting process is killed. Ends are counted separately from
memory ownership, so "the last writer went away" is noticed the moment a
descriptor is closed or its process exits — including by a crash.

**Descriptors refer to files or pipe ends.** The descriptor table entries
became an enumeration, and descriptors 0–2 are entries like any other,
empty meaning "the console". `read` and `write` dispatch on the kind.

**`spawn` connects the child's standard streams.** Each of standard
input, output and error is either inherited from the caller or connected
to a descriptor the caller names — a pipe end or a file. A program that
starts a filter keeps one end of each pipe and closes its copies of the
ends it handed over. There is no `fork`, `dup` or descriptor inheritance
beyond these three streams; none was needed.

**A read that does not wait.** `read` takes a flag to return `WouldBlock`
instead of waiting on an empty pipe. The desktop's event loop uses it to
collect program output between input events without a second thread.

**The shell builds pipelines.** `a | b | c` starts the programs with pipes
between them; `< file` connects the first one's input to a file; `>` and
`>>` apply to the last. The line's exit status is that of the last
program. The shell's built-in commands do not take part — a pipeline
stage is always a program — but where a program of the same name exists
in `/bin` (`cat`, `ls`, `echo`) it is used, so the common cases read
naturally.

**The desktop's Run window is a line-mode terminal.** A program's output
and error streams arrive through one pipe; typed lines go to its standard
input through another; Ctrl+D closes that end, Ctrl+C kills the program.

## Verification

- A kernel self-test at every boot: partial reads, end-of-input, broken
  pipe, cloned ends, and three pipe-capacities of data from another
  thread with the writer blocking.
- `/bin/pipetest`: the same from user space, across processes — the
  output of another program, and 300 KB sent through `cat` and compared,
  with both sides having to wait for each other.
- The shell's parser and pipeline logic on the build host.
- In QEMU: pipelines of two to four programs, input redirection, a
  Windows program at either end of a pipeline, exit status and error
  reporting; and in the desktop test, error output on screen and a typed
  line reaching `tee`'s file.

## Limitations

- A pipe carries bytes only: no descriptor passing, no message
  boundaries, no shared memory. A display server needs at least one of
  those; this is the first half of that prerequisite.
- No `poll`/`select`: a program can wait on one pipe, or poll several
  with non-waiting reads.
- Writes of any size are completed piecewise; there is no atomicity
  guarantee for small writes from several writers.
- Built-in shell commands cannot be pipeline stages, and there is still
  no job control beyond `&` and `kill`.
- The Run window is line-oriented: no escape sequences, so full-screen
  programs would not work in it (none exist yet).
