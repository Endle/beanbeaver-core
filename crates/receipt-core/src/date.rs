//! One calendar date, validated on construction.
//!
//! Dates moved through this crate as bare `(i32, u32, u32)` tuples in five
//! signatures plus `ParsedReceiptData.date`, alongside a separate `SimpleDate`
//! struct inside `receipt_fields`. A tuple offers no protection against
//! day/month transposition, which is the live failure mode for ambiguous North
//! American receipt dates — `03/04/2026` is March 4th or April 3rd depending on
//! the merchant, and nothing in `(i32, u32, u32)` says which slot is which.
//!
//! Validation lives in [`Date::new`], which is the only way to build one, so an
//! impossible date cannot exist. That is the same check `receipt_fields`'
//! `safe_date` performed, moved here and made unavoidable: the year range
//! rejects digit runs that are really SKUs or barcodes (LCBO's Baby Duck SKU
//! `00001123` parsed as `0000-11-23`), and the day is checked against the
//! actual length of the month, leap years included.

use std::fmt;

/// A calendar date that is known to exist. Build with [`Date::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: i32,
    month: u32,
    day: u32,
}

impl Date {
    /// Returns `None` for any date that cannot exist, and for years outside
    /// 1990..=2100 — outside that band a "date" on a receipt is a product code.
    pub fn new(year: i32, month: u32, day: u32) -> Option<Self> {
        if !(1990..=2100).contains(&year) {
            return None;
        }
        if !(1..=12).contains(&month) || day < 1 {
            return None;
        }
        if day > days_in_month(year, month) {
            return None;
        }
        Some(Date { year, month, day })
    }

    #[inline]
    pub const fn year(self) -> i32 {
        self.year
    }

    #[inline]
    pub const fn month(self) -> u32 {
        self.month
    }

    #[inline]
    pub const fn day(self) -> u32 {
        self.day
    }

    /// `(year, month, day)`, for the few callers that still want the parts.
    #[inline]
    pub const fn ymd(self) -> (i32, u32, u32) {
        (self.year, self.month, self.day)
    }

    /// Parse `YYYY-MM-DD`. Rejects anything else, including valid-looking text
    /// with the wrong separators or field widths.
    pub fn parse_iso(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let year = text[0..4].parse::<i32>().ok()?;
        let month = text[5..7].parse::<u32>().ok()?;
        let day = text[8..10].parse::<u32>().ok()?;
        Date::new(year, month, day)
    }
}

/// ISO `YYYY-MM-DD` — the form beancount wants and the only one this crate emits.
impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_real_dates() {
        assert!(Date::new(2026, 8, 23).is_some());
        assert!(Date::new(2024, 2, 29).is_some(), "2024 is a leap year");
        assert!(Date::new(1990, 1, 1).is_some());
        assert!(Date::new(2100, 12, 31).is_some());
    }

    #[test]
    fn rejects_impossible_dates() {
        assert!(Date::new(2026, 2, 29).is_none(), "2026 is not a leap year");
        assert!(Date::new(2026, 13, 1).is_none());
        assert!(Date::new(2026, 0, 1).is_none());
        assert!(Date::new(2026, 4, 31).is_none(), "April has 30 days");
        assert!(Date::new(2026, 1, 0).is_none());
        assert!(Date::new(2026, 1, 32).is_none());
    }

    #[test]
    fn rejects_years_that_are_really_product_codes() {
        // LCBO's Baby Duck SKU "00001123" used to parse as 0000-11-23.
        assert!(Date::new(0, 11, 23).is_none());
        assert!(Date::new(1989, 12, 31).is_none());
        assert!(Date::new(2101, 1, 1).is_none());
    }

    #[test]
    fn century_leap_rule() {
        assert!(Date::new(2000, 2, 29).is_some(), "2000 divisible by 400");
        assert!(Date::new(2100, 2, 29).is_none(), "2100 is not a leap year");
    }

    #[test]
    fn display_is_iso_and_zero_padded() {
        assert_eq!(Date::new(2026, 8, 23).unwrap().to_string(), "2026-08-23");
        assert_eq!(Date::new(2026, 1, 2).unwrap().to_string(), "2026-01-02");
    }

    #[test]
    fn parse_iso_round_trips_and_is_strict() {
        let d = Date::new(2026, 8, 23).unwrap();
        assert_eq!(Date::parse_iso(&d.to_string()), Some(d));
        assert_eq!(Date::parse_iso("2026-8-23"), None, "needs zero padding");
        assert_eq!(Date::parse_iso("2026/08/23"), None, "needs dashes");
        assert_eq!(Date::parse_iso("2026-02-30"), None, "still validated");
        assert_eq!(Date::parse_iso(""), None);
    }
}
