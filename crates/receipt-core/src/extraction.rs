//! Shared output of the text and spatial item extractors.
use crate::common::ReceiptWarning;
use crate::money::Money;

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

#[cfg(test)]
mod tests {
    use super::leading_item_number;

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
