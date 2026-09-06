//! Decimal normalization and price tokens shared by field readers.
use crate::ocr_confusion;
use regex::Regex;
use std::sync::OnceLock;

pub(super) fn re_price_end() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$?\s*(\d+\.\d{2})\s*$").unwrap())
}
pub(super) fn re_price_anywhere() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$?\s*(\d+\.\d{2})").unwrap())
}
pub(super) fn re_standalone_amount() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\$?\s*\d+\.\d{2}\s*$").unwrap())
}

/// A tax label, with or without the rate printed as part of it.
///
/// The rate suffix is why this is not a plain `\b(HST|…)\b`: the Bestco/Foody/C&C
/// POS family prints its 5% bucket as `hst5%`, with no separator, so the trailing
/// word boundary never lands and the row read as untaxed text. Accepting the
/// suffix makes `hst5%` an ordinary `HST 5%` row — the same shape as Costco's
/// `P(H)HST 13%` and Loblaw's `H=HST 13%`, which already matched — rather than a
/// token needing rules of its own.
///
/// The alternation ends in *either* a rate or a word boundary so `HSTX` still
/// fails to match; only the rate form is allowed to end on `%`, which is not a
/// word character and so cannot satisfy `\b` itself.
pub(super) fn re_tax_tokens() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(HST|GST|PST|TAX)(?:\s*\d{1,2}(?:\.\d+)?\s*%|\b)").unwrap())
}
pub(super) fn normalize_decimal_spacing(text: &str) -> String {
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
            && (i + 3 == bytes.len()
                || !(bytes[i + 3].is_ascii_digit() || is_digit_confusable(bytes[i + 3])))
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

/// Whether `b` is a letter OCR routinely prints in place of a digit — `O` for 0,
/// `l`/`I` for 1, `S` for 5, and the rest of [`ocr_confusion`]'s same-glyph tier.
///
/// Used only to widen a *negative* guard: the char after a suspected thousands
/// separator. `Win a $1,00o PC gift card` is `$1,000` with its last zero read as
/// `o`, and without this the comma repair sees a non-digit there, decides the
/// comma must be a decimal point, and manufactures a `$1.00` price out of survey
/// marketing copy — which then classifies as a gift-card tender.
///
/// [`ocr_confusion`]: crate::ocr_confusion
pub(super) fn is_digit_confusable(b: u8) -> bool {
    let ch = (b as char).to_ascii_uppercase();
    !ch.is_ascii_digit()
        && ('0'..='9').any(|d| {
            ocr_confusion::canonicalize_same_glyph(&ch.to_string())
                == ocr_confusion::canonicalize_same_glyph(&d.to_string())
        })
}
pub(super) fn parse_cents(token: &str) -> Option<i64> {
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
    Some(dollars * 100 + cents)
}
pub fn extract_price_from_line(line: &str) -> Option<i64> {
    let normalized = normalize_decimal_spacing(line);
    for regex in [re_price_end(), re_price_anywhere()] {
        if let Some(captures) = regex.captures(&normalized) {
            if let Some(token) = captures.get(1) {
                if let Some(value) = parse_cents(token.as_str()) {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Return the largest price found on the line, or None if no price is present.
/// Used to disambiguate cases like a single OCR line collapsing two columns
/// `TOTAL ... TOTAL TAX ... $74.55 $1.82` — the trailing price is the tax, but
/// the total is by definition the larger of the two.
pub(super) fn extract_max_price_from_line(line: &str) -> Option<i64> {
    let normalized = normalize_decimal_spacing(line);
    re_price_anywhere()
        .captures_iter(&normalized)
        .filter_map(|captures| captures.get(1).and_then(|m| parse_cents(m.as_str())))
        .max()
}

/// Every amount on `line`, in printed order.
pub(super) fn prices_in_line(line: &str) -> Vec<i64> {
    let normalized = normalize_decimal_spacing(line);
    re_price_anywhere()
        .captures_iter(&normalized)
        .filter_map(|captures| captures.get(1).and_then(|m| parse_cents(m.as_str())))
        .collect()
}
