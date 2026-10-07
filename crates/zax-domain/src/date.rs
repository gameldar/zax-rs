//! Calendar dates, with no time of day, no time zone and no clock.
//!
//! Today's date is the caller's to supply, so everything that depends on it
//! can be tested with a literal.

use std::fmt;

use thiserror::Error;

/// A day of the proleptic Gregorian calendar, held as days since 1970-01-01
/// so ordering and arithmetic are integer operations. Every value is a real
/// day: the constructors refuse anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    days: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DateError {
    #[error("`{0}` is not a date; write it as YYYY-MM-DD")]
    Format(String),
    #[error("`{0}` is not a day of the calendar")]
    NoSuchDay(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    pub fn short(self) -> &'static str {
        match self {
            Self::Mon => "Mon",
            Self::Tue => "Tue",
            Self::Wed => "Wed",
            Self::Thu => "Thu",
            Self::Fri => "Fri",
            Self::Sat => "Sat",
            Self::Sun => "Sun",
        }
    }

    /// `mon` or `monday`, in any case.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "mon" | "monday" => Some(Self::Mon),
            "tue" | "tuesday" => Some(Self::Tue),
            "wed" | "wednesday" => Some(Self::Wed),
            "thu" | "thursday" => Some(Self::Thu),
            "fri" | "friday" => Some(Self::Fri),
            "sat" | "saturday" => Some(Self::Sat),
            "sun" | "sunday" => Some(Self::Sun),
            _ => None,
        }
    }

    /// Monday is 0. Nothing in this crate calls it yet; it is for weekday
    /// arithmetic on top of `Date`.
    #[allow(dead_code)]
    pub(crate) fn index(self) -> i64 {
        match self {
            Self::Mon => 0,
            Self::Tue => 1,
            Self::Wed => 2,
            Self::Thu => 3,
            Self::Fri => 4,
            Self::Sat => 5,
            Self::Sun => 6,
        }
    }

    fn from_index(index: i64) -> Self {
        match index.rem_euclid(7) {
            0 => Self::Mon,
            1 => Self::Tue,
            2 => Self::Wed,
            3 => Self::Thu,
            4 => Self::Fri,
            5 => Self::Sat,
            _ => Self::Sun,
        }
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

impl Date {
    pub fn from_ymd(year: i32, month: u32, day: u32) -> Result<Self, DateError> {
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return Err(DateError::NoSuchDay(format!(
                "{year:04}-{month:02}-{day:02}"
            )));
        }
        Ok(Self {
            days: days_from_civil(i64::from(year), i64::from(month), i64::from(day)),
        })
    }

    /// Exactly `YYYY-MM-DD`, ASCII digits only.
    pub fn parse_iso(raw: &str) -> Result<Self, DateError> {
        let bytes = raw.as_bytes();
        let shaped = bytes.len() == 10
            && bytes.iter().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    *b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            });
        if !shaped {
            return Err(DateError::Format(raw.to_owned()));
        }
        let part = |from: usize, to: usize| raw.get(from..to).and_then(|s| s.parse::<u32>().ok());
        match (part(0, 4), part(5, 7), part(8, 10)) {
            (Some(year), Some(month), Some(day)) => {
                let year = i32::try_from(year).map_err(|_| DateError::Format(raw.to_owned()))?;
                Self::from_ymd(year, month, day).map_err(|_| DateError::NoSuchDay(raw.to_owned()))
            }
            _ => Err(DateError::Format(raw.to_owned())),
        }
    }

    pub fn add_days(self, n: i64) -> Self {
        Self {
            days: self.days + n,
        }
    }

    /// How many days from `self` to `later`; negative when `later` is earlier.
    pub fn days_until(self, later: Date) -> i64 {
        later.days - self.days
    }

    pub fn weekday(self) -> Weekday {
        // 1970-01-01 was a Thursday, which is index 3.
        Weekday::from_index(self.days + 3)
    }

    pub fn year(self) -> i32 {
        i32::try_from(civil_from_days(self.days).0).unwrap_or(i32::MAX)
    }

    pub fn month(self) -> u32 {
        u32::try_from(civil_from_days(self.days).1).unwrap_or(1)
    }

    pub fn day(self) -> u32 {
        u32::try_from(civil_from_days(self.days).2).unwrap_or(1)
    }

    pub fn month_short(self) -> &'static str {
        let index = usize::try_from(self.month().saturating_sub(1)).unwrap_or(0);
        MONTHS.get(index).copied().unwrap_or("Jan")
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (y, m, d) = civil_from_days(self.days);
        write!(f, "{y:04}-{m:02}-{d:02}")
    }
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

// Howard Hinnant's `days_from_civil` and `civil_from_days`
// (https://howardhinnant.github.io/date_algorithms.html).

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn d(raw: &str) -> Date {
        Date::parse_iso(raw).unwrap()
    }

    #[test]
    fn an_iso_date_round_trips_through_display() {
        assert_eq!(d("2026-10-07").to_string(), "2026-10-07");
        assert_eq!(d("0999-01-02").to_string(), "0999-01-02");
    }

    #[test]
    fn only_the_iso_shape_parses() {
        for bad in [
            "2026-1-07",
            "2026/10/07",
            "20261007",
            "2026-10-07 ",
            "someday",
            "",
            "２０２６-10-07",
        ] {
            assert!(
                matches!(Date::parse_iso(bad), Err(DateError::Format(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_day_the_calendar_does_not_have_is_refused() {
        assert!(matches!(
            Date::parse_iso("2026-02-29"),
            Err(DateError::NoSuchDay(_))
        ));
        assert!(matches!(
            Date::parse_iso("2026-13-01"),
            Err(DateError::NoSuchDay(_))
        ));
        assert!(matches!(
            Date::parse_iso("2026-04-31"),
            Err(DateError::NoSuchDay(_))
        ));
        assert!(matches!(
            Date::parse_iso("2026-10-00"),
            Err(DateError::NoSuchDay(_))
        ));
        assert!(Date::parse_iso("2024-02-29").is_ok());
        assert!(Date::parse_iso("2000-02-29").is_ok());
        assert!(Date::parse_iso("1900-02-29").is_err());
    }

    #[test]
    fn days_carry_across_months_and_years() {
        assert_eq!(d("2026-12-31").add_days(1), d("2027-01-01"));
        assert_eq!(d("2026-03-01").add_days(-1), d("2026-02-28"));
        assert_eq!(d("2024-03-01").add_days(-1), d("2024-02-29"));
        assert_eq!(d("2026-10-07").days_until(d("2026-10-05")), -2);
        assert_eq!(d("2026-10-07").days_until(d("2027-10-07")), 365);
    }

    #[test]
    fn weekdays_are_right_on_known_dates() {
        assert_eq!(d("1970-01-01").weekday(), Weekday::Thu);
        assert_eq!(d("2026-10-07").weekday(), Weekday::Wed);
        assert_eq!(d("2026-10-05").weekday(), Weekday::Mon);
        assert_eq!(d("2000-02-29").weekday(), Weekday::Tue);
    }

    #[test]
    fn the_parts_and_names_read_back() {
        let date = d("2026-10-07");
        assert_eq!((date.year(), date.month(), date.day()), (2026, 10, 7));
        assert_eq!(date.month_short(), "Oct");
        assert_eq!(date.weekday().short(), "Wed");
    }

    #[test]
    fn a_weekday_parses_short_or_long_in_any_case() {
        assert_eq!(Weekday::parse("fri"), Some(Weekday::Fri));
        assert_eq!(Weekday::parse("Friday"), Some(Weekday::Fri));
        assert_eq!(Weekday::parse("THU"), Some(Weekday::Thu));
        assert_eq!(Weekday::parse("fr"), None);
    }

    #[test]
    fn dates_order_by_day() {
        assert!(d("2026-10-07") < d("2026-10-08"));
        assert!(d("2025-12-31") < d("2026-01-01"));
    }
}
