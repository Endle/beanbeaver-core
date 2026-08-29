//! Single high-level entry point for the on-device pipeline:
//! raw OCR detections -> structured receipt + beancount text, with no Python.
//!
//! Chains `ocr_transform::transform` -> `parser::parse_receipt` ->
//! `formatter::format_parsed_receipt`, loading rules from the bundled
//! defaults. This mirrors the desktop flow
//! (`ocr_helpers.transform_paddleocr_result` + `ocr_result_parser.parse_receipt`
//! + `formatter.format_parsed_receipt`).

use crate::date::Date;
use crate::money::Money;
use std::borrow::Cow;

use crate::common::ReceiptWarningKind;
use crate::formatter::{
    format_parsed_receipt, FormatterItemInput, FormatterReceiptInput, FormatterTenderInput,
    FormatterWarningInput,
};
use crate::merchant_match::{MerchantFamily, MerchantMatch, MerchantMatchStatus};
use crate::ocr_transform::{transform, RawDetection};
use crate::parser::{parse_receipt, ParsedReceiptData, ParsedReceiptItem, ParserRuleLayers};
use crate::rules::RuleBook;

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
    /// The raw OCR detections this parse was built from (padded-image pixel
    /// coordinates, pre-transform), surfaced for debugging/E2E snapshot diffing.
    /// Empty on the reformat path (no OCR was run).
    pub detections: Vec<RawDetection>,
    /// The tag vocabulary in force for this parse, including anything an
    /// override document added. Carried on the result so a consumer can label
    /// an item's tags without rebuilding the rule book — and so the labels
    /// always match the rules that actually ran.
    pub tag_vocabulary: Vec<crate::categories::TagNode>,
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
fn date_iso(parsed: &ParsedReceiptData, today: Date) -> String {
    match parsed.date {
        Some(d) => d.to_string(),
        // Placeholder mirrors `date_utils.placeholder_receipt_date()`:
        // first day of the current (reference) month.
        None => format!("{:04}-{:02}-01", today.year(), today.month()),
    }
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

    let total = match Some(parsed.total.cents()) {
        Some(total_cents) if total_cents != 0 || !parsed.items.is_empty() => {
            let items_sum: i64 = parsed.items.iter().map(|it| it.price.cents()).sum();
            let tax_cents = parsed.tax.map(Money::cents).unwrap_or(0);
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

    // Deliberately counts only findings about the receipt's *numbers*: this
    // legacy roll-up predates warning kinds, and folding
    // `UncategorizedItem` in would flip it true for most receipts overnight —
    // an unclassified line is normal, not a reason to re-check a parse. New
    // consumers should rank `warnings` by kind themselves rather than read this.
    let has_numeric_warning = parsed
        .warnings
        .iter()
        .any(|w| w.kind != ReceiptWarningKind::UncategorizedItem);

    let needs_review = merchant < 0.7
        || date < 0.5
        || total < 0.55
        || has_numeric_warning
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

/// The rule corpus for one parse. With no overrides this hands back the
/// process-wide cached book instead of re-parsing four TOML documents per
/// receipt, which is what the old free-function path did.
fn resolve_rule_book(options: &ProcessOptions) -> Result<Cow<'static, RuleBook>, String> {
    if options.item_classifier_override_tomls.is_empty() {
        Ok(Cow::Borrowed(RuleBook::bundled()))
    } else {
        let refs: Vec<&str> = options
            .item_classifier_override_tomls
            .iter()
            .map(String::as_str)
            .collect();
        RuleBook::with_overrides(&refs).map(Cow::Owned)
    }
}

/// Parse and calendar-validate an ISO `YYYY-MM-DD` date string.
fn parse_iso_ymd(iso: &str) -> Result<Date, String> {
    let iso = iso.trim();
    let mut parts = iso.split('-');
    let (Some(ys), Some(ms), Some(ds), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("date_iso must be YYYY-MM-DD (got {iso:?})"));
    };
    if ys.len() != 4 || ms.len() != 2 || ds.len() != 2 {
        return Err(format!(
            "date_iso must be YYYY-MM-DD with zero-padded month/day (got {iso:?})"
        ));
    }
    let y: i32 = ys
        .parse()
        .map_err(|_| format!("date_iso year is not an integer (got {iso:?})"))?;
    let m: u32 = ms
        .parse()
        .map_err(|_| format!("date_iso month is not an integer (got {iso:?})"))?;
    let d: u32 = ds
        .parse()
        .map_err(|_| format!("date_iso day is not an integer (got {iso:?})"))?;
    if !(1..=12).contains(&m) {
        return Err(format!("date_iso month out of range (got {iso:?})"));
    }
    // `Date::new` owns the calendar rules now, including the 1990..=2100 band
    // this function did not previously apply. That band is the parser's own
    // (a year outside it on a receipt is a product code, not a date), so a
    // correction is now held to the same standard as a parse -- an explicit
    // error rather than a silently accepted impossible date.
    Date::new(y, m, d).ok_or_else(|| format!("date_iso is not a real date (got {iso:?})"))
}

#[allow(clippy::too_many_arguments)]
fn format_from_parsed(
    parsed: &ParsedReceiptData,
    _rule_layers: &ParserRuleLayers,
    today: Date,
    credit_card_account: &str,
    currency: &str,
    tax_account: &str,
    image_sha256: Option<&str>,
    corrections: Option<&ReceiptCorrections>,
) -> Result<(String, Option<String>, Option<String>), String> {
    let merchant = corrections
        .and_then(|c| c.merchant.clone())
        .unwrap_or_else(|| parsed.merchant.clone());

    let (date_iso_str, date_is_placeholder) =
        if let Some(iso) = corrections.and_then(|c| c.date_iso.as_ref()) {
            // Validate before emitting into beancount so structured + text stay aligned.
            let _ = parse_iso_ymd(iso)?;
            (iso.clone(), false)
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
            item.account
                .clone()
                .unwrap_or_else(|| DEFAULT_ITEM_ACCOUNT.to_string())
        })
        .collect();

    let formatter_input = FormatterReceiptInput {
        merchant: merchant.clone(),
        date_iso: date_iso_str.clone(),
        date_is_placeholder,
        total: parsed.total,
        tax: parsed.tax,
        image_filename: parsed.image_filename.clone(),
        raw_text: parsed.raw_text.clone(),
        items: parsed
            .items
            .iter()
            .zip(&item_accounts)
            .map(|(item, account)| FormatterItemInput {
                description: item.description.clone(),
                price: item.price,
                quantity: item.quantity,
                posting_account: account.clone(),
            })
            .collect(),
        currency: currency.to_string(),
        tax_account: tax_account.to_string(),
        warnings: parsed
            .warnings
            .iter()
            .map(|warning| FormatterWarningInput {
                kind: warning.kind,
                message: warning.message.clone(),
                after_item_index: warning.after_item_index,
            })
            .collect(),
        tenders: parsed
            .tenders
            .iter()
            .map(|tender| FormatterTenderInput {
                amount: tender.amount,
                account: tender.account.clone(),
                kind: tender.kind.clone(),
            })
            .collect(),
    };

    let beancount = format_parsed_receipt(&formatter_input, credit_card_account, image_sha256);
    let beanbeaver_id = crate::formatter::beanbeaver_id(
        &formatter_input.date_iso,
        formatter_input.date_is_placeholder,
        image_sha256,
    );
    let document_relpath = crate::formatter::beanbeaver_document_relpath(
        &formatter_input.date_iso,
        formatter_input.date_is_placeholder,
        &formatter_input.merchant,
        image_sha256,
    );
    Ok((beancount, beanbeaver_id, document_relpath))
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
    today: Date,
    credit_card_account: &str,
    currency: &str,
    tax_account: &str,
    image_sha256: Option<&str>,
) -> ProcessedReceipt {
    let options = ProcessOptions {
        known_merchants,
        ..Default::default()
    };
    process_receipt_with_options(
        detections,
        padded_width,
        padded_height,
        padding,
        image_filename,
        today,
        credit_card_account,
        currency,
        tax_account,
        image_sha256,
        &options,
    )
    .expect("default ProcessOptions never fail rule loading")
}

/// Like [`process_receipt`] but accepts rule / merchant overlays.
///
/// Returns `Err` when classifier override TOML is invalid (or date corrections
/// are invalid on the reformat path — see [`reformat_parsed_receipt`]).
#[allow(clippy::too_many_arguments)]
pub fn process_receipt_with_options(
    detections: Vec<RawDetection>,
    padded_width: i64,
    padded_height: i64,
    padding: i64,
    image_filename: &str,
    today: Date,
    credit_card_account: &str,
    currency: &str,
    tax_account: &str,
    image_sha256: Option<&str>,
    options: &ProcessOptions,
) -> Result<ProcessedReceipt, String> {
    let rule_book = resolve_rule_book(options)?;
    let rule_layers = rule_book.layers();
    let merchants = options
        .known_merchants
        .clone()
        .unwrap_or_else(|| rule_book.known_merchants().to_vec());
    let merchant_families = options
        .merchant_families
        .clone()
        .unwrap_or_else(|| rule_book.merchant_families().to_vec());

    // Keep a copy of the raw detections for debugging/E2E diffing before
    // `transform` consumes them.
    let detections_out = detections.clone();
    let ocr = transform(detections, padded_width, padded_height, padding);

    let parsed = parse_receipt(
        &ocr,
        rule_layers,
        image_filename,
        &merchants,
        &merchant_families,
        today.year(),
    );

    let confidence = field_confidence(&parsed);
    let (beancount, beanbeaver_id, document_relpath) = format_from_parsed(
        &parsed,
        rule_layers,
        today,
        credit_card_account,
        currency,
        tax_account,
        image_sha256,
        None,
    )?;

    Ok(ProcessedReceipt {
        tag_vocabulary: rule_book.layers().category_rules.tag_vocabulary.clone(),
        parsed,
        beancount,
        beanbeaver_id,
        document_relpath,
        confidence,
        detections: detections_out,
    })
}

/// Re-render beancount from an existing parse with optional user corrections
/// (no OCR). Uses the same default rule layers as a fresh process unless
/// `options` supplies classifier overrides (for account resolution only).
///
/// Returns `Err` on invalid override TOML or invalid `corrections.date_iso`.
#[allow(clippy::too_many_arguments)]
pub fn reformat_parsed_receipt(
    parsed: &ParsedReceiptData,
    today: Date,
    credit_card_account: &str,
    currency: &str,
    tax_account: &str,
    image_sha256: Option<&str>,
    corrections: &ReceiptCorrections,
    options: Option<&ProcessOptions>,
) -> Result<ProcessedReceipt, String> {
    let default_opts = ProcessOptions::default();
    let options = options.unwrap_or(&default_opts);
    let rule_book = resolve_rule_book(options)?;
    let rule_layers = rule_book.layers();

    // Apply corrections first so confidence reflects the edited view.
    let mut parsed_out = parsed.clone();
    if let Some(m) = &corrections.merchant {
        parsed_out.merchant = m.clone();
        // User-supplied merchant is high-trust for review UX.
        parsed_out.merchant_match = MerchantMatch {
            raw: m.clone(),
            canonical: Some(m.clone()),
            status: MerchantMatchStatus::Corrected,
            score: 1.0,
        };
    }
    if let Some(iso) = &corrections.date_iso {
        parsed_out.date = Some(parse_iso_ymd(iso)?);
        parsed_out.date_is_placeholder = false;
    }
    // Item account overrides are applied only when formatting beancount
    // (`format_from_parsed` reads `corrections.item_accounts`). Keep
    // `item.category` as the classifier key so UI mapping stays intact.

    let confidence = field_confidence(&parsed_out);
    let (beancount, beanbeaver_id, document_relpath) = format_from_parsed(
        &parsed_out,
        rule_layers,
        today,
        credit_card_account,
        currency,
        tax_account,
        image_sha256,
        Some(corrections),
    )?;

    Ok(ProcessedReceipt {
        tag_vocabulary: rule_book.layers().category_rules.tag_vocabulary.clone(),
        parsed: parsed_out,
        beancount,
        beanbeaver_id,
        document_relpath,
        confidence,
        detections: Vec::new(),
    })
}

/// Apply a single item-account override for callers that only tweak one line.
///
/// Note: this writes the beancount account into `category` for legacy callers.
/// Prefer [`ReceiptCorrections::item_accounts`] + [`reformat_parsed_receipt`]
/// which keep the classifier key separate from the posting account.
pub fn override_item_account(items: &mut [ParsedReceiptItem], index: usize, account: String) {
    if let Some(item) = items.get_mut(index) {
        item.category = Some(account);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merchant_match::{MerchantMatch, MerchantMatchStatus};

    fn sample_parsed() -> ParsedReceiptData {
        ParsedReceiptData {
            merchant: "COSTCO".into(),
            merchant_match: MerchantMatch {
                raw: "COSTCO".into(),
                canonical: Some("COSTCO".into()),
                status: MerchantMatchStatus::Exact,
                score: 1.0,
            },
            merchant_details: Default::default(),
            date: Date::new(2026, 2, 18),
            date_is_placeholder: false,
            total: "10.00".into(),
            items: vec![ParsedReceiptItem {
                description: "Milk".into(),
                price: "10.00".into(),
                quantity: 1,
                category: Some("grocery/dairy".into()),
                account: Some("Expenses:Food:Grocery:Dairy".into()),
                tags: vec!["grocery".into(), "grocery/dairy".into()],
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
            Date::new(2026, 7, 1).unwrap(),
            "Liabilities:CreditCard",
            "CAD",
            "Expenses:Tax:HST",
            Some("abcd"),
            &corrections,
            None,
        )
        .expect("reformat");
        assert_eq!(out.parsed.merchant, "Costco Wholesale");
        assert_eq!(out.parsed.date, Date::new(2026, 3, 1));
        assert!(!out.parsed.date_is_placeholder);
        assert!(out.beancount.contains("Costco Wholesale"));
        assert!(out.beancount.contains("2026-03-01"));
        assert!(out.beancount.contains("Expenses:Food:Grocery:Dairy"));
        // Classifier key preserved; account override only affects beancount.
        assert_eq!(
            out.parsed.items[0].category.as_deref(),
            Some("grocery/dairy")
        );
        // User merchant edit is high-trust.
        assert_eq!(
            out.parsed.merchant_match.status,
            MerchantMatchStatus::Corrected
        );
        assert!(!out.confidence.needs_review);
    }

    #[test]
    fn reformat_rejects_invalid_date_iso() {
        let parsed = sample_parsed();
        let corrections = ReceiptCorrections {
            merchant: None,
            date_iso: Some("not-a-date".into()),
            item_accounts: vec![],
        };
        let err = reformat_parsed_receipt(
            &parsed,
            Date::new(2026, 7, 1).unwrap(),
            "Liabilities:CreditCard",
            "CAD",
            "Expenses:Tax:HST",
            None,
            &corrections,
            None,
        )
        .unwrap_err();
        assert!(err.contains("date_iso"), "{err}");
    }

    #[test]
    fn reformat_preserves_tenders_and_raw_text() {
        let mut parsed = sample_parsed();
        parsed.raw_text = "COSTCO\n**** 1234\nTOTAL 10.00".into();
        parsed.tenders = vec![crate::parser::ParsedReceiptTender {
            amount: "10.00".into(),
            account: None,
            kind: "card".into(),
            raw_label: "MASTERCARD".into(),
        }];
        let corrections = ReceiptCorrections::default();
        let out = reformat_parsed_receipt(
            &parsed,
            Date::new(2026, 7, 1).unwrap(),
            "Liabilities:CreditCard",
            "CAD",
            "Expenses:Tax:HST",
            Some("abcd"),
            &corrections,
            None,
        )
        .expect("reformat");
        assert_eq!(out.parsed.raw_text, parsed.raw_text);
        assert_eq!(out.parsed.tenders.len(), 1);
        assert!(
            out.beancount.contains("****1234") || out.beancount.contains("1234"),
            "card last4 from raw_text should survive reformat: {}",
            out.beancount
        );
    }

    #[test]
    fn invalid_override_toml_returns_err() {
        let opts = ProcessOptions {
            item_classifier_override_tomls: vec!["this is not toml {{{".into()],
            ..Default::default()
        };
        let err = process_receipt_with_options(
            vec![],
            100,
            100,
            0,
            "x.jpg",
            Date::new(2026, 1, 1).unwrap(),
            "Liabilities:CreditCard",
            "CAD",
            "Expenses:Tax:HST",
            None,
            &opts,
        )
        .unwrap_err();
        assert!(
            err.contains("TOML") || err.contains("toml") || err.contains("invalid"),
            "{err}"
        );
    }
}
