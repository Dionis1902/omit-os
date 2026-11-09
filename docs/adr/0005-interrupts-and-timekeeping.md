# ADR 0005: Interrupts and timekeeping

- Status: accepted
- Date: 2026-11-09

## Decisions

### Interrupt delivery

- **Local APIC only.** The legacy 8259 PICs are remapped to vectors 32-47 (so
  a spurious PIC interrupt cannot masquerade as a CPU exception) and fully
  masked. Vectors 48-254 are available to handlers; 255 is the APIC spurious
  vector.
- **x2APIC when the CPU offers it, xAPIC otherwise.** x2APIC register access
  is an MSR read/write rather than a device-memory access, which is cheaper
  (substantially so under virtualisation), and it is the only mode that scales
  past 255 CPUs. The mode is chosen once at boot; register access goes through
  two small functions that branch on it.
- **One entry path for all 256 vectors.** Assembly stubs build the same trap
  frame for exceptions and interrupts and call one Rust function. All gates
  are interrupt gates, so handlers run with interrupts disabled; there is no
  interrupt nesting.
- **Handlers are plain `fn()` in a table** behind the interrupt-safe spinlock.
  The dispatcher copies the function pointer out, runs it without the lock,
  then signals end-of-interrupt. Interrupts on vectors without a handler are
  counted and acknowledged, never ignored silently; the boot log reports the
  count.
- **Spinlocks disable interrupts while held** and restore the previous state
  on release. This is the simplest rule under which any lock may be taken
  from both thread and interrupt context without deadlock; it requires
  critical sections to stay short.

### Time

- **Monotonic clock: the TSC**, scaled by a frequency measured at boot.
  Reading it is a single unprivileged instruction with no locking.
- **Scheduling tick: the local APIC timer**, periodic at 100 Hz.
- **Calibration against PIT channel 2**, polled through port 0x61 over a
  20 ms window, measuring the TSC and the APIC timer in the same window.

  | Reference | Availability | Notes |
  | --- | --- | --- |
  | **PIT channel 2** | all PC-compatibles, every emulator | no interrupts or table parsing needed; chosen |
  | CPUID leaves 0x15/0x16 | recent Intel only | exact, but absent on AMD and under most hypervisors |
  | HPET / ACPI PM timer | most hardware | needs ACPI table parsing, which the kernel does not have yet |

- **Self-test at boot.** After enabling interrupts the kernel waits for ten
  ticks, halting the CPU in between, and compares the elapsed time on the TSC
  clock with the expected 100 ms. This checks interrupt delivery, the
  acknowledge path, and the calibration of both clocks against each other.
  The tolerance is a factor of two, because emulated timers jitter; a wrong
  divider or vector would miss by far more.

## Consequences and open issues

- **Hardware without a PIT** fails calibration with a clear error. CPUID- and
  HPET-based calibration should be added before real-hardware bring-up.
- **TSC quality is not enforced.** The kernel logs whether the TSC is
  invariant but uses it regardless. On CPUs where it is not, a different clock
  source will be needed; under QEMU's emulation the flag is simply not
  advertised.
- **A periodic tick wakes an idle CPU 100 times a second.** This contradicts
  the long-term energy goals and is a deliberate interim choice: tickless
  operation only makes sense once there is a scheduler to decide when the
  next wake-up is actually needed.
- **x2APIC mode is not exercised by the local test environment** (QEMU's TCG
  does not offer the feature). CI additionally runs the boot tests under KVM
  with the host CPU model to cover it when the runner supports KVM.
- No I/O APIC, so no device interrupts yet; that needs the ACPI MADT.
- NMI and machine-check handlers share the normal exception path and stack.
