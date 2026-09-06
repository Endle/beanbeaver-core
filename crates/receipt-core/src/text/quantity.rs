//! Text extraction: quantity.
use super::patterns::*;
use super::tokens::*;
use super::types::*;
use crate::money::Money;
use regex::Regex;
use std::sync::OnceLock;
pub(super) fn parse_quantity_modifier(line: &str) -> Option<QuantityModifier> {
    let normalized = normalize_decimal_spacing(line.trim());

    if let Some(captures) = re_count_at_price().captures(&normalized) {
        let quantity = captures.get(1)?.as_str().parse::<i32>().ok()?;
        let unit_price_cents = parse_cents(captures.get(2)?.as_str())?;
        return Some(QuantityModifier {
            quantity,
            unit_price: Some(unit_price_cents),
            weight_text: None,
            deal_price: None,
            pattern_type: QuantityPatternType::CountAtPrice,
        });
    }

    if let Some(captures) = re_weight_at_price()
        .captures(&normalized)
        .or_else(|| re_weight_rate_no_at().captures(&normalized))
    {
        return Some(QuantityModifier {
            quantity: 1,
            unit_price: captures.get(2).and_then(|m| parse_cents(m.as_str())),
            weight_text: Some(captures.get(1)?.as_str().to_string()),
            deal_price: None,
            pattern_type: QuantityPatternType::WeightAtPrice,
        });
    }

    if let Some(captures) = re_multi_for_price().captures(&normalized) {
        let quantity = captures.get(1)?.as_str().parse::<i32>().ok()?;
        let deal_price_cents = parse_cents(captures.get(2)?.as_str())?;
        return Some(QuantityModifier {
            quantity,
            unit_price: Some(Money::from_cents(
                deal_price_cents.cents() / i64::from(quantity),
            )),
            weight_text: None,
            deal_price: Some(deal_price_cents),
            pattern_type: QuantityPatternType::MultiForPrice,
        });
    }

    None
}

/// Whether a quantity row's own arithmetic proves the amount at its end is
/// that row's total rather than a unit price.
///
/// This is the reconciliation half of `has_trailing_total`. The regex half
/// cannot see a `$`-prefixed amount — T&T prints "0.428 kg @ $29.92/kg
/// W $12.81 G F" — and cannot be taught to without turning every
/// "6 @ $0.98" into a phantom item. Arithmetic separates the two cleanly:
/// 0.428 × 29.92 = 12.81 is the row's total, while 6 × 0.98 = 5.88 ≠ 0.98
/// says 0.98 was only the unit price.
///
/// The rate must be readable. `validate_quantity_price` gives an unreadable
/// one the benefit of the doubt (always-own-total), which is the right call
/// where it is used to *price* a row that was already going to be kept, but
/// here it would hand that benefit to every weight row on every receipt.
pub(super) fn qty_row_owns_trailing_total(line: &str) -> bool {
    let Some((price_cents, _, _)) = extract_trailing_price_cents(line) else {
        return false;
    };
    if price_cents <= Money::ZERO {
        return false;
    }
    parse_quantity_modifier(line)
        .map(|modifier| {
            // A rate equal to the trailing amount reconciles tautologically —
            // "1 @ $1.99  1.99" multiplies by one, and a 1.00 kg weight row
            // does the same. That row's amount is its unit price and its total
            // at once, so the arithmetic distinguishes nothing and the regex
            // half stays the only witness. Without this guard every "1 @ $X"
            // row qualifies and emits a phantom item named "1 @ $".
            modifier.unit_price.is_some_and(|unit| unit != price_cents)
                && validate_quantity_price(price_cents, &modifier)
        })
        .unwrap_or(false)
}
pub(super) fn validate_quantity_price(total_price: Money, modifier: &QuantityModifier) -> bool {
    let tolerance = 2i64;
    match modifier.pattern_type {
        QuantityPatternType::CountAtPrice => modifier
            .unit_price
            .map(|unit| {
                (unit.cents() * i64::from(modifier.quantity) - total_price.cents()).abs()
                    <= tolerance
            })
            .unwrap_or(false),
        QuantityPatternType::MultiForPrice => modifier
            .deal_price
            .map(|deal| (deal.cents() - total_price.cents()).abs() <= tolerance)
            .unwrap_or(false),
        QuantityPatternType::WeightAtPrice => {
            // When both the weight and the per-unit rate are readable, the
            // row's own total is weight × rate; a trailing price that doesn't
            // reconcile is another item's drifted price, not this row's total.
            // When either is unreadable, keep the historical benefit of the
            // doubt (always-own-total).
            let computed = modifier.unit_price.and_then(|unit| {
                modifier
                    .weight_text
                    .as_deref()
                    .and_then(|weight| weight.parse::<f64>().ok())
                    .map(|weight| (weight * unit.cents() as f64).round() as i64)
            });
            match computed {
                Some(own_total) => (own_total - total_price.cents()).abs() <= 3,
                None => true,
            }
        }
    }
}
pub(super) fn looks_like_quantity_expression(text: &str) -> bool {
    let normalized = normalize_decimal_spacing(text.trim());
    if normalized.is_empty() {
        return false;
    }

    if parse_quantity_modifier(&normalized).is_some() {
        return true;
    }

    // OCR-dropped `@`: lines like "2 $2.99" (qty + unit price, no `@`).
    // Without this, the line "2 $2.99 5.98" splits into desc_part "2 $2.99"
    // and trailing price 5.98, then the IF push emits a phantom item with
    // "2 $2.99" as the description — eating the real item name that sits on
    // the line above (Shepherds Purse 250g on fresh_140_18).
    static RE_QTY_UNIT_NO_AT: OnceLock<Regex> = OnceLock::new();
    let re_qty_unit_no_at =
        RE_QTY_UNIT_NO_AT.get_or_init(|| Regex::new(r"^\d+\s+\$\d+\.\d{2}\s*$").unwrap());
    if re_qty_unit_no_at.is_match(&normalized) {
        return true;
    }

    let upper = normalized.to_ascii_uppercase();
    if upper.starts_with('(') && upper.contains('@') && upper.contains("/$") {
        let alpha_count = upper.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
        if alpha_count <= 2 {
            return true;
        }
    }

    // Deal-subtext shape "(<size>)<@|0><unit>(<deal>)" where OCR read the `@`
    // as `0` (or kept it) and the parenthetical noise carries more letters
    // than the alpha caps allow: "(HEARH)03.99(1/$0.98)". The `)<price>(`
    // bridge plus the `/$` deal marker make the shape unambiguous.
    if upper.starts_with('(') && upper.contains("/$") {
        static RE_PRICE_BRIDGE: OnceLock<Regex> = OnceLock::new();
        let re_price_bridge =
            RE_PRICE_BRIDGE.get_or_init(|| Regex::new(r"\)\s*[0@]?\d+\.\d{2}\s*\(").unwrap());
        if re_price_bridge.is_match(&upper) {
            return true;
        }
    }

    if upper.contains('@') && upper.contains("/$") {
        let compact: String = upper
            .chars()
            .filter(|ch| !ch.is_ascii_whitespace())
            .collect();
        let alpha_count = compact
            .chars()
            .filter(|ch| ch.is_ascii_alphabetic())
            .count();
        let digit_count = compact.chars().filter(|ch| ch.is_ascii_digit()).count();
        if digit_count >= 3 && alpha_count <= 4 {
            return true;
        }
    }

    re_negative_unit_qty().is_match(&normalized)
        || Regex::new(r"(?i)^\d+\s*/\s*for\b")
            .unwrap()
            .is_match(&normalized)
        || re_compact_offer_fragment().is_match(&normalized)
        || re_compact_slash_deal().is_match(&normalized)
        || re_multi_for_price().is_match(&normalized)
        || re_parenthetical_offer_prefix().is_match(&normalized)
}
