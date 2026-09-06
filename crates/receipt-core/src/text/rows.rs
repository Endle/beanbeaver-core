//! Text extraction: rows.
use super::patterns::*;
use super::quantity::*;
use super::tokens::*;
use super::types::*;
use crate::money::Money;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

/// Repair a trailing Shoppers tax code that OCR confused with the digit `5`.
///
/// A bare `9.99 5` stays deliberately invalid: it could be a quantity or a
/// neighbouring column merged into the line. The repair requires the chain's
/// stronger row shape, `<unit price> <real tax flags> <extended price> 5`, so
/// the earlier `GP` (or equivalent) corroborates that the final one-character
/// column is another tax flag rather than money or quantity.
pub(super) fn normalize_tax_code_ocr(text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let trimmed = text.trim_end();
    let shoppers_row = RE.get_or_init(|| {
        Regex::new(r"(?i)\b\d+\.\d{2}\s*\*?[BCFGHJPTXS]{1,3}\d{0,2}\s+\$?\d+\.\d{2}\s+5$").unwrap()
    });
    if shoppers_row.is_match(trimmed) {
        format!("{}S", &trimmed[..trimmed.len() - 1])
    } else {
        text.to_string()
    }
}

pub(super) fn alpha_ratio(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let alpha_count = value.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    alpha_count as f64 / value.len() as f64
}
pub(super) fn strip_leading_receipt_codes(text: &str) -> String {
    let trimmed = text.trim();
    let trimmed = Regex::new(r"^\(\d+\)\s*").unwrap().replace(trimmed, "");
    let trimmed = Regex::new(r"^\d{6,}\s*")
        .unwrap()
        .replace(trimmed.as_ref(), "");
    trimmed.trim().to_string()
}

/// Strip the OCR-glued `<size>)@<unit>(<qty>/$<deal>)` sale-price subtext
/// that some receipts append to item descriptions.
pub(super) fn strip_sale_price_subtext(text: &str) -> String {
    let stripped = re_sale_price_subtext().replace(text, "");
    re_size_paren_residue()
        .replace(&stripped, "")
        .trim()
        .to_string()
}
pub(super) fn is_section_header_text(text: &str) -> bool {
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
pub(super) fn looks_like_summary_line(text: &str) -> bool {
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
pub(super) fn line_has_trailing_price(text: &str) -> bool {
    re_trailing_price().is_match(&normalize_decimal_spacing(text.trim()))
}
pub(super) fn looks_like_onsale_marker(text: &str) -> bool {
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
pub(super) fn is_generic_counter_label(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_uppercase().as_str(),
        "MEAT" | "BAKERY" | "FROZEN"
    )
}
pub(super) fn is_priced_generic_item_label(left_text: &str, full_text: &str) -> bool {
    !left_text.is_empty()
        && line_has_trailing_price(full_text)
        && is_generic_counter_label(left_text)
}

/// Count qty rows whose trailing price fails to reconcile as the row's own
/// qty×unit total — each is a witness that the price column drifted one row
/// up relative to the text column (see `price_drift` in
/// `extract_text_items`).
pub(super) fn count_price_drift_evidence(lines: &[String]) -> usize {
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
pub(super) fn nearest_desc_above_consumed(
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

pub(super) fn is_descriptive_candidate(text: &str) -> bool {
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
pub(super) fn merge_description_context(lines: &[String], base: &str, source_idx: usize) -> String {
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
pub(super) fn is_weak_inline_description(text: &str) -> bool {
    let stripped = text.trim();
    if stripped.is_empty() {
        return false;
    }
    re_weak_parenthetical().is_match(stripped) || re_weak_measure().is_match(stripped)
}

/// The grand-total row, which is where the item region ends.
///
/// Every exclusion here is a row that says TOTAL without being the total. The
/// column-header case ("DESCRIPTION QTY UNIT TOTAL") is the dangerous one: read
/// as the grand total it sits *above* the items, so treating it as the total
/// truncates the whole item region.
pub(super) fn grand_total_line(lines: &[String]) -> Option<usize> {
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
pub(super) fn total_price_cap(lines: &[String]) -> Option<Money> {
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

/// A REG row printing exactly one price is quoting suggested retail, not
/// charging it.
///
/// The marker is read off the description side with digits and punctuation
/// stripped, because OCR splices it in a dozen ways ("REG$", "0REG", "@REG").
pub(super) fn is_suggested_retail_row(line: &str, desc_part: &str) -> bool {
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
pub(super) fn is_ghost_promo_row(
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
pub(super) fn price_row_is_summary(
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
pub(super) fn malformed_marker_is_multi_buy(index: usize, rows: Lines<'_>) -> bool {
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
pub(super) fn has_reg_price_marker(line_upper: &str) -> bool {
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
pub(super) fn is_bare_price_marker(desc_part: &str) -> bool {
    !desc_part.is_empty()
        && desc_part.starts_with('(')
        && desc_part.contains('$')
        && !desc_part.contains(' ')
        && desc_part.len() <= 16
        && !desc_part.contains('@')
        && !desc_part.to_ascii_uppercase().contains("REG")
}
