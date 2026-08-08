use std::collections::HashSet;

use crate::receipt_categories;
use crate::receipt_common::ReceiptWarningKind;
use crate::receipt_fields;
use crate::receipt_parse_helpers;
use crate::receipt_spatial;
use crate::receipt_text;

#[derive(Clone, Debug)]
pub struct ParserRuleLayers {
    pub category_rules: receipt_categories::CategoryRuleLayers,
    pub account_mapping: Vec<(String, String)>,
    /// Per-merchant abbreviation tables, applied before classification so that
    /// a chain's fixed-width shorthand (`KS LIQ LNDRY`) can reach keywords that
    /// are spelled out in full. See [`crate::merchant_vocab`].
    pub merchant_vocab: Vec<crate::merchant_vocab::MerchantVocab>,
}

#[derive(Clone, Debug)]
pub struct ParsedReceiptItem {
    pub description: String,
    pub price: String,
    pub quantity: i32,
    /// The winning rule's declared tag path (`grocery/dairy`), or `None`.
    pub category: Option<String>,
    /// The beancount account `category` resolves to, resolved at parse time so
    /// consumers do not have to carry the account map around. `None` when the
    /// path claims no account.
    pub account: Option<String>,
    /// The beanbeaver-internal semantic classification for this line — a
    /// multi-tag view (e.g. `["grocery", "meat", "chicken"]`) that is upstream
    /// of, and richer than, the single `category` beancount account. Consumers
    /// (the app UI) can present or filter on tags without reverse-engineering
    /// the account path. Empty when no classifier rule matched.
    pub tags: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ParsedReceiptWarning {
    pub kind: ReceiptWarningKind,
    pub message: String,
    pub after_item_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ParsedReceiptTender {
    pub amount: String,
    pub account: Option<String>,
    pub kind: String,
    pub raw_label: String,
}

#[derive(Clone, Debug)]
pub struct ParsedReceiptData {
    /// Display merchant name: the canonical family when confidently resolved,
    /// otherwise the raw OCR text. Equal to `merchant_match.display()`.
    pub merchant: String,
    /// Full merchant resolution (raw OCR text, canonical family, confidence),
    /// for consumers that want to surface the correction to the user.
    pub merchant_match: crate::merchant_match::MerchantMatch,
    pub date: Option<(i32, u32, u32)>,
    pub date_is_placeholder: bool,
    pub total: String,
    pub items: Vec<ParsedReceiptItem>,
    pub tax: Option<String>,
    pub subtotal: Option<String>,
    pub raw_text: String,
    pub image_filename: String,
    pub warnings: Vec<ParsedReceiptWarning>,
    pub tenders: Vec<ParsedReceiptTender>,
}

/// Some merchants print a line-item discount with NO sign — e.g. FreshCo's
/// "INSTANT SAVINGS $5.00" — unlike Costco's trailing-minus ("4.00-") or the
/// leading-minus Asian-grocery form. Such a line is a reduction, so its price
/// must be negative. Detection is keyword-based on the resolved item
/// description. Summary lines ("TOTAL SAVINGS", "Your Total Savings") are
/// filtered upstream and never reach here as items; the `TOTAL` guard is a
/// belt-and-suspenders exclusion in case one slips through.
fn is_unsigned_discount_line(description: &str) -> bool {
    let upper = description.to_ascii_uppercase();
    if upper.contains("TOTAL") {
        return false;
    }
    upper.contains("SAVINGS")
}

fn cents_to_fixed(value: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

fn scaled_to_fixed(value: i64, scale: i64) -> String {
    let sign = if value < 0 { "-" } else { "" };
    let abs = value.abs();
    let whole = abs / scale;
    let frac = abs % scale;
    format!("{sign}{whole}.{:04}", frac)
}

fn resolve_account_target(
    target: Option<&str>,
    rule_layers: &ParserRuleLayers,
    default: Option<&str>,
) -> Option<String> {
    match target {
        None => default.map(str::to_string),
        Some(raw) => {
            let cleaned = raw.trim();
            if cleaned.is_empty() {
                return default.map(str::to_string);
            }
            if cleaned.starts_with("Expenses:") {
                return Some(cleaned.to_string());
            }
            for (key, mapped) in &rule_layers.account_mapping {
                if key == cleaned {
                    return Some(mapped.clone());
                }
            }
            default.map(str::to_string)
        }
    }
}

fn categorize_description(description: &str, rule_layers: &ParserRuleLayers) -> Option<String> {
    let category_key =
        receipt_categories::classify_item_key(description, &rule_layers.category_rules, None);
    resolve_account_target(category_key.as_deref(), rule_layers, None)
}

/// The internal semantic tags for an item description — the multi-tag layer that
/// sits upstream of the single beancount account `categorize_description`
/// resolves. Classified from the same source string so the two agree.
fn item_tags(description: &str, rule_layers: &ParserRuleLayers) -> Vec<String> {
    receipt_categories::classify_item_tags(description, &rule_layers.category_rules)
}

/// Assemble one parsed item, applying the merchant's abbreviation vocabulary.
///
/// `category_source` is the string the extractor decided should drive
/// classification — the same line as `description` on the spatial path, but a
/// separate (often longer) source on the text path. Both get expanded, so the
/// recovered name and the category always agree about what the item is.
///
/// When `vocab` is `None` — no merchant table, or a merchant that never
/// resolved — every expansion is a no-op and this is exactly the old behavior.
fn build_item(
    description: String,
    price: String,
    quantity: i32,
    category_source: &str,
    rule_layers: &ParserRuleLayers,
    vocab: Option<&crate::merchant_vocab::MerchantVocab>,
) -> ParsedReceiptItem {
    // Expansion is a **fallback, never an override**. Classify the printed text
    // first and keep that answer whenever it produces one; only reach for the
    // expanded reading when the shorthand classified to nothing.
    //
    // This is load-bearing in two directions, both found by corpus measurement:
    //
    //  - Expansions can collide with another category. `CQLDWTR -> Cold Water`
    //    turns Tide detergent into "TIDE Cold Water", and `WATER` is a Drink
    //    keyword — so an override would have filed laundry soap as a beverage.
    //  - Several existing rules are keyed to the *abbreviated* text
    //    (`KS BAGS 60`, `TIDE CQLDWTR`, `KS ORG 2%`). Rewriting the string out
    //    from under them makes those rules stop matching.
    //
    // Fallback ordering sidesteps both: a line the rules already understand is
    // untouched, and expansion can only ever fill a gap.
    let printed_category = categorize_description(category_source, rule_layers);
    let (category, tags) = if printed_category.is_some() {
        (printed_category, item_tags(category_source, rule_layers))
    } else {
        match vocab.and_then(|vocab| {
            crate::merchant_vocab::expand_for_classification(category_source, vocab)
        }) {
            Some(expanded) => (
                categorize_description(&expanded, rule_layers),
                item_tags(&expanded, rule_layers),
            ),
            None => (None, item_tags(category_source, rule_layers)),
        }
    };

    // The printed text stays the leading part of the description: it is what the
    // receipt actually says, so it must survive for ledger review and for
    // matching against a bank line. The recovered reading is appended, not
    // substituted.
    let description = match vocab
        .and_then(|v| crate::merchant_vocab::expand(&description, v))
        .and_then(|recovered| crate::merchant_vocab::recovered_tail(&description, &recovered))
    {
        Some(tail) => format!("{description} ({tail})"),
        None => description,
    };

    let account = receipt_categories::resolve_account_target(
        category.as_deref(),
        &rule_layers.category_rules.account_mapping,
        None,
    );

    ParsedReceiptItem {
        description,
        price,
        quantity,
        category,
        account,
        tags,
    }
}

pub fn parse_receipt(
    full_text: &str,
    pages_for_helper: &[receipt_parse_helpers::MerchantPageInput],
    pages_for_spatial: &[receipt_spatial::PageInput],
    rule_layers: &ParserRuleLayers,
    image_filename: &str,
    known_merchants: &[String],
    merchant_families: &[crate::merchant_match::MerchantFamily],
    current_year: i32,
) -> ParsedReceiptData {
    let lines = full_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    // Rows standing in a print-grid column that never carries a price annotate
    // the item above them rather than being one (see
    // `receipt_spatial::annotation_line_flags`). The verdict is geometric, so
    // it has to be taken here: the text path below sees only strings, and the
    // chains this catches — Food Basics' `Saving 4.72` — word their annotations
    // in a vocabulary no keyword list has met yet.
    //
    // `full_text` is the grouped lines joined with newlines and each group's own
    // text never contains one, so `full_text.lines()` is 1:1 with the spatial
    // pages' lines and the flags index straight into it. Callers that build the
    // two independently (tests, and any consumer passing no geometry at all) are
    // not 1:1, and there the flags index nothing — so require the counts to
    // agree before trusting them rather than silently marking the wrong row.
    let annotation_flags = receipt_spatial::annotation_line_flags(pages_for_spatial);
    let aligned = annotation_flags.len() == full_text.lines().count();
    let item_lines: Vec<String> = full_text
        .lines()
        .enumerate()
        .filter(|(index, _)| !aligned || !annotation_flags[*index])
        .map(|(_, line)| line.trim())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    let merchant_match = receipt_parse_helpers::extract_merchant_match(
        &lines,
        full_text,
        pages_for_helper,
        known_merchants,
        merchant_families,
    );
    let merchant = merchant_match.display().to_string();
    // Scoped to the *canonical* family, not the raw OCR header: an unresolved
    // merchant gets no expansions, so the feature fails closed.
    let vocab = merchant_match.canonical.as_deref().and_then(|canonical| {
        crate::merchant_vocab::for_merchant(canonical, &rule_layers.merchant_vocab)
    });
    let parsed_date = receipt_fields::extract_date(&lines, full_text, current_year);
    let date = parsed_date.map(|value| (value.year, value.month, value.day));
    let date_is_placeholder = date.is_none();
    let total_cents = receipt_fields::extract_total(&lines);
    let tax_cents = receipt_fields::extract_tax_reconciled(&lines, total_cents);
    let subtotal_cents = receipt_fields::extract_subtotal(&lines);

    let mut summary_amounts = HashSet::new();
    if total_cents != 0 {
        summary_amounts.insert(total_cents);
    }
    if let Some(tax_cents) = tax_cents {
        summary_amounts.insert(tax_cents);
    }
    if let Some(subtotal_cents) = subtotal_cents {
        summary_amounts.insert(subtotal_cents);
    }

    let spatial_layout = receipt_parse_helpers::has_useful_bbox_data(pages_for_helper)
        && receipt_parse_helpers::is_spatial_layout_receipt(full_text);

    let (items, mut warnings): (Vec<ParsedReceiptItem>, Vec<ParsedReceiptWarning>) =
        if spatial_layout {
            let spatial_outcome =
                receipt_spatial::extract_spatial_items(pages_for_spatial.to_vec());
            if spatial_outcome.items.is_empty() {
                let (items, warnings) =
                    receipt_text::extract_text_items(&item_lines, &summary_amounts);
                (
                    items
                        .into_iter()
                        .map(|item| {
                            build_item(
                                item.description.clone(),
                                cents_to_fixed(item.price_cents),
                                item.quantity,
                                &item.category_source,
                                rule_layers,
                                vocab,
                            )
                        })
                        .collect(),
                    warnings
                        .into_iter()
                        .map(|warning| ParsedReceiptWarning {
                            kind: warning.kind,
                            message: warning.message,
                            after_item_index: warning.after_item_index,
                        })
                        .collect(),
                )
            } else {
                (
                    spatial_outcome
                        .items
                        .into_iter()
                        .map(|item| {
                            build_item(
                                item.description.clone(),
                                scaled_to_fixed(item.price_scaled, 10_000),
                                1,
                                &item.description,
                                rule_layers,
                                vocab,
                            )
                        })
                        .collect(),
                    spatial_outcome
                        .warnings
                        .into_iter()
                        .map(|warning| ParsedReceiptWarning {
                            kind: warning.kind,
                            message: warning.message,
                            after_item_index: warning.after_item_index,
                        })
                        .collect(),
                )
            }
        } else {
            let (items, warnings) = receipt_text::extract_text_items(&item_lines, &summary_amounts);
            (
                items
                    .into_iter()
                    .map(|item| {
                        build_item(
                            item.description.clone(),
                            cents_to_fixed(item.price_cents),
                            item.quantity,
                            &item.category_source,
                            rule_layers,
                            vocab,
                        )
                    })
                    .collect(),
                warnings
                    .into_iter()
                    .map(|warning| ParsedReceiptWarning {
                        kind: warning.kind,
                        message: warning.message,
                        after_item_index: warning.after_item_index,
                    })
                    .collect(),
            )
        };

    // Sign-correct unsigned line-item discounts (e.g. FreshCo "INSTANT
    // SAVINGS $5.00"), covering both the spatial and text paths at their
    // single merge point.
    let items: Vec<ParsedReceiptItem> = items
        .into_iter()
        .map(|mut item| {
            if !item.price.starts_with('-') && is_unsigned_discount_line(&item.description) {
                item.price = format!("-{}", item.price);
            }
            item
        })
        .collect();

    // Postings that overshoot the receipt total cannot balance, and until now
    // nothing said so. `receipt_formatter` closes an *undershoot* with an
    // `Expenses:FIXME` remainder — the ordinary "we missed an item" case, 26 of
    // 125 corpus receipts — but has no answer for the other direction and
    // silently emits a transaction beancount will reject. Overshoot is always a
    // defect: an item is duplicated, or a summary amount was parsed as an item.
    // It is also rare and specific — 6 of 125 receipts, every one genuinely
    // wrong — so warning on it is a signal, not noise.
    if total_cents > 0 {
        let posted_cents = items
            .iter()
            .map(|item| crate::receipt_formatter::decimal_to_cents(&item.price))
            .sum::<i64>()
            + tax_cents.unwrap_or(0);
        if posted_cents > total_cents {
            warnings.push(ParsedReceiptWarning {
                kind: ReceiptWarningKind::TotalMismatch,
                message: format!(
                    "items{} total {} but the receipt total is {} — {} too much, so this transaction will not balance",
                    if tax_cents.is_some() { " and tax" } else { "" },
                    cents_to_fixed(posted_cents),
                    cents_to_fixed(total_cents),
                    cents_to_fixed(posted_cents - total_cents),
                ),
                after_item_index: None,
            });
        }

        // Undershoot is the other half, and it used to say nothing at all: the
        // formatter quietly closes the gap with an `Expenses:FIXME` remainder,
        // so a receipt could be *entirely* mis-paired and still report clean.
        // A No Frills scan lost one item and gave the remaining three their
        // neighbours' prices, and the only trace was a 3.78 plug nobody saw.
        //
        // Warning on every undershoot is noise — 23 of 122 receipts, most of
        // them a few cents. What makes it a signal is asking the question
        // against the printed SUBTOTAL instead of the total: fees, deposits,
        // rounding and tax defects all live *between* subtotal and total, so
        // measuring there is what mixes them into the item-block question.
        //
        // Measured over the corpus, the two deltas triangulate:
        //
        //   items != subtotal, posted != total -> the item block is wrong (16)
        //   items != subtotal, posted == total -> the SUBTOTAL was misread (2)
        //   items == subtotal, posted != total -> items fine, tax/fees (9)
        //
        // Only the first is a missing or spurious line, and requiring both to
        // disagree is what removes the entire sub-dollar noise band — every
        // 9c/10c/15c/20c case in the corpus lands in the third bucket.
        if let Some(subtotal_cents) = subtotal_cents {
            let items_cents = posted_cents - tax_cents.unwrap_or(0);
            let item_block_delta = items_cents - subtotal_cents;
            if item_block_delta != 0 && posted_cents != total_cents {
                let (verb, amount) = if item_block_delta < 0 {
                    ("short of", -item_block_delta)
                } else {
                    ("more than", item_block_delta)
                };
                warnings.push(ParsedReceiptWarning {
                    kind: ReceiptWarningKind::SubtotalMismatch,
                    message: format!(
                        "items total {}, {} the receipt's subtotal of {} by {} — a line was probably {}",
                        cents_to_fixed(items_cents),
                        verb,
                        cents_to_fixed(subtotal_cents),
                        cents_to_fixed(amount),
                        if item_block_delta < 0 { "missed" } else { "counted twice" },
                    ),
                    after_item_index: None,
                });
            }
        }
    }

    // The payment block is an independent witness to the total: when a receipt
    // prints its tenders, they partition the total rather than echoing it, so
    // their sum is a second reading of the same number. Report the disagreement
    // — `extract_tenders` used to swallow it, returning nothing at all, which
    // made a misread amount look like a receipt with no payment block.
    //
    // Deliberately *only* a report. Which side is wrong is not recoverable from
    // the arithmetic (see `ReceiptWarningKind::TenderMismatch`), so the total
    // stands as parsed and `receipt_formatter` keeps the entry balanced by
    // falling back to a single payment posting.
    let tender_lines = receipt_fields::extract_tenders(&lines);
    if !receipt_fields::tenders_reconcile(&lines, &tender_lines, total_cents) {
        let net_cents = receipt_fields::tendered_net_cents(&lines, &tender_lines);
        warnings.push(ParsedReceiptWarning {
            kind: ReceiptWarningKind::TenderMismatch,
            message: format!(
                "payment lines account for {} but the receipt total is {} — {} unaccounted for, so one of the two is misread",
                cents_to_fixed(net_cents),
                cents_to_fixed(total_cents),
                cents_to_fixed((net_cents - total_cents).abs()),
            ),
            after_item_index: None,
        });
    }

    // "This line matched no classifier rule" is a finding like any other, and
    // the parser is the only layer that knows it first-hand. It used to be
    // re-derived by each client from `tags.is_empty()`, which is how a
    // perfectly-parsed discount line ended up indistinguishable from a product
    // nobody has written a rule for. Reported last so the arithmetic warnings —
    // which are about the receipt as a whole — keep their existing position.
    for (index, item) in items.iter().enumerate() {
        if item.tags.is_empty() {
            warnings.push(ParsedReceiptWarning {
                kind: ReceiptWarningKind::UncategorizedItem,
                message: format!("no classifier rule matched \"{}\"", item.description),
                after_item_index: Some(index),
            });
        }
    }

    let tenders = tender_lines
        .into_iter()
        .map(|tender| ParsedReceiptTender {
            amount: cents_to_fixed(tender.amount_cents),
            account: None,
            kind: tender.kind.to_string(),
            raw_label: tender.raw_label,
        })
        .collect();

    ParsedReceiptData {
        merchant,
        merchant_match,
        date,
        date_is_placeholder,
        total: cents_to_fixed(total_cents),
        items,
        tax: tax_cents.map(cents_to_fixed),
        subtotal: subtotal_cents.map(cents_to_fixed),
        raw_text: full_text.to_string(),
        image_filename: image_filename.to_string(),
        warnings,
        tenders,
    }
}

#[cfg(test)]
mod tests {
    use super::{cents_to_fixed, is_unsigned_discount_line, item_tags};
    use crate::receipt_common::ReceiptWarningKind;
    use crate::rules::default_parser_rule_layers;

    #[test]
    fn item_tags_are_the_multi_tag_classification() {
        let layers = default_parser_rule_layers();
        // A rotisserie chicken matches several rules — the meat rule
        // (grocery, meat), the semantic chicken tag, and the prepared-meal rule
        // — and their tags accumulate (deduped, first-seen order) onto one item.
        // Tags are node PATHS, least specific first, so the tree survives the
        // trip to a consumer without being reconstructed from bare segments.
        assert_eq!(
            item_tags("ROTISSERIE CHICKEN", &layers),
            vec![
                "grocery",
                "grocery/meat",
                "grocery/meat/chicken",
                "grocery/prepared_meal"
            ]
        );
        // Milk carries the dairy rule's tags plus its own semantic "milk" tag.
        assert_eq!(
            item_tags("MILK", &layers),
            vec!["grocery", "grocery/dairy", "grocery/dairy/milk"]
        );
        // An unrecognized line classifies to no tags rather than a guess.
        assert!(item_tags("ZZQW UNKNOWN ITEM", &layers).is_empty());
    }

    /// Parse plain text through the real pipeline: no bbox data, so the text
    /// path runs, which is all these balance assertions need.
    fn parse_text(text: &str) -> super::ParsedReceiptData {
        let layers = default_parser_rule_layers();
        super::parse_receipt(text, &[], &[], &layers, "receipt.jpg", &[], &[], 2026)
    }

    #[test]
    fn warns_when_postings_overshoot_the_receipt_total() {
        // The No Frills scan that prompted this: line grouping gave the subtotal
        // its own item row, so the postings came to double the total and the
        // emitted transaction could never balance — silently, until now.
        let parsed = parse_text(
            "NOFRILLS\n\
             22-DAIRY MRJ 2.29\n\
             2% NATURAL YOGUR\n\
             27-PRODUCE WATERMLN SGRBABY MRJ 23.96\n\
             (4)4331 26.25\n\
             SUBTOTAL 26.25\n\
             TOTAL 26.25\n",
        );

        assert_eq!(parsed.total, "26.25");
        let balance: Vec<_> = parsed
            .warnings
            .iter()
            .filter(|w| w.kind == ReceiptWarningKind::TotalMismatch)
            .collect();
        assert_eq!(balance.len(), 1, "warnings were {:?}", parsed.warnings);

        // Assert the message against the parse it describes rather than against
        // numbers copied from one scan: the text path reaches a different item
        // set than the spatial one, and a warning that misreports the amounts
        // would be worse than no warning at all.
        let posted: i64 = parsed
            .items
            .iter()
            .map(|item| crate::receipt_formatter::decimal_to_cents(&item.price))
            .sum();
        assert!(posted > 2_625, "fixture should overshoot, posted {posted}");
        assert!(
            balance[0].message.contains(&cents_to_fixed(posted))
                && balance[0].message.contains("26.25")
                && balance[0].message.contains(&cents_to_fixed(posted - 2_625)),
            "message should name the posted total, the receipt total and the gap: {}",
            balance[0].message
        );
    }

    #[test]
    fn warns_when_items_disagree_with_the_printed_subtotal() {
        // The signal case: items fall short of the printed subtotal *and* the
        // postings miss the total, so a line was genuinely lost.
        let parsed = parse_text(
            "NOFRILLS\n\
             MILK 4.00\n\
             BREAD 3.00\n\
             SUBTOTAL 10.00\n\
             HST 1.30\n\
             TOTAL 11.30\n",
        );
        assert!(
            parsed
                .warnings
                .iter()
                .any(|w| w.kind == ReceiptWarningKind::SubtotalMismatch
                    && w.message.contains("short of the receipt's subtotal")),
            "warnings were {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn does_not_warn_when_only_fees_sit_between_subtotal_and_total() {
        // items == subtotal exactly, but posted != total because of a deposit
        // between them. This is the whole sub-dollar noise band the corpus is
        // full of, and it must stay silent.
        let parsed = parse_text(
            "NOFRILLS\n\
             MILK 4.00\n\
             BREAD 3.00\n\
             SUBTOTAL 7.00\n\
             TOTAL 7.10\n",
        );
        assert!(
            !parsed
                .warnings
                .iter()
                .any(|w| w.kind == ReceiptWarningKind::SubtotalMismatch),
            "warnings were {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn does_not_warn_when_postings_merely_undershoot() {
        // The ordinary "we missed an item" case — 26 of 125 corpus receipts.
        // `receipt_formatter` closes it with an `Expenses:FIXME` remainder, so
        // the transaction balances and there is nothing to report.
        let parsed = parse_text(
            "NOFRILLS\n\
             MILK 2.29\n\
             TOTAL 26.25\n",
        );

        assert_eq!(parsed.total, "26.25");
        assert!(
            !parsed
                .warnings
                .iter()
                .any(|w| w.kind == ReceiptWarningKind::TotalMismatch),
            "warnings were {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn flags_unsigned_savings_lines() {
        // FreshCo prints "INSTANT SAVINGS $5.00" with no minus sign.
        assert!(is_unsigned_discount_line("INSTANT SAVINGS $"));
        assert!(is_unsigned_discount_line("Member Savings"));
    }

    #[test]
    fn ignores_summary_and_regular_items() {
        // Summary rollups must never be sign-flipped as items.
        assert!(!is_unsigned_discount_line("TOTAL SAVINGS"));
        assert!(!is_unsigned_discount_line("Your Total Savings"));
        // Ordinary products are untouched.
        assert!(!is_unsigned_discount_line("Tom Diced"));
        assert!(!is_unsigned_discount_line("Soft Drink Orange"));
    }

    #[test]
    fn discount_lines_classify_as_discounts_not_as_nothing() {
        let layers = default_parser_rule_layers();
        // Costco: the discount code, then the code of the item it reduces.
        assert_eq!(item_tags("2087683 TPD/969786", &layers), vec!["discount"]);
        // FreshCo's two spellings.
        assert_eq!(item_tags("INSTANT SAVINGS", &layers), vec!["discount"]);
        assert_eq!(item_tags("YOU SAVED", &layers), vec!["discount"]);
        // A discount that names what it discounts nets against that category —
        // the rule sits *below* the product rules on purpose, so the product
        // wins the account while the line still picks up the `discount` tag.
        let both = item_tags("INSTANT SAVINGS ON MILK", &layers);
        assert!(both.contains(&"discount".to_string()), "tags were {both:?}");
        assert!(
            both.contains(&"grocery/dairy".to_string()),
            "tags were {both:?}"
        );
        assert_eq!(
            super::categorize_description("INSTANT SAVINGS ON MILK", &layers).as_deref(),
            Some("Expenses:Food:Grocery:Dairy")
        );
        // With nothing to net against, it files to its own account rather than
        // to the Expenses:FIXME it used to get.
        assert_eq!(
            super::categorize_description("2087683 TPD/969786", &layers).as_deref(),
            Some("Expenses:Discount")
        );
        // And the keyword is specific: a bare product line near those words is
        // still a product.
        assert!(!item_tags("MILK", &layers).contains(&"discount".to_string()));
    }

    #[test]
    fn uncategorized_items_are_reported_as_a_finding_with_their_index() {
        let parsed = parse_text(
            "NOFRILLS\n\
             MILK 4.00\n\
             ZZQW UNKNOWN ITEM 3.00\n\
             SUBTOTAL 7.00\n\
             TOTAL 7.00\n",
        );
        let uncategorized: Vec<_> = parsed
            .warnings
            .iter()
            .filter(|w| w.kind == ReceiptWarningKind::UncategorizedItem)
            .collect();
        assert_eq!(
            uncategorized.len(),
            1,
            "only the unknown line should be uncategorized: {:?}",
            parsed.warnings
        );
        let index = uncategorized[0]
            .after_item_index
            .expect("an uncategorized item names the item it is about");
        assert!(
            parsed.items[index].description.contains("UNKNOWN"),
            "warning points at {:?}",
            parsed.items[index]
        );
    }

    #[test]
    fn a_parsed_discount_line_is_not_a_finding() {
        // The Costco receipt that prompted this: the discount reconciles
        // exactly, so nothing about this parse deserves the user's attention —
        // and before discounts were classified, this receipt reported an
        // uncategorized item forever.
        let parsed = parse_text(
            "COSTCO\n\
             969786 PANDA COOKIE 15.99\n\
             2087683 TPD/969786 4.00-\n\
             SUBTOTAL 11.99\n\
             TOTAL 11.99\n",
        );
        assert!(
            parsed.warnings.is_empty(),
            "a clean discount receipt should report nothing: {:?}",
            parsed.warnings
        );
        let discount = parsed
            .items
            .iter()
            .find(|i| i.description.contains("TPD/"))
            .expect("the discount line is an item");
        assert_eq!(discount.price, "-4.00");
        assert_eq!(discount.tags, vec!["discount"]);
        assert_eq!(discount.account.as_deref(), Some("Expenses:Discount"));
    }
}
