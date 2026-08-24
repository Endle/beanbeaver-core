//! Text item extraction: pair each description with a price using the *line
//! text* only, for receipts whose word boxes give no usable amount column.
//!
//! [`extract_text_items`] at the bottom of this file is the entry point, and it
//! reads as six stages:
//!
//! 1. Receipt-level facts — [`grand_total_line`], [`total_price_cap`], and the
//!    price-drift verdict. All three bound what the per-row stages may decide.
//! 2. Rows the loop can answer without a price: [`orphan_qty_pairing`],
//!    [`drifted_price_pairing`], [`unpriced_line_outcome`].
//! 3. [`plan_price_line`] — what a trailing price means, or that it means
//!    nothing. Its answer is a [`PricePlan`], and everything after it acts on
//!    that plan rather than re-deciding.
//! 4. [`find_description`] — which row owns a price the row itself did not
//!    describe. Four walks; the order between them is the algorithm.
//! 5. [`resolve_deferred`] — replay the ordered outcomes, recovering malformed
//!    prices against the receipt's own summary amounts.
//! 6. [`drop_prices_above_cap`] — discard what the receipt total cannot support.
//!
//! Stages 2-4 read the rows through [`Lines`], which carries the shared borrow,
//! the claimed-row set, and the drift verdict together. Claiming a row is
//! always the *caller's* job: a stage returns which row it wants and never
//! marks it, which is what keeps the order of claims in one place.

use crate::money::Money;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use super::patterns::*;
use super::types::*;
use crate::common::ReceiptWarningKind;

pub(crate) fn normalize_decimal_spacing(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' && i > 0 && bytes[i - 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j > i + 1
                && j + 1 < bytes.len()
                && bytes[j].is_ascii_digit()
                && bytes[j + 1].is_ascii_digit()
                && (j + 2 == bytes.len() || !bytes[j + 2].is_ascii_digit())
            {
                out.push('.');
                out.push(bytes[j] as char);
                out.push(bytes[j + 1] as char);
                i = j + 2;
                continue;
            }
        }
        // OCR sometimes reads a price's decimal point as a comma ("0,99").
        // Only treat a comma as a decimal point when it sits directly between
        // a digit and exactly two fraction digits, so thousands separators
        // ("1,000") and prose ("Anytown, ON") are left untouched.
        if bytes[i] == b','
            && i > 0
            && bytes[i - 1].is_ascii_digit()
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && (i + 3 == bytes.len() || !bytes[i + 3].is_ascii_digit())
        {
            out.push('.');
            out.push(bytes[i + 1] as char);
            out.push(bytes[i + 2] as char);
            i += 3;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn parse_cents(token: &str) -> Option<Money> {
    let trimmed = token.trim();
    let (whole, frac) = trimmed.split_once('.')?;
    if whole.is_empty() || frac.len() != 2 {
        return None;
    }
    if !whole.chars().all(|ch| ch.is_ascii_digit()) || !frac.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let dollars = whole.parse::<i64>().ok()?;
    let cents = frac.parse::<i64>().ok()?;
    Some(Money::from_cents(dollars * 100 + cents))
}

fn alpha_ratio(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let alpha_count = value.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    alpha_count as f64 / value.len() as f64
}

fn strip_leading_receipt_codes(text: &str) -> String {
    let trimmed = text.trim();
    let trimmed = Regex::new(r"^\(\d+\)\s*").unwrap().replace(trimmed, "");
    let trimmed = Regex::new(r"^\d{6,}\s*")
        .unwrap()
        .replace(trimmed.as_ref(), "");
    trimmed.trim().to_string()
}

/// Strip the OCR-glued `<size>)@<unit>(<qty>/$<deal>)` sale-price subtext
/// that some receipts append to item descriptions.
fn strip_sale_price_subtext(text: &str) -> String {
    let stripped = re_sale_price_subtext().replace(text, "");
    re_size_paren_residue()
        .replace(&stripped, "")
        .trim()
        .to_string()
}

fn is_section_header_text(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let normalized = re_compact_space()
        .replace_all(&text.trim().to_ascii_uppercase(), " ")
        .to_string();
    if re_dept_marker_prefix().is_match(&normalized) {
        return true;
    }
    if matches!(
        normalized.as_str(),
        "MEAT" | "SEAFOOD" | "PRODUCE" | "DELI" | "GROCERY" | "BAKERY" | "FROZEN"
    ) {
        return true;
    }
    if re_section_header_with_aisle().is_match(&normalized) {
        return true;
    }
    if re_section_aisle_prefix().is_match(&normalized) {
        let tokens: HashSet<String> = re_ascii_words()
            .find_iter(&normalized)
            .map(|m| m.as_str().to_string())
            .collect();
        if tokens.iter().any(|token| {
            matches!(
                token.as_str(),
                "MEAT" | "SEAFOOD" | "PRODUCE" | "DELI" | "GROCERY" | "BAKERY" | "FROZEN" | "FOOD"
            )
        }) {
            return true;
        }
    }
    false
}

fn looks_like_summary_line(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let upper = text.trim().to_ascii_uppercase();
    // "Member Pricing" / "Manager's Special" / "Manager Special" rows on
    // Loblaws-family receipts are line-item discounts (negative price), not
    // membership/store-info metadata, so they must NOT match the
    // `^MEMBER\b` arm of re_summary_patterns. Without this carve-out the line
    // is filtered, the discount is dropped, and the items sum overshoots the
    // printed subtotal.
    if upper.starts_with("MEMBER PRICING")
        || upper.starts_with("MANAGER'S SPECIAL")
        || upper.starts_with("MANAGER SPECIAL")
    {
        return false;
    }
    if re_summary_patterns().is_match(&upper) {
        return true;
    }
    if upper.contains("SUBTOTAL") || upper.contains("SUB TOTAL") || upper.contains("TOTAL") {
        return true;
    }
    if re_total_ocr_variants().is_match(&upper) {
        return true;
    }
    if re_tax_tokens().is_match(&upper) {
        return true;
    }
    upper.starts_with("H=") && re_tax_tokens().is_match(&upper)
}

fn line_has_trailing_price(text: &str) -> bool {
    re_trailing_price().is_match(&normalize_decimal_spacing(text.trim()))
}

fn looks_like_onsale_marker(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let normalized = normalize_decimal_spacing(&text.to_ascii_uppercase());
    let without_price = re_trailing_price().replace(&normalized, "").to_string();
    let compact: String = without_price
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    Regex::new(r"(?:[A-Z0-9]{0,3})?ONSAL[E]?$")
        .unwrap()
        .is_match(&compact)
}

/// Bare counter labels that double as section-header words. As a standalone
/// line about to receive a price they are real items (Foody Mart's meat
/// counter prints "Meat" over a Chinese subtext per cut); as part of a
/// department banner ("&& 03-Meat") they are headers.
fn is_generic_counter_label(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_uppercase().as_str(),
        "MEAT" | "BAKERY" | "FROZEN"
    )
}

fn is_priced_generic_item_label(left_text: &str, full_text: &str) -> bool {
    !left_text.is_empty()
        && line_has_trailing_price(full_text)
        && is_generic_counter_label(left_text)
}

fn parse_quantity_modifier(line: &str) -> Option<QuantityModifier> {
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

fn validate_quantity_price(total_price: Money, modifier: &QuantityModifier) -> bool {
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

/// Count qty rows whose trailing price fails to reconcile as the row's own
/// qty×unit total — each is a witness that the price column drifted one row
/// up relative to the text column (see `price_drift` in
/// `extract_text_items`).
fn count_price_drift_evidence(lines: &[String]) -> usize {
    lines
        .iter()
        .filter(|line| {
            let line = line.trim();
            // A section header carrying a trailing price is the strongest
            // witness: straight receipts never price their "&& <Dept>" rows,
            // so the amount can only be the first item's, drifted up. Bare
            // counter labels ("Meat 4.19") are items, not headers.
            if let Some((cents, _, price_start)) = extract_trailing_price_cents(line) {
                let head = line[..price_start].trim();
                if cents > Money::ZERO
                    && !head.is_empty()
                    && is_section_header_text(head)
                    && !is_generic_counter_label(head)
                {
                    return true;
                }
            }
            if !looks_like_quantity_expression(line) {
                return false;
            }
            let prices: Vec<Money> = re_find_prices()
                .captures_iter(line)
                .filter_map(|caps| caps.get(1).and_then(|m| parse_cents(m.as_str())))
                .collect();
            if prices.len() < 2 {
                return false;
            }
            let trailing = extract_trailing_price_cents(line).map(|(c, _, _)| c);
            if trailing != prices.last().copied() {
                return false;
            }
            let Some(orphan) = trailing else {
                return false;
            };
            orphan > Money::ZERO
                && !parse_quantity_modifier(line)
                    .map(|modifier| validate_quantity_price(orphan, &modifier))
                    .unwrap_or(false)
        })
        .count()
}

/// True when the nearest description-like line above `i` has already been
/// consumed by an earlier pairing. Under receipt-level drift this decides a
/// row's donation direction: a qty/paren row below an *unclaimed* description
/// still owes it its price ("Broccoli (Crowns)" / "0.41 lb @ $1.98/lb 0.81"),
/// while one below an already-priced description carries the NEXT item's
/// drifted price ("Pork Lard" priced from above, so "(3 380g) 2.98" belongs
/// to Pak Fok below).
fn nearest_desc_above_consumed(
    normalized_lines: &[String],
    used_text_lines: &[bool],
    i: usize,
) -> bool {
    for j in (i.saturating_sub(3)..i).rev() {
        let prev = normalized_lines[j].trim();
        if prev.is_empty()
            || looks_like_quantity_expression(prev)
            || re_parenthetical_only().is_match(prev)
            || re_skip_patterns().is_match(prev)
        {
            continue;
        }
        // A priced junk row ("(WRER)  10.04" — mangled subtext carrying a
        // price) is not the description; the walk continues to the real
        // name above it.
        if let Some((_, _, price_start)) = extract_trailing_price_cents(prev) {
            let head = prev[..price_start].trim();
            if head.starts_with('(') || alpha_ratio(head) < 0.4 {
                continue;
            }
        }
        if used_text_lines[j] {
            return true;
        }
        // An unclaimed priceless row directly under a used, priced
        // description is that item's multi-line name continuation
        // ("Natrel  4.98" / "1 - 2% Partly Skimme"), not a new item — keep
        // walking. Any other unclaimed description owns the row below it.
        let is_continuation = j > 0 && used_text_lines[j - 1] && {
            let above = normalized_lines[j - 1].trim();
            match extract_trailing_price_cents(above) {
                Some((_, _, price_start)) => {
                    let head = above[..price_start].trim();
                    !head.starts_with('(') && alpha_ratio(head) >= 0.4
                }
                None => false,
            }
        };
        if !is_continuation {
            return false;
        }
    }
    false
}

fn looks_like_quantity_expression(text: &str) -> bool {
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

pub(crate) fn extract_trailing_price_cents(line: &str) -> Option<(Money, bool, usize)> {
    let captures = re_trailing_price().captures(line)?;
    let cents = parse_cents(captures.get(1)?.as_str())?;
    let trailing_minus = captures.get(2).map(|m| m.as_str() == "-").unwrap_or(false);
    let start = captures.get(1)?.start();
    // Leading-minus discount convention (e.g. Asian-grocery lines like
    // "D9 -$1.96"): a '-' glued to the price — directly or through a '$' —
    // marks a discount, complementing Costco's trailing-minus form. Require
    // the '-' to sit at a token boundary or against a '$' so mid-token
    // hyphens ("ITEM-1.96") and " - 1.96" separators are not mis-signed.
    let leading_minus = {
        let prefix = &line[..start];
        let had_dollar = prefix.ends_with('$');
        let stripped = prefix.strip_suffix('$').unwrap_or(prefix);
        match stripped.strip_suffix('-') {
            Some(rest) => had_dollar || rest.is_empty() || rest.ends_with(char::is_whitespace),
            None => false,
        }
    };
    let is_discount = trailing_minus || leading_minus;
    Some((if is_discount { -cents } else { cents }, is_discount, start))
}

fn is_descriptive_candidate(text: &str) -> bool {
    if text.is_empty() || text.len() <= 2 {
        return false;
    }
    if re_skip_patterns().is_match(text) {
        return false;
    }
    if looks_like_summary_line(text) {
        return false;
    }
    if re_mangled_reg_marker().is_match(text.trim()) {
        return false;
    }
    if looks_like_quantity_expression(text) {
        return false;
    }
    if re_trailing_price().is_match(text) {
        return false;
    }
    if re_standalone_price_line().is_match(text) {
        return false;
    }
    if re_long_digits_line().is_match(text) {
        return false;
    }
    let cleaned = strip_leading_receipt_codes(text);
    if cleaned.is_empty() {
        return false;
    }
    if looks_like_onsale_marker(&cleaned) {
        return false;
    }
    if is_section_header_text(&cleaned) {
        return false;
    }
    alpha_ratio(&cleaned) >= 0.4
}

fn merge_description_context(lines: &[String], base: &str, source_idx: usize) -> String {
    let mut merged = base.trim().to_string();
    if source_idx > 0 {
        let prev_line = lines[source_idx - 1].trim();
        let prev_clean = strip_leading_receipt_codes(prev_line);
        if !prev_clean.is_empty()
            && prev_clean.ends_with('-')
            && is_descriptive_candidate(prev_line)
        {
            merged = format!("{prev_clean} {merged}").trim().to_string();
        }
    }
    if source_idx + 1 < lines.len() {
        let next_line = lines[source_idx + 1].trim();
        let next_clean = strip_leading_receipt_codes(next_line);
        if !next_clean.is_empty() && merged.ends_with('-') && is_descriptive_candidate(next_line) {
            merged = format!("{merged} {next_clean}").trim().to_string();
        }
    }
    re_compact_space().replace_all(&merged, " ").to_string()
}

fn is_weak_inline_description(text: &str) -> bool {
    let stripped = text.trim();
    if stripped.is_empty() {
        return false;
    }
    re_weak_parenthetical().is_match(stripped) || re_weak_measure().is_match(stripped)
}

fn maybe_push_warning(
    warnings: &mut Vec<TextParserWarning>,
    items_len: usize,
    kind: ReceiptWarningKind,
    message: String,
) {
    warnings.push(TextParserWarning {
        kind,
        message,
        after_item_index: if items_len > 0 {
            Some(items_len - 1)
        } else {
            None
        },
    });
}

fn truncated_context(line: &str) -> String {
    // Truncate to 80 *characters* (matching Python's `[:80]`); a byte-index
    // `truncate(80)` panics when byte 80 lands inside a multibyte char (e.g.
    // CJK text on Asian-grocery receipts).
    let trimmed = line.trim();
    match trimmed.char_indices().nth(80) {
        Some((byte_idx, _)) => trimmed[..byte_idx].to_string(),
        None => trimmed.to_string(),
    }
}

fn extract_trailing_noisy_price(line: &str) -> Option<(String, String, i64, usize)> {
    let captures = re_trailing_noisy_price()
        .captures(line)
        .or_else(|| re_trailing_letter_fraction_price().captures(line))?;
    let whole = captures.get(1)?.as_str().to_string();
    let fraction = captures.get(2)?.as_str().to_string();
    let whole_dollars = whole.parse::<i64>().ok()?;
    let start = captures.get(1)?.start();
    Some((
        format!("{whole}.{fraction}"),
        fraction,
        whole_dollars,
        start,
    ))
}

fn build_malformed_price_candidate(line: &str) -> Option<MalformedTrailingPriceCandidate> {
    let line_upper = line.to_ascii_uppercase();
    if line_upper.contains("TOTAL")
        || line_upper.contains("SUBTOTAL")
        || line_upper.contains("SUB TOTAL")
        || re_tax_tokens().is_match(&line_upper)
    {
        return None;
    }

    let (observed_token, observed_fraction, whole_dollars, price_start) =
        extract_trailing_noisy_price(line)?;
    let desc_part = line[..price_start].trim();
    if desc_part.is_empty() {
        return None;
    }

    let cleaned = strip_leading_receipt_codes(desc_part);
    if cleaned.is_empty()
        || cleaned.len() <= 2
        || looks_like_summary_line(&cleaned)
        || looks_like_quantity_expression(&cleaned)
        || is_section_header_text(&cleaned)
        || alpha_ratio(&cleaned) < 0.4
    {
        return None;
    }

    Some(MalformedTrailingPriceCandidate {
        description: cleaned.clone(),
        category_source: cleaned,
        observed_token,
        observed_fraction,
        whole_dollars,
        context: truncated_context(line),
    })
}

fn levenshtein_distance(left: &str, right: &str) -> usize {
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    let mut prev = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut curr = vec![0; right_chars.len() + 1];

    for (i, left_char) in left_chars.iter().enumerate() {
        curr[0] = i + 1;
        for (j, right_char) in right_chars.iter().enumerate() {
            let substitution_cost = usize::from(left_char != right_char);
            curr[j + 1] = (prev[j + 1] + 1)
                .min(curr[j] + 1)
                .min(prev[j] + substitution_cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[right_chars.len()]
}

fn malformed_candidate_price_options(
    candidate: &MalformedTrailingPriceCandidate,
) -> Vec<CandidatePriceOption> {
    let mut best_by_price: HashMap<Money, usize> = HashMap::new();

    for cents in 0..=99i64 {
        let fraction = format!("{cents:02}");
        let score = levenshtein_distance(&candidate.observed_fraction, &fraction);
        if score > 2 {
            continue;
        }
        let price = Money::from_cents(candidate.whole_dollars * 100 + cents);
        best_by_price
            .entry(price)
            .and_modify(|best_score| *best_score = (*best_score).min(score))
            .or_insert(score);
    }

    let mut options = best_by_price
        .into_iter()
        .map(|(price, score)| CandidatePriceOption { price, score })
        .collect::<Vec<_>>();
    options.sort_by_key(|option| (option.score, option.price));
    options
}

fn reconcile_malformed_price_candidates(
    regular_total_cents: Money,
    summary_amounts: &HashSet<Money>,
    candidates: &[MalformedTrailingPriceCandidate],
) -> Option<ReconciledMalformedPrices> {
    if candidates.is_empty() {
        return None;
    }

    let mut results = Vec::new();
    let mut targets = summary_amounts
        .iter()
        .copied()
        .filter(|amount| *amount >= regular_total_cents)
        .collect::<Vec<_>>();
    targets.sort_unstable();
    targets.dedup();

    for target in targets {
        let mut states = HashMap::new();
        states.insert(
            regular_total_cents,
            ReconciliationState {
                score: 0,
                prices: Vec::new(),
                ambiguous: false,
            },
        );

        let mut failed_target = false;
        for candidate in candidates {
            let options = malformed_candidate_price_options(candidate);
            if options.is_empty() {
                failed_target = true;
                break;
            }

            let mut next_states: HashMap<Money, ReconciliationState> = HashMap::new();
            for (running_total, state) in &states {
                for option in &options {
                    let next_total = *running_total + option.price;
                    if next_total > target {
                        continue;
                    }
                    let next_score = state.score + option.score;
                    let mut next_prices = state.prices.clone();
                    next_prices.push(option.price);

                    match next_states.get_mut(&next_total) {
                        Some(existing) => {
                            if next_score < existing.score {
                                *existing = ReconciliationState {
                                    score: next_score,
                                    prices: next_prices,
                                    ambiguous: state.ambiguous,
                                };
                            } else if next_score == existing.score
                                && (existing.prices != next_prices
                                    || existing.ambiguous
                                    || state.ambiguous)
                            {
                                existing.ambiguous = true;
                            }
                        }
                        None => {
                            next_states.insert(
                                next_total,
                                ReconciliationState {
                                    score: next_score,
                                    prices: next_prices,
                                    ambiguous: state.ambiguous,
                                },
                            );
                        }
                    }
                }
            }
            states = next_states;
            if states.is_empty() {
                failed_target = true;
                break;
            }
        }

        if failed_target {
            continue;
        }

        let Some(state) = states.get(&target) else {
            continue;
        };
        if state.ambiguous {
            continue;
        }
        results.push((state.score, state.prices.clone()));
    }

    results.sort_by_key(|(score, prices)| (*score, prices.clone()));
    let (best_score, best_prices) = results.first()?.clone();
    if results
        .iter()
        .skip(1)
        .any(|(score, prices)| *score == best_score && *prices != best_prices)
    {
        return None;
    }

    Some(ReconciledMalformedPrices {
        prices: best_prices,
    })
}

/// Stage 5 — resolve the deferred stream into items and warnings.
///
/// The loop above cannot emit directly: a malformed price is only recoverable
/// once every *well-formed* item is known, because the recovery is a
/// reconciliation against the receipt's own summary amounts. So the loop
/// records outcomes in order and this stage replays them, substituting a
/// recovered price where one was found and a warning where none was.
///
/// Order is what makes `after_item_index` meaningful, so the replay must stay
/// a single pass over `deferred` in the order the loop pushed it.
fn resolve_deferred(
    deferred: Vec<DeferredTextOutcome>,
    summary_amounts: &HashSet<Money>,
) -> (Vec<ParsedTextItem>, Vec<TextParserWarning>) {
    let regular_total_cents = deferred
        .iter()
        .filter_map(|outcome| match outcome {
            DeferredTextOutcome::Item(item) => Some(item.price),
            _ => None,
        })
        .sum();
    let malformed_candidates = deferred
        .iter()
        .filter_map(|outcome| match outcome {
            DeferredTextOutcome::Malformed(candidate) => Some(candidate.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let reconciled = reconcile_malformed_price_candidates(
        regular_total_cents,
        summary_amounts,
        &malformed_candidates,
    )
    .map(|resolved| resolved.prices.into_iter());

    let mut malformed_prices = reconciled;
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    for outcome in deferred {
        match outcome {
            DeferredTextOutcome::Item(item) => items.push(item),
            DeferredTextOutcome::Warning(kind, message) => {
                maybe_push_warning(&mut warnings, items.len(), kind, message);
            }
            DeferredTextOutcome::Malformed(candidate) => {
                if let Some(recovered_price_cents) =
                    malformed_prices.as_mut().and_then(|prices| prices.next())
                {
                    items.push(ParsedTextItem {
                        description: candidate.description.clone(),
                        category_source: candidate.category_source.clone(),
                        price: recovered_price_cents,
                        quantity: 1,
                    });
                    maybe_push_warning(
                        &mut warnings,
                        items.len(),
                        ReceiptWarningKind::PriceAutoCorrected,
                        format!(
                            "auto-corrected malformed OCR price \"{}\" -> \"{}\" using summary reconciliation",
                            candidate.observed_token,
                            recovered_price_cents,
                        ),
                    );
                } else {
                    maybe_push_warning(
                        &mut warnings,
                        items.len(),
                        ReceiptWarningKind::PossibleMissedItem,
                        format!(
                            "maybe missed item with malformed OCR price \"{}\" (context: \"{}\")",
                            candidate.observed_token, candidate.context
                        ),
                    );
                }
            }
        }
    }

    (items, warnings)
}

/// Stage 6 — drop item prices above what the receipt itself can support.
///
/// A single positive line item can never exceed the receipt total plus the
/// discounts that reduced it, so a price above that ceiling is an OCR artifact
/// ("$1.58" misread as "81.58"). Dropping it with a warning is the "prefer
/// missing items over wrong pairings" rule: a wrong price corrupts the ledger,
/// a missing one is visible.
fn drop_prices_above_cap(
    items: Vec<ParsedTextItem>,
    warnings: &mut Vec<TextParserWarning>,
    cap_base: Money,
) -> Vec<ParsedTextItem> {
    let discount_sum: Money = items
        .iter()
        .filter(|it| it.price < Money::ZERO)
        .map(|it| -it.price)
        .sum();
    let cap = cap_base + discount_sum;
    let mut kept = Vec::with_capacity(items.len());
    for it in items.into_iter() {
        if it.price > cap {
            maybe_push_warning(
                warnings,
                kept.len(),
                ReceiptWarningKind::DroppedImplausiblePrice,
                format!(
                    "dropped implausible item price \"{}\" exceeding receipt total (context: \"{}\")",
                    it.price, it.description,
                ),
            );
        } else {
            kept.push(it);
        }
    }
    kept
}

/// The grand-total row, which is where the item region ends.
///
/// Every exclusion here is a row that says TOTAL without being the total. The
/// column-header case ("DESCRIPTION QTY UNIT TOTAL") is the dangerous one: read
/// as the grand total it sits *above* the items, so treating it as the total
/// truncates the whole item region.
fn grand_total_line(lines: &[String]) -> Option<usize> {
    lines.iter().position(|line| {
        let upper = line.to_ascii_uppercase();
        re_total_word().is_match(line)
            && !upper.contains("SUBTOTAL")
            && !upper.contains("TOTAL NUMBER")
            && !upper.contains("TOTAL DISCOUNT")
            && !upper.contains("TOTAL ITEMS")
            && !upper.contains("TOTAL SAVINGS")
            && !upper.contains("TOTAL SAVED")
            // A column-header row ("DESCRIPTION QTY UNIT TOTAL") is not the
            // grand total; treating it as one truncates the whole item region.
            && !upper.contains("QTY")
            && !upper.contains("DESCRIPTION")
    })
}

/// Authoritative receipt total, when a grand-total line carries a price.
///
/// Used as a sanity ceiling on individual item prices: a single positive line
/// item can never exceed (total + sum of discounts), so a price above that
/// ceiling is an OCR artifact (e.g. "$1.58" misread as "81.58") and is dropped
/// rather than mis-paired — "prefer missing items over wrong pairings". Taken
/// as the max over genuine grand-total lines (not the first match) so sub-lines
/// like "TOTAL TAX" never stand in for the total.
///
/// The exclusion list differs from [`grand_total_line`]'s on purpose: this one
/// is looking for an *amount* to bound by, so it also rejects TOTAL TAX and
/// TOTAL POINTS and accepts a card tender, while that one is looking for where
/// the items stop.
fn total_price_cap(lines: &[String]) -> Option<Money> {
    lines
        .iter()
        .filter(|line| {
            let upper = line.to_ascii_uppercase();
            let is_total_line = re_total_word().is_match(line)
                && !upper.contains("SUBTOTAL")
                && !upper.contains("TOTAL TAX")
                && !upper.contains("TOTAL NUMBER")
                && !upper.contains("TOTAL DISCOUNT")
                && !upper.contains("TOTAL ITEMS")
                && !upper.contains("TOTAL SAVINGS")
                && !upper.contains("TOTAL SAVED")
                && !upper.contains("TOTAL POINTS");
            // A card tender is an equally valid ceiling: the charge is never
            // less than any single item. This keeps the cap honest when the
            // TOTAL row picked up a neighbouring smaller amount during line
            // grouping (Pharmasave's "TOTAL $1.40" carrying the HST).
            is_total_line || re_tender_label().is_match(line)
        })
        .filter_map(|line| extract_trailing_price_cents(line).map(|(c, _, _)| c))
        .filter(|c| *c > Money::ZERO)
        .max()
}

/// A `N/for` multi-buy row whose total OCR mangled into a token carrying
/// letters ("3/for 5.0O"). The amount is unrecoverable — there is no second
/// reading of it anywhere on the receipt — so the only honest output is to say
/// an item may have been missed here.
///
/// Two arms of the loop reach this, and they used to carry a copy each: a
/// `/for` row is a quantity expression whether or not its mangled tail happened
/// to parse as a trailing price, so both the qty-row arm and the no-price tail
/// need the same answer.
fn multi_buy_tail_warning(line: &str) -> Option<DeferredTextOutcome> {
    if !line.to_ascii_lowercase().contains("/for") {
        return None;
    }
    let tail_token = re_tail_token()
        .captures(line)
        .and_then(|captures| captures.get(1).map(|m| m.as_str().to_string()))?;
    if !tail_token.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    let context = truncated_context(line);
    Some(DeferredTextOutcome::Warning(
        ReceiptWarningKind::PossibleMissedItem,
        format!(
            "maybe missed item near malformed multi-buy total \"{tail_token}\" (context: \"{context}\")"
        ),
    ))
}

/// What a row carrying no well-formed trailing price is worth, if anything.
///
/// Three alternatives in priority order: a price mangled *recoverably* (the
/// summary reconciliation in [`reconcile_malformed_price_candidates`] can put
/// it back), a price mangled beyond recovery, and a multi-buy row whose total
/// was eaten. Everything else a receipt prints — addresses, phone numbers,
/// loyalty text — is silence, which is why falling through returns `None`
/// rather than a warning.
fn unpriced_line_outcome(line: &str) -> Option<DeferredTextOutcome> {
    if let Some(candidate) = build_malformed_price_candidate(line) {
        return Some(DeferredTextOutcome::Malformed(candidate));
    }
    if let Some(captures) = re_malformed_ocr_price().captures(line) {
        let token = captures.get(1).map(|m| m.as_str()).unwrap_or("");
        let context = truncated_context(line);
        return Some(DeferredTextOutcome::Warning(
            ReceiptWarningKind::PossibleMissedItem,
            format!(
                "maybe missed item with malformed OCR price \"{token}\" (context: \"{context}\")"
            ),
        ));
    }
    multi_buy_tail_warning(line)
}

/// The rows one pass over a receipt reads, plus the receipt-level verdict that
/// changes how it reads them.
///
/// Every stage below wants all three at once: which rows exist, which of them
/// an earlier price already claimed, and whether the right-hand price column
/// drifted a row up. Bundling them is what keeps the extracted stages under
/// `too_many_arguments` without an `#[allow]`, and it is honest rather than
/// convenient — a stage that consults one of these consults all of them.
///
/// `used` is a shared borrow on purpose. Claiming a row is the caller's job, so
/// a stage returns *which* row it claimed and never marks it: that keeps the
/// order of claims in one place, which is what the cross-row-leak guards
/// (bugs C, H, K) depend on.
#[derive(Clone, Copy)]
struct Lines<'a> {
    all: &'a [String],
    used: &'a [bool],
    drift: bool,
}

impl<'a> Lines<'a> {
    fn of(all: &'a [String], used: &'a [bool], drift: bool) -> Self {
        Lines { all, used, drift }
    }
}

/// A description row claimed by a price that was printed somewhere else.
struct OrphanQtyPairing {
    item: ParsedTextItem,
    /// The row the price was paired with; the caller marks it used.
    description_line: usize,
}

/// OCR column-merge recovery: a quantity line can absorb the NEXT item's price
/// into its own text row.
///
/// FreshCo prints "2 @ 1/ $12.98 $11.19 C", where $11.19 is the price of the
/// price-less "Natrel Milk 2% 4L" line below — the right-hand price column
/// drifted up one row in OCR reading order. When a qty line carries a trailing
/// price that does NOT reconcile as its own line total (qty × unit) and the
/// next line is a bare, price-less description, that orphan price belongs to
/// that description. The qty line itself is still consumed later as a modifier
/// of the item above.
fn orphan_qty_pairing(index: usize, line: &str, rows: Lines<'_>) -> Option<OrphanQtyPairing> {
    let prices: Vec<Money> = re_find_prices()
        .captures_iter(line)
        .filter_map(|caps| caps.get(1).and_then(|m| parse_cents(m.as_str())))
        .collect();
    // The orphan must be a genuine trailing price, not the tail of a
    // parenthetical deal ("(2/$3.50)" ends the coriander line; that 3.50 is
    // subtext, not a drifted amount).
    let trailing = extract_trailing_price_cents(line).map(|(c, _, _)| c);
    if prices.len() < 2 || trailing != prices.last().copied() {
        return None;
    }
    let orphan_cents = *prices.last().unwrap();
    let reconciles_as_own_total = parse_quantity_modifier(line)
        .map(|modifier| validate_quantity_price(orphan_cents, &modifier))
        .unwrap_or(false);
    // The downward pairing is only valid when the description above is already
    // priced, so this row has nothing left to donate upward. An unclaimed
    // description above keeps the trailing price as its own — whether the row
    // reconciles ("Broccoli (Crowns)" / "0.41 lb @ $1.98/lb  0.81") or not
    // ("HLY - Potato Chips Honey" / "(...)@3.99(1/$0.98)  5.88H", where 5.88 is
    // Honey's own price on its deal-subtext row). Under receipt-level drift even
    // a coincidentally-reconciling echo ("1 @ $1.99  1.99" where the next item
    // also costs 1.99) is the next item's price.
    let above_consumed = nearest_desc_above_consumed(rows.all, rows.used, index);
    let echo_is_drifted = rows.drift && above_consumed;
    if orphan_cents <= Money::ZERO
        || !above_consumed
        || (reconciles_as_own_total && !echo_is_drifted)
    {
        return None;
    }

    let next_idx = orphan_description_row(index, rows)?;
    let next_trimmed = rows.all[next_idx].trim();
    // Skip a line an earlier price's search already consumed — that is the
    // cross-row leak. A bare counter label ("Meat" above its mangled Chinese
    // subtext) is a real item here: the orphan price arriving from this qty row
    // is what prices it, and `is_priced_generic_item_label`'s trailing-price
    // requirement cannot see a price delivered from above.
    if rows.used[next_idx]
        || !(is_descriptive_candidate(next_trimmed) || is_generic_counter_label(next_trimmed))
    {
        return None;
    }
    let desc = strip_sale_price_subtext(&strip_leading_receipt_codes(next_trimmed));
    Some(OrphanQtyPairing {
        item: ParsedTextItem {
            category_source: desc.clone(),
            description: desc,
            price: orphan_cents,
            quantity: 1,
        },
        description_line: next_idx,
    })
}

/// The row an orphan price found on a qty line is describing.
///
/// It can sit a row or two further down when the block carries its own qty row:
/// Foody Mart prints "(<size>)@<unit>(<deal>) <orphan>" / "1 @ $2.99" /
/// "<item name>". Skip qty rows; never leap over a priced row, which is another
/// item's territory.
fn orphan_description_row(index: usize, rows: Lines<'_>) -> Option<usize> {
    for j in (index + 1)..rows.all.len().min(index + 4) {
        let candidate = rows.all[j].trim();
        if candidate.is_empty() || looks_like_quantity_expression(candidate) {
            continue;
        }
        if line_has_trailing_price(candidate) {
            return None;
        }
        return Some(j);
    }
    None
}

/// Where a trailing price goes when the row printing it is already spoken for.
///
/// Only reachable under established drift: an unclaimed row's own trailing
/// price is its own. Stops at the first priced row rather than leaping over it,
/// because that row is the next item's territory.
fn drifted_price_pairing(
    index: usize,
    price_cents: Money,
    rows: Lines<'_>,
) -> Option<OrphanQtyPairing> {
    for j in (index + 1)..rows.all.len().min(index + 4) {
        if rows.used[j] {
            return None;
        }
        let candidate = rows.all[j].trim();
        if line_has_trailing_price(candidate) {
            return None;
        }
        if candidate.is_empty()
            || looks_like_quantity_expression(candidate)
            || !(is_descriptive_candidate(candidate) || is_generic_counter_label(candidate))
        {
            continue;
        }
        let desc = strip_sale_price_subtext(&strip_leading_receipt_codes(candidate));
        return Some(OrphanQtyPairing {
            item: ParsedTextItem {
                category_source: desc.clone(),
                description: desc,
                price: price_cents,
                quantity: 1,
            },
            description_line: j,
        });
    }
    None
}

/// What the flag cascade decided about one row's trailing price.
///
/// These eight used to be mutable locals threaded through 200 lines of rules,
/// and that is what made the region hard to read: each rule could still change
/// what an earlier one had set, so no line of it could be understood without
/// the whole. Naming the result makes the boundary explicit — everything above
/// this struct *decides*, everything below it *acts*.
///
/// `desc_part` is part of the decision, not an input: three of the rules clear
/// it (a priced section header donates its price downward; a malformed price
/// marker keeps only the price), which is how "this row has no usable
/// description of its own" reaches the search stage.
struct PricePlan {
    /// The description text left of the price, after the rules had their say.
    desc_part: String,
    /// Look below the price for its description before looking above.
    prefer_forward_desc: bool,
    /// If the forward search finds nothing, drop the price rather than
    /// back-walking — the row above is somebody else's.
    skip_if_no_forward_desc: bool,
    /// The inline description is not usable; walk back for a real one.
    force_backward: bool,
    /// The inline text is a size/weight fragment ("(1kg)") that belongs
    /// *appended* to whatever description the search finds, not discarded.
    weak_inline_desc: bool,
    /// A parenthetical subtext row under established drift, whose price is the
    /// item below's. Distinct from `prefer_forward_desc` because it also stops
    /// the forward walk at the first priced row.
    drift_paren_forward: bool,
    /// A department banner carrying a price ("&& 01-Grocery  5.59"), whose
    /// price belongs to the first real item below it.
    is_priced_section_header: bool,
    /// The inline text is a quantity expression, not a description.
    is_qty_expr: bool,
}

/// Stage 3 — decide how one trailing price should be paired, or that it should
/// not be.
///
/// `None` means the row is not an item price at all: a summary row, a
/// suggested-retail REG marker, a ghost promo artifact, a quantity stub, or a
/// section header whose price the item below repeats. Every one of those used
/// to be a bare `continue` in the middle of the loop.
fn plan_price_line(
    index: usize,
    line: &str,
    price_start: usize,
    price_cents: Money,
    summary_amounts: &HashSet<Money>,
    rows: Lines<'_>,
) -> Option<PricePlan> {
    let line_upper = line.to_ascii_uppercase();
    let mut desc_part = line[..price_start].trim().to_string();
    let compact_line = re_compact_space().replace_all(&line_upper, "").to_string();
    let mut prefer_forward_desc = false;
    let mut skip_if_no_forward_desc = false;
    let previous_line = || rows.all[index - 1].as_str();

    let has_reg_marker = has_reg_price_marker(&line_upper);

    if has_reg_marker {
        if is_suggested_retail_row(line, &desc_part) {
            return None;
        }
        // Two prices on a REG row means one of them is the real one; with the
        // row above already priced, the item this row prices is below it.
        if re_find_prices().find_iter(line).count() > 1
            && index > 0
            && re_trailing_price().is_match(previous_line())
        {
            prefer_forward_desc = true;
            skip_if_no_forward_desc = true;
        }
    }

    if is_ghost_promo_row(&line_upper, &compact_line, &desc_part, index, rows) {
        return None;
    }
    if price_row_is_summary(index, &line_upper, price_cents, summary_amounts, rows) {
        return None;
    }

    let weak_inline_desc = is_weak_inline_description(&desc_part);
    let mut force_backward =
        line_upper.contains("REG$") || line_upper.contains("@REG") || weak_inline_desc;
    // Under receipt-level drift a paren-subtext row's trailing price belongs to
    // the item BELOW when the description above is already priced ("Pork Lard"
    // claimed from its qty row, so "(3 380g) 2.98" is Pak Fok's) — search
    // forward, stopping at the first priced row rather than leaping over it.
    // When the description above is still unclaimed, the price is its own
    // ("Fresh Chicken Wings" / "(WRER)  10.04") and the backward walk is right.
    let drift_paren_forward = rows.drift
        && desc_part.trim_start().starts_with('(')
        && nearest_desc_above_consumed(rows.all, rows.used, index);
    if drift_paren_forward {
        prefer_forward_desc = true;
    }
    if has_reg_marker
        && force_backward
        && index > 0
        && !previous_line().trim().is_empty()
        && line_has_trailing_price(previous_line().trim())
        && desc_part.starts_with('(')
    {
        prefer_forward_desc = true;
    }

    if !desc_part.is_empty() {
        desc_part = Regex::new(r"^\d{8,}\s*")
            .unwrap()
            .replace(&desc_part, "")
            .to_string();
    }
    let is_onsale_marker_desc = looks_like_onsale_marker(&desc_part);
    if is_onsale_marker_desc {
        prefer_forward_desc = true;
        if index > 0 && line_has_trailing_price(previous_line().trim()) {
            skip_if_no_forward_desc = true;
        }
    }

    let is_priced_section_header = !desc_part.is_empty()
        && is_section_header_text(&desc_part)
        && !is_priced_generic_item_label(&desc_part, line);
    if is_priced_section_header {
        desc_part.clear();
        if section_header_price_is_repeated(index, price_cents, rows) {
            return None;
        }
    }

    let is_malformed_price_marker = is_bare_price_marker(&desc_part);
    let is_quantity_stub = re_malformed_price_marker().is_match(&desc_part);
    let mut is_qty_expr = if !desc_part.is_empty() {
        looks_like_quantity_expression(&desc_part)
            || re_onsale_parenthetical().is_match(&desc_part)
            || is_onsale_marker_desc
    } else {
        false
    };

    if is_malformed_price_marker {
        if !malformed_marker_is_multi_buy(index, rows) {
            return None;
        }
        force_backward = true;
        desc_part.clear();
        is_qty_expr = false;
    }
    if is_quantity_stub {
        return None;
    }

    // A mangled REG-price marker (OCR ate the leading R, so "REG$15.99" became
    // "#EG15.99" or "(EG$5.99") means the trailing price is the suggested-retail
    // amount, not an item price. The inline branch already filters these via
    // `re_mangled_reg_marker`, but the search branch would back-walk and emit a
    // phantom item paired with the previous line, so suppress the whole row.
    if !desc_part.is_empty() && re_mangled_reg_marker().is_match(desc_part.trim()) {
        // Under drift with the description above already priced, the REG amount
        // lives inside the marker itself and the trailing price is the NEXT
        // item's ("(-EG4.99  2.99" above "LKS Dried Cod Fish Slice") — forward
        // it. In every other shape it is suggested retail, so keep suppressing.
        if !drift_paren_forward {
            return None;
        }
        skip_if_no_forward_desc = true;
    }

    Some(PricePlan {
        desc_part,
        prefer_forward_desc,
        skip_if_no_forward_desc,
        force_backward,
        weak_inline_desc,
        drift_paren_forward,
        is_priced_section_header,
        is_qty_expr,
    })
}

/// Whether the item below a priced department banner repeats the banner's own
/// price — in which case the banner is echoing the item, not pricing it, and
/// counting both would double the item.
///
/// Only the first non-blank row below is consulted: past it the price belongs
/// to some other item, and a summary row ends the question outright.
fn section_header_price_is_repeated(index: usize, price_cents: Money, rows: Lines<'_>) -> bool {
    for j in (index + 1)..rows.all.len().min(index + 4) {
        let next_line = rows.all[j].trim();
        if next_line.is_empty() {
            continue;
        }
        if looks_like_summary_line(next_line) {
            return false;
        }
        return extract_trailing_price_cents(next_line)
            .is_some_and(|(next_price, _, _)| next_price == price_cents);
    }
    false
}

impl PricePlan {
    /// Whether the row's own text is the description, so no search is needed.
    ///
    /// This is the common case and the cheap one — everything below exists for
    /// the rows where it is false.
    fn describes_itself(&self) -> bool {
        !self.desc_part.is_empty()
            && self.desc_part.len() > 2
            && !self.is_qty_expr
            && !self.force_backward
            && !self.drift_paren_forward
            && !looks_like_summary_line(self.desc_part.trim())
    }
}

/// The item a row that describes itself yields.
fn inline_item(plan: &PricePlan, price_cents: Money) -> ParsedTextItem {
    let desc_clean = strip_sale_price_subtext(&plan.desc_part);
    let desc_clean = re_embedded_unit_price_suffix()
        .replace(&desc_clean, "")
        .trim()
        .to_string();
    ParsedTextItem {
        description: desc_clean.clone(),
        category_source: desc_clean,
        price: price_cents,
        quantity: 1,
    }
}

/// A price with no row willing to own it.
fn unowned_price_warning(line: &str, price_cents: Money) -> DeferredTextOutcome {
    let mut message = format!("maybe missed item near price {}", price_cents);
    let context = truncated_context(line);
    if !context.is_empty() {
        message.push_str(&format!(" (context: \"{context}\")"));
    }
    DeferredTextOutcome::Warning(ReceiptWarningKind::PossibleMissedItem, message)
}

/// The quantity rows a backward walk passed on its way to a description.
///
/// Kept separate from the description itself because the walk that collects
/// them can fail to find a description, and the *next* walk's find still needs
/// them: "1 @ $2.99" above a price is that price's quantity no matter which
/// direction the name turned up in.
#[derive(Default)]
struct QuantityContext {
    /// Quantity rows in walk order — nearest the price first.
    info: Vec<String>,
    /// The subset that parsed as a structured modifier.
    modifiers: Vec<QuantityModifier>,
}

/// What the four walks found.
struct DescriptionSearch {
    /// The row that owns the price, and its cleaned text.
    found: Option<(usize, String)>,
    qty: QuantityContext,
    /// The plan said this price is only an item if a description turned up
    /// below it; none did, so drop it silently instead of warning. Distinct
    /// from `found: None`, which *is* worth a warning — the difference is
    /// whether the parser expected to find nothing.
    abandoned: bool,
}

/// Stage 4 — find the row that owns a price its own row did not describe.
///
/// Four walks, and the **order is the algorithm**: each one is more permissive
/// than the last about what counts as a description, so running them in any
/// other order would let a weaker signal win. A priced department banner points
/// at the item below it; an explicit forward preference from the plan comes
/// next; the ordinary backward walk (the common case, and the only one that
/// collects quantity rows) after that; and the forward fallback last, for the
/// layouts that print the price before the name.
///
/// Every walk stops at a row an earlier price already claimed. That is what
/// keeps one item's description from leaking into another's (bugs C, H, K).
fn find_description(index: usize, plan: &PricePlan, rows: Lines<'_>) -> DescriptionSearch {
    let abandon = |qty| DescriptionSearch {
        found: None,
        qty,
        abandoned: true,
    };

    if plan.is_priced_section_header {
        let found = describe_below_priced_header(index, rows);
        if found.is_none() {
            return abandon(QuantityContext::default());
        }
        return DescriptionSearch {
            found,
            qty: QuantityContext::default(),
            abandoned: false,
        };
    }

    let mut found = None;
    if plan.prefer_forward_desc {
        found = describe_forward(index, plan.drift_paren_forward, rows);
    }
    if plan.skip_if_no_forward_desc && found.is_none() {
        return abandon(QuantityContext::default());
    }

    let mut qty = QuantityContext::default();
    if found.is_none() {
        let walk = describe_backward(index, plan, rows);
        found = walk.found;
        qty = walk.qty;
    }
    if found.is_none()
        && !plan.prefer_forward_desc
        && (plan.desc_part.is_empty() || plan.desc_part.len() <= 3 || plan.force_backward)
    {
        found = describe_forward_fallback(index, rows);
    }

    DescriptionSearch {
        found,
        qty,
        abandoned: false,
    }
}

/// Walk 1 — the item below a priced department banner ("&& 01-Grocery  5.59").
fn describe_below_priced_header(index: usize, rows: Lines<'_>) -> Option<(usize, String)> {
    for j in (index + 1)..rows.all.len().min(index + 5) {
        if rows.used[j] {
            // A used line marks the start of another item's territory; don't
            // walk past it.
            return None;
        }
        let next_line = rows.all[j].trim();
        // Under established drift the first item below a priced header often
        // carries the SECOND item's price on its own name row ("&& 01-Grocery
        // 5.59" / "S & B - Wasabi  2.68"). The header's price belongs to that
        // name regardless; skipping it would cross the whole section's pairing
        // by one.
        if rows.drift && re_trailing_price().is_match(next_line) {
            if let Some((_, _, price_start)) = extract_trailing_price_cents(next_line) {
                let head = next_line[..price_start].trim();
                let cleaned_head = strip_leading_receipt_codes(head);
                if !cleaned_head.is_empty()
                    && !is_section_header_text(&cleaned_head)
                    && alpha_ratio(&cleaned_head) >= 0.5
                {
                    return Some((j, cleaned_head));
                }
            }
        }
        if next_line.is_empty()
            || re_skip_patterns().is_match(next_line)
            || looks_like_summary_line(next_line)
            || looks_like_quantity_expression(next_line)
            || looks_like_onsale_marker(next_line)
            || re_trailing_price().is_match(next_line)
            || re_standalone_price_line().is_match(next_line)
            || re_long_digits_line().is_match(next_line)
        {
            continue;
        }
        let cleaned_next = strip_leading_receipt_codes(next_line);
        // Bare counter labels ("Meat" below a priced "&& 03-Meat" banner) are
        // the item the header's drifted price belongs to, not another banner.
        if cleaned_next.is_empty()
            || (is_section_header_text(&cleaned_next) && !is_generic_counter_label(&cleaned_next))
        {
            continue;
        }
        // The `&& <Dept> price` section-header signal is strong: the next
        // non-section line is almost always the item name even if it carries
        // trailing OCR-mangled subtext like `(125gx5)@8.99(1/$6.99)` that drags
        // the alpha ratio below 0.5. A more permissive threshold here lets
        // descriptions like "MN - Crispy Coffee Flavor 6*60g)..." (ratio 0.46)
        // pair correctly, while pure-noise lines are still rejected.
        if alpha_ratio(&cleaned_next) < 0.35 {
            continue;
        }
        return Some((j, cleaned_next));
    }
    None
}

/// Walk 2 — the plan asked to look below before looking above.
///
/// `drift_paren_forward` is the difference between skipping a priced row and
/// stopping at it: under drift the price belongs to the item immediately below,
/// so a priced row in the way means the search has already gone too far.
fn describe_forward(
    index: usize,
    drift_paren_forward: bool,
    rows: Lines<'_>,
) -> Option<(usize, String)> {
    for j in (index + 1)..rows.all.len().min(index + 5) {
        if rows.used[j] {
            return None;
        }
        let next_line = rows.all[j].trim();
        if line_has_trailing_price(next_line) {
            if drift_paren_forward {
                return None;
            }
            continue;
        }
        if next_line.is_empty()
            || re_skip_patterns().is_match(next_line)
            || looks_like_summary_line(next_line)
            || looks_like_quantity_expression(next_line)
            || looks_like_onsale_marker(next_line)
        {
            continue;
        }
        let cleaned_next = strip_leading_receipt_codes(next_line);
        // Bare counter labels ("Meat") are items about to be priced by this row,
        // not department banners.
        if cleaned_next.is_empty()
            || (is_section_header_text(&cleaned_next) && !is_generic_counter_label(&cleaned_next))
        {
            continue;
        }
        if alpha_ratio(&cleaned_next) < 0.5 {
            continue;
        }
        return Some((j, cleaned_next));
    }
    None
}

/// Walk 3's result: a description, and the quantity rows passed to reach it.
struct BackwardWalk {
    found: Option<(usize, String)>,
    qty: QuantityContext,
}

/// Walk 3 — the ordinary case: the name is printed above its price.
///
/// This is the only walk that collects quantity context, because a quantity row
/// is printed between the name and its price ("Broccoli" / "0.41 lb @ $1.98/lb
/// 0.81") and so is passed on the way back up. It keeps collecting even when it
/// ends up finding no description, so walk 4 can still use what it saw.
fn describe_backward(index: usize, plan: &PricePlan, rows: Lines<'_>) -> BackwardWalk {
    let mut qty = QuantityContext::default();
    let lower_bound = index.saturating_sub(5);
    for j in (lower_bound..index).rev() {
        if rows.used[j] {
            // A used line marks the end of the previous item's territory; don't
            // walk past it to grab a description belonging to an item we've
            // already paired.
            break;
        }
        let prev_line = rows.all[j].trim();
        if Regex::new(&format!(r"^[\d.]+\s*{TAX_FLAG_CLASS}\s*$"))
            .unwrap()
            .is_match(prev_line)
            || Regex::new(r"^\d{8,}$").unwrap().is_match(prev_line)
            || re_skip_patterns().is_match(prev_line)
        {
            continue;
        }
        if let Some(modifier) = parse_quantity_modifier(prev_line) {
            qty.modifiers.push(modifier);
            qty.info.push(prev_line.to_string());
            continue;
        }
        if looks_like_quantity_expression(prev_line) {
            qty.info.push(prev_line.to_string());
            continue;
        }
        if looks_like_onsale_marker(prev_line)
            || re_price_info_line().is_match(prev_line)
            || re_parenthetical_closed().is_match(prev_line)
            || (prev_line.starts_with('(') && !prev_line.contains(')'))
            || re_onsale_parenthetical().is_match(prev_line)
            || re_parenthetical_multibuy().is_match(prev_line)
            || prev_line.len() <= 3
        {
            continue;
        }
        // See patterns::SKIP_PRICED_LINES_IN_BACKWARD_DESC_SEARCH for rationale
        // and revert instructions. Limited to bare-price triggers (no qty
        // expression, no description) so OCR column-merge cases like
        // "1 @ $9.99 3.99" can still back-walk into a legitimate
        // "ITEM NAME 9.99" description line.
        if SKIP_PRICED_LINES_IN_BACKWARD_DESC_SEARCH
            && !plan.is_qty_expr
            && !plan.force_backward
            && line_has_trailing_price(prev_line)
        {
            continue;
        }

        let desc_for_ratio = strip_leading_receipt_codes(prev_line);
        if alpha_ratio(&desc_for_ratio) < 0.5 {
            continue;
        }
        if prev_line.len() > 2 && !Regex::new(r"^[\d.]+$").unwrap().is_match(prev_line) {
            let cleaned_prev = strip_leading_receipt_codes(prev_line);
            if !cleaned_prev.is_empty() {
                return BackwardWalk {
                    found: Some((j, cleaned_prev)),
                    qty,
                };
            }
        }
    }
    BackwardWalk { found: None, qty }
}

/// Walk 4 — the price came before the name.
///
/// Reached when the price row has no usable description of its own (empty, very
/// short, or a weak parenthetical like "(1kg)") and the backward walk found
/// nothing. Foody Mart-style layouts print it this way.
fn describe_forward_fallback(index: usize, rows: Lines<'_>) -> Option<(usize, String)> {
    for j in (index + 1)..rows.all.len().min(index + 3) {
        if rows.used[j] {
            return None;
        }
        let next_line = rows.all[j].trim();
        if next_line.is_empty()
            || re_skip_patterns().is_match(next_line)
            || looks_like_summary_line(next_line)
            || looks_like_quantity_expression(next_line)
            || looks_like_onsale_marker(next_line)
            || line_has_trailing_price(next_line)
            || re_standalone_price_line().is_match(next_line)
            || re_long_digits_line().is_match(next_line)
        {
            continue;
        }
        let cleaned_next = strip_leading_receipt_codes(next_line);
        // Treat unpriced "Meat" / "Bakery" lines as legitimate descriptions even
        // though those words are also in the section-name table — that's how
        // Asian-grocery receipts label the items.
        let is_generic_priced_label = matches!(
            cleaned_next.trim().to_ascii_uppercase().as_str(),
            "MEAT" | "BAKERY"
        );
        if cleaned_next.is_empty()
            || (is_section_header_text(&cleaned_next) && !is_generic_priced_label)
        {
            continue;
        }
        if alpha_ratio(&cleaned_next) < 0.5 {
            continue;
        }
        return Some((j, cleaned_next));
    }
    None
}

/// The item a searched-out description yields.
///
/// The quantity context decides between two shapes: a modifier whose arithmetic
/// checks out against the price becomes a real `quantity` (and a weight suffix),
/// while one that does not is appended as text instead. Keeping an unreconciled
/// quantity as prose rather than as a number is deliberate — a wrong quantity
/// silently corrupts the ledger, a parenthetical is visible.
fn searched_item(
    all_lines: &[String],
    desc_line: usize,
    desc_text: String,
    price_cents: Money,
    plan: &PricePlan,
    qty: &QuantityContext,
) -> ParsedTextItem {
    let mut found_desc_value = merge_description_context(all_lines, &desc_text, desc_line);
    if plan.weak_inline_desc {
        found_desc_value = format!("{found_desc_value} {}", plan.desc_part)
            .trim()
            .to_string();
    }
    let mut quantity = 1;
    let mut description_suffix = String::new();
    let as_text = || {
        format!(
            " ({})",
            qty.info
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    match qty.modifiers.first() {
        Some(modifier) if validate_quantity_price(price_cents, modifier) => {
            quantity = modifier.quantity;
            if let Some(weight_text) = &modifier.weight_text {
                description_suffix = format!(" ({weight_text} lb)");
            }
        }
        _ if !qty.info.is_empty() => description_suffix = as_text(),
        _ => {}
    }

    let cleaned_desc = strip_sale_price_subtext(&found_desc_value);
    ParsedTextItem {
        category_source: cleaned_desc.clone(),
        description: format!("{cleaned_desc}{description_suffix}"),
        price: price_cents,
        quantity,
    }
}

/// A REG row printing exactly one price is quoting suggested retail, not
/// charging it.
///
/// The marker is read off the description side with digits and punctuation
/// stripped, because OCR splices it in a dozen ways ("REG$", "0REG", "@REG").
fn is_suggested_retail_row(line: &str, desc_part: &str) -> bool {
    if re_find_prices().find_iter(line).count() != 1 {
        return false;
    }
    let marker: String = desc_part
        .to_ascii_uppercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    Regex::new(r"^\d+")
        .unwrap()
        .replace(&marker, "")
        .ends_with("REG")
}

/// A ghost promo artifact like "EG2.99", where OCR ran letters and a price
/// together into something that reads as a priced item but is not one.
///
/// Only fires when the *original* (uncompacted) line matches too: a row with
/// clear whitespace separation ("Meat 20.53") is a real item, not a ghost. And
/// only when the row above is already priced, which is what makes the amount a
/// leftover rather than this row's own.
fn is_ghost_promo_row(
    line_upper: &str,
    compact_line: &str,
    desc_part: &str,
    index: usize,
    rows: Lines<'_>,
) -> bool {
    re_compact_promo_ghost().is_match(compact_line)
        && re_compact_promo_ghost().is_match(line_upper.trim())
        && !looks_like_onsale_marker(desc_part)
        && index > 0
        && line_has_trailing_price(&rows.all[index - 1])
}

/// Whether a priced row is the receipt talking about its own totals.
///
/// Two shapes. The row says TOTAL itself — including OCR-mangled variants like
/// "Tota1$" (l→1) or "SUBTCTAL" (O→C); without that arm these passed the
/// literal `contains` checks, fell through to the description search, and
/// emitted a phantom "maybe missed item" at the summary amount (the Al-Premium
/// 16.93 case). Or the row carries a known summary amount directly under its
/// own label, which is the same thing split across two lines.
fn price_row_is_summary(
    index: usize,
    line_upper: &str,
    price_cents: Money,
    summary_amounts: &HashSet<Money>,
    rows: Lines<'_>,
) -> bool {
    if line_upper.contains("TOTAL")
        || line_upper.contains("SUBTOTAL")
        || line_upper.contains("SUB TOTAL")
        || re_total_ocr_variants().is_match(line_upper)
    {
        return true;
    }
    if index == 0 || !summary_amounts.contains(&price_cents.abs()) {
        return false;
    }
    let prev_upper = rows.all[index - 1].to_ascii_uppercase();
    prev_upper.contains("TOTAL")
        || prev_upper.contains("SUBTOTAL")
        || prev_upper.contains("SUB TOTAL")
}

/// Whether a bare "($3.50)"-shaped marker row sits inside a multi-buy block — a
/// real description above it and a quantity row below.
///
/// That is the one arrangement where the marker's price is an item price rather
/// than deal subtext, and it is why such a row is kept at all.
fn malformed_marker_is_multi_buy(index: usize, rows: Lines<'_>) -> bool {
    let prev_line = if index > 0 {
        rows.all[index - 1].trim()
    } else {
        ""
    };
    let next_line = if index + 1 < rows.all.len() {
        rows.all[index + 1].trim()
    } else {
        ""
    };
    let prev_looks_like_description = !prev_line.is_empty()
        && !re_skip_patterns().is_match(prev_line)
        && !looks_like_summary_line(prev_line)
        && !looks_like_quantity_expression(prev_line)
        && !line_has_trailing_price(prev_line);
    let next_supports_multi_buy =
        !next_line.is_empty() && looks_like_quantity_expression(next_line);
    prev_looks_like_description && next_supports_multi_buy
}

/// Whether a row quotes a REG (suggested retail) price anywhere in its text.
///
/// The four literals are the ways OCR splices the marker into its neighbour —
/// "0REG" and "OREG" are a leading `@` misread as a digit or a letter.
fn has_reg_price_marker(line_upper: &str) -> bool {
    line_upper.contains("REG$")
        || line_upper.contains("@REG")
        || line_upper.contains("0REG")
        || line_upper.contains("OREG")
        || re_reg_price_marker().is_match(line_upper)
}

/// Whether the text left of the price is nothing but a parenthesised amount —
/// "($3.50)" and nothing else.
///
/// Such a row carries a price and no name, so it is either deal subtext to be
/// discarded or the middle of a multi-buy block; [`malformed_marker_is_multi_buy`]
/// decides which. The length and space bounds are what keep a real description
/// that happens to open with a bracket out of this class.
fn is_bare_price_marker(desc_part: &str) -> bool {
    !desc_part.is_empty()
        && desc_part.starts_with('(')
        && desc_part.contains('$')
        && !desc_part.contains(' ')
        && desc_part.len() <= 16
        && !desc_part.contains('@')
        && !desc_part.to_ascii_uppercase().contains("REG")
}

pub fn extract_text_items(
    lines: &[String],
    summary_amounts: &HashSet<Money>,
) -> (Vec<ParsedTextItem>, Vec<TextParserWarning>) {
    let mut deferred = Vec::new();
    let normalized_lines: Vec<String> = lines
        .iter()
        .map(|line| normalize_decimal_spacing(line))
        .collect();
    // Track description lines already consumed by an earlier price so a later
    // price's forward/backward search can't grab the same description. Without
    // this, a "weak inline desc" line like "(1kg) 16.99" forces a backward walk
    // that pulls the previous item's description, producing a cross-row leak
    // (Foody Mart bug C).
    let mut used_text_lines: Vec<bool> = vec![false; normalized_lines.len()];

    let total_line_idx = grand_total_line(&normalized_lines);
    let total_cap_cents = total_price_cap(&normalized_lines);

    // Receipt-level evidence that the right price column drifted one row up
    // (photo shear on two-column Asian-grocery receipts): count qty rows whose
    // trailing price does NOT reconcile as the row's own qty×unit total. On a
    // straight receipt this is ~0; on a leaning one ("1 @ $2.59  0.91H", …)
    // nearly every deal block contributes one. With systematic drift
    // established, a qty row whose trailing price *coincidentally* equals its
    // own total ("1 @ $1.99  1.99" where the next item also costs 1.99) is
    // still treated as carrying the next item's price, and paren-subtext rows
    // pair forward instead of backward.
    let price_drift = count_price_drift_evidence(&normalized_lines) >= PRICE_DRIFT_EVIDENCE_MIN;

    for (i, line) in normalized_lines.iter().enumerate() {
        if total_line_idx.is_some_and(|total_idx| i > total_idx) {
            break;
        }
        if re_skip_patterns().is_match(line) {
            continue;
        }
        if line.len() < 3 || re_digits_only().is_match(line) {
            continue;
        }

        let is_qty_line = looks_like_quantity_expression(line);
        let has_trailing_total = re_trailing_total_presence().is_match(line);
        if is_qty_line {
            if let Some(pairing) = orphan_qty_pairing(
                i,
                line,
                Lines::of(&normalized_lines, &used_text_lines, price_drift),
            ) {
                used_text_lines[pairing.description_line] = true;
                deferred.push(DeferredTextOutcome::Item(pairing.item));
                continue;
            }
        }

        if is_qty_line && !has_trailing_total {
            if let Some(warning) = multi_buy_tail_warning(line) {
                deferred.push(warning);
            }
            continue;
        }

        if re_parenthetical_only().is_match(line) && !re_trailing_price().is_match(line) {
            continue;
        }

        if let Some((price_cents, _is_discount, price_start)) = extract_trailing_price_cents(line) {
            // A row already claimed as another pairing's description can still
            // carry a trailing price — under drift it is the NEXT item's,
            // drifted onto this name row ("S & B - Wasabi  2.68" right after
            // the priced header claimed Wasabi at 5.59). Forward it to the next
            // unclaimed description and consume this row either way.
            if price_drift && used_text_lines[i] {
                if let Some(pairing) = drifted_price_pairing(
                    i,
                    price_cents,
                    Lines::of(&normalized_lines, &used_text_lines, price_drift),
                ) {
                    used_text_lines[pairing.description_line] = true;
                    deferred.push(DeferredTextOutcome::Item(pairing.item));
                }
                continue;
            }
            let Some(plan) = plan_price_line(
                i,
                line,
                price_start,
                price_cents,
                summary_amounts,
                Lines::of(&normalized_lines, &used_text_lines, price_drift),
            ) else {
                continue;
            };

            if plan.describes_itself() {
                deferred.push(DeferredTextOutcome::Item(inline_item(&plan, price_cents)));
                // Only block subsequent backward walks when the inline text is
                // a genuine description. Low-alpha junk like "#E$" must stay
                // walkable so the next price below can reach the real item
                // sitting above the junk.
                if alpha_ratio(plan.desc_part.trim()) >= 0.5 {
                    used_text_lines[i] = true;
                }
            } else {
                let search = find_description(
                    i,
                    &plan,
                    Lines::of(&normalized_lines, &used_text_lines, price_drift),
                );
                if search.abandoned {
                    continue;
                }
                match search.found {
                    Some((desc_idx, desc_text)) => {
                        deferred.push(DeferredTextOutcome::Item(searched_item(
                            &normalized_lines,
                            desc_idx,
                            desc_text,
                            price_cents,
                            &plan,
                            &search.qty,
                        )));
                        used_text_lines[desc_idx] = true;
                    }
                    None => {
                        if price_cents > Money::ZERO {
                            deferred.push(unowned_price_warning(line, price_cents));
                        }
                    }
                }
            }
        } else if let Some(outcome) = unpriced_line_outcome(line) {
            deferred.push(outcome);
        }
    }

    let (mut items, mut warnings) = resolve_deferred(deferred, summary_amounts);

    if let Some(cap_base) = total_cap_cents {
        items = drop_prices_above_cap(items, &mut warnings, cap_base);
    }

    (items, warnings)
}
