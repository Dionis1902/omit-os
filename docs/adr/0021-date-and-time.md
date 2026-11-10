# ADR 0021: Date and time

- Status: accepted
- Date: 2026-11-10

## Context

The system knew how long it had been running but not what day it was.
Programs that ask — every Windows program with a C runtime does at
start-up — had nothing to be told.

## Decision

- The PC real-time clock (the CMOS clock) is read once at start-up:
  both encodings (BCD and binary), both hour forms, the century
  register where plausible, and two matching readings in a row so that
  an update in progress cannot produce a torn value.
- From then on the date and time are that reading plus the system's own
  monotonic clock. The hardware clock is not read again and never
  written; nothing adjusts the time.
- The clock is taken to be **UTC**. There are no time zones.
- `realtime` system call (microseconds since 1970-01-01 UTC); calendar
  conversion in `omit_abi::datetime`, host-tested day by day over eight
  centuries; `date` in the shell; `GetSystemTimeAsFileTime`,
  `GetSystemTime` and `GetLocalTime` for Windows programs.

## What works

- The decoder is checked at every boot against known register contents;
  the `boot` test checks that the emulated clock (set to a fixed moment)
  is read correctly, `shell-session` runs `date`, and `wintest.exe`
  checks the Windows functions (a present-day file time that advances,
  a valid broken-down time, the date itself).

## Limitations

- On a machine whose firmware keeps local time in the clock (the
  Windows convention) the system is off by the time-zone offset.
- No time zones, no daylight saving, no leap seconds.
- The time drifts with the accuracy of the TSC calibration; there is no
  network time and no way to set the clock.
- File systems do not record timestamps yet.
- If the clock holds no valid date, there is no date: `date` says so,
  the system call fails, and Windows programs are told 1601-01-01.
