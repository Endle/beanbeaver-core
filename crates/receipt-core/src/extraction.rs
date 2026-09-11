//! Shared output of the text and spatial item extractors.
use crate::common::{ReceiptWarning, ReceiptWarningKind};
use crate::money::Money;
use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub(crate) struct ExtractedItem {
    pub description: String,
    /// Candidate from the selected row before cleanup; validated by merchant in parser.
    pub item_number: Option<String>,
    /// Printed text selected for classification, before display expansion.
    pub category_source: String,
    pub price: Money,
    pub quantity: i32,
}

#[derive(Clone, Debug)]
pub(crate) struct ExtractionOutcome {
    pub items: Vec<ExtractedItem>,
    pub warnings: Vec<ReceiptWarning>,
}

/// Costco prints a numeric item code before the description. Capture the row's
/// own code, never the product reference after `TPD/`. The caller must gate this
/// candidate on the resolved merchant; other chains use leading quantities.
/// Keep digits verbatim, including leading zeros, and do not repair OCR guesses.
pub(crate) fn leading_item_number(row: &str) -> Option<String> {
    let mut tokens = row.split_whitespace();
    let mut number = tokens.next()?;
    // An optional E marker can precede the item code.
    if number.eq_ignore_ascii_case("E") {
        number = tokens.next()?;
    }
    (number.bytes().all(|b| b.is_ascii_digit())
        && tokens.any(|token| token.chars().any(|c| c.is_ascii_alphabetic())))
    .then(|| number.to_string())
}

/// A load row: a short code and an integer, nothing else — `BH 25`.
fn re_load_row() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Z]{1,3}\s+(\d{1,4})$").unwrap())
}

/// The amount a load row names in its own text, when its price agrees.
fn load_row_amount(item: &ExtractedItem) -> Option<Money> {
    let dollars = re_load_row()
        .captures(item.description.trim())?
        .get(1)?
        .as_str()
        .parse::<i64>()
        .ok()?;
    let named = Money::from_cents(dollars.checked_mul(100)?);
    (named > Money::ZERO && named == item.price).then_some(named)
}

/// Give a product printed at 0.00 the charge printed on the row below it.
///
/// Dollarama sells a gift card as a zero-priced SKU followed by a load row —
///
/// ```text
/// AMAZON.CA $25        07675062289    0.00
/// BH 25                               25.00
///   6300012693211842
/// ```
///
/// — so both extractors emit two items and the one card is counted twice:
/// once at nothing, once under a description no rule can classify. The load
/// row *names its own amount* (`BH 25` is 25.00), and that agreement is what
/// makes this a fold rather than a guess: the row is only absorbed when the
/// integer in its text is its price in dollars. A free item followed by an
/// ordinary priced product keeps both lines, and a load row whose text and
/// price disagree — a misread digit on either — is left alone, which is the
/// visible failure ("prefer missing items over wrong pairings").
///
/// Runs at the parser's merge point so both paths see it, though only the text
/// path has met the shape. Measured over the 144-receipt private corpus this
/// touches one receipt: no other parse emits a zero-priced item at all.
pub(crate) fn fold_zero_priced_loads(outcome: &mut ExtractionOutcome) {
    let mut i = 0;
    while i + 1 < outcome.items.len() {
        if outcome.items[i].price == Money::ZERO {
            if let Some(amount) = load_row_amount(&outcome.items[i + 1]) {
                let load = outcome.items.remove(i + 1);
                outcome.items[i].price = amount;
                // Warnings anchored below the absorbed row move up with the items.
                for warning in &mut outcome.warnings {
                    if let Some(index) = &mut warning.after_item_index {
                        if *index > i {
                            *index -= 1;
                        }
                    }
                }
                outcome.warnings.push(ReceiptWarning {
                    kind: ReceiptWarningKind::PriceAutoCorrected,
                    message: format!(
                        "\"{}\" is printed at 0.00 and charged on the load row \"{}\" — using {}",
                        outcome.items[i].description, load.description, amount,
                    ),
                    after_item_index: Some(i),
                });
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{fold_zero_priced_loads, leading_item_number, ExtractedItem, ExtractionOutcome};
    use crate::common::{ReceiptWarning, ReceiptWarningKind};
    use crate::money::Money;

    fn item(description: &str, cents: i64) -> ExtractedItem {
        ExtractedItem {
            description: description.to_string(),
            item_number: None,
            category_source: description.to_string(),
            price: Money::from_cents(cents),
            quantity: 1,
        }
    }

    fn fold(items: Vec<ExtractedItem>) -> ExtractionOutcome {
        let mut outcome = ExtractionOutcome {
            items,
            warnings: Vec::new(),
        };
        fold_zero_priced_loads(&mut outcome);
        outcome
    }

    #[test]
    fn zero_priced_card_takes_the_load_rows_price() {
        // The Dollarama shape, as the text path emits it.
        let outcome = fold(vec![
            item("AMAZON.CA $25 07675062289", 0),
            item("BH 25", 25_00),
        ]);
        let items: Vec<_> = outcome
            .items
            .iter()
            .map(|i| (i.description.as_str(), i.price.cents()))
            .collect();
        assert_eq!(items, vec![("AMAZON.CA $25 07675062289", 25_00)]);
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(
            outcome.warnings[0].kind,
            ReceiptWarningKind::PriceAutoCorrected
        );
        assert_eq!(outcome.warnings[0].after_item_index, Some(0));
    }

    #[test]
    fn a_free_item_before_a_product_keeps_both() {
        let outcome = fold(vec![item("FREE BAG", 0), item("MILK 2L", 4_99)]);
        assert_eq!(outcome.items.len(), 2);
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn a_load_row_that_disagrees_with_its_price_is_left_alone() {
        // OCR read the code's digits or the price wrong; neither is trusted.
        let outcome = fold(vec![item("AMAZON.CA $25", 0), item("BH 26", 25_00)]);
        assert_eq!(outcome.items.len(), 2);
        assert_eq!(outcome.items[0].price, Money::ZERO);
    }

    #[test]
    fn only_a_zero_priced_item_can_absorb_a_load_row() {
        let outcome = fold(vec![item("AMAZON.CA $25", 25_00), item("BH 25", 25_00)]);
        assert_eq!(outcome.items.len(), 2);
    }

    #[test]
    fn warnings_below_the_absorbed_row_move_up() {
        let mut outcome = ExtractionOutcome {
            items: vec![
                item("AMAZON.CA $25", 0),
                item("BH 25", 25_00),
                item("TAPE", 1_50),
            ],
            warnings: vec![ReceiptWarning {
                kind: ReceiptWarningKind::PossibleMissedItem,
                message: String::new(),
                after_item_index: Some(2),
            }],
        };
        fold_zero_priced_loads(&mut outcome);
        assert_eq!(outcome.items.len(), 2);
        assert_eq!(outcome.warnings[0].after_item_index, Some(1));
    }

    #[test]
    fn does_not_guess_codes_from_references_sizes_or_ocr_confusions() {
        for row in [
            "TPD/401150",
            "2% MILK",
            "4L MILK",
            "23Z952 COKE",
            "232952",
            "17.19",
        ] {
            assert_eq!(leading_item_number(row), None, "{row}");
        }
        assert_eq!(
            leading_item_number("E 000458 MILK").as_deref(),
            Some("000458")
        );
    }
}
