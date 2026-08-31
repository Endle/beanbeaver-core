use crate::date::Date;
use regex::Regex;
use std::cmp::Ordering;
use std::sync::OnceLock;

use crate::ocr_confusion;

#[derive(Clone, Debug)]
struct RankedDateCandidate {
    score: i32,
    line_index: usize,
    start: usize,
    date: Date,
}

/// Marks a line as date context, which is what lets `extract_date` consider the
/// year-first (`26/08/02`) reading at all — see the `ymd2` gate in
/// [`extract_date`], where a missing hint *discards* that candidate outright.
///
/// Matches the `DATE` prefix rather than whole words, because the suffix is
/// where OCR damage lands and it carries no information. A No Frills receipt
/// printing `DateTime: 26/08/02` came back as `Datelime`, and the lost word
/// boundary after `DATE` cost the hint, the year-first reading, and finally the
/// date itself: 2026-08-02 became 2002-08-26. Keying on the prefix survives any
/// mangling of `TIME` — `DATETIME`, `DATELIME`, `DATEIIME` all hint alike.
///
/// `\bDATE` still requires a boundary *before* the prefix, so `UPDATE` does not
/// match.
fn re_date_context_hint() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(DATE\w*|TRANS(?:ACTION)?\s*DATE\w*)").unwrap())
}

fn re_separated_date() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(^|[^0-9])(\d{1,4})[./-](\d{1,2})[./-](\d{1,4})([^0-9]|$)").unwrap()
    })
}

fn re_compact_date() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(^|[^0-9])(\d{4})(\d{2})(\d{2})([^0-9]|$)").unwrap())
}

fn re_month_name_date() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\w*\s+(\d{1,2}),?\s+(\d{4})\b",
        )
        .unwrap()
    })
}

// Day-first month-name dates, e.g. "22-May-2026" or "22 May 2026". The month
// may carry an abbreviation period ("02-Apr.-2026", Clover's format).
fn re_dmy_month_name_date() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(\d{1,2})[-\s]+(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\w*\.?[-\s]+(\d{4})\b",
        )
        .unwrap()
    })
}

/// Whether a date is explicitly labelled as a return deadline rather than the
/// transaction date.
///
/// The label can sit one or two OCR lines above the date itself (LCBO prints an
/// English line, a French line, then the deadline), so inspecting only the
/// candidate line is insufficient. A date between that label and the current
/// line closes the context: it is the deadline itself, and a following date is
/// free to be the transaction timestamp. Returning no date is safer than
/// booking a purchase on the last day it may be returned.
fn is_return_deadline_context(lines: &[String], line_index: usize) -> bool {
    let start = line_index.saturating_sub(2);
    let context = lines[start..=line_index].join(" ").to_ascii_uppercase();
    let has_return_label =
        context.contains("DATE") && (context.contains("RETURN") || context.contains("RETOUR"));
    let has_intervening_date = lines[start..line_index].iter().any(|line| {
        re_separated_date().is_match(line)
            || re_compact_date().is_match(line)
            || re_month_name_date().is_match(line)
            || re_dmy_month_name_date().is_match(line)
    });
    has_return_label && !has_intervening_date
}

fn month_number_from_name(name: &str) -> Option<i32> {
    match name.get(..3).unwrap_or("").to_ascii_lowercase().as_str() {
        "jan" => Some(1),
        "feb" => Some(2),
        "mar" => Some(3),
        "apr" => Some(4),
        "may" => Some(5),
        "jun" => Some(6),
        "jul" => Some(7),
        "aug" => Some(8),
        "sep" => Some(9),
        "oct" => Some(10),
        "nov" => Some(11),
        "dec" => Some(12),
        _ => None,
    }
}

fn re_price_end() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$?\s*(\d+\.\d{2})\s*$").unwrap())
}

fn re_price_anywhere() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\$?\s*(\d+\.\d{2})").unwrap())
}

fn re_standalone_amount() -> &'static Regex {
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
fn re_tax_tokens() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(HST|GST|PST|TAX)(?:\s*\d{1,2}(?:\.\d+)?\s*%|\b)").unwrap())
}

fn normalize_decimal_spacing(text: &str) -> String {
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
fn is_digit_confusable(b: u8) -> bool {
    let ch = (b as char).to_ascii_uppercase();
    !ch.is_ascii_digit()
        && ('0'..='9').any(|d| {
            ocr_confusion::canonicalize_same_glyph(&ch.to_string())
                == ocr_confusion::canonicalize_same_glyph(&d.to_string())
        })
}

fn parse_cents(token: &str) -> Option<i64> {
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
fn extract_max_price_from_line(line: &str) -> Option<i64> {
    let normalized = normalize_decimal_spacing(line);
    re_price_anywhere()
        .captures_iter(&normalized)
        .filter_map(|captures| captures.get(1).and_then(|m| parse_cents(m.as_str())))
        .max()
}

/// Every amount on `line`, in printed order.
fn prices_in_line(line: &str) -> Vec<i64> {
    let normalized = normalize_decimal_spacing(line);
    re_price_anywhere()
        .captures_iter(&normalized)
        .filter_map(|captures| captures.get(1).and_then(|m| parse_cents(m.as_str())))
        .collect()
}

/// Public total extractor: the raw label-scan pick, a currency-symbol repair,
/// then a guarded reconciliation against the payment block (see
/// `reconcile_total_with_charge`).
pub fn extract_total(lines: &[String]) -> i64 {
    let raw = repair_leading_currency_digit(lines, extract_total_raw(lines));
    reconcile_total_with_charge(lines, raw)
}

/// A `$` read as a `5`: Costco's "TOTAL $173.15" comes back as one detection
/// reading `5173.15`, thirty times the real amount.
///
/// `5173.15` is a perfectly good number, so the shape alone proves nothing —
/// this fires only when the receipt's own arithmetic contradicts it and the
/// repaired reading is the one that adds up. Both corroborators are exact-match,
/// not tolerances: `SUBTOTAL + TAX`, or an amount the payment block prints.
///
/// Only a leading `5` is stripped, and only when the remaining amount is still
/// non-empty. This does not generalise to other glyphs on purpose: `$` and `5`
/// are the confusable pair that occurs here, and every extra digit admitted
/// widens the space of numbers this could silently rewrite.
///
/// Surfaced by the deskew sweep. Before it, the Costco fixture's TOTAL label was
/// grouped with a *different* `173.15` elsewhere on the receipt and read
/// correctly by luck; once the summary block was aligned, TOTAL claimed the
/// amount actually printed beside it and the misread became visible.
fn repair_leading_currency_digit(lines: &[String], candidate: i64) -> i64 {
    if candidate <= 0 {
        return candidate;
    }
    let dollars = (candidate / 100).to_string();
    if !dollars.starts_with('5') || dollars.len() < 2 {
        return candidate;
    }
    let Ok(stripped_dollars) = dollars[1..].parse::<i64>() else {
        return candidate;
    };
    let repaired = stripped_dollars * 100 + candidate % 100;
    if repaired <= 0 {
        return candidate;
    }

    let sums_to_repaired = match (extract_subtotal(lines), extract_tax(lines)) {
        (Some(subtotal), Some(tax)) => subtotal + tax == repaired,
        _ => false,
    };
    let charged_repaired = lines.iter().enumerate().any(|(idx, line)| {
        let upper = line.to_ascii_uppercase();
        let is_payment = upper.contains("AMOUNT:")
            || upper.contains("CREDIT TN")
            || matches!(classify_tender_line(&upper), Some("card"));
        is_payment && tender_amount_for_line(lines, idx) == Some(repaired)
    });

    if sums_to_repaired || charged_repaired {
        repaired
    } else {
        candidate
    }
}

/// When the on-device box-position artifact mis-pairs the TOTAL label with a
/// neighbouring amount (the tax row, or nothing → 0), the printed charged amount
/// is more reliable. Prefer an amount corroborated by **two** payment-block
/// lines (a card tender and/or an "AMOUNT:" echo), but only when it **exceeds**
/// the raw candidate — so cash-with-change and split-tender receipts, where the
/// real total legitimately exceeds the card portion, are left untouched. On a
/// correctly-paired receipt the candidate already equals the charged amount, so
/// this never fires, keeping desktop output (cached baseline + parity) unchanged.
fn reconcile_total_with_charge(lines: &[String], candidate: i64) -> i64 {
    let mut payment_amounts: Vec<i64> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let upper = line.to_ascii_uppercase();
        // "CREDIT TN" is the Loblaws-family card slip's echo of the charged
        // amount, corroborating the "Account: VISA …" line above it. It is
        // recognized here only — adding it to `classify_tender_line` would
        // make `extract_tenders` double-count the charge against the card
        // tender it echoes.
        let is_payment = upper.contains("AMOUNT:")
            || upper.contains("CREDIT TN")
            || matches!(classify_tender_line(&upper), Some("card"));
        if is_payment {
            if let Some(cents) = tender_amount_for_line(lines, idx) {
                if cents > 0 {
                    payment_amounts.push(cents);
                }
            }
        }
    }
    // Nothing was handed back AND only one instrument paid ⇒ that one tender is
    // the grand total, so a single payment line suffices as corroboration (a
    // mis-grouped TOTAL row can pick up the tax amount, leaving the true total
    // only on the tender line). Otherwise the two-line requirement stays.
    //
    // Both halves are load-bearing, and the second was missing. `payment_amounts`
    // collects only card lines and their echoes — cash is deliberately excluded —
    // so on a split-tender slip the lone card amount is a *portion* of the total
    // and a zero-change line makes it look like the whole. The `a > candidate`
    // guard below does not save it: that only holds while the candidate is
    // already right, and this function exists for when it isn't. A slip paying
    // VISA 23.41 + CASH 10.00 against a mis-grouped `TOTAL 2.41` adopted 23.41
    // and reported it as the total with no warning.
    //
    // Reading the change amount matters just as much as testing it. This used to
    // ask `ends_with("0.00")`, which is also true of `10.00`, `20.00` and
    // `100.00` — so every whole-ten-dollar change amount, the ordinary case for
    // cash, counted as zero change and unlocked the relaxation on exactly the
    // receipts the two-line rule was there to protect.
    let zero_change = lines.iter().enumerate().any(|(idx, line)| {
        if !re_change_label().is_match(&line.to_ascii_uppercase()) {
            return false;
        }
        // "CHANGE DUE $12.19 $0.00" prints the amount tendered first and the
        // change second, so the change is the LAST amount on the row.
        match prices_in_line(line).last().copied() {
            Some(amount) => amount == 0,
            None => tender_amount_for_line(lines, idx) == Some(0),
        }
    });
    let single_instrument = extract_tenders(lines).len() <= 1;
    let min_corroboration = if zero_change && single_instrument {
        1
    } else {
        2
    };
    let mut corroborated: Vec<i64> = payment_amounts
        .iter()
        .copied()
        .filter(|&a| {
            a > candidate
                && payment_amounts.iter().filter(|&&b| b == a).count() >= min_corroboration
        })
        .collect();
    corroborated.sort_unstable();
    corroborated.dedup();
    match corroborated.as_slice() {
        [only] => *only,
        _ => candidate,
    }
}

/// A savings summary is never the grand total, however the chain words it.
///
/// This was two literal phrases in [`extract_total_raw`]'s exclusion list,
/// `TOTAL SAVINGS` and `TOTAL SAVED`, which only match when the words are
/// adjacent. Food Basics prints `Total of your savings 6.73` as the last
/// TOTAL-bearing line on the receipt, and the scan runs *upward* — so it was
/// the first candidate found, and a $6.96 receipt reported a $6.73 total.
///
/// Allowing a short gap covers the wording variants without loosening what the
/// rule means. Checked against every TOTAL-bearing line in the corpus: this
/// excludes `Total Savings` and `Your Total Savings` — both already excluded
/// by the literals it replaces — and no real grand total, including the ones
/// that merely mention the word (`AMOUNT OF THE TOTAL SHOWN ABOVE`,
/// `Total after Tax`, `TOTAL PURCHASE`).
fn re_total_savings_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)TOTAL\b.{0,15}?\bSAV(?:E|ED|ING|INGS)\b").unwrap())
}

fn extract_total_raw(lines: &[String]) -> i64 {
    const EXCLUDED_PHRASES: [&str; 4] = [
        "TOTAL DISCOUNT",
        "TOTAL DISCOUNT(S)",
        "TOTAL NUMBER OF ITEMS",
        "TOTAL ITEMS",
    ];

    for reversed_index in 0..lines.len() {
        let idx = lines.len() - 1 - reversed_index;
        let line_upper = lines[idx].to_ascii_uppercase();
        if line_upper.contains("TOTAL NUMBER") {
            continue;
        }
        if re_total_savings_label().is_match(&line_upper) {
            continue;
        }
        if EXCLUDED_PHRASES
            .iter()
            .any(|phrase| line_upper.contains(phrase))
        {
            continue;
        }
        // An "AFTER TAX" line is the grand total even when OCR mangles the
        // word "Total" itself (e.g. Foody Mart's "lotal after Tax 163.95",
        // T->l). Match on that phrase too. Exclude any subtotal label via the
        // OCR-tolerant regex so a spaced "Sub Total" row is never mistaken for
        // the grand total when the real total line is unreadable.
        if (line_upper.contains("TOTAL") || line_upper.contains("AFTER TAX"))
            && !re_subtotal_label().is_match(&line_upper)
        {
            let prev_upper = if idx > 0 {
                lines[idx - 1].to_ascii_uppercase()
            } else {
                String::new()
            };
            let next_upper = if idx + 1 < lines.len() {
                lines[idx + 1].to_ascii_uppercase()
            } else {
                String::new()
            };
            if next_upper.contains("DISCOUNT") {
                continue;
            }
            // The same label can split the other way once line grouping shifts:
            // Costco's "TOTAL DISCOUNT(S) $9.00" can regroup as "DISCOUNT(S)"
            // then "TOTAL $ 9.00", leaving a TOTAL row holding the discount.
            // Only a *bare* discount label means the qualifier was split off —
            // a real discount row carries its own amount, and suppressing the
            // total after one of those would be wrong.
            if prev_upper.contains("DISCOUNT") && extract_price_from_line(&lines[idx - 1]).is_none()
            {
                continue;
            }
            if prev_upper.contains("TOTAL NUMBER OF ITEMS SOLD") {
                continue;
            }
            if let Some(amount) = extract_price_from_line(&lines[idx]) {
                if amount == 0
                    && line_upper.contains("AFTER TAX")
                    && idx + 1 < lines.len()
                    && re_standalone_amount().is_match(&lines[idx + 1])
                {
                    if let Some(next_amount) = extract_price_from_line(&lines[idx + 1]) {
                        return next_amount;
                    }
                }
                // A TOTAL row can also collect a *neighbouring* row's amount
                // when the price column leans up: FreshCo's gift-card tender
                // lands on the total row as "TOTAL $157.38 $116.24", and the
                // trailing-price pick takes the tender. The printed subtotal
                // and tax settle which one is the total — but only when their
                // sum is actually one of the amounts on this line, so a
                // receipt whose total legitimately differs from subtotal + tax
                // (fees, rounding, tax-inclusive pricing) is never overridden.
                if prices_in_line(&lines[idx]).len() > 1 {
                    if let (Some(subtotal), Some(tax)) =
                        (extract_subtotal(lines), extract_tax(lines))
                    {
                        let expected = subtotal + tax;
                        if expected != amount && prices_in_line(&lines[idx]).contains(&expected) {
                            return expected;
                        }
                    }
                }
                // Collapsed two-column TOTAL row: when the same line carries
                // both a TOTAL label and a TAX label (e.g. OCR mashes
                // "TOTAL | TOTAL TAX | $74.55 | $1.82" into one line), the
                // trailing price is the tax. Prefer the largest of the two,
                // which by definition is the total.
                if re_tax_tokens().is_match(&line_upper) {
                    if let Some(max_amount) = extract_max_price_from_line(&lines[idx]) {
                        if max_amount > amount {
                            return max_amount;
                        }
                    }
                }
                return amount;
            }
            if idx + 1 < lines.len() {
                if let Some(amount) = extract_price_from_line(&lines[idx + 1]) {
                    return amount;
                }
            }
            if idx > 0 {
                let prev_line_upper = lines[idx - 1].to_ascii_uppercase();
                let prev_is_tax_row = prev_line_upper.contains("TAX")
                    || prev_line_upper.contains("HST")
                    || prev_line_upper.contains("GST");
                if !prev_is_tax_row {
                    if let Some(amount) = extract_price_from_line(&lines[idx - 1]) {
                        return amount;
                    }
                } else if let Some(amount) = extract_price_from_line(&lines[idx - 1]) {
                    // Up-leaned line grouping shifts the whole summary block
                    // one row: SUBTOTAL shows the tax, TAX shows the total,
                    // TOTAL is bare. A genuine tax can never exceed the
                    // subtotal amount, so a larger value on the TAX row is
                    // the drifted grand total.
                    if extract_subtotal(lines).is_some_and(|subtotal| amount > subtotal) {
                        return amount;
                    }
                }
            }
            // Costco-style layout: the TOTAL label sits on its own line
            // ("TOTAL.") and the value lives further down in the payment
            // block as a standalone amount (typically a few lines above
            // an "AMOUNT :" label that OCR linearization reorders).
            // Scan forward for the first standalone decimal, stopping
            // at section boundaries that can't be the total.
            const FORWARD_SCAN_WINDOW: usize = 20;
            let upper_bound = (idx + 1 + FORWARD_SCAN_WINDOW).min(lines.len());
            for scan_line in lines.iter().take(upper_bound).skip(idx + 1) {
                let scan_upper = scan_line.to_ascii_uppercase();
                if scan_upper.contains("SUBTOTAL")
                    || scan_upper.contains("CHANGE")
                    || scan_upper.contains("BALANCE")
                {
                    break;
                }
                if re_standalone_amount().is_match(scan_line) {
                    if let Some(amount) = extract_price_from_line(scan_line) {
                        return amount;
                    }
                }
            }
        }
    }
    0
}

/// The most tax a Canadian grocery receipt can plausibly carry, as a fraction of
/// its subtotal. The real ceiling is 15% (HST in the Atlantic provinces); the
/// headroom absorbs receipts where part of the basket is zero-rated food and the
/// printed subtotal excludes a deposit or fee.
const TAX_PLAUSIBLE_MAX_FRAC: f64 = 0.25;

/// Repair a tax amount that the summary block's label/amount pairing got wrong.
///
/// `SUBTOTAL + TAX = TOTAL` is an identity, so any two of the three determine the
/// third. Both corpus cases are the same underlying defect — the summary block's
/// right column drifting against its labels — surfacing as a tax that is
/// *impossible* rather than merely off:
///
/// - Pharmasave prints `SUBTOTAL 10.79 / HST 1.40 / TOTAL 12.19`; the column
///   drifted up a row, so `HST` claimed 10.79 and the tax equalled the subtotal.
/// - Walmart's `HST` merged onto the TOTAL row as `HST TOTAL $58.94`, so the tax
///   equalled the total.
/// - Foody Mart prints `Sub Total 117.56 / HST 1.82 / hst5% 0.00 / Total after
///   Tax 119.38`, and OCR read the HST amount as `11:82` — no decimal point, so
///   no amount at all. [`sum_summary_block_tax_rows`] then summed the one row it
///   could read and returned the `hst5%` bucket's **0.00**, which is not a
///   near-miss: the receipt charged 1.82 and the entry carried none of it.
///
/// A zero tax is only impossible *because* `total > subtotal` — the guard above
/// — and that is what separates it from the ordinary untaxed receipt, which
/// prints 0.00 and a total equal to its subtotal. It is the one repair here that
/// fires on a well-formed amount rather than a mangled one, which is why the
/// parser reports it (`ReceiptWarningKind::PriceAutoCorrected`) instead of
/// swallowing it.
///
/// Only those impossibilities (and a tax exceeding [`TAX_PLAUSIBLE_MAX_FRAC`] of
/// the subtotal) trigger a repair, and only when `total - subtotal` is itself a
/// plausible tax. A tax that is merely *inconsistent* by a few cents is left
/// alone: deposits, bottle fees and post-subtotal discounts all break the
/// identity legitimately, and this must not start rewriting those.
fn reconcile_tax(tax: Option<i64>, subtotal: Option<i64>, total: i64) -> Option<i64> {
    // Both `None` arms return `tax` untouched rather than propagating: no
    // subtotal means nothing to check against, which is a reason to leave the
    // tax alone, never to discard it.
    let (Some(tax), Some(subtotal)) = (tax, subtotal) else {
        return tax;
    };
    if subtotal <= 0 || total <= subtotal {
        return Some(tax);
    }
    let ceiling = (subtotal as f64 * TAX_PLAUSIBLE_MAX_FRAC) as i64;
    if !tax_is_impossible(tax, subtotal, total) {
        return Some(tax);
    }
    let derived = total - subtotal;
    if derived > 0 && derived <= ceiling {
        Some(derived)
    } else {
        Some(tax)
    }
}

/// Repair a summary block whose labels and amounts are off by a whole row.
///
/// [`reconcile_tax`] fixes **one** wrong summary field by deriving it from the
/// other two. This handles the case where that is not enough because *two* of
/// them are wrong at once: the label column and the amount column have slipped
/// by a row, so every label holds its neighbour's amount and the identity has no
/// clean anchor left to derive from.
///
/// It is a whole family, not one receipt. `pair_columns` walks the label rows in
/// reading order and takes the **first** amount clearing its overlap gate, and
/// detection boxes are taller than the row pitch — 44-51px against 38-42px on the
/// Costco corpus — so a label already overlaps its neighbour's amount by ~0.2
/// before the receipt has leaned at all. Perhaps 3px of column lean is enough to
/// push that past the gate, at which point the label claims the row above's
/// amount and the whole block slides down one. Five corpus receipts do this
/// (`2026-04-26_costco_173_15`, `2026-05-20_costco_74_22`,
/// `2026-06-20_costco_122_04`, `2026-07-08_costco_112_95`,
/// `2026-08-26_costco_737_56`), and on the last of those a 96%-of-total tax
/// reached the ledger.
///
/// **Fixing this in the geometry was measured and rejected.** The lean exceeds
/// that budget on 63 of 125 corpus receipts while only 7 actually misparse, so
/// the geometric signal is necessary but nowhere near sufficient — any change to
/// the gate perturbs the 63 that currently survive. The deskew declines on all
/// five, each at a different estimator gate, because a Costco summary block
/// offers it 10-29 candidate pairs and 1-3 inliers to fit from.
///
/// So this repairs arithmetically instead, from two independent readings the
/// receipt already carries, and only where the printed reading is impossible:
///
/// 1. **The trailer tax echo.** Costco restates the tax below the total, broken
///    down by code (`P (H)HST 13% 30.02`, `TOTAL TAX 30.02`).
///    [`sum_summary_block_tax_rows`] deliberately stops at the total line so it
///    never *sums* that restatement — but as a second reading of the same figure
///    it is exactly what the shifted block lost. Across the corpus it is present
///    on 15 receipts: it agrees with the parsed tax on all 11 that parse right,
///    and contradicts all 4 that do not, correctly every time.
/// 2. **An identity search**, when no echo is readable (OCR mangles it on
///    `173_15`). `SUBTOTAL + TAX = TOTAL`, so look for the one pair of amounts
///    printed in the summary block that satisfies it.
///
/// Both require the derived subtotal to be an amount **printed on the receipt**,
/// so the repair only ever re-assigns figures the receipt actually carries — it
/// never invents one. The identity search additionally refuses when more than one
/// pair fits: on `112_95` a $100.00 gift card plus a $12.95 card payment sum to
/// the total exactly as subtotal-plus-tax does, and a split tender is not
/// something arithmetic alone can tell apart from a summary block. The echo
/// resolves that receipt; ambiguity alone never does.
fn reconcile_summary_shift(
    lines: &[String],
    subtotal: Option<i64>,
    tax: Option<i64>,
    total: i64,
) -> Option<(i64, i64)> {
    let (subtotal, tax) = (subtotal?, tax?);
    if total <= 0 || subtotal <= 0 || total <= subtotal {
        return None;
    }
    // Already consistent, or off by an amount a deposit or fee can explain —
    // both are reasons to leave a money field alone.
    if subtotal + tax == total || !tax_is_impossible(tax, subtotal, total) {
        return None;
    }

    let printed = summary_block_amounts(lines, total);
    let plausible = |derived_subtotal: i64, derived_tax: i64| {
        derived_tax > 0
            && derived_subtotal > 0
            && derived_tax <= (derived_subtotal as f64 * TAX_PLAUSIBLE_MAX_FRAC) as i64
            && printed.contains(&derived_subtotal)
    };

    if let Some(echo) = trailer_tax_echo(lines, total) {
        let derived_subtotal = total - echo;
        if plausible(derived_subtotal, echo) {
            return Some((derived_subtotal, echo));
        }
    }

    let mut fits = printed
        .iter()
        .filter(|&&candidate_subtotal| plausible(candidate_subtotal, total - candidate_subtotal))
        .copied();
    let only = fits.next()?;
    fits.next().is_none().then_some((only, total - only))
}

/// The impossibility test [`reconcile_tax`] repairs on, named so
/// [`reconcile_summary_shift`] can ask the same question rather than restate it.
fn tax_is_impossible(tax: i64, subtotal: i64, total: i64) -> bool {
    tax == total
        || tax == subtotal
        || tax > (subtotal as f64 * TAX_PLAUSIBLE_MAX_FRAC) as i64
        || tax == 0
}

/// Every amount printed in the summary block — the rows from the subtotal label
/// through the total, plus a little slack either side, because the shift this
/// serves is precisely a block whose amounts have slid out of their own rows.
fn summary_block_amounts(lines: &[String], total: i64) -> Vec<i64> {
    let labelled: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            let upper = line.to_ascii_uppercase();
            re_subtotal_label().is_match(&upper) || upper.contains("TOTAL")
        })
        .map(|(idx, _)| idx)
        .collect();
    let (Some(&first), Some(&last)) = (labelled.first(), labelled.last()) else {
        return Vec::new();
    };
    let mut amounts: Vec<i64> = lines[first.saturating_sub(2)..(last + 3).min(lines.len())]
        .iter()
        .flat_map(|line| prices_in_line(line))
        .filter(|&amount| amount > 0 && amount < total)
        .collect();
    amounts.sort_unstable();
    amounts.dedup();
    amounts
}

/// The tax figure a receipt restates *below* its total line, as a rate breakdown
/// (`P (H)HST 13% 30.02`) or a roll-up (`TOTAL TAX 30.02`).
///
/// Below the total is what makes it a restatement rather than a component — the
/// same boundary [`sum_summary_block_tax_rows`] uses from the other side, for the
/// same reason. Amounts at or above the total are rejected: the trailer also
/// carries the charged amount, and a tax is by definition a part of the whole.
fn trailer_tax_echo(lines: &[String], total: i64) -> Option<i64> {
    let total_idx = lines
        .iter()
        .position(|line| line.to_ascii_uppercase().contains("TOTAL"))?;
    lines[total_idx + 1..].iter().find_map(|line| {
        let upper = line.to_ascii_uppercase();
        let is_echo = re_tax_rate_breakdown().is_match(&upper) || upper.contains("TOTAL TAX");
        (is_echo)
            .then(|| prices_in_line(line))
            .and_then(|amounts| amounts.last().copied())
            .filter(|&amount| amount > 0 && amount < total)
    })
}

/// A tax row that names its own rate — `P (H)HST 13%`, `H=HST 13%`, `hst5%`.
/// The rate is required: it is what separates a restatement of the tax charged
/// from the registration number in the header (`HST#…RT0001`).
fn re_tax_rate_breakdown() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(HST|GST|PST|QST)\s*\d{1,2}(\.\d+)?\s*%").unwrap())
}

/// The tax as the receipt printed it, and the tax after [`reconcile_tax`].
///
/// Both readings, because a repair the caller cannot see is a repair nobody can
/// audit: the parser compares them and reports the difference. `printed` is what
/// the summary block yielded — `None` when no tax row was readable at all.
pub struct TaxReading {
    pub cents: Option<i64>,
    pub printed_cents: Option<i64>,
}

impl TaxReading {
    /// True when [`reconcile_tax`] replaced the printed reading with a derived
    /// one. A `None` printed reading is not a repair — there was nothing to
    /// contradict.
    pub fn was_repaired(&self) -> bool {
        self.printed_cents.is_some() && self.cents != self.printed_cents
    }
}

/// The subtotal and tax the receipt printed, and what they are after both
/// summary repairs: [`reconcile_tax`] for a single wrong field, then
/// [`reconcile_summary_shift`] for a whole block that slipped a row.
///
/// One entry point because the two repairs share a precondition — an
/// impossible printed reading — and reading them apart is how a caller ends up
/// applying the second to a receipt the first already fixed.
pub struct SummaryReading {
    pub subtotal_cents: Option<i64>,
    pub tax: TaxReading,
    pub printed_subtotal_cents: Option<i64>,
}

impl SummaryReading {
    /// True when the whole block was re-assigned, as opposed to the tax alone
    /// being derived. Reported separately because it rewrites *two* money
    /// fields, so it deserves its own line in the warnings.
    pub fn shift_repaired(&self) -> bool {
        self.printed_subtotal_cents.is_some() && self.subtotal_cents != self.printed_subtotal_cents
    }
}

/// [`extract_subtotal`] and [`extract_tax`], with both summary repairs applied.
pub fn extract_summary_reconciled(lines: &[String], total: i64) -> SummaryReading {
    let printed_subtotal_cents = extract_subtotal(lines);
    let printed_tax_cents = extract_tax(lines);
    let tax_cents = reconcile_tax(printed_tax_cents, printed_subtotal_cents, total);

    match reconcile_summary_shift(lines, printed_subtotal_cents, tax_cents, total) {
        Some((subtotal, tax)) => SummaryReading {
            subtotal_cents: Some(subtotal),
            tax: TaxReading {
                cents: Some(tax),
                printed_cents: printed_tax_cents,
            },
            printed_subtotal_cents,
        },
        None => SummaryReading {
            subtotal_cents: printed_subtotal_cents,
            tax: TaxReading {
                cents: tax_cents,
                printed_cents: printed_tax_cents,
            },
            printed_subtotal_cents,
        },
    }
}

/// Sum of the tax rows printed between the subtotal and the total.
///
/// A receipt may split its tax across buckets rather than printing one figure.
/// The Bestco/Foody/C&C POS family does exactly that — an `HST` row for the 13%
/// basket and an `hst5%` row for the 5%-only basket — and the two add up to what
/// the customer paid:
///
/// ```text
/// Sub Total       81.76
/// HST              0.00
/// hst5%            0.20
/// Total after Tax 81.96
/// ```
///
/// Reading one row and stopping loses the other, which reaches the ledger as an
/// unaccounted FIXME.
///
/// **The window is what makes summing safe.** Costco prints its tax twice — once
/// as `TAX` in the summary block, then again as `P(H)HST 13%` in the trailer
/// that breaks the tax down by code — and those are one bucket restated, not two
/// buckets. Every genuine component sits *above* the total line, because that is
/// what the total is the sum of; a restatement sits below it. So the scan starts
/// after the subtotal label and stops at the first line bearing `TOTAL`, which is
/// the grand total in both layouts (`Total after Tax`, `**** TOTAL`). That also
/// keeps the header's `HST#…RT0001` registration number out of the sum.
fn sum_summary_block_tax_rows(lines: &[String]) -> Option<i64> {
    let subtotal_idx = (0..lines.len()).rev().find(|&idx| {
        let line_upper = lines[idx].to_ascii_uppercase();
        re_subtotal_label().is_match(&line_upper) || line_upper.contains("SUB TOTAL")
    })?;

    let mut amounts = Vec::new();
    for line in &lines[subtotal_idx + 1..] {
        let line_upper = line.to_ascii_uppercase();
        // "AFTER TAX" ends the window on its own: OCR mangles the word "Total"
        // often enough ("lotal after Tax 163.95") that the label alone cannot be
        // relied on to mark the grand total.
        if line_upper.contains("TOTAL") || line_upper.contains("AFTER TAX") {
            break;
        }
        if line_upper.contains("TAXED") || line_upper.contains("TAXABLE") {
            continue;
        }
        if !re_tax_tokens().is_match(&line_upper) {
            continue;
        }
        if let Some(amount) = extract_price_from_line(line) {
            amounts.push(amount);
        }
    }
    (!amounts.is_empty()).then(|| amounts.iter().sum())
}

pub fn extract_tax(lines: &[String]) -> Option<i64> {
    // Falls through to the row scan below when the receipt has no subtotal label
    // or prints no tax between it and the total — that scan also reaches amounts
    // sitting on a neighbouring line, which a column split can produce.
    if let Some(summed) = sum_summary_block_tax_rows(lines) {
        return Some(summed);
    }
    for idx in (0..lines.len()).rev() {
        let line_upper = lines[idx].to_ascii_uppercase();
        if line_upper.contains("SUBTOTAL") || line_upper.contains("SUB TOTAL") {
            continue;
        }
        if line_upper.contains("TAXED") || line_upper.contains("TAXABLE") {
            continue;
        }
        // An "after tax" line is a grand total, never the tax amount — skip it
        // regardless of whether OCR preserved the literal word "Total" (Foody
        // Mart's "lotal after Tax 163.95" would otherwise be read as the tax).
        if line_upper.contains("AFTER TAX") {
            continue;
        }

        let has_total = line_upper.contains("TOTAL");
        let has_tax_keyword = re_tax_tokens().is_match(&line_upper);
        if has_total && !has_tax_keyword {
            continue;
        }

        if has_tax_keyword {
            if let Some(amount) = extract_price_from_line(&lines[idx]) {
                return Some(amount);
            }

            if idx + 1 < lines.len() {
                let next_line = &lines[idx + 1];
                let next_line_upper = next_line.to_ascii_uppercase();
                let mut is_total_value = next_line_upper.contains("TOTAL");
                if !is_total_value && idx + 2 < lines.len() {
                    let line_i2_upper = lines[idx + 2].to_ascii_uppercase();
                    if line_i2_upper.contains("TOTAL") && !line_i2_upper.contains("SUBTOTAL") {
                        if idx + 3 < lines.len()
                            && extract_price_from_line(&lines[idx + 3]).is_some()
                        {
                            is_total_value = false;
                        } else {
                            is_total_value = true;
                        }
                    }
                }

                if !is_total_value && re_standalone_amount().is_match(next_line) {
                    if let Some(amount) = extract_price_from_line(next_line) {
                        return Some(amount);
                    }
                }
            }

            if idx > 0 && re_standalone_amount().is_match(&lines[idx - 1]) {
                let prev_upper = lines[idx - 1].to_ascii_uppercase();
                if !prev_upper.contains("SUBTOTAL") && !prev_upper.contains("TOTAL") {
                    if let Some(amount) = extract_price_from_line(&lines[idx - 1]) {
                        return Some(amount);
                    }
                }
            }
        }
    }
    None
}

fn re_subtotal_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // The inner 'O' in SUBTOTAL is the most common OCR victim — accept the
    // usual O-confusables (0/C/Q/D/G). Costco receipts have been observed as
    // SUBTCTAL.
    RE.get_or_init(|| Regex::new(r"SUB\s*T[OCQDG0]TAL").unwrap())
}

#[derive(Clone, Debug)]
pub struct TenderLine {
    pub raw_label: String,
    pub amount_cents: i64,
    pub kind: &'static str,
}

fn classify_tender_line(line_upper: &str) -> Option<&'static str> {
    // Reject noise lines that contain price-paired keywords but aren't tenders.
    if line_upper.contains("BALANCE") {
        return None;
    }
    if line_upper.contains("CASH BACK") {
        return None;
    }
    if line_upper.contains("CHANGE") {
        return None;
    }
    if line_upper.contains("AMOUNT:") {
        // Costco prints "AMOUNT: $25.00" as the *card-charge* echo of the next
        // Shop Card line; ignore the echo and let the labelled line classify.
        return None;
    }
    if line_upper.contains("GIFT CARD")
        || line_upper.contains("GIFTCARD")
        || line_upper.contains("GIFT CRD")
        || line_upper.contains("SHOP CARD")
    {
        return Some("gift_card");
    }
    if line_upper.contains("MERCH CRED")
        || line_upper.contains("MERCH CREDIT")
        || line_upper.contains("STORE CREDIT")
    {
        return Some("store_credit");
    }
    if line_upper.contains("MASTERCARD")
        || line_upper.contains("VISA")
        || line_upper.contains("AMEX")
        || line_upper.contains("AMERICAN EXPRESS")
        || line_upper.contains("DEBIT")
    {
        return Some("card");
    }
    if re_cash_label().is_match(line_upper) {
        return Some("cash");
    }
    None
}

fn re_cash_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bCASH\b").unwrap())
}

fn tender_amount_for_line(lines: &[String], idx: usize) -> Option<i64> {
    if let Some(amount) = extract_price_from_line(&lines[idx]) {
        return Some(amount);
    }
    if idx + 1 < lines.len() && re_standalone_amount().is_match(&lines[idx + 1]) {
        return extract_price_from_line(&lines[idx + 1]);
    }
    None
}

fn trim_tender_label(line: &str) -> String {
    let mut text = line.trim().to_string();
    // Strip trailing currency token like "$25.00" so the label reads cleanly.
    if let Some(captures) = re_price_anywhere().captures(&text) {
        if let Some(matched) = captures.get(0) {
            let start = matched.start();
            text = text[..start]
                .trim_end_matches(['$', ' ', ':', '-', '\t'])
                .to_string();
        }
    }
    text
}

/// Scan OCR lines for explicit tender lines (gift card / store credit / cash / card).
///
/// Each candidate line picks an amount from the same line or the next
/// standalone-amount line. **Whether those amounts add up to the total is not
/// this function's business** — it reports what the payment block says, and the
/// caller reconciles.
///
/// It used to end by summing the tenders and comparing them against the total,
/// returning an empty vec when they disagreed by more than $0.05. That was
/// wrong in two directions at once:
///
/// - **It discarded the evidence.** The tender block is the only independent
///   witness to the total the receipt prints, so throwing it away on
///   disagreement destroyed the one signal that says a number is misread —
///   and it did so precisely when the receipt was trying to say so. Worse, it
///   was silent: no tenders and no warning, indistinguishable from a receipt
///   that prints no payment block at all.
/// - **The tolerance emitted entries that could not balance.** A gap of 1–5c
///   passed as "reconciled" and the tenders became postings, so the payment
///   side summed to `-sum` while the item side summed to `total` and beancount
///   rejected the transaction. A gap of a cent is not a rounding artifact on a
///   receipt — every amount here is printed to the cent — it is a misread
///   digit, which is exactly the LCBO case where a $66.60 gift card read as
///   $65.60.
///
/// Callers now get the lines plus the arithmetic and decide: `parser`
/// raises [`ReceiptWarningKind::TenderMismatch`], and `formatter` falls
/// back to the single-payment posting so the ledger still balances.
///
/// [`ReceiptWarningKind::TenderMismatch`]: crate::common::ReceiptWarningKind::TenderMismatch
pub fn extract_tenders(lines: &[String]) -> Vec<TenderLine> {
    let mut tenders: Vec<TenderLine> = Vec::new();
    let mut consumed_next = false;
    for (idx, line) in lines.iter().enumerate() {
        if consumed_next {
            consumed_next = false;
            continue;
        }
        let line_upper = line.to_ascii_uppercase();
        let Some(kind) = classify_tender_line(&line_upper) else {
            continue;
        };
        let Some(amount_cents) = tender_amount_for_line(lines, idx) else {
            continue;
        };
        if amount_cents <= 0 {
            continue;
        }
        // If the amount came from the next standalone-amount line, skip it next iter.
        if extract_price_from_line(line).is_none()
            && idx + 1 < lines.len()
            && re_standalone_amount().is_match(&lines[idx + 1])
        {
            consumed_next = true;
        }
        tenders.push(TenderLine {
            raw_label: trim_tender_label(line),
            amount_cents,
            kind,
        });
    }

    tenders
}

fn re_change_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Word-boundary matched on purpose: plain `contains("CHANGE")` also fires
    // on every `EXCHANGE` in a return policy, and the corpus prints several
    // ("No Refund, Exchange Only Within 7 Days"). Same substring trap as
    // `CASH` inside `CASHIER`.
    RE.get_or_init(|| Regex::new(r"\bCHANGE\b").unwrap())
}

/// Cash handed back, summed over every change line the receipt prints.
///
/// Without this the tender identity is wrong for the commonest cash receipt
/// there is: `CASH 25.00 / CHANGE 5.00` against a `TOTAL 20.00` is not a
/// contradiction, it is arithmetic — the amount *tendered* is not the amount
/// *applied*, and change is the missing term.
///
/// The change is the **last** amount on its row, never the first. Two shapes
/// force this and one of them is not rare: Pharmasave prints
/// `CHANGE DUE $12.19 $0.00`, tendered before change; and line grouping merges
/// Costco's customer-copy rows into `MasterCard 441.68 CHANGE 0.00`, where
/// taking the first amount reads the entire card charge as change handed back
/// and drives the net tendered negative.
pub fn extract_change(lines: &[String]) -> i64 {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| re_change_label().is_match(&line.to_ascii_uppercase()))
        .filter_map(|(idx, line)| {
            prices_in_line(line)
                .last()
                .copied()
                .or_else(|| tender_amount_for_line(lines, idx))
        })
        .sum()
}

/// Do the printed tenders account for exactly the printed total, once cash
/// handed back is netted off?
///
/// Exact, deliberately: see [`extract_tenders`] on why there is no tolerance.
/// An empty tender block is not a disagreement — most receipts print none — and
/// neither is a receipt with no usable total to check against.
pub fn tenders_reconcile(lines: &[String], tenders: &[TenderLine], total_cents: i64) -> bool {
    tenders.is_empty() || total_cents <= 0 || tendered_net_cents(lines, tenders) == total_cents
}

/// What the payment block says was actually applied to this receipt: everything
/// tendered, less any change handed back.
///
/// **Change is dropped when it exceeds the tenders**, because no receipt hands
/// back more than it took in. That is not a repair — the sum still fails to
/// match the total and [`tenders_reconcile`] still reports it — it only keeps a
/// misread change line from turning the report into an impossible number.
/// Costco's redacted `costco_46668` is the case: the amount column shifts a row
/// into `MasterCard` / `CHANGE 441.68` / `0.00`, so the card tender loses its
/// amount *and* the whole card charge reads as change. Netting it gives -416.68
/// and a warning claiming 883.36 is unaccounted for, when the truth is that one
/// 441.68 tender went missing — which is what the guarded number says.
pub fn tendered_net_cents(lines: &[String], tenders: &[TenderLine]) -> i64 {
    let tendered: i64 = tenders.iter().map(|t| t.amount_cents).sum();
    let change = extract_change(lines);
    if change > tendered {
        return tendered;
    }
    tendered - change
}

pub fn extract_subtotal(lines: &[String]) -> Option<i64> {
    for (idx, line) in lines.iter().enumerate() {
        let line_upper = line.to_ascii_uppercase();
        if re_subtotal_label().is_match(&line_upper) {
            if let Some(amount) = extract_price_from_line(line) {
                return Some(amount);
            }
            if idx + 1 < lines.len() {
                if let Some(amount) = extract_price_from_line(&lines[idx + 1]) {
                    return Some(amount);
                }
            }
        }
    }
    None
}

fn to_four_digit_year(year: i32) -> i32 {
    if year < 100 {
        if year <= 69 {
            2000 + year
        } else {
            1900 + year
        }
    } else {
        year
    }
}

fn numeric_date_candidates(part1: &str, part2: &str, part3: &str) -> Vec<(Date, &'static str)> {
    let a = match part1.parse::<i32>() {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let b = match part2.parse::<i32>() {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let c = match part3.parse::<i32>() {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    let mut candidates = Vec::new();
    let mut add = |year: i32, month: i32, day: i32, kind: &'static str| {
        let (Ok(month), Ok(day)) = (u32::try_from(month), u32::try_from(day)) else {
            return;
        };
        if let Some(parsed) = Date::new(year, month, day) {
            candidates.push((parsed, kind));
        }
    };

    if part1.len() == 4 {
        add(a, b, c, "ymd4");
        return candidates;
    }

    if part3.len() == 4 {
        if a > 12 && b <= 12 {
            add(c, b, a, "dmy4");
        } else if b > 12 && a <= 12 {
            add(c, a, b, "mdy4");
        } else {
            add(c, a, b, "mdy4");
            add(c, b, a, "dmy4");
        }
        return candidates;
    }

    let year_a = to_four_digit_year(a);
    let year_c = to_four_digit_year(c);

    if b <= 12 && c <= 31 {
        add(year_a, b, c, "ymd2");
    }
    if a <= 12 && b <= 31 {
        add(year_c, a, b, "mdy2");
    }
    if b <= 12 && a <= 31 {
        add(year_c, b, a, "dmy2");
    }

    candidates
}

fn year_score(candidate_year: i32, current_year: i32) -> i32 {
    10 - (candidate_year - current_year).abs().min(10)
}

fn kind_base_score(kind: &str) -> i32 {
    match kind {
        "ymd4" => 35,
        "ymd2" => 28,
        "mdy4" => 25,
        "dmy4" => 24,
        "mdy2" => 22,
        "dmy2" => 20,
        _ => 0,
    }
}

fn compare_ranked_candidates(left: &RankedDateCandidate, right: &RankedDateCandidate) -> Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.line_index.cmp(&right.line_index))
        .then_with(|| left.start.cmp(&right.start))
}

pub fn extract_date(lines: &[String], full_text: &str, current_year: i32) -> Option<Date> {
    if lines.is_empty() && full_text.is_empty() {
        return None;
    }

    let source_lines: Vec<String> = if lines.is_empty() {
        full_text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        lines.to_vec()
    };
    let current_yy = current_year.rem_euclid(100);
    let mut ranked_candidates = Vec::new();

    for (line_index, line) in source_lines.iter().enumerate() {
        let normalized_line = normalize_decimal_spacing(line);
        if is_return_deadline_context(&source_lines, line_index) {
            continue;
        }
        let hint_bonus = if re_date_context_hint().is_match(&normalized_line) {
            40
        } else {
            0
        };
        let prefer_year_first = hint_bonus > 0;

        for captures in re_separated_date().captures_iter(&normalized_line) {
            let part1 = captures.get(2).map(|m| m.as_str()).unwrap_or("");
            let part2 = captures.get(3).map(|m| m.as_str()).unwrap_or("");
            let part3 = captures.get(4).map(|m| m.as_str()).unwrap_or("");
            let start = captures.get(2).map(|m| m.start()).unwrap_or(0);
            for (candidate_date, kind) in numeric_date_candidates(part1, part2, part3) {
                if kind == "ymd2" {
                    let year_token = match part1.parse::<i32>() {
                        Ok(value) => value,
                        Err(_) => continue,
                    };
                    if !(prefer_year_first && (20..=current_yy + 1).contains(&year_token)) {
                        continue;
                    }
                }
                let mut base = kind_base_score(kind);
                if kind == "mdy2" {
                    base += 2;
                }
                if kind == "ymd2" && prefer_year_first {
                    base += 3;
                }
                ranked_candidates.push(RankedDateCandidate {
                    score: base + hint_bonus + year_score(candidate_date.year(), current_year),
                    line_index,
                    start,
                    date: candidate_date,
                });
            }
        }

        for captures in re_compact_date().captures_iter(&normalized_line) {
            let year = captures.get(2).and_then(|m| m.as_str().parse::<i32>().ok());
            let month = captures.get(3).and_then(|m| m.as_str().parse::<i32>().ok());
            let day = captures.get(4).and_then(|m| m.as_str().parse::<i32>().ok());
            let start = captures.get(2).map(|m| m.start()).unwrap_or(0);
            if let (Some(year), Some(month), Some(day)) = (year, month, day) {
                if let Some(compact_date) =
                    Date::new(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)
                {
                    ranked_candidates.push(RankedDateCandidate {
                        score: 30 + hint_bonus + year_score(compact_date.year(), current_year),
                        line_index,
                        start,
                        date: compact_date,
                    });
                }
            }
        }

        for captures in re_month_name_date().captures_iter(&normalized_line) {
            let month = captures
                .get(1)
                .and_then(|m| month_number_from_name(m.as_str()));
            let day = captures.get(2).and_then(|m| m.as_str().parse::<i32>().ok());
            let year = captures.get(3).and_then(|m| m.as_str().parse::<i32>().ok());
            let start = captures.get(1).map(|m| m.start()).unwrap_or(0);
            if let (Some(month), Some(day), Some(year)) = (month, day, year) {
                if let Some(parsed) =
                    Date::new(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)
                {
                    ranked_candidates.push(RankedDateCandidate {
                        score: 26 + hint_bonus + year_score(parsed.year(), current_year),
                        line_index,
                        start,
                        date: parsed,
                    });
                }
            }
        }

        for captures in re_dmy_month_name_date().captures_iter(&normalized_line) {
            let day = captures.get(1).and_then(|m| m.as_str().parse::<i32>().ok());
            let month = captures
                .get(2)
                .and_then(|m| month_number_from_name(m.as_str()));
            let year = captures.get(3).and_then(|m| m.as_str().parse::<i32>().ok());
            let start = captures.get(1).map(|m| m.start()).unwrap_or(0);
            if let (Some(month), Some(day), Some(year)) = (month, day, year) {
                if let Some(parsed) =
                    Date::new(year, u32::try_from(month).ok()?, u32::try_from(day).ok()?)
                {
                    ranked_candidates.push(RankedDateCandidate {
                        score: 26 + hint_bonus + year_score(parsed.year(), current_year),
                        line_index,
                        start,
                        date: parsed,
                    });
                }
            }
        }
    }

    if ranked_candidates.is_empty() {
        return None;
    }

    ranked_candidates.sort_by(compare_ranked_candidates);
    ranked_candidates.first().map(|candidate| candidate.date)
}

#[cfg(test)]
mod tests {
    use super::{
        extract_change, extract_date, extract_subtotal, extract_summary_reconciled, extract_tax,
        extract_tenders, extract_total, normalize_decimal_spacing, reconcile_tax,
        tendered_net_cents, tenders_reconcile,
    };

    #[test]
    fn tax_equal_to_the_subtotal_is_rederived() {
        // Pharmasave: the summary column drifted up a row, so HST claimed the
        // subtotal's 10.79 and TOTAL claimed the tax's 1.40.
        assert_eq!(reconcile_tax(Some(1079), Some(1079), 1219), Some(140));
    }

    #[test]
    fn tax_equal_to_the_total_is_rederived() {
        // Walmart: "HST" merged onto the TOTAL row as "HST TOTAL $58.94".
        assert_eq!(reconcile_tax(Some(5894), Some(5380), 5894), Some(514));
    }

    #[test]
    fn implausibly_large_tax_is_rederived() {
        // Half the subtotal is not a Canadian tax rate.
        assert_eq!(reconcile_tax(Some(5000), Some(10000), 11300), Some(1300));
    }

    #[test]
    fn zero_tax_under_a_larger_total_is_rederived() {
        // Foody Mart 2026-08-22: OCR read "HST 1.82" as "11:82" — no parseable
        // amount — so the summed buckets returned the hst5% row's 0.00 while the
        // receipt charged 1.82 on its hot-food line.
        assert_eq!(reconcile_tax(Some(0), Some(11756), 11938), Some(182));
    }

    #[test]
    fn zero_tax_on_an_untaxed_receipt_is_left_alone() {
        // The ordinary zero-rated grocery basket: 0.00 tax printed, and the total
        // agrees with the subtotal. There is nothing to derive and nothing wrong.
        assert_eq!(reconcile_tax(Some(0), Some(4210), 4210), Some(0));
    }

    #[test]
    fn zero_tax_is_left_alone_when_the_gap_is_too_large_to_be_tax() {
        // A gap of 40% of the subtotal is a mis-read total or a missing summary
        // line, not a tax rate — deriving from it would invent an amount.
        assert_eq!(reconcile_tax(Some(0), Some(1000), 1400), Some(0));
    }

    #[test]
    fn a_repaired_tax_is_reported_as_repaired() {
        let lines = vec![
            "Sub Total 117.56".to_string(),
            "HST 11:82".to_string(),
            "hst5% 0.00".to_string(),
            "Total after Tax 119.38".to_string(),
        ];
        let reading = extract_summary_reconciled(&lines, 11938).tax;
        assert_eq!(reading.printed_cents, Some(0));
        assert_eq!(reading.cents, Some(182));
        assert!(reading.was_repaired());
    }

    #[test]
    fn an_untouched_tax_is_not_reported_as_repaired() {
        let lines = vec![
            "Sub Total 181.32".to_string(),
            "HST 2.36".to_string(),
            "hst5% 0.09".to_string(),
            "Total after Tax 183.77".to_string(),
        ];
        let reading = extract_summary_reconciled(&lines, 18377).tax;
        assert_eq!(reading.cents, Some(245));
        assert!(!reading.was_repaired());
    }

    #[test]
    fn summary_block_off_by_a_row_is_re_read_from_the_trailer_echo() {
        // costco/2026-08-26_costco_737_56. SUBTOTAL merged onto the DEPOSIT VL
        // row and took its 4.00, so TAX took the subtotal's 707.54 and TOTAL the
        // tax's 30.02. `reconcile_tax` alone cannot save this: it derives
        // 737.56 - 4.00 = 733.56, which is not a plausible tax either, because
        // the subtotal it derives from is wrong too.
        let lines = vec![
            "SUBTOTAL DEPOSIT VL 4.00".to_string(),
            "TAX 707.54".to_string(),
            "***TOTAL 30.02".to_string(),
            "737.56".to_string(),
            "MasterCard 737.56".to_string(),
            "P (H)HST 13% 30.02".to_string(),
            "TOTAL TAX 30.02".to_string(),
        ];
        let reading = extract_summary_reconciled(&lines, 73756);
        assert_eq!(reading.subtotal_cents, Some(70754));
        assert_eq!(reading.tax.cents, Some(3002));
        assert!(reading.shift_repaired());
    }

    #[test]
    fn summary_block_off_by_a_row_is_re_read_from_the_identity() {
        // costco/2026-04-26_costco_173_15: the SUBTCTAL row absorbed two
        // amounts and the parser took the second. Its trailer echo is mangled
        // ("(HOHST 13% 12" carries no readable amount), so only the identity
        // search can place these — and exactly one pair of the printed amounts
        // satisfies it.
        let lines = vec![
            "SUBTCTAL 159.08 14.07".to_string(),
            "TAX 173.15".to_string(),
            "**** TOTAL".to_string(),
            "AMOUNT: 173.15".to_string(),
            "(HOHST 13% 12".to_string(),
        ];
        let reading = extract_summary_reconciled(&lines, 17315);
        assert_eq!(reading.subtotal_cents, Some(15908));
        assert_eq!(reading.tax.cents, Some(1407));
    }

    #[test]
    fn a_split_tender_summing_to_the_total_does_not_pass_for_a_summary_block() {
        // costco/2026-07-08_costco_112_95 pays with a $100.00 gift card and
        // $12.95 on a card, and those sum to the total exactly as subtotal plus
        // tax does. Arithmetic alone cannot separate the two readings, so the
        // identity search must decline — the echo is what settles this receipt.
        let lines = vec![
            "TOTAL NUMBER OF ITEMS SOLD = 9 104.77".to_string(),
            "SUBTOTAL 8.18".to_string(),
            "TAX 112.95".to_string(),
            "**** TOTAL".to_string(),
            "Shop Card AMOUNT: $100.00 Resp: Approved".to_string(),
            "Shop Card 100.00".to_string(),
            "AMOUNT: 12.95".to_string(),
        ];
        let ambiguous = extract_summary_reconciled(&lines, 11295);
        assert_eq!(ambiguous.subtotal_cents, Some(818));
        assert!(!ambiguous.shift_repaired());

        let mut with_echo = lines.clone();
        with_echo.push("P (H)HST 13% 8.18".to_string());
        let reading = extract_summary_reconciled(&with_echo, 11295);
        assert_eq!(reading.subtotal_cents, Some(10477));
        assert_eq!(reading.tax.cents, Some(818));
    }

    #[test]
    fn a_consistent_summary_block_is_never_reshuffled() {
        // The identity holds, so nothing here is impossible and no repair may
        // fire — even though 70.32 + 3.90 is not the only pair on the receipt.
        let lines = vec![
            "SUBTOTAL 70.32".to_string(),
            "TAX 3.90".to_string(),
            "**** TOTAL 74.22".to_string(),
            "P (H)HST 13% 3.90".to_string(),
        ];
        let reading = extract_summary_reconciled(&lines, 7422);
        assert_eq!(reading.subtotal_cents, Some(7032));
        assert_eq!(reading.tax.cents, Some(390));
        assert!(!reading.shift_repaired());
    }

    #[test]
    fn a_deposit_breaking_the_identity_is_not_a_shift() {
        // subtotal + tax falls short of the total by a bottle deposit charged
        // after the subtotal. The tax is plausible, so this is a receipt doing
        // something legitimate, not a block that slipped.
        let lines = vec![
            "SUBTOTAL 10.00".to_string(),
            "TAX 1.30".to_string(),
            "DEPOSIT 0.20".to_string(),
            "**** TOTAL 11.50".to_string(),
        ];
        let reading = extract_summary_reconciled(&lines, 1150);
        assert_eq!(reading.subtotal_cents, Some(1000));
        assert_eq!(reading.tax.cents, Some(130));
        assert!(!reading.shift_repaired());
    }

    #[test]
    fn a_derived_subtotal_the_receipt_never_printed_is_refused() {
        // The echo says 30.02, which would imply a 707.54 subtotal — but no such
        // amount is printed here. The repair re-assigns figures the receipt
        // carries; it never invents one.
        let lines = vec![
            "SUBTOTAL 4.00".to_string(),
            "TAX 900.00".to_string(),
            "**** TOTAL".to_string(),
            "P (H)HST 13% 30.02".to_string(),
        ];
        let reading = extract_summary_reconciled(&lines, 73756);
        assert_eq!(reading.subtotal_cents, Some(400));
        assert!(!reading.shift_repaired());
    }

    #[test]
    fn merely_inconsistent_tax_is_left_alone() {
        // subtotal + tax != total by 10c — a deposit or bottle fee, not a
        // mis-paired label. Rewriting these is exactly what this must not do.
        assert_eq!(reconcile_tax(Some(130), Some(1000), 1140), Some(130));
    }

    #[test]
    fn tax_is_left_alone_when_the_derived_value_is_implausible() {
        // Contradictory tax, but total - subtotal is 60% of the subtotal, so the
        // arithmetic offers nothing better to swap in.
        assert_eq!(reconcile_tax(Some(1000), Some(1000), 1600), Some(1000));
    }

    #[test]
    fn tax_is_left_alone_without_a_subtotal_to_check_against() {
        assert_eq!(reconcile_tax(Some(5894), None, 5894), Some(5894));
        assert_eq!(reconcile_tax(None, Some(5380), 5894), None);
    }

    #[test]
    fn rate_suffixed_tax_label_is_read_as_a_tax_row() {
        // Foody Mart 2026-08-07 prints its 5% bucket as "hst5%", with no space
        // before the rate, so the label's trailing word boundary never landed and
        // the row read as untaxed text. The 0.20 then reached the ledger as an
        // unaccounted FIXME.
        let lines = vec![
            "Sub Total 81.76".to_string(),
            "HST 0.00".to_string(),
            "hst5% 0.20".to_string(),
            "Total after Tax 81.96".to_string(),
        ];
        assert_eq!(extract_tax(&lines), Some(20));
    }

    #[test]
    fn split_tax_buckets_are_summed() {
        // Bestco 2026-06-25 charges both buckets: 181.32 + 2.36 + 0.09 = 183.77.
        // Reading either row alone loses the other.
        let lines = vec![
            "Sub Total 181.32".to_string(),
            "HST 2.36".to_string(),
            "hst5% 0.09".to_string(),
            "Total after Tax 183.77".to_string(),
        ];
        assert_eq!(extract_tax(&lines), Some(245));
    }

    #[test]
    fn tax_restated_below_the_total_is_not_double_counted() {
        // Costco prints the tax twice — once in the summary block, then again in
        // the trailer that breaks it down by code. Both rows say 1.04, and the
        // receipt's tax is 1.04, not 2.08. Only the rows above the total line are
        // components of it.
        let lines = vec![
            "SUBTOTAL 70.23".to_string(),
            "TAX 1.04".to_string(),
            "**** TOTAL 71.27".to_string(),
            "P (H)HST 13% 1.04".to_string(),
            "TOTAL TAX 1.04".to_string(),
        ];
        assert_eq!(extract_tax(&lines), Some(104));
    }

    #[test]
    fn tax_registration_number_in_the_header_is_not_a_tax_row() {
        // "HST#821366291RT0001" sits above the subtotal, outside the window, and
        // carries no amount besides — neither reason alone should be relied on.
        let lines = vec![
            "(905)305-9866 HST#821366291RT0001".to_string(),
            "Sub Total 81.76".to_string(),
            "hst5% 0.20".to_string(),
            "Total after Tax 81.96".to_string(),
        ];
        assert_eq!(extract_tax(&lines), Some(20));
    }

    #[test]
    fn comma_read_as_decimal_point_still_yields_a_total() {
        // Foody Mart 2026-07-29 printed both amounts identically, but OCR read
        // only the grand-total row's point as a comma: "Sub Total 110.05" /
        // "Total after Tax 110,05". The total row parsed to nothing, so the
        // credit-card posting was written as 0.00.
        let lines = vec![
            "Sub Total 110.05".to_string(),
            "Total after Tax 110,05".to_string(),
        ];
        assert_eq!(extract_total(&lines), 11005);
    }

    #[test]
    fn thousands_separator_is_not_rewritten_as_a_decimal_point() {
        // The guard that makes the comma rule safe, asserted against *this*
        // module's copy of `normalize_decimal_spacing` — it is the copy that
        // drifted, so testing the shared behavior elsewhere would not have
        // caught it. Whether the extractor then reads the leading "1," is a
        // separate, pre-existing limitation: it takes the "299.99" tail.
        assert_eq!(
            normalize_decimal_spacing("TOTAL 1,299.99"),
            "TOTAL 1,299.99"
        );
        assert_eq!(normalize_decimal_spacing("Anytown, ON"), "Anytown, ON");
    }

    #[test]
    fn date_parses_day_first_hyphenated_month_name() {
        // Jin Lian Food / Clover format: "22-May-2026 3:22:42p.m."
        let lines = vec!["22-May-2026 3:22:42p.m.".to_string()];
        let parsed = extract_date(&lines, "", 2026).expect("date should parse");
        assert_eq!(parsed.ymd(), (2026, 5, 22));
    }

    #[test]
    fn return_deadline_does_not_outrank_the_transaction_date() {
        let lines = vec![
            "Last Valid Date for Return of Product Is:".to_string(),
            "Date limite pour retour de produits".to_string(),
            "26 SEP 2026".to_string(),
            "V124.04 27 AUG 2026 04:11PM".to_string(),
        ];
        let parsed = extract_date(&lines, "", 2026).expect("transaction date should parse");
        assert_eq!(parsed.to_string(), "2026-08-27");
    }

    #[test]
    fn a_return_deadline_alone_is_not_a_purchase_date() {
        let lines = vec![
            "Last Valid Date for Return of Product Is:".to_string(),
            "Date limite pour retour de produits".to_string(),
            "26 SEP 2026".to_string(),
        ];
        assert_eq!(extract_date(&lines, "", 2026), None);
    }

    #[test]
    fn date_parses_dotted_month_abbreviation() {
        // Clover also prints an abbreviation period: "02-Apr.-2026 2:27:39p.m."
        let lines = vec!["02-Apr.-2026 2:27:39p.m.".to_string()];
        let parsed = extract_date(&lines, "", 2026).expect("date should parse");
        assert_eq!(parsed.ymd(), (2026, 4, 2));
    }

    #[test]
    fn date_hint_survives_ocr_damage_to_the_datetime_suffix() {
        // No Frills prints "DateTime: 26/08/02"; PP-OCRv5 read it as "Datelime".
        // The hint is what admits the year-first reading, so losing it to a
        // single glyph moved the date 24 years: 2026-08-02 -> 2002-08-26.
        for label in ["DateTime", "Datelime", "DATETIME", "Dateiime", "Date"] {
            let lines = vec![format!("{label}: 26/08/02 15:48:10")];
            let parsed = extract_date(&lines, "", 2026)
                .unwrap_or_else(|| panic!("date should parse for label {label:?}"));
            assert_eq!(
                parsed.ymd(),
                (2026, 8, 2),
                "label {label:?} should read 26/08/02 as year-first"
            );
        }
    }

    #[test]
    fn date_hint_does_not_fire_inside_a_longer_word() {
        // `\bDATE` still needs a boundary before the prefix, so "UPDATE" is not
        // date context and the year-first reading stays gated.
        let lines = vec!["UPDATED: 26/08/02".to_string()];
        let parsed = extract_date(&lines, "", 2026).expect("date should parse");
        assert_ne!(parsed.ymd(), (2026, 8, 2));
    }

    #[test]
    fn subtotal_tolerates_costco_subtctal_ocr_typo() {
        // Costco "SUBTOTAL" OCR'd as "SUBTCTAL" (inner O → C).
        let lines = vec![
            "***END OF PRE-SCANNED ITEMS***".to_string(),
            "SUBTCTAL 159.08".to_string(),
            "TAX 14.07".to_string(),
        ];

        assert_eq!(extract_subtotal(&lines), Some(15_908));
    }

    #[test]
    fn bare_total_takes_tax_row_amount_when_it_exceeds_the_subtotal() {
        // Costco 2026-07-08_costco_112_95: up-leaned line grouping left the
        // TOTAL row bare and put the grand total on the TAX row (and the tax
        // on SUBTOTAL). A real tax can never exceed the subtotal amount.
        let lines = vec![
            "TOTAL NUMBER OF ITEMS SOLD = 9 104.77".to_string(),
            "SUBTOTAL 8.18".to_string(),
            "TAX 112.95".to_string(),
            "**** TOTAL".to_string(),
            "XXXXXXXXXXXX7735".to_string(),
        ];

        assert_eq!(extract_total(&lines), 11_295);
    }

    #[test]
    fn total_row_holding_a_split_off_discount_label_is_not_the_grand_total() {
        // Costco 2026-03-07_costco_466_68: once line grouping shifts, the
        // "TOTAL DISCOUNT(S) $9.00" row can split, stranding a bare
        // "DISCOUNT(S)" above a "TOTAL $ 9.00" that is really the discount.
        let lines = vec![
            "AMOUNT: 441.68".to_string(),
            "466.68".to_string(),
            "NUMBER OF".to_string(),
            "TOTAL ITEMS SOLD".to_string(),
            "DISCOUNT(S)".to_string(),
            "TOTAL $ 9.00".to_string(),
        ];

        assert_ne!(extract_total(&lines), 900);
    }

    #[test]
    fn a_savings_summary_is_not_the_grand_total_however_it_is_worded() {
        // Food Basics 2026-07-31: the savings block is the last thing on the
        // receipt above the payment slip, and the scan runs upward, so
        // "Total of your savings" was reached before the real "TOTAL 6.96".
        // The words are not adjacent, so the old `TOTAL SAVINGS` literal missed
        // it and a $6.96 receipt reported $6.73.
        let lines = vec![
            "SUBTOTAL 6.96".to_string(),
            "TOTAL 6.96".to_string(),
            "CREDIT CR 6.96".to_string(),
            "Total number of items sold = 11".to_string(),
            "****** Your savings today ******".to_string(),
            "Promotional discounts 6.73".to_string(),
            "Total of your savings 6.73".to_string(),
        ];

        assert_eq!(extract_total(&lines), 696);
    }

    #[test]
    fn the_savings_guard_does_not_swallow_a_real_total() {
        // It must stay a *savings* rule: an ordinary grand total that happens
        // to sit under a savings line is still the total.
        let lines = vec![
            "Your Total Savings 6.73".to_string(),
            "TOTAL 95.00".to_string(),
        ];

        assert_eq!(extract_total(&lines), 9_500);
    }

    #[test]
    fn discount_row_carrying_its_own_amount_still_allows_a_real_total() {
        // The guard above must stay narrow: a discount line that has its own
        // number is an ordinary row, and the total after it is genuine.
        let lines = vec!["DISCOUNT 5.00".to_string(), "TOTAL 95.00".to_string()];

        assert_eq!(extract_total(&lines), 9_500);
    }

    #[test]
    fn total_row_carrying_a_tender_amount_is_settled_by_subtotal_plus_tax() {
        // FreshCo unknown-date_freshco_157_38: the price column leans up, so
        // the Corp Gift Card tender's 116.24 lands on the TOTAL row. The
        // trailing-price pick takes the tender; subtotal + tax says otherwise,
        // and 157.38 is right there on the same line.
        let lines = vec![
            "SUBTOTAL $146.48".to_string(),
            "TOTAL TAX $10.90".to_string(),
            "TOTAL $157.38 $116.24".to_string(),
            "Corp Gift Card TENDER".to_string(),
        ];

        assert_eq!(extract_total(&lines), 15_738);
    }

    #[test]
    fn total_row_is_left_alone_when_the_sum_is_not_printed_on_it() {
        // The override needs the arithmetic to be corroborated *on that row*.
        // Here subtotal + tax = 157.38 but the row carries no such amount, so
        // the ordinary pick stands — receipts whose total legitimately differs
        // from subtotal + tax (fees, rounding) must not be rewritten.
        let lines = vec![
            "SUBTOTAL $146.48".to_string(),
            "TOTAL TAX $10.90".to_string(),
            "TOTAL $160.00".to_string(),
        ];

        assert_eq!(extract_total(&lines), 16_000);
    }

    #[test]
    fn bare_total_still_ignores_a_plausible_tax_row_above() {
        // The TAX guard must keep holding when the tax amount is smaller than
        // the subtotal (the normal case for a bare TOTAL line).
        let lines = vec![
            "SUBTOTAL 104.77".to_string(),
            "TAX 8.18".to_string(),
            "**** TOTAL".to_string(),
        ];

        assert_eq!(extract_total(&lines), 0);
    }

    #[test]
    fn total_prefers_single_tender_when_change_due_is_zero() {
        // Pharmasave 2026-07-07_pharmasave_12_19: line grouping handed the
        // TOTAL row the HST amount. With CHANGE DUE at $0.00 the lone VISA
        // tender is the grand total by definition.
        let lines = vec![
            "SUBTOTAL".to_string(),
            "HST $10.79".to_string(),
            "TOTAL $1.40".to_string(),
            "VISA $12.19".to_string(),
            "CHANGE DUE $12.19 $0.00".to_string(),
        ];

        assert_eq!(extract_total(&lines), 1_219);
    }

    #[test]
    fn zero_change_does_not_promote_one_tender_of_a_split() {
        // The relaxation above says "nothing handed back ⇒ that tender is the
        // whole total". True of one instrument; false of two. Cash is not in
        // `payment_amounts`, so the VISA portion looked like the entire charge
        // and was adopted: 23.41 reported as the total of a 33.41 receipt, with
        // nothing to say so. Falling through to the mis-grouped 2.41 is the
        // correct outcome here — wrong, but wrong *loudly*: it disagrees with
        // both the subtotal and the tender block, and `TenderMismatch` fires.
        let lines = vec![
            "SUBTOTAL 31.42".to_string(),
            "HST 2.41".to_string(),
            "TOTAL 2.41".to_string(),
            "VISA 23.41".to_string(),
            "CASH 10.00".to_string(),
            "CHANGE 0.00".to_string(),
        ];
        assert_eq!(extract_total(&lines), 241);
    }

    #[test]
    fn ten_dollars_change_is_not_zero_change() {
        // `"10.00".ends_with("0.00")` is true, so the old suffix test read every
        // whole-ten-dollar change amount as zero — the ordinary cash case, and
        // precisely the population the two-line rule protects.
        let lines = vec![
            "SUBTOTAL 31.42".to_string(),
            "HST 2.41".to_string(),
            "TOTAL 2.41".to_string(),
            "VISA 23.41".to_string(),
            "CHANGE 10.00".to_string(),
        ];
        assert_eq!(extract_total(&lines), 241);
    }

    #[test]
    fn total_picks_max_when_total_and_tax_share_a_line() {
        // OCR collapsed Freshco's two-column "TOTAL | TOTAL TAX | $74.55 | $1.82"
        // row into a single line. The trailing price is the tax; the actual
        // total is the larger value.
        let lines = vec![
            "SUBTOTAL $72.73".to_string(),
            "TOTAL TOTAL TAX $74.55 $1.82".to_string(),
        ];

        assert_eq!(extract_total(&lines), 7_455);
    }

    #[test]
    fn total_reconciles_to_corroborated_charge_when_label_mispaired() {
        // On-device box-position artifact: the TOTAL label paired with the tax
        // row (20.14); the real total (245.87) is orphaned but corroborated by
        // the card tender and the AMOUNT: echo. Reconciliation recovers it.
        let lines = vec![
            "TOTAL 20.14".to_string(),
            "245.87".to_string(),
            "AMOUNT: 245.87".to_string(),
            "MasterCard 245.87".to_string(),
        ];
        assert_eq!(extract_total(&lines), 24_587);
    }

    #[test]
    fn total_reconciles_from_credit_tn_echo_when_total_digits_garbled() {
        // No Frills 2026-04-23_nofrills_11_15: bleed-through from the reverse
        // side garbles the digits on the SUBTOTAL/TOTAL rows ("1 1.1 5" /
        // "1 11 5"), so the label scan yields 0. The clean amount survives on
        // the card slip's "Account: VISA" line and its "CREDIT TN" echo —
        // two corroborating payment lines.
        let lines = vec![
            "SUBTOTALbemutord yom eaib1 1.1 5".to_string(),
            "TOTAL dtiw eeorotuqto yobA nir1 11 5".to_string(),
            "yob Al oto ylno egnorox3.gnigoxbq bnd apot".to_string(),
            "Trans.Type: PURCHASE qqo anoitqeoxe amo2".to_string(),
            "Account: VISA CAD$ 11. 15".to_string(),
            "Card Type: CREDIT".to_string(),
            "CREDIT TN 11.15".to_string(),
        ];
        assert_eq!(extract_total(&lines), 1_115);
    }

    #[test]
    fn total_reconciliation_leaves_correct_total_unchanged() {
        // Correctly paired: the candidate already equals the charged amount, so
        // reconciliation must not fire (this is the desktop/cached-parity guard).
        let lines = vec![
            "TOTAL 50.00".to_string(),
            "AMOUNT: 50.00".to_string(),
            "VISA 50.00".to_string(),
        ];
        assert_eq!(extract_total(&lines), 5_000);
    }

    #[test]
    fn total_reconciliation_ignores_split_tender_card_portion() {
        // Split tender: the real total (50.00) exceeds the card portion (30.00),
        // so the corroborated card+AMOUNT amount must NOT override it.
        let lines = vec![
            "TOTAL 50.00".to_string(),
            "GIFT CARD 20.00".to_string(),
            "AMOUNT: 30.00".to_string(),
            "VISA 30.00".to_string(),
        ];
        assert_eq!(extract_total(&lines), 5_000);
    }

    #[test]
    fn total_reconciliation_holds_on_real_costco_split_tender() {
        // Real Costco split tender (2026-03-07, $466.68 = $25.00 Shop Card +
        // $441.68 MasterCard). The receipt carries two "AMOUNT:" echoes plus the
        // card line, but neither charged amount exceeds the printed total, so the
        // `> candidate` guard must leave 466.68 intact. Exercises the two-AMOUNT,
        // gift-card-classified shape the synthetic split-tender case above misses.
        let lines = vec![
            "TOTAL 466.68".to_string(),
            "Shop Card 25.00".to_string(),
            "AMOUNT: $25.00".to_string(),
            "MASTERCARD".to_string(),
            "AMOUNT: 441.68".to_string(),
            "MasterCard 441.68".to_string(),
            "CHANGE 0.00".to_string(),
        ];
        assert_eq!(extract_total(&lines), 46_668);
    }

    #[test]
    fn total_reconciliation_holds_on_real_costco_single_tender() {
        // Real Costco desktop OCR (2026-03-05): TOTAL is already correctly paired
        // and the AMOUNT:/MasterCard echoes equal it, so reconciliation never
        // fires (charge == candidate, not >). Desktop/cached-parity guard.
        let lines = vec![
            "SUBTOTAL 225.73".to_string(),
            "TAX 20.14".to_string(),
            "TOTAL 245.87".to_string(),
            "AMOUNT: 245.87".to_string(),
            "MasterCard 245.87".to_string(),
            "CHANGE 0.00".to_string(),
        ];
        assert_eq!(extract_total(&lines), 24_587);
    }

    #[test]
    fn total_after_tax_zero_prefers_following_standalone_amount() {
        let lines = vec![
            "Item Count: 33".to_string(),
            "Sub Total 153.55".to_string(),
            "HST".to_string(),
            "hst5% 0.00".to_string(),
            "Total after Tax 0.00".to_string(),
            "153.55".to_string(),
            "Credit Card".to_string(),
            "153.55".to_string(),
        ];

        assert_eq!(extract_total(&lines), 15_355);
    }

    #[test]
    fn total_and_tax_survive_ocr_mangled_total_after_tax_label() {
        // Foody Mart 2026 receipt footer where OCR mangled "Total" -> "lotal":
        //   Sub Total 159.41 / HST 4.54 / list5% 0.00 / lotal after Tax 163.95
        // The grand total is the "after tax" line (163.95) even though "Total"
        // is unreadable, the tax is the HST line (4.54) not the after-tax
        // amount, and the spaced "Sub Total" must not be taken as the total.
        let lines = vec![
            "1iem Count: 40".to_string(),
            "Sub Total 159.41".to_string(),
            "HST 4.54".to_string(),
            "list5% 0.00".to_string(),
            "lotal after Tax 163.95".to_string(),
            "Credit Cand 163.95".to_string(),
        ];

        assert_eq!(extract_total(&lines), 16_395);
        assert_eq!(extract_tax(&lines), Some(454));
        assert_eq!(extract_subtotal(&lines), Some(15_941));
    }

    #[test]
    fn tenders_split_costco_shop_card_and_mastercard() {
        // Costco prints: AMOUNT: $25.00 / REMAINING BALANCE: $0.00 / Shop Card 25.00
        // / XXXXXXXXXXXX4385 / ACCT: MASTERCARD / (next line) 441.68.
        let lines = vec![
            "TOTAL".to_string(),
            "466.68".to_string(),
            "AMOUNT: $25.00".to_string(),
            "REMAINING BALANCE: $0.00".to_string(),
            "Shop Card".to_string(),
            "25.00".to_string(),
            "XXXXXXXXXXXX4385".to_string(),
            "MASTERCARD".to_string(),
            "441.68".to_string(),
        ];

        let tenders = extract_tenders(&lines);
        assert!(tenders_reconcile(&lines, &tenders, 46_668));
        assert_eq!(tenders.len(), 2);
        assert_eq!(tenders[0].kind, "gift_card");
        assert_eq!(tenders[0].amount_cents, 2_500);
        assert_eq!(tenders[0].raw_label, "Shop Card");
        assert_eq!(tenders[1].kind, "card");
        assert_eq!(tenders[1].amount_cents, 44_168);
        assert_eq!(tenders[1].raw_label, "MASTERCARD");
    }

    #[test]
    fn tenders_are_reported_even_when_the_sum_does_not_reconcile() {
        let lines = vec!["TOTAL 50.00".to_string(), "MASTERCARD 30.00".to_string()];
        // Only 30 of 50 covered. The tender line is still what the receipt
        // printed, so it is still reported — discarding it was how a misread
        // amount became indistinguishable from a receipt with no payment block.
        let tenders = extract_tenders(&lines);
        assert_eq!(tenders.len(), 1);
        assert_eq!(tenders[0].amount_cents, 3_000);
        assert!(!tenders_reconcile(&lines, &tenders, 5_000));
    }

    #[test]
    fn a_one_cent_tender_gap_does_not_reconcile() {
        // The old $0.05 tolerance called this reconciled and emitted both
        // tenders as postings, so the payment side summed to 96.64 against an
        // item side summing to 96.65 and beancount rejected the entry. Every
        // amount in a payment block is printed to the cent: a cent off is a
        // misread digit, not rounding.
        let lines = vec![
            "Total 96.65".to_string(),
            "Gift Card 30.05".to_string(),
            "Gift Card 66.59".to_string(),
        ];
        let tenders = extract_tenders(&lines);
        assert_eq!(tenders.len(), 2);
        assert!(!tenders_reconcile(&lines, &tenders, 9_665));
    }

    #[test]
    fn lcbo_split_gift_cards_reconcile() {
        // The shape this must keep working: LCBO pays one slip from two gift
        // cards, and the amounts partition the total instead of echoing it.
        let lines = vec![
            "Total 39.90".to_string(),
            "Deposit (DEP) 0.40".to_string(),
            "Gift Card 18.10".to_string(),
            "608835xxxxx2424684x EXP:NONE".to_string(),
            "AUTHOR.#:607022550 BAL: 0.00".to_string(),
            "Gift Card 21.80".to_string(),
        ];
        let tenders = extract_tenders(&lines);
        assert_eq!(tenders.len(), 2);
        assert!(tenders_reconcile(&lines, &tenders, 3_990));
    }

    #[test]
    fn an_empty_tender_block_is_not_a_disagreement() {
        // Most receipts print no payment block at all; that is silence, not a
        // contradiction, and must not warn.
        let lines = vec!["TOTAL 50.00".to_string()];
        let tenders = extract_tenders(&lines);
        assert!(tenders.is_empty());
        assert!(tenders_reconcile(&lines, &tenders, 5_000));
    }

    #[test]
    fn tenders_ignores_change_and_cash_back_lines() {
        let lines = vec![
            "TOTAL 20.00".to_string(),
            "CASH 25.00".to_string(),
            "CASH BACK 0.00".to_string(),
            "CHANGE 5.00".to_string(),
        ];
        let tenders = extract_tenders(&lines);
        // Only the CASH line is a tender; CASH BACK and CHANGE are not.
        assert_eq!(tenders.len(), 1);
        assert_eq!(tenders[0].amount_cents, 2_500);
        // ...and $25 tendered against a $20 total is not a disagreement once
        // the $5 change is netted off. This used to pass for the wrong reason:
        // the old tolerance check saw 25 vs 20, gave up, and returned nothing.
        assert!(tenders_reconcile(&lines, &tenders, 2_000));
    }

    #[test]
    fn change_is_the_last_amount_on_a_merged_row() {
        // Costco's customer copy prints the card charge and the change on
        // consecutive rows, and line grouping merges them. Reading the FIRST
        // amount took 441.68 as change handed back, so the net tendered went
        // negative (-416.68) and the warning reported a $883.36 discrepancy on
        // a receipt that is merely missing one tender line.
        let lines = vec![
            "TOTAL 466.68".to_string(),
            "Shop Card 25.00".to_string(),
            "MasterCard 441.68 CHANGE 0.00".to_string(),
        ];
        assert_eq!(extract_change(&lines), 0);
    }

    #[test]
    fn exchange_in_a_return_policy_is_not_a_change_line() {
        // `contains("CHANGE")` matches "EXCHANGE"; several corpus receipts
        // print a return policy, and reading one as change due would net a
        // real amount off the tender sum and invent a mismatch.
        let lines = vec![
            "TOTAL 20.00".to_string(),
            "CASH 20.00".to_string(),
            "No Refund, Exchange Only Within 7 Days 20.00".to_string(),
        ];
        assert_eq!(extract_change(&lines), 0);
        let tenders = extract_tenders(&lines);
        assert!(tenders_reconcile(&lines, &tenders, 2_000));
    }

    #[test]
    fn cash_inside_a_longer_word_is_not_a_tender() {
        // `1424970 CASHMERE TP 26.99 H` is toilet paper in the item block, and
        // a bare `contains("CASH")` read it as a $26.99 cash payment — the only
        // tender on the receipt, so it warned that $40.83 was unaccounted for.
        // CASHIER lines are the same trap.
        let lines = vec![
            "1424970 CASHMERE TP 26.99 H".to_string(),
            "CASHIER: 12.00".to_string(),
            "**** TOTAL 67.82".to_string(),
        ];
        assert!(extract_tenders(&lines).is_empty());
    }

    #[test]
    fn plain_cash_is_still_a_tender() {
        let lines = vec!["TOTAL 124.13".to_string(), "CASH 124.13".to_string()];
        let tenders = extract_tenders(&lines);
        assert_eq!(tenders.len(), 1);
        assert_eq!(tenders[0].kind, "cash");
    }

    #[test]
    fn a_gift_card_balance_echo_is_not_a_second_tender() {
        // FreshCo prints the card's REMAINING balance after the purchase. It
        // carries the "GIFT CARD" keyword and a price, so it classified as a
        // second gift-card tender and every FreshCo gift-card receipt warned.
        // Guarding on BALANCE (not just Costco's "REMAINING BALANCE") covers
        // both wordings; no real tender line in either corpus says BALANCE.
        let lines = vec![
            "TOTAL TOTAL TAX $135.46 $3.37".to_string(),
            "Corp Gift Card TENDER $135.46".to_string(),
            "Gift Card Balance: $116.24".to_string(),
        ];
        let tenders = extract_tenders(&lines);
        assert_eq!(tenders.len(), 1);
        assert_eq!(tenders[0].amount_cents, 13_546);
        assert!(tenders_reconcile(&lines, &tenders, 13_546));
    }

    #[test]
    fn a_thousands_separator_with_a_misread_zero_is_not_a_price() {
        // "Win a $1,000 PC gift card" comes back as `$1,00o`. The comma repair
        // only guards against a following *digit*, so `o` let it through and
        // manufactured a $1.00 price out of survey marketing copy — which then
        // classified as a gift-card tender on No Frills and RCSS receipts.
        assert_eq!(
            normalize_decimal_spacing("Vin a $1,00o PC gift card or"),
            "Vin a $1,00o PC gift card or"
        );
        let lines = vec![
            "TOTAL 6.88".to_string(),
            "Account: MASTERCARD CAD$ 6.88".to_string(),
            "Vin a $1,00o PC gift card or".to_string(),
        ];
        let tenders = extract_tenders(&lines);
        assert_eq!(tenders.len(), 1);
        assert!(tenders_reconcile(&lines, &tenders, 688));
    }

    #[test]
    fn a_real_comma_decimal_point_still_parses() {
        // The repair this guard sits inside must keep working: OCR reads a
        // price's decimal point as a comma often enough to be worth repairing.
        assert_eq!(normalize_decimal_spacing("BANANAS 0,99"), "BANANAS 0.99");
        assert_eq!(
            normalize_decimal_spacing("TOTAL $12,50 H"),
            "TOTAL $12.50 H"
        );
    }

    #[test]
    fn change_larger_than_the_tenders_is_dropped() {
        // Redaction re-scanned costco_46668 with the amount column shifted one
        // row: `MasterCard` / `CHANGE 441.68` / `0.00`. The card tender loses
        // its amount AND the card charge reads as change, so netting gives
        // -416.68 and the warning claims 883.36 is unaccounted for. No receipt
        // hands back more than it took in, so the change term is the untrusted
        // one. Dropping it is not a repair — the sum still misses the total,
        // and now by 441.68, which is exactly the tender that went missing.
        let lines = vec![
            "****TOTAL 466.68".to_string(),
            "Shop Card 25.00".to_string(),
            "MasterCard".to_string(),
            "CHANGE 441.68".to_string(),
            "0.00".to_string(),
        ];
        let tenders = extract_tenders(&lines);
        assert_eq!(tenders.len(), 1);
        assert_eq!(extract_change(&lines), 44_168);
        assert_eq!(tendered_net_cents(&lines, &tenders), 2_500);
        assert!(!tenders_reconcile(&lines, &tenders, 46_668));
    }
}
