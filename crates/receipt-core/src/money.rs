//! One money type for the whole parser.
//!
//! Before this existed the same amount was carried six different ways between
//! OCR and the ledger: `i64` cents in field extraction, `i64` scaled by 10 000
//! on the spatial item path, `i64` cents on the text item path, `String` in the
//! parse result, `i64` cents again for formatter arithmetic, and `String` across
//! the FFI. Six codecs converted between them, two of which (`cents_to_fixed`)
//! were defined twice with identical bodies.
//!
//! # Scale
//!
//! **Cents.** The 10 000 scale on the spatial path carried no information:
//! nothing sub-cent ever entered it — the only producer, `trailing_price_scaled`,
//! reads at most two fractional digits off the receipt — so the extra two digits
//! bought one more conversion and one more way to be wrong. If sub-cent input
//! ever becomes reachable, changing this constant is not sufficient: revisit
//! [`Money::from_scaled_4`] and the truncation note below at the same time.
//!
//! # Truncation
//!
//! Every codec this replaces truncated, and so does this type. Round-half-up
//! would be a *behaviour* change, and it is a no-op today only because nothing
//! sub-cent is reachable — which makes it a landmine the day that stops being
//! true, not a free improvement. If it is ever wanted, land it as its own commit
//! with its own before/after corpus diff, never inside a refactor gated on zero
//! movement.

use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// A string handed to [`Money::parse_strict`] that is not a money amount.
///
/// Carries the offending input so the message a user sees can quote what they
/// actually typed, which is the difference between "invalid price" and
/// "invalid price: 12x.99".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoneyParseError {
    pub input: String,
}

impl fmt::Display for MoneyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a valid amount: {:?}", self.input)
    }
}

impl std::error::Error for MoneyParseError {}

/// An amount of money, in cents, truncated. See the module docs.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct Money(i64);

impl Money {
    pub const ZERO: Money = Money(0);

    #[inline]
    pub const fn from_cents(cents: i64) -> Self {
        Money(cents)
    }

    #[inline]
    pub const fn cents(self) -> i64 {
        self.0
    }

    #[inline]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    #[inline]
    pub const fn abs(self) -> Self {
        Money(self.0.abs())
    }

    /// Convert from the retired 10 000 scale, truncating toward zero.
    ///
    /// Integer division in Rust truncates toward zero, which is what the old
    /// `format_scaled_currency` did by taking `abs()` first and dividing twice.
    #[inline]
    pub const fn from_scaled_4(scaled: i64) -> Self {
        Money(scaled / 100)
    }

    /// Parse a decimal string, rejecting anything that is not money.
    ///
    /// The counterpart to [`Money::from_decimal_str`], and the one to use on
    /// **input a human typed**. The lenient parser reads `"12x.99"` as `$0.00`
    /// and `"$12.34"` as `$0.34` — silently, with no way for a caller to tell
    /// those from a real zero. That is tolerable over text this crate emitted
    /// itself; it is not tolerable over a correction, where the whole point of
    /// the edit is that the number matters.
    ///
    /// Deliberately strict rather than forgiving: optional `-`, decimal digits,
    /// an optional `.` with at most two fractional digits, and at least one
    /// digit overall. No currency symbols, no thousands separators, no
    /// whitespace inside. Presentation-level tidying belongs in the UI that owns
    /// the text field — core cannot tell a `,` meant as a thousands separator
    /// from one meant as a decimal point, and guessing is how `1,50` becomes
    /// `$150.00`.
    ///
    /// Unlike the lenient parser this is also overflow-safe; `whole * 100` there
    /// wraps in release and panics in debug on absurd input.
    pub fn parse_strict(value: &str) -> Result<Money, MoneyParseError> {
        let bad = || MoneyParseError {
            input: value.to_string(),
        };
        if value.is_empty() {
            return Err(bad());
        }
        let (negative, unsigned) = match value.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, value),
        };

        let mut parts = unsigned.split('.');
        let whole_str = parts.next().unwrap_or_default();
        let frac_str = parts.next().unwrap_or("");
        // A second `.` means this is not a decimal number.
        if parts.next().is_some() {
            return Err(bad());
        }
        // "." alone, "-" alone, "-.": no digits at all.
        if whole_str.is_empty() && frac_str.is_empty() {
            return Err(bad());
        }
        if frac_str.len() > 2 {
            return Err(bad());
        }
        let digits_only = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if !digits_only(whole_str) || !digits_only(frac_str) {
            return Err(bad());
        }

        let whole: i64 = if whole_str.is_empty() {
            0
        } else {
            whole_str.parse().map_err(|_| bad())?
        };
        // "5" -> 0 cents of fraction; "5.1" -> 10; "5.12" -> 12.
        let frac: i64 = match frac_str.len() {
            0 => 0,
            1 => frac_str.parse::<i64>().map_err(|_| bad())? * 10,
            _ => frac_str.parse().map_err(|_| bad())?,
        };

        let cents = whole
            .checked_mul(100)
            .and_then(|c| c.checked_add(frac))
            .ok_or_else(bad)?;
        Ok(Money(if negative { -cents } else { cents }))
    }

    /// Parse a decimal string. **Lenient and total**: anything unparseable reads
    /// as zero, and more than two fractional digits truncate.
    ///
    /// This is deliberately bug-compatible with the `decimal_to_cents` it
    /// replaces, because it runs on the reformat path over text this crate
    /// previously emitted. Tightening it is a behaviour change and needs its own
    /// corpus diff.
    pub fn from_decimal_str(value: &str) -> Self {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Money::ZERO;
        }
        let negative = trimmed.starts_with('-');
        let unsigned = trimmed.trim_start_matches('-');
        let mut parts = unsigned.splitn(2, '.');
        let whole = parts.next().unwrap_or("0").parse::<i64>().unwrap_or(0);
        let frac_raw = parts.next().unwrap_or("0");
        let mut frac = frac_raw.chars().take(2).collect::<String>();
        while frac.len() < 2 {
            frac.push('0');
        }
        let frac_value = frac.parse::<i64>().unwrap_or(0);
        let value = whole * 100 + frac_value;
        Money(if negative { -value } else { value })
    }

    #[inline]
    pub fn checked_add(self, rhs: Money) -> Option<Money> {
        self.0.checked_add(rhs.0).map(Money)
    }

    #[inline]
    pub fn checked_sub(self, rhs: Money) -> Option<Money> {
        self.0.checked_sub(rhs.0).map(Money)
    }

    /// Multiply by a whole count, as in `quantity * unit price`.
    #[inline]
    pub fn checked_mul_int(self, rhs: i64) -> Option<Money> {
        self.0.checked_mul(rhs).map(Money)
    }
}

/// Beancount amount text: a sign, whole dollars, and exactly two decimals.
///
/// This is the single definition of what money looks like on the way out. The
/// spatial item path used to render four decimals here (`"6.9700"`); it now
/// renders two like every other path.
impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.abs();
        write!(f, "{sign}{}.{:02}", abs / 100, abs % 100)
    }
}

impl std::str::FromStr for Money {
    /// [`Money::from_decimal_str`] is total, so parsing cannot fail. This impl
    /// exists for `.parse()` call sites; it never returns `Err`.
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Money::from_decimal_str(s))
    }
}

/// Lenient, for literals and fixtures: `"6.97".into()`.
///
/// Delegates to [`Money::from_decimal_str`], so it shares that method's
/// bug-compatible leniency — unparseable text becomes zero rather than
/// failing. Prefer the named constructor in parsing code, where reading
/// `from_decimal_str` at the call site is the point.
impl From<&str> for Money {
    fn from(value: &str) -> Self {
        Money::from_decimal_str(value)
    }
}

impl Add for Money {
    type Output = Money;
    #[inline]
    fn add(self, rhs: Money) -> Money {
        Money(self.0 + rhs.0)
    }
}

impl Sub for Money {
    type Output = Money;
    #[inline]
    fn sub(self, rhs: Money) -> Money {
        Money(self.0 - rhs.0)
    }
}

impl Neg for Money {
    type Output = Money;
    #[inline]
    fn neg(self) -> Money {
        Money(-self.0)
    }
}

impl AddAssign for Money {
    #[inline]
    fn add_assign(&mut self, rhs: Money) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Money {
    #[inline]
    fn sub_assign(&mut self, rhs: Money) {
        self.0 -= rhs.0;
    }
}

impl Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        Money(iter.map(|m| m.0).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_the_retired_cents_to_fixed() {
        // Exactly the cases the two identical `cents_to_fixed` bodies produced.
        assert_eq!(Money::from_cents(0).to_string(), "0.00");
        assert_eq!(Money::from_cents(7).to_string(), "0.07");
        assert_eq!(Money::from_cents(697).to_string(), "6.97");
        assert_eq!(Money::from_cents(-697).to_string(), "-6.97");
        assert_eq!(Money::from_cents(-5).to_string(), "-0.05");
        assert_eq!(Money::from_cents(100_000).to_string(), "1000.00");
    }

    #[test]
    fn from_decimal_str_matches_the_retired_decimal_to_cents() {
        assert_eq!(Money::from_decimal_str("6.97"), Money::from_cents(697));
        assert_eq!(Money::from_decimal_str("-6.97"), Money::from_cents(-697));
        assert_eq!(Money::from_decimal_str(" 6.97 "), Money::from_cents(697));
        assert_eq!(Money::from_decimal_str("6"), Money::from_cents(600));
        assert_eq!(Money::from_decimal_str("6.9"), Money::from_cents(690));
        // truncates, never rounds
        assert_eq!(Money::from_decimal_str("6.999"), Money::from_cents(699));
        assert_eq!(Money::from_decimal_str("-6.999"), Money::from_cents(-699));
        // total: garbage reads as zero rather than failing
        assert_eq!(Money::from_decimal_str(""), Money::ZERO);
        assert_eq!(Money::from_decimal_str("   "), Money::ZERO);
        assert_eq!(Money::from_decimal_str("abc"), Money::ZERO);
    }

    /// The cases that motivated `parse_strict`: each of these is a number the
    /// lenient parser turns into a *plausible* wrong amount rather than a
    /// refusal, which is invisible once it reaches a ledger.
    #[test]
    fn parse_strict_rejects_what_the_lenient_parser_silently_mangles() {
        for (input, lenient_reads_as) in [
            ("12x.99", Money::from_cents(99)),
            ("$12.34", Money::from_cents(34)),
            ("1,234.56", Money::from_cents(56)),
            ("12.34.56", Money::from_cents(1234)),
            ("abc", Money::ZERO),
            ("", Money::ZERO),
            (" 12.34", Money::from_cents(1234)),
            ("12.345", Money::from_cents(1234)),
        ] {
            assert_eq!(
                Money::from_decimal_str(input),
                lenient_reads_as,
                "lenient reading of {input:?} changed — the point of this test is \
                 the contrast, so re-check the strict expectation below too"
            );
            assert!(
                Money::parse_strict(input).is_err(),
                "parse_strict accepted {input:?}"
            );
        }
    }

    #[test]
    fn parse_strict_accepts_the_shapes_a_person_or_this_crate_writes() {
        for (input, expected) in [
            ("12.34", Money::from_cents(1234)),
            ("0.00", Money::ZERO),
            ("-1.50", Money::from_cents(-150)),
            ("12", Money::from_cents(1200)),
            ("12.5", Money::from_cents(1250)),
            (".50", Money::from_cents(50)),
            ("-0.01", Money::from_cents(-1)),
        ] {
            assert_eq!(Money::parse_strict(input), Ok(expected), "input {input:?}");
        }
    }

    /// Everything `Money` renders must parse back exactly, or the round trip
    /// through the FFI would start rejecting this crate's own output.
    #[test]
    fn parse_strict_round_trips_every_rendered_amount() {
        for cents in [0, 1, -1, 99, 100, -12_345, 22_197, i64::MAX / 100] {
            let m = Money::from_cents(cents);
            assert_eq!(Money::parse_strict(&m.to_string()), Ok(m), "cents {cents}");
        }
    }

    /// The lenient parser computes `whole * 100` unchecked, so this input panics
    /// in debug and wraps in release. Strict parsing reports it instead.
    #[test]
    fn parse_strict_reports_overflow_rather_than_wrapping() {
        // i64::MAX is 9_223_372_036_854_775_807 cents, so this is the largest
        // amount that fits exactly — it must parse, not be rejected as overflow.
        assert_eq!(
            Money::parse_strict("92233720368547758.07"),
            Ok(Money::from_cents(i64::MAX))
        );
        // One cent more, and a whole part too large to scale at all.
        assert!(Money::parse_strict("92233720368547758.08").is_err());
        assert!(Money::parse_strict("999999999999999999999").is_err());
    }

    #[test]
    fn from_scaled_4_truncates_toward_zero_like_format_scaled_currency() {
        assert_eq!(Money::from_scaled_4(69_700), Money::from_cents(697));
        assert_eq!(Money::from_scaled_4(-69_700), Money::from_cents(-697));
        // sub-cent input is unreachable today; if it ever is not, this is the
        // line that silently drops it.
        assert_eq!(Money::from_scaled_4(69_799), Money::from_cents(697));
        assert_eq!(Money::from_scaled_4(-69_799), Money::from_cents(-697));
    }

    #[test]
    fn round_trips_through_text() {
        for cents in [-100_000i64, -697, -5, 0, 5, 697, 100_000] {
            let m = Money::from_cents(cents);
            assert_eq!(Money::from_decimal_str(&m.to_string()), m, "{cents}");
        }
    }

    #[test]
    fn arithmetic() {
        assert_eq!(
            Money::from_cents(100) + Money::from_cents(23),
            Money::from_cents(123)
        );
        assert_eq!(
            Money::from_cents(100) - Money::from_cents(23),
            Money::from_cents(77)
        );
        assert_eq!(-Money::from_cents(100), Money::from_cents(-100));
        assert_eq!(
            Money::from_cents(100).checked_mul_int(3),
            Some(Money::from_cents(300))
        );
        assert_eq!(
            Money::from_cents(i64::MAX).checked_add(Money::from_cents(1)),
            None
        );
        let sum: Money = [Money::from_cents(1), Money::from_cents(2)]
            .into_iter()
            .sum();
        assert_eq!(sum, Money::from_cents(3));
    }
}
