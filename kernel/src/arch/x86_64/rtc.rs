// SPDX-License-Identifier: GPL-2.0-only
//! The PC real-time clock (the battery-backed clock in the CMOS chip).
//!
//! It is read once, at start-up; afterwards the system keeps time with
//! its own clock (see `time::realtime_us`). The registers are those of
//! the MC146818 that every PC has imitated since the AT: seconds,
//! minutes, hours, day, month and two-digit year, in BCD or binary and
//! in 12- or 24-hour form according to status register B. The clock is
//! taken to be UTC, as firmware keeps it on machines that do not also
//! run Windows.

use omit_abi::datetime::DateTime;

use super::port::{inb, outb};

const INDEX_PORT: u16 = 0x70;
const DATA_PORT: u16 = 0x71;
/// Bit 7 of the index port disables non-maskable interrupts; kept as
/// found (clear).
const REGISTER_SECONDS: u8 = 0x00;
const REGISTER_MINUTES: u8 = 0x02;
const REGISTER_HOURS: u8 = 0x04;
const REGISTER_DAY: u8 = 0x07;
const REGISTER_MONTH: u8 = 0x08;
const REGISTER_YEAR: u8 = 0x09;
const REGISTER_STATUS_A: u8 = 0x0A;
const REGISTER_STATUS_B: u8 = 0x0B;
/// The century, on machines whose firmware keeps it there (the usual
/// place; ACPI can name another, which is not looked up).
const REGISTER_CENTURY: u8 = 0x32;

const STATUS_A_UPDATING: u8 = 1 << 7;
const STATUS_B_24_HOUR: u8 = 1 << 1;
const STATUS_B_BINARY: u8 = 1 << 2;
const HOURS_PM: u8 = 1 << 7;

fn read(register: u8) -> u8 {
    // SAFETY: the CMOS index and data ports are owned by this module;
    // selecting a register and reading it has no other effect.
    unsafe {
        outb(INDEX_PORT, register);
        inb(DATA_PORT)
    }
}

/// The raw time registers, read while no update is in progress.
fn snapshot() -> [u8; 7] {
    // An update takes about 2 ms; give up waiting after far longer.
    for _ in 0..100_000 {
        if read(REGISTER_STATUS_A) & STATUS_A_UPDATING == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    [
        read(REGISTER_SECONDS),
        read(REGISTER_MINUTES),
        read(REGISTER_HOURS),
        read(REGISTER_DAY),
        read(REGISTER_MONTH),
        read(REGISTER_YEAR),
        read(REGISTER_CENTURY),
    ]
}

/// Decodes raw register values; `status_b` says how they are encoded.
fn decode(raw: [u8; 7], status_b: u8) -> Option<DateTime> {
    let binary = status_b & STATUS_B_BINARY != 0;
    let number = |value: u8| {
        if binary {
            Some(value)
        } else {
            // Binary-coded decimal: two digits, each below ten.
            (value & 0x0F < 10 && value >> 4 < 10).then(|| (value >> 4) * 10 + (value & 0x0F))
        }
    };
    let [second, minute, hour, day, month, year, century] = raw;
    let pm = hour & HOURS_PM != 0;
    let mut hour = number(hour & !HOURS_PM)?;
    if status_b & STATUS_B_24_HOUR == 0 {
        // 12-hour form: 12 AM is midnight, 12 PM noon.
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = hour % 12 + if pm { 12 } else { 0 };
    }
    let year = i32::from(number(year)?);
    // A plausible century register is used; otherwise the two-digit
    // year is taken to be in this century.
    let century = number(century)
        .map(i32::from)
        .filter(|century| (19..=25).contains(century))
        .unwrap_or(20);
    let moment = DateTime {
        year: century * 100 + year,
        month: number(month)?,
        day: number(day)?,
        hour,
        minute: number(minute)?,
        second: number(second)?,
    };
    moment.is_valid().then_some(moment)
}

/// Reads the clock. `None` if it holds no valid date and time.
pub fn now() -> Option<DateTime> {
    // Two identical readings in a row: neither straddled an update.
    let mut previous = snapshot();
    for _ in 0..8 {
        let current = snapshot();
        if current == previous {
            return decode(current, read(REGISTER_STATUS_B));
        }
        previous = current;
    }
    None
}

/// Checks the decoding against known register contents.
pub fn self_test() -> Result<(), &'static str> {
    let moment = |hour| DateTime {
        year: 2026,
        month: 11,
        day: 10,
        hour,
        minute: 59,
        second: 7,
    };
    // BCD, 24-hour — what PC firmware uses.
    if decode([0x07, 0x59, 0x23, 0x10, 0x11, 0x26, 0x20], STATUS_B_24_HOUR) != Some(moment(23)) {
        return Err("BCD 24-hour time decoded wrongly");
    }
    // Binary, 12-hour: 11 PM, 12 AM, 12 PM.
    for (raw_hour, hour) in [(11 | HOURS_PM, 23), (12, 0), (12 | HOURS_PM, 12)] {
        if decode([7, 59, raw_hour, 10, 11, 26, 20], STATUS_B_BINARY) != Some(moment(hour)) {
            return Err("binary 12-hour time decoded wrongly");
        }
    }
    // No century register: this century.
    if decode([0x07, 0x59, 0x23, 0x10, 0x11, 0x26, 0xFF], STATUS_B_24_HOUR) != Some(moment(23)) {
        return Err("a missing century register was not tolerated");
    }
    // Not BCD, not a date.
    if decode([0x0A, 0, 0, 1, 1, 0, 0x20], STATUS_B_24_HOUR).is_some()
        || decode([0, 0, 0, 0x31, 0x02, 0x26, 0x20], STATUS_B_24_HOUR).is_some()
    {
        return Err("invalid clock contents were accepted");
    }
    Ok(())
}
