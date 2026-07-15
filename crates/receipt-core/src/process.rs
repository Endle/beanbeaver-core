//! Single high-level entry point for the on-device pipeline:
//! raw OCR detections -> structured receipt + beancount text, with no Python.
//!
//! Chains `ocr_transform::transform` -> `receipt_parser::parse_receipt` ->
//! `receipt_formatter::format_parsed_receipt`, loading rules from the bundled
//! defaults. This mirrors the desktop flow
//! (`ocr_helpers.transform_paddleocr_result` + `ocr_result_parser.parse_receipt`
//! + `formatter.format_parsed_receipt`).

use crate::merchant_match::{MerchantFamily, MerchantMatchStatus};
use crate::ocr_transform::{transform, RawDetection};
use crate::receipt_categories::resolve_account_target;
use crate::receipt_formatter::{
    format_parsed_receipt, FormatterItemInput, FormatterReceiptInput, FormatterTenderInput,
    FormatterWarningInput,
};
use crate::receipt_parser::{
    parse_receipt, ParsedReceiptData, ParsedReceiptItem, ParserRuleLayers,
};
use crate::rules::{
    default_known_merchants, default_merchant_families, default_parser_rule_layers,
    parser_rule_layers_with_overrides,
};

const DEFAULT_ITEM_ACCOUNT: &str = "Expenses:FIXME";

/// Optional knobs for [`process_receipt_with_options`] (and the FFI overlay path).
///
/// Defaults match the historical [`process_receipt`] behavior: bundled public
/// rules, bundled merchant families/keywords, no classifier overrides.
#[derive(Clone, Debug, Default)]
pub struct ProcessOptions {
    /// When `None`, use [`default_known_merchants`].
    pub known_merchants: Option<Vec<String>>,
    /// When `None`, use [`default_merchant_families`].
    pub merchant_families: Option<Vec<MerchantFamily>>,
    /// Extra item-classifier TOML layers (later wins). Empty = public defaults only.
    pub item_classifier_override_tomls: Vec<String>,
}

/// Per-field trust signals for "needs review" UI. Values in `[0, 1]` unless noted.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldConfidence {
    /// From [`crate::merchant_match::MerchantMatch::score`], adjusted by status.
    pub merchant: f64,
    /// `1.0` when a non-placeholder date was extracted; `0.0` when placeholder/missing.
    pub date: f64,
    /// High when a total string parses as money and items/tax roughly reconcile.
    pub total: f64,
    /// Share of items that have a non-empty category key (or 1.0 if no items).
    pub items_categorized: f64,
    /// True when the UI should prompt the user (low merchant/date or warnings).
    pub needs_review: bool,
}

/// Result of the full pipeline: the structured parse plus the rendered beancount.
#[derive(Clone, Debug)]
pub struct ProcessedReceipt {
    pub parsed: ParsedReceiptData,
    pub beancount: String,
    /// Greppable identity stamped into `beancount` (`bb-<yyyymmdd>-<sha8>`), and
    /// the receipt image's path relative to the documents root
    /// (`beanbeaver/<name>.jpg`) written into the `document:` metadata. Both are
    /// `None` when no image hash was supplied. Surfaced here so a caller (e.g.
    /// the iOS app saving the JPEG) uses the *same* values embedded in the text.
    pub beanbeaver_id: Option<String>,
    pub document_relpath: Option<String>,
    /// Heuristic field confidences for review UX.
    pub confidence: FieldConfidence,
}

/// User corrections applied when regenerating beancount without re-running OCR.
#[derive(Clone, Debug, Default)]
pub struct ReceiptCorrections {
    pub merchant: Option<String>,
    /// ISO `YYYY-MM-DD`. When set, clears the placeholder flag.
    pub date_iso: Option<String>,
    /// Parallel to items: `Some(account)` overrides that item's posting account.
    /// Length may be shorter than items (trailing items keep auto accounts).
    pub item_accounts: Vec<Option<String>>,
}

/// Round a decimal string to 2 places using banker's rounding (round-half-even),
/// matching Python's `Decimal.__format__(".2f")` that the formatter glue applies
/// to item prices, total, and tax. Inputs are well-formed fixed-point strings
/// (e.g. "12.34" from cents, "1.2345" from the scaled spatial path).
fn to_fixed_2(value: &str) -> String {
    let negative = value.starts_with('-');
    let digits = value.trim_start_matches('-');
    let (int_part, frac_part) = match digits.split_once('.') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };

    // Build an integer at the source scale, then round to scale 2.
    let int_value: i128 = int_part.parse().unwrap_or(0);
    let scale = frac_part.len();
    let frac_value: i128 = if frac_part.is_empty() {
        0
    } else {
        frac_part.parse().unwrap_or(0)
    };
    let scale_pow = 10_i128.pow(scale as u32);
    let total = int_value * scale_pow + frac_value;

    let rounded_hundredths: i128 = if scale <= 2 {
        total * 10_i128.pow((2 - scale) as u32)
    } else {
        let divisor = 10_i128.pow((scale - 2) as u32);
        let q = total / divisor;
        let r = total % divisor;
        let half = divisor / 2;
        if r > half || (r == half && q % 2 != 0) {
            q + 1
        } else {
            q
        }
    };

    let sign = if negative && rounded_hundredths != 0 {
        "-"
    } else {
        ""
    };
    format!(
        "{sign}{}.{:02}",
        rounded_hundredths / 100,
        rounded_hundredths % 100
    )
}

fn date_iso(parsed: &ParsedReceiptData, today: (i32, u32, u32)) -> String {
    match parsed.date {
        Some((y, m, d)) => format!("{y:04}-{m:02}-{d:02}"),
        // Placeholder mirrors `date_utils.placeholder_receipt_date()`:
        // first day of the current (reference) month.
        None => format!("{:04}-{:02}-01", today.0, today.1),
    }
}

fn parse_money_cents(value: &str) -> Option<i64> {
    let fixed = to_fixed_2(value);
    let neg = fixed.starts_with('-');
    let digits = fixed.trim_start_matches('-').replace('.', "");
    let cents: i64 = digits.parse().ok()?;
    Some(if neg { -cents } else { cents })
}

/// Compute review-oriented confidences from a parsed receipt.
pub fn field_confidence(parsed: &ParsedReceiptData) -> FieldConfidence {
    let merchant = match parsed.merchant_match.status {
        MerchantMatchStatus::Exact => 1.0,
        MerchantMatchStatus::Corrected => parsed.merchant_match.score.max(0.85),
        MerchantMatchStatus::Suggested => parsed.merchant_match.score * 0.7,
        MerchantMatchStatus::Unknown => {
            if parsed.merchant_match.raw.trim().is_empty() {
                0.0
            } else {
                0.35
            }
        }
    };

    let date = if parsed.date.is_some() && !parsed.date_is_placeholder {
        1.0
    } else {
        0.0
    };

    let total = match parse_money_cents(&parsed.total) {
        Some(total_cents) if total_cents != 0 || !parsed.items.is_empty() => {
            let items_sum: i64 = parsed
                .items
                .iter()
                .filter_map(|it| parse_money_cents(&it.price))
                .sum();
            let tax_cents = parsed
                .tax
                .as_deref()
                .and_then(parse_money_cents)
                .unwrap_or(0);
            let combined = items_sum + tax_cents;
            if total_cents == 0 {
                0.5
            } else {
                let drift = (combined - total_cents).unsigned_abs() as f64;
                let denom = total_cents.unsigned_abs().max(1) as f64;
                // Within 2% → ~1.0; larger drift drops toward 0.4.
                (1.0 - (drift / denom).min(1.0) * 0.6).clamp(0.4, 1.0)
            }
        }
        _ => 0.2,
    };

    let items_categorized = if parsed.items.is_empty() {
        1.0
    } else {
        let n = parsed.items.len() as f64;
        let cat = parsed
            .items
            .iter()
            .filter(|it| it.category.as_ref().is_some_and(|c| !c.is_empty()))
            .count() as f64;
        cat / n
    };

    let needs_review = merchant < 0.7
        || date < 0.5
        || total < 0.55
        || !parsed.warnings.is_empty()
        || parsed.merchant_match.status == MerchantMatchStatus::Suggested
        || parsed.merchant_match.status == MerchantMatchStatus::Unknown;

    FieldConfidence {
        merchant,
        date,
        total,
        items_categorized,
        needs_review,
    }
}

fn resolve_rule_layers(options: &ProcessOptions) -> ParserRuleLayers {
    if options.item_classifier_override_tomls.is_empty() {
        default_parser_rule_layers()
    } else {
        let refs: Vec<&str> = options
            .item_classifier_override_tomls
            .iter()
            .map(String::as_str)
            .collect();
        parser_rule_layers_with_overrides(&refs)
    }
}

fn format_from_parsed(
    parsed: &ParsedReceiptData,
    rule_layers: &ParserRuleLayers,
    today: (i32, u32, u32),
    credit_card_account: &str,
    image_sha256: Option<&str>,
    corrections: Option<&ReceiptCorrections>,
) -> (String, Option<String>, Option<String>) {
    let merchant = corrections
        .and_then(|c| c.merchant.clone())
        .unwrap_or_else(|| parsed.merchant.clone());

    let (date_iso_str, date_is_placeholder) =
        if let Some(iso) = corrections.and_then(|c| c.date_iso.clone()) {
            (iso, false)
        } else {
            (date_iso(parsed, today), parsed.date_is_placeholder)
        };

    let item_accounts: Vec<String> = parsed
        .items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            if let Some(Some(account)) = corrections.and_then(|c| c.item_accounts.get(idx)) {
                return account.clone();
            }
            resolve_account_target(
                item.category.as_deref(),
                &rule_layers.category_rules.account_mapping,
                Some(DEFAULT_ITEM_ACCOUNT),
            )
            .unwrap_or_else(|| DEFAULT_ITEM_ACCOUNT.to_string())
        })
        .collect();

    let formatter_input = FormatterReceiptInput {
        merchant: merchant.clone(),
        date_iso: date_iso_str.clone(),
        date_is_placeholder,
        total: to_fixed_2(&parsed.total),
        tax: parsed.tax.as_deref().map(to_fixed_2),
        image_filename: parsed.image_filename.clone(),
        raw_text: parsed.raw_text.clone(),
        items: parsed
            .items
            .iter()
            .zip(&item_accounts)
            .map(|(item, account)| FormatterItemInput {
                description: item.description.clone(),
                price: to_fixed_2(&item.price),
                quantity: item.quantity,
                posting_account: account.clone(),
            })
            .collect(),
        warnings: parsed
            .warnings
            .iter()
            .map(|warning| FormatterWarningInput {
                message: warning.message.clone(),
                after_item_index: warning.after_item_index,
            })
            .collect(),
        tenders: parsed
            .tenders
            .iter()
            .map(|tender| FormatterTenderInput {
                amount: to_fixed_2(&tender.amount),
                account: tender.account.clone(),
                kind: tender.kind.clone(),
            })
            .collect(),
    };

    let beancount = format_parsed_receipt(&formatter_input, credit_card_account, image_sha256);
    let beanbeaver_id = crate::receipt_formatter::beanbeaver_id(
        &formatter_input.date_iso,
        formatter_input.date_is_placeholder,
        image_sha256,
    );
    let document_relpath = crate::receipt_formatter::beanbeaver_document_relpath(
        &formatter_input.date_iso,
        formatter_input.date_is_placeholder,
        &formatter_input.merchant,
        image_sha256,
    );
    (beancount, beanbeaver_id, document_relpath)
}

/// Run the full pipeline. `today` is the reference date (year, month, day) used
/// for date inference and the placeholder date. When `known_merchants` is `None`,
/// the bundled default merchant keywords are used.
#[allow(clippy::too_many_arguments)]
pub fn process_receipt(
    detections: Vec<RawDetection>,
    padded_width: i64,
    padded_height: i64,
    padding: i64,
    image_filename: &str,
    known_merchants: Option<Vec<String>>,
    today: (i32, u32, u32),
    credit_card_account: &str,
    image_sha256: Option<&str>,
) -> ProcessedReceipt {
    let mut options = ProcessOptions::default();
    options.known_merchants = known_merchants;
    process_receipt_with_options(
        detections,
        padded_width,
        padded_height,
        padding,
        image_filename,
        today,
        credit_card_account,
        image_sha256,
        &options,
    )
}

/// Like [`process_receipt`] but accepts rule / merchant overlays.
#[allow(clippy::too_many_arguments)]
pub fn process_receipt_with_options(
    detections: Vec<RawDetection>,
    padded_width: i64,
    padded_height: i64,
    padding: i64,
    image_filename: &str,
    today: (i32, u32, u32),
    credit_card_account: &str,
    image_sha256: Option<&str>,
    options: &ProcessOptions,
) -> ProcessedReceipt {
    let rule_layers = resolve_rule_layers(options);
    let merchants = options
        .known_merchants
        .clone()
        .unwrap_or_else(default_known_merchants);
    let merchant_families = options
        .merchant_families
        .clone()
        .unwrap_or_else(default_merchant_families);

    let ocr = transform(detections, padded_width, padded_height, padding);

    let parsed = parse_receipt(
        &ocr.full_text,
        &ocr.helper_pages,
        &ocr.spatial_pages,
        &rule_layers,
        image_filename,
        &merchants,
        &merchant_families,
        today.0,
    );

    let confidence = field_confidence(&parsed);
    let (beancount, beanbeaver_id, document_relpath) = format_from_parsed(
        &parsed,
        &rule_layers,
        today,
        credit_card_account,
        image_sha256,
        None,
    );

    ProcessedReceipt {
        parsed,
        beancount,
        beanbeaver_id,
        document_relpath,
        confidence,
    }
}

/// Re-render beancount from an existing parse with optional user corrections
/// (no OCR). Uses the same default rule layers as a fresh process unless
/// `options` supplies classifier overrides (for account resolution only).
pub fn reformat_parsed_receipt(
    parsed: &ParsedReceiptData,
    today: (i32, u32, u32),
    credit_card_account: &str,
    image_sha256: Option<&str>,
    corrections: &ReceiptCorrections,
    options: Option<&ProcessOptions>,
) -> ProcessedReceipt {
    let default_opts = ProcessOptions::default();
    let options = options.unwrap_or(&default_opts);
    let rule_layers = resolve_rule_layers(options);
    let confidence = field_confidence(parsed);
    let (beancount, beanbeaver_id, document_relpath) = format_from_parsed(
        parsed,
        &rule_layers,
        today,
        credit_card_account,
        image_sha256,
        Some(corrections),
    );

    // Apply corrections onto a clone for the returned structured view.
    let mut parsed_out = parsed.clone();
    if let Some(m) = &corrections.merchant {
        parsed_out.merchant = m.clone();
    }
    if let Some(iso) = &corrections.date_iso {
        if let Some((y, rest)) = iso.split_once('-') {
            if let Some((m, d)) = rest.split_once('-') {
                if let (Ok(y), Ok(m), Ok(d)) = (y.parse(), m.parse(), d.parse()) {
                    parsed_out.date = Some((y, m, d));
                    parsed_out.date_is_placeholder = false;
                }
            }
        }
    }
    for (idx, override_acct) in corrections.item_accounts.iter().enumerate() {
        if let (Some(account), Some(item)) = (override_acct.as_ref(), parsed_out.items.get_mut(idx))
        {
            // Store the beancount account in category so UI shows the override.
            item.category = Some(account.clone());
        }
    }

    ProcessedReceipt {
        parsed: parsed_out,
        beancount,
        beanbeaver_id,
        document_relpath,
        confidence,
    }
}

/// Apply a single item-account override helper for callers that only tweak one line.
pub fn override_item_account(items: &mut [ParsedReceiptItem], index: usize, account: String) {
    if let Some(item) = items.get_mut(index) {
        item.category = Some(account);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merchant_match::{MerchantMatch, MerchantMatchStatus};

    #[test]
    fn rounds_half_even_like_python_decimal() {
        assert_eq!(to_fixed_2("12.34"), "12.34");
        assert_eq!(to_fixed_2("1.2345"), "1.23"); // 4->5 at third place rounds down to even
        assert_eq!(to_fixed_2("1.2355"), "1.24"); // half rounds to even (4)
        assert_eq!(to_fixed_2("1.2350"), "1.24"); // exactly half -> even
        assert_eq!(to_fixed_2("1.2250"), "1.22"); // exactly half -> even
        assert_eq!(to_fixed_2("0.005"), "0.00"); // half -> even (0)
        assert_eq!(to_fixed_2("-5.00"), "-5.00");
        assert_eq!(to_fixed_2("3"), "3.00");
    }

    fn sample_parsed() -> ParsedReceiptData {
        ParsedReceiptData {
            merchant: "COSTCO".into(),
            merchant_match: MerchantMatch {
                raw: "COSTCO".into(),
                canonical: Some("COSTCO".into()),
                status: MerchantMatchStatus::Exact,
                score: 1.0,
            },
            date: Some((2026, 2, 18)),
            date_is_placeholder: false,
            total: "10.00".into(),
            items: vec![ParsedReceiptItem {
                description: "Milk".into(),
                price: "10.00".into(),
                quantity: 1,
                category: Some("grocery_dairy".into()),
                tags: vec!["grocery".into(), "dairy".into()],
            }],
            tax: None,
            subtotal: Some("10.00".into()),
            raw_text: "COSTCO\nMilk 10.00\nTOTAL 10.00".into(),
            image_filename: "x.jpg".into(),
            warnings: vec![],
            tenders: vec![],
        }
    }

    #[test]
    fn confidence_high_for_clean_exact_receipt() {
        let c = field_confidence(&sample_parsed());
        assert!((c.merchant - 1.0).abs() < 1e-9);
        assert!((c.date - 1.0).abs() < 1e-9);
        assert!(c.total >= 0.9);
        assert!((c.items_categorized - 1.0).abs() < 1e-9);
        assert!(!c.needs_review);
    }

    #[test]
    fn reformat_applies_merchant_and_date_overrides() {
        let parsed = sample_parsed();
        let corrections = ReceiptCorrections {
            merchant: Some("Costco Wholesale".into()),
            date_iso: Some("2026-03-01".into()),
            item_accounts: vec![Some("Expenses:Food:Grocery:Dairy".into())],
        };
        let out = reformat_parsed_receipt(
            &parsed,
            (2026, 7, 1),
            "Liabilities:CreditCard",
            Some("abcd"),
            &corrections,
            None,
        );
        assert_eq!(out.parsed.merchant, "Costco Wholesale");
        assert_eq!(out.parsed.date, Some((2026, 3, 1)));
        assert!(!out.parsed.date_is_placeholder);
        assert!(out.beancount.contains("Costco Wholesale"));
        assert!(out.beancount.contains("2026-03-01"));
        assert!(out.beancount.contains("Expenses:Food:Grocery:Dairy"));
    }
}
