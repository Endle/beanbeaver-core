use crate::receipt_common::ReceiptWarningKind;

#[derive(Clone, Debug)]
pub struct FormatterItemInput {
    pub description: String,
    pub price: String,
    pub quantity: i32,
    pub posting_account: String,
}

#[derive(Clone, Debug)]
pub struct FormatterWarningInput {
    pub kind: ReceiptWarningKind,
    pub message: String,
    pub after_item_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct FormatterTenderInput {
    pub amount: String,
    pub account: Option<String>,
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct FormatterReceiptInput {
    pub merchant: String,
    pub date_iso: String,
    pub date_is_placeholder: bool,
    pub total: String,
    pub tax: Option<String>,
    /// Only the staged-draft path ever read this; see
    /// [`format_draft_beancount`]. Unreachable at HEAD.
    #[allow(dead_code, reason = "unreachable staged-draft/matching path")]
    pub image_filename: String,
    pub raw_text: String,
    pub items: Vec<FormatterItemInput>,
    pub warnings: Vec<FormatterWarningInput>,
    pub tenders: Vec<FormatterTenderInput>,
    /// Beancount commodity for every amount on this entry (e.g. `CAD`, `USD`,
    /// `GBP`). The user's per-device operating currency — not hard-coded.
    pub currency: String,
    /// Account the tax posting lands on (e.g. `Expenses:Tax:HST`,
    /// `Expenses:Tax:VAT`). Per-device, since the tax regime is stable per user.
    pub tax_account: String,
}

fn pending_account_for_kind(kind: &str) -> &'static str {
    match kind {
        "gift_card" => "Assets:GiftCards:PENDING",
        "cash" => "Assets:Cash:PENDING",
        "store_credit" => "Assets:StoreCredit:PENDING",
        _ => "Liabilities:CreditCard:PENDING",
    }
}

/// Build payment postings for the receipt. Returns one posting per tender when
/// `receipt.tenders` is non-empty and they account for exactly the total,
/// otherwise a single posting for `-total` against `fallback_account` (today's
/// legacy shape).
///
/// The reconciliation guard lives here, in the one place that owes beancount a
/// balanced entry, rather than upstream in `receipt_fields::extract_tenders`
/// where it used to sit. That matters because the two layers want opposite
/// things from a tender block that doesn't add up: the parser wants to *report*
/// it (`ReceiptWarningKind::TenderMismatch`), and this function must not
/// *emit* it — postings summing to `-sum` against an item side summing to
/// `total` are a transaction beancount rejects.
///
/// Note this is not the parser's warning restated as a silent drop. The receipt
/// is flagged either way; all that is withheld is the breakdown of a payment
/// split we know to be wrong, in favour of the one posting that is certainly
/// right — the total was charged somehow.
fn build_payment_postings(
    receipt: &FormatterReceiptInput,
    fallback_account: &str,
    total_cents: i64,
) -> Vec<(String, String, Option<String>)> {
    let currency = &receipt.currency;
    let card_comment =
        extract_card_last4(&receipt.raw_text).map(|last4| format!("card ****{last4}"));
    let tender_cents: i64 = receipt
        .tenders
        .iter()
        .map(|tender| decimal_to_cents(&tender.amount))
        .sum();
    if receipt.tenders.is_empty() || tender_cents != total_cents {
        return vec![(
            fallback_account.to_string(),
            format!("{} {currency}", cents_to_fixed(-total_cents)),
            card_comment,
        )];
    }

    receipt
        .tenders
        .iter()
        .map(|tender| {
            let account = tender
                .account
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| pending_account_for_kind(&tender.kind).to_string());
            let amount_cents = decimal_to_cents(&tender.amount);
            let comment = if tender.kind == "card" {
                card_comment.clone()
            } else {
                Some(tender.kind.replace('_', " "))
            };
            (
                account,
                format!("{} {currency}", cents_to_fixed(-amount_cents)),
                comment,
            )
        })
        .collect()
}

#[allow(
    dead_code,
    reason = "unreachable staged-draft/matching path; see above"
)]
#[derive(Clone, Debug)]
pub struct EnrichedPostingInput {
    pub account: String,
    pub number: Option<String>,
    pub currency: Option<String>,
}

#[allow(
    dead_code,
    reason = "unreachable staged-draft/matching path; see above"
)]
#[derive(Clone, Debug)]
pub struct EnrichedMatchInput {
    pub transaction_date_iso: String,
    pub payee: String,
    pub narration: String,
    pub postings: Vec<EnrichedPostingInput>,
    pub file_path: String,
    pub line_number: i32,
    pub confidence: f64,
    pub match_details: String,
}

/// Shared with `receipt_parser`'s balance check on purpose: that warning exists
/// to predict whether the postings this module emits will balance, so the two
/// must read a price string exactly the same way.
pub(crate) fn decimal_to_cents(value: &str) -> i64 {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0;
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
    if negative {
        -value
    } else {
        value
    }
}

fn cents_to_fixed(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

fn format_postings_aligned(
    postings: &[(String, String, Option<String>)],
    indent: &str,
) -> Vec<String> {
    if postings.is_empty() {
        return Vec::new();
    }

    let max_account_len = postings
        .iter()
        .map(|(account, _, _)| account.len())
        .max()
        .unwrap_or(0);
    let max_amount_len = postings
        .iter()
        .map(|(_, amount, _)| amount.len())
        .max()
        .unwrap_or(0);

    postings
        .iter()
        .map(|(account, amount, comment)| {
            let base = format!(
                "{indent}{account:<account_width$}  {amount:>amount_width$}",
                account_width = max_account_len,
                amount_width = max_amount_len,
            );
            match comment {
                Some(comment) if !comment.is_empty() => format!("{base}  ; {comment}"),
                _ => base,
            }
        })
        .collect()
}

fn extract_card_last4(raw_text: &str) -> Option<String> {
    for line in raw_text.lines() {
        if !line.contains('*') {
            continue;
        }
        let mut star_run = 0usize;
        let chars: Vec<char> = line.chars().collect();
        let mut idx = 0usize;
        while idx < chars.len() {
            if chars[idx] == '*' {
                star_run += 1;
                idx += 1;
                continue;
            }
            if star_run >= 2 {
                while idx < chars.len() && chars[idx].is_whitespace() {
                    idx += 1;
                }
                if idx + 4 <= chars.len() {
                    let candidate: String = chars[idx..idx + 4].iter().collect();
                    if candidate.chars().all(|ch| ch.is_ascii_digit()) {
                        let boundary_ok =
                            idx + 4 == chars.len() || !chars[idx + 4].is_ascii_digit();
                        if boundary_ok {
                            return Some(candidate);
                        }
                    }
                }
            }
            star_run = 0;
            idx += 1;
        }
    }
    None
}

/// Which warning kinds earn a `; WARN:PARSER` comment in the ledger text.
///
/// This is the formatter exercising the same right the phone UI has: the parser
/// reports every finding, and each consumer decides what to do with it. A ledger
/// entry is a durable artifact a human reads later, so it carries the findings
/// that question the *numbers* — and not [`ReceiptWarningKind::UncategorizedItem`],
/// which would stamp a comment on every unclassified line on every receipt and
/// say nothing the `Expenses:FIXME` posting right above it doesn't already.
fn belongs_in_ledger_text(kind: ReceiptWarningKind) -> bool {
    !matches!(kind, ReceiptWarningKind::UncategorizedItem)
}

fn build_posting_warning_map(
    warnings: &[FormatterWarningInput],
    item_posting_indexes: &[usize],
) -> Vec<(usize, String)> {
    let mut mapped = Vec::new();
    for warning in warnings {
        if warning.message.is_empty() || !belongs_in_ledger_text(warning.kind) {
            continue;
        }
        let posting_idx = if item_posting_indexes.is_empty() {
            0
        } else {
            let target_item_idx = match warning.after_item_index {
                Some(index) => index.min(item_posting_indexes.len().saturating_sub(1)),
                None => item_posting_indexes.len().saturating_sub(1),
            };
            item_posting_indexes[target_item_idx]
        };
        mapped.push((posting_idx, warning.message.clone()));
    }
    mapped
}

fn inject_posting_warnings(
    formatted_postings: Vec<String>,
    posting_warnings: Vec<(usize, String)>,
) -> Vec<String> {
    if posting_warnings.is_empty() {
        return formatted_postings;
    }

    let mut output = Vec::new();
    for (idx, posting_line) in formatted_postings.into_iter().enumerate() {
        output.push(posting_line);
        for (warning_idx, message) in posting_warnings
            .iter()
            .filter(|(warning_idx, _)| *warning_idx == idx)
        {
            let _ = warning_idx;
            output.push(format!("; WARN:PARSER {message}"));
        }
    }
    output
}

pub fn format_parsed_receipt(
    receipt: &FormatterReceiptInput,
    credit_card_account: &str,
    image_sha256: Option<&str>,
) -> String {
    let currency = &receipt.currency;
    let total_cents = decimal_to_cents(&receipt.total);
    let tax_cents = receipt.tax.as_deref().map(decimal_to_cents);
    let mut lines = Vec::new();

    lines.push("; === PARSED RECEIPT - AWAITING CC MATCH ===".to_string());
    lines.push(format!("; @merchant: {}", receipt.merchant));
    if receipt.date_is_placeholder {
        lines.push("; @date: UNKNOWN".to_string());
        lines.push(format!(
            "; FIXME: unknown date (placeholder used: {})",
            receipt.date_iso
        ));
    } else {
        lines.push(format!("; @date: {}", receipt.date_iso));
    }
    lines.push(format!("; @total: {}", cents_to_fixed(total_cents)));
    lines.push(format!("; @items: {}", receipt.items.len()));
    if let Some(tax_cents) = tax_cents {
        if tax_cents != 0 {
            lines.push(format!("; @tax: {}", cents_to_fixed(tax_cents)));
        }
    }
    lines.push(String::new());

    let merchant_clean = receipt.merchant.replace('"', "'");
    lines.push(format!(
        r#"{} * "{}" "Receipt scan""#,
        receipt.date_iso, merchant_clean
    ));

    // Real beancount metadata (not `;` comments) so a consumer can find every
    // BeanBeaver-generated entry with `grep -R beanbeaver-id <ledger-root>`, and
    // a specific receipt by its content-hash token. `document:` is beancount's
    // native link and is resolved by each user against their own
    // `option "documents"` root, so it stays correct across arbitrary layouts.
    if let Some(id) = beanbeaver_id(&receipt.date_iso, receipt.date_is_placeholder, image_sha256) {
        lines.push(format!("  beanbeaver-id: \"{id}\""));
    }
    if let Some(sha) = image_sha256.filter(|value| !value.is_empty()) {
        lines.push(format!("  beanbeaver-image-sha256: \"{sha}\""));
    }
    if let Some(doc) = beanbeaver_document_relpath(
        &receipt.date_iso,
        receipt.date_is_placeholder,
        &receipt.merchant,
        image_sha256,
    ) {
        lines.push(format!("  document: \"{doc}\""));
    }

    let mut postings = build_payment_postings(receipt, credit_card_account, total_cents);
    let payment_posting_count = postings.len();

    let mut item_total_cents = 0i64;
    let mut item_posting_indexes = Vec::new();
    for item in &receipt.items {
        item_posting_indexes.push(postings.len());
        let desc_clean = item.description.replace('"', "'");
        let comment = if item.quantity > 1 {
            Some(format!("{desc_clean} (qty {})", item.quantity))
        } else {
            Some(desc_clean)
        };
        postings.push((
            item.posting_account.clone(),
            format!(
                "{} {currency}",
                cents_to_fixed(decimal_to_cents(&item.price))
            ),
            comment,
        ));
        item_total_cents += decimal_to_cents(&item.price);
    }

    if let Some(tax_cents) = tax_cents {
        if tax_cents != 0 {
            postings.push((
                receipt.tax_account.clone(),
                format!("{} {currency}", cents_to_fixed(tax_cents)),
                None,
            ));
            item_total_cents += tax_cents;
        }
    }

    if total_cents > 0 && item_total_cents != total_cents {
        let diff = total_cents - item_total_cents;
        if diff > 0 {
            postings.push((
                "Expenses:FIXME".to_string(),
                format!("{} {currency}", cents_to_fixed(diff)),
                Some("FIXME: unaccounted amount".to_string()),
            ));
        }
    }

    let formatted_postings = format_postings_aligned(&postings, "  ");
    let posting_warnings = build_posting_warning_map(&receipt.warnings, &item_posting_indexes);
    lines.extend(inject_posting_warnings(
        formatted_postings,
        posting_warnings,
    ));
    let _ = payment_posting_count;

    if !receipt.raw_text.is_empty() {
        lines.push(String::new());
        lines.push("; --- Raw OCR Text (for reference) ---".to_string());
        for ocr_line in receipt.raw_text.lines() {
            if !ocr_line.trim().is_empty() {
                lines.push(format!("; {ocr_line}"));
            }
        }
    }

    lines.push(String::new());
    lines.join("\n")
}

/// **Unreachable at HEAD — awaiting a removal decision.** This and the items
/// marked alongside it (`EnrichedPostingInput`, `EnrichedMatchInput`,
/// `format_draft_beancount`, `generate_filename`, `format_enriched_transaction`,
/// and `FormatterReceiptInput::image_filename`) are the desktop staged-draft and
/// receipt-to-transaction *matching* output path. Nothing in this workspace calls
/// them; their own unit tests are the only callers, which is why narrowing this
/// module to `pub(crate)` is what finally surfaced them.
///
/// Two reasons they are marked rather than deleted: the workspace rule that
/// features are not removed without fresh explicit approval, and the fact that
/// `CLAUDE.md` already says matching "never belongs here" — which makes their
/// removal a scope question for a human, not a refactor step.
#[allow(
    dead_code,
    reason = "unreachable staged-draft/matching path; see above"
)]
pub fn format_draft_beancount(
    receipt: &FormatterReceiptInput,
    credit_card_account: &str,
) -> String {
    let currency = &receipt.currency;
    let total_cents = decimal_to_cents(&receipt.total);
    let tax_cents = receipt.tax.as_deref().map(decimal_to_cents);
    let mut lines = Vec::new();

    lines.push("; === DRAFT - REVIEW NEEDED ===".to_string());
    lines.push(format!("; Source: {}", receipt.image_filename));
    lines.push("; Generated from OCR - please verify all values".to_string());
    lines.push(String::new());

    if receipt.date_is_placeholder {
        lines.push(format!(
            "; FIXME: unknown date (placeholder used: {})",
            receipt.date_iso
        ));
    }
    let merchant_clean = receipt.merchant.replace('"', "'");
    lines.push(format!(
        r#"{} * "{}" "FIXME: add description""#,
        receipt.date_iso, merchant_clean
    ));

    let mut postings = build_payment_postings(receipt, credit_card_account, total_cents);

    let mut item_total_cents = 0i64;
    let mut item_posting_indexes = Vec::new();
    for item in &receipt.items {
        item_posting_indexes.push(postings.len());
        let desc_clean = item.description.replace('"', "'");
        let comment = if item.quantity > 1 {
            Some(format!("{desc_clean} (qty {})", item.quantity))
        } else {
            Some(desc_clean)
        };
        postings.push((
            item.posting_account.clone(),
            format!(
                "{} {currency}",
                cents_to_fixed(decimal_to_cents(&item.price))
            ),
            comment,
        ));
        item_total_cents += decimal_to_cents(&item.price);
    }

    if let Some(tax_cents) = tax_cents {
        if tax_cents != 0 {
            postings.push((
                receipt.tax_account.clone(),
                format!("{} {currency}", cents_to_fixed(tax_cents)),
                None,
            ));
            item_total_cents += tax_cents;
        }
    }

    if total_cents > 0 && item_total_cents != total_cents {
        let diff = total_cents - item_total_cents;
        if diff > 0 {
            postings.push((
                "Expenses:FIXME".to_string(),
                format!("{} {currency}", cents_to_fixed(diff)),
                Some("FIXME: unaccounted amount".to_string()),
            ));
        } else if diff < 0 {
            lines.push(format!(
                "  ; WARNING: items total ({}) exceeds receipt total ({})",
                cents_to_fixed(item_total_cents),
                cents_to_fixed(total_cents)
            ));
        }
    }

    let formatted_postings = format_postings_aligned(&postings, "  ");
    let posting_warnings = build_posting_warning_map(&receipt.warnings, &item_posting_indexes);
    lines.extend(inject_posting_warnings(
        formatted_postings,
        posting_warnings,
    ));
    lines.push(String::new());
    lines.push("; --- Raw OCR Text (for reference) ---".to_string());
    for ocr_line in receipt.raw_text.lines() {
        if !ocr_line.trim().is_empty() {
            lines.push(format!("; {ocr_line}"));
        }
    }

    lines.join("\n")
}

/// Subdirectory, relative to the ledger's `option "documents"` root, that
/// BeanBeaver writes scanned receipt images into. Kept out of the ledger root so
/// exporting receipts never pollutes the user's own directory layout.
pub const BEANBEAVER_DOC_SUBDIR: &str = "beanbeaver";

/// Lowercase, dash-collapsed slug of a merchant name for use in filenames/ids
/// (e.g. `COSTCO WHOLESALE #123` -> `costco-wholesale-123`). Never empty.
fn merchant_slug(merchant: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in merchant.to_ascii_lowercase().chars() {
        let normalized = if ch.is_ascii_alphanumeric() { ch } else { '-' };
        if normalized == '-' {
            if previous_dash {
                continue;
            }
            previous_dash = true;
        } else {
            previous_dash = false;
        }
        slug.push(normalized);
    }
    slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        slug = "unknown".to_string();
    }
    slug
}

/// The 8-char content-hash token shared by the `beanbeaver-id`, the `document:`
/// path, and the image filename, derived from the full image SHA-256. `None`
/// when no (non-empty) hash is available, so callers omit the metadata entirely.
fn sha_token(image_sha256: Option<&str>) -> Option<String> {
    let sha = image_sha256?.trim();
    if sha.is_empty() {
        return None;
    }
    Some(sha.chars().take(8).collect())
}

/// Stable, greppable identifier stamped on every BeanBeaver-generated
/// transaction: `bb-<yyyymmdd>-<sha8>` (date compacted so the token is one
/// word). `unknowndate` stands in for a placeholder date.
pub fn beanbeaver_id(
    date_iso: &str,
    date_is_placeholder: bool,
    image_sha256: Option<&str>,
) -> Option<String> {
    let token = sha_token(image_sha256)?;
    let date_str = if date_is_placeholder {
        "unknowndate".to_string()
    } else {
        date_iso.replace('-', "")
    };
    Some(format!("bb-{date_str}-{token}"))
}

/// Path of a receipt image relative to the documents root:
/// `beanbeaver/<date>-<merchant>-<sha8>.jpg`. This is exactly the value written
/// into the `document:` metadata, so the caller that saves the JPEG must use it
/// verbatim as the destination path. `None` when no image hash is available.
pub fn beanbeaver_document_relpath(
    date_iso: &str,
    date_is_placeholder: bool,
    merchant: &str,
    image_sha256: Option<&str>,
) -> Option<String> {
    let token = sha_token(image_sha256)?;
    let date_str = if date_is_placeholder {
        "unknown-date"
    } else {
        date_iso
    };
    Some(format!(
        "{BEANBEAVER_DOC_SUBDIR}/{date_str}-{}-{token}.jpg",
        merchant_slug(merchant)
    ))
}

#[allow(
    dead_code,
    reason = "unreachable staged-draft/matching path; see format_draft_beancount"
)]
pub fn generate_filename(date_iso: &str, date_is_placeholder: bool, merchant: &str) -> String {
    let date_str = if date_is_placeholder {
        "unknown-date"
    } else {
        date_iso
    };

    format!("{date_str}-{}.beancount", merchant_slug(merchant))
}

#[allow(
    dead_code,
    reason = "unreachable staged-draft/matching path; see format_draft_beancount"
)]
pub fn format_enriched_transaction(
    receipt: &FormatterReceiptInput,
    match_input: &EnrichedMatchInput,
    default_expense: &str,
    // Carried forward from the receipt entry being matched so the merged
    // transaction keeps the same greppable identity and image link. `None` for
    // receipts predating the metadata (e.g. an older scan without a hash).
    beanbeaver_id: Option<&str>,
    document: Option<&str>,
) -> String {
    let currency = &receipt.currency;
    let receipt_total_cents = decimal_to_cents(&receipt.total);
    let tax_cents = receipt.tax.as_deref().map(decimal_to_cents);
    let mut lines = Vec::new();

    lines.push("; === ENRICHED TRANSACTION - REVIEW NEEDED ===".to_string());
    lines.push(format!("; Receipt: {}", receipt.image_filename));
    lines.push(format!(
        "; Matched: {}:{}",
        match_input.file_path, match_input.line_number
    ));
    lines.push(format!(
        "; Confidence: {:.0}% ({})",
        match_input.confidence * 100.0,
        match_input.match_details
    ));
    lines.push(String::new());

    let payee_clean = match_input.payee.replace('"', "'");
    let narration_clean = match_input.narration.replace('"', "'");
    lines.push(format!(
        r#"{} * "{}" "{}""#,
        match_input.transaction_date_iso, payee_clean, narration_clean
    ));

    // Carry the receipt's identity onto the merged transaction so `grep -R
    // beanbeaver-id` still finds it and the image link survives the match.
    if let Some(id) = beanbeaver_id.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("  beanbeaver-id: \"{id}\""));
    }
    if let Some(doc) = document.filter(|value| !value.trim().is_empty()) {
        lines.push(format!("  document: \"{doc}\""));
    }

    let mut cc_account: Option<String> = None;
    let mut cc_amount_cents: Option<i64> = None;
    let mut original_expense: Option<String> = None;

    for posting in &match_input.postings {
        let Some(number) = posting.number.as_deref() else {
            continue;
        };
        let number_cents = decimal_to_cents(number);
        if number_cents < 0 {
            cc_account = Some(posting.account.clone());
            cc_amount_cents = Some(number_cents);
        } else if number_cents > 0 {
            original_expense = Some(posting.account.clone());
        }
    }

    let expense_base = original_expense.unwrap_or_else(|| default_expense.to_string());
    let mut postings = Vec::new();
    if !receipt.tenders.is_empty() {
        // Multi-tender: replace the card tender's PENDING placeholder with the matched
        // CC account; non-card tenders render as additional postings (PENDING fallback
        // until the user picks a real asset account in review).
        let resolved_card_account = cc_account
            .clone()
            .unwrap_or_else(|| "Liabilities:CreditCard:FIXME".to_string());
        let mut card_used = false;
        for tender in &receipt.tenders {
            let amount_cents = decimal_to_cents(&tender.amount);
            let (account, comment) = if tender.kind == "card" && !card_used {
                card_used = true;
                (resolved_card_account.clone(), None)
            } else {
                let account = tender
                    .account
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| pending_account_for_kind(&tender.kind).to_string());
                (account, Some(tender.kind.replace('_', " ")))
            };
            postings.push((
                account,
                format!("{} {currency}", cents_to_fixed(-amount_cents)),
                comment,
            ));
        }
    } else if let (Some(cc_account), Some(cc_amount_cents)) = (cc_account.clone(), cc_amount_cents)
    {
        postings.push((
            cc_account,
            format!("{} {currency}", cents_to_fixed(cc_amount_cents)),
            None,
        ));
    } else {
        postings.push((
            "Liabilities:CreditCard:FIXME".to_string(),
            format!("{} {currency}", cents_to_fixed(-receipt_total_cents)),
            None,
        ));
    }

    let mut items_total_cents = 0i64;
    for item in &receipt.items {
        let desc_clean = item.description.replace('"', "'");
        let comment = if item.quantity > 1 {
            Some(format!("{desc_clean} (qty {})", item.quantity))
        } else {
            Some(desc_clean)
        };
        postings.push((
            item.posting_account.clone(),
            format!(
                "{} {currency}",
                cents_to_fixed(decimal_to_cents(&item.price))
            ),
            comment,
        ));
        items_total_cents += decimal_to_cents(&item.price);
    }

    if let Some(tax_cents) = tax_cents {
        if tax_cents != 0 {
            postings.push((
                receipt.tax_account.clone(),
                format!("{} {currency}", cents_to_fixed(tax_cents)),
                None,
            ));
            items_total_cents += tax_cents;
        }
    }

    // Multi-tender: items+tax should equal the full receipt total (the matcher's
    // amount comparison already handles the card vs. total reconciliation).
    let expected_total_cents = if !receipt.tenders.is_empty() {
        receipt_total_cents
    } else {
        cc_amount_cents
            .map(|value| value.abs())
            .unwrap_or(receipt_total_cents)
    };
    if expected_total_cents > 0 && items_total_cents != expected_total_cents {
        let diff = expected_total_cents - items_total_cents;
        if diff > 1 {
            postings.push((
                expense_base.clone(),
                format!("{} {currency}", cents_to_fixed(diff)),
                Some("remaining/unitemized".to_string()),
            ));
        } else if diff < -1 {
            lines.push(format!(
                "  ; WARNING: items total ({}) exceeds transaction ({})",
                cents_to_fixed(items_total_cents),
                cents_to_fixed(expected_total_cents)
            ));
        }
    }

    lines.extend(format_postings_aligned(&postings, "  "));
    lines.push(String::new());
    lines.push("; --- Original Transaction (to be replaced) ---".to_string());
    lines.push(format!(
        r#"; {} * "{}" "{}""#,
        match_input.transaction_date_iso, payee_clean, narration_clean
    ));
    for posting in &match_input.postings {
        if let (Some(number), Some(currency)) = (&posting.number, &posting.currency) {
            lines.push(format!(";   {}  {} {}", posting.account, number, currency));
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CC: &str = "Liabilities:CreditCard:PENDING";

    fn item(description: &str, price: &str, quantity: i32, account: &str) -> FormatterItemInput {
        FormatterItemInput {
            description: description.to_string(),
            price: price.to_string(),
            quantity,
            posting_account: account.to_string(),
        }
    }

    /// Minimal receipt; tests override individual fields.
    fn base() -> FormatterReceiptInput {
        FormatterReceiptInput {
            merchant: "COSTCO".to_string(),
            date_iso: "2026-02-18".to_string(),
            date_is_placeholder: false,
            total: "20.00".to_string(),
            tax: None,
            image_filename: "costco.jpg".to_string(),
            raw_text: String::new(),
            items: vec![],
            warnings: vec![],
            tenders: vec![],
            currency: "CAD".to_string(),
            tax_account: "Expenses:Tax:HST".to_string(),
        }
    }

    /// `format_parsed_receipt`: header metadata, transaction line, item + tax
    /// postings, the card-last4 comment lifted from raw text, and the FIXME
    /// unaccounted-amount balancing posting.
    #[test]
    fn parsed_receipt_renders_metadata_postings_and_fixme() {
        let mut r = base();
        r.tax = Some("1.00".to_string());
        r.raw_text = "COSTCO\n**** 1234".to_string();
        r.items = vec![item(
            "COKE ZERO",
            "17.19",
            1,
            "Expenses:Food:Grocery:Drink:CocaCola",
        )];

        let out = format_parsed_receipt(&r, CC, None);

        assert!(out.contains("; === PARSED RECEIPT - AWAITING CC MATCH ==="));
        assert!(out.contains("; @merchant: COSTCO"));
        assert!(out.contains("; @date: 2026-02-18"));
        assert!(out.contains("; @total: 20.00"));
        assert!(out.contains("; @items: 1"));
        assert!(out.contains("; @tax: 1.00"));
        // With no image hash, no BeanBeaver identity/document metadata is emitted.
        assert!(
            !out.contains("beanbeaver-id"),
            "no sha passed => no id line"
        );
        assert!(
            !out.contains("document:"),
            "no sha passed => no document line"
        );
        assert!(out.contains(r#"2026-02-18 * "COSTCO" "Receipt scan""#));

        // payment posting: fallback CC, negative total, card ****last4 comment
        assert!(out.contains(CC));
        assert!(out.contains("-20.00 CAD"));
        assert!(out.contains("; card ****1234"));

        // item posting
        assert!(out.contains("Expenses:Food:Grocery:Drink:CocaCola"));
        assert!(out.contains("17.19 CAD"));
        assert!(out.contains("; COKE ZERO"));

        // tax posting + FIXME unaccounted (20.00 - 17.19 - 1.00 = 1.81)
        assert!(out.contains("Expenses:Tax:HST"));
        assert!(out.contains("Expenses:FIXME"));
        assert!(out.contains("1.81 CAD"));
        assert!(out.contains("; FIXME: unaccounted amount"));

        // raw OCR reference block
        assert!(out.contains("; --- Raw OCR Text (for reference) ---"));
        assert!(out.contains("; **** 1234"));
    }

    /// A passed image sha renders the greppable BeanBeaver metadata block: a
    /// stable id, the full sha, and a `document:` link under `beanbeaver/`. The
    /// 8-char content token is shared by the id and the document filename.
    #[test]
    fn parsed_receipt_includes_beanbeaver_metadata_when_sha_present() {
        let out = format_parsed_receipt(&base(), CC, Some("a1b2c3d4e5f6a7b8"));
        assert!(
            out.contains(r#"  beanbeaver-id: "bb-20260218-a1b2c3d4""#),
            "{out}"
        );
        assert!(out.contains(r#"  beanbeaver-image-sha256: "a1b2c3d4e5f6a7b8""#));
        assert!(out.contains(r#"  document: "beanbeaver/2026-02-18-costco-a1b2c3d4.jpg""#));
        // The id and the document filename share the same 8-char content token,
        // so grepping it finds both the ledger entry and the image file.
        assert!(out.matches("a1b2c3d4").count() >= 2);
    }

    /// Placeholder-date receipts still get identity/document metadata, using the
    /// `unknowndate` / `unknown-date` stand-ins.
    #[test]
    fn beanbeaver_metadata_handles_placeholder_date() {
        let id = beanbeaver_id("2026-02-18", true, Some("a1b2c3d4e5")).unwrap();
        assert_eq!(id, "bb-unknowndate-a1b2c3d4");
        let doc = beanbeaver_document_relpath("2026-02-18", true, "COSTCO #42", Some("a1b2c3d4e5"))
            .unwrap();
        assert_eq!(doc, "beanbeaver/unknown-date-costco-42-a1b2c3d4.jpg");
    }

    /// No hash => the identity helpers yield nothing (metadata is omitted).
    #[test]
    fn beanbeaver_identity_absent_without_hash() {
        assert!(beanbeaver_id("2026-02-18", false, None).is_none());
        assert!(beanbeaver_id("2026-02-18", false, Some("  ")).is_none());
        assert!(beanbeaver_document_relpath("2026-02-18", false, "COSTCO", None).is_none());
    }

    /// Placeholder dates surface an UNKNOWN marker plus a FIXME note.
    #[test]
    fn parsed_receipt_flags_placeholder_date() {
        let mut r = base();
        r.date_is_placeholder = true;
        let out = format_parsed_receipt(&r, CC, None);
        assert!(out.contains("; @date: UNKNOWN"));
        assert!(out.contains("; FIXME: unknown date (placeholder used: 2026-02-18)"));
    }

    /// Quantities > 1 are annotated in the item posting comment.
    #[test]
    fn parsed_receipt_annotates_multi_quantity_items() {
        let mut r = base();
        r.items = vec![item("WATER", "2.00", 3, "Expenses:Food:Grocery:Drink")];
        let out = format_parsed_receipt(&r, CC, None);
        assert!(out.contains("; WATER (qty 3)"), "{out}");
    }

    /// Multiple tenders each get a payment posting; a non-card tender uses its
    /// review-assigned asset account and a kind comment.
    #[test]
    fn parsed_receipt_renders_multiple_tenders_with_account_override() {
        let mut r = base();
        r.raw_text = "**** 9999".to_string();
        r.tenders = vec![
            FormatterTenderInput {
                amount: "15.00".to_string(),
                account: None,
                kind: "card".to_string(),
            },
            FormatterTenderInput {
                amount: "5.00".to_string(),
                account: Some("Assets:GiftCards:Costco".to_string()),
                kind: "gift_card".to_string(),
            },
        ];
        let out = format_parsed_receipt(&r, CC, None);
        // card tender -> PENDING CC with card comment
        assert!(out.contains(CC));
        assert!(out.contains("-15.00 CAD"));
        assert!(out.contains("; card ****9999"));
        // gift-card tender -> overridden asset account with "gift card" comment
        assert!(out.contains("Assets:GiftCards:Costco"));
        assert!(out.contains("-5.00 CAD"));
        assert!(out.contains("; gift card"));
    }

    /// Tenders that don't account for the total are not posted as a split.
    ///
    /// This is the LCBO shape: two gift cards, the second OCR'd a dollar low
    /// (66.60 -> 65.60). Posting them would put -95.65 on the payment side
    /// against a 96.65 item side and beancount would reject the entry, so the
    /// formatter falls back to the one posting it knows is right — the total
    /// was charged somehow. The receipt is not silently "fine": the parser has
    /// already raised `TenderMismatch` on the same arithmetic.
    #[test]
    fn parsed_receipt_falls_back_when_tenders_do_not_account_for_the_total() {
        let mut r = base();
        r.total = "96.65".to_string();
        r.tenders = vec![
            FormatterTenderInput {
                amount: "30.05".to_string(),
                account: None,
                kind: "gift_card".to_string(),
            },
            FormatterTenderInput {
                amount: "65.60".to_string(),
                account: None,
                kind: "gift_card".to_string(),
            },
        ];
        let out = format_parsed_receipt(&r, CC, None);
        assert!(out.contains(&format!("{CC}  -96.65 CAD")) || out.contains("-96.65 CAD"));
        assert!(!out.contains("-65.60 CAD"));
        assert!(!out.contains("-30.05 CAD"));
    }

    /// One cent short is still short — the old $0.05 tolerance let this through
    /// and emitted an entry that could not balance.
    #[test]
    fn parsed_receipt_falls_back_on_a_one_cent_tender_gap() {
        let mut r = base();
        r.tenders = vec![FormatterTenderInput {
            amount: "19.99".to_string(),
            account: None,
            kind: "cash".to_string(),
        }];
        let out = format_parsed_receipt(&r, CC, None);
        assert!(out.contains("-20.00 CAD"));
        assert!(!out.contains("Assets:Cash:PENDING"));
    }

    /// A gift-card tender with no assigned account falls back to its PENDING slot.
    #[test]
    fn parsed_receipt_uses_pending_account_for_unassigned_gift_card() {
        let mut r = base();
        r.tenders = vec![FormatterTenderInput {
            amount: "20.00".to_string(),
            account: None,
            kind: "gift_card".to_string(),
        }];
        let out = format_parsed_receipt(&r, CC, None);
        assert!(out.contains("Assets:GiftCards:PENDING"));
    }

    /// `format_draft_beancount`: draft header, source line, FIXME narration.
    #[test]
    fn draft_beancount_uses_review_header_and_fixme_narration() {
        let mut r = base();
        r.items = vec![item(
            "COKE ZERO",
            "17.19",
            1,
            "Expenses:Food:Grocery:Drink:CocaCola",
        )];
        let out = format_draft_beancount(&r, CC);
        assert!(out.contains("; === DRAFT - REVIEW NEEDED ==="));
        assert!(out.contains("; Source: costco.jpg"));
        assert!(out.contains(r#"2026-02-18 * "COSTCO" "FIXME: add description""#));
    }

    /// `generate_filename`: slugifies the merchant, handles placeholders and
    /// all-punctuation merchants.
    #[test]
    fn generate_filename_slugifies_and_handles_edge_cases() {
        assert_eq!(
            generate_filename("2026-02-18", false, "No Frills!"),
            "2026-02-18-no-frills.beancount"
        );
        assert_eq!(
            generate_filename("2026-02-18", true, "COSTCO"),
            "unknown-date-costco.beancount"
        );
        assert_eq!(
            generate_filename("2026-01-01", false, "!!!"),
            "2026-01-01-unknown.beancount"
        );
    }

    /// `format_enriched_transaction`: reuses the matched CC posting/expense,
    /// itemizes the receipt, and appends the original transaction for reference.
    #[test]
    fn enriched_transaction_reuses_match_and_itemizes() {
        let mut r = base();
        r.tax = Some("1.00".to_string());
        r.items = vec![item(
            "COKE ZERO",
            "17.19",
            1,
            "Expenses:Food:Grocery:Drink:CocaCola",
        )];

        let match_input = EnrichedMatchInput {
            transaction_date_iso: "2026-02-20".to_string(),
            payee: "COSTCO WHOLESALE".to_string(),
            narration: "Purchase".to_string(),
            postings: vec![
                EnrichedPostingInput {
                    account: "Liabilities:CreditCard:Visa".to_string(),
                    number: Some("-20.00".to_string()),
                    currency: Some("CAD".to_string()),
                },
                EnrichedPostingInput {
                    account: "Expenses:Uncategorized".to_string(),
                    number: Some("20.00".to_string()),
                    currency: Some("CAD".to_string()),
                },
            ],
            file_path: "ledger.beancount".to_string(),
            line_number: 42,
            confidence: 0.9,
            match_details: "amount+date".to_string(),
        };

        let out = format_enriched_transaction(
            &r,
            &match_input,
            "Expenses:FIXME",
            Some("bb-20260218-a1b2c3d4"),
            Some("beanbeaver/2026-02-18-costco-a1b2c3d4.jpg"),
        );

        assert!(out.contains("; === ENRICHED TRANSACTION - REVIEW NEEDED ==="));
        assert!(out.contains("; Receipt: costco.jpg"));
        assert!(out.contains("; Matched: ledger.beancount:42"));
        assert!(out.contains("; Confidence: 90% (amount+date)"));
        assert!(out.contains(r#"2026-02-20 * "COSTCO WHOLESALE" "Purchase""#));
        // receipt identity carried forward onto the merged transaction
        assert!(out.contains(r#"  beanbeaver-id: "bb-20260218-a1b2c3d4""#));
        assert!(out.contains(r#"  document: "beanbeaver/2026-02-18-costco-a1b2c3d4.jpg""#));
        // matched CC posting reused (negative number -> credit posting)
        assert!(out.contains("Liabilities:CreditCard:Visa"));
        assert!(out.contains("-20.00 CAD"));
        // items rendered; remainder balances against the matched expense account
        assert!(out.contains("Expenses:Food:Grocery:Drink:CocaCola"));
        assert!(out.contains("; remaining/unitemized"));
        assert!(out.contains("; --- Original Transaction (to be replaced) ---"));
    }

    /// Identity args are optional: omit them and no metadata line appears.
    #[test]
    fn enriched_transaction_omits_identity_when_absent() {
        let match_input = EnrichedMatchInput {
            transaction_date_iso: "2026-02-20".to_string(),
            payee: "COSTCO".to_string(),
            narration: String::new(),
            postings: vec![EnrichedPostingInput {
                account: "Liabilities:CreditCard:Visa".to_string(),
                number: Some("-20.00".to_string()),
                currency: Some("CAD".to_string()),
            }],
            file_path: "ledger.beancount".to_string(),
            line_number: 1,
            confidence: 0.9,
            match_details: "amount".to_string(),
        };
        let out = format_enriched_transaction(&base(), &match_input, "Expenses:FIXME", None, None);
        assert!(!out.contains("beanbeaver-id"));
        assert!(!out.contains("document:"));
    }

    /// The ledger text carries findings about the *numbers* and drops the rest.
    /// The parser reports both; choosing between them is this layer's job, and
    /// an uncategorized line already announces itself as `Expenses:FIXME` one
    /// line above where the comment would go.
    #[test]
    fn only_numeric_findings_reach_the_ledger_text() {
        let mut receipt = base();
        receipt.items = vec![item("MILK", "4.00", 1, "Expenses:Food:Grocery:Dairy")];
        receipt.warnings = vec![
            FormatterWarningInput {
                kind: ReceiptWarningKind::UncategorizedItem,
                message: "no classifier rule matched \"ZZQW\"".to_string(),
                after_item_index: Some(0),
            },
            FormatterWarningInput {
                kind: ReceiptWarningKind::TotalMismatch,
                message: "items total 24.00 but the receipt total is 20.00".to_string(),
                after_item_index: None,
            },
        ];
        let out = format_parsed_receipt(&receipt, CC, None);
        assert!(
            out.contains("; WARN:PARSER items total 24.00"),
            "the balance finding belongs in the ledger:\n{out}"
        );
        assert!(
            !out.contains("no classifier rule matched"),
            "an uncategorized line should not comment the ledger:\n{out}"
        );
    }
}
