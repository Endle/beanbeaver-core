//! Receipt tenders extraction.
use super::prices::*;
use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct TenderLine {
    pub raw_label: String,
    pub amount_cents: i64,
    pub kind: &'static str,
}
pub(super) fn classify_tender_line(line_upper: &str) -> Option<&'static str> {
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
pub(super) fn re_cash_label() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bCASH\b").unwrap())
}
pub(super) fn tender_amount_for_line(lines: &[String], idx: usize) -> Option<i64> {
    if let Some(amount) = extract_price_from_line(&lines[idx]) {
        return Some(amount);
    }
    if idx + 1 < lines.len() && re_standalone_amount().is_match(&lines[idx + 1]) {
        return extract_price_from_line(&lines[idx + 1]);
    }
    None
}
pub(super) fn trim_tender_label(line: &str) -> String {
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
pub(super) fn re_change_label() -> &'static Regex {
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
