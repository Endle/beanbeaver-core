use std::collections::HashSet;

use crate::receipt_categories;
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
    pub category: Option<String>,
    /// The beanbeaver-internal semantic classification for this line — a
    /// multi-tag view (e.g. `["grocery", "meat", "chicken"]`) that is upstream
    /// of, and richer than, the single `category` beancount account. Consumers
    /// (the app UI) can present or filter on tags without reverse-engineering
    /// the account path. Empty when no classifier rule matched.
    pub tags: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ParsedReceiptWarning {
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

    ParsedReceiptItem {
        description,
        price,
        quantity,
        category,
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
    let tax_cents = receipt_fields::extract_tax(&lines);
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

    let (items, warnings): (Vec<ParsedReceiptItem>, Vec<ParsedReceiptWarning>) = if spatial_layout {
        let spatial_outcome = receipt_spatial::extract_spatial_items(pages_for_spatial.to_vec());
        if spatial_outcome.items.is_empty() {
            let (items, warnings) = receipt_text::extract_text_items(&lines, &summary_amounts);
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
                        message: warning.message,
                        after_item_index: warning.after_item_index,
                    })
                    .collect(),
            )
        }
    } else {
        let (items, warnings) = receipt_text::extract_text_items(&lines, &summary_amounts);
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

    let tenders = receipt_fields::extract_tenders(&lines, total_cents)
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
    use super::{is_unsigned_discount_line, item_tags};
    use crate::rules::default_parser_rule_layers;

    #[test]
    fn item_tags_are_the_multi_tag_classification() {
        let layers = default_parser_rule_layers();
        // A rotisserie chicken matches several rules — the meat rule
        // (grocery, meat), the semantic chicken tag, and the prepared-meal rule
        // — and their tags accumulate (deduped, first-seen order) onto one item.
        // "prepared_meal" is one tag now: the old category key split it in two.
        assert_eq!(
            item_tags("ROTISSERIE CHICKEN", &layers),
            vec!["grocery", "meat", "chicken", "prepared_meal"]
        );
        // Milk carries the dairy rule's tags plus its own semantic "milk" tag.
        assert_eq!(item_tags("MILK", &layers), vec!["grocery", "dairy", "milk"]);
        // An unrecognized line classifies to no tags rather than a guess.
        assert!(item_tags("ZZQW UNKNOWN ITEM", &layers).is_empty());
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
}
