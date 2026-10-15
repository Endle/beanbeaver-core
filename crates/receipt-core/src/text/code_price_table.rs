//! Explicit item tables with names above or below code/quantity/price rows.
use crate::common::{ReceiptWarning, ReceiptWarningKind};
use crate::extraction::{ExtractedItem, ExtractionOutcome};
use crate::money::Money;
use regex::Regex;
use std::sync::OnceLock;

/// The explicit table header establishes the column meanings. Require every
/// row to have an adjacent name, unit × quantity to equal its line total, and
/// the complete table to equal the printed subtotal. This permits a lost
/// decimal (`250`, `2 50`) without guessing from an isolated integer, and
/// prevents the unit-price column from becoming a product description.
pub(super) fn extract(lines: &[String]) -> Option<ExtractionOutcome> {
    let (header, name_above) = lines.iter().enumerate().find_map(|(i, line)| {
        match line
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_uppercase()
            .as_str()
        {
            "ITEM CODE PRICE QTY TOTAL PRICE" => Some((i, false)),
            "ITEM QTY PRICE AMOUNT" => Some((i, true)),
            _ => None,
        }
    })?;
    let start = header + 1;
    let end = (start..lines.len()).find(|&i| {
        let upper = lines[i].to_ascii_uppercase();
        upper.starts_with("SUB TOTAL") || upper.starts_with("SUBTOTAL")
    })?;
    let subtotal = crate::fields::extract_subtotal(&lines[end..])?;
    let body: Vec<_> = lines[start..end]
        .iter()
        .filter(|line| {
            // Rules drawn around the table may survive OCR as hyphens.
            !(name_above && line.trim().chars().all(|c| c == '-' || c.is_whitespace()))
        })
        .collect();
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
    static NAME_ABOVE_ROW: OnceLock<Regex> = OnceLock::new();
    let row = if name_above {
        NAME_ABOVE_ROW.get_or_init(|| {
            Regex::new(concat!(
                r"^(?P<code>[A-Za-z0-9]+)\s+",
                r"(?:(?P<qty>\d+)\s+)?",
                r"(?P<unit>\d+[.,]\d{2})\s+",
                r"(?P<total>\d+[.,]\d{2})\s*[A-Z]?\s*$"
            ))
            .unwrap()
        })
    } else {
        row
    };
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut sum = 0i64;
    for pair in body.chunks_exact(2) {
        let (price_row, name_row) = if name_above {
            (pair[1], pair[0])
        } else {
            (pair[0], pair[1])
        };
        let captures = row.captures(price_row.trim())?;
        if name_above && !captures["code"].chars().any(|c| c.is_ascii_digit()) {
            return None;
        }
        let unit = amount(&captures["unit"])?;
        let total = amount(&captures["total"])?;
        let quantity = match captures.name("qty") {
            Some(qty) => qty.as_str().parse::<i32>().ok()?,
            // OCR can drop the isolated quantity column. Only infer a whole
            // positive count from exact cents; the table must also reconcile
            // with the printed subtotal below. Never override a printed count.
            None if name_above && unit > 0 && total % unit == 0 => {
                i32::try_from(total / unit).ok()?
            }
            None if name_above => return None,
            None => 1,
        };
        if quantity <= 0 || unit.checked_mul(i64::from(quantity))? != total {
            return None;
        }
        let description = name_row.trim();
        if !description.chars().any(|c| c.is_ascii_alphabetic())
            || super::patterns::re_skip_patterns().is_match(description)
            || (name_above && super::tokens::extract_trailing_price_cents(description).is_some())
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

    #[test]
    fn name_above_tables_require_complete_names_and_exact_arithmetic() {
        let table = |body: &str| {
            format!("Item Qty Price Amount\n{body}")
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let valid = table("-----\nSmall bowl\nAB123 4.00 8.00H\n-----\nSubtotal: $8.00");
        let result = extract(&valid).unwrap();
        assert_eq!(result.items[0].description, "Small bowl");
        assert_eq!(result.items[0].quantity, 2);
        assert!(extract(&valid[1..]).is_none());
        for body in [
            "Small bowl\nAB123 4.00 8.01H\nSubtotal: $8.01",
            "Small bowl\nAB123 1 4.00 8.00H\nSubtotal: $8.00",
            "Small bowl\nAB123 0 4.00 0.00H\nSubtotal: $0.00",
            "Small bowl\nAB123 0.00 8.00H\nSubtotal: $8.00",
            "Small bowl\nAB123 4.00 8.00H\nSubtotal: $9.00",
            "Small bowl\nAB123 4.00 8.00H\nLarge bowl\nSubtotal: $8.00",
            "Small bowl 8.00\nAB123 4.00 8.00H\nSubtotal: $8.00",
            "HST 123456789\nAB123 4.00 8.00H\nSubtotal: $8.00",
            "Small bowl\nSALE 4.00 8.00H\nSubtotal: $8.00",
            "Small bowl\nAB123 400 800\nSubtotal: $8.00",
            "Small bowl\nAB123 0.01 21474836.48H\nSubtotal: $21474836.48",
        ] {
            assert!(extract(&table(body)).is_none(), "{body}");
        }
    }
}
