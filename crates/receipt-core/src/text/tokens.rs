//! Price token parsing and decimal normalization shared by row and quantity readers.
use super::patterns::*;
use crate::money::Money;

pub(crate) fn normalize_decimal_spacing(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' && i > 0 && bytes[i - 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j > i + 1
                && j + 1 < bytes.len()
                && bytes[j].is_ascii_digit()
                && bytes[j + 1].is_ascii_digit()
                && (j + 2 == bytes.len() || !bytes[j + 2].is_ascii_digit())
            {
                out.push('.');
                out.push(bytes[j] as char);
                out.push(bytes[j + 1] as char);
                i = j + 2;
                continue;
            }
        }
        // OCR sometimes reads a price's decimal point as a comma ("0,99").
        // Only treat a comma as a decimal point when it sits directly between
        // a digit and exactly two fraction digits, so thousands separators
        // ("1,000") and prose ("Anytown, ON") are left untouched.
        if bytes[i] == b','
            && i > 0
            && bytes[i - 1].is_ascii_digit()
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && (i + 3 == bytes.len() || !bytes[i + 3].is_ascii_digit())
        {
            out.push('.');
            out.push(bytes[i + 1] as char);
            out.push(bytes[i + 2] as char);
            i += 3;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

pub(super) fn parse_cents(token: &str) -> Option<Money> {
    let trimmed = token.trim();
    let (whole, frac) = trimmed.split_once('.')?;
    if whole.is_empty() || frac.len() != 2 {
        return None;
    }
    if !whole.chars().all(|ch| ch.is_ascii_digit()) || !frac.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let dollars = whole.parse::<i64>().ok()?;
    let cents = frac.parse::<i64>().ok()?;
    Some(Money::from_cents(dollars * 100 + cents))
}

pub(crate) fn extract_trailing_price_cents(line: &str) -> Option<(Money, bool, usize)> {
    let captures = re_trailing_price().captures(line)?;
    let cents = parse_cents(captures.get(1)?.as_str())?;
    let trailing_minus = captures.get(2).map(|m| m.as_str() == "-").unwrap_or(false);
    let start = captures.get(1)?.start();
    // Leading-minus discount convention (e.g. Asian-grocery lines like
    // "D9 -$1.96"): a '-' glued to the price — directly or through a '$' —
    // marks a discount, complementing Costco's trailing-minus form. Require
    // the '-' to sit at a token boundary or against a '$' so mid-token
    // hyphens ("ITEM-1.96") and " - 1.96" separators are not mis-signed.
    let leading_minus = {
        let prefix = &line[..start];
        let had_dollar = prefix.ends_with('$');
        let stripped = prefix.strip_suffix('$').unwrap_or(prefix);
        match stripped.strip_suffix('-') {
            Some(rest) => had_dollar || rest.is_empty() || rest.ends_with(char::is_whitespace),
            None => false,
        }
    };
    let is_discount = trailing_minus || leading_minus;
    Some((if is_discount { -cents } else { cents }, is_discount, start))
}
