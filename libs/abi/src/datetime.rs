// SPDX-License-Identifier: GPL-2.0-only
//! Calendar dates and times.
//!
//! The system counts time in seconds since 1970-01-01 00:00:00 UTC (the
//! "Unix epoch"), without leap seconds. This module converts between
//! that count and a date and time of day in the proleptic Gregorian
//! calendar. There are no time zones: everything is UTC.

/// A date and time of day (UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    /// The year, for example 2026.
    pub year: i32,
    /// 1 (January) to 12.
    pub month: u8,
    /// 1 to 31.
    pub day: u8,
    /// 0 to 23.
    pub hour: u8,
    /// 0 to 59.
    pub minute: u8,
    /// 0 to 59.
    pub second: u8,
}

const SECONDS_PER_DAY: i64 = 86_400;

/// Whether `year` is a leap year.
pub const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Number of days in `month` (1 to 12) of `year`; 0 for an invalid month.
pub const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days from 1970-01-01 to the given date (negative before it). The
/// arithmetic counts in 400-year cycles of the Gregorian calendar, with
/// years starting in March so that the leap day comes last.
const fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let year = year as i64 - (month <= 2) as i64;
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = (month as i64 + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

impl DateTime {
    /// Whether the fields form a date and time that exists.
    pub const fn is_valid(&self) -> bool {
        self.month >= 1
            && self.month <= 12
            && self.day >= 1
            && self.day <= days_in_month(self.year, self.month)
            && self.hour < 24
            && self.minute < 60
            && self.second < 60
    }

    /// Seconds since the Unix epoch; `None` if the fields are not a
    /// valid date and time.
    pub const fn to_unix(&self) -> Option<i64> {
        if !self.is_valid() {
            return None;
        }
        Some(
            days_from_civil(self.year, self.month, self.day) * SECONDS_PER_DAY
                + self.hour as i64 * 3600
                + self.minute as i64 * 60
                + self.second as i64,
        )
    }

    /// The date and time `seconds` after the Unix epoch. `None` if the
    /// year does not fit (some hundred billion years away).
    pub const fn from_unix(seconds: i64) -> Option<Self> {
        let days = seconds.div_euclid(SECONDS_PER_DAY);
        let second_of_day = seconds.rem_euclid(SECONDS_PER_DAY);
        // The inverse of `days_from_civil`.
        let shifted = days + 719_468;
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_from_march = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
        let month = if month_from_march < 10 {
            month_from_march + 3
        } else {
            month_from_march - 9
        };
        let year = year_of_era + era * 400 + (month <= 2) as i64;
        if year < i32::MIN as i64 || year > i32::MAX as i64 {
            return None;
        }
        Some(Self {
            year: year as i32,
            month: month as u8,
            day: day as u8,
            hour: (second_of_day / 3600) as u8,
            minute: (second_of_day / 60 % 60) as u8,
            second: (second_of_day % 60) as u8,
        })
    }

    /// The day of the week: 0 is Sunday, 6 Saturday.
    pub const fn weekday(&self) -> u8 {
        // 1970-01-01 was a Thursday.
        (days_from_civil(self.year, self.month, self.day) + 4).rem_euclid(7) as u8
    }
}

impl core::fmt::Display for DateTime {
    /// `2026-11-10 12:34:56`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> DateTime {
        DateTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    #[test]
    fn known_moments() {
        let cases = [
            (0, at(1970, 1, 1, 0, 0, 0), 4),
            (951_782_400, at(2000, 2, 29, 0, 0, 0), 2),
            (1_000_000_000, at(2001, 9, 9, 1, 46, 40), 0),
            (1_794_312_000, at(2026, 11, 10, 12, 0, 0), 2),
            (2_147_483_647, at(2038, 1, 19, 3, 14, 7), 2),
            (4_107_542_399, at(2100, 2, 28, 23, 59, 59), 0),
            (4_107_542_400, at(2100, 3, 1, 0, 0, 0), 1),
            (-1, at(1969, 12, 31, 23, 59, 59), 3),
            // The epoch of Windows file times.
            (-11_644_473_600, at(1601, 1, 1, 0, 0, 0), 1),
        ];
        for (seconds, moment, weekday) in cases {
            assert_eq!(DateTime::from_unix(seconds), Some(moment), "{seconds}");
            assert_eq!(moment.to_unix(), Some(seconds), "{moment}");
            assert_eq!(moment.weekday(), weekday, "{moment}");
        }
    }

    #[test]
    fn every_day_of_eight_centuries_round_trips() {
        // From 1600 to 2400: two full 400-year cycles around the epoch.
        let mut expected = at(1600, 1, 1, 13, 14, 15);
        let start = expected.to_unix().unwrap();
        for day in 0..(800 * 365 + 194) {
            let seconds = start + day * SECONDS_PER_DAY;
            assert_eq!(DateTime::from_unix(seconds), Some(expected));
            assert_eq!(expected.to_unix(), Some(seconds));
            // The next calendar day, by the calendar's own rules.
            if expected.day < days_in_month(expected.year, expected.month) {
                expected.day += 1;
            } else if expected.month < 12 {
                expected.month += 1;
                expected.day = 1;
            } else {
                expected.year += 1;
                expected.month = 1;
                expected.day = 1;
            }
        }
        assert_eq!(expected, at(2400, 1, 1, 13, 14, 15));
    }

    #[test]
    fn invalid_dates_are_refused() {
        for bad in [
            at(2026, 0, 1, 0, 0, 0),
            at(2026, 13, 1, 0, 0, 0),
            at(2026, 2, 29, 0, 0, 0),
            at(2100, 2, 29, 0, 0, 0),
            at(2026, 4, 31, 0, 0, 0),
            at(2026, 1, 0, 0, 0, 0),
            at(2026, 1, 1, 24, 0, 0),
            at(2026, 1, 1, 0, 60, 0),
            at(2026, 1, 1, 0, 0, 60),
        ] {
            assert_eq!(bad.to_unix(), None, "{bad:?}");
        }
        assert!(at(2024, 2, 29, 23, 59, 59).is_valid());
        assert!(at(2000, 2, 29, 0, 0, 0).is_valid());
    }

    #[test]
    fn formatting_and_extremes() {
        extern crate std;
        use std::string::ToString;
        assert_eq!(at(2026, 11, 10, 9, 5, 0).to_string(), "2026-11-10 09:05:00");
        assert!(DateTime::from_unix(i64::MAX).is_none());
        assert!(DateTime::from_unix(i64::MIN).is_none());
        let far = DateTime::from_unix(i64::from(i32::MAX) * 31_000_000).unwrap();
        assert_eq!(DateTime::from_unix(far.to_unix().unwrap()), Some(far));
    }
}
