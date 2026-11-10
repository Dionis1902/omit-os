# ADR 0024: Random numbers

- Status: accepted
- Date: 2026-11-10

## Context

Nothing in the system could produce a number that someone else could
not guess. Encrypted connections (the next step of networking) need
such numbers for every connection; so do many ordinary programs.

## Decision

- One generator in the kernel, behind a `random` system call that fills
  a buffer of up to 4096 bytes. Programs do not seed anything
  themselves.
- The generator (`libs/rng`) is the ChaCha20 block function of RFC 8439
  used with key erasure: before any output is handed over, the key that
  produced it is replaced by key stream, so the generator's state does
  not reveal what it gave out earlier. Input is mixed in by adding it
  to the key and replacing the key the same way.
- Its input, in `kernel/src/random.rs`:
  - the processor's `RDRAND` instruction where there is one — 32 bytes
    at start-up and 32 more before every request;
  - the time-stamp counter at every timer tick, key press and mouse
    report, folded together, and the clock at start-up.
- **The call says which kind of numbers it gave**: resting on the
  processor's generator, or on timing alone. On a processor without
  `RDRAND` timing is all there is, and how unpredictable that is has
  not been measured — in a virtual machine it may be very little. The
  kernel says so at start-up, and a program doing something that
  matters is expected to pass the warning on.

## What works

- Host tests of `libs/rng`: the block function against the RFC's test
  vector (also compared with OpenSSL's ChaCha20 when this was written);
  that output never repeats, depends on everything mixed in, and is
  made with a key the generator no longer holds afterwards.
- `random --check` in the `shell-session` QEMU test asks for two lots
  of 64 bytes and fails if they are blank, equal, or lopsided. It runs
  on a processor model with `RDRAND` and, in the `qemu64`
  configuration, on one without: both sources are exercised.

## Limits

- **Neither the construction nor the code has been reviewed by a
  cryptographer.** The pieces are standard; the way they are put
  together here is this project's.
- `RDRAND` is trusted as the processor delivers it. `RDSEED` is not
  used.
- The quality of the timing source is not estimated, nothing is kept
  from one start to the next, and nothing waits for "enough" input:
  on a processor without `RDRAND`, numbers asked for right after
  start-up are the weakest.
- The tests can only catch a generator that is plainly broken. Passing
  them says nothing about unpredictability.
