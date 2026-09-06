//! Receipt amounts extraction.
use super::prices::*;
use super::tenders::*;
use regex::Regex;
use std::sync::OnceLock;
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
pub(super) fn repair_leading_currency_digit(lines: &[String], candidate: i64) -> i64 {
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
pub(super) fn reconcile_total_with_charge(lines: &[String], candidate: i64) -> i64 {
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
pub(super) fn re_total_savings_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)TOTAL\b.{0,15}?\bSAV(?:E|ED|ING|INGS)\b").unwrap())
}
pub(super) fn extract_total_raw(lines: &[String]) -> i64 {
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
pub(super) fn reconcile_tax(tax: Option<i64>, subtotal: Option<i64>, total: i64) -> Option<i64> {
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
pub(super) fn reconcile_summary_shift(
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
pub(super) fn tax_is_impossible(tax: i64, subtotal: i64, total: i64) -> bool {
    tax == total
        || tax == subtotal
        || tax > (subtotal as f64 * TAX_PLAUSIBLE_MAX_FRAC) as i64
        || tax == 0
}

/// Every amount printed in the summary block — the rows from the subtotal label
/// through the total, plus a little slack either side, because the shift this
/// serves is precisely a block whose amounts have slid out of their own rows.
pub(super) fn summary_block_amounts(lines: &[String], total: i64) -> Vec<i64> {
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
pub(super) fn trailer_tax_echo(lines: &[String], total: i64) -> Option<i64> {
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
pub(super) fn re_tax_rate_breakdown() -> &'static Regex {
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
pub(super) fn sum_summary_block_tax_rows(lines: &[String]) -> Option<i64> {
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
                        // A price on the line after TOTAL means that price is
                        // the total, so this earlier amount is not.
                        is_total_value = !(idx + 3 < lines.len()
                            && extract_price_from_line(&lines[idx + 3]).is_some());
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
pub(super) fn re_subtotal_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // The inner 'O' in SUBTOTAL is the most common OCR victim — accept the
    // usual O-confusables (0/C/Q/D/G). Costco receipts have been observed as
    // SUBTCTAL.
    RE.get_or_init(|| Regex::new(r"SUB\s*T[OCQDG0]TAL").unwrap())
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
