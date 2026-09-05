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
use std::collections::HashMap;

use crate::common::ReceiptWarningKind;
use crate::formatter::{
    format_parsed_receipt, FormatterItemInput, FormatterReceiptInput, FormatterTenderInput,
    FormatterWarningInput,
};
use crate::merchant_match::{MerchantFamily, MerchantMatch, MerchantMatchStatus};
use crate::ocr_transform::{transform, RawDetection, RawDetectionPage};
use crate::parser::{
    balance_warnings, classified_item, item_with_tag_path, parse_receipt, uncategorized_warnings,
    ParsedReceiptData, ParsedReceiptItem, ParsedReceiptWarning, ParserRuleLayers,
};
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
    /// When `Some`, the item block the user wants recorded — replacing the
    /// parse's own. `None` leaves the items exactly as parsed.
    ///
    /// A whole-list replacement rather than positional patches, because the
    /// edits that matter most change the list's *shape*: adding the line a
    /// dropped price belongs to, deleting a banner row the parser mistook for
    /// an item. Positional patches against a shifting list is index arithmetic
    /// at the FFI boundary, and it is the kind of arithmetic that silently
    /// applies an edit to the wrong row.
    pub items: Option<Vec<ItemCorrection>>,
    /// Summary amounts, as decimal strings. `None` keeps what was parsed.
    ///
    /// Editable because OCR misreads them like anything else, and because they
    /// are the yardstick the item block is measured against: a receipt whose
    /// printed total was misread can never be made to balance by fixing items.
    pub total: Option<String>,
    pub tax: Option<String>,
    pub subtotal: Option<String>,
}

/// One line of the item block as the user wants it recorded.
#[derive(Clone, Debug)]
pub struct ItemCorrection {
    pub description: String,
    /// Decimal string, matching how prices cross the FFI.
    pub price: String,
    pub quantity: i32,
    /// The tag path the user picked (`grocery/snacks`), or empty to classify
    /// `description` with the rules in force.
    ///
    /// Empty is the common case and the right default: a renamed line should be
    /// re-classified from its new text. A non-empty path is the user overruling
    /// the classifier for this line, which is the only way a retag can show up
    /// on the item's chips as well as in the ledger.
    pub tag_path: String,
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
            .filter(|it| it.tag_path.as_ref().is_some_and(|c| !c.is_empty()))
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

    // Every item carries its own account by the time it reaches here — the
    // classifier resolved it at parse time, and `reformat_parsed_receipt`
    // re-resolves it for any line the user edited. This used to also consult a
    // positional override list on `corrections`, which changed the beancount
    // posting while leaving `item.tag_path` and the item's tags saying something
    // else; the two could not be told apart by any consumer.
    let item_accounts: Vec<String> = parsed
        .items
        .iter()
        .map(|item| {
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
    page: RawDetectionPage,
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
        page,
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
    page: RawDetectionPage,
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
    // `transform` consumes the page.
    let detections_out = page.detections().to_vec();
    let ocr = transform(page);

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

/// The findings that survive an edit, plus the ones the edit changed.
///
/// A finding is a claim about a receipt, and an edit can falsify a claim. Left
/// alone, a `TotalMismatch` outlives the mismatch it describes and the app
/// badges a receipt that now balances — which is the difference between "this
/// app tells me which line to check" and "this app is permanently unhappy".
///
/// Three groups:
///
/// - **Recomputed** from the corrected numbers: the arithmetic findings, and the
///   per-item "no rule matched", which a retag is precisely the edit that clears.
/// - **Dropped when the item block was replaced**: `PossibleMissedItem` and
///   `DroppedImplausiblePrice` are statements about the *parser's* item list. Once
///   the user has rewritten that list, they describe something that no longer
///   exists — and a user who has just added the missing line should not still be
///   told a line is missing.
/// - **Kept**: everything else, including `PriceAutoCorrected` (an audit note
///   about a repair that did happen) and `TenderMismatch`. The tender finding is
///   the one loose end: it compares the payment block against the total, so
///   editing the total can strand it, and recomputing it needs the OCR lines,
///   which this path does not have.
fn refreshed_warnings(
    parsed: &ParsedReceiptData,
    items_replaced: bool,
) -> Vec<ParsedReceiptWarning> {
    let mut warnings: Vec<ParsedReceiptWarning> = parsed
        .warnings
        .iter()
        .filter(|warning| match warning.kind {
            ReceiptWarningKind::TotalMismatch
            | ReceiptWarningKind::SubtotalMismatch
            | ReceiptWarningKind::ImplausibleSummary
            | ReceiptWarningKind::UncategorizedItem => false,
            ReceiptWarningKind::PossibleMissedItem
            | ReceiptWarningKind::DroppedImplausiblePrice => !items_replaced,
            _ => true,
        })
        .cloned()
        .collect();
    warnings.extend(balance_warnings(
        &parsed.items,
        parsed.total,
        parsed.tax,
        parsed.subtotal,
    ));
    warnings.extend(uncategorized_warnings(&parsed.items));
    warnings
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
    if let Some(items) = &corrections.items {
        // **Only text the user actually changed is re-classified.** A line they
        // left alone keeps the classification the parse gave it, because
        // re-deriving it from the description we get back is not the same
        // computation.
        //
        // `build_item` classifies the *printed* text and only falls back to the
        // merchant-vocabulary expansion, and separately appends that expansion
        // to the description for display. So "TIDE CQLDWTR" arrives back here as
        // "TIDE CQLDWTR (Cold Water)" — and classifying that matches the `WATER`
        // drink keyword and files laundry detergent as a beverage. The
        // vocabulary marks `CQLDWTR` as `classify = false` for exactly this
        // reason, and that marking is upstream of the string this path is
        // handed. Editing one line's price would otherwise re-file another.
        //
        // Matched on description rather than by index, so it survives an insert
        // or a delete anywhere in the list.
        let previous: HashMap<&str, &ParsedReceiptItem> = parsed
            .items
            .iter()
            .map(|item| (item.description.as_str(), item))
            .collect();

        let mut rebuilt = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            // Strict: this is a number the user typed. The lenient parser reads
            // "12x.99" as $0.00 and "$12.34" as $0.34, and a corrected receipt
            // that silently records the wrong amount is worse than one that
            // refuses the edit. The row index is in the message because the app
            // shows an editable list.
            let price =
                Money::parse_strict(&item.price).map_err(|e| format!("item {}: {e}", i + 1))?;
            // A line has to be worth at least one of something. Quantity is
            // display-only here (prices on a receipt are already extended), so
            // clamping cannot move an amount.
            let quantity = item.quantity.max(1);
            rebuilt.push(if !item.tag_path.is_empty() {
                item_with_tag_path(
                    item.description.clone(),
                    price,
                    quantity,
                    &item.tag_path,
                    rule_layers,
                )?
            } else if let Some(prior) = previous.get(item.description.as_str()) {
                ParsedReceiptItem {
                    description: item.description.clone(),
                    price,
                    quantity,
                    tag_path: prior.tag_path.clone(),
                    account: prior.account.clone(),
                    tags: prior.tags.clone(),
                }
            } else {
                classified_item(item.description.clone(), price, quantity, rule_layers)
            });
        }
        parsed_out.items = rebuilt;
    }
    if let Some(total) = &corrections.total {
        parsed_out.total = Money::parse_strict(total).map_err(|e| format!("total: {e}"))?;
    }
    if let Some(tax) = &corrections.tax {
        parsed_out.tax = Some(Money::parse_strict(tax).map_err(|e| format!("tax: {e}"))?);
    }
    if let Some(subtotal) = &corrections.subtotal {
        parsed_out.subtotal =
            Some(Money::parse_strict(subtotal).map_err(|e| format!("subtotal: {e}"))?);
    }

    if corrections.items.is_some()
        || corrections.total.is_some()
        || corrections.tax.is_some()
        || corrections.subtotal.is_some()
    {
        parsed_out.warnings = refreshed_warnings(&parsed_out, corrections.items.is_some());
    }

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
                tag_path: Some("grocery/dairy".into()),
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
            items: None,
            total: None,
            tax: None,
            subtotal: None,
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
        // Items untouched by a header-only correction.
        assert_eq!(
            out.parsed.items[0].tag_path.as_deref(),
            Some("grocery/dairy")
        );
        // User merchant edit is high-trust.
        assert_eq!(
            out.parsed.merchant_match.status,
            MerchantMatchStatus::Corrected
        );
        assert!(!out.confidence.needs_review);
    }

    /// No corrections at all — the shape every new field has to leave alone.
    fn no_corrections() -> ReceiptCorrections {
        ReceiptCorrections {
            merchant: None,
            date_iso: None,
            items: None,
            total: None,
            tax: None,
            subtotal: None,
        }
    }

    fn reformat(parsed: &ParsedReceiptData, corrections: &ReceiptCorrections) -> ProcessedReceipt {
        reformat_parsed_receipt(
            parsed,
            Date::new(2026, 7, 1).unwrap(),
            "Liabilities:CreditCard",
            "CAD",
            "Expenses:Tax:HST",
            None,
            corrections,
            None,
        )
        .expect("reformat")
    }

    fn item(description: &str, price: &str, tag_path: &str) -> ItemCorrection {
        ItemCorrection {
            description: description.into(),
            price: price.into(),
            quantity: 1,
            tag_path: tag_path.into(),
        }
    }

    fn has(warnings: &[ParsedReceiptWarning], kind: ReceiptWarningKind) -> bool {
        warnings.iter().any(|w| w.kind == kind)
    }

    fn try_reformat(
        parsed: &ParsedReceiptData,
        corrections: &ReceiptCorrections,
    ) -> Result<ProcessedReceipt, String> {
        reformat_parsed_receipt(
            parsed,
            Date::new(2026, 7, 1).unwrap(),
            "Liabilities:CreditCard",
            "CAD",
            "Expenses:Tax:HST",
            None,
            corrections,
            None,
        )
    }

    /// The defect `tag_path` is named for: a scanned line reported
    /// `grocery/dairy` here and a line the user re-tagged reported
    /// `Expenses:Food:Grocery:Dairy` — the same field, two kinds of string, and
    /// no way for a reader to tell which one it held.
    #[test]
    fn a_corrected_line_reports_the_same_kind_of_classification_as_a_scanned_one() {
        let parsed = sample_parsed();
        let scanned = parsed.items[0].tag_path.clone();
        assert_eq!(scanned.as_deref(), Some("grocery/dairy"));

        let corrections = ReceiptCorrections {
            items: Some(vec![item("MILK", "6.69", "grocery/dairy")]),
            ..no_corrections()
        };
        let out = try_reformat(&parsed, &corrections).expect("reformat");
        let corrected = &out.parsed.items[0];

        assert_eq!(
            corrected.tag_path, scanned,
            "a user-picked tag must land in `tag_path` as a tag path, not as the \
             account it resolves to"
        );
        // The account is still resolved and still available — it just lives in
        // the field that says "account".
        assert_eq!(
            corrected.account.as_deref(),
            Some("Expenses:Food:Grocery:Dairy")
        );
        assert!(corrected
            .tag_path
            .as_deref()
            .is_some_and(|p| !p.starts_with("Expenses:")));
    }

    /// A correction is a number a person typed, so it is refused rather than
    /// guessed at. Every input here used to reformat "successfully" into a
    /// plausible wrong amount: `12x.99` became $0.00, `$12.34` became $0.34.
    #[test]
    fn a_correction_that_is_not_a_number_is_refused_not_read_as_zero() {
        let parsed = sample_parsed();
        for bad in ["12x.99", "$12.34", "1,234.56", "", "abc", "12.345"] {
            let corrections = ReceiptCorrections {
                items: Some(vec![item("MILK", bad, "")]),
                ..no_corrections()
            };
            let err = try_reformat(&parsed, &corrections)
                .expect_err(&format!("accepted item price {bad:?}"));
            assert!(
                err.contains("item 1") && err.contains(bad),
                "message should name the row and quote the input, got {err:?}"
            );

            for (label, corrections) in [
                (
                    "total",
                    ReceiptCorrections {
                        total: Some(bad.to_string()),
                        ..no_corrections()
                    },
                ),
                (
                    "tax",
                    ReceiptCorrections {
                        tax: Some(bad.to_string()),
                        ..no_corrections()
                    },
                ),
                (
                    "subtotal",
                    ReceiptCorrections {
                        subtotal: Some(bad.to_string()),
                        ..no_corrections()
                    },
                ),
            ] {
                let err = try_reformat(&parsed, &corrections)
                    .expect_err(&format!("accepted {label} {bad:?}"));
                assert!(err.starts_with(label), "got {err:?}");
            }
        }
    }

    /// The other half of the contract: strictness must not reject the shapes the
    /// editor actually produces, including this crate's own rendering.
    #[test]
    fn ordinary_corrections_still_apply() {
        let parsed = sample_parsed();
        let corrections = ReceiptCorrections {
            items: Some(vec![item("MILK", "6.69", "")]),
            total: Some("7.56".into()),
            tax: Some("0.87".into()),
            subtotal: Some("6.69".into()),
            ..no_corrections()
        };
        let out = try_reformat(&parsed, &corrections).expect("valid corrections apply");
        assert_eq!(out.parsed.items[0].price, Money::from_cents(669));
        assert_eq!(out.parsed.total, Money::from_cents(756));
    }

    #[test]
    fn a_renamed_line_is_reclassified_from_its_new_text() {
        let parsed = sample_parsed();
        let corrections = ReceiptCorrections {
            items: Some(vec![item("BONELESS CHICKEN THIGH", "10.00", "")]),
            ..no_corrections()
        };
        let out = reformat(&parsed, &corrections);
        // The fixture line was Milk/dairy. Keeping those tags after a rename is
        // the exact failure this path exists to prevent.
        assert!(
            out.parsed.items[0].tags.iter().any(|t| t == "grocery/meat"),
            "expected meat tags, got {:?}",
            out.parsed.items[0].tags
        );
        assert!(!out.parsed.items[0]
            .tags
            .iter()
            .any(|t| t == "grocery/dairy"));
        assert!(out.beancount.contains("Expenses:Food:Grocery:Meat"));
    }

    /// The failure `build_item`'s own comment warns about, reached from the
    /// other side: the expansion it deliberately keeps out of the classifier is
    /// in the description it hands back.
    #[test]
    fn a_line_the_user_left_alone_keeps_the_classification_the_parse_gave_it() {
        let mut parsed = sample_parsed();
        parsed.items[0] = ParsedReceiptItem {
            description: "TIDE CQLDWTR (Cold Water)".into(),
            price: "10.00".into(),
            quantity: 1,
            tag_path: Some("household/supply".into()),
            account: Some("Expenses:Home:HouseholdSupply".into()),
            tags: vec!["household".into(), "household/supply".into()],
        };

        // Same text, new price — the shape of "I edited a different line".
        let corrections = ReceiptCorrections {
            items: Some(vec![item("TIDE CQLDWTR (Cold Water)", "12.00", "")]),
            ..no_corrections()
        };
        let out = reformat(&parsed, &corrections);

        assert_eq!(out.parsed.items[0].price, "12.00".into());
        assert_eq!(
            out.parsed.items[0].tags,
            vec!["household".to_string(), "household/supply".to_string()],
            "re-classifying the round-tripped description would match WATER and \
             file detergent as a beverage"
        );
        assert_eq!(
            out.parsed.items[0].account.as_deref(),
            Some("Expenses:Home:HouseholdSupply")
        );

        // And the hazard is real rather than hypothetical — this is what the
        // guard is avoiding. If the rules ever stop matching here, this line
        // fails and the guard's justification needs re-reading.
        let book = resolve_rule_book(&ProcessOptions::default()).expect("bundled rules");
        let naive = classified_item(
            "TIDE CQLDWTR (Cold Water)".into(),
            "12.00".into(),
            1,
            book.layers(),
        );
        assert!(
            naive.tags.iter().any(|t| t.starts_with("grocery/drink")),
            "expected the naive classification to reach a drink tag, got {:?}",
            naive.tags
        );
    }

    #[test]
    fn a_chosen_tag_path_overrules_the_classifier_and_reaches_the_tags() {
        let parsed = sample_parsed();
        let corrections = ReceiptCorrections {
            // Text the snacks rules do not match, filed by hand.
            items: Some(vec![item("WHITE RABBIT", "10.00", "grocery/snacks")]),
            ..no_corrections()
        };
        let out = reformat(&parsed, &corrections);
        assert_eq!(
            out.parsed.items[0].tags,
            vec!["grocery".to_string(), "grocery/snacks".to_string()]
        );
        assert_eq!(
            out.parsed.items[0].account.as_deref(),
            Some("Expenses:Food:Grocery:Snacks")
        );
        assert!(out.beancount.contains("Expenses:Food:Grocery:Snacks"));
    }

    /// 11 of the 42 bundled tags carry no account of their own; a rule stack
    /// covers that with a broader rule, and a hand-picked tag has nothing to
    /// lean on. Without the ancestor walk this files to `Expenses:FIXME`.
    #[test]
    fn a_chosen_tag_with_no_account_takes_its_nearest_mapped_ancestor() {
        let parsed = sample_parsed();
        let corrections = ReceiptCorrections {
            items: Some(vec![item("Whatever", "10.00", "grocery/meat/chicken")]),
            ..no_corrections()
        };
        let out = reformat(&parsed, &corrections);
        assert_eq!(
            out.parsed.items[0].account.as_deref(),
            Some("Expenses:Food:Grocery:Meat")
        );
        // The specific tag still shows — the walk buys an account, not a demotion.
        assert!(out.parsed.items[0]
            .tags
            .iter()
            .any(|t| t == "grocery/meat/chicken"));
    }

    #[test]
    fn an_undeclared_tag_path_is_an_error() {
        let parsed = sample_parsed();
        let corrections = ReceiptCorrections {
            items: Some(vec![item("Milk", "10.00", "grocery/nonsuch")]),
            ..no_corrections()
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
        assert!(err.contains("grocery/nonsuch"), "{err}");
    }

    /// The whole point of the edit screen: fix the receipt and it stops warning.
    #[test]
    fn adding_the_missing_line_clears_the_arithmetic_finding() {
        let mut parsed = sample_parsed();
        // One 5.00 line against a printed subtotal and total of 10.00.
        parsed.items[0].price = "5.00".into();

        let short = ReceiptCorrections {
            items: Some(vec![item("Milk", "5.00", "")]),
            ..no_corrections()
        };
        let before = reformat(&parsed, &short);
        assert!(
            has(
                &before.parsed.warnings,
                ReceiptWarningKind::SubtotalMismatch
            ),
            "fixture should start out short: {:?}",
            before.parsed.warnings
        );

        let fixed = ReceiptCorrections {
            items: Some(vec![item("Milk", "5.00", ""), item("Bread", "5.00", "")]),
            ..no_corrections()
        };
        let after = reformat(&parsed, &fixed);
        assert!(
            !has(&after.parsed.warnings, ReceiptWarningKind::SubtotalMismatch),
            "a receipt the user made add up should stop saying it does not: {:?}",
            after.parsed.warnings
        );
    }

    #[test]
    fn replacing_the_item_block_drops_findings_about_the_old_one() {
        let mut parsed = sample_parsed();
        parsed.warnings.push(ParsedReceiptWarning {
            kind: ReceiptWarningKind::PossibleMissedItem,
            message: "a price with no description".into(),
            after_item_index: None,
        });

        let untouched = reformat(&parsed, &no_corrections());
        assert!(
            has(
                &untouched.parsed.warnings,
                ReceiptWarningKind::PossibleMissedItem
            ),
            "a header-only edit must not touch findings about the items"
        );

        let replaced = ReceiptCorrections {
            items: Some(vec![item("Milk", "10.00", "")]),
            ..no_corrections()
        };
        let out = reformat(&parsed, &replaced);
        assert!(
            !has(&out.parsed.warnings, ReceiptWarningKind::PossibleMissedItem),
            "it describes a list the user has replaced: {:?}",
            out.parsed.warnings
        );
    }

    #[test]
    fn correcting_a_misread_total_is_what_makes_the_receipt_balance() {
        let mut parsed = sample_parsed();
        parsed.total = "100.00".into();
        parsed.subtotal = Some("100.00".into());

        let corrections = ReceiptCorrections {
            total: Some("10.00".into()),
            subtotal: Some("10.00".into()),
            ..no_corrections()
        };
        let out = reformat(&parsed, &corrections);
        assert_eq!(out.parsed.total, "10.00".into());
        assert!(out.beancount.contains("10.00"));
        assert!(!has(
            &out.parsed.warnings,
            ReceiptWarningKind::SubtotalMismatch
        ));
    }

    #[test]
    fn reformat_rejects_invalid_date_iso() {
        let parsed = sample_parsed();
        let corrections = ReceiptCorrections {
            merchant: None,
            date_iso: Some("not-a-date".into()),
            items: None,
            total: None,
            tax: None,
            subtotal: None,
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
            RawDetectionPage::try_new(vec![], 100, 100, 0).expect("valid empty page"),
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
