//! Code/price/quantity/total rows whose product descriptions print underneath.
use crate::common::{ReceiptWarning, ReceiptWarningKind};
use crate::extraction::{ExtractedItem, ExtractionOutcome};
use crate::money::Money;
use regex::Regex;
use std::sync::OnceLock;

/// The explicit table header establishes the column meanings. Require every
/// row to have a following name, unit × quantity to equal its line total, and
/// the complete table to equal the printed subtotal. This permits a lost
/// decimal (`250`, `2 50`) without guessing from an isolated integer, and
/// prevents the unit-price column from becoming a product description.
pub(super) fn extract(lines: &[String]) -> Option<ExtractionOutcome> {
    let start = lines.iter().position(|line| {
        line.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .eq_ignore_ascii_case("ITEM CODE PRICE QTY TOTAL PRICE")
    })? + 1;
    let end = (start..lines.len()).find(|&i| {
        let upper = lines[i].to_ascii_uppercase();
        upper.starts_with("SUB TOTAL") || upper.starts_with("SUBTOTAL")
    })?;
    let subtotal = crate::fields::extract_subtotal(&lines[end..])?;
    let body = &lines[start..end];
    if body.is_empty() || body.len() % 2 != 0 {
        return None;
    }
    static ROW: OnceLock<Regex> = OnceLock::new();
    let row = ROW.get_or_init(|| {
        Regex::new(concat!(
            r"^\d{8,14}\s+",
            r"(?P<unit>\d+[.,]\d{2}|\d+\s+\d{2}|\d{3,})\s+",
            r"(?:(?P<qty>\d+)\s+)?",
            r"(?P<total>\d+[.,]\d{2}|\d+\s+\d{2}|\d{3,})",
            r"\s*[A-Z]?\s*$"
        ))
        .unwrap()
    });
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut sum = 0i64;
    for pair in body.chunks_exact(2) {
        let captures = row.captures(pair[0].trim())?;
        let unit = amount(&captures["unit"])?;
        let total = amount(&captures["total"])?;
        let quantity = captures
            .name("qty")
            .map(|m| m.as_str().parse::<i32>())
            .transpose()
            .ok()?
            .unwrap_or(1);
        if quantity <= 0 || unit.checked_mul(i64::from(quantity))? != total {
            return None;
        }
        let description = pair[1].trim();
        if !description.chars().any(|c| c.is_ascii_alphabetic())
            || super::patterns::re_skip_patterns().is_match(description)
        {
            return None;
        }
        sum = sum.checked_add(total)?;
        if !captures["total"].contains(['.', ',']) {
            warnings.push(ReceiptWarning {
                kind: ReceiptWarningKind::PriceAutoCorrected,
                message: format!(
                    "recovered table amount {:?} as {} for {description:?}; unit price × quantity and subtotal agree",
                    &captures["total"], Money::from_cents(total)
                ),
                after_item_index: Some(items.len()),
            });
        }
        items.push(ExtractedItem {
            description: description.to_string(),
            category_source: description.to_string(),
            item_number: None,
            price: Money::from_cents(total),
            quantity,
            department: None,
        });
    }
    (sum == subtotal).then_some(ExtractionOutcome { items, warnings })
}

fn amount(token: &str) -> Option<i64> {
    token
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(body: &str) -> Vec<String> {
        format!("ITEM CODE PRICE QTY TOTAL PRICE\n{body}")
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn names_below_codes_and_missing_decimals_preserve_every_line() {
        let input = lines(concat!(
            "1234567890123 250 2.50\nBRUSH\n",
            "1234567890124 2.75 2 5 50 T\nBOWL\n",
            "1234567890125 300 300\nSPONGE\n",
            "1234567890125 3.00 3 00\nSPONGE\n",
            "Sub Total $: 14.00\nTOTAL $: 15.82"
        ));
        let result = extract(&input).unwrap();
        assert_eq!(
            result
                .items
                .iter()
                .map(|i| (i.description.as_str(), i.price.cents(), i.quantity))
                .collect::<Vec<_>>(),
            [
                ("BRUSH", 250, 1),
                ("BOWL", 550, 2),
                ("SPONGE", 300, 1),
                ("SPONGE", 300, 1)
            ]
        );
    }

    #[test]
    fn refuses_unconfirmed_or_incomplete_tables() {
        let valid = lines("1234567890123 2.50 2.50\nBRUSH\nSubtotal 2.50");
        assert!(extract(&valid).is_some());
        for input in [
            valid[1..].to_vec(),
            lines("1234567890123 250 250\nBRUSH\nSubtotal 250.00"),
            lines("1234567890123 2.50 2 2.50\nBRUSH\nSubtotal 2.50"),
            lines("1234567890123 2.50 2.50\nSubtotal 2.50"),
            lines("1234567890123 2.50 2.50\nGST 123456789\nSubtotal 2.50"),
        ] {
            assert!(extract(&input).is_none(), "{input:?}");
        }
    }
}
