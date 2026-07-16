//! Helpers and the main [`extract_text_items`] entry point.

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use super::patterns::*;
use super::types::*;

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

fn parse_cents(token: &str) -> Option<i64> {
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
    Some(dollars * 100 + cents)
}

fn format_cents(value: i64) -> String {
    let abs_value = value.abs();
    let dollars = abs_value / 100;
    let cents = abs_value % 100;
    if value < 0 {
        format!("-{dollars}.{cents:02}")
    } else {
        format!("{dollars}.{cents:02}")
    }
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
            unit_price_cents: Some(unit_price_cents),
            weight_text: None,
            deal_price_cents: None,
            pattern_type: QuantityPatternType::CountAtPrice,
        });
    }

    if let Some(captures) = re_weight_at_price()
        .captures(&normalized)
        .or_else(|| re_weight_rate_no_at().captures(&normalized))
    {
        return Some(QuantityModifier {
            quantity: 1,
            unit_price_cents: captures.get(2).and_then(|m| parse_cents(m.as_str())),
            weight_text: Some(captures.get(1)?.as_str().to_string()),
            deal_price_cents: None,
            pattern_type: QuantityPatternType::WeightAtPrice,
        });
    }

    if let Some(captures) = re_multi_for_price().captures(&normalized) {
        let quantity = captures.get(1)?.as_str().parse::<i32>().ok()?;
        let deal_price_cents = parse_cents(captures.get(2)?.as_str())?;
        return Some(QuantityModifier {
            quantity,
            unit_price_cents: Some(deal_price_cents / i64::from(quantity)),
            weight_text: None,
            deal_price_cents: Some(deal_price_cents),
            pattern_type: QuantityPatternType::MultiForPrice,
        });
    }

    None
}

fn validate_quantity_price(total_price_cents: i64, modifier: &QuantityModifier) -> bool {
    let tolerance = 2i64;
    match modifier.pattern_type {
        QuantityPatternType::CountAtPrice => modifier
            .unit_price_cents
            .map(|unit| {
                (unit * i64::from(modifier.quantity) - total_price_cents).abs() <= tolerance
            })
            .unwrap_or(false),
        QuantityPatternType::MultiForPrice => modifier
            .deal_price_cents
            .map(|deal| (deal - total_price_cents).abs() <= tolerance)
            .unwrap_or(false),
        QuantityPatternType::WeightAtPrice => {
            // When both the weight and the per-unit rate are readable, the
            // row's own total is weight × rate; a trailing price that doesn't
            // reconcile is another item's drifted price, not this row's total.
            // When either is unreadable, keep the historical benefit of the
            // doubt (always-own-total).
            let computed = modifier.unit_price_cents.and_then(|unit| {
                modifier
                    .weight_text
                    .as_deref()
                    .and_then(|weight| weight.parse::<f64>().ok())
                    .map(|weight| (weight * unit as f64).round() as i64)
            });
            match computed {
                Some(own_total) => (own_total - total_price_cents).abs() <= 3,
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
                if cents > 0
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
            let prices: Vec<i64> = re_find_prices()
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
            orphan > 0
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

pub(crate) fn extract_trailing_price_cents(line: &str) -> Option<(i64, bool, usize)> {
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

fn maybe_push_warning(warnings: &mut Vec<TextParserWarning>, items_len: usize, message: String) {
    warnings.push(TextParserWarning {
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
    let mut best_by_price: HashMap<i64, usize> = HashMap::new();

    for cents in 0..=99i64 {
        let fraction = format!("{cents:02}");
        let score = levenshtein_distance(&candidate.observed_fraction, &fraction);
        if score > 2 {
            continue;
        }
        let price_cents = candidate.whole_dollars * 100 + cents;
        best_by_price
            .entry(price_cents)
            .and_modify(|best_score| *best_score = (*best_score).min(score))
            .or_insert(score);
    }

    let mut options = best_by_price
        .into_iter()
        .map(|(price_cents, score)| CandidatePriceOption { price_cents, score })
        .collect::<Vec<_>>();
    options.sort_by_key(|option| (option.score, option.price_cents));
    options
}

fn reconcile_malformed_price_candidates(
    regular_total_cents: i64,
    summary_amounts: &HashSet<i64>,
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

            let mut next_states: HashMap<i64, ReconciliationState> = HashMap::new();
            for (running_total, state) in &states {
                for option in &options {
                    let next_total = running_total + option.price_cents;
                    if next_total > target {
                        continue;
                    }
                    let next_score = state.score + option.score;
                    let mut next_prices = state.prices.clone();
                    next_prices.push(option.price_cents);

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

pub fn extract_text_items(
    lines: &[String],
    summary_amounts: &HashSet<i64>,
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

    let total_line_idx = normalized_lines.iter().position(|line| {
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
    });

    // Authoritative receipt total, when a grand-total line carries a price.
    // Used as a sanity ceiling on individual item prices: a single positive
    // line item can never exceed (total + sum of discounts), so a price above
    // that ceiling is an OCR artifact (e.g. "$1.58" misread as "81.58") and is
    // dropped rather than mis-paired — "prefer missing items over wrong
    // pairings". Taken as the max over genuine grand-total lines (not the
    // first match) so sub-lines like "TOTAL TAX" never stand in for the total.
    let total_cap_cents = normalized_lines
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
        .filter(|c| *c > 0)
        .max();

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

        // OCR column-merge recovery: a quantity line can absorb the NEXT
        // item's price into its own text row -- e.g. FreshCo
        // "2 @ 1/ $12.98 $11.19 C", where $11.19 is the price of the
        // price-less "Natrel Milk 2% 4L" line below (the right-hand price
        // column drifted up one row in OCR reading order). When a qty line
        // carries a trailing price that does NOT reconcile as its own line
        // total (qty x unit) and the next line is a bare, price-less
        // description, pair that orphan price with that description. The qty
        // line itself is still consumed below as a modifier of the item above.
        if is_qty_line {
            let prices: Vec<i64> = re_find_prices()
                .captures_iter(line)
                .filter_map(|caps| caps.get(1).and_then(|m| parse_cents(m.as_str())))
                .collect();
            // The orphan must be a genuine trailing price, not the tail of a
            // parenthetical deal ("(2/$3.50)" ends the coriander line; that
            // 3.50 is subtext, not a drifted amount).
            let trailing = extract_trailing_price_cents(line).map(|(c, _, _)| c);
            if prices.len() >= 2 && trailing == prices.last().copied() {
                let orphan_cents = *prices.last().unwrap();
                let reconciles_as_own_total = parse_quantity_modifier(line)
                    .map(|modifier| validate_quantity_price(orphan_cents, &modifier))
                    .unwrap_or(false);
                // The downward pairing is only valid when the description
                // above is already priced, so this row has nothing left to
                // donate upward. An unclaimed description above keeps the
                // trailing price as its own — whether the row reconciles
                // ("Broccoli (Crowns)" / "0.41 lb @ $1.98/lb  0.81") or not
                // ("HLY - Potato Chips Honey" / "(...)@3.99(1/$0.98)  5.88H",
                // where 5.88 is Honey's own price on its deal-subtext row).
                // Under receipt-level drift even a coincidentally-reconciling
                // echo ("1 @ $1.99  1.99" where the next item also costs
                // 1.99) is the next item's price.
                let above_consumed =
                    nearest_desc_above_consumed(&normalized_lines, &used_text_lines, i);
                let echo_is_drifted = price_drift && above_consumed;
                if orphan_cents > 0
                    && above_consumed
                    && (!reconciles_as_own_total || echo_is_drifted)
                {
                    // The descriptive line can sit a row or two further down
                    // when the block carries its own qty row: Foody Mart prints
                    // "(<size>)@<unit>(<deal>) <orphan>" / "1 @ $2.99" /
                    // "<item name>". Skip qty rows; never leap over a priced
                    // row (that's another item's territory).
                    let mut desc_line_idx = None;
                    for j in (i + 1)..normalized_lines.len().min(i + 4) {
                        let candidate = normalized_lines[j].trim();
                        if candidate.is_empty() || looks_like_quantity_expression(candidate) {
                            continue;
                        }
                        if line_has_trailing_price(candidate) {
                            break;
                        }
                        desc_line_idx = Some(j);
                        break;
                    }
                    if let Some(next_idx) = desc_line_idx {
                        let next_trimmed = normalized_lines[next_idx].trim();
                        // Skip if the line was already consumed by an
                        // earlier price's search — avoids cross-row leak.
                        // Do NOT mark used here: this orphan-qty pairing is a
                        // low-confidence OCR-column-merge heuristic, so a later
                        // higher-confidence search (backward / weak-desc forward)
                        // is allowed to claim the same description.
                        // A bare counter label ("Meat" above its mangled
                        // Chinese subtext) is a real item here — the orphan
                        // price arriving from this qty row is what prices it.
                        // `is_priced_generic_item_label`'s trailing-price
                        // requirement can't see a price delivered from above.
                        if !used_text_lines[next_idx]
                            && (is_descriptive_candidate(next_trimmed)
                                || is_generic_counter_label(next_trimmed))
                        {
                            let desc = strip_sale_price_subtext(&strip_leading_receipt_codes(
                                next_trimmed,
                            ));
                            deferred.push(DeferredTextOutcome::Item(ParsedTextItem {
                                category_source: desc.clone(),
                                description: desc,
                                price_cents: orphan_cents,
                                quantity: 1,
                            }));
                            // If the line right after the description also
                            // carries a trailing price equal to orphan_cents
                            // (the typical Asian-grocery `desc / size+price /
                            // qty / ...` layout where the qty repeats the unit
                            // price), the pairing is confirmed: mark the desc
                            // line used so the following iteration's weak-desc
                            // backward search can't re-claim it (bug H/K). If
                            // it does NOT match, the pairing is speculative —
                            // leave the line unmarked so a later
                            // higher-confidence backward search can reach it.
                            // Mark the claimed description so the next link
                            // of the chain sees it as consumed: the row below
                            // it can then donate ITS trailing price downward
                            // (nearest_desc_above_consumed), and the paren
                            // back-walk can't emit the same item again at a
                            // drifted price. The pairing itself was already
                            // gated on the description above being consumed,
                            // so this is no longer speculative.
                            used_text_lines[next_idx] = true;
                            // The orphan-qty path just paired this line's
                            // trailing price with the description below. Don't
                            // also let the regular extract path pair the same
                            // trailing price with a description ABOVE — that
                            // produces a duplicate extraction (bug K) where the
                            // qty/sale-subtext gets glued onto the wrong item.
                            continue;
                        }
                    }
                }
            }
        }

        if is_qty_line && !has_trailing_total {
            if line.to_ascii_lowercase().contains("/for") {
                let tail_token = re_tail_token()
                    .captures(line)
                    .and_then(|captures| captures.get(1).map(|m| m.as_str().to_string()))
                    .unwrap_or_default();
                if !tail_token.is_empty() && tail_token.chars().any(|ch| ch.is_ascii_alphabetic()) {
                    let context = truncated_context(line);
                    deferred.push(DeferredTextOutcome::Warning(
                        format!(
                            "maybe missed item near malformed multi-buy total \"{tail_token}\" (context: \"{context}\")"
                        ),
                    ));
                }
            }
            continue;
        }

        if re_parenthetical_only().is_match(line) && !re_trailing_price().is_match(line) {
            continue;
        }

        if let Some((price_cents, _is_discount, price_start)) = extract_trailing_price_cents(line) {
            // A row already claimed as another pairing's description can
            // still carry a trailing price — under drift it is the NEXT
            // item's, drifted onto this name row ("S & B - Wasabi  2.68"
            // right after the priced header claimed Wasabi at 5.59). Forward
            // it to the next unclaimed description and consume this row.
            if price_drift && used_text_lines[i] {
                for j in (i + 1)..normalized_lines.len().min(i + 4) {
                    if used_text_lines[j] {
                        break;
                    }
                    let candidate = normalized_lines[j].trim();
                    if line_has_trailing_price(candidate) {
                        break;
                    }
                    if candidate.is_empty()
                        || looks_like_quantity_expression(candidate)
                        || !(is_descriptive_candidate(candidate)
                            || is_generic_counter_label(candidate))
                    {
                        continue;
                    }
                    let desc = strip_sale_price_subtext(&strip_leading_receipt_codes(candidate));
                    deferred.push(DeferredTextOutcome::Item(ParsedTextItem {
                        category_source: desc.clone(),
                        description: desc,
                        price_cents,
                        quantity: 1,
                    }));
                    used_text_lines[j] = true;
                    break;
                }
                continue;
            }
            let line_upper = line.to_ascii_uppercase();
            let mut desc_part = line[..price_start].trim().to_string();
            let compact_line = re_compact_space().replace_all(&line_upper, "").to_string();
            let mut prefer_forward_desc = false;
            let mut skip_if_no_forward_desc = false;

            let has_reg_marker = line_upper.contains("REG$")
                || line_upper.contains("@REG")
                || line_upper.contains("0REG")
                || line_upper.contains("OREG")
                || re_reg_price_marker().is_match(&line_upper);

            if has_reg_marker {
                let prices: Vec<_> = re_find_prices().find_iter(line).collect();
                if prices.len() == 1 {
                    let mut marker: String = desc_part
                        .to_ascii_uppercase()
                        .chars()
                        .filter(|ch| ch.is_ascii_alphanumeric())
                        .collect();
                    marker = Regex::new(r"^\d+")
                        .unwrap()
                        .replace(&marker, "")
                        .to_string();
                    if marker.ends_with("REG") {
                        continue;
                    }
                }
                if prices.len() > 1
                    && i > 0
                    && re_trailing_price().is_match(&normalized_lines[i - 1])
                {
                    prefer_forward_desc = true;
                    skip_if_no_forward_desc = true;
                }
            }

            // Skip ghost promo artifacts like "EG2.99" where letters and price
            // run together.  Only fire when the *original* (uncompacted) line also
            // matches — lines with clear whitespace separation (e.g. "Meat 20.53")
            // are real items, not ghosts.
            if re_compact_promo_ghost().is_match(&compact_line)
                && re_compact_promo_ghost().is_match(line_upper.trim())
                && !looks_like_onsale_marker(&desc_part)
            {
                if i > 0 && line_has_trailing_price(&normalized_lines[i - 1]) {
                    continue;
                }
            }

            // Skip TOTAL/SUBTOTAL summary rows, including OCR-mangled variants
            // like "Tota1$" (l→1) or "SUBTCTAL" (O→C). Without the
            // `re_total_ocr_variants` arm these lines passed the literal
            // contains() checks, fell into the description-search else branch,
            // and emitted a "maybe missed item" warning at the summary amount
            // (Al-Premium 16.93 phantom).
            if line_upper.contains("TOTAL")
                || line_upper.contains("SUBTOTAL")
                || line_upper.contains("SUB TOTAL")
                || re_total_ocr_variants().is_match(&line_upper)
            {
                continue;
            }

            if i > 0 && summary_amounts.contains(&price_cents.abs()) {
                let prev_upper = normalized_lines[i - 1].to_ascii_uppercase();
                if prev_upper.contains("TOTAL")
                    || prev_upper.contains("SUBTOTAL")
                    || prev_upper.contains("SUB TOTAL")
                {
                    continue;
                }
            }

            let weak_inline_desc = is_weak_inline_description(&desc_part);
            let mut force_backward =
                line_upper.contains("REG$") || line_upper.contains("@REG") || weak_inline_desc;
            // Under receipt-level drift a paren-subtext row's trailing price
            // belongs to the item BELOW when the description above is already
            // priced ("Pork Lard" claimed from its qty row, so "(3 380g)
            // 2.98" is Pak Fok's) — search forward, stopping at the first
            // priced row rather than leaping over it. When the description
            // above is still unclaimed, the price is its own ("Fresh Chicken
            // Wings" / "(WRER)  10.04") and the backward walk stays correct.
            let drift_paren_forward = price_drift
                && desc_part.trim_start().starts_with('(')
                && nearest_desc_above_consumed(&normalized_lines, &used_text_lines, i);
            if drift_paren_forward {
                prefer_forward_desc = true;
            }
            if has_reg_marker
                && force_backward
                && i > 0
                && !normalized_lines[i - 1].trim().is_empty()
                && line_has_trailing_price(normalized_lines[i - 1].trim())
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
                if i > 0 && line_has_trailing_price(normalized_lines[i - 1].trim()) {
                    skip_if_no_forward_desc = true;
                }
            }

            let is_priced_section_header = !desc_part.is_empty()
                && is_section_header_text(&desc_part)
                && !is_priced_generic_item_label(&desc_part, line);
            let mut skip_section_header_price = false;
            if is_priced_section_header {
                desc_part.clear();
                for j in (i + 1)..normalized_lines.len().min(i + 4) {
                    let next_line = normalized_lines[j].trim();
                    if next_line.is_empty() {
                        continue;
                    }
                    if looks_like_summary_line(next_line) {
                        break;
                    }
                    if let Some((next_price, _, _)) = extract_trailing_price_cents(next_line) {
                        if next_price == price_cents {
                            skip_section_header_price = true;
                        }
                    }
                    break;
                }
            }
            if skip_section_header_price {
                continue;
            }

            let is_malformed_price_marker = !desc_part.is_empty()
                && desc_part.starts_with('(')
                && desc_part.contains('$')
                && !desc_part.contains(' ')
                && desc_part.len() <= 16
                && !desc_part.contains('@')
                && !desc_part.to_ascii_uppercase().contains("REG");
            let is_quantity_stub = re_malformed_price_marker().is_match(&desc_part);
            let mut is_qty_expr = if !desc_part.is_empty() {
                looks_like_quantity_expression(&desc_part)
                    || re_onsale_parenthetical().is_match(&desc_part)
                    || is_onsale_marker_desc
            } else {
                false
            };

            if is_malformed_price_marker {
                let prev_line = if i > 0 {
                    normalized_lines[i - 1].trim()
                } else {
                    ""
                };
                let next_line = if i + 1 < normalized_lines.len() {
                    normalized_lines[i + 1].trim()
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
                if prev_looks_like_description && next_supports_multi_buy {
                    force_backward = true;
                    desc_part.clear();
                    is_qty_expr = false;
                } else {
                    continue;
                }
            }
            if is_quantity_stub {
                continue;
            }

            // If desc_part is a mangled REG-price marker (OCR ate the leading R,
            // so "REG$15.99" became "#EG15.99" or "(EG$5.99"), the trailing
            // price is the suggested-retail marker, not an item price. The if
            // block below already filters via `!re_mangled_reg_marker`, but the
            // else branch would back-walk and emit a phantom item paired with
            // the previous line. Suppress the whole line instead.
            if !desc_part.is_empty() && re_mangled_reg_marker().is_match(desc_part.trim()) {
                // Under drift with the description above already priced, the
                // REG amount lives inside the marker itself and the trailing
                // price is the NEXT item's ("(-EG4.99  2.99" above "LKS Dried
                // Cod Fish Slice") — forward it; in every other shape the
                // trailing price is the suggested-retail amount, so keep
                // suppressing the row.
                if drift_paren_forward {
                    skip_if_no_forward_desc = true;
                } else {
                    continue;
                }
            }

            if !desc_part.is_empty()
                && desc_part.len() > 2
                && !is_qty_expr
                && !force_backward
                && !drift_paren_forward
                && !looks_like_summary_line(desc_part.trim())
            {
                let desc_alpha = alpha_ratio(desc_part.trim());
                let desc_clean = strip_sale_price_subtext(&desc_part);
                let desc_clean = re_embedded_unit_price_suffix()
                    .replace(&desc_clean, "")
                    .trim()
                    .to_string();
                deferred.push(DeferredTextOutcome::Item(ParsedTextItem {
                    description: desc_clean.clone(),
                    category_source: desc_clean,
                    price_cents,
                    quantity: 1,
                }));
                // Only block subsequent backward walks when desc_part is a
                // genuine description. Low-alpha junk like "#E$" must stay
                // walkable so the next price below can reach the real item
                // sitting above the junk.
                if desc_alpha >= 0.5 {
                    used_text_lines[i] = true;
                }
            } else {
                let mut qty_info = Vec::new();
                let mut qty_modifiers = Vec::new();
                let mut found_desc: Option<String> = None;
                let mut found_desc_line_idx: Option<usize> = None;

                if is_priced_section_header {
                    for j in (i + 1)..normalized_lines.len().min(i + 5) {
                        if used_text_lines[j] {
                            // A used line marks the start of another item's
                            // territory; don't walk past it.
                            break;
                        }
                        let next_line = normalized_lines[j].trim();
                        // Under established drift the first item below a
                        // priced header often carries the SECOND item's price
                        // on its own name row ("&& 01-Grocery  5.59" / "S & B
                        // - Wasabi  2.68"). The header's price belongs to that
                        // name regardless; skipping it would cross the whole
                        // section's pairing by one.
                        if price_drift && re_trailing_price().is_match(next_line) {
                            if let Some((_, _, price_start)) =
                                extract_trailing_price_cents(next_line)
                            {
                                let head = next_line[..price_start].trim();
                                let cleaned_head = strip_leading_receipt_codes(head);
                                if !cleaned_head.is_empty()
                                    && !is_section_header_text(&cleaned_head)
                                    && alpha_ratio(&cleaned_head) >= 0.5
                                {
                                    found_desc = Some(cleaned_head);
                                    found_desc_line_idx = Some(j);
                                    break;
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
                        // Bare counter labels ("Meat" below a priced
                        // "&& 03-Meat" banner) are the item the header's
                        // drifted price belongs to, not another banner.
                        if cleaned_next.is_empty()
                            || (is_section_header_text(&cleaned_next)
                                && !is_generic_counter_label(&cleaned_next))
                        {
                            continue;
                        }
                        // The `&& <Dept> price` section-header signal is strong:
                        // the next non-section line is almost always the item
                        // name even if it carries trailing OCR-mangled subtext
                        // like `(125gx5)@8.99(1/$6.99)` that drags the alpha
                        // ratio below 0.5. A more permissive threshold here lets
                        // descriptions like "MN - Crispy Coffee Flavor 6*60g)..."
                        // (ratio 0.46) pair correctly, while pure-noise lines
                        // are still rejected.
                        if alpha_ratio(&cleaned_next) < 0.35 {
                            continue;
                        }
                        found_desc = Some(cleaned_next);
                        found_desc_line_idx = Some(j);
                        break;
                    }
                }
                if is_priced_section_header && found_desc.is_none() {
                    continue;
                }

                if found_desc.is_none() && prefer_forward_desc {
                    for j in (i + 1)..normalized_lines.len().min(i + 5) {
                        if used_text_lines[j] {
                            break;
                        }
                        let next_line = normalized_lines[j].trim();
                        if line_has_trailing_price(next_line) {
                            if drift_paren_forward {
                                break;
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
                        // Bare counter labels ("Meat") are items about to be
                        // priced by this row, not department banners.
                        if cleaned_next.is_empty()
                            || (is_section_header_text(&cleaned_next)
                                && !is_generic_counter_label(&cleaned_next))
                        {
                            continue;
                        }
                        if alpha_ratio(&cleaned_next) < 0.5 {
                            continue;
                        }
                        found_desc = Some(cleaned_next);
                        found_desc_line_idx = Some(j);
                        break;
                    }
                }
                if skip_if_no_forward_desc && found_desc.is_none() {
                    continue;
                }

                if found_desc.is_none() {
                    let lower_bound = i.saturating_sub(5);
                    for j in (lower_bound..i).rev() {
                        if used_text_lines[j] {
                            // A used line marks the end of the previous item's
                            // territory; don't walk past it to grab a description
                            // belonging to an item we've already paired.
                            break;
                        }
                        let prev_line = normalized_lines[j].trim();
                        if Regex::new(&format!(r"^[\d.]+\s*{TAX_FLAG_CLASS}\s*$"))
                            .unwrap()
                            .is_match(prev_line)
                            || Regex::new(r"^\d{8,}$").unwrap().is_match(prev_line)
                            || re_skip_patterns().is_match(prev_line)
                        {
                            continue;
                        }
                        if let Some(modifier) = parse_quantity_modifier(prev_line) {
                            qty_modifiers.push(modifier);
                            qty_info.push(prev_line.to_string());
                            continue;
                        }
                        if looks_like_quantity_expression(prev_line) {
                            qty_info.push(prev_line.to_string());
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
                        // See patterns::SKIP_PRICED_LINES_IN_BACKWARD_DESC_SEARCH
                        // for rationale and revert instructions.
                        // Limited to bare-price triggers (no qty expression,
                        // no description) so OCR column-merge cases like
                        // "1 @ $9.99 3.99" can still back-walk into a
                        // legitimate "ITEM NAME 9.99" description line.
                        if SKIP_PRICED_LINES_IN_BACKWARD_DESC_SEARCH
                            && !is_qty_expr
                            && !force_backward
                            && line_has_trailing_price(prev_line)
                        {
                            continue;
                        }

                        let desc_for_ratio = strip_leading_receipt_codes(prev_line);
                        if alpha_ratio(&desc_for_ratio) < 0.5 {
                            continue;
                        }
                        if prev_line.len() > 2
                            && !Regex::new(r"^[\d.]+$").unwrap().is_match(prev_line)
                        {
                            let cleaned_prev = strip_leading_receipt_codes(prev_line);
                            if !cleaned_prev.is_empty() {
                                found_desc = Some(cleaned_prev);
                                found_desc_line_idx = Some(j);
                                break;
                            }
                        }
                    }
                }

                // Forward fallback: when the price line has no usable
                // description on its own (empty / very short / weak-parenthetical
                // like "(1kg)" or "()") and backward search returned nothing,
                // try a couple of lines forward. This handles Foody Mart-style
                // layouts where the price comes BEFORE the description.
                if found_desc.is_none()
                    && !is_priced_section_header
                    && !prefer_forward_desc
                    && (desc_part.is_empty() || desc_part.len() <= 3 || force_backward)
                {
                    for j in (i + 1)..normalized_lines.len().min(i + 3) {
                        if used_text_lines[j] {
                            break;
                        }
                        let next_line = normalized_lines[j].trim();
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
                        // Treat unpriced "Meat" / "Bakery" lines as legitimate
                        // descriptions even though those words are also in the
                        // section-name table — that's how Asian-grocery receipts
                        // label the items.
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
                        found_desc = Some(cleaned_next);
                        found_desc_line_idx = Some(j);
                        break;
                    }
                }

                if let Some(mut found_desc_value) = found_desc {
                    if let Some(source_idx) = found_desc_line_idx {
                        found_desc_value = merge_description_context(
                            &normalized_lines,
                            &found_desc_value,
                            source_idx,
                        );
                    }
                    if weak_inline_desc {
                        found_desc_value =
                            format!("{found_desc_value} {desc_part}").trim().to_string();
                    }
                    let mut quantity = 1;
                    let mut description_suffix = String::new();

                    if let Some(modifier) = qty_modifiers.first() {
                        if validate_quantity_price(price_cents, modifier) {
                            quantity = modifier.quantity;
                            if let Some(weight_text) = &modifier.weight_text {
                                description_suffix = format!(" ({weight_text} lb)");
                            }
                        } else if !qty_info.is_empty() {
                            let reversed: Vec<String> = qty_info.iter().rev().cloned().collect();
                            description_suffix = format!(" ({})", reversed.join(", "));
                        }
                    } else if !qty_info.is_empty() {
                        let reversed: Vec<String> = qty_info.iter().rev().cloned().collect();
                        description_suffix = format!(" ({})", reversed.join(", "));
                    }

                    let cleaned_desc = strip_sale_price_subtext(&found_desc_value);
                    deferred.push(DeferredTextOutcome::Item(ParsedTextItem {
                        category_source: cleaned_desc.clone(),
                        description: format!("{cleaned_desc}{description_suffix}"),
                        price_cents,
                        quantity,
                    }));
                    if let Some(idx) = found_desc_line_idx {
                        used_text_lines[idx] = true;
                    }
                } else if price_cents > 0 {
                    let mut message =
                        format!("maybe missed item near price {}", format_cents(price_cents));
                    let context = truncated_context(line);
                    if !context.is_empty() {
                        message.push_str(&format!(" (context: \"{context}\")"));
                    }
                    deferred.push(DeferredTextOutcome::Warning(message));
                }
            }
        } else if let Some(candidate) = build_malformed_price_candidate(line) {
            deferred.push(DeferredTextOutcome::Malformed(candidate));
        } else if let Some(captures) = re_malformed_ocr_price().captures(line) {
            let token = captures.get(1).map(|m| m.as_str()).unwrap_or("");
            let context = truncated_context(line);
            deferred.push(DeferredTextOutcome::Warning(format!(
                "maybe missed item with malformed OCR price \"{token}\" (context: \"{context}\")"
            )));
        } else if line.to_ascii_lowercase().contains("/for")
            && re_tail_token().is_match(line)
            && re_tail_token()
                .captures(line)
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                .is_some_and(|tail| tail.chars().any(|ch| ch.is_ascii_alphabetic()))
        {
            let tail_token = re_tail_token()
                .captures(line)
                .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
                .unwrap_or_default();
            let context = truncated_context(line);
            deferred.push(DeferredTextOutcome::Warning(
                format!(
                    "maybe missed item near malformed multi-buy total \"{tail_token}\" (context: \"{context}\")"
                ),
            ));
        }
    }

    let regular_total_cents = deferred
        .iter()
        .filter_map(|outcome| match outcome {
            DeferredTextOutcome::Item(item) => Some(item.price_cents),
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
            DeferredTextOutcome::Warning(message) => {
                maybe_push_warning(&mut warnings, items.len(), message);
            }
            DeferredTextOutcome::Malformed(candidate) => {
                if let Some(recovered_price_cents) =
                    malformed_prices.as_mut().and_then(|prices| prices.next())
                {
                    items.push(ParsedTextItem {
                        description: candidate.description.clone(),
                        category_source: candidate.category_source.clone(),
                        price_cents: recovered_price_cents,
                        quantity: 1,
                    });
                    maybe_push_warning(
                        &mut warnings,
                        items.len(),
                        format!(
                            "auto-corrected malformed OCR price \"{}\" -> \"{}\" using summary reconciliation",
                            candidate.observed_token,
                            format_cents(recovered_price_cents),
                        ),
                    );
                } else {
                    maybe_push_warning(
                        &mut warnings,
                        items.len(),
                        format!(
                            "maybe missed item with malformed OCR price \"{}\" (context: \"{}\")",
                            candidate.observed_token, candidate.context
                        ),
                    );
                }
            }
        }
    }

    if let Some(cap_base) = total_cap_cents {
        let discount_sum: i64 = items
            .iter()
            .filter(|it| it.price_cents < 0)
            .map(|it| -it.price_cents)
            .sum();
        let cap = cap_base + discount_sum;
        let mut kept = Vec::with_capacity(items.len());
        for it in items.into_iter() {
            if it.price_cents > cap {
                maybe_push_warning(
                    &mut warnings,
                    kept.len(),
                    format!(
                        "dropped implausible item price \"{}\" exceeding receipt total (context: \"{}\")",
                        format_cents(it.price_cents),
                        it.description,
                    ),
                );
            } else {
                kept.push(it);
            }
        }
        items = kept;
    }

    (items, warnings)
}
