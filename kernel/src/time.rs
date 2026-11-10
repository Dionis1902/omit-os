// SPDX-License-Identifier: GPL-2.0-only
//! Kernel timekeeping.
//!
//! Two facilities, both set up by [`init`]:
//!
//! * a monotonic clock ([`monotonic_ns`]) read from the CPU's time-stamp
//!   counter, with its frequency measured at boot;
//! * a periodic tick ([`ticks`]) driven by the local APIC timer interrupt,
//!   which is what will drive scheduling.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use omit_acpi::PmTimer;

use crate::arch::apic::{self, ApicError, Reference};
use crate::arch::{cpu, interrupts};

/// Frequency of the periodic tick.
pub const TICK_HZ: u32 = 100;

const NANOS_PER_SEC: u64 = 1_000_000_000;

static TSC_HZ: AtomicU64 = AtomicU64::new(0);
static TSC_AT_INIT: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Why the tick self-test failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickTestError {
    /// No (or too few) timer interrupts arrived within the deadline.
    NoTicks {
        /// Ticks that did arrive.
        seen: u64,
    },
    /// Ticks arrived, but not at the expected rate.
    WrongRate {
        /// Time the expected number of ticks actually took, in nanoseconds.
        elapsed_ns: u64,
        /// Time they should have taken, in nanoseconds.
        expected_ns: u64,
    },
}

impl fmt::Display for TickTestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoTicks { seen } => {
                write!(f, "timer interrupts are not arriving ({seen} seen)")
            }
            Self::WrongRate {
                elapsed_ns,
                expected_ns,
            } => write!(
                f,
                "tick rate is off: took {} us, expected {} us",
                elapsed_ns / 1000,
                expected_ns / 1000
            ),
        }
    }
}

/// What [`init`] measured.
#[derive(Debug, Clone, Copy)]
pub struct TimeInfo {
    /// Time-stamp counter frequency in Hz.
    pub tsc_hz: u64,
    /// Local APIC timer frequency in Hz.
    pub timer_hz: u64,
    /// The clock the two were measured against.
    pub reference: Reference,
    /// If a second reference clock was available: by how many parts per
    /// thousand its TSC measurement differed from the one used. Where
    /// the two disagreed, the measurements were repeated; this is the
    /// difference of the last pair.
    pub cross_check_per_mille: Option<u64>,
}

/// Calibrates the clocks and starts the periodic tick.
///
/// Must be called once, after the local APIC is initialised, with interrupts
/// disabled. Ticks start being counted once interrupts are enabled.
///
/// The clocks are measured against the legacy PIT, or, where that does
/// not count (or `prefer_pm_timer` asks for it), against the ACPI
/// power-management timer if the machine has one. When both work, the
/// second serves as a check on the first.
pub fn init(pm_timer: Option<PmTimer>, prefer_pm_timer: bool) -> Result<TimeInfo, ApicError> {
    let mut references = [Some(Reference::Pit), pm_timer.map(Reference::PmTimer)];
    if prefer_pm_timer {
        references.reverse();
    }
    // A measurement lasts 20 ms and is spoilt by anything that holds
    // the CPU up in the wrong moment (on hardware, firmware interrupts;
    // under emulation, the host). With two clocks that can be noticed:
    // if they disagree, both are measured again, a few times.
    const ATTEMPTS: usize = 3;
    /// Agreement that is good enough to stop measuring.
    const AGREE_PER_MILLE: u64 = 20;
    let mut attempt = 1;
    let (reference, calibration, cross_check_per_mille) = loop {
        let mut results = references
            .iter()
            .flatten()
            .filter_map(|&reference| Some((reference, apic::calibrate(reference).ok()?)));
        let (reference, calibration) = results.next().ok_or(ApicError::CalibrationTimeout)?;
        let second = results.next();
        drop(results);
        let difference = second.map(|(_, other)| {
            other.tsc_hz.abs_diff(calibration.tsc_hz) * 1000 / calibration.tsc_hz
        });
        if difference.is_none_or(|difference| difference <= AGREE_PER_MILLE) || attempt == ATTEMPTS
        {
            break (reference, calibration, difference);
        }
        attempt += 1;
        // A clock that did not count is not asked again: finding that
        // out takes seconds.
        for slot in &mut references {
            if slot.is_some_and(|candidate| {
                candidate != reference && second.is_none_or(|(other, _)| other != candidate)
            }) {
                *slot = None;
            }
        }
    };
    TSC_AT_INIT.store(cpu::read_tsc(), Ordering::Relaxed);
    TSC_HZ.store(calibration.tsc_hz, Ordering::Release);

    interrupts::register(apic::TIMER_VECTOR, on_tick).map_err(ApicError::Register)?;
    apic::start_periodic_timer(calibration.timer_hz, TICK_HZ)?;
    Ok(TimeInfo {
        tsc_hz: calibration.tsc_hz,
        timer_hz: calibration.timer_hz,
        reference,
        cross_check_per_mille,
    })
}

/// The real time at the moment `monotonic_ns` was 0, in microseconds
/// since the Unix epoch; 0 if the machine's clock could not be read.
static REALTIME_AT_INIT_US: AtomicU64 = AtomicU64::new(0);

/// Reads the machine's real-time clock and from then on derives the
/// date and time from it and the system's own clock. Returns what the
/// clock said, or `None` if it holds no valid date after 1970.
pub fn init_realtime() -> Option<omit_abi::datetime::DateTime> {
    let moment = crate::arch::rtc::now()?;
    let seconds = u64::try_from(moment.to_unix()?).ok().filter(|&s| s != 0)?;
    let since_init_us = monotonic_ns() / 1000;
    REALTIME_AT_INIT_US.store(
        (seconds * 1_000_000).saturating_sub(since_init_us),
        Ordering::Relaxed,
    );
    Some(moment)
}

/// The date and time as microseconds since the Unix epoch (UTC), or
/// `None` if the machine's clock could not be read at start-up. The
/// clock is read once; afterwards time advances with the system's own
/// clock and is never adjusted.
pub fn realtime_us() -> Option<u64> {
    match REALTIME_AT_INIT_US.load(Ordering::Relaxed) {
        0 => None,
        base => Some(base + monotonic_ns() / 1000),
    }
}

fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    crate::sched::on_tick();
}

/// Number of timer ticks since [`init`].
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Nanoseconds since [`init`], from the time-stamp counter. Returns 0 before
/// [`init`].
pub fn monotonic_ns() -> u64 {
    let hz = TSC_HZ.load(Ordering::Acquire);
    if hz == 0 {
        return 0;
    }
    let elapsed = cpu::read_tsc().wrapping_sub(TSC_AT_INIT.load(Ordering::Relaxed));
    (u128::from(elapsed) * u128::from(NANOS_PER_SEC) / u128::from(hz)) as u64
}

/// Verifies end to end that timer interrupts are delivered and arrive at
/// the configured rate: waits for a number of ticks, sleeping the CPU
/// between them, and compares the elapsed time on the independent TSC clock.
///
/// Enables interrupts and leaves them enabled. Returns the measured duration
/// of [`SELF_TEST_TICKS`] ticks in nanoseconds.
pub fn self_test() -> Result<u64, TickTestError> {
    let expected_ns = SELF_TEST_TICKS * NANOS_PER_SEC / u64::from(TICK_HZ);
    let deadline_ns = 10 * expected_ns;

    interrupts::enable();
    // Synchronise to a tick edge so that whole periods are measured.
    let first = ticks();
    let sync_start = monotonic_ns();
    while ticks() == first {
        if monotonic_ns() - sync_start > deadline_ns {
            return Err(TickTestError::NoTicks { seen: 0 });
        }
        interrupts::wait();
    }

    let start_ticks = ticks();
    let start_ns = monotonic_ns();
    while ticks() - start_ticks < SELF_TEST_TICKS {
        if monotonic_ns() - start_ns > deadline_ns {
            return Err(TickTestError::NoTicks {
                seen: ticks() - start_ticks,
            });
        }
        interrupts::wait();
    }
    let elapsed_ns = monotonic_ns() - start_ns;

    // Generous bounds: under emulation both clocks are derived from host
    // time and jitter by several percent. A mis-programmed divider or a
    // wrong calibration would be off by a factor of two or more.
    if elapsed_ns < expected_ns / 2 || elapsed_ns > expected_ns * 2 {
        return Err(TickTestError::WrongRate {
            elapsed_ns,
            expected_ns,
        });
    }
    Ok(elapsed_ns)
}

/// Number of ticks [`self_test`] waits for.
pub const SELF_TEST_TICKS: u64 = 10;
